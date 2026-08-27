use anyhow::{bail, Context};
use base64::Engine;
use reqwest::header::{HeaderMap, AUTHORIZATION, CONTENT_TYPE, REFERER};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const DEFAULT_STT_MODEL: &str = "openai/gpt-4o-mini-transcribe";
const DEFAULT_STT_TIMEOUT_MS: u64 = 15_000;
const DEFAULT_STT_ATTEMPTS: usize = 3;
const DEFAULT_STT_RETRY_BACKOFF_MS: u64 = 180;
const DEFAULT_STT_LANGUAGE: &str = "en";

#[derive(Debug, Clone)]
pub struct SttClient {
    client: reqwest::Client,
    model: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SttTranscript {
    pub text: String,
    pub model: String,
    pub generation_id: Option<String>,
    pub usage: Option<SttUsage>,
}

impl SttClient {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            model: model.into(),
        }
    }

    pub async fn transcribe_wav(
        &self,
        api_key: &str,
        wav_bytes: &[u8],
    ) -> anyhow::Result<SttTranscript> {
        let body = SttRequest {
            model: &self.model,
            input_audio: InputAudio {
                data: base64::engine::general_purpose::STANDARD.encode(wav_bytes),
                format: "wav",
            },
            language: DEFAULT_STT_LANGUAGE,
            temperature: 0.0,
        };
        let response = self.send_transcription_request(api_key, &body).await?;
        let status = response.status();
        let generation_id = generation_id(response.headers());
        let value: serde_json::Value = response.json().await.context("decode transcription")?;
        if !status.is_success() {
            bail!("transcription failed with {status}: {value}");
        }
        parse_transcription_value(&self.model, generation_id, value)
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

fn generation_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-generation-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn parse_transcription_value(
    model: &str,
    generation_id: Option<String>,
    value: serde_json::Value,
) -> anyhow::Result<SttTranscript> {
    let text = value["text"]
        .as_str()
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        bail!("transcription returned empty text");
    }
    let usage = value
        .get("usage")
        .cloned()
        .and_then(|usage| serde_json::from_value::<SttUsage>(usage).ok());
    Ok(SttTranscript {
        text,
        model: model.to_string(),
        generation_id,
        usage,
    })
}

impl SttClient {
    async fn send_transcription_request(
        &self,
        api_key: &str,
        body: &SttRequest<'_>,
    ) -> anyhow::Result<reqwest::Response> {
        let attempts =
            retry_attempts_from_env("CUA_VOICE_STT_RETRY_ATTEMPTS", DEFAULT_STT_ATTEMPTS);
        let backoff = retry_backoff_from_env(
            "CUA_VOICE_STT_RETRY_BACKOFF_MS",
            DEFAULT_STT_RETRY_BACKOFF_MS,
        );
        let mut last_error = None;
        for attempt in 1..=attempts {
            match self
                .client
                .post("https://openrouter.ai/api/v1/audio/transcriptions")
                .header(AUTHORIZATION, format!("Bearer {api_key}"))
                .header(CONTENT_TYPE, "application/json")
                .header(REFERER, "http://localhost/cua")
                .json(body)
                .timeout(openrouter_stt_timeout())
                .send()
                .await
            {
                Ok(response) if !retryable_status(response.status()) || attempt == attempts => {
                    return Ok(response);
                }
                Ok(response) => {
                    last_error = Some(format!(
                        "transcription retryable status {}",
                        response.status()
                    ));
                }
                Err(error) if attempt == attempts || !error.is_request() => {
                    return Err(error).context("send transcription request");
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                }
            }
            tokio::time::sleep(backoff * attempt as u32).await;
        }
        bail!(
            "send transcription request failed: {}",
            last_error.unwrap_or_else(|| "retry attempts exhausted".to_string())
        )
    }
}

fn openrouter_stt_timeout() -> Duration {
    timeout_from_env("CUA_VOICE_STT_TIMEOUT_MS", DEFAULT_STT_TIMEOUT_MS)
}

