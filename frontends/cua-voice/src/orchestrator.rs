use crate::audio::{record_default_input_until, record_default_input_until_stop, RecordedAudio};
use crate::client::{CuaClient, CuaSession};
use crate::daemon::{spawn_profile_daemon, wait_until_ready};
use crate::memory::{
    load_agent_context_with_chat, load_chat_context, AgentContext, ChatStore, CtxMemory,
};
use crate::planner::{
    browser_research_bootstrap_plan, extract_planner_hints, parse_fast_command, PlanAttemptContext,
    PlannedTurn, Planner, PlannerRequest,
};
use crate::stt::{SttClient, SttTranscript, DEFAULT_STT_BACKEND, DEFAULT_STT_MODEL};
use crate::ui_state::VoiceUiEvent;
use anyhow::Context;
use cua_core::{
    profile_ctx_dir, profile_voice_trace_path, DesktopState, FrameEnvelope, FramePayload,
};
use serde::Serialize;
use serde_json::json;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

type LocalTask = tokio::task::JoinHandle<anyhow::Result<LocalReady>>;
type ContextTask = tokio::task::JoinHandle<PrefetchedContext>;
type ChatContextTask = tokio::task::JoinHandle<anyhow::Result<String>>;
type AgentContextTask = tokio::task::JoinHandle<anyhow::Result<crate::memory::AgentContext>>;
static VOICE_TRACE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const VOICE_STEP_SOURCE: &str = "voice";
const VOICE_STEP_TTL_MS: u64 = 5_000;
const VOICE_STEP_LABEL_MAX: usize = 96;
const VOICE_STEP_TIMEOUT_MS: u64 = 120;
const VOICE_STEP_FLUSH_TIMEOUT_MS: u64 = 120;
const DEFAULT_CONTEXT_PREFETCH_TIMEOUT_MS: u64 = 2_500;
const MIN_RECORDING_DURATION: Duration = Duration::from_millis(650);
pub const DEFAULT_PLANNER_MODEL: &str = "google/gemini-3.7-flash";

struct PrefetchedContext {
    session: Option<CuaSession>,
    frame: Option<FramePayload>,
    desktop: Option<DesktopState>,
    errors: Vec<String>,
    elapsed: Duration,
}

struct CompletedAssistantTurn {
    response: String,
    action: Option<serde_json::Value>,
    evidence: Option<serde_json::Value>,
}

struct LocalReady {
    client: CuaClient,
    session: Option<CuaSession>,
}

#[derive(Debug, Clone)]
pub struct VoiceConfig {
    pub profile: String,
    pub record_ms: u64,
    pub stt_backend: String,
    pub stt_model: String,
    pub planner_model: String,
    pub debug_trace: bool,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            profile: "default".to_string(),
            record_ms: 4_500,
            stt_backend: DEFAULT_STT_BACKEND.to_string(),
            stt_model: DEFAULT_STT_MODEL.to_string(),
            planner_model: DEFAULT_PLANNER_MODEL.to_string(),
            debug_trace: false,
        }
    }
}

pub async fn run_voice_turn(config: VoiceConfig, tx: Sender<VoiceUiEvent>) {
    if let Err(error) = run_voice_turn_checked(config, tx.clone()).await {
        eprintln!("cua voice turn failed: {error:#}");
        let _ = tx.send(VoiceUiEvent::Error(user_visible_turn_error(&error)));
    }
}

pub async fn run_voice_turn_until(
    config: VoiceConfig,
    tx: Sender<VoiceUiEvent>,
    stop_requested: Arc<AtomicBool>,
) {
    if let Err(error) = run_voice_turn_checked_until(config, tx.clone(), stop_requested).await {
        eprintln!("cua voice turn failed: {error:#}");
        let _ = tx.send(VoiceUiEvent::Error(user_visible_turn_error(&error)));
    }
}

pub async fn run_text_turn(config: VoiceConfig, transcript: String, tx: Sender<VoiceUiEvent>) {
    if let Err(error) = run_text_turn_checked(config, transcript, tx.clone()).await {
        eprintln!("cua voice scripted turn failed: {error:#}");
        let _ = tx.send(VoiceUiEvent::Error(user_visible_turn_error(&error)));
    }
}

pub async fn run_wav_turn(config: VoiceConfig, wav_bytes: Vec<u8>, tx: Sender<VoiceUiEvent>) {
    if let Err(error) = run_wav_turn_checked(config, wav_bytes, tx.clone()).await {
        eprintln!("cua voice wav turn failed: {error:#}");
        let _ = tx.send(VoiceUiEvent::Error(user_visible_turn_error(&error)));
    }
}

pub async fn run_text_turn_checked(
    config: VoiceConfig,
    transcript: String,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    run_transcript_turn(config, transcript, tx).await
}

pub async fn run_voice_turn_checked(
    config: VoiceConfig,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    record_and_run_turn(
        config.clone(),
        tx,
        Arc::new(AtomicBool::new(false)),
        Some(Duration::from_millis(config.record_ms)),
    )
    .await
}

pub async fn run_voice_turn_checked_until(
    config: VoiceConfig,
    tx: Sender<VoiceUiEvent>,
    stop_requested: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    record_and_run_turn(config, tx, stop_requested, None).await
}

pub async fn run_wav_turn_checked(
    config: VoiceConfig,
    wav_bytes: Vec<u8>,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    let local_task = spawn_local_preflight(config.profile.clone());
    let trace = VoiceTurnTrace::new(&config);
    trace
        .append(
            "turn_start",
            json!({"mode": "wav", "config": trace_config(&config)}),
        )
        .await;
    transcribe_and_run_turn_after_local(config, wav_bytes, local_task, tx, trace).await
}

async fn record_and_run_turn(
    config: VoiceConfig,
    tx: Sender<VoiceUiEvent>,
    stop_requested: Arc<AtomicBool>,
    max_duration: Option<Duration>,
) -> anyhow::Result<()> {
    let trace = VoiceTurnTrace::new(&config);
    trace
        .append(
            "turn_start",
            json!({"mode": "live_record", "config": trace_config(&config)}),
        )
        .await;
    tx.send(VoiceUiEvent::Armed).ok();
    tx.send(VoiceUiEvent::Listening { ms: 0 }).ok();
    trace
        .append(
            "record_start",
            json!({
                "stop": if max_duration.is_some() { "timeout_or_signal" } else { "signal" },
                "max_duration_ms": max_duration.map(elapsed_ms),
            }),
        )
        .await;
    let local_task = spawn_local_preflight(config.profile.clone());
    let progress_flag = Arc::new(AtomicBool::new(true));
    let progress_task = spawn_recording_progress(tx.clone(), progress_flag.clone());
    let record_task = tokio::task::spawn_blocking(move || {
        if let Some(max_duration) = max_duration {
            record_default_input_until(max_duration, stop_requested)
        } else {
            record_default_input_until_stop(stop_requested)
        }
    });
    let record_result = record_task.await.context("join audio recorder");
    progress_flag.store(false, Ordering::Release);
    progress_task.abort();
    let audio = match record_result {
        Ok(Ok(audio)) => audio,
        Ok(Err(error)) => {
            trace
                .append("record_error", json!({"error": format!("{error:#}")}))
                .await;
            return Err(error);
        }
        Err(error) => {
            trace
                .append("record_join_error", json!({"error": format!("{error:#}")}))
                .await;
            return Err(error).context("join audio recorder");
        }
    };
    trace
        .append("audio_diagnostic", audio_trace_json(&audio))
        .await;
    tx.send(VoiceUiEvent::RecordingStopped).ok();
    if let Err(error) = validate_recorded_audio(&audio) {
        trace
            .append(
                "audio_validation_error",
                json!({"error": format!("{error:#}")}),
            )
            .await;
        return Err(error);
    }
    publish_audio_diagnostic(&tx, &audio);
    transcribe_and_run_turn_after_local(config, audio.wav_bytes, local_task, tx, trace).await
}

async fn transcribe_and_run_turn_after_local(
    config: VoiceConfig,
    wav_bytes: Vec<u8>,
    local_task: LocalTask,
    tx: Sender<VoiceUiEvent>,
    trace: VoiceTurnTrace,
) -> anyhow::Result<()> {
    let turn_started = Instant::now();
    let api_key = api_key_for_stt_backend(&config.stt_backend)?;
    tx.send(VoiceUiEvent::Transcribing).ok();
    trace
        .append(
            "stt_start",
            json!({
                "backend": &config.stt_backend,
                "model": &config.stt_model,
                "wav_bytes": wav_bytes.len()
            }),
        )
        .await;
    trace
        .write_artifact(
            "input.wav",
            &wav_bytes,
            json!({
                "kind": "stt_input_wav",
                "bytes": wav_bytes.len()
            }),
        )
        .await;
    let stt_backend = config.stt_backend.clone();
    let stt_model = config.stt_model.clone();
    let stt_api_key = api_key.clone().unwrap_or_default();
    let stt_started = Instant::now();
    let stt_task = tokio::spawn(async move {
        SttClient::new(stt_backend, stt_model)?
            .transcribe_wav(&stt_api_key, &wav_bytes)
            .await
    });
    let overlap_started = Instant::now();
    let local_ready = local_task.await.context("join local daemon preflight")??;
    let local = local_ready.client;
    send_metric(&tx, "stt_preflight_overlap_ms", overlap_started.elapsed());
    trace
        .append(
            "local_preflight_ready",
            json!({"overlap_ms": elapsed_ms(overlap_started.elapsed())}),
        )
        .await;
    let step_publisher = VoiceStepPublisher::start(local.clone());
    step_publisher.publish("transcribing audio");
    let context_task = spawn_context_prefetch(local.clone(), local_ready.session);
    let chat_task = spawn_chat_context_prefetch(config.profile.clone());
    let stt_result = stt_task.await.context("join speech to text")?;
    let stt_elapsed = stt_started.elapsed();
    send_metric(&tx, "stt_ms", stt_elapsed);
    let transcript = match stt_result {
        Ok(transcript) => {
            trace
                .append(
                    "stt_result",
                    transcript_trace_json(&transcript, stt_elapsed),
                )
                .await;
            transcript
        }
        Err(error) if is_missed_speech_error(&error) => {
            trace
                .append(
                    "stt_missed_speech",
                    json!({"elapsed_ms": elapsed_ms(stt_elapsed), "error": format!("{error:#}")}),
                )
                .await;
            abort_context_prefetch(&context_task, &trace, "stt_missed_speech").await;
            abort_chat_context_prefetch(&chat_task, &trace, "stt_missed_speech").await;
            tx.send(VoiceUiEvent::Error(user_visible_turn_error(&error)))
                .ok();
            return Ok(());
        }
        Err(error) => {
            trace
                .append(
                    "stt_error",
                    json!({"elapsed_ms": elapsed_ms(stt_elapsed), "error": format!("{error:#}")}),
                )
                .await;
            abort_context_prefetch(&context_task, &trace, "stt_error").await;
            abort_chat_context_prefetch(&chat_task, &trace, "stt_error").await;
            return Err(error);
        }
    };
    publish_stt_diagnostic(&tx, &transcript);
    if let Err(error) = validate_stt_transcript(&transcript) {
        trace
            .append(
                "transcript_validation_error",
                json!({
                    "class": transcript_class(&transcript.text),
                    "error": format!("{error:#}")
                }),
            )
            .await;
        abort_context_prefetch(&context_task, &trace, "transcript_validation_error").await;
        abort_chat_context_prefetch(&chat_task, &trace, "transcript_validation_error").await;
        tx.send(VoiceUiEvent::Error(user_visible_turn_error(&error)))
            .ok();
        return Ok(());
    }
    tx.send(VoiceUiEvent::Accepted).ok();
    tx.send(VoiceUiEvent::Transcript(transcript.text.clone()))
        .ok();
    step_publisher.publish(voice_step_label("transcript", &transcript.text));
    plan_and_dispatch(
        config,
        transcript.text,
        api_key,
        local,
        Some(context_task),
        Some(chat_task),
        step_publisher,
        tx.clone(),
        trace.clone(),
    )
    .await?;
    send_metric(&tx, "turn_total_ms", turn_started.elapsed());
    trace
        .append(
            "turn_complete",
            json!({"total_ms": elapsed_ms(turn_started.elapsed())}),
        )
        .await;
    Ok(())
}

fn api_key_for_stt_backend(backend: &str) -> anyhow::Result<Option<String>> {
    api_key_for_stt_backend_from(backend, || std::env::var("OPENROUTER_API_KEY").ok())
}

fn api_key_for_stt_backend_from(
    backend: &str,
    lookup: impl FnOnce() -> Option<String>,
) -> anyhow::Result<Option<String>> {
    let key = lookup().filter(|key| !key.trim().is_empty());
    if backend == "openrouter" {
        return key
            .map(Some)
            .context("OPENROUTER_API_KEY is required for OpenRouter speech-to-text");
    }
    Ok(key)
}

async fn run_transcript_turn(
    config: VoiceConfig,
    transcript: String,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    let turn_started = Instant::now();
    let trace = VoiceTurnTrace::new(&config);
    trace
        .append(
            "turn_start",
            json!({"mode": "text", "config": trace_config(&config)}),
        )
        .await;
    tx.send(VoiceUiEvent::Armed).ok();
    let local_ready = preflight_local_client(&config.profile).await?;
    let local = local_ready.client;
    let step_publisher = VoiceStepPublisher::start(local.clone());
    let chat_task = spawn_chat_context_prefetch(config.profile.clone());
    let context_task = spawn_context_prefetch(local.clone(), local_ready.session);
    tx.send(VoiceUiEvent::Transcript(transcript.clone())).ok();
    step_publisher.publish(voice_step_label("transcript", &transcript));
    plan_and_dispatch(
        config,
        transcript,
        None,
        local,
        Some(context_task),
        Some(chat_task),
        step_publisher,
        tx.clone(),
        trace.clone(),
    )
    .await?;
    send_metric(&tx, "turn_total_ms", turn_started.elapsed());
    trace
        .append(
            "turn_complete",
            json!({"total_ms": elapsed_ms(turn_started.elapsed())}),
        )
        .await;
    Ok(())
}

fn validate_recorded_audio(audio: &RecordedAudio) -> anyhow::Result<()> {
    if audio.duration < MIN_RECORDING_DURATION {
        anyhow::bail!("recording was too short to contain a clear command");
    }
    Ok(())
}

fn validate_stt_transcript(transcript: &SttTranscript) -> anyhow::Result<()> {
    let class = transcript_class(&transcript.text);
    if class == "command_candidate" {
        return Ok(());
    }
    anyhow::bail!(
        "speech-to-text did not produce a command: model={} generation_id={} class={}",
        transcript.model,
        transcript.generation_id.as_deref().unwrap_or("unknown"),
        class
    )
}

