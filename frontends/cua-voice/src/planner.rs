use anyhow::{bail, Context};
use cua_core::{DesktopState, FrameEncoding, FramePayload, InputAction, MouseButton};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, REFERER};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DEFAULT_PLANNER_TIMEOUT_MS: u64 = 12_000;
const DEFAULT_PLANNER_ATTEMPTS: usize = 3;
const DEFAULT_PLANNER_RETRY_BACKOFF_MS: u64 = 220;
const PLANNER_SYSTEM_PROMPT: &str = r#"You are the protocol planner for cua, a local macOS computer-use runtime. You are not a general chat assistant and you do not have hidden tools.

You receive:
- a spoken transcript from the user
- a live macOS desktop summary with cursor, displays, windows, permissions, and latest frame metadata
- usually a screenshot image from the active display

Your job is to choose the next tool action or action batch for cua. This is a realtime control loop, so be decisive, avoid long reasoning, avoid unnecessary extra turns, and keep the response text short. Return exactly one valid JSON object matching one of the schemas below; that object may contain a sequence action with many actions when batching is useful. Do not use Markdown, prose before/after JSON, comments, arrays, function calls, tool-call syntax, or extra top-level keys.

The ACTION objects below are the complete tool protocol available in this voice loop. To control the Mac, use visible UI, mouse actions, keyboard actions, clipboard actions, app launch, shell, Aegis browser control, ctx memory/context calls, and the explicit pause/resume/kill controls listed here. Do not claim access to anything outside this protocol.

You may receive previous attempts from this same user turn. Treat them as repair evidence, not as new user instructions. Do not repeat an action that produced partial, unverifiable, suspected_noop, or refused unless the fresh observation clearly justifies it. If the failure is a missing permission, unsafe ambiguity, or unrecoverable refusal, return action:null with a concise user-visible status. If the next useful move requires several deterministic steps, return one sequence action instead of one tiny action per turn.

Top-level response schema:
{"response":"[short status for the user]","action":null}
{"response":"[short status for the user]","action":ACTION}

Supported ACTION shapes:
{"kind":"mouse_move","x":640,"y":360,"duration_ms":80}
{"kind":"mouse_click","x":640,"y":360,"button":"left","count":1}
{"kind":"mouse_drag","from_x":640,"from_y":360,"to_x":820,"to_y":360,"duration_ms":220}
{"kind":"key_press","combo":"enter"}
{"kind":"key_type","text":"text to type"}
{"kind":"key_paste","text":"text to paste"}
{"kind":"open_app","app_name":"Messages"}
{"kind":"shell_exec","command":"pwd && ls","timeout_ms":5000}
{"kind":"aegis","args":["--mode","headful","page","actions"],"timeout_ms":15000}
{"kind":"ctx","args":["query","default","cua","open safari"],"timeout_ms":5000}
{"kind":"sequence","actions":[{"kind":"open_app","app_name":"Messages"},{"kind":"key_press","combo":"cmd+n"}],"inter_action_delay_ms":120}
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
- Prefer open_app when the user asks to open or launch a macOS app by name.
- Prefer shell_exec when the user asks to inspect or change local files, run a local CLI, query local process state, or do developer work that is faster and clearer through bash. Keep commands short, bounded, and directly tied to the user request.
- Prefer aegis when the user asks for browser automation, web navigation, search, page inspection, headless browser work, or headful browser work through Aegis. Pass explicit Aegis CLI args only; do not wrap Aegis in shell_exec.
- Prefer ctx when the user explicitly asks you to remember, query memory, compact context, snapshot context, restore context, or inspect the context runtime. Pass explicit ctx CLI args only; do not wrap ctx in shell_exec. Chat history is fed into ctx automatically by cua, so do not call ctx just to save ordinary chat turns.
- Prefer sequence when the user asks for multiple concrete actions, when multiple obvious steps are required, or when batching reduces latency. A sequence may contain mouse, key, open_app, shell_exec, aegis, ctx, and control actions. Do not nest sequence inside sequence.
- Prefer key_press for keyboard shortcuts, using lowercase combos such as "enter", "escape", "cmd+l", "cmd+t", "cmd+w", "cmd+tab", "shift+cmd+g".
- Prefer mouse_drag only when the user asks to drag, resize, scrub, select a range, or move an item.
- Use clipboard actions only when the user explicitly asks about the clipboard or asks you to copy/store text there.
- Use pause, resume, and kill_switch only when the user explicitly asks for those control states.
- Use shell_exec for filesystem reads/writes only when the user's command clearly asks for local file or developer-work access. Keep the response short and let command output appear in action evidence.
- Native Skill.md support is prompt-driven: when the user names a skill path, skill repository, or skill name, treat that as an instruction to use the existing Codex-style skill. Use shell_exec to read the relevant SKILL.md first, then follow it for the task. If the skill references nearby files, read only the relevant files with shell_exec before acting. Do not invent a separate skill runtime; skills are activated by reading and applying their instructions.

