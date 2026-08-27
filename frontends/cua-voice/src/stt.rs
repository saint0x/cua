use anyhow::{bail, Context};
use base64::Engine;
use reqwest::header::{HeaderMap, AUTHORIZATION, CONTENT_TYPE, REFERER};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;
use uuid::Uuid;

pub const DEFAULT_STT_BACKEND: &str = "local";
pub const DEFAULT_STT_MODEL: &str = "tiny.en";
pub const DEFAULT_LOCAL_STT_FALLBACK_MODEL: &str = "base.en";
pub const DEFAULT_OPENROUTER_STT_MODEL: &str = "openai/gpt-4o-mini-transcribe";
const DEFAULT_STT_TIMEOUT_MS: u64 = 15_000;
const DEFAULT_STT_ATTEMPTS: usize = 3;
const DEFAULT_STT_RETRY_BACKOFF_MS: u64 = 180;
const DEFAULT_STT_LANGUAGE: &str = "en";
const LOCAL_WHISPER_INITIAL_PROMPT: &str = "Short spoken macOS computer control command.";
const LOCAL_STT_PATH_PREFIXES: [&str; 4] =
    ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

#[derive(Debug, Clone)]
pub struct SttClient {
    client: reqwest::Client,
    backend: SttBackend,
    model: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SttTranscript {
    pub text: String,
    pub backend: String,
    pub model: String,
    pub generation_id: Option<String>,
    pub usage: Option<SttUsage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SttBackend {
    Local,
    OpenRouter,
}

impl SttClient {
    pub fn new(backend: impl AsRef<str>, model: impl Into<String>) -> anyhow::Result<Self> {
        let backend = match backend.as_ref() {
            "local" => SttBackend::Local,
            "openrouter" => SttBackend::OpenRouter,
            value => bail!("unsupported speech-to-text backend: {value}"),
        };
        Ok(Self {
            client: reqwest::Client::new(),
            backend,
            model: model.into(),
        })
    }

    pub async fn transcribe_wav(
        &self,
        api_key: &str,
        wav_bytes: &[u8],
    ) -> anyhow::Result<SttTranscript> {
        match self.backend {
            SttBackend::Local => self.transcribe_wav_local(wav_bytes).await,
            SttBackend::OpenRouter => self.transcribe_wav_openrouter(api_key, wav_bytes).await,
        }
    }

    pub fn backend(&self) -> &'static str {
        self.backend.as_str()
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    async fn transcribe_wav_local(&self, wav_bytes: &[u8]) -> anyhow::Result<SttTranscript> {
        let model = self.model.clone();
        let wav_bytes = wav_bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            transcribe_wav_with_local_whisper_retry(&model, &wav_bytes)
        })
        .await
        .context("join local speech-to-text")?
    }

    async fn transcribe_wav_openrouter(
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
        parse_transcription_value(
            SttBackend::OpenRouter.as_str(),
            &self.model,
            generation_id,
            value,
        )
    }
}

impl SttBackend {
    fn as_str(self) -> &'static str {
        match self {
            SttBackend::Local => "local",
            SttBackend::OpenRouter => "openrouter",
        }
    }
}