fn timeout_from_env(name: &str, default_ms: u64) -> Duration {
    let ms = std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default_ms);
    Duration::from_millis(ms)
}

fn retry_attempts_from_env(name: &str, default_attempts: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=5).contains(value))
        .unwrap_or(default_attempts)
}

fn retry_backoff_from_env(name: &str, default_ms: u64) -> Duration {
    let ms = std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (10..=2_000).contains(value))
        .unwrap_or(default_ms);
    Duration::from_millis(ms)
}

fn retryable_status(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 429 || status.is_server_error()
}

#[derive(Debug, Serialize)]
struct SttRequest<'a> {
    model: &'a str,
    input_audio: InputAudio,
    language: &'static str,
    temperature: f32,
}

#[derive(Debug, Serialize)]
struct InputAudio {
    data: String,
    format: &'static str,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SttUsage {
    pub seconds: Option<f64>,
    pub total_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_env_ignores_invalid_values() {
        assert_eq!(
            timeout_from_env("__CUA_VOICE_TEST_TIMEOUT_MISSING", 123),
            Duration::from_millis(123)
        );
    }

    #[test]
    fn retry_env_bounds_ignore_invalid_values() {
        assert_eq!(
            retry_attempts_from_env("__CUA_VOICE_TEST_RETRY_MISSING", 3),
            3
        );

        let attempts = "__CUA_VOICE_TEST_RETRY_ATTEMPTS";
        std::env::set_var(attempts, "0");
        assert_eq!(retry_attempts_from_env(attempts, 3), 3);
        std::env::set_var(attempts, "4");
        assert_eq!(retry_attempts_from_env(attempts, 3), 4);
        std::env::remove_var(attempts);

        let backoff = "__CUA_VOICE_TEST_RETRY_BACKOFF";
        std::env::set_var(backoff, "1");
        assert_eq!(
            retry_backoff_from_env(backoff, 180),
            Duration::from_millis(180)
        );
        std::env::set_var(backoff, "250");
        assert_eq!(
            retry_backoff_from_env(backoff, 180),
            Duration::from_millis(250)
        );
        std::env::remove_var(backoff);
    }

    #[test]
    fn retryable_status_covers_rate_limits_and_server_errors() {
        assert!(retryable_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(retryable_status(reqwest::StatusCode::BAD_GATEWAY));
        assert!(!retryable_status(reqwest::StatusCode::BAD_REQUEST));
    }

    #[test]
    fn stt_request_pins_current_model_and_english_language() {
        let body = SttRequest {
            model: DEFAULT_STT_MODEL,
            input_audio: InputAudio {
                data: "UklGRg==".to_string(),
                format: "wav",
            },
            language: DEFAULT_STT_LANGUAGE,
            temperature: 0.0,
        };
        let value = serde_json::to_value(body).unwrap();

        assert_eq!(value["model"], DEFAULT_STT_MODEL);
        assert_eq!(value["language"], "en");
        assert_eq!(value["input_audio"]["format"], "wav");
        assert_eq!(value["temperature"], 0.0);
    }

    #[test]
    fn parse_transcription_preserves_provider_evidence() {
        let transcript = parse_transcription_value(
            DEFAULT_STT_MODEL,
            Some("gen_123".to_string()),
            serde_json::json!({
                "text": " click the center target ",
                "usage": {
                    "seconds": 1.2,
                    "total_tokens": 42,
                    "input_tokens": 40,
                    "output_tokens": 2,
                    "cost": 0.00001
                }
            }),
        )
        .unwrap();

        assert_eq!(transcript.text, "click the center target");
        assert_eq!(transcript.model, DEFAULT_STT_MODEL);
        assert_eq!(transcript.generation_id.as_deref(), Some("gen_123"));
        assert_eq!(transcript.usage.unwrap().seconds, Some(1.2));
    }
}
