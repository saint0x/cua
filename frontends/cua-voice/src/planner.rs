use anyhow::{bail, Context};
use cua_core::{FramePayload, InputAction, MouseButton};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, REFERER};
use serde::{Deserialize, Serialize};

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
                "You control a macOS desktop through a local HTTP API. Transcript: {transcript}\nReturn strict JSON only: {{\"response\":\"short user-facing status\",\"action\":null}} or {{\"response\":\"short status\",\"action\":{{...InputAction JSON...}}}}. Supported action kinds: mouse_move, mouse_click, mouse_drag, key_press, key_type, key_paste, pause, resume, kill_switch. Use integer coordinates."
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
    let words = lower.split_whitespace().collect::<Vec<_>>();
    if words.is_empty() {
        return None;
    }
    if words[0] == "pause" {
        return Some(turn("Paused.", Some(InputAction::Pause)));
    }
    if words[0] == "resume" {
        return Some(turn("Resumed.", Some(InputAction::Resume)));
    }
    if words[0] == "click" && words.len() >= 3 {
        if let (Ok(x), Ok(y)) = (words[1].parse::<i32>(), words[2].parse::<i32>()) {
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
    }
    if words[0] == "move" && words.len() >= 3 {
        if let (Ok(x), Ok(y)) = (words[1].parse::<i32>(), words[2].parse::<i32>()) {
            return Some(turn(
                format!("Moving to {x}, {y}."),
                Some(InputAction::MouseMove {
                    x,
                    y,
                    duration_ms: 80,
                }),
            ));
        }
    }
    if let Some(text) = lower.strip_prefix("type ") {
        return Some(turn(
            "Typing.".to_string(),
            Some(InputAction::KeyType {
                text: text.to_string(),
            }),
        ));
    }
    None
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
    fn parses_fenced_model_action() {
        let raw = "```json\n{\"response\":\"ok\",\"action\":{\"kind\":\"key_type\",\"text\":\"hello\"}}\n```";
        let plan = parse_model_plan(raw).unwrap();
        assert!(matches!(plan.action, Some(InputAction::KeyType { ref text }) if text == "hello"));
    }
}