fn transcribe_wav_with_local_whisper(
    model: &str,
    wav_bytes: &[u8],
) -> anyhow::Result<SttTranscript> {
    let whisper = std::env::var("CUA_VOICE_LOCAL_WHISPER")
        .ok()
        .filter(|path| !path.trim().is_empty())
        .unwrap_or_else(|| "/opt/homebrew/bin/whisper".to_string());
    let temp_dir = std::env::temp_dir().join(format!("cua-stt-{}", Uuid::new_v4()));
    fs::create_dir_all(&temp_dir).context("create local speech-to-text temp directory")?;
    let wav_path = temp_dir.join("input.wav");
    fs::write(&wav_path, wav_bytes).context("write local speech-to-text wav")?;
    let output = local_whisper_command(&whisper)
        .arg(&wav_path)
        .arg("--model")
        .arg(model)
        .arg("--language")
        .arg(DEFAULT_STT_LANGUAGE)
        .arg("--output_format")
        .arg("json")
        .arg("--output_dir")
        .arg(&temp_dir)
        .arg("--verbose")
        .arg("False")
        .arg("--fp16")
        .arg("False")
        .arg("--condition_on_previous_text")
        .arg("False")
        .arg("--no_speech_threshold")
        .arg("1.0")
        .arg("--logprob_threshold")
        .arg("-2.0")
        .arg("--compression_ratio_threshold")
        .arg("3.5")
        .arg("--initial_prompt")
        .arg(LOCAL_WHISPER_INITIAL_PROMPT)
        .output()
        .with_context(|| format!("run local speech-to-text executable {whisper}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let _ = fs::remove_dir_all(&temp_dir);
        bail!(
            "local speech-to-text failed with status {}: {}{}",
            output.status,
            stdout.trim(),
            stderr.trim()
        );
    }
    let value = read_local_whisper_json(&temp_dir, &wav_path, &output)?;
    let _ = fs::remove_dir_all(&temp_dir);
    parse_transcription_value(SttBackend::Local.as_str(), model, None, value)
}

fn local_whisper_command(whisper: &str) -> Command {
    let mut command = Command::new(whisper);
    command.env("PATH", local_stt_path());
    command
}

fn local_stt_path() -> String {
    let existing = std::env::var("PATH").unwrap_or_default();
    let mut parts = LOCAL_STT_PATH_PREFIXES
        .iter()
        .map(|part| (*part).to_string())
        .collect::<Vec<_>>();
    parts.extend(
        existing
            .split(':')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(ToString::to_string),
    );
    let mut deduped = Vec::<String>::new();
    for part in parts {
        if !deduped.iter().any(|existing| existing == &part) {
            deduped.push(part);
        }
    }
    deduped.join(":")
}

fn transcribe_wav_with_local_whisper_retry(
    model: &str,
    wav_bytes: &[u8],
) -> anyhow::Result<SttTranscript> {
    let first = transcribe_wav_with_local_whisper(model, wav_bytes);
    match first {
        Ok(transcript) if !local_transcript_needs_retry(&transcript.text) => Ok(transcript),
        Ok(transcript) => {
            let Some(fallback_model) = local_whisper_fallback_model(model) else {
                return Ok(transcript);
            };
            transcribe_wav_with_local_whisper(&fallback_model, wav_bytes)
                .with_context(|| {
                    format!(
                        "local speech-to-text fallback after low-signal transcript {:?}",
                        transcript.text
                    )
                })
                .or(Ok(transcript))
        }
        Err(error) => {
            let Some(fallback_model) = local_whisper_fallback_model(model) else {
                return Err(error);
            };
            transcribe_wav_with_local_whisper(&fallback_model, wav_bytes).with_context(|| {
                format!("local speech-to-text fallback after primary failure: {error:#}")
            })
        }
    }
}

fn local_whisper_fallback_model(primary_model: &str) -> Option<String> {
    let disabled = std::env::var("CUA_VOICE_LOCAL_WHISPER_DISABLE_FALLBACK")
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);
    if disabled {
        return None;
    }
    let fallback = std::env::var("CUA_VOICE_LOCAL_WHISPER_FALLBACK_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_LOCAL_STT_FALLBACK_MODEL.to_string());
    if fallback == primary_model {
        None
    } else {
        Some(fallback)
    }
}

fn local_transcript_needs_retry(text: &str) -> bool {
    let normalized = text
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "" | "you"
            | "thank you"
            | "thanks"
            | "thanks for watching"
            | "thank you for watching"
            | "subscribe"
    )
}

fn read_local_whisper_json(
    temp_dir: &Path,
    wav_path: &Path,
    output: &Output,
) -> anyhow::Result<serde_json::Value> {
    let stem = wav_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .context("local speech-to-text wav path has no stem")?;
    let json_path: PathBuf = temp_dir.join(format!("{stem}.json"));
    let json_path = if json_path.exists() {
        json_path
    } else {
        find_single_json_output(temp_dir).with_context(|| {
            format!(
                "local speech-to-text produced no JSON output at {}; files=[{}] stdout={} stderr={}",
                json_path.display(),
                temp_dir_listing(temp_dir),
                concise_process_text(&output.stdout),
                concise_process_text(&output.stderr)
            )
        })?
    };
    let bytes = fs::read(&json_path)
        .with_context(|| format!("read local speech-to-text json {}", json_path.display()))?;
    serde_json::from_slice(&bytes).context("decode local speech-to-text json")
}

