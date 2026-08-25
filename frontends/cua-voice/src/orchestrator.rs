use crate::audio::record_default_input;
use crate::client::CuaClient;
use crate::planner::{parse_fast_command, PlannedTurn, Planner};
use crate::stt::SttClient;
use crate::ui_state::VoiceUiEvent;
use anyhow::Context;
use cua_core::FramePayload;
use std::sync::mpsc::Sender;
use std::time::Duration;

type ScreenshotTask = tokio::task::JoinHandle<anyhow::Result<FramePayload>>;

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
    let local_task = spawn_local_client(config.profile.clone());
    transcribe_and_run_turn(config, wav_bytes, local_task, tx).await
}

async fn record_and_run_turn(config: VoiceConfig, tx: Sender<VoiceUiEvent>) -> anyhow::Result<()> {
    tx.send(VoiceUiEvent::Armed).ok();
    tx.send(VoiceUiEvent::Listening {
        ms: config.record_ms,
    })
    .ok();
    let local_task = spawn_local_client(config.profile.clone());
    let record_ms = config.record_ms;
    let audio =
        tokio::task::spawn_blocking(move || record_default_input(Duration::from_millis(record_ms)))
            .await
            .context("join audio recorder")??;
    transcribe_and_run_turn(config, audio.wav_bytes, local_task, tx).await
}

async fn transcribe_and_run_turn(
    config: VoiceConfig,
    wav_bytes: Vec<u8>,
    local_task: tokio::task::JoinHandle<Result<CuaClient, anyhow::Error>>,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    let api_key = std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY is required")?;
    tx.send(VoiceUiEvent::Transcribing).ok();
    let stt_model = config.stt_model.clone();
    let stt_api_key = api_key.clone();
    let stt_task = tokio::spawn(async move {
        SttClient::new(stt_model)
            .transcribe_wav(&stt_api_key, &wav_bytes)
            .await
    });
    let local = match local_task.await.context("join local client")? {
        Ok(local) => local,
        Err(error) => {
            stt_task.abort();
            return Err(error);
        }
    };
    let screenshot_task = spawn_screenshot_prefetch(local.clone());
    let transcript = stt_task.await.context("join speech to text")??;
    tx.send(VoiceUiEvent::Transcript(transcript.clone())).ok();
    plan_and_dispatch(
        config,
        transcript,
        Some(api_key),
        local,
        Some(screenshot_task),
        tx,
    )
    .await
}

async fn run_transcript_turn(
    config: VoiceConfig,
    transcript: String,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    tx.send(VoiceUiEvent::Armed).ok();
    tx.send(VoiceUiEvent::Transcript(transcript.clone())).ok();
    let local_task = spawn_local_client(config.profile.clone());
    let local = local_task.await.context("join local client")??;
    plan_and_dispatch(config, transcript, None, local, None, tx).await
}

async fn plan_and_dispatch(
    config: VoiceConfig,
    transcript: String,
    api_key: Option<String>,
    local: CuaClient,
    screenshot_task: Option<ScreenshotTask>,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    let plan = if let Some(plan) = parse_fast_command(&transcript) {
        if let Some(task) = screenshot_task {
            task.abort();
        }
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
        let frame = match screenshot_task {
            Some(task) => task.await.ok().and_then(Result::ok),
            None => local.screenshot(true).await.ok(),
        };
        let api_key = api_key
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .context("OPENROUTER_API_KEY is required for non-fast voice commands")?;
        Planner::new(&config.planner_model)
            .plan(&api_key, &transcript, frame.as_ref())
            .await?
    };
    dispatch_plan(local, plan, tx).await
}

async fn dispatch_plan(
    local: CuaClient,
    plan: PlannedTurn,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    if let Some(action) = &plan.action {
        tx.send(VoiceUiEvent::Dispatching(format!("{action:?}")))
            .ok();
        let result = local.dispatch(action).await?;
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

fn spawn_local_client(
    profile: String,
) -> tokio::task::JoinHandle<Result<CuaClient, anyhow::Error>> {
    tokio::spawn(async move {
        let local = CuaClient::new(profile).await?;
        Ok::<_, anyhow::Error>(local)
    })
}

fn spawn_screenshot_prefetch(local: CuaClient) -> ScreenshotTask {
    tokio::spawn(async move { local.screenshot(true).await })
}