fn user_visible_turn_error(error: &anyhow::Error) -> String {
    let message = format!("{error:#}");
    if is_missed_speech_message(&message) {
        "Didn't catch a command.".to_string()
    } else if message.contains("local speech-to-text")
        || message.contains("No such file or directory: 'ffmpeg'")
    {
        "Local speech-to-text needs attention.".to_string()
    } else if message.contains("planning model returned empty content") {
        "Planner returned empty content.".to_string()
    } else if message.contains("model output was not valid action JSON")
        || message.contains("parse plan JSON")
    {
        "Planner returned invalid action JSON.".to_string()
    } else {
        message
    }
}

fn is_missed_speech_error(error: &anyhow::Error) -> bool {
    is_missed_speech_message(&format!("{error:#}"))
}

fn is_missed_speech_message(message: &str) -> bool {
    message.contains("missed_speech")
        || message.contains("speech-to-text returned empty text")
        || message.contains("speech-to-text did not produce a command")
}

fn planning_error_is_empty_content(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains("planning model returned empty content")
}

fn planning_error_is_invalid_action_json(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}");
    message.contains("model output was not valid action JSON")
        || message.contains("parse action")
        || message.contains("parse plan JSON")
}

fn normalized_transcript(transcript: &str) -> String {
    transcript
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn is_common_missed_speech(transcript: &str) -> bool {
    transcript.is_empty()
        || matches!(transcript, "you" | "thanks" | "subscribe")
        || transcript.starts_with("thank you")
        || transcript.starts_with("thanks for")
        || transcript.starts_with("you're welcome")
        || transcript.starts_with("you are welcome")
        || transcript.starts_with("subtitles by")
}

fn transcript_class(transcript: &str) -> &'static str {
    let normalized = normalized_transcript(transcript);
    if normalized.is_empty() {
        "empty"
    } else if is_common_missed_speech(&normalized) {
        "missed_speech"
    } else {
        "command_candidate"
    }
}

fn publish_stt_diagnostic(tx: &Sender<VoiceUiEvent>, transcript: &SttTranscript) {
    tx.send(VoiceUiEvent::SttDiagnostic {
        backend: transcript.backend.clone(),
        model: transcript.model.clone(),
        generation_id: transcript.generation_id.clone(),
        audio_ms: transcript
            .usage
            .as_ref()
            .and_then(|usage| usage.seconds)
            .map(|seconds| (seconds * 1_000.0).round().max(0.0) as u64),
        transcript_class: transcript_class(&transcript.text).to_string(),
    })
    .ok();
}

fn publish_audio_diagnostic(tx: &Sender<VoiceUiEvent>, audio: &RecordedAudio) {
    tx.send(VoiceUiEvent::AudioDiagnostic {
        device_name: audio.device_name.clone(),
        channels: audio.channels,
        sample_format: audio.sample_format.clone(),
        sample_rate: audio.sample_rate,
        duration_ms: audio.duration.as_millis().min(u128::from(u64::MAX)) as u64,
        peak_amplitude: audio.peak_amplitude,
        rms_amplitude_ppm: (audio.rms_amplitude * 1_000_000.0).round().max(0.0) as u32,
        wav_bytes: audio.wav_bytes.len(),
    })
    .ok();
}

#[allow(clippy::too_many_arguments)]
async fn plan_and_dispatch(
    config: VoiceConfig,
    transcript: String,
    api_key: Option<String>,
    local: CuaClient,
    context_task: Option<ContextTask>,
    chat_task: Option<ChatContextTask>,
    step_publisher: VoiceStepPublisher,
    tx: Sender<VoiceUiEvent>,
    trace: VoiceTurnTrace,
) -> anyhow::Result<()> {
    let plan_started = Instant::now();
    trace
        .append("planning_start", json!({"transcript": &transcript}))
        .await;
    let planner_hints = extract_planner_hints(&transcript);
    if !planner_hints.is_empty() {
        trace.append("planner_hints", json!(&planner_hints)).await;
    }
    let completed = if let Some(plan) = parse_fast_command(&transcript) {
        tx.send(VoiceUiEvent::Planning {
            tool: "Command parser".to_string(),
        })
        .ok();
        step_publisher.publish("planning fast command");
        if let Some(context_task) = context_task {
            context_task.abort();
            send_metric(&tx, "context_prefetch_aborted_ms", plan_started.elapsed());
        }
        if let Some(chat_task) = chat_task {
            abort_chat_context_prefetch(&chat_task, &trace, "fast_command").await;
        }
        trace
            .append(
                "planning_result",
                plan_trace_json("fast_command", plan_started.elapsed(), &plan),
            )
            .await;
        send_metric(&tx, "plan_ms", plan_started.elapsed());
        dispatch_plan(
            local,
            None,
            plan,
            None,
            &step_publisher,
            tx.clone(),
            &trace,
            &config.profile,
        )
        .await?
    } else {
        tx.send(VoiceUiEvent::Planning {
            tool: "Desktop context".to_string(),
        })
        .ok();
        step_publisher.publish("checking desktop context");
        let wait_started = Instant::now();
        let agent_context_task =
            spawn_agent_context(config.profile.clone(), transcript.clone(), chat_task);
        let (mut context, agent_context_result) = tokio::join!(
            resolve_context_for_planning(local.clone(), context_task),
            resolve_agent_context(agent_context_task)
        );
        let (agent_context, agent_context_errors) = match agent_context_result {
            Ok(agent_context) => (agent_context, Vec::new()),
            Err(error) => {
                let chat = resolve_chat_context(config.profile.clone(), None)
                    .await
                    .unwrap_or_else(|chat_error| {
                        format!("Recent chat: unavailable. error={chat_error:#}")
                    });
                let message = format!("{error:#}");
                (
                    AgentContext {
                        chat,
                        ctx: format!("Context: unavailable.\nctx_error: {message}"),
                        scratchpads: "Scratchpads: unavailable because ctx context load failed."
                            .to_string(),
                    },
                    vec![message],
                )
            }
        };
        trace
            .append(
                "context_result",
                json!({
                    "elapsed_ms": elapsed_ms(context.elapsed),
                    "has_session": context.session.is_some(),
                    "has_frame": context.frame.is_some(),
                    "has_desktop": context.desktop.is_some(),
                    "frame": context.frame.as_ref().map(frame_trace_json),
                    "windows": context.desktop.as_ref().map(|desktop| desktop.windows.len()),
                    "displays": context.desktop.as_ref().map(|desktop| desktop.displays.len()),
                    "errors": &context.errors,
                }),
            )
            .await;
        send_metric(&tx, "context_wait_ms", wait_started.elapsed());
        send_metric(&tx, "context_prefetch_ms", context.elapsed);
        trace
            .append(
                "agent_context_result",
                json!({
                    "chat_chars": agent_context.chat.chars().count(),
                    "ctx_chars": agent_context.ctx.chars().count(),
                    "scratchpad_chars": agent_context.scratchpads.chars().count(),
                    "errors": &agent_context_errors,
                }),
            )
            .await;
        let api_key = api_key
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .context("OPENROUTER_API_KEY is required for non-fast voice commands")?;
        let loop_budget = agent_loop_budget();
        trace
            .append("agent_loop_start", json!({"budget": loop_budget}))
            .await;
        let planner = Planner::new(&config.planner_model);
        let mut latest_chat_context = agent_context.chat.clone();
        let mut attempts = Vec::new();
        let mut attempt_index = 1;
        let completed = loop {
            let planner_agent_context = combined_agent_context(
                &latest_chat_context,
                &agent_context.ctx,
                &agent_context.scratchpads,
                &context.errors,
                &agent_context_errors,
            );
            let attempt_started = Instant::now();
            tx.send(VoiceUiEvent::Planning {
                tool: if attempt_index == 1 {
                    "OpenRouter Vision".to_string()
                } else {
                    format!(
                        "OpenRouter repair {}",
                        loop_budget.format_attempt(attempt_index)
                    )
                },
            })
            .ok();
            step_publisher.publish(voice_step_label(
                "planning",
                &format!("attempt {}", loop_budget.format_attempt(attempt_index)),
            ));
            trace
                .append(
                    "agent_attempt_start",
                    json!({
                        "attempt_index": attempt_index,
                        "prior_attempts": attempts.len(),
                    }),
                )
                .await;
            let mut plan = match planner
                .plan_request(
                    &api_key,
                    PlannerRequest {
                        transcript: &transcript,
                        agent_context: Some(&planner_agent_context),
                        hints: Some(&planner_hints),
                        frame: context.frame.as_ref(),
                        desktop: context.desktop.as_ref(),
                        prior_attempts: &attempts,
                    },
                )
                .await
            {
                Ok(plan) => plan,
                Err(error) => {
                    trace
                        .append(
                            "planning_error",
                            json!({
                                "attempt_index": attempt_index,
                                "error": format!("{error:#}")
                            }),
                        )
                        .await;
                    let recoverable_planning_error = planning_error_is_empty_content(&error)
                        || planning_error_is_invalid_action_json(&error);
                    if recoverable_planning_error {
                        if let Some(plan) = browser_research_bootstrap_plan(&transcript) {
                            trace
                                .append(
                                    "planning_error_recovered",
                                    json!({
                                        "attempt_index": attempt_index,
                                        "strategy": "browser_research_bootstrap",
                                        "error": format!("{error:#}"),
                                    }),
                                )
                                .await;
                            plan
                        } else if loop_budget.can_continue_after(attempt_index) {
                            let error_message = format!("{error:#}");
                            attempts.push(PlanAttemptContext {
                                attempt_index,
                                response: "Planner output could not be used.".to_string(),
                                action: None,
                                effect: Some("failed".to_string()),
                                evidence: Some(json!({
                                    "effect": "failed",
                                    "reason": "planning_error",
                                    "error": error_message,
                                })),
                            });
                            attempt_index += 1;
                            continue;
                        } else {
                            break CompletedAssistantTurn {
                                response:
                                    "Planner output could not be used before the loop budget ended."
                                        .to_string(),
                                action: None,
                                evidence: Some(json!({
                                    "effect": "partial",
                                    "reason": "planning_error",
                                    "error": format!("{error:#}"),
                                })),
                            };
                        }
                    } else if !transcript_requests_long_range_work(&transcript) {
                        if let Some(turn) = completed_from_confirmed_prior_attempt(&attempts) {
                            trace
                                .append(
                                    "planning_error_after_confirmed_action",
                                    json!({
                                        "attempt_index": attempt_index,
                                        "error": format!("{error:#}"),
                                    }),
                                )
                                .await;
                            break turn;
                        }
                        return Err(error);
                    } else {
                        return Err(error);
                    }
                }
            };
            let should_bootstrap_browser_research =
                transcript_requests_long_range_work(&transcript)
                    && (plan
                        .action
                        .as_ref()
                        .is_some_and(input_action_is_open_only_setup)
                        || (plan.action.is_none()
                            && planner_response_claims_pending_work(&plan.response)));
            if should_bootstrap_browser_research {
                if let Some(recovered_plan) = browser_research_bootstrap_plan(&transcript) {
                    trace
                        .append(
                            "planning_browser_research_bootstrapped",
                            json!({
                                "attempt_index": attempt_index,
                                "strategy": "browser_research_bootstrap",
                                "model_response": plan.response,
                                "model_action": plan.action.as_ref().map(serde_json::to_value).transpose()?,
                            }),
                        )
                        .await;
                    plan = recovered_plan;
                }
            }
            repair_new_note_text_entry_plan(&transcript, &mut plan.action);
            let dedupe_report = dedupe_redundant_sequence_actions(&mut plan.action);
            if dedupe_report.removed > 0 {
                trace
                    .append(
                        "planning_action_normalized",
                        json!({
                            "attempt_index": attempt_index,
                            "removed_redundant_actions": dedupe_report.removed,
                        }),
                    )
                    .await;
            }
            trace
                .append(
                    "planning_result",
                    plan_trace_json(&config.planner_model, attempt_started.elapsed(), &plan),
                )
                .await;
            let planned_action_json = plan.action.as_ref().map(serde_json::to_value).transpose()?;
            if long_range_null_plan_is_incomplete(&transcript, &plan.response, &planned_action_json)
            {
                trace
                    .append(
                        "planning_rejected",
                        json!({
                            "attempt_index": attempt_index,
                            "reason": "long_range_null_plan_claimed_pending_work",
                            "response": plan.response.clone(),
                        }),
                    )
                    .await;
                attempts.push(PlanAttemptContext {
                    attempt_index,
                    response: plan.response,
                    action: None,
                    effect: Some("suspected_noop".to_string()),
                    evidence: Some(json!({
                        "effect": "suspected_noop",
                        "reason": "long_range_null_plan_claimed_pending_work",
                        "repair_hint": "Return the next tool action needed to complete or verify the user's request, not a progress description.",
                    })),
                });
                if !loop_budget.can_continue_after(attempt_index) {
                    anyhow::bail!(
                        "planning model returned an incomplete action-null long-range plan"
                    );
                }
                attempt_index += 1;
                continue;
            }
            if planned_action_json.as_ref().is_some_and(|action| {
                transcript_requests_long_range_work(&transcript)
                    && action_repeats_confirmed_attempt(&attempts, action)
            }) {
                trace
                    .append(
                        "planning_rejected",
                        json!({
                            "attempt_index": attempt_index,
                            "reason": "repeated_confirmed_long_range_action",
                            "action": planned_action_json.clone(),
                        }),
                    )
                    .await;
                attempts.push(PlanAttemptContext {
                    attempt_index,
                    response: plan.response,
                    action: planned_action_json,
                    effect: Some("suspected_noop".to_string()),
                    evidence: Some(json!({
                        "effect": "suspected_noop",
                        "reason": "repeated_confirmed_long_range_action",
                        "repair_hint": "The same action already completed partial progress. Use the fresh observation to choose the next different action or final verified answer.",
                    })),
                });
                if !loop_budget.can_continue_after(attempt_index) {
                    anyhow::bail!("planning model repeated an already completed long-range action");
                }
                attempt_index += 1;
                continue;
            }
            if transcript_requests_text_entry(&transcript)
                && !plan
                    .action
                    .as_ref()
                    .is_some_and(action_satisfies_text_entry_request)
                && !action_null_finishes_after_prior_attempts(
                    &plan.response,
                    &planned_action_json,
                    &attempts,
                )
            {
                trace
                    .append(
                        "planning_rejected",
                        json!({
                            "attempt_index": attempt_index,
                            "reason": "text_request_without_text_entry_action",
                            "action": planned_action_json.clone(),
                        }),
                    )
                    .await;
                attempts.push(PlanAttemptContext {
                    attempt_index,
                    response: plan.response,
                    action: planned_action_json,
                    effect: Some("suspected_noop".to_string()),
                    evidence: Some(json!({
                        "effect": "suspected_noop",
                        "reason": "text_request_without_text_entry_action",
                    })),
                });
                if !loop_budget.can_continue_after(attempt_index) {
                    anyhow::bail!(
                        "planning model did not include a text-entry action for a text-writing command"
                    );
                }
                attempt_index += 1;
                continue;
            }
            let source_frame = context.frame.as_ref().map(|frame| frame.envelope.clone());
            let turn = match dispatch_plan(
                local.clone(),
                context.session,
                plan,
                source_frame,
                &step_publisher,
                tx.clone(),
                &trace,
                &config.profile,
            )
            .await
            {
                Ok(turn) => turn,
                Err(error) => {
                    let evidence = Some(json!({
                        "effect": "failed",
                        "reason": "dispatch_error",
                        "error": format!("{error:#}"),
                    }));
                    trace
                        .append(
                            "agent_attempt_outcome",
                            json!({
                                "attempt_index": attempt_index,
                                "effect": "failed",
                                "should_replan": loop_budget.can_continue_after(attempt_index),
                                "dispatch_error": format!("{error:#}"),
                                "has_action": planned_action_json.is_some(),
                            }),
                        )
                        .await;
                    attempts.push(PlanAttemptContext {
                        attempt_index,
                        response: "Dispatch failed before the action completed.".to_string(),
                        action: planned_action_json,
                        effect: Some("failed".to_string()),
                        evidence,
                    });
                    if !loop_budget.can_continue_after(attempt_index) {
                        break CompletedAssistantTurn {
                            response: "I hit a dispatch error before completing the full task."
                                .to_string(),
                            action: None,
                            evidence: Some(json!({
                                "effect": "partial",
                                "reason": "dispatch_error",
                                "attempt_count": attempt_index,
                            })),
                        };
                    }
                    tx.send(VoiceUiEvent::Planning {
                        tool: "Reobserving".to_string(),
                    })
                    .ok();
                    step_publisher.publish(voice_step_label(
                        "observe",
                        &format!("after dispatch error {attempt_index}"),
                    ));
                    context = prefetch_context_for_planning(local.clone(), None).await;
                    latest_chat_context = resolve_chat_context(config.profile.clone(), None)
                        .await
                        .unwrap_or_else(|chat_error| {
                            format!("Recent chat: unavailable. error={chat_error:#}")
                        });
                    attempt_index += 1;
                    continue;
                }
            };
            let mut effect = turn_effect(&turn);
            let open_only_incomplete = loop_budget.can_continue_after(attempt_index)
                && open_only_incomplete_for_goal(&transcript, &turn);
            let evidence = if open_only_incomplete {
                effect = Some("partial".to_string());
                Some(open_only_incomplete_evidence(turn.evidence.clone()))
            } else {
                turn.evidence.clone()
            };
            let long_range_continuation = should_continue_long_range_after_verified_action(
                &transcript,
                &turn,
                effect.as_deref(),
                attempt_index,
                loop_budget,
            );
            let should_continue = should_replan_after_turn(
                &transcript,
                &turn,
                effect.as_deref(),
                attempt_index,
                loop_budget,
            );
            trace
                .append(
                    "agent_attempt_outcome",
                    json!({
                        "attempt_index": attempt_index,
                        "effect": effect,
                        "should_replan": should_continue,
                        "open_only_incomplete": open_only_incomplete,
                        "long_range_continuation": long_range_continuation,
                        "has_action": turn.action.is_some(),
                    }),
                )
                .await;
            let should_verify = turn
                .action
                .as_ref()
                .is_some_and(action_needs_fresh_verification);
            if !should_continue && !should_verify {
                break mark_long_range_budget_exhausted_if_needed(
                    &transcript,
                    turn,
                    attempt_index,
                    loop_budget,
                );
            }
            let finish_after_reobserve =
                should_finish_after_reobserve(should_verify, should_continue);
            attempts.push(PlanAttemptContext {
                attempt_index,
                response: turn.response.clone(),
                action: turn.action.clone(),
                effect,
                evidence,
            });
            tx.send(VoiceUiEvent::Planning {
                tool: "Reobserving".to_string(),
            })
            .ok();
            step_publisher.publish(voice_step_label(
                "observe",
                &format!("after attempt {attempt_index}"),
            ));
            trace
                .append(
                    "agent_reobserve_start",
                    json!({"after_attempt": attempt_index, "reason": if should_verify { "verify_action" } else { "repair_after_effect" }}),
                )
                .await;
            let observe_started = Instant::now();
            context = prefetch_context_for_planning(local.clone(), None).await;
            latest_chat_context = resolve_chat_context(config.profile.clone(), None)
                .await
                .unwrap_or_else(|chat_error| {
                    format!("Recent chat: unavailable. error={chat_error:#}")
                });
            send_metric(&tx, "reobserve_ms", observe_started.elapsed());
            trace
                .append(
                    "agent_reobserve_result",
                    json!({
                        "elapsed_ms": elapsed_ms(observe_started.elapsed()),
                        "has_session": context.session.is_some(),
                        "has_frame": context.frame.is_some(),
                        "has_desktop": context.desktop.is_some(),
                        "frame": context.frame.as_ref().map(frame_trace_json),
                    }),
                )
                .await;
            if finish_after_reobserve {
                break mark_long_range_budget_exhausted_if_needed(
                    &transcript,
                    turn,
                    attempt_index,
                    loop_budget,
                );
            }
            attempt_index += 1;
        };
        send_metric(&tx, "plan_ms", plan_started.elapsed());
        let attempt_count = loop_attempt_count(&completed, &attempts);
        let completed = attach_loop_evidence(completed, &attempts);
        trace
            .append(
                "agent_loop_stop",
                json!({
                    "attempts": attempt_count,
                    "final_effect": turn_effect(&completed),
                }),
            )
            .await;
        completed
    };
    emit_completed_reply(&completed, &step_publisher, &tx, &trace).await;
    step_publisher.finish().await;
    persist_turn_memory(&config, &transcript, &completed, &trace).await?;
    Ok(())
}

