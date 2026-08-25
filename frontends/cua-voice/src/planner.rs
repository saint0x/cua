use anyhow::{bail, Context};
use cua_core::{FramePayload, InputAction, MouseButton};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, REFERER};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DEFAULT_PLANNER_TIMEOUT_MS: u64 = 12_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedTurn {
    pub response: String,
    pub action: Option<InputAction>,
}

#[derive(Debug, Clone)]
pub struct Planner {
    client: reqwest::Client,
    model: String,
}

impl Planner {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            model: model.into(),
        }
    }

    pub async fn plan(
        &self,
        api_key: &str,
        transcript: &str,
        frame: Option<&FramePayload>,
    ) -> anyhow::Result<PlannedTurn> {
        if let Some(turn) = parse_fast_command(transcript) {
            return Ok(turn);
        }
        let mut content = vec![serde_json::json!({
            "type": "text",
            "text": format!(
                "You control a macOS desktop through a local Unix socket. Transcript: {transcript}\nReturn strict JSON only: {{\"response\":\"short user-facing status\",\"action\":null}} or {{\"response\":\"short status\",\"action\":{{...InputAction JSON...}}}}. Supported action kinds: mouse_move, mouse_click, mouse_drag, key_press, key_type, key_paste, pause, resume, kill_switch. Use integer coordinates."
            )
        })];
        if let Some(bytes) = frame.and_then(|frame| frame.bytes_base64.as_ref()) {
            content.push(serde_json::json!({
                "type": "image_url",
                "image_url": {"url": format!("data:image/png;base64,{bytes}")},
            }));
        }
        let body = serde_json::json!({
            "model": self.model,
            "messages": [{"role": "user", "content": content}],
            "max_tokens": 160,
            "temperature": 0,
        });
        let response = self
            .client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .header(AUTHORIZATION, format!("Bearer {api_key}"))
            .header(CONTENT_TYPE, "application/json")
            .header(REFERER, "http://localhost/cua")
            .json(&body)
            .timeout(openrouter_planner_timeout())
            .send()
            .await
            .context("send planning request")?;
        let status = response.status();
        let value: serde_json::Value = response.json().await.context("decode planning response")?;
        if !status.is_success() {
            bail!("planning failed with {status}: {value}");
        }
        let raw = value["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default();
        parse_model_plan(raw)
    }
}

fn openrouter_planner_timeout() -> Duration {
    timeout_from_env("CUA_VOICE_PLANNER_TIMEOUT_MS", DEFAULT_PLANNER_TIMEOUT_MS)
}

fn timeout_from_env(name: &str, default_ms: u64) -> Duration {
    let ms = std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default_ms);
    Duration::from_millis(ms)
}

pub fn parse_model_plan(raw: &str) -> anyhow::Result<PlannedTurn> {
    let trimmed = raw.trim();
    let json = trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .or_else(|| {
            trimmed
                .strip_prefix("```")
                .and_then(|value| value.strip_suffix("```"))
        })
        .unwrap_or(trimmed)
        .trim();
    let value: serde_json::Value = serde_json::from_str(json).context("parse plan JSON")?;
    let response = value["response"].as_str().unwrap_or("Ready.").to_string();
    let action = if value.get("action").map(|v| v.is_null()).unwrap_or(true) {
        None
    } else {
        Some(serde_json::from_value(value["action"].clone()).context("parse action")?)
    };
    Ok(PlannedTurn { response, action })
}

pub fn parse_fast_command(transcript: &str) -> Option<PlannedTurn> {
    let lower = transcript.trim().to_ascii_lowercase();
    let words = lower
        .split_whitespace()
        .map(normalize_command_token)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if words.is_empty() {
        return None;
    }
    if words[0].as_str() == "pause" {
        return Some(turn("Paused.", Some(InputAction::Pause)));
    }
    if words[0].as_str() == "resume" {
        return Some(turn("Resumed.", Some(InputAction::Resume)));
    }
    if words[0].as_str() == "click" && words.len() >= 2 {
        let (x, y) = parse_coordinate_pair(&words[1..])?;
        return Some(turn(
            format!("Clicking {x}, {y}."),
            Some(InputAction::MouseClick {
                x,
                y,
                button: MouseButton::Left,
                count: 1,
            }),
        ));
    }
    if words[0].as_str() == "move" && words.len() >= 2 {
        let (x, y) = parse_coordinate_pair(&words[1..])?;
        return Some(turn(
            format!("Moving to {x}, {y}."),
            Some(InputAction::MouseMove {
                x,
                y,
                duration_ms: 80,
            }),
        ));
    }
    if matches!(words[0].as_str(), "type" | "typed") && words.len() >= 2 {
        let text = words[1..].join(" ");
        return Some(turn(
            "Typing.".to_string(),
            Some(InputAction::KeyType { text }),
        ));
    }
    None
}

fn normalize_command_token(token: &str) -> String {
    token
        .trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != '-')
        .to_string()
}

fn parse_coordinate_pair(tokens: &[String]) -> Option<(i32, i32)> {
    let numbers = tokens
        .iter()
        .flat_map(|token| {
            token
                .split('-')
                .filter(|part| !part.is_empty())
                .filter_map(|part| part.parse::<i32>().ok())
        })
        .collect::<Vec<_>>();
    match numbers.as_slice() {
        [x, y, ..] => Some((*x, *y)),
        _ => None,
    }
}

fn turn(response: impl Into<String>, action: Option<InputAction>) -> PlannedTurn {
    PlannedTurn {
        response: response.into(),
        action,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fast_click() {
        let plan = parse_fast_command("click 640 360").unwrap();
        assert!(matches!(
            plan.action,
            Some(InputAction::MouseClick { x: 640, y: 360, .. })
        ));
    }

    #[test]
    fn parses_fast_commands_from_stt_punctuation() {
        let pause = parse_fast_command("Pause.").unwrap();
        assert!(matches!(pause.action, Some(InputAction::Pause)));

        let click = parse_fast_command("Click 640, 360.").unwrap();
        assert!(matches!(
            click.action,
            Some(InputAction::MouseClick { x: 640, y: 360, .. })
        ));
    }

    #[test]
    fn parses_fast_commands_from_stt_separators_and_verbs() {
        let plan = parse_fast_command("Move 10-20.").unwrap();
        assert!(matches!(
            plan.action,
            Some(InputAction::MouseMove { x: 10, y: 20, .. })
        ));

        let plan = parse_fast_command("Typed hello.").unwrap();
        assert!(matches!(plan.action, Some(InputAction::KeyType { ref text }) if text == "hello"));
    }

    #[test]
    fn leaves_ambiguous_collapsed_coordinates_for_planner() {
        assert!(parse_fast_command("CLICK 64360").is_none());
    }

    #[test]
    fn parses_fenced_model_action() {
        let raw = "```json\n{\"response\":\"ok\",\"action\":{\"kind\":\"key_type\",\"text\":\"hello\"}}\n```";
        let plan = parse_model_plan(raw).unwrap();
        assert!(matches!(plan.action, Some(InputAction::KeyType { ref text }) if text == "hello"));
    }

    #[test]
    fn timeout_env_ignores_invalid_values() {
        assert_eq!(
            timeout_from_env("__CUA_VOICE_TEST_TIMEOUT_MISSING", 456),
            Duration::from_millis(456)
        );
    }
}