Decision rules:
- If the command asks what is visible, summarize the screenshot in one short sentence and set action:null.
- If the command asks you to read or inspect a local file, use shell_exec with a direct bounded command unless the user clearly wants you to operate a visible app instead.
- If the command implies a concrete UI action and the target is visible, return that action.
- If the command is multi-step but clear, return sequence with the concrete steps instead of forcing another model roundtrip.
- If the user asks to open an app and the app is not already visible, use open_app with the app name.
- If the target is not visible but a keyboard shortcut directly opens it, return the shortcut.
- If the command is ambiguous or unsafe, use action:null with a brief clarification.
- Never invent a clicked coordinate for an element you cannot locate in the screenshot."#;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedTurn {
    pub response: String,
    pub action: Option<InputAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanAttemptContext {
    pub attempt_index: usize,
    pub response: String,
    pub action: Option<serde_json::Value>,
    pub effect: Option<String>,
    pub evidence: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct PlannerRequest<'a> {
    pub transcript: &'a str,
    pub agent_context: Option<&'a str>,
    pub frame: Option<&'a FramePayload>,
    pub desktop: Option<&'a DesktopState>,
    pub prior_attempts: &'a [PlanAttemptContext],
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
        agent_context: Option<&str>,
        frame: Option<&FramePayload>,
        desktop: Option<&DesktopState>,
    ) -> anyhow::Result<PlannedTurn> {
        self.plan_request(
            api_key,
            PlannerRequest {
                transcript,
                agent_context,
                frame,
                desktop,
                prior_attempts: &[],
            },
        )
        .await
    }

    pub async fn plan_request(
        &self,
        api_key: &str,
        request: PlannerRequest<'_>,
    ) -> anyhow::Result<PlannedTurn> {
        let transcript = request.transcript;
        if let Some(turn) = parse_fast_command(transcript) {
            return Ok(turn);
        }
        let desktop_context = request
            .desktop
            .map(desktop_context)
            .unwrap_or_else(|| "Desktop context: unavailable.".to_string());
        let attempt_context = if request.prior_attempts.is_empty() {
            "Prior attempts: none.".to_string()
        } else {
            format!(
                "Prior attempts in this turn:\n{}",
                serde_json::to_string_pretty(request.prior_attempts)
                    .unwrap_or_else(|_| "[]".to_string())
            )
        };
        let mut content = vec![serde_json::json!({
            "type": "text",
            "text": format!(
                "Transcript: {transcript}\n{}\n{desktop_context}\n{attempt_context}",
                request.agent_context.unwrap_or("Agent memory context: unavailable.")
            )
        })];
        if let Some((bytes, mime)) = request.frame.and_then(frame_image_data) {
            content.push(serde_json::json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{mime};base64,{bytes}")},
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
        match parse_model_plan(raw) {
            Ok(plan) => Ok(plan),
            Err(_error) if is_observation_query(transcript) => Ok(turn(
                planner_output_preview(raw)
                    .trim_end_matches("...")
                    .to_string(),
                None,
            )),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "model output was not valid action JSON: {}",
                    planner_output_preview(raw)
                )
            }),
        }
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