async fn prefetch_context(local: CuaClient, warm_session: Option<CuaSession>) -> PrefetchedContext {
    let mut errors = Vec::new();
    let mut session = match warm_session {
        Some(session) => session,
        None => match local.session().await {
            Ok(session) => session,
            Err(error) => {
                errors.push(format!("session acquire failed: {error}"));
                return PrefetchedContext {
                    session: None,
                    frame: None,
                    desktop: None,
                    errors,
                    elapsed: Duration::ZERO,
                };
            }
        },
    };
    let snapshot =
        match tokio::time::timeout(context_prefetch_timeout(), session.context(true)).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(error)) => {
                errors.push(format!("context snapshot failed: {error}"));
                return PrefetchedContext {
                    session: Some(session),
                    frame: None,
                    desktop: None,
                    errors,
                    elapsed: Duration::ZERO,
                };
            }
            Err(_) => {
                errors.push(format!(
                    "context snapshot timed out after {}ms",
                    context_prefetch_timeout().as_millis()
                ));
                let desktop = match local.observe().await {
                    Ok(desktop) => Some(desktop),
                    Err(error) => {
                        errors.push(format!("fallback desktop observe failed: {error}"));
                        None
                    }
                };
                return PrefetchedContext {
                    session: None,
                    frame: None,
                    desktop,
                    errors,
                    elapsed: Duration::ZERO,
                };
            }
        };
    PrefetchedContext {
        session: Some(session),
        frame: Some(snapshot.frame),
        desktop: Some(snapshot.desktop),
        errors,
        elapsed: Duration::ZERO,
    }
}

