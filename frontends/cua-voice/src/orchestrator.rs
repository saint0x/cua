use crate::audio::record_default_input;
use crate::client::CuaClient;
use crate::planner::Planner;
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
    if let Err(error) = run_voice_turn_inner(config, tx.clone()).await {
        let _ = tx.send(VoiceUiEvent::Error(error.to_string()));
    }
}

async fn run_voice_turn_inner(config: VoiceConfig, tx: Sender<VoiceUiEvent>) -> anyhow::Result<()> {
    let api_key = std::env::var("OPENROUTER_API_KEY").context("OPENROUTER_API_KEY is required")?;
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
    tx.send(VoiceUiEvent::Transcribing).ok();
    let transcript = SttClient::new(&config.stt_model)
        .transcribe_wav(&api_key, &audio.wav_bytes)
        .await?;
    tx.send(VoiceUiEvent::Transcript(transcript.clone())).ok();
    tx.send(VoiceUiEvent::Planning).ok();
    let local = CuaClient::new(config.profile).await?;
    let frame = local.screenshot(true).await.ok();
    let plan = Planner::new(&config.planner_model)
        .plan(&api_key, &transcript, frame.as_ref())
        .await?;
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