fn find_single_json_output(temp_dir: &Path) -> anyhow::Result<PathBuf> {
    let mut json_paths = fs::read_dir(temp_dir)
        .with_context(|| format!("list local speech-to-text temp dir {}", temp_dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    json_paths.sort();
    match json_paths.as_slice() {
        [path] => Ok(path.clone()),
        [] => bail!("no json outputs found"),
        _ => bail!(
            "multiple json outputs found: {}",
            json_paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn temp_dir_listing(temp_dir: &Path) -> String {
    fs::read_dir(temp_dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|error| format!("unavailable: {error}"))
}

fn concise_process_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    const MAX_LEN: usize = 800;
    if text.chars().count() <= MAX_LEN {
        return text;
    }
    let mut truncated = text
        .chars()
        .take(MAX_LEN.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
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
    backend: &str,
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
        bail!("speech-to-text returned empty text: backend={backend} model={model}");
    }
    let usage = value
        .get("usage")
        .cloned()
        .and_then(|usage| serde_json::from_value::<SttUsage>(usage).ok());
    Ok(SttTranscript {
        text,
        backend: backend.to_string(),
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
            model: DEFAULT_OPENROUTER_STT_MODEL,
            input_audio: InputAudio {
                data: "UklGRg==".to_string(),
                format: "wav",
            },
            language: DEFAULT_STT_LANGUAGE,
            temperature: 0.0,
        };
        let value = serde_json::to_value(body).unwrap();

        assert_eq!(value["model"], DEFAULT_OPENROUTER_STT_MODEL);
        assert_eq!(value["language"], "en");
        assert_eq!(value["input_audio"]["format"], "wav");
        assert_eq!(value["temperature"], 0.0);
    }

    #[test]
    fn parse_transcription_preserves_provider_evidence() {
        let transcript = parse_transcription_value(
            SttBackend::OpenRouter.as_str(),
            DEFAULT_OPENROUTER_STT_MODEL,
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
        assert_eq!(transcript.backend, "openrouter");
        assert_eq!(transcript.model, DEFAULT_OPENROUTER_STT_MODEL);
        assert_eq!(transcript.generation_id.as_deref(), Some("gen_123"));
        assert_eq!(transcript.usage.unwrap().seconds, Some(1.2));
    }

    #[test]
    fn local_transcription_parser_uses_trimmed_text() {
        let transcript = parse_transcription_value(
            SttBackend::Local.as_str(),
            DEFAULT_STT_MODEL,
            None,
            serde_json::json!({
                "text": " open safari "
            }),
        )
        .unwrap();

        assert_eq!(transcript.backend, "local");
        assert_eq!(transcript.model, DEFAULT_STT_MODEL);
        assert_eq!(transcript.text, "open safari");
    }

    #[test]
    fn local_stt_path_includes_homebrew_for_packaged_app_launches() {
        let path = local_stt_path();

        assert!(path.split(':').any(|part| part == "/opt/homebrew/bin"));
        assert!(path.split(':').any(|part| part == "/usr/local/bin"));
    }

    #[test]
    fn local_retry_only_targets_silence_hallucinations() {
        assert!(local_transcript_needs_retry("You."));
        assert!(local_transcript_needs_retry("thank you"));
        assert!(!local_transcript_needs_retry("settings"));
        assert!(!local_transcript_needs_retry("open safari"));
    }

    #[test]
    fn local_fallback_model_is_distinct_and_disableable() {
        let disable = "__CUA_VOICE_DISABLE_SHADOW";
        std::env::remove_var("CUA_VOICE_LOCAL_WHISPER_DISABLE_FALLBACK");
        std::env::remove_var("CUA_VOICE_LOCAL_WHISPER_FALLBACK_MODEL");
        assert_eq!(
            local_whisper_fallback_model("tiny.en").as_deref(),
            Some(DEFAULT_LOCAL_STT_FALLBACK_MODEL)
        );
        assert_eq!(
            local_whisper_fallback_model(DEFAULT_LOCAL_STT_FALLBACK_MODEL),
            None
        );

        std::env::set_var("CUA_VOICE_LOCAL_WHISPER_DISABLE_FALLBACK", "1");
        assert_eq!(local_whisper_fallback_model("tiny.en"), None);
        std::env::remove_var("CUA_VOICE_LOCAL_WHISPER_DISABLE_FALLBACK");
        std::env::set_var("CUA_VOICE_LOCAL_WHISPER_FALLBACK_MODEL", disable);
        assert_eq!(local_whisper_fallback_model(disable), None);
        std::env::remove_var("CUA_VOICE_LOCAL_WHISPER_FALLBACK_MODEL");
    }

    #[test]
    fn finds_fallback_whisper_json_output() {
        let temp_dir = std::env::temp_dir().join(format!("cua-stt-json-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).unwrap();
        let json_path = temp_dir.join("renamed.json");
        fs::write(&json_path, br#"{"text":"open safari"}"#).unwrap();

        assert_eq!(find_single_json_output(&temp_dir).unwrap(), json_path);

        let _ = fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn missing_whisper_json_error_includes_diagnostics() {
        let temp_dir =
            std::env::temp_dir().join(format!("cua-stt-missing-json-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).unwrap();
        fs::write(temp_dir.join("input.wav"), b"wav").unwrap();
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg("printf stdout-text; printf stderr-text >&2")
            .output()
            .unwrap();
        let error =
            read_local_whisper_json(&temp_dir, &temp_dir.join("input.wav"), &output).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("produced no JSON output"));
        assert!(message.contains("input.wav"));
        assert!(message.contains("stdout-text"));
        assert!(message.contains("stderr-text"));

        let _ = fs::remove_dir_all(temp_dir);
    }
}
