use anyhow::{bail, Context};
use cua_core::{DesktopState, FramePayload, InputAction, MouseButton};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, REFERER};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DEFAULT_PLANNER_TIMEOUT_MS: u64 = 12_000;
const DEFAULT_PLANNER_ATTEMPTS: usize = 3;
const DEFAULT_PLANNER_RETRY_BACKOFF_MS: u64 = 220;
const PLANNER_SYSTEM_PROMPT: &str = r#"You are the planner for cua, a local macOS computer-use runtime.

You receive:
- a spoken transcript from the user
- a live macOS desktop summary with cursor, displays, windows, permissions, and latest frame metadata
- usually a screenshot image from the active display

Your job is to choose exactly one next tool action for cua. This is a realtime control loop, so be decisive, avoid long reasoning, avoid multi-step plans, and keep the response text short. Return only valid JSON. Do not use Markdown, prose before/after JSON, comments, or extra top-level keys.

Top-level response schema:
{"response":"short status for the user","action":null}
{"response":"short status for the user","action":ACTION}

Supported ACTION shapes:
{"kind":"mouse_move","x":640,"y":360,"duration_ms":80}
{"kind":"mouse_click","x":640,"y":360,"button":"left","count":1}
{"kind":"mouse_drag","from_x":640,"from_y":360,"to_x":820,"to_y":360,"duration_ms":220}
{"kind":"key_press","combo":"enter"}
{"kind":"key_type","text":"text to type"}
{"kind":"key_paste","text":"text to paste"}
{"kind":"clipboard_read","allow_sensitive":false}
{"kind":"clipboard_write","text":"text to put on clipboard"}
{"kind":"pause"}
{"kind":"resume"}
{"kind":"kill_switch"}

Coordinate rules:
- x/y values are screenshot pixel coordinates in the attached frame image.
- Do not return physical display coordinates.
- For visible controls, click the center of the visual target.
- Prefer a mouse_click for visible buttons, links, tabs, menus, fields, and icons.
- Prefer key_type for short text into a focused field.
- Prefer key_paste for longer text or exact multi-line text.
- Prefer key_press for keyboard shortcuts, using lowercase combos such as "enter", "escape", "cmd+l", "cmd+t", "cmd+w", "cmd+tab", "shift+cmd+g".
- Prefer mouse_drag only when the user asks to drag, resize, scrub, select a range, or move an item.
- Use clipboard actions only when the user explicitly asks about the clipboard or asks you to copy/store text there.
- Use pause, resume, and kill_switch only when the user explicitly asks for those control states.

