use crate::audio::record_default_input;
use crate::client::{CuaClient, CuaSession};
use crate::daemon::{spawn_profile_daemon, wait_until_ready};
use crate::planner::{parse_fast_command, PlannedTurn, Planner};
use crate::stt::SttClient;
use crate::ui_state::VoiceUiEvent;
use anyhow::Context;
use cua_core::{DesktopState, FrameEnvelope, FramePayload};
use std::sync::mpsc::Sender;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

type LocalTask = tokio::task::JoinHandle<anyhow::Result<CuaClient>>;
type ContextTask = tokio::task::JoinHandle<PrefetchedContext>;
const VOICE_STEP_SOURCE: &str = "voice";
const VOICE_STEP_TTL_MS: u64 = 5_000;
const VOICE_STEP_LABEL_MAX: usize = 96;
const VOICE_STEP_TIMEOUT_MS: u64 = 500;
const VOICE_STEP_FLUSH_TIMEOUT_MS: u64 = 2_000;

struct PrefetchedContext {
    session: Option<CuaSession>,
    frame: Option<FramePayload>,
    desktop: Option<DesktopState>,
    elapsed: Duration,
}

#[derive(Debug, Clone)]
pub struct VoiceConfig {
    pub profile: String,
    pub record_ms: u64,
    pub stt_model: String,
    pub planner_model: String,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            profile: "default".to_string(),
            record_ms: 4_500,
            stt_model: "openai/whisper-1".to_string(),
            planner_model: "openai/gpt-5.4-mini".to_string(),
        }
    }
}

pub async fn run_voice_turn(config: VoiceConfig, tx: Sender<VoiceUiEvent>) {
    if let Err(error) = run_voice_turn_checked(config, tx.clone()).await {
        eprintln!("cua voice turn failed: {error:#}");
        let _ = tx.send(VoiceUiEvent::Error(error.to_string()));
    }
}

pub async fn run_text_turn(config: VoiceConfig, transcript: String, tx: Sender<VoiceUiEvent>) {
    if let Err(error) = run_text_turn_checked(config, transcript, tx.clone()).await {
        eprintln!("cua voice scripted turn failed: {error:#}");
        let _ = tx.send(VoiceUiEvent::Error(error.to_string()));
    }
}

