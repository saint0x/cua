use anyhow::{bail, Context};
use base64::Engine;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, REFERER};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct SttClient {
    client: reqwest::Client,
    model: String,
}

impl SttClient {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            model: model.into(),
        }
    }

    pub async fn transcribe_wav(&self, api_key: &str, wav_bytes: &[u8]) -> anyhow::Result<String> {
        let body = SttRequest {
            model: &self.model,
            input_audio: InputAudio {
                data: base64::engine::general_purpose::STANDARD.encode(wav_bytes),
                format: "wav",
            },
            temperature: 0.0,
        };
        let response = self
            .client
            .post("https://openrouter.ai/api/v1/audio/transcriptions")
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header(CONTENT_TYPE, "application/json")
            .header(REFERER, "http://localhost/cua")
            .json(&body)
            .send()
            .await
            .context("send transcription request")?;
        let status = response.status();
        let value: serde_json::Value = response.json().await.context("decode transcription")?;
        if !status.is_success() {
            bail!("transcription failed with {status}: {value}");
        }
        let text = value["text"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        if text.is_empty() {
            bail!("transcription returned empty text");
        }
        Ok(text)
    }
}

#[derive(Debug, Serialize)]
struct SttRequest<'a> {
    model: &'a str,
    input_audio: InputAudio,
    temperature: f32,
}

#[derive(Debug, Serialize)]
struct InputAudio {
    data: String,
    format: &'static str,
}

#[derive(Debug, Deserialize)]
pub struct SttUsage {
    pub seconds: Option<f64>,
    pub total_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost: Option<f64>,
}