Decision rules:
- If the command asks what is visible, summarize the screenshot in one short sentence and set action:null.
- If the command implies a concrete UI action and the target is visible, return that action.
- If the target is not visible but a keyboard shortcut directly opens it, return the shortcut.
- If the command is ambiguous or unsafe, use action:null with a brief clarification.
- Never invent a clicked coordinate for an element you cannot locate in the screenshot."#;

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
        desktop: Option<&DesktopState>,
    ) -> anyhow::Result<PlannedTurn> {
        if let Some(turn) = parse_fast_command(transcript) {
            return Ok(turn);
        }
        let desktop_context = desktop
            .map(desktop_context)
            .unwrap_or_else(|| "Desktop context: unavailable.".to_string());
        let mut content = vec![serde_json::json!({
            "type": "text",
            "text": format!(
                "Transcript: {transcript}\n{desktop_context}"
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
            "messages": [
                {"role": "system", "content": PLANNER_SYSTEM_PROMPT},
                {"role": "user", "content": content}
            ],
            "max_tokens": 180,
            "response_format": {"type": "json_object"},
        });
        let response = self.send_planning_request(api_key, &body).await?;
        let status = response.status();
        let value: serde_json::Value = response.json().await.context("decode planning response")?;
        if !status.is_success() {
            bail!("planning failed with {status}: {value}");
        }
        let raw = value["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default();
        if raw.trim().is_empty() {
            bail!("planning model returned empty content");
        }
        parse_model_plan(raw)
    }

    async fn send_planning_request(
        &self,
        api_key: &str,
        body: &serde_json::Value,
    ) -> anyhow::Result<reqwest::Response> {
        let attempts =
            retry_attempts_from_env("CUA_VOICE_PLANNER_RETRY_ATTEMPTS", DEFAULT_PLANNER_ATTEMPTS);
        let backoff = retry_backoff_from_env(
            "CUA_VOICE_PLANNER_RETRY_BACKOFF_MS",
            DEFAULT_PLANNER_RETRY_BACKOFF_MS,
        );
        let mut last_error = None;
        for attempt in 1..=attempts {
            match self
                .client
                .post("https://openrouter.ai/api/v1/chat/completions")
                .header(AUTHORIZATION, format!("Bearer {api_key}"))
                .header(CONTENT_TYPE, "application/json")
                .header(REFERER, "http://localhost/cua")
                .json(body)
                .timeout(openrouter_planner_timeout())
                .send()
                .await
            {
                Ok(response) if !retryable_status(response.status()) || attempt == attempts => {
                    return Ok(response);
                }
                Ok(response) => {
                    last_error = Some(format!("planning retryable status {}", response.status()));
                }
                Err(error) if attempt == attempts || !error.is_request() => {
                    return Err(error).context("send planning request");
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                }
            }
            tokio::time::sleep(backoff * attempt as u32).await;
        }
        bail!(
            "send planning request failed: {}",
            last_error.unwrap_or_else(|| "retry attempts exhausted".to_string())
        )
    }
}

fn desktop_context(desktop: &DesktopState) -> String {
    let displays = desktop
        .displays
        .iter()
        .take(4)
        .map(|display| {
            format!(
                "{}:{}x{}@{},{} scale {:.1}{}",
                display.name,
                display.width,
                display.height,
                display.x,
                display.y,
                display.scale_factor,
                if display.active { " active" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let windows = desktop
        .windows
        .iter()
        .take(8)
        .map(|window| {
            let app = window.app_name.as_deref().unwrap_or("unknown");
            let title = window.title.as_deref().unwrap_or("untitled");
            format!(
                "{app} \"{title}\" {}x{}@{},{}{}",
                window.width,
                window.height,
                window.x,
                window.y,
                if window.focused { " focused" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let frame = desktop
        .latest_frame
        .as_ref()
        .map(|frame| {
            format!(
                "latest frame {}x{} display {} sha {}",
                frame.width, frame.height, frame.display_id, frame.sha256
            )
        })
        .unwrap_or_else(|| "latest frame unavailable".to_string());
    format!(
        "Desktop context: cursor at {},{}; displays [{}]; windows [{}]; {frame}.",
        desktop.cursor.x, desktop.cursor.y, displays, windows
    )
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
    let value: serde_json::Value = serde_json::from_str(json)
        .or_else(|_| {
            extract_first_json_object(json)
                .map(serde_json::from_str)
                .unwrap_or_else(|| serde_json::from_str(json))
        })
        .context("parse plan JSON")?;
    let response = value["response"].as_str().unwrap_or("Ready.").to_string();
    let action = if value.get("action").map(|v| v.is_null()).unwrap_or(true) {
        None
    } else {
        Some(parse_action_value(value["action"].clone())?)
    };
    Ok(PlannedTurn { response, action })
}

fn extract_first_json_object(raw: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut start = None;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in raw.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && in_string {
            escaped = true;
            continue;
        }
        if character == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match character {
            '{' => {
                if depth == 0 {
                    start = Some(index);
                }
                depth += 1;
            }
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    return start.map(|start| &raw[start..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_action_value(mut value: serde_json::Value) -> anyhow::Result<InputAction> {
    normalize_action_value(&mut value);
    serde_json::from_value(value).context("parse action")
}

fn normalize_action_value(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let Some(kind) = object.get("kind").and_then(|kind| kind.as_str()) else {
        return;
    };
    match kind {
        "mouse_move" => {
            object
                .entry("duration_ms")
                .or_insert_with(|| serde_json::json!(80));
        }
        "mouse_click" => {
            object
                .entry("button")
                .or_insert_with(|| serde_json::json!("left"));
            object
                .entry("count")
                .or_insert_with(|| serde_json::json!(1));
        }
        "mouse_drag" => {
            object
                .entry("duration_ms")
                .or_insert_with(|| serde_json::json!(220));
        }
        _ => {}
    }
    if let Some(button) = object.get_mut("button").and_then(|button| button.as_str()) {
        let normalized = match button {
            "Left" => Some("left"),
            "Right" => Some("right"),
            "Middle" => Some("middle"),
            _ => None,
        };
        if let Some(normalized) = normalized {
            object.insert("button".to_string(), serde_json::json!(normalized));
        }
    }
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
    fn parses_model_pointer_actions_with_safe_defaults() {
        let raw = r#"{"response":"Moving.","action":{"kind":"mouse_move","x":10,"y":20}}"#;
        let plan = parse_model_plan(raw).unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::MouseMove {
                x: 10,
                y: 20,
                duration_ms: 80
            })
        ));
    }

    #[test]
    fn parses_model_click_actions_with_mouse_defaults() {
        let raw = r#"{"response":"Clicking.","action":{"kind":"mouse_click","x":10,"y":20,"button":"left"}}"#;
        let plan = parse_model_plan(raw).unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::MouseClick {
                x: 10,
                y: 20,
                button: MouseButton::Left,
                count: 1
            })
        ));
    }

    #[test]
    fn parses_model_action_inside_provider_text_wrapper() {
        let raw = r#"Here is the action: {"response":"Clicking.","action":{"kind":"mouse_click","x":10,"y":20}}"#;
        let plan = parse_model_plan(raw).unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::MouseClick { x: 10, y: 20, .. })
        ));
    }

    #[test]
    fn desktop_context_includes_cursor_display_and_windows() {
        let desktop = DesktopState {
            schema_version: "test".to_string(),
            displays: vec![cua_core::DisplayInfo {
                id: "main".to_string(),
                name: "Built-in".to_string(),
                x: 0,
                y: 0,
                width: 1512,
                height: 982,
                scale_factor: 2.0,
                active: true,
            }],
            windows: vec![cua_core::WindowInfo {
                id: "1".to_string(),
                app_name: Some("Terminal".to_string()),
                title: Some("cua".to_string()),
                layer: 0,
                x: 10,
                y: 20,
                width: 900,
                height: 700,
                focused: true,
            }],
            cursor: cua_core::CursorState {
                x: 42.0,
                y: 64.0,
                visible: true,
                included_in_frame: true,
            },
            permissions: cua_core::PermissionReport {
                screen_recording: cua_core::PermissionState::Granted,
                accessibility_input: cua_core::PermissionState::Granted,
                input_monitoring: cua_core::PermissionState::Granted,
                automation: cua_core::PermissionState::Granted,
                clipboard: cua_core::PermissionState::Granted,
                portal: cua_core::PermissionState::NotApplicable,
            },
            latest_frame: None,
        };

        let context = desktop_context(&desktop);

        assert!(context.contains("cursor at 42,64"));
        assert!(context.contains("Built-in:1512x982@0,0"));
        assert!(context.contains("Terminal \"cua\" 900x700@10,20 focused"));
    }

    #[test]
    fn timeout_env_ignores_invalid_values() {
        assert_eq!(
            timeout_from_env("__CUA_VOICE_TEST_TIMEOUT_MISSING", 456),
            Duration::from_millis(456)
        );
    }

    #[test]
    fn retry_env_bounds_ignore_invalid_values() {
        assert_eq!(
            retry_attempts_from_env("__CUA_VOICE_TEST_RETRY_MISSING", 3),
            3
        );

        let attempts = "__CUA_VOICE_TEST_PLANNER_RETRY_ATTEMPTS";
        std::env::set_var(attempts, "9");
        assert_eq!(retry_attempts_from_env(attempts, 3), 3);
        std::env::set_var(attempts, "2");
        assert_eq!(retry_attempts_from_env(attempts, 3), 2);
        std::env::remove_var(attempts);

        let backoff = "__CUA_VOICE_TEST_PLANNER_RETRY_BACKOFF";
        std::env::set_var(backoff, "3");
        assert_eq!(
            retry_backoff_from_env(backoff, 220),
            Duration::from_millis(220)
        );
        std::env::set_var(backoff, "500");
        assert_eq!(
            retry_backoff_from_env(backoff, 220),
            Duration::from_millis(500)
        );
        std::env::remove_var(backoff);
    }

    #[test]
    fn retryable_status_covers_rate_limits_and_server_errors() {
        assert!(retryable_status(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(retryable_status(reqwest::StatusCode::BAD_GATEWAY));
        assert!(!retryable_status(reqwest::StatusCode::UNAUTHORIZED));
    }
}