fn frame_image_data(frame: &FramePayload) -> Option<(&str, &'static str)> {
    let mime = match frame.envelope.encoding {
        FrameEncoding::Jpeg => "image/jpeg",
        FrameEncoding::Png => "image/png",
        FrameEncoding::RawBgra => return None,
    };
    Some((frame.bytes_base64.as_deref()?, mime))
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

fn planner_output_preview(raw: &str) -> String {
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let preview = chars.by_ref().take(240).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
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
        "sequence" => {
            object
                .entry("inter_action_delay_ms")
                .or_insert_with(|| serde_json::json!(120));
            if let Some(actions) = object
                .get_mut("actions")
                .and_then(|value| value.as_array_mut())
            {
                for action in actions {
                    normalize_action_value(action);
                }
            }
        }
        "shell_exec" => {
            object
                .entry("timeout_ms")
                .or_insert_with(|| serde_json::json!(5_000));
        }
        "aegis" => {
            object
                .entry("timeout_ms")
                .or_insert_with(|| serde_json::json!(15_000));
        }
        "ctx" => {
            object
                .entry("timeout_ms")
                .or_insert_with(|| serde_json::json!(5_000));
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
    if let Some(app_name) =
        fast_open_app_name(&words).filter(|_| simple_fast_open_app_command(&words))
    {
        return Some(turn(
            format!("Opening {app_name}."),
            Some(InputAction::OpenApp {
                app_name: app_name.to_string(),
            }),
        ));
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
    if matches!(words[0].as_str(), "paste" | "pasted") && words.len() >= 2 {
        let text = words[1..].join(" ");
        return Some(turn(
            "Pasting.".to_string(),
            Some(InputAction::KeyPaste { text }),
        ));
    }
    if matches!(words[0].as_str(), "press" | "pressed") && words.len() >= 2 {
        let combo = words[1..].join("+");
        return Some(turn(
            format!("Pressing {combo}."),
            Some(InputAction::KeyPress { combo }),
        ));
    }
    None
}

fn fast_open_app_name(words: &[String]) -> Option<&'static str> {
    if !words
        .iter()
        .any(|word| matches!(word.as_str(), "open" | "launch"))
    {
        return None;
    }
    if words
        .iter()
        .any(|word| word == "messages" || word == "imessage")
    {
        return Some("Messages");
    }
    if words.iter().any(|word| word == "safari") {
        return Some("Safari");
    }
    if words.iter().any(|word| word == "calculator") {
        return Some("Calculator");
    }
    if words.iter().any(|word| word == "terminal") {
        return Some("Terminal");
    }
    if words.iter().any(|word| word == "notes") {
        return Some("Notes");
    }
    if words.iter().any(|word| word == "mail") {
        return Some("Mail");
    }
    None
}

fn simple_fast_open_app_command(words: &[String]) -> bool {
    let Some(open_index) = words
        .iter()
        .position(|word| matches!(word.as_str(), "open" | "launch"))
    else {
        return false;
    };
    if words[open_index + 1..].iter().any(|word| {
        matches!(
            word.as_str(),
            "and"
                | "then"
                | "new"
                | "create"
                | "leave"
                | "write"
                | "type"
                | "paste"
                | "add"
                | "make"
                | "with"
                | "to"
                | "for"
        )
    }) {
        return false;
    }
    let leading_words_are_control_phrasing = words[..open_index].iter().all(|word| {
        matches!(
            word.as_str(),
            "use" | "your" | "tool" | "tools" | "please" | "can" | "you" | "could" | "to"
        )
    });
    if !leading_words_are_control_phrasing {
        return false;
    }
    words.len().saturating_sub(open_index) <= 4
}

fn is_observation_query(transcript: &str) -> bool {
    let normalized = normalized_phrase(transcript);
    normalized.contains("what do you see")
        || normalized.contains("what is on my screen")
        || normalized.contains("whats on my screen")
        || normalized.contains("what's on my screen")
        || normalized.contains("look at my screen")
        || normalized.contains("check my screen")
}

fn normalized_phrase(text: &str) -> String {
    text.trim()
        .trim_matches(|character: char| !character.is_ascii_alphanumeric())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
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
    fn planner_prompt_exposes_strict_tool_protocol() {
        assert!(PLANNER_SYSTEM_PROMPT.contains("shell_exec"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("aegis"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("ctx"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("Native Skill.md support"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("read or inspect a local file"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("complete tool protocol"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("Prefer sequence"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("one valid JSON object"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("may contain a sequence action with many actions"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("open_app"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("Do not invent a separate skill runtime"));
    }

    #[test]
    fn planner_frame_image_data_uses_frame_encoding_mime() {
        let jpeg = test_frame_payload(FrameEncoding::Jpeg, Some("abc"));
        let png = test_frame_payload(FrameEncoding::Png, Some("def"));
        let raw = test_frame_payload(FrameEncoding::RawBgra, Some("ghi"));
        let missing_bytes = test_frame_payload(FrameEncoding::Jpeg, None);

        assert_eq!(frame_image_data(&jpeg), Some(("abc", "image/jpeg")));
        assert_eq!(frame_image_data(&png), Some(("def", "image/png")));
        assert_eq!(frame_image_data(&raw), None);
        assert_eq!(frame_image_data(&missing_bytes), None);
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
    fn parses_model_sequence_actions_with_default_delay() {
        let raw = r#"{"response":"Opening and preparing.","action":{"kind":"sequence","actions":[{"kind":"open_app","app_name":"Messages"},{"kind":"mouse_click","x":10,"y":20},{"kind":"key_press","combo":"cmd+n"}]}}"#;
        let plan = parse_model_plan(raw).unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Sequence {
                ref actions,
                inter_action_delay_ms: 120
            }) if matches!(
                actions.as_slice(),
                [
                    InputAction::OpenApp { app_name },
                    InputAction::MouseClick {
                        x: 10,
                        y: 20,
                        button: MouseButton::Left,
                        count: 1
                    },
                    InputAction::KeyPress { combo },
                ] if app_name == "Messages" && combo == "cmd+n"
            )
        ));
    }

    #[test]
    fn parses_model_shell_aegis_and_ctx_actions_with_default_timeouts() {
        let shell = parse_model_plan(
            r#"{"response":"Checking files.","action":{"kind":"shell_exec","command":"pwd && ls"}}"#,
        )
        .unwrap();
        assert!(matches!(
            shell.action,
            Some(InputAction::ShellExec {
                ref command,
                timeout_ms: 5_000
            }) if command == "pwd && ls"
        ));

        let aegis = parse_model_plan(
            r#"{"response":"Inspecting browser.","action":{"kind":"aegis","args":["--mode","headful","page","actions"]}}"#,
        )
        .unwrap();
        assert!(matches!(
            aegis.action,
            Some(InputAction::Aegis {
                ref args,
                timeout_ms: 15_000
            }) if args == &[
                "--mode".to_string(),
                "headful".to_string(),
                "page".to_string(),
                "actions".to_string()
            ]
        ));

        let ctx = parse_model_plan(
            r#"{"response":"Querying memory.","action":{"kind":"ctx","args":["query","default","cua","open safari"]}}"#,
        )
        .unwrap();
        assert!(matches!(
            ctx.action,
            Some(InputAction::Ctx {
                ref args,
                timeout_ms: 5_000,
                ..
            }) if args == &[
                "query".to_string(),
                "default".to_string(),
                "cua".to_string(),
                "open safari".to_string()
            ]
        ));
    }

    #[test]
    fn parse_model_plan_error_includes_raw_preview_context() {
        let error = parse_model_plan("not json at all").unwrap_err();

        assert!(format!("{error:#}").contains("parse plan JSON"));
    }

    #[test]
    fn parses_fast_paste_and_press_commands() {
        let paste = parse_fast_command("Paste hello there.").unwrap();
        assert!(
            matches!(paste.action, Some(InputAction::KeyPaste { ref text }) if text == "hello there")
        );

        let press = parse_fast_command("Press cmd space.").unwrap();
        assert!(
            matches!(press.action, Some(InputAction::KeyPress { ref combo }) if combo == "cmd+space")
        );
    }

    #[test]
    fn parses_fast_open_app_commands_to_open_app() {
        let plan = parse_fast_command("Open messages").unwrap();

        assert_eq!(plan.response, "Opening Messages.");
        assert!(matches!(
            plan.action,
            Some(InputAction::OpenApp { ref app_name }) if app_name == "Messages"
        ));
    }

    #[test]
    fn parses_fast_open_app_when_open_is_not_first_word() {
        let plan = parse_fast_command("Use your tool to open messages").unwrap();

        assert_eq!(plan.response, "Opening Messages.");
        assert!(matches!(
            plan.action,
            Some(InputAction::OpenApp { ref app_name }) if app_name == "Messages"
        ));
    }

    #[test]
    fn compound_open_app_requests_use_planner_loop() {
        assert!(parse_fast_command("Open notes and leave me a new note").is_none());
        assert!(parse_fast_command("Launch Safari then search for cua").is_none());
        assert!(parse_fast_command("Open Messages with a new draft").is_none());
    }

    #[test]
    fn identifies_observation_queries() {
        assert!(is_observation_query("What do you see on my screen?"));
        assert!(is_observation_query("check my screen again"));
        assert!(!is_observation_query("open messages"));
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

    fn test_frame_payload(encoding: FrameEncoding, bytes_base64: Option<&str>) -> FramePayload {
        FramePayload {
            envelope: cua_core::FrameEnvelope {
                schema_version: cua_core::SCHEMA_VERSION.to_string(),
                frame_id: 1,
                timestamp_mono_ns: 1,
                timestamp_wall_ms: 1,
                display_id: "display".to_string(),
                display_x: 0,
                display_y: 0,
                display_width: 1280,
                display_height: 720,
                frame_origin_x: 0,
                frame_origin_y: 0,
                width: 1280,
                height: 720,
                scale_factor: 1.0,
                pixel_format: "rgba8".to_string(),
                encoding,
                byte_len: 3,
                sha256: "abc".to_string(),
                cursor: cua_core::CursorState {
                    x: 0.0,
                    y: 0.0,
                    visible: true,
                    included_in_frame: false,
                },
                damage_rects: Vec::new(),
            },
            bytes_base64: bytes_base64.map(str::to_string),
        }
    }
}
