use crate::audio::record_default_input;
use crate::client::{CuaClient, CuaSession};
use crate::daemon::{spawn_profile_daemon, wait_until_ready};
use crate::planner::{parse_fast_command, PlannedTurn, Planner};
use crate::stt::SttClient;
use crate::ui_state::VoiceUiEvent;
use anyhow::Context;
use cua_core::{DesktopState, FramePayload};
use std::sync::mpsc::Sender;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

type LocalTask = tokio::task::JoinHandle<anyhow::Result<CuaClient>>;
type ContextTask = tokio::task::JoinHandle<PrefetchedContext>;

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
    let context_task = spawn_context_prefetch(local.clone());
    let transcript = stt_task.await.context("join speech to text")??;
    tx.send(VoiceUiEvent::Transcript(transcript.clone())).ok();
    plan_and_dispatch(
        config,
        transcript,
        Some(api_key),
        local,
        Some(context_task),
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
    tx.send(VoiceUiEvent::Transcript(transcript.clone())).ok();
    plan_and_dispatch(config, transcript, None, local, None, tx.clone()).await?;
    send_metric(&tx, "turn_total_ms", turn_started.elapsed());
    Ok(())
}

async fn plan_and_dispatch(
    config: VoiceConfig,
    transcript: String,
    api_key: Option<String>,
    local: CuaClient,
    context_task: Option<ContextTask>,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    let plan_started = Instant::now();
    let plan = if let Some(plan) = parse_fast_command(&transcript) {
        abort_context_prefetch(context_task);
        tx.send(VoiceUiEvent::Planning {
            tool: "Command parser".to_string(),
        })
        .ok();
        plan
    } else {
        tx.send(VoiceUiEvent::Planning {
            tool: "OpenRouter Vision".to_string(),
        })
        .ok();
        let wait_started = Instant::now();
        let context = match context_task {
            Some(task) => task.await.unwrap_or_else(|_| PrefetchedContext::empty()),
            None => prefetch_context_for_planning(local.clone()).await,
        };
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
        return dispatch_plan(local, context.session, plan, tx).await;
    };
    send_metric(&tx, "plan_ms", plan_started.elapsed());
    dispatch_plan(local, None, plan, tx).await
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
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    if let Some(action) = &plan.action {
        tx.send(VoiceUiEvent::Dispatching(format!("{action:?}")))
            .ok();
        let dispatch_started = Instant::now();
        let result = match session.as_mut() {
            Some(session) => session.dispatch(action).await?,
            None => local.dispatch(action).await?,
        };
        send_metric(&tx, "dispatch_ms", dispatch_started.elapsed());
        tx.send(VoiceUiEvent::Reply(format!(
            "{} {}",
            plan.response,
            result["effect"].as_str().unwrap_or("sent")
        )))
        .ok();
    } else {
        tx.send(VoiceUiEvent::Reply(plan.response)).ok();
    }
    Ok(())
}

impl PrefetchedContext {
    fn empty() -> Self {
        Self {
            session: None,
            frame: None,
            desktop: None,
            elapsed: Duration::ZERO,
        }
    }
}

fn spawn_context_prefetch(local: CuaClient) -> ContextTask {
    tokio::spawn(async move { prefetch_context_for_planning(local).await })
}

fn abort_context_prefetch(context_task: Option<ContextTask>) {
    if let Some(task) = context_task {
        task.abort();
    }
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
}
