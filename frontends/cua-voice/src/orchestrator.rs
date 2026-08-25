use crate::audio::record_default_input;
use crate::client::CuaClient;
use crate::planner::{parse_fast_command, PlannedTurn, Planner};
use crate::stt::SttClient;
use crate::ui_state::VoiceUiEvent;
use anyhow::Context;
use std::sync::mpsc::Sender;
use std::time::Duration;

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
    if let Err(error) = record_and_run_turn(config, tx.clone()).await {
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

pub async fn run_wav_turn_checked(
    config: VoiceConfig,
    wav_bytes: Vec<u8>,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    transcribe_and_run_turn(config, wav_bytes, tx).await
}

async fn record_and_run_turn(config: VoiceConfig, tx: Sender<VoiceUiEvent>) -> anyhow::Result<()> {
    tx.send(VoiceUiEvent::Armed).ok();
    tx.send(VoiceUiEvent::Listening {
        ms: config.record_ms,
    })
    .ok();
    let record_ms = config.record_ms;
    let audio =
        tokio::task::spawn_blocking(move || record_default_input(Duration::from_millis(record_ms)))
            .await
            .context("join audio recorder")??;
    transcribe_and_run_turn(config, audio.wav_bytes, tx).await
}

async fn transcribe_and_run_turn(
    config: VoiceConfig,
    wav_bytes: Vec<u8>,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    let api_key = std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY is required")?;
    tx.send(VoiceUiEvent::Transcribing).ok();
    let local_task = tokio::spawn({
        let profile = config.profile.clone();
        async move {
            let local = CuaClient::new(profile).await?;
            Ok::<_, anyhow::Error>(local)
        }
    });
    let transcript = SttClient::new(&config.stt_model)
        .transcribe_wav(&api_key, &wav_bytes)
        .await?;
    tx.send(VoiceUiEvent::Transcript(transcript.clone())).ok();
    plan_and_dispatch(config, transcript, Some(api_key), local_task, tx).await
}

async fn run_transcript_turn(
    config: VoiceConfig,
    transcript: String,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    tx.send(VoiceUiEvent::Armed).ok();
    tx.send(VoiceUiEvent::Transcript(transcript.clone())).ok();
    let local_task = tokio::spawn({
        let profile = config.profile.clone();
        async move {
            let local = CuaClient::new(profile).await?;
            Ok::<_, anyhow::Error>(local)
        }
    });
    plan_and_dispatch(config, transcript, None, local_task, tx).await
}

async fn plan_and_dispatch(
    config: VoiceConfig,
    transcript: String,
    api_key: Option<String>,
    local_task: tokio::task::JoinHandle<Result<CuaClient, anyhow::Error>>,
    tx: Sender<VoiceUiEvent>,
) -> anyhow::Result<()> {
    tx.send(VoiceUiEvent::Planning).ok();
    let (local, plan) = if let Some(plan) = parse_fast_command(&transcript) {
        let local = local_task.await.context("join local client")??;
        (local, plan)
    } else {
        let local = local_task.await.context("join local client")??;
        let frame = local.screenshot(true).await.ok();
        let api_key = api_key
            .or_else(|| std::env::var("OPENROUTER_API_KEY").ok())
            .context("OPENROUTER_API_KEY is required for non-fast voice commands")?;
        let plan = Planner::new(&config.planner_model)
            .plan(&api_key, &transcript, frame.as_ref())
            .await?;
        (local, plan)
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