fn context_prefetch_timeout() -> Duration {
    let ms = std::env::var("CUA_VOICE_CONTEXT_PREFETCH_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_CONTEXT_PREFETCH_TIMEOUT_MS)
        .clamp(250, 30_000);
    Duration::from_millis(ms)
}

fn combined_agent_context(
    chat: &str,
    ctx: &str,
    scratchpads: &str,
    desktop_errors: &[String],
    agent_context_errors: &[String],
) -> String {
    let mut sections = vec![chat.to_string(), ctx.to_string(), scratchpads.to_string()];
    if !desktop_errors.is_empty() || !agent_context_errors.is_empty() {
        sections.push(format!(
            "Runtime context errors visible to the agent:\ndesktop={}\nagent_context={}",
            serde_json::to_string(desktop_errors).unwrap_or_else(|_| "[]".to_string()),
            serde_json::to_string(agent_context_errors).unwrap_or_else(|_| "[]".to_string())
        ));
    }
    sections.join("\n")
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_plan(
    local: CuaClient,
    mut session: Option<CuaSession>,
    mut plan: PlannedTurn,
    source_frame: Option<FrameEnvelope>,
    step_publisher: &VoiceStepPublisher,
    tx: Sender<VoiceUiEvent>,
    trace: &VoiceTurnTrace,
    profile: &str,
) -> anyhow::Result<CompletedAssistantTurn> {
    if let Some(action) = plan.action.as_mut() {
        stamp_ctx_workspace_root(action, profile);
    }
    if let Some(action) = &plan.action {
        tx.send(VoiceUiEvent::Dispatching(format!("{action:?}")))
            .ok();
        step_publisher.publish(voice_step_label("dispatch", &format!("{action:?}")));
        trace
            .append(
                "dispatch_start",
                json!({
                    "action": action,
                    "frame_remap": source_frame.is_some(),
                }),
            )
            .await;
        let dispatch_started = Instant::now();
        let owner = local
            .acquire_owner("cua voice dispatch", Some(60_000))
            .await
            .context("acquire voice dispatch owner session")?;
        let owner_session_id = owner.session.session_id.clone();
        let result = match session.as_mut() {
            Some(session) => match source_frame.clone() {
                Some(frame) => {
                    session
                        .dispatch_frame(frame, action, &owner_session_id)
                        .await
                }
                None => session.dispatch(action, &owner_session_id).await,
            },
            None => match source_frame {
                Some(frame) => local.dispatch_frame(frame, action, &owner_session_id).await,
                None => local.dispatch(action, &owner_session_id).await,
            },
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                if let Err(cancel_error) =
                    local.cancel_session(owner_session_id.clone(), None).await
                {
                    trace
                        .append(
                            "dispatch_owner_cancel_error",
                            json!({
                                "owner_session_id": owner_session_id,
                                "error": format!("{cancel_error:#}")
                            }),
                        )
                        .await;
                    eprintln!(
                        "cua voice dispatch owner cleanup failed after dispatch error: {cancel_error:#}"
                    );
                }
                trace
                    .append(
                        "dispatch_error",
                        json!({
                            "elapsed_ms": elapsed_ms(dispatch_started.elapsed()),
                            "action": action,
                            "error": format!("{error:#}")
                        }),
                    )
                    .await;
                return Err(error.into());
            }
        };
        if let Err(error) = local.cancel_session(owner_session_id.clone(), None).await {
            trace
                .append(
                    "dispatch_owner_cancel_error",
                    json!({
                        "owner_session_id": owner_session_id,
                        "error": format!("{error:#}")
                    }),
                )
                .await;
            eprintln!("cua voice dispatch owner cleanup failed: {error:#}");
        }
        send_metric(&tx, "dispatch_ms", dispatch_started.elapsed());
        trace
            .append(
                "dispatch_result",
                json!({
                    "elapsed_ms": elapsed_ms(dispatch_started.elapsed()),
                    "result": result.clone(),
                }),
            )
            .await;
        Ok(CompletedAssistantTurn {
            response: plan.response.clone(),
            action: Some(serde_json::to_value(action)?),
            evidence: Some(result),
        })
    } else {
        Ok(CompletedAssistantTurn {
            response: plan.response,
            action: None,
            evidence: None,
        })
    }
}

async fn emit_completed_reply(
    completed: &CompletedAssistantTurn,
    step_publisher: &VoiceStepPublisher,
    tx: &Sender<VoiceUiEvent>,
    trace: &VoiceTurnTrace,
) {
    let action = completed.action.is_some();
    tx.send(VoiceUiEvent::Reply(user_visible_reply_text(completed)))
        .ok();
    step_publisher.publish(voice_step_label("reply", &completed.response));
    trace
        .append(
            "reply",
            json!({"text": completed.response, "action": action}),
        )
        .await;
}

fn user_visible_reply_text(completed: &CompletedAssistantTurn) -> String {
    completed.response.clone()
}

fn stamp_ctx_workspace_root(action: &mut cua_core::InputAction, profile: &str) {
    match action {
        cua_core::InputAction::Ctx { workspace_root, .. } => {
            if workspace_root.is_none() {
                *workspace_root = Some(ctx_workspace_root(profile));
            }
        }
        cua_core::InputAction::Sequence { actions, .. } => {
            for action in actions {
                stamp_ctx_workspace_root(action, profile);
            }
        }
        _ => {}
    }
}

fn ctx_workspace_root(profile: &str) -> String {
    profile_ctx_dir(profile)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| format!(".cua/profiles/{profile}/ctx"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AgentLoopBudget {
    Finite { max_attempts: usize },
    Unbounded,
}

impl AgentLoopBudget {
    fn can_continue_after(self, attempt_index: usize) -> bool {
        match self {
            Self::Finite { max_attempts } => attempt_index < max_attempts,
            Self::Unbounded => true,
        }
    }

    fn exhausted_at(self, attempt_index: usize) -> bool {
        match self {
            Self::Finite { max_attempts } => attempt_index >= max_attempts,
            Self::Unbounded => false,
        }
    }

    fn format_attempt(self, attempt_index: usize) -> String {
        match self {
            Self::Finite { max_attempts } => format!("{attempt_index}/{max_attempts}"),
            Self::Unbounded => format!("{attempt_index}/n"),
        }
    }

    fn max_attempts(self) -> Option<usize> {
        match self {
            Self::Finite { max_attempts } => Some(max_attempts),
            Self::Unbounded => None,
        }
    }
}

fn agent_loop_budget() -> AgentLoopBudget {
    let Some(value) = std::env::var("CUA_AGENT_LOOP_MAX_ATTEMPTS")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
    else {
        return default_agent_loop_budget();
    };

    if value == "n" {
        return AgentLoopBudget::Unbounded;
    }

    value
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
        .map(|max_attempts| AgentLoopBudget::Finite { max_attempts })
        .unwrap_or_else(default_agent_loop_budget)
}

fn default_agent_loop_budget() -> AgentLoopBudget {
    AgentLoopBudget::Unbounded
}

fn turn_effect(turn: &CompletedAssistantTurn) -> Option<String> {
    turn.evidence
        .as_ref()
        .and_then(|evidence| evidence["effect"].as_str())
        .map(ToString::to_string)
}

fn action_needs_fresh_verification(action: &serde_json::Value) -> bool {
    matches!(
        action.get("kind").and_then(|kind| kind.as_str()),
        Some("sequence" | "key_type" | "key_paste" | "shell_exec" | "aegis" | "ctx")
    )
}

fn action_is_text_entry(action: &serde_json::Value) -> bool {
    match action.get("kind").and_then(|kind| kind.as_str()) {
        Some("key_type" | "key_paste" | "clipboard_write") => true,
        Some("sequence") => action
            .get("actions")
            .and_then(|actions| actions.as_array())
            .is_some_and(|actions| actions.iter().any(action_is_text_entry)),
        _ => false,
    }
}

fn action_is_browser_research_setup(action: &serde_json::Value) -> bool {
    let Some(actions) = action
        .get("actions")
        .and_then(|actions| actions.as_array())
        .filter(|actions| !actions.is_empty())
    else {
        return false;
    };
    let has_query_entry = actions.iter().any(|action| {
        matches!(
            action.get("kind").and_then(|kind| kind.as_str()),
            Some("key_type" | "key_paste")
        )
    });
    let has_submit = actions.iter().any(|action| {
        action.get("kind").and_then(|kind| kind.as_str()) == Some("key_press")
            && action
                .get("combo")
                .and_then(|combo| combo.as_str())
                .is_some_and(|combo| combo.eq_ignore_ascii_case("enter"))
    });
    has_query_entry && has_submit
}

fn action_is_user_text_fulfillment(transcript: &str, action: &serde_json::Value) -> bool {
    action_is_text_entry(action)
        && transcript_requests_text_entry(transcript)
        && !transcript_requests_long_range_work(transcript)
        && !action_is_browser_research_setup(action)
}

fn transcript_requests_text_entry(transcript: &str) -> bool {
    let words = transcript
        .split_whitespace()
        .map(normalize_voice_token)
        .collect::<Vec<_>>();
    if words
        .iter()
        .any(|word| matches!(word.as_str(), "write" | "type" | "paste"))
    {
        return true;
    }
    let mentions_note = words.iter().any(|word| word == "note" || word == "notes");
    let note_creation = words
        .iter()
        .any(|word| matches!(word.as_str(), "new" | "create" | "make" | "says"));
    if mentions_note && note_creation {
        return true;
    }
    let mentions_message = words
        .iter()
        .any(|word| word == "message" || word == "messages");
    let message_writing = words
        .iter()
        .any(|word| matches!(word.as_str(), "send" | "leave" | "draft" | "says"));
    if mentions_message && message_writing {
        return true;
    }
    false
}

fn transcript_requests_new_note(transcript: &str) -> bool {
    let words = transcript
        .split_whitespace()
        .map(normalize_voice_token)
        .collect::<Vec<_>>();
    words.iter().any(|word| word == "note" || word == "notes")
        && words
            .iter()
            .any(|word| matches!(word.as_str(), "new" | "create" | "make"))
}

fn normalize_voice_token(token: &str) -> String {
    token
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
        .to_ascii_lowercase()
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ActionDedupeReport {
    removed: usize,
}

fn dedupe_redundant_sequence_actions(
    action: &mut Option<cua_core::InputAction>,
) -> ActionDedupeReport {
    let mut report = ActionDedupeReport::default();
    if let Some(action) = action {
        dedupe_redundant_actions(action, &mut report);
    }
    report
}

fn dedupe_redundant_actions(action: &mut cua_core::InputAction, report: &mut ActionDedupeReport) {
    let cua_core::InputAction::Sequence { actions, .. } = action else {
        return;
    };

    for action in actions.iter_mut() {
        dedupe_redundant_actions(action, report);
    }

    let mut seen_open_apps = Vec::<String>::new();
    let mut seen_text_entries = Vec::<(ActionTextKind, String)>::new();
    actions.retain(|action| {
        let keep = match action {
            cua_core::InputAction::OpenApp { app_name } => {
                let key = app_name.trim().to_ascii_lowercase();
                if seen_open_apps.iter().any(|seen| seen == &key) {
                    false
                } else {
                    seen_open_apps.push(key);
                    true
                }
            }
            cua_core::InputAction::KeyType { text } => {
                retain_first_text_entry(&mut seen_text_entries, ActionTextKind::Type, text)
            }
            cua_core::InputAction::KeyPaste { text } => {
                retain_first_text_entry(&mut seen_text_entries, ActionTextKind::Paste, text)
            }
            cua_core::InputAction::ClipboardWrite { text } => {
                retain_first_text_entry(&mut seen_text_entries, ActionTextKind::Clipboard, text)
            }
            _ => true,
        };
        if !keep {
            report.removed += 1;
        }
        keep
    });
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionTextKind {
    Type,
    Paste,
    Clipboard,
}

fn retain_first_text_entry(
    seen_text_entries: &mut Vec<(ActionTextKind, String)>,
    kind: ActionTextKind,
    text: &str,
) -> bool {
    let key = text.trim().to_string();
    if seen_text_entries
        .iter()
        .any(|(seen_kind, seen_text)| *seen_kind == kind && seen_text == &key)
    {
        return false;
    }
    seen_text_entries.push((kind, key));
    true
}

fn repair_new_note_text_entry_plan(transcript: &str, action: &mut Option<cua_core::InputAction>) {
    if !transcript_requests_new_note(transcript) {
        return;
    }
    let Some(current) = action.take() else {
        return;
    };
    let Some(text_action) = first_text_entry_action(&current) else {
        *action = Some(current);
        return;
    };
    if transcript_mentions_notes(transcript) {
        *action = Some(cua_core::InputAction::Sequence {
            actions: vec![
                cua_core::InputAction::OpenApp {
                    app_name: "Notes".to_string(),
                },
                cua_core::InputAction::KeyPress {
                    combo: "cmd+n".to_string(),
                },
                text_action,
            ],
            inter_action_delay_ms: 120,
        });
    } else if action_contains_cmd_n(&current) {
        *action = Some(current);
    } else {
        *action = Some(cua_core::InputAction::Sequence {
            actions: vec![
                cua_core::InputAction::KeyPress {
                    combo: "cmd+n".to_string(),
                },
                text_action,
            ],
            inter_action_delay_ms: 120,
        });
    }
}

fn first_text_entry_action(action: &cua_core::InputAction) -> Option<cua_core::InputAction> {
    match action {
        cua_core::InputAction::KeyType { .. }
        | cua_core::InputAction::KeyPaste { .. }
        | cua_core::InputAction::ClipboardWrite { .. } => Some(action.clone()),
        cua_core::InputAction::Sequence { actions, .. } => {
            actions.iter().find_map(first_text_entry_action)
        }
        _ => None,
    }
}

fn transcript_mentions_notes(transcript: &str) -> bool {
    transcript
        .split_whitespace()
        .map(normalize_voice_token)
        .any(|word| word == "note" || word == "notes")
}

fn action_satisfies_text_entry_request(action: &cua_core::InputAction) -> bool {
    match action {
        cua_core::InputAction::KeyType { .. }
        | cua_core::InputAction::KeyPaste { .. }
        | cua_core::InputAction::ClipboardWrite { .. }
        | cua_core::InputAction::ShellExec { .. } => true,
        cua_core::InputAction::Sequence { actions, .. } => {
            actions.iter().any(action_satisfies_text_entry_request)
        }
        _ => false,
    }
}

fn action_contains_cmd_n(action: &cua_core::InputAction) -> bool {
    match action {
        cua_core::InputAction::KeyPress { combo } => combo.eq_ignore_ascii_case("cmd+n"),
        cua_core::InputAction::Sequence { actions, .. } => {
            actions.iter().any(action_contains_cmd_n)
        }
        _ => false,
    }
}

fn open_only_incomplete_for_goal(transcript: &str, turn: &CompletedAssistantTurn) -> bool {
    transcript_requests_long_range_work(transcript)
        && turn.action.as_ref().is_some_and(action_is_open_only_setup)
}

fn transcript_requests_long_range_work(transcript: &str) -> bool {
    transcript
        .split_whitespace()
        .map(normalize_voice_token)
        .any(|word| {
            matches!(
                word.as_str(),
                "research"
                    | "search"
                    | "browse"
                    | "browsing"
                    | "web"
                    | "google"
                    | "lookup"
                    | "look"
                    | "find"
                    | "read"
                    | "investigate"
                    | "compare"
                    | "summarize"
                    | "while"
                    | "watching"
            )
        })
}

fn action_is_open_only_setup(action: &serde_json::Value) -> bool {
    match action.get("kind").and_then(|kind| kind.as_str()) {
        Some("open_app") => true,
        Some("sequence") => action
            .get("actions")
            .and_then(|actions| actions.as_array())
            .is_some_and(|actions| {
                !actions.is_empty() && actions.iter().all(action_is_open_only_setup)
            }),
        _ => false,
    }
}

fn input_action_is_open_only_setup(action: &cua_core::InputAction) -> bool {
    match action {
        cua_core::InputAction::OpenApp { .. } => true,
        cua_core::InputAction::Sequence { actions, .. } => {
            !actions.is_empty() && actions.iter().all(input_action_is_open_only_setup)
        }
        _ => false,
    }
}

fn open_only_incomplete_evidence(evidence: Option<serde_json::Value>) -> serde_json::Value {
    let mut evidence = evidence.unwrap_or_else(|| json!({}));
    if let Some(object) = evidence.as_object_mut() {
        object.insert("effect".to_string(), json!("partial"));
        object.insert(
            "reason".to_string(),
            json!("open_app_only_did_not_satisfy_long_range_goal"),
        );
        object.insert(
            "repair_hint".to_string(),
            json!("Continue with browser, shell, visible UI, reading, search, or verification actions."),
        );
        evidence
    } else {
        json!({
            "effect": "partial",
            "reason": "open_app_only_did_not_satisfy_long_range_goal",
            "dispatch_evidence": evidence,
        })
    }
}

fn should_finish_after_reobserve(should_verify: bool, should_continue: bool) -> bool {
    should_verify && !should_continue
}

fn should_continue_long_range_after_verified_action(
    transcript: &str,
    turn: &CompletedAssistantTurn,
    effect: Option<&str>,
    attempt_index: usize,
    loop_budget: AgentLoopBudget,
) -> bool {
    if !loop_budget.can_continue_after(attempt_index) || effect != Some("confirmed") {
        return false;
    }
    if !transcript_requests_long_range_work(transcript) {
        return false;
    }
    let Some(action) = turn.action.as_ref() else {
        return false;
    };
    action_can_advance_long_range_goal(action)
        && !action_is_user_text_fulfillment(transcript, action)
}

fn action_can_advance_long_range_goal(action: &serde_json::Value) -> bool {
    matches!(
        action.get("kind").and_then(|kind| kind.as_str()),
        Some(
            "sequence"
                | "open_app"
                | "mouse_click"
                | "key_press"
                | "key_type"
                | "key_paste"
                | "clipboard_write"
                | "shell_exec"
                | "aegis"
                | "ctx"
        )
    )
}

fn planner_response_claims_pending_work(response: &str) -> bool {
    let lower = response.trim().to_ascii_lowercase();
    [
        "opening",
        "searching",
        "browsing",
        "researching",
        "looking up",
        "checking",
        "reading",
        "i'll",
        "i will",
        "let me",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn long_range_null_plan_is_incomplete(
    transcript: &str,
    response: &str,
    action: &Option<serde_json::Value>,
) -> bool {
    transcript_requests_long_range_work(transcript)
        && action.is_none()
        && planner_response_claims_pending_work(response)
}

fn action_null_finishes_after_prior_attempts(
    response: &str,
    action: &Option<serde_json::Value>,
    prior_attempts: &[PlanAttemptContext],
) -> bool {
    action.is_none()
        && !prior_attempts.is_empty()
        && (final_response_claims_verified_result(response)
            || final_response_reports_prior_failure(response))
}

fn action_repeats_confirmed_attempt(
    attempts: &[PlanAttemptContext],
    action: &serde_json::Value,
) -> bool {
    attempts.iter().any(|attempt| {
        attempt.effect.as_deref() == Some("confirmed")
            && attempt
                .action
                .as_ref()
                .is_some_and(|prior| actions_have_same_intent(prior, action))
    })
}

fn actions_have_same_intent(left: &serde_json::Value, right: &serde_json::Value) -> bool {
    let left_kind = left.get("kind").and_then(|kind| kind.as_str());
    let right_kind = right.get("kind").and_then(|kind| kind.as_str());
    if left_kind != right_kind {
        return false;
    }
    if left_kind == Some("sequence") {
        let Some(left_actions) = left.get("actions").and_then(|actions| actions.as_array()) else {
            return false;
        };
        let Some(right_actions) = right.get("actions").and_then(|actions| actions.as_array())
        else {
            return false;
        };
        return left_actions.len() == right_actions.len()
            && left_actions
                .iter()
                .zip(right_actions)
                .all(|(left, right)| actions_have_same_intent(left, right));
    }
    left == right
}

fn long_range_budget_exhausted_without_finish(
    transcript: &str,
    turn: &CompletedAssistantTurn,
    attempt_index: usize,
    loop_budget: AgentLoopBudget,
) -> bool {
    loop_budget.exhausted_at(attempt_index)
        && transcript_requests_long_range_work(transcript)
        && turn.action.is_some()
}

fn mark_long_range_budget_exhausted_if_needed(
    transcript: &str,
    mut turn: CompletedAssistantTurn,
    attempt_index: usize,
    loop_budget: AgentLoopBudget,
) -> CompletedAssistantTurn {
    if !long_range_budget_exhausted_without_finish(transcript, &turn, attempt_index, loop_budget) {
        return turn;
    }
    let max_attempts = loop_budget
        .max_attempts()
        .expect("budget exhaustion is only possible for finite loop budgets");
    turn.response = format!(
        "I made progress but hit the {max_attempts}-attempt loop budget before completing the full task."
    );
    let prior_evidence = turn.evidence.take();
    turn.evidence = Some(json!({
        "effect": "partial",
        "reason": "long_range_goal_reached_agent_loop_budget",
        "max_attempts": max_attempts,
        "last_evidence": prior_evidence,
    }));
    turn
}

fn completed_from_confirmed_prior_attempt(
    attempts: &[PlanAttemptContext],
) -> Option<CompletedAssistantTurn> {
    let attempt = attempts.last()?;
    if attempt.effect.as_deref() != Some("confirmed") {
        return None;
    }
    let action = attempt.action.as_ref()?;
    if !action_needs_fresh_verification(action) {
        return None;
    }
    Some(CompletedAssistantTurn {
        response: attempt.response.clone(),
        action: attempt.action.clone(),
        evidence: attempt.evidence.clone(),
    })
}

fn attach_loop_evidence(
    mut completed: CompletedAssistantTurn,
    prior_attempts: &[PlanAttemptContext],
) -> CompletedAssistantTurn {
    if prior_attempts.is_empty() {
        return completed;
    }
    let final_attempt_already_recorded =
        prior_attempts_include_completed(&completed, prior_attempts);
    let final_evidence = completed.evidence.take();
    let final_effect = final_evidence
        .as_ref()
        .and_then(|evidence| evidence["effect"].as_str())
        .unwrap_or_else(|| inferred_final_effect(&completed, prior_attempts))
        .to_string();
    let mut attempts = prior_attempts.to_vec();
    if !final_attempt_already_recorded {
        attempts.push(PlanAttemptContext {
            attempt_index: attempts.len() + 1,
            response: completed.response.clone(),
            action: completed.action.clone(),
            effect: Some(final_effect.clone()),
            evidence: final_evidence.clone(),
        });
    }
    completed.evidence = Some(json!({
        "effect": final_effect,
        "final_evidence": final_evidence,
        "attempt_count": attempts.len(),
        "attempts": attempts,
    }));
    completed
}

fn inferred_final_effect(
    completed: &CompletedAssistantTurn,
    prior_attempts: &[PlanAttemptContext],
) -> &'static str {
    if completed.action.is_some() {
        return "unverifiable";
    }
    if prior_attempts
        .iter()
        .any(|attempt| attempt.effect.as_deref() == Some("confirmed"))
        && final_response_claims_verified_result(&completed.response)
    {
        return "confirmed";
    }
    if prior_attempts.iter().any(|attempt| {
        matches!(
            attempt.effect.as_deref(),
            Some("failed" | "refused" | "suspected_noop" | "unverifiable")
        )
    }) && final_response_reports_prior_failure(&completed.response)
    {
        return "failed";
    }
    "stopped"
}

fn final_response_claims_verified_result(response: &str) -> bool {
    let lower = response.to_ascii_lowercase();
    !planner_response_claims_pending_work(response)
        && [
            "verified",
            "result",
            "visible",
            "displayed",
            "shows",
            "title",
            "reads",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}

fn final_response_reports_prior_failure(response: &str) -> bool {
    let lower = response.to_ascii_lowercase();
    !planner_response_claims_pending_work(response)
        && [
            "error:",
            "failed",
            "refused",
            "does not exist",
            "no such file or directory",
            "permission denied",
            "not found",
            "unavailable",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}

fn loop_attempt_count(
    completed: &CompletedAssistantTurn,
    attempts: &[PlanAttemptContext],
) -> usize {
    attempts.len()
        + if prior_attempts_include_completed(completed, attempts) {
            0
        } else {
            1
        }
}

fn prior_attempts_include_completed(
    completed: &CompletedAssistantTurn,
    attempts: &[PlanAttemptContext],
) -> bool {
    attempts.last().is_some_and(|attempt| {
        (attempt.response == completed.response
            && attempt.action == completed.action
            && attempt.evidence == completed.evidence)
            || budget_exhausted_relabels_attempt(completed, attempt)
    })
}

fn budget_exhausted_relabels_attempt(
    completed: &CompletedAssistantTurn,
    attempt: &PlanAttemptContext,
) -> bool {
    let Some(evidence) = completed.evidence.as_ref() else {
        return false;
    };
    evidence["reason"] == "long_range_goal_reached_agent_loop_budget"
        && completed.action == attempt.action
        && evidence.get("last_evidence") == attempt.evidence.as_ref()
}

fn should_replan_after_effect(
    effect: Option<&str>,
    attempt_index: usize,
    loop_budget: AgentLoopBudget,
) -> bool {
    if !loop_budget.can_continue_after(attempt_index) {
        return false;
    }
    matches!(
        effect,
        Some("partial" | "unverifiable" | "suspected_noop" | "refused" | "failed")
    )
}

fn should_replan_after_turn(
    transcript: &str,
    turn: &CompletedAssistantTurn,
    effect: Option<&str>,
    attempt_index: usize,
    loop_budget: AgentLoopBudget,
) -> bool {
    if turn
        .action
        .as_ref()
        .is_some_and(|action| action_is_user_text_fulfillment(transcript, action))
    {
        return false;
    }
    should_replan_after_effect(effect, attempt_index, loop_budget)
        || should_continue_long_range_after_verified_action(
            transcript,
            turn,
            effect,
            attempt_index,
            loop_budget,
        )
}

fn spawn_recording_progress(
    tx: Sender<VoiceUiEvent>,
    running: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let started = Instant::now();
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        while running.load(Ordering::Acquire) {
            interval.tick().await;
            if !running.load(Ordering::Acquire) {
                break;
            }
            tx.send(VoiceUiEvent::Listening {
                ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            })
            .ok();
        }
    })
}

struct VoiceStepPublisher {
    tx: mpsc::UnboundedSender<String>,
    join: tokio::task::JoinHandle<()>,
}

impl VoiceStepPublisher {
    fn start(local: CuaClient) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let join = tokio::spawn(async move {
            let mut session = local.session().await.ok();
            while let Some(mut label) = rx.recv().await {
                while let Ok(newer_label) = rx.try_recv() {
                    label = newer_label;
                }
                let request = async {
                    if let Some(session) = session.as_mut() {
                        session
                            .ui_step(
                                label,
                                Some(VOICE_STEP_SOURCE.to_string()),
                                Some("Voice control".to_string()),
                                None,
                                None,
                                None,
                                Some(VOICE_STEP_TTL_MS),
                            )
                            .await
                    } else {
                        local
                            .ui_step(
                                label,
                                Some(VOICE_STEP_SOURCE.to_string()),
                                Some("Voice control".to_string()),
                                None,
                                None,
                                None,
                                Some(VOICE_STEP_TTL_MS),
                            )
                            .await
                    }
                };
                let _ = tokio::time::timeout(Duration::from_millis(VOICE_STEP_TIMEOUT_MS), request)
                    .await;
            }
        });
        Self { tx, join }
    }

    fn publish(&self, label: impl Into<String>) {
        let _ = self.tx.send(label.into());
    }

    async fn finish(self) {
        drop(self.tx);
        let _ = tokio::time::timeout(
            Duration::from_millis(VOICE_STEP_FLUSH_TIMEOUT_MS),
            self.join,
        )
        .await;
    }
}

fn voice_step_label(kind: &str, value: &str) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = if compact.is_empty() {
        kind.to_string()
    } else {
        format!("{kind}: {compact}")
    };
    truncate_step_label(label)
}

fn truncate_step_label(label: String) -> String {
    if label.chars().count() <= VOICE_STEP_LABEL_MAX {
        return label;
    }
    let mut truncated = label
        .chars()
        .take(VOICE_STEP_LABEL_MAX.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

async fn prefetch_context_for_planning(
    local: CuaClient,
    warm_session: Option<CuaSession>,
) -> PrefetchedContext {
    let started = Instant::now();
    let mut context = prefetch_context(local, warm_session).await;
    context.elapsed = started.elapsed();
    context
}

fn spawn_context_prefetch(local: CuaClient, warm_session: Option<CuaSession>) -> ContextTask {
    tokio::spawn(async move { prefetch_context_for_planning(local, warm_session).await })
}

fn spawn_chat_context_prefetch(profile: String) -> ChatContextTask {
    tokio::spawn(async move { load_chat_context(&profile).await })
}

fn spawn_agent_context(
    profile: String,
    request: String,
    chat_task: Option<ChatContextTask>,
) -> AgentContextTask {
    tokio::spawn(async move {
        let chat = resolve_chat_context(profile.clone(), chat_task).await?;
        load_agent_context_with_chat(&profile, &request, chat).await
    })
}

async fn resolve_chat_context(
    profile: String,
    chat_task: Option<ChatContextTask>,
) -> anyhow::Result<String> {
    if let Some(chat_task) = chat_task {
        return chat_task
            .await
            .context("join chat context prefetch")?
            .context("load chat context");
    }
    load_chat_context(&profile).await
}

async fn resolve_agent_context(
    agent_context_task: AgentContextTask,
) -> anyhow::Result<crate::memory::AgentContext> {
    agent_context_task
        .await
        .context("join ctx agent context")?
        .context("load ctx agent context")
}

async fn abort_context_prefetch(context_task: &ContextTask, trace: &VoiceTurnTrace, reason: &str) {
    context_task.abort();
    trace
        .append("context_prefetch_aborted", json!({ "reason": reason }))
        .await;
}

async fn abort_chat_context_prefetch(
    chat_task: &ChatContextTask,
    trace: &VoiceTurnTrace,
    reason: &str,
) {
    chat_task.abort();
    trace
        .append("chat_context_prefetch_aborted", json!({ "reason": reason }))
        .await;
}

async fn resolve_context_for_planning(
    local: CuaClient,
    context_task: Option<ContextTask>,
) -> PrefetchedContext {
    if let Some(context_task) = context_task {
        match context_task.await {
            Ok(context) => return context,
            Err(error) => {
                let mut context = prefetch_context_for_planning(local, None).await;
                context
                    .errors
                    .insert(0, format!("context prefetch join failed: {error}"));
                return context;
            }
        }
    }
    prefetch_context_for_planning(local, None).await
}

async fn persist_turn_memory(
    config: &VoiceConfig,
    transcript: &str,
    completed: &CompletedAssistantTurn,
    trace: &VoiceTurnTrace,
) -> anyhow::Result<()> {
    let started = Instant::now();
    let turn_id = trace.turn_id.clone();
    let chat_store = ChatStore::new(config.profile.clone())?;
    let ctx_memory = CtxMemory::new(config.profile.clone())?;
    let append_chat = chat_store.append_turn(
        &turn_id,
        transcript,
        &completed.response,
        completed.action.as_ref(),
        completed.evidence.as_ref(),
        &config.planner_model,
    );
    let remember_ctx = ctx_memory.remember_chat_turn(transcript, &completed.response);
    tokio::try_join!(append_chat, remember_ctx)?;
    trace
        .append(
            "memory_persisted",
            json!({"elapsed_ms": elapsed_ms(started.elapsed())}),
        )
        .await;
    Ok(())
}

async fn preflight_local_client(profile: &str) -> anyhow::Result<LocalReady> {
    let local = CuaClient::new(profile.to_string()).await?;
    if let Ok(session) = local.session().await {
        return Ok(LocalReady {
            client: local,
            session: Some(session),
        });
    }
    spawn_profile_daemon(profile).context("start bundled cua daemon")?;
    wait_until_ready(Duration::from_secs(2), || {
        let local = local.clone();
        async move {
            local
                .session()
                .await
                .map(|_| ())
                .map_err(anyhow::Error::from)
        }
    })
    .await
    .context("voice requires a running cua daemon on the profile Unix socket")?;
    let session = local.session().await.ok();
    Ok(LocalReady {
        client: local,
        session,
    })
}

fn spawn_local_preflight(profile: String) -> LocalTask {
    tokio::spawn(async move { preflight_local_client(&profile).await })
}

fn send_metric(tx: &Sender<VoiceUiEvent>, name: &'static str, elapsed: Duration) {
    tx.send(VoiceUiEvent::Metric {
        name: name.to_string(),
        ms: elapsed.as_millis().min(u128::from(u64::MAX)) as u64,
    })
    .ok();
}

#[derive(Debug, Clone)]
struct VoiceTurnTrace {
    enabled: bool,
    path: Option<PathBuf>,
    turn_id: String,
}

impl VoiceTurnTrace {
    fn new(config: &VoiceConfig) -> Self {
        let sequence = VOICE_TRACE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self {
            enabled: config.debug_trace,
            path: config
                .debug_trace
                .then(|| voice_trace_path(&config.profile))
                .flatten(),
            turn_id: format!("{}-{}-{sequence}", std::process::id(), wall_ms()),
        }
    }

    async fn append(&self, event: &'static str, data: serde_json::Value) {
        if !self.enabled {
            return;
        }
        let Some(path) = self.path.as_ref() else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(error) = tokio::fs::create_dir_all(parent).await {
                report_voice_trace_error("create trace directory", parent, &error);
                return;
            }
        }
        let line = json!({
            "schema_version": "cua.voice_trace.v1",
            "turn_id": self.turn_id,
            "event": event,
            "at_wall_ms": wall_ms(),
            "data": data,
        });
        let mut file = match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
        {
            Ok(file) => file,
            Err(error) => {
                report_voice_trace_error("open trace file", path, &error);
                return;
            }
        };
        use tokio::io::AsyncWriteExt;
        if let Err(error) = file.write_all(line.to_string().as_bytes()).await {
            report_voice_trace_error("write trace line", path, &error);
            return;
        }
        if let Err(error) = file.write_all(b"\n").await {
            report_voice_trace_error("write trace newline", path, &error);
            return;
        }
        if let Err(error) = file.flush().await {
            report_voice_trace_error("flush trace file", path, &error);
        }
    }

    async fn write_artifact(&self, name: &'static str, bytes: &[u8], data: serde_json::Value) {
        if !self.enabled {
            return;
        }
        let Some(path) = self.artifact_path(name) else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(error) = tokio::fs::create_dir_all(parent).await {
                report_voice_trace_error("create artifact directory", parent, &error);
                return;
            }
        }
        if let Err(error) = tokio::fs::write(&path, bytes).await {
            report_voice_trace_error("write trace artifact", &path, &error);
            return;
        }
        self.append(
            "artifact",
            json!({
                "path": path.display().to_string(),
                "data": data,
            }),
        )
        .await;
    }

    fn artifact_path(&self, name: &str) -> Option<PathBuf> {
        let parent = self.path.as_ref()?.parent()?;
        Some(
            parent
                .join("voice-turn-artifacts")
                .join(&self.turn_id)
                .join(name),
        )
    }
}

fn report_voice_trace_error(action: &str, path: &std::path::Path, error: &dyn std::fmt::Display) {
    eprintln!(
        "cua voice debug trace {action} failed for {}: {error}",
        path.display()
    );
}

fn voice_trace_path(profile: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CUA_VOICE_TRACE_PATH") {
        return Some(PathBuf::from(path));
    }
    profile_voice_trace_path(profile).ok()
}

fn wall_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn elapsed_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn trace_config(config: &VoiceConfig) -> serde_json::Value {
    json!({
        "profile": &config.profile,
        "stt_backend": &config.stt_backend,
        "stt_model": &config.stt_model,
        "planner_model": &config.planner_model,
    })
}

fn audio_trace_json(audio: &RecordedAudio) -> serde_json::Value {
    json!({
        "device_name": &audio.device_name,
        "channels": audio.channels,
        "sample_format": &audio.sample_format,
        "sample_rate": audio.sample_rate,
        "duration_ms": elapsed_ms(audio.duration),
        "peak_amplitude": audio.peak_amplitude,
        "rms_amplitude": audio.rms_amplitude,
        "wav_bytes": audio.wav_bytes.len(),
    })
}

fn transcript_trace_json(transcript: &SttTranscript, elapsed: Duration) -> serde_json::Value {
    let usage = transcript.usage.as_ref().map(|usage| {
        json!({
            "seconds": usage.seconds,
            "cost": usage.cost,
        })
    });
    json!({
        "text": &transcript.text,
        "class": transcript_class(&transcript.text),
        "backend": &transcript.backend,
        "model": &transcript.model,
        "generation_id": transcript.generation_id.as_deref(),
        "usage": usage,
        "elapsed_ms": elapsed_ms(elapsed),
    })
}

fn frame_trace_json(frame: &FramePayload) -> serde_json::Value {
    let envelope = &frame.envelope;
    json!({
        "frame_id": envelope.frame_id,
        "display_id": &envelope.display_id,
        "timestamp_wall_ms": envelope.timestamp_wall_ms,
        "width": envelope.width,
        "height": envelope.height,
        "display_width": envelope.display_width,
        "display_height": envelope.display_height,
        "encoding": &envelope.encoding,
        "byte_len": envelope.byte_len,
        "sha256": &envelope.sha256,
        "has_bytes": frame.bytes_base64.is_some(),
    })
}

fn plan_trace_json(model: &str, elapsed: Duration, plan: &PlannedTurn) -> serde_json::Value {
    json!({
        "model": model,
        "elapsed_ms": elapsed_ms(elapsed),
        "response": &plan.response,
        "action": &plan.action,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[tokio::test]
    async fn recording_progress_emits_elapsed_listening_updates() {
        let (tx, rx) = channel();
        let running = Arc::new(AtomicBool::new(true));
        let task = spawn_recording_progress(tx, running.clone());

        tokio::time::sleep(Duration::from_millis(260)).await;
        running.store(false, Ordering::Release);
        task.abort();

        let updates = rx
            .try_iter()
            .filter_map(|event| match event {
                VoiceUiEvent::Listening { ms } => Some(ms),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert!(updates.len() >= 2, "{updates:?}");
        assert!(updates.windows(2).all(|pair| pair[1] >= pair[0]));
        assert!(updates.iter().any(|ms| *ms > 0));
    }

    #[test]
    fn voice_step_label_compacts_and_bounds_turn_text() {
        assert_eq!(
            voice_step_label("transcript", " click   the center target "),
            "transcript: click the center target"
        );

        let label = voice_step_label("reply", &"done ".repeat(40));
        assert!(label.chars().count() <= VOICE_STEP_LABEL_MAX);
        assert!(label.ends_with("..."));
    }

    #[test]
    fn recorded_audio_validation_rejects_only_short_audio() {
        let short = RecordedAudio {
            device_name: "test".to_string(),
            channels: 1,
            sample_format: "I16".to_string(),
            sample_rate: 16_000,
            wav_bytes: vec![0; 44],
            duration: Duration::from_millis(500),
            peak_amplitude: 2_000,
            rms_amplitude: 0.05,
        };
        let silent = RecordedAudio {
            device_name: "test".to_string(),
            channels: 1,
            sample_format: "I16".to_string(),
            sample_rate: 16_000,
            wav_bytes: vec![0; 44],
            duration: Duration::from_millis(800),
            peak_amplitude: 8,
            rms_amplitude: 0.00001,
        };
        let low_rms_but_audible_peak = RecordedAudio {
            device_name: "test".to_string(),
            channels: 1,
            sample_format: "I16".to_string(),
            sample_rate: 48_000,
            wav_bytes: vec![0; 44],
            duration: Duration::from_millis(800),
            peak_amplitude: 45,
            rms_amplitude: 0.00001,
        };
        let speech = RecordedAudio {
            device_name: "test".to_string(),
            channels: 1,
            sample_format: "I16".to_string(),
            sample_rate: 16_000,
            wav_bytes: vec![0; 44],
            duration: Duration::from_millis(800),
            peak_amplitude: 2_000,
            rms_amplitude: 0.01,
        };
        let quiet_but_real_speech = RecordedAudio {
            device_name: "test".to_string(),
            channels: 1,
            sample_format: "I16".to_string(),
            sample_rate: 48_000,
            wav_bytes: vec![0; 44],
            duration: Duration::from_millis(800),
            peak_amplitude: 106,
            rms_amplitude: 0.00095,
        };

        assert!(validate_recorded_audio(&short).is_err());
        assert!(validate_recorded_audio(&silent).is_ok());
        assert!(validate_recorded_audio(&low_rms_but_audible_peak).is_ok());
        assert!(validate_recorded_audio(&speech).is_ok());
        assert!(validate_recorded_audio(&quiet_but_real_speech).is_ok());
    }

    #[test]
    fn transcript_class_identifies_command_candidates_and_hallucinations() {
        assert_eq!(transcript_class("you"), "missed_speech");
        assert_eq!(transcript_class("Thank you."), "missed_speech");
        assert_eq!(
            transcript_class("Thank you for joining us today."),
            "missed_speech"
        );
        assert_eq!(
            transcript_class("You're welcome! Let me know if you need anything."),
            "missed_speech"
        );
        assert_eq!(transcript_class("settings"), "command_candidate");
        assert_eq!(
            transcript_class("click the center target"),
            "command_candidate"
        );
    }

    #[test]
    fn stt_validation_classifies_you_as_missed_speech() {
        let transcript = SttTranscript {
            text: "You.".to_string(),
            backend: "openrouter".to_string(),
            model: DEFAULT_STT_MODEL.to_string(),
            generation_id: Some("gen_test".to_string()),
            usage: None,
        };
        let error = validate_stt_transcript(&transcript).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("model=tiny.en"));
        assert!(message.contains("generation_id=gen_test"));
        assert!(message.contains("class=missed_speech"));
        assert!(!message.contains("You"));
        assert_eq!(user_visible_turn_error(&error), "Didn't catch a command.");
    }

    #[test]
    fn user_visible_error_names_planner_parse_failure() {
        let error = anyhow::anyhow!("parse plan JSON: expected value");

        assert_eq!(
            user_visible_turn_error(&error),
            "Planner returned invalid action JSON."
        );
    }

    #[test]
    fn user_visible_error_names_empty_planner_response() {
        let error = anyhow::anyhow!("planning model returned empty content");

        assert_eq!(
            user_visible_turn_error(&error),
            "Planner returned empty content."
        );
        assert!(planning_error_is_empty_content(&error));
    }

    #[test]
    fn planner_parse_failures_are_recoverable_for_deterministic_browser_bootstrap() {
        let error = anyhow::anyhow!(
            "model output was not valid action JSON: parse action: invalid type: null"
        );
        let parse_plan = anyhow::anyhow!("parse plan JSON: expected value");

        assert!(planning_error_is_invalid_action_json(&error));
        assert!(planning_error_is_invalid_action_json(&parse_plan));
    }

    #[test]
    fn user_visible_error_hides_local_stt_tracebacks() {
        let error = anyhow::anyhow!(
            "local speech-to-text fallback after primary failure: No such file or directory: 'ffmpeg'"
        );

        assert_eq!(
            user_visible_turn_error(&error),
            "Local speech-to-text needs attention."
        );
    }

    #[test]
    fn local_stt_does_not_require_openrouter_key() {
        assert_eq!(
            api_key_for_stt_backend_from("local", || None).unwrap(),
            None
        );
        assert_eq!(
            api_key_for_stt_backend_from("local", || Some("test-key".to_string())).unwrap(),
            Some("test-key".to_string())
        );
    }

    #[test]
    fn openrouter_stt_requires_openrouter_key() {
        let error = api_key_for_stt_backend_from("openrouter", || None).unwrap_err();

        assert!(format!("{error:#}").contains("OpenRouter speech-to-text"));
        assert_eq!(
            api_key_for_stt_backend_from("openrouter", || Some("test-key".to_string())).unwrap(),
            Some("test-key".to_string())
        );
    }

    #[test]
    fn stt_diagnostic_uses_class_without_promoting_transcript() {
        let transcript = SttTranscript {
            text: "you".to_string(),
            backend: "openrouter".to_string(),
            model: DEFAULT_STT_MODEL.to_string(),
            generation_id: None,
            usage: None,
        };
        let (tx, rx) = std::sync::mpsc::channel();

        publish_stt_diagnostic(&tx, &transcript);

        assert_eq!(
            rx.recv().unwrap(),
            VoiceUiEvent::SttDiagnostic {
                backend: "openrouter".to_string(),
                model: DEFAULT_STT_MODEL.to_string(),
                generation_id: None,
                audio_ms: None,
                transcript_class: "missed_speech".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn debug_trace_is_off_by_default() {
        let trace_path = std::env::temp_dir().join(format!(
            "cua-trace-off-{}-{}.jsonl",
            std::process::id(),
            wall_ms()
        ));
        VoiceTurnTrace {
            enabled: false,
            path: Some(trace_path.clone()),
            turn_id: "test-turn".to_string(),
        }
        .append("test_event", json!({"ok": true}))
        .await;

        assert!(!trace_path.exists());
    }

    #[tokio::test]
    async fn debug_trace_writes_jsonl_when_enabled() {
        let trace_path = std::env::temp_dir().join(format!(
            "cua-trace-on-{}-{}.jsonl",
            std::process::id(),
            wall_ms()
        ));
        VoiceTurnTrace {
            enabled: true,
            path: Some(trace_path.clone()),
            turn_id: "test-turn".to_string(),
        }
        .append("test_event", json!({"ok": true}))
        .await;

        let contents = std::fs::read_to_string(&trace_path).unwrap();
        let value: serde_json::Value = serde_json::from_str(contents.trim()).unwrap();
        assert_eq!(value["schema_version"], "cua.voice_trace.v1");
        assert_eq!(value["event"], "test_event");
        assert_eq!(value["data"]["ok"], true);
        let _ = std::fs::remove_file(trace_path);
    }

    #[tokio::test]
    async fn planning_context_can_resolve_from_prefetch_task() {
        let local = CuaClient::new(format!("prefetch-test-{}", uuid::Uuid::new_v4()))
            .await
            .unwrap();
        let expected_elapsed = Duration::from_millis(7);
        let task = tokio::spawn(async move {
            PrefetchedContext {
                session: None,
                frame: None,
                desktop: None,
                errors: Vec::new(),
                elapsed: expected_elapsed,
            }
        });

        let context = resolve_context_for_planning(local, Some(task)).await;

        assert_eq!(context.elapsed, expected_elapsed);
        assert!(context.session.is_none());
        assert!(context.frame.is_none());
        assert!(context.desktop.is_none());
    }

    #[tokio::test]
    async fn planning_context_records_prefetch_join_failure() {
        let local = CuaClient::new(format!("prefetch-join-test-{}", uuid::Uuid::new_v4()))
            .await
            .unwrap();
        let task = tokio::spawn(async {
            panic!("synthetic context prefetch failure");
            #[allow(unreachable_code)]
            PrefetchedContext {
                session: None,
                frame: None,
                desktop: None,
                errors: Vec::new(),
                elapsed: Duration::ZERO,
            }
        });

        let context = resolve_context_for_planning(local, Some(task)).await;

        assert!(
            context
                .errors
                .first()
                .is_some_and(|error| error.contains("context prefetch join failed")),
            "errors were {:?}",
            context.errors
        );
    }

    #[tokio::test]
    async fn chat_context_can_resolve_from_prefetch_task() {
        let task = tokio::spawn(async { Ok("Recent chat:\nuser: cached".to_string()) });

        let context = resolve_chat_context("unused-profile".to_string(), Some(task))
            .await
            .unwrap();

        assert_eq!(context, "Recent chat:\nuser: cached");
    }

    #[tokio::test]
    async fn aborting_context_prefetch_cancels_background_work() {
        let task = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            PrefetchedContext {
                session: None,
                frame: None,
                desktop: None,
                errors: Vec::new(),
                elapsed: Duration::from_secs(60),
            }
        });
        let trace = VoiceTurnTrace {
            enabled: false,
            path: None,
            turn_id: "test-turn".to_string(),
        };

        abort_context_prefetch(&task, &trace, "test").await;
        let result = task.await;

        assert!(result.is_err_and(|error| error.is_cancelled()));
    }

    #[tokio::test]
    async fn aborting_chat_context_prefetch_cancels_background_work() {
        let task = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok("late chat".to_string())
        });
        let trace = VoiceTurnTrace {
            enabled: false,
            path: None,
            turn_id: "test-turn".to_string(),
        };

        abort_chat_context_prefetch(&task, &trace, "test").await;
        let result = task.await;

        assert!(result.is_err_and(|error| error.is_cancelled()));
    }

    #[test]
    fn context_prefetch_timeout_is_bounded_for_voice_latency() {
        std::env::set_var("CUA_VOICE_CONTEXT_PREFETCH_TIMEOUT_MS", "1");
        assert_eq!(context_prefetch_timeout(), Duration::from_millis(250));

        std::env::set_var("CUA_VOICE_CONTEXT_PREFETCH_TIMEOUT_MS", "60000");
        assert_eq!(context_prefetch_timeout(), Duration::from_millis(30_000));

        std::env::remove_var("CUA_VOICE_CONTEXT_PREFETCH_TIMEOUT_MS");
        assert_eq!(
            context_prefetch_timeout(),
            Duration::from_millis(DEFAULT_CONTEXT_PREFETCH_TIMEOUT_MS)
        );
    }

    #[test]
    fn voice_step_flush_does_not_hold_turn_completion() {
        const { assert!(VOICE_STEP_FLUSH_TIMEOUT_MS <= 150) };
    }

    #[test]
    fn voice_step_request_timeout_stays_latency_oriented() {
        const { assert!(VOICE_STEP_TIMEOUT_MS <= 150) };
    }

    #[test]
    fn agent_loop_attempt_budget_defaults_to_unbounded_and_accepts_operator_overrides() {
        std::env::remove_var("CUA_AGENT_LOOP_MAX_ATTEMPTS");
        assert_eq!(agent_loop_budget(), AgentLoopBudget::Unbounded);

        std::env::set_var("CUA_AGENT_LOOP_MAX_ATTEMPTS", "12");
        assert_eq!(
            agent_loop_budget(),
            AgentLoopBudget::Finite { max_attempts: 12 }
        );

        std::env::set_var("CUA_AGENT_LOOP_MAX_ATTEMPTS", "n");
        assert_eq!(agent_loop_budget(), AgentLoopBudget::Unbounded);

        std::env::set_var("CUA_AGENT_LOOP_MAX_ATTEMPTS", " N ");
        assert_eq!(agent_loop_budget(), AgentLoopBudget::Unbounded);

        std::env::set_var("CUA_AGENT_LOOP_MAX_ATTEMPTS", "30");
        assert_eq!(
            agent_loop_budget(),
            AgentLoopBudget::Finite { max_attempts: 30 }
        );

        std::env::set_var("CUA_AGENT_LOOP_MAX_ATTEMPTS", "0");
        assert_eq!(agent_loop_budget(), AgentLoopBudget::Unbounded);

        std::env::set_var("CUA_AGENT_LOOP_MAX_ATTEMPTS", "not-a-number");
        assert_eq!(agent_loop_budget(), AgentLoopBudget::Unbounded);
        std::env::remove_var("CUA_AGENT_LOOP_MAX_ATTEMPTS");
    }

    #[test]
    fn default_planner_model_uses_latest_gemini_flash() {
        assert_eq!(DEFAULT_PLANNER_MODEL, "google/gemini-3.7-flash");
    }

    #[test]
    fn agent_loop_replans_only_for_recoverable_effects_inside_budget() {
        for effect in [
            "partial",
            "unverifiable",
            "suspected_noop",
            "refused",
            "failed",
        ] {
            assert!(should_replan_after_effect(
                Some(effect),
                1,
                AgentLoopBudget::Finite { max_attempts: 3 }
            ));
        }
        for effect in ["confirmed", "sent", "unknown"] {
            assert!(!should_replan_after_effect(
                Some(effect),
                1,
                AgentLoopBudget::Finite { max_attempts: 3 }
            ));
        }
        assert!(!should_replan_after_effect(
            None,
            1,
            AgentLoopBudget::Finite { max_attempts: 3 }
        ));
        assert!(!should_replan_after_effect(
            Some("suspected_noop"),
            3,
            AgentLoopBudget::Finite { max_attempts: 3 }
        ));
        assert!(should_replan_after_effect(
            Some("suspected_noop"),
            30,
            AgentLoopBudget::Unbounded
        ));
    }

    #[test]
    fn planner_context_includes_chat_ctx_scratchpads_and_runtime_errors() {
        let context = combined_agent_context(
            "Recent chat:\nuser: previous request",
            "Context:\nselected_memory: prefer Aegis",
            "Scratchpads:\nactive-goal",
            &["context snapshot failed: timeout".to_string()],
            &["ctx exited 2: missing index".to_string()],
        );

        assert!(context.contains("Recent chat:"));
        assert!(context.contains("selected_memory"));
        assert!(context.contains("Scratchpads:"));
        assert!(context.contains("Runtime context errors visible to the agent"));
        assert!(context.contains("context snapshot failed"));
        assert!(context.contains("ctx exited 2"));
    }

    #[test]
    fn agent_loop_never_replays_text_entry_side_effects() {
        let turn = CompletedAssistantTurn {
            response: "Writing the note.".to_string(),
            action: Some(json!({
                "kind": "sequence",
                "actions": [
                    {"kind": "open_app", "app_name": "Notes"},
                    {"kind": "key_press", "combo": "cmd+n"},
                    {"kind": "key_paste", "text": "Once upon a time"}
                ]
            })),
            evidence: Some(json!({"effect": "unverifiable"})),
        };

        for effect in ["partial", "unverifiable", "suspected_noop", "refused"] {
            assert!(!should_replan_after_turn(
                "Write me a note",
                &turn,
                Some(effect),
                1,
                AgentLoopBudget::Finite { max_attempts: 3 }
            ));
        }
    }

    #[test]
    fn agent_loop_still_replans_non_text_recoverable_effects() {
        let turn = CompletedAssistantTurn {
            response: "Opening Notes.".to_string(),
            action: Some(json!({"kind": "open_app", "app_name": "Notes"})),
            evidence: Some(json!({"effect": "suspected_noop"})),
        };

        assert!(should_replan_after_turn(
            "Open Notes",
            &turn,
            Some("suspected_noop"),
            1,
            AgentLoopBudget::Finite { max_attempts: 3 }
        ));
    }

    #[test]
    fn edge_actions_require_fresh_verification_before_final_reply() {
        for action in [
            json!({"kind": "sequence", "actions": []}),
            json!({"kind": "key_type", "text": "hello"}),
            json!({"kind": "key_paste", "text": "hello"}),
            json!({"kind": "shell_exec", "command": "pwd"}),
            json!({"kind": "aegis", "args": ["--help"]}),
            json!({"kind": "ctx", "args": ["query", "default", "cua"]}),
        ] {
            assert!(action_needs_fresh_verification(&action));
        }
        assert!(!action_needs_fresh_verification(
            &json!({"kind": "open_app", "app_name": "Calculator"})
        ));
        assert!(!action_needs_fresh_verification(
            &json!({"kind": "mouse_click", "x": 1, "y": 2})
        ));
    }

    #[test]
    fn text_entry_actions_are_detected_inside_sequences() {
        assert!(action_is_text_entry(
            &json!({"kind": "key_paste", "text": "hello"})
        ));
        assert!(action_is_text_entry(&json!({
            "kind": "sequence",
            "actions": [
                {"kind": "open_app", "app_name": "Notes"},
                {"kind": "key_press", "combo": "cmd+n"},
                {"kind": "key_paste", "text": "hello"}
            ]
        })));
        assert!(!action_is_text_entry(&json!({
            "kind": "sequence",
            "actions": [
                {"kind": "open_app", "app_name": "Notes"},
                {"kind": "key_press", "combo": "cmd+n"}
            ]
        })));
    }

    #[test]
    fn duplicate_side_effects_are_removed_inside_sequences() {
        let mut action = Some(cua_core::InputAction::Sequence {
            actions: vec![
                cua_core::InputAction::OpenApp {
                    app_name: "Notes".to_string(),
                },
                cua_core::InputAction::OpenApp {
                    app_name: "Notes".to_string(),
                },
                cua_core::InputAction::KeyPress {
                    combo: "cmd+n".to_string(),
                },
                cua_core::InputAction::KeyPress {
                    combo: "cmd+n".to_string(),
                },
                cua_core::InputAction::KeyPaste {
                    text: "Once upon a time".to_string(),
                },
                cua_core::InputAction::KeyPaste {
                    text: "Once upon a time".to_string(),
                },
            ],
            inter_action_delay_ms: 120,
        });

        let report = dedupe_redundant_sequence_actions(&mut action);

        assert_eq!(report.removed, 2);
        let Some(cua_core::InputAction::Sequence { actions, .. }) = action else {
            panic!("expected sequence");
        };
        assert_eq!(actions.len(), 4);
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, cua_core::InputAction::KeyPress { .. }))
                .count(),
            2
        );
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, cua_core::InputAction::KeyPaste { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn text_entry_requests_reject_open_only_plans() {
        assert!(transcript_requests_text_entry(
            "Open Notes and write me a note that says hello"
        ));
        assert!(transcript_requests_text_entry(
            "Make a new note that says hello"
        ));
        assert!(transcript_requests_text_entry(
            "Leave a message that says hello"
        ));
        assert!(!transcript_requests_text_entry(
            "Research the web and verify the page content"
        ));
        assert!(!transcript_requests_text_entry(
            "Read the page and summarize what it says"
        ));
        assert!(!transcript_requests_text_entry("Read the story aloud"));
        assert!(!transcript_requests_text_entry("Find the map marker"));
        assert!(!transcript_requests_text_entry("Open Notes"));
        assert!(!transcript_requests_text_entry("Open Messages"));
        assert!(!action_satisfies_text_entry_request(
            &cua_core::InputAction::OpenApp {
                app_name: "Notes".to_string(),
            }
        ));
        assert!(action_satisfies_text_entry_request(
            &cua_core::InputAction::Sequence {
                actions: vec![
                    cua_core::InputAction::OpenApp {
                        app_name: "Notes".to_string(),
                    },
                    cua_core::InputAction::KeyPaste {
                        text: "hello".to_string(),
                    },
                ],
                inter_action_delay_ms: 120,
            }
        ));
    }

    #[test]
    fn long_range_requests_replan_after_open_only_setup() {
        let turn = CompletedAssistantTurn {
            response: "Opening Safari.".to_string(),
            action: Some(json!({"kind": "open_app", "app_name": "Safari"})),
            evidence: Some(json!({"effect": "confirmed"})),
        };

        assert!(open_only_incomplete_for_goal(
            "Open Safari and do some research while I watch",
            &turn
        ));
        assert!(should_replan_after_turn(
            "Open Safari and do some research while I watch",
            &turn,
            Some("partial"),
            1,
            AgentLoopBudget::Finite { max_attempts: 3 }
        ));
        assert!(!open_only_incomplete_for_goal("Open Safari", &turn));
    }

    #[test]
    fn long_range_browser_research_expands_typed_open_only_setup_before_dispatch() {
        let action = cua_core::InputAction::OpenApp {
            app_name: "Safari".to_string(),
        };

        assert!(input_action_is_open_only_setup(&action));
        assert!(transcript_requests_long_range_work(
            "Open Safari and search for the official Gemini documentation"
        ));
    }

    #[test]
    fn agent_loop_long_range_confirmed_tool_actions_continue_after_reobserve() {
        let turn = CompletedAssistantTurn {
            response: "Searching with Aegis.".to_string(),
            action: Some(json!({
                "kind": "aegis",
                "args": ["--mode", "headful", "page", "goto", "https://example.com"]
            })),
            evidence: Some(json!({"effect": "confirmed"})),
        };

        assert!(should_continue_long_range_after_verified_action(
            "Research cloud computer agents and summarize the options",
            &turn,
            Some("confirmed"),
            1,
            AgentLoopBudget::Finite { max_attempts: 5 }
        ));
        assert!(should_replan_after_turn(
            "Research cloud computer agents and summarize the options",
            &turn,
            Some("confirmed"),
            1,
            AgentLoopBudget::Finite { max_attempts: 5 }
        ));
        assert!(!should_continue_long_range_after_verified_action(
            "Open the browser",
            &turn,
            Some("confirmed"),
            1,
            AgentLoopBudget::Finite { max_attempts: 5 }
        ));
        assert!(!should_continue_long_range_after_verified_action(
            "Research cloud computer agents",
            &turn,
            Some("confirmed"),
            5,
            AgentLoopBudget::Finite { max_attempts: 5 }
        ));
        assert!(should_continue_long_range_after_verified_action(
            "Research cloud computer agents",
            &turn,
            Some("confirmed"),
            50,
            AgentLoopBudget::Unbounded
        ));
    }

    #[test]
    fn agent_loop_treats_null_action_progress_claims_as_incomplete() {
        assert!(planner_response_claims_pending_work(
            "Opening Safari and searching for the official Gemini page."
        ));
        assert!(planner_response_claims_pending_work(
            "Let me check the browser and read the page title."
        ));
        assert!(!planner_response_claims_pending_work(
            "The verified title is Gemini models."
        ));
    }

    #[test]
    fn agent_loop_long_range_text_entry_actions_continue_to_verification() {
        let turn = CompletedAssistantTurn {
            response: "Writing the summary.".to_string(),
            action: Some(json!({
                "kind": "sequence",
                "actions": [
                    {"kind": "open_app", "app_name": "Notes"},
                    {"kind": "key_paste", "text": "Summary"}
                ]
            })),
            evidence: Some(json!({"effect": "confirmed"})),
        };

        assert!(should_continue_long_range_after_verified_action(
            "Research cloud computer agents, write the summary in Notes, then read it back to verify it",
            &turn,
            Some("confirmed"),
            1,
            AgentLoopBudget::Finite { max_attempts: 5 }
        ));
        assert!(should_replan_after_turn(
            "Research cloud computer agents, write the summary in Notes, then read it back to verify it",
            &turn,
            Some("confirmed"),
            1,
            AgentLoopBudget::Finite { max_attempts: 5 }
        ));
    }

    #[test]
    fn agent_loop_long_range_standalone_text_entry_continues_to_verification() {
        let turn = CompletedAssistantTurn {
            response: "Pasting the requested text.".to_string(),
            action: Some(json!({"kind": "key_paste", "text": "Summary"})),
            evidence: Some(json!({"effect": "confirmed"})),
        };

        assert!(should_continue_long_range_after_verified_action(
            "Paste Summary into Notes, then read it back to verify it",
            &turn,
            Some("confirmed"),
            1,
            AgentLoopBudget::Finite { max_attempts: 5 }
        ));
        assert!(should_replan_after_turn(
            "Paste Summary into Notes, then read it back to verify it",
            &turn,
            Some("confirmed"),
            1,
            AgentLoopBudget::Finite { max_attempts: 5 }
        ));
    }

    #[test]
    fn agent_loop_browser_search_text_entry_continues_after_reobserve() {
        let turn = CompletedAssistantTurn {
            response: "Searching the web.".to_string(),
            action: Some(json!({
                "kind": "sequence",
                "actions": [
                    {"kind": "mouse_click", "x": 510, "y": 204, "button": "left", "count": 1},
                    {"kind": "key_paste", "text": "official Gemini 3.7 Flash documentation"},
                    {"kind": "key_press", "combo": "enter"}
                ],
                "inter_action_delay_ms": 120
            })),
            evidence: Some(json!({"effect": "confirmed"})),
        };

        assert!(turn
            .action
            .as_ref()
            .is_some_and(action_is_browser_research_setup));
        assert!(should_continue_long_range_after_verified_action(
            "Open Safari and search for official Gemini docs, then read the title",
            &turn,
            Some("confirmed"),
            1,
            AgentLoopBudget::Unbounded
        ));
        assert!(should_replan_after_turn(
            "Open Safari and search for official Gemini docs, then read the title",
            &turn,
            Some("confirmed"),
            1,
            AgentLoopBudget::Unbounded
        ));
    }

    #[test]
    fn agent_loop_calculator_input_continues_to_read_result() {
        let turn = CompletedAssistantTurn {
            response: "Opening Calculator and calculating 123 + 456.".to_string(),
            action: Some(json!({
                "kind": "sequence",
                "actions": [
                    {"kind": "open_app", "app_name": "Calculator"},
                    {"kind": "key_type", "text": "123+456="}
                ],
                "inter_action_delay_ms": 300
            })),
            evidence: Some(json!({"effect": "confirmed"})),
        };

        assert!(!turn
            .action
            .as_ref()
            .is_some_and(|action| action_is_user_text_fulfillment(
                "Open Calculator, calculate 123 plus 456, read the displayed result, and report the verified result.",
                action
            )));
        assert!(should_replan_after_turn(
            "Open Calculator, calculate 123 plus 456, read the displayed result, and report the verified result.",
            &turn,
            Some("confirmed"),
            1,
            AgentLoopBudget::Unbounded
        ));
    }

    #[test]
    fn action_reply_text_does_not_append_dispatch_effect() {
        let turn = CompletedAssistantTurn {
            response: "Opening Calculator and calculating 123 + 456.".to_string(),
            action: Some(json!({
                "kind": "key_type",
                "text": "123+456="
            })),
            evidence: Some(json!({"effect": "confirmed"})),
        };

        assert_eq!(
            user_visible_reply_text(&turn),
            "Opening Calculator and calculating 123 + 456."
        );
    }

    #[test]
    fn long_range_null_pending_reply_is_rejected_as_incomplete() {
        assert!(long_range_null_plan_is_incomplete(
            "Open Calculator, calculate 123 plus 456, read the displayed result, and report the verified result.",
            "Opening Calculator via Spotlight and typing 123",
            &None
        ));
        assert!(!long_range_null_plan_is_incomplete(
            "Open Calculator, calculate 123 plus 456, read the displayed result, and report the verified result.",
            "The displayed result is 579.",
            &None
        ));
    }

    #[test]
    fn final_visible_readback_after_confirmed_action_infers_confirmed_effect() {
        assert!(final_response_claims_verified_result(
            "The visible month and year in Calendar is August 2026."
        ));
        assert!(final_response_claims_verified_result(
            "Calculator shows 123 + 456 with a result of 579."
        ));
        assert!(!final_response_claims_verified_result(
            "Opening Calculator via Spotlight and typing 123"
        ));
    }

    #[test]
    fn final_error_readback_after_refused_action_infers_failed_effect() {
        let prior_attempts = vec![PlanAttemptContext {
            attempt_index: 1,
            response: "Reading the file via shell.".to_string(),
            action: Some(json!({
                "kind": "shell_exec",
                "command": "cat /tmp/missing",
                "timeout_ms": 5000
            })),
            effect: Some("refused".to_string()),
            evidence: Some(json!({
                "effect": "refused",
                "reason": "dispatch_error",
                "error": "cat: /tmp/missing: No such file or directory"
            })),
        }];
        let completed = CompletedAssistantTurn {
            response:
                "The file does not exist. Error: cat: /tmp/missing: No such file or directory"
                    .to_string(),
            action: None,
            evidence: None,
        };

        assert!(final_response_reports_prior_failure(&completed.response));
        assert_eq!(inferred_final_effect(&completed, &prior_attempts), "failed");
    }

    #[test]
    fn verified_action_null_reply_can_finish_text_request_after_prior_evidence() {
        let prior_attempts = vec![PlanAttemptContext {
            attempt_index: 1,
            response: "Writing and reading clipboard contents.".to_string(),
            action: Some(json!({
                "kind": "shell_exec",
                "command": "printf 'clipboard loop proof 612' | pbcopy && pbpaste",
                "timeout_ms": 5000
            })),
            effect: Some("confirmed".to_string()),
            evidence: Some(json!({
                "effect": "confirmed",
                "stdout": "clipboard loop proof 612"
            })),
        }];

        assert!(action_null_finishes_after_prior_attempts(
            "The verified clipboard contents are: clipboard loop proof 612",
            &None,
            &prior_attempts
        ));
        assert!(!action_null_finishes_after_prior_attempts(
            "Writing proof text to clipboard.",
            &None,
            &prior_attempts
        ));
        assert!(!action_null_finishes_after_prior_attempts(
            "The verified clipboard contents are: clipboard loop proof 612",
            &None,
            &[]
        ));
    }

    #[test]
    fn repeated_confirmed_long_range_action_is_repair_evidence_not_dispatch() {
        let action = json!({
            "kind": "sequence",
            "actions": [
                {"kind": "open_app", "app_name": "Calculator"},
                {"kind": "key_type", "text": "123+456="}
            ],
            "inter_action_delay_ms": 300
        });
        let attempts = vec![PlanAttemptContext {
            attempt_index: 1,
            response: "Opening Calculator and calculating 123 + 456.".to_string(),
            action: Some(json!({
                "kind": "sequence",
                "actions": [
                    {"kind": "open_app", "app_name": "Calculator"},
                    {"kind": "key_type", "text": "123+456="}
                ],
                "inter_action_delay_ms": 500
            })),
            effect: Some("confirmed".to_string()),
            evidence: Some(json!({"effect": "confirmed"})),
        }];

        assert!(action_repeats_confirmed_attempt(&attempts, &action));
    }

    #[test]
    fn agent_loop_long_range_navigation_clicks_continue_after_reobserve() {
        let turn = CompletedAssistantTurn {
            response: "Opening the search result.".to_string(),
            action: Some(json!({
                "kind": "mouse_click",
                "x": 360,
                "y": 430,
                "button": "left",
                "count": 1
            })),
            evidence: Some(json!({"effect": "confirmed"})),
        };

        assert!(should_replan_after_turn(
            "Open Safari, research Gemini docs, read the title, and report it",
            &turn,
            Some("confirmed"),
            4,
            AgentLoopBudget::Unbounded
        ));
    }

    #[test]
    fn agent_loop_marks_long_range_budget_exhaustion_as_partial_progress() {
        let turn = CompletedAssistantTurn {
            response: "Opening Safari for research.".to_string(),
            action: Some(json!({
                "kind": "sequence",
                "actions": [
                    {"kind": "open_app", "app_name": "Safari"},
                    {"kind": "key_press", "combo": "cmd+l"}
                ]
            })),
            evidence: Some(json!({"effect": "confirmed"})),
        };

        let completed = mark_long_range_budget_exhausted_if_needed(
            "Research cloud computer agents and summarize what you verify",
            turn,
            5,
            AgentLoopBudget::Finite { max_attempts: 5 },
        );

        assert_eq!(turn_effect(&completed), Some("partial".to_string()));
        assert!(completed.response.contains("5-attempt loop budget"));
        assert_eq!(
            completed.evidence.as_ref().unwrap()["reason"],
            "long_range_goal_reached_agent_loop_budget"
        );
    }

    #[test]
    fn agent_loop_unbounded_budget_never_marks_attempts_exhausted() {
        let turn = CompletedAssistantTurn {
            response: "Reading another source.".to_string(),
            action: Some(json!({
                "kind": "aegis",
                "args": ["--mode", "headful", "page", "text", "--scope", "main"]
            })),
            evidence: Some(json!({"effect": "confirmed"})),
        };

        let completed = mark_long_range_budget_exhausted_if_needed(
            "Research cloud computer agents and keep going until verified",
            turn,
            10_000,
            AgentLoopBudget::Unbounded,
        );

        assert_eq!(turn_effect(&completed), Some("confirmed".to_string()));
        assert_eq!(completed.response, "Reading another source.");
    }

    #[test]
    fn agent_loop_evidence_counts_budget_exhausted_final_attempt_once() {
        let action = Some(json!({
            "kind": "aegis",
            "args": ["--mode", "headful", "page", "text", "--scope", "main"]
        }));
        let last_evidence = Some(json!({"effect": "confirmed"}));
        let prior_attempts = vec![
            PlanAttemptContext {
                attempt_index: 1,
                response: "Searching.".to_string(),
                action: Some(
                    json!({"kind": "aegis", "args": ["--mode", "headful", "search", "cloud agents"]}),
                ),
                effect: Some("confirmed".to_string()),
                evidence: Some(json!({"effect": "confirmed"})),
            },
            PlanAttemptContext {
                attempt_index: 2,
                response: "Reading.".to_string(),
                action: action.clone(),
                effect: Some("confirmed".to_string()),
                evidence: last_evidence.clone(),
            },
        ];
        let completed = CompletedAssistantTurn {
            response:
                "I made progress but hit the 2-attempt loop budget before completing the full task."
                    .to_string(),
            action,
            evidence: Some(json!({
                "effect": "partial",
                "reason": "long_range_goal_reached_agent_loop_budget",
                "max_attempts": 2,
                "last_evidence": last_evidence,
            })),
        };

        assert_eq!(loop_attempt_count(&completed, &prior_attempts), 2);
        let completed = attach_loop_evidence(completed, &prior_attempts);

        assert_eq!(completed.evidence.as_ref().unwrap()["attempt_count"], 2);
    }

    #[test]
    fn agent_loop_marks_text_entry_budget_exhaustion_as_partial_for_long_range_work() {
        let turn = CompletedAssistantTurn {
            response: "Writing the summary.".to_string(),
            action: Some(json!({"kind": "key_paste", "text": "Summary"})),
            evidence: Some(json!({"effect": "confirmed"})),
        };

        let completed = mark_long_range_budget_exhausted_if_needed(
            "Research cloud computer agents and write the summary in Notes",
            turn,
            5,
            AgentLoopBudget::Finite { max_attempts: 5 },
        );

        assert_eq!(turn_effect(&completed), Some("partial".to_string()));
        assert!(completed.response.contains("5-attempt loop budget"));
    }

    #[test]
    fn new_note_text_entry_plans_are_repaired_to_create_note_once() {
        let mut action = Some(cua_core::InputAction::KeyPaste {
            text: "hello".to_string(),
        });

        repair_new_note_text_entry_plan(
            "Open Notes and create a new note that says hello",
            &mut action,
        );

        let Some(cua_core::InputAction::Sequence { actions, .. }) = action else {
            panic!("expected repaired sequence");
        };
        assert!(matches!(
            actions.as_slice(),
            [
                cua_core::InputAction::OpenApp { app_name },
                cua_core::InputAction::KeyPress { combo },
                cua_core::InputAction::KeyPaste { text },
            ] if app_name == "Notes" && combo == "cmd+n" && text == "hello"
        ));
    }

    #[test]
    fn ordinary_text_entry_plans_are_not_forced_into_new_notes() {
        let mut action = Some(cua_core::InputAction::KeyPaste {
            text: "hello".to_string(),
        });

        repair_new_note_text_entry_plan("Paste hello", &mut action);

        assert!(matches!(
            action,
            Some(cua_core::InputAction::KeyPaste { ref text }) if text == "hello"
        ));
    }

    #[test]
    fn confirmed_edge_actions_finish_after_reobserve_without_replanning() {
        assert!(should_finish_after_reobserve(true, false));
        assert!(!should_finish_after_reobserve(true, true));
        assert!(!should_finish_after_reobserve(false, false));
    }

    #[test]
    fn malformed_verification_after_confirmed_edge_action_keeps_confirmed_turn() {
        let attempts = vec![PlanAttemptContext {
            attempt_index: 1,
            response: "Creating the note.".to_string(),
            action: Some(json!({
                "kind": "sequence",
                "actions": [
                    {"kind": "open_app", "app_name": "Notes"},
                    {"kind": "key_paste", "text": "hello"}
                ]
            })),
            effect: Some("confirmed".to_string()),
            evidence: Some(json!({"effect": "confirmed"})),
        }];

        let completed = completed_from_confirmed_prior_attempt(&attempts).unwrap();

        assert_eq!(completed.response, "Creating the note.");
        assert_eq!(turn_effect(&completed), Some("confirmed".to_string()));
    }

    #[test]
    fn loop_evidence_does_not_duplicate_final_attempt_already_recorded_for_reobserve() {
        let action =
            Some(json!({"kind": "sequence", "actions": [{"kind": "key_paste", "text": "hello"}]}));
        let evidence = Some(json!({"effect": "confirmed"}));
        let completed = CompletedAssistantTurn {
            response: "Done.".to_string(),
            action: action.clone(),
            evidence: evidence.clone(),
        };
        let attempts = vec![PlanAttemptContext {
            attempt_index: 1,
            response: "Done.".to_string(),
            action,
            effect: Some("confirmed".to_string()),
            evidence,
        }];

        assert_eq!(loop_attempt_count(&completed, &attempts), 1);
        let completed = attach_loop_evidence(completed, &attempts);

        assert_eq!(completed.evidence.as_ref().unwrap()["attempt_count"], 1);
    }

    #[test]
    fn agent_loop_evidence_preserves_failed_attempts_and_final_effect() {
        let completed = CompletedAssistantTurn {
            response: "Done.".to_string(),
            action: Some(json!({"kind": "open_app", "app_name": "Safari"})),
            evidence: Some(json!({"effect": "confirmed", "message": "Safari opened"})),
        };
        let prior_attempts = vec![PlanAttemptContext {
            attempt_index: 1,
            response: "Opening Safari.".to_string(),
            action: Some(json!({"kind": "mouse_click", "x": 1, "y": 2})),
            effect: Some("suspected_noop".to_string()),
            evidence: Some(json!({"effect": "suspected_noop", "message": "No change"})),
        }];

        let completed = attach_loop_evidence(completed, &prior_attempts);
        let evidence = completed.evidence.unwrap();

        assert_eq!(evidence["effect"], "confirmed");
        assert_eq!(evidence["attempt_count"], 2);
        assert_eq!(evidence["attempts"][0]["effect"], "suspected_noop");
        assert_eq!(evidence["attempts"][1]["effect"], "confirmed");
        assert_eq!(
            turn_effect(&CompletedAssistantTurn {
                response: completed.response,
                action: completed.action,
                evidence: Some(evidence),
            }),
            Some("confirmed".to_string())
        );
    }

    #[test]
    fn agent_loop_evidence_survives_final_clarification() {
        let completed = CompletedAssistantTurn {
            response: "I need a visible target.".to_string(),
            action: None,
            evidence: None,
        };
        let prior_attempts = vec![PlanAttemptContext {
            attempt_index: 1,
            response: "Clicking.".to_string(),
            action: Some(json!({"kind": "mouse_click", "x": 1, "y": 2})),
            effect: Some("unverifiable".to_string()),
            evidence: Some(json!({"effect": "unverifiable"})),
        }];

        let completed = attach_loop_evidence(completed, &prior_attempts);
        let evidence = completed.evidence.unwrap();

        assert_eq!(completed.action, None);
        assert_eq!(evidence["effect"], "stopped");
        assert_eq!(evidence["attempt_count"], 2);
        assert_eq!(evidence["attempts"][0]["effect"], "unverifiable");
        assert_eq!(evidence["attempts"][1]["effect"], "stopped");
    }

    #[test]
    fn agent_loop_marks_verified_answer_after_actions_as_confirmed() {
        let completed = CompletedAssistantTurn {
            response: "The verified page title is Gemini models.".to_string(),
            action: None,
            evidence: None,
        };
        let prior_attempts = vec![PlanAttemptContext {
            attempt_index: 1,
            response: "Opening the result.".to_string(),
            action: Some(json!({
                "kind": "mouse_click",
                "x": 320,
                "y": 428,
                "button": "left",
                "count": 1
            })),
            effect: Some("confirmed".to_string()),
            evidence: Some(json!({"effect": "confirmed"})),
        }];

        let completed = attach_loop_evidence(completed, &prior_attempts);

        assert_eq!(turn_effect(&completed), Some("confirmed".to_string()));
    }
}
