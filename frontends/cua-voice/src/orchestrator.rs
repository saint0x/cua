use crate::audio::{record_default_input_until, RecordedAudio};
use crate::client::{CuaClient, CuaSession};
use crate::daemon::{spawn_profile_daemon, wait_until_ready};
use crate::memory::{load_agent_context_with_chat, load_chat_context, ChatStore, CtxMemory};
use crate::planner::{
    parse_fast_command, PlanAttemptContext, PlannedTurn, Planner, PlannerRequest,
};
use crate::stt::{SttClient, SttTranscript, DEFAULT_STT_BACKEND, DEFAULT_STT_MODEL};
use crate::ui_state::VoiceUiEvent;
use anyhow::Context;
use cua_core::{DesktopState, FrameEnvelope, FramePayload};
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
const VOICE_TRACE_FILE: &str = "voice-turns.jsonl";
static VOICE_TRACE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const VOICE_STEP_SOURCE: &str = "voice";
const VOICE_STEP_TTL_MS: u64 = 5_000;
const VOICE_STEP_LABEL_MAX: usize = 96;
const VOICE_STEP_TIMEOUT_MS: u64 = 120;
const VOICE_STEP_FLUSH_TIMEOUT_MS: u64 = 120;
const DEFAULT_CONTEXT_PREFETCH_TIMEOUT_MS: u64 = 2_500;
const DEFAULT_AGENT_LOOP_MAX_ATTEMPTS: usize = 3;
const MIN_RECORDING_DURATION: Duration = Duration::from_millis(650);

struct PrefetchedContext {
    session: Option<CuaSession>,
    frame: Option<FramePayload>,
    desktop: Option<DesktopState>,
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
            planner_model: "anthropic/claude-sonnet-5".to_string(),
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
    run_voice_turn_checked_until(config, tx, Arc::new(AtomicBool::new(false))).await
}