pub async fn run_wav_turn(config: VoiceConfig, wav_bytes: Vec<u8>, tx: Sender<VoiceUiEvent>) {
    if let Err(error) = run_wav_turn_checked(config, wav_bytes, tx.clone()).await {
        eprintln!("cua voice wav turn failed: {error:#}");
        let _ = tx.send(VoiceUiEvent::Error(error.to_string()));
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
    record_and_run_turn(config, tx).await
}

pub async fn run_wav_turn_checked(
    config: VoiceConfig,
    wav_bytes: Vec<u8>,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    let local_task = spawn_local_preflight(config.profile.clone());
    transcribe_and_run_turn_after_local(config, wav_bytes, local_task, tx).await
}

async fn record_and_run_turn(config: VoiceConfig, tx: Sender<VoiceUiEvent>) -> anyhow::Result<()> {
    tx.send(VoiceUiEvent::Armed).ok();
    tx.send(VoiceUiEvent::Listening { ms: 0 }).ok();
    let record_ms = config.record_ms;
    let local_task = spawn_local_preflight(config.profile.clone());
    let progress_flag = Arc::new(AtomicBool::new(true));
    let progress_task = spawn_recording_progress(tx.clone(), progress_flag.clone());
    let record_task =
        tokio::task::spawn_blocking(move || record_default_input(Duration::from_millis(record_ms)));
    let record_result = record_task.await.context("join audio recorder");
    progress_flag.store(false, Ordering::Release);
    progress_task.abort();
    let audio = record_result??;
    transcribe_and_run_turn_after_local(config, audio.wav_bytes, local_task, tx).await
}

async fn transcribe_and_run_turn_after_local(
    config: VoiceConfig,
    wav_bytes: Vec<u8>,
    local_task: LocalTask,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    let turn_started = Instant::now();
    let api_key = std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY is required")?;
    tx.send(VoiceUiEvent::Transcribing).ok();
    let stt_model = config.stt_model.clone();
    let stt_api_key = api_key.clone();
    let stt_task = tokio::spawn(async move {
        SttClient::new(stt_model)
            .transcribe_wav(&stt_api_key, &wav_bytes)
            .await
    });
    let overlap_started = Instant::now();
    let local = local_task.await.context("join local daemon preflight")??;
    send_metric(&tx, "stt_preflight_overlap_ms", overlap_started.elapsed());
    let step_publisher = VoiceStepPublisher::start(local.clone());
    step_publisher.publish("transcribing audio");
    let context_overlap_started = Instant::now();
    let context_task = spawn_context_prefetch(local.clone());
    step_publisher.publish("prefetching screen context");
    let transcript = stt_task.await.context("join speech to text")??;
    send_metric(
        &tx,
        "context_stt_overlap_ms",
        context_overlap_started.elapsed(),
    );
    tx.send(VoiceUiEvent::Transcript(transcript.clone())).ok();
    step_publisher.publish(voice_step_label("transcript", &transcript));
    plan_and_dispatch(
        config,
        transcript,
        Some(api_key),
        local,
        Some(context_task),
        step_publisher,
        tx.clone(),
    )
    .await?;
    send_metric(&tx, "turn_total_ms", turn_started.elapsed());
    Ok(())
}

async fn run_transcript_turn(
    config: VoiceConfig,
    transcript: String,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    let turn_started = Instant::now();
    tx.send(VoiceUiEvent::Armed).ok();
    let local = preflight_local_client(&config.profile).await?;
    let step_publisher = VoiceStepPublisher::start(local.clone());
    tx.send(VoiceUiEvent::Transcript(transcript.clone())).ok();
    step_publisher.publish(voice_step_label("transcript", &transcript));
    plan_and_dispatch(
        config,
        transcript,
        None,
        local,
        None,
        step_publisher,
        tx.clone(),
    )
    .await?;
    send_metric(&tx, "turn_total_ms", turn_started.elapsed());
    Ok(())
}

async fn plan_and_dispatch(
    config: VoiceConfig,
    transcript: String,
    api_key: Option<String>,
    local: CuaClient,
    context_task: Option<ContextTask>,
    step_publisher: VoiceStepPublisher,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    let plan_started = Instant::now();
    let plan = if let Some(plan) = parse_fast_command(&transcript) {
        tx.send(VoiceUiEvent::Planning {
            tool: "Command parser".to_string(),
        })
        .ok();
        step_publisher.publish("planning fast command");
        if let Some(context_task) = context_task {
            context_task.abort();
            send_metric(&tx, "context_prefetch_aborted_ms", plan_started.elapsed());
        }
        plan
    } else {
        tx.send(VoiceUiEvent::Planning {
            tool: "OpenRouter Vision".to_string(),
        })
        .ok();
        step_publisher.publish("planning from screen context");
        let wait_started = Instant::now();
        let context = resolve_context_for_planning(local.clone(), context_task).await;
        send_metric(&tx, "context_wait_ms", wait_started.elapsed());
        send_metric(&tx, "context_prefetch_ms", context.elapsed);
        let api_key = api_key
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .context("OPENROUTER_API_KEY is required for non-fast voice commands")?;
        let plan = Planner::new(&config.planner_model)
            .plan(
                &api_key,
                &transcript,
                context.frame.as_ref(),
                context.desktop.as_ref(),
            )
            .await?;
        send_metric(&tx, "plan_ms", plan_started.elapsed());
        let source_frame = context.frame.as_ref().map(|frame| frame.envelope.clone());
        return dispatch_plan(
            local,
            context.session,
            plan,
            source_frame,
            step_publisher,
            tx,
        )
        .await;
    };
    send_metric(&tx, "plan_ms", plan_started.elapsed());
    dispatch_plan(local, None, plan, None, step_publisher, tx).await
}

async fn prefetch_context(
    local: CuaClient,
) -> (
    Option<CuaSession>,
    Option<FramePayload>,
    Option<DesktopState>,
) {
    let mut session = match local.session().await {
        Ok(session) => session,
        Err(_) => return (None, None, None),
    };
    let snapshot = match session.context(true).await {
        Ok(snapshot) => snapshot,
        Err(_) => return (None, None, None),
    };
    (Some(session), Some(snapshot.frame), Some(snapshot.desktop))
}

async fn dispatch_plan(
    local: CuaClient,
    mut session: Option<CuaSession>,
    plan: PlannedTurn,
    source_frame: Option<FrameEnvelope>,
    step_publisher: VoiceStepPublisher,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    if let Some(action) = &plan.action {
        tx.send(VoiceUiEvent::Dispatching(format!("{action:?}")))
            .ok();
        step_publisher.publish(voice_step_label("dispatch", &format!("{action:?}")));
        let dispatch_started = Instant::now();
        let result = match session.as_mut() {
            Some(session) => match source_frame.clone() {
                Some(frame) => session.dispatch_frame(frame, action).await?,
                None => session.dispatch(action).await?,
            },
            None => match source_frame {
                Some(frame) => local.dispatch_frame(frame, action).await?,
                None => local.dispatch(action).await?,
            },
        };
        send_metric(&tx, "dispatch_ms", dispatch_started.elapsed());
        tx.send(VoiceUiEvent::Reply(format!(
            "{} {}",
            plan.response,
            result["effect"].as_str().unwrap_or("sent")
        )))
        .ok();
        step_publisher.publish(voice_step_label("reply", &plan.response));
    } else {
        step_publisher.publish(voice_step_label("reply", &plan.response));
        tx.send(VoiceUiEvent::Reply(plan.response)).ok();
    }
    step_publisher.finish().await;
    Ok(())
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
            while let Some(label) = rx.recv().await {
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

async fn prefetch_context_for_planning(local: CuaClient) -> PrefetchedContext {
    let started = Instant::now();
    let (session, frame, desktop) = prefetch_context(local).await;
    PrefetchedContext {
        session,
        frame,
        desktop,
        elapsed: started.elapsed(),
    }
}

fn spawn_context_prefetch(local: CuaClient) -> ContextTask {
    tokio::spawn(async move { prefetch_context_for_planning(local).await })
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
    prefetch_context_for_planning(local).await
}

async fn preflight_local_client(profile: &str) -> anyhow::Result<CuaClient> {
    let local = CuaClient::new(profile.to_string()).await?;
    if local.preflight().await.is_ok() {
        return Ok(local);
    }
    spawn_profile_daemon(profile).context("start bundled cua daemon")?;
    wait_until_ready(Duration::from_secs(2), || {
        let local = local.clone();
        async move { local.preflight().await }
    })
    .await
    .context("voice requires a running cua daemon on the profile Unix socket")?;
    Ok(local)
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
}