pub async fn run_voice_turn_checked_until(
    config: VoiceConfig,
    tx: Sender<VoiceUiEvent>,
    stop_requested: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    record_and_run_turn(config, tx, stop_requested).await
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
    let record_ms = config.record_ms;
    trace
        .append("record_start", json!({"record_ms": record_ms}))
        .await;
    let local_task = spawn_local_preflight(config.profile.clone());
    let progress_flag = Arc::new(AtomicBool::new(true));
    let progress_task = spawn_recording_progress(tx.clone(), progress_flag.clone());
    let record_task = tokio::task::spawn_blocking(move || {
        record_default_input_until(Duration::from_millis(record_ms), stop_requested)
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
        let (mut context, agent_context) = tokio::join!(
            resolve_context_for_planning(local.clone(), context_task),
            resolve_agent_context(agent_context_task)
        );
        let agent_context = agent_context?;
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
                }),
            )
            .await;
        let api_key = api_key
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .context("OPENROUTER_API_KEY is required for non-fast voice commands")?;
        trace
            .append(
                "agent_loop_start",
                json!({"max_attempts": agent_loop_max_attempts()}),
            )
            .await;
        let planner = Planner::new(&config.planner_model);
        let combined_agent_context = format!("{}\n{}", agent_context.chat, agent_context.ctx);
        let mut attempts = Vec::new();
        let max_attempts = agent_loop_max_attempts();
        let mut completed = None;
        for attempt_index in 1..=max_attempts {
            let attempt_started = Instant::now();
            tx.send(VoiceUiEvent::Planning {
                tool: if attempt_index == 1 {
                    "OpenRouter Vision".to_string()
                } else {
                    format!("OpenRouter repair {attempt_index}/{max_attempts}")
                },
            })
            .ok();
            step_publisher.publish(voice_step_label(
                "planning",
                &format!("attempt {attempt_index}/{max_attempts}"),
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
            let plan = match planner
                .plan_request(
                    &api_key,
                    PlannerRequest {
                        transcript: &transcript,
                        agent_context: Some(&combined_agent_context),
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
                    return Err(error);
                }
            };
            trace
                .append(
                    "planning_result",
                    plan_trace_json(&config.planner_model, attempt_started.elapsed(), &plan),
                )
                .await;
            let source_frame = context.frame.as_ref().map(|frame| frame.envelope.clone());
            let turn = dispatch_plan(
                local.clone(),
                context.session,
                plan,
                source_frame,
                &step_publisher,
                tx.clone(),
                &trace,
                &config.profile,
            )
            .await?;
            let effect = turn_effect(&turn);
            let should_continue =
                should_replan_after_effect(effect.as_deref(), attempt_index, max_attempts);
            trace
                .append(
                    "agent_attempt_outcome",
                    json!({
                        "attempt_index": attempt_index,
                        "effect": effect,
                        "should_replan": should_continue,
                        "has_action": turn.action.is_some(),
                    }),
                )
                .await;
            if !should_continue {
                completed = Some(turn);
                break;
            }
            attempts.push(PlanAttemptContext {
                attempt_index,
                response: turn.response.clone(),
                action: turn.action.clone(),
                effect,
                evidence: turn.evidence.clone(),
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
                    json!({"after_attempt": attempt_index}),
                )
                .await;
            let observe_started = Instant::now();
            context = prefetch_context_for_planning(local.clone(), None).await;
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
        }
        send_metric(&tx, "plan_ms", plan_started.elapsed());
        let completed = attach_loop_evidence(
            completed.context("agent loop exhausted without a completed turn")?,
            &attempts,
        );
        trace
            .append(
                "agent_loop_stop",
                json!({
                    "attempts": attempts.len() + 1,
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

async fn prefetch_context(
    local: CuaClient,
    warm_session: Option<CuaSession>,
) -> (
    Option<CuaSession>,
    Option<FramePayload>,
    Option<DesktopState>,
) {
    let mut session = match warm_session {
        Some(session) => session,
        None => match local.session().await {
            Ok(session) => session,
            Err(_) => return (None, None, None),
        },
    };
    let snapshot =
        match tokio::time::timeout(context_prefetch_timeout(), session.context(true)).await {
            Ok(Ok(snapshot)) => snapshot,
            Ok(Err(_)) => return (None, None, None),
            Err(_) => {
                let desktop = local.observe().await.ok();
                return (None, None, desktop);
            }
        };
    (Some(session), Some(snapshot.frame), Some(snapshot.desktop))
}

fn context_prefetch_timeout() -> Duration {
    let ms = std::env::var("CUA_VOICE_CONTEXT_PREFETCH_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_CONTEXT_PREFETCH_TIMEOUT_MS)
        .clamp(250, 30_000);
    Duration::from_millis(ms)
}

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
        let result = match session.as_mut() {
            Some(session) => match source_frame.clone() {
                Some(frame) => session.dispatch_frame(frame, action).await,
                None => session.dispatch(action).await,
            },
            None => match source_frame {
                Some(frame) => local.dispatch_frame(frame, action).await,
                None => local.dispatch(action).await,
            },
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
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
                return Err(error);
            }
        };
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
    let text = if action {
        format!(
            "{} {}",
            completed.response,
            turn_effect(completed).unwrap_or_else(|| "sent".to_string())
        )
    } else {
        completed.response.clone()
    };
    tx.send(VoiceUiEvent::Reply(text)).ok();
    step_publisher.publish(voice_step_label("reply", &completed.response));
    trace
        .append(
            "reply",
            json!({"text": completed.response, "action": action}),
        )
        .await;
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
    std::env::var("HOME")
        .map(|home| format!("{home}/.cua/profiles/{profile}/ctx"))
        .unwrap_or_else(|_| format!(".cua/profiles/{profile}/ctx"))
}

fn agent_loop_max_attempts() -> usize {
    std::env::var("CUA_AGENT_LOOP_MAX_ATTEMPTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=5).contains(value))
        .unwrap_or(DEFAULT_AGENT_LOOP_MAX_ATTEMPTS)
}

fn turn_effect(turn: &CompletedAssistantTurn) -> Option<String> {
    turn.evidence
        .as_ref()
        .and_then(|evidence| evidence["effect"].as_str())
        .map(ToString::to_string)
}

fn attach_loop_evidence(
    mut completed: CompletedAssistantTurn,
    prior_attempts: &[PlanAttemptContext],
) -> CompletedAssistantTurn {
    if prior_attempts.is_empty() {
        return completed;
    }
    let final_evidence = completed.evidence.take();
    let final_effect = final_evidence
        .as_ref()
        .and_then(|evidence| evidence["effect"].as_str())
        .unwrap_or(if completed.action.is_some() {
            "unverifiable"
        } else {
            "stopped"
        })
        .to_string();
    let mut attempts = prior_attempts.to_vec();
    attempts.push(PlanAttemptContext {
        attempt_index: attempts.len() + 1,
        response: completed.response.clone(),
        action: completed.action.clone(),
        effect: Some(final_effect.clone()),
        evidence: final_evidence.clone(),
    });
    completed.evidence = Some(json!({
        "effect": final_effect,
        "final_evidence": final_evidence,
        "attempt_count": attempts.len(),
        "attempts": attempts,
    }));
    completed
}

fn should_replan_after_effect(
    effect: Option<&str>,
    attempt_index: usize,
    max_attempts: usize,
) -> bool {
    if attempt_index >= max_attempts {
        return false;
    }
    matches!(
        effect,
        Some("partial" | "unverifiable" | "suspected_noop" | "refused")
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
    let (session, frame, desktop) = prefetch_context(local, warm_session).await;
    PrefetchedContext {
        session,
        frame,
        desktop,
        elapsed: started.elapsed(),
    }
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
        if let Ok(context) = context_task.await {
            return context;
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
        async move { local.session().await.map(|_| ()) }
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
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let line = json!({
            "schema_version": "cua.voice_trace.v1",
            "turn_id": self.turn_id,
            "event": event,
            "at_wall_ms": wall_ms(),
            "data": data,
        });
        if let Ok(mut file) = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
        {
            use tokio::io::AsyncWriteExt;
            let _ = file.write_all(line.to_string().as_bytes()).await;
            let _ = file.write_all(b"\n").await;
            let _ = file.flush().await;
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
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        if tokio::fs::write(&path, bytes).await.is_ok() {
            self.append(
                "artifact",
                json!({
                    "path": path.display().to_string(),
                    "data": data,
                }),
            )
            .await;
        }
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

fn voice_trace_path(profile: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CUA_VOICE_TRACE_PATH") {
        return Some(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".cua")
            .join("profiles")
            .join(profile)
            .join(VOICE_TRACE_FILE),
    )
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
        "record_ms": config.record_ms,
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
        assert!(VOICE_STEP_FLUSH_TIMEOUT_MS <= 150);
    }

    #[test]
    fn voice_step_request_timeout_stays_latency_oriented() {
        assert!(VOICE_STEP_TIMEOUT_MS <= 150);
    }

    #[test]
    fn agent_loop_attempt_budget_is_conservative_and_configurable() {
        std::env::remove_var("CUA_AGENT_LOOP_MAX_ATTEMPTS");
        assert_eq!(agent_loop_max_attempts(), 3);

        std::env::set_var("CUA_AGENT_LOOP_MAX_ATTEMPTS", "4");
        assert_eq!(agent_loop_max_attempts(), 4);

        std::env::set_var("CUA_AGENT_LOOP_MAX_ATTEMPTS", "30");
        assert_eq!(agent_loop_max_attempts(), 3);

        std::env::set_var("CUA_AGENT_LOOP_MAX_ATTEMPTS", "0");
        assert_eq!(agent_loop_max_attempts(), 3);
        std::env::remove_var("CUA_AGENT_LOOP_MAX_ATTEMPTS");
    }

    #[test]
    fn agent_loop_replans_only_for_recoverable_effects_inside_budget() {
        for effect in ["partial", "unverifiable", "suspected_noop", "refused"] {
            assert!(should_replan_after_effect(Some(effect), 1, 3));
        }
        for effect in ["confirmed", "sent", "unknown"] {
            assert!(!should_replan_after_effect(Some(effect), 1, 3));
        }
        assert!(!should_replan_after_effect(None, 1, 3));
        assert!(!should_replan_after_effect(Some("suspected_noop"), 3, 3));
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
}
