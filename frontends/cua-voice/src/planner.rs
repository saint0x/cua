use anyhow::{bail, Context};
use cua_core::{DesktopState, FrameEncoding, FramePayload, InputAction, MouseButton};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, REFERER};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const DEFAULT_PLANNER_TIMEOUT_MS: u64 = 12_000;
const DEFAULT_PLANNER_ATTEMPTS: usize = 3;
const DEFAULT_PLANNER_OUTPUT_ATTEMPTS: usize = 2;
const DEFAULT_PLANNER_RETRY_BACKOFF_MS: u64 = 220;
const DEFAULT_PLANNER_MAX_TOKENS: u32 = 768;
const DEFAULT_PLANNER_TEXT_MAX_TOKENS: u32 = 1_200;
const MODEL_VISIBLE_PRIOR_ATTEMPTS_MAX: usize = 8;
const MODEL_VISIBLE_EVIDENCE_ITEMS_MAX: usize = 4;
const MODEL_VISIBLE_TEXT_MAX_CHARS: usize = 700;
const PLANNER_CHAT_COMPLETIONS_URL_ENV: &str = "CUA_VOICE_PLANNER_CHAT_COMPLETIONS_URL";
const GEMINI_OPENAI_CHAT_COMPLETIONS_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions";
const OPENROUTER_CHAT_COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const PLANNER_SYSTEM_PROMPT: &str = r#"You are the protocol planner for cua, a backend-neutral computer-use runtime. You are not a general chat assistant and you do not have hidden tools.

You receive:
- a spoken transcript from the user
- a live computer summary with backend, cursor, displays, windows, permissions, and latest frame metadata
- usually a screenshot image from the active display

Your job is to advance the user's current goal by choosing the next tool action or action batch for cua. You are operating inside cua's runtime-governed RLM loop: observe, plan, act, verify, repair, and continue until the goal is complete, blocked by a real permission/safety issue, or requires user clarification. The default loop budget is unbounded; do not invent a five-turn or one-shot stopping point. This is a realtime control loop, so be decisive, avoid long reasoning, avoid unnecessary extra roundtrips, and keep the response text short. Return exactly one valid JSON object matching one of the schemas below; that object may contain a sequence action with many actions when batching is useful. Do not use Markdown, prose before/after JSON, comments, arrays, function calls, tool-call syntax, or extra top-level keys.

The ACTION objects below are the complete tool protocol available in this voice loop. To control the active computer backend, use visible UI, mouse actions, keyboard actions, clipboard actions, app launch, shell, the browser automation adapter, ctx memory/context calls, profile scratchpad state exposed by cua CLI/Unix/HTTP, and the explicit pause/resume/kill controls listed here. Do not claim access to anything outside this protocol.

You may receive previous attempts from this same user turn. Treat them as RLM repair evidence, not as new user instructions. Use that evidence to choose the next useful move toward completion; do not collapse a multi-step goal into a polite one-shot reply after only opening, clicking, or typing once. Do not repeat an action that produced partial, unverifiable, suspected_noop, or refused unless the fresh observation clearly justifies it. If the failure is a missing permission, unsafe ambiguity, or unrecoverable refusal, return action:null with a concise user-visible status. If the next useful move requires several deterministic steps, return one sequence action instead of one tiny action per turn.

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
{"kind":"sequence","actions":[{"kind":"open_app","app_name":"Safari"},{"kind":"key_press","combo":"cmd+l"},{"kind":"key_paste","text":"cloud computer agents"},{"kind":"key_press","combo":"enter"}],"inter_action_delay_ms":120}
{"kind":"aegis","args":["--mode","headless","search","cloud computer agents"],"timeout_ms":15000}
{"kind":"aegis","args":["--mode","headful","page","actions"],"timeout_ms":15000}
{"kind":"aegis","args":["--mode","headful","page","links"],"timeout_ms":15000}
{"kind":"aegis","args":["--mode","headful","page","text","--scope","main"],"timeout_ms":15000}
{"kind":"aegis","args":["--mode","headful","page","open-link","AWS Bedrock"],"timeout_ms":15000}
{"kind":"aegis","args":["--mode","headful","page","open-link","--exact","AWS Bedrock"],"timeout_ms":15000}
{"kind":"aegis","args":["--mode","headful","page","open-link","--href-contains","aws.amazon.com","AWS Bedrock"],"timeout_ms":15000}
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
- Prefer open_app when the user asks only to open or launch a desktop app by name.
- Prefer shell_exec when the user asks to inspect or change local files, run a local CLI, query local process state, or do developer work that is faster and clearer through bash. Keep commands short, bounded, and directly tied to the user request.
- For normal browser/search work, use the active computer backend naturally: visible browser UI actions are valid when the target is visible or the user asks to use Safari/browser normally. Use the Aegis action when the user explicitly asks for Aegis/headless browser control, when page-structured inspection is the clearest available path, or when normal visible UI is not enough to verify the task.
- When using Aegis, pass explicit Aegis CLI args only; do not wrap Aegis in shell_exec. Supported Aegis research forms are `search <query words>`, `navigate <url>`, `page actions`, `page links`, `page text --scope main`, `page markdown --scope article`, `page find <text>`, and `page open-link <link text>`. If `page open-link` is ambiguous, use `page links`, then retry `page open-link` with `--exact`, `--index`, or `--href-contains` followed immediately by its value. Do not emit incomplete options, trailing `--`, runtime-only `--server-addr`/`--profile`, or unsupported commands such as `page actions --url` or `page click --index`; use `navigate` before page inspection and `page open-link` for links.
- Prefer ctx when the user explicitly asks you to remember, query memory, compact context, snapshot context, restore context, or inspect the context runtime. Pass explicit ctx CLI args only; do not wrap ctx in shell_exec. Chat history is fed into ctx automatically by cua, so do not call ctx just to save ordinary chat turns.
- Profile scratchpads are fed into planner context automatically. If the user explicitly asks to add, read, list, or delete a scratchpad, use shell_exec with the bounded cua scratchpad CLI command and the active owner session only when that session is available in the runtime evidence.
- Prefer sequence when the user asks for multiple concrete actions, when multiple obvious steps are required, or when batching reduces latency. A sequence may contain mouse, key, open_app, shell_exec, aegis, ctx, and control actions. Do not nest sequence inside sequence.
- Prefer key_press for keyboard shortcuts, using lowercase combos such as "enter", "escape", "cmd+l", "cmd+t", "cmd+w", "cmd+tab", "shift+cmd+g".
- Prefer key_paste, not key_type, when leaving exact user-provided content inside an app after creating or focusing a field. This is the production writing path for note/message/body text.
- If the user asks you to write, leave, paste, type, draft, or create text in an app, the returned action must include the text-entry action in the same response, usually as a sequence with open_app, key_press or focusing, then key_paste. Returning only open_app for a text-writing command is invalid.
- Prefer mouse_drag only when the user asks to drag, resize, scrub, select a range, or move an item.
- Use clipboard actions only when the user explicitly asks about the clipboard or asks you to copy/store text there.
- Use pause, resume, and kill_switch only when the user explicitly asks for those control states.
- Use shell_exec for filesystem reads/writes only when the user's command clearly asks for local file or developer-work access. Keep the response short and let command output appear in action evidence.
- Native Skill.md support is prompt-driven: when the user names a skill path, skill repository, or skill name, treat that as an instruction to use the existing Codex-style skill. Use shell_exec to read the relevant SKILL.md first, then follow it for the task. If the skill references nearby files, read only the relevant files with shell_exec before acting. Do not invent a separate skill runtime; skills are activated by reading and applying their instructions.

Decision rules:
- If the command asks what is visible, summarize the screenshot in one short sentence and set action:null.
- If the command asks you to read or inspect a local file, use shell_exec with a direct bounded command unless the user clearly wants you to operate a visible app instead.
- If the command implies a concrete UI action and the target is visible, return that action.
- Opening a browser or app is setup only for research, browsing, search, reading, comparison, or other long-range work. Continue with aegis, shell_exec, visible UI actions, or another useful action until the real goal is satisfied and verified.
- For long-range tasks, return the best next action or sequence for the current state, then use later RLM attempts to verify and continue. Only finish with action:null when the user's goal is actually satisfied, impossible without permission, unsafe, or ambiguous.
- Keep user-visible response text semantic and natural. Do not echo internal effect labels such as "confirmed", "sent", "partial", "suspected_noop", or "unverifiable"; use those only as prior-attempt evidence.
- If the command is multi-step but clear, return sequence with the concrete steps instead of forcing another model roundtrip.
- If the user asks you to observe an expected failure before recovering, do not combine the failing action and recovery into one shell command or one sequence. First return only the failing read/action so cua can observe failed evidence; use the next RLM attempt to recover.
- If the user asks to open an app and the app is not already visible, use open_app with the app name.
- Before reporting that a visible/computer-use task is done, take or use a fresh screenshot/reobserve pass after dispatch when the runtime provides it. Do not claim that text was written, a file changed, or an app state was reached solely because an input event was posted; rely on fresh observation/evidence when available, and repair if verification contradicts the intended result.
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannerHints {
    pub detected_actions: Vec<InputAction>,
    pub notes: Vec<String>,
}

impl PlannerHints {
    pub fn is_empty(&self) -> bool {
        self.detected_actions.is_empty() && self.notes.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct PlannerRequest<'a> {
    pub transcript: &'a str,
    pub agent_context: Option<&'a str>,
    pub hints: Option<&'a PlannerHints>,
    pub frame: Option<&'a FramePayload>,
    pub desktop: Option<&'a DesktopState>,
    pub prior_attempts: &'a [PlanAttemptContext],
}

#[derive(Debug, Clone)]
pub struct Planner {
    client: reqwest::Client,
    model: String,
    provider: PlannerProvider,
}

impl Planner {
    pub fn new(model: impl Into<String>) -> Self {
        let model = model.into();
        let provider = PlannerProvider::from_model(&model);
        Self {
            client: reqwest::Client::new(),
            model,
            provider,
        }
    }

    pub fn api_key_from_env(&self) -> Option<String> {
        self.provider.api_key_from_env()
    }

    pub fn required_api_key_name(&self) -> &'static str {
        self.provider.required_api_key_name()
    }

    pub fn planning_tool_label(&self, attempt_index: usize, formatted_attempt: String) -> String {
        self.provider
            .planning_tool_label(attempt_index, formatted_attempt)
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
                hints: None,
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
        let desktop_context = request
            .desktop
            .map(desktop_context)
            .unwrap_or_else(|| "Desktop context: unavailable.".to_string());
        let model_prior_attempts = model_visible_prior_attempts(request.prior_attempts);
        let attempt_context = if model_prior_attempts.is_empty() {
            "Prior attempts: none.".to_string()
        } else {
            format!(
                "Prior attempts in this turn:\n{}",
                serde_json::to_string_pretty(&model_prior_attempts)
                    .unwrap_or_else(|_| "[]".to_string())
            )
        };
        let hints_context = request
            .hints
            .filter(|hints| !hints.is_empty())
            .and_then(|hints| serde_json::to_string_pretty(hints).ok())
            .map(|hints| {
                format!(
                    "Planner hints from deterministic transcript parsing. These are suggestions only; use, revise, reorder, or ignore them based on the screenshot and user intent:\n{hints}"
                )
            })
            .unwrap_or_else(|| "Planner hints: none.".to_string());
        let mut content = vec![serde_json::json!({
            "type": "text",
            "text": format!(
                "Transcript: {transcript}\n{}\n{hints_context}\n{desktop_context}\n{attempt_context}",
                request.agent_context.unwrap_or("Agent memory context: unavailable.")
            )
        })];
        if let Some((bytes, mime)) = request.frame.and_then(frame_image_data) {
            content.push(serde_json::json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{mime};base64,{bytes}")},
            }));
        }
        let body = self.request_body(transcript, content);
        let output_attempts = retry_attempts_from_env(
            "CUA_VOICE_PLANNER_OUTPUT_ATTEMPTS",
            DEFAULT_PLANNER_OUTPUT_ATTEMPTS,
        );
        let backoff = retry_backoff_from_env(
            "CUA_VOICE_PLANNER_RETRY_BACKOFF_MS",
            DEFAULT_PLANNER_RETRY_BACKOFF_MS,
        );
        let mut last_error = None;
        for output_attempt in 1..=output_attempts {
            let response = self.send_planning_request(api_key, &body).await?;
            let status = response.status();
            let value: serde_json::Value =
                response.json().await.context("decode planning response")?;
            if !status.is_success() {
                bail!("planning failed with {status}: {value}");
            }
            let raw = value["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_default();
            if raw.trim().is_empty() {
                last_error = Some(empty_planner_response_error(&value));
            } else {
                match parse_model_plan(raw) {
                    Ok(plan) => return Ok(plan),
                    Err(_error) if is_observation_query(transcript) => {
                        return Ok(turn(
                            planner_output_preview(raw)
                                .trim_end_matches("...")
                                .to_string(),
                            None,
                        ));
                    }
                    Err(error) => {
                        last_error = Some(error).map(|error| {
                            error.context(format!(
                                "model output was not valid action JSON: {}",
                                planner_output_preview(raw)
                            ))
                        });
                    }
                }
            }
            if output_attempt < output_attempts {
                tokio::time::sleep(backoff).await;
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("planning model produced no output")))
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
                .post(self.provider.chat_completions_url())
                .headers(self.provider.headers(api_key)?)
                .json(body)
                .timeout(planner_request_timeout())
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

    fn request_body(&self, transcript: &str, content: Vec<serde_json::Value>) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.provider.request_model(&self.model),
            "messages": [
                {"role": "system", "content": PLANNER_SYSTEM_PROMPT},
                {"role": "user", "content": content}
            ],
            "max_tokens": planner_max_tokens(transcript),
            "response_format": {"type": "json_object"},
        });
        if self.provider.should_pin_temperature(&self.model) {
            body["temperature"] = serde_json::json!(0);
        }
        body
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlannerProvider {
    Gemini,
    OpenRouter,
}

impl PlannerProvider {
    fn from_model(model: &str) -> Self {
        if model.starts_with("gemini-") {
            Self::Gemini
        } else {
            Self::OpenRouter
        }
    }

    fn request_model(self, model: &str) -> String {
        match self {
            Self::Gemini => model.to_string(),
            Self::OpenRouter => model
                .strip_prefix("openrouter/")
                .unwrap_or(model)
                .to_string(),
        }
    }

    fn default_chat_completions_url(self) -> &'static str {
        match self {
            Self::Gemini => GEMINI_OPENAI_CHAT_COMPLETIONS_URL,
            Self::OpenRouter => OPENROUTER_CHAT_COMPLETIONS_URL,
        }
    }

    fn chat_completions_url(self) -> String {
        std::env::var(PLANNER_CHAT_COMPLETIONS_URL_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| self.default_chat_completions_url().to_string())
    }

    fn required_api_key_name(self) -> &'static str {
        match self {
            Self::Gemini => "GEMINI_API_KEY or GOOGLE_API_KEY",
            Self::OpenRouter => "OPENROUTER_API_KEY",
        }
    }

    fn api_key_from_env(self) -> Option<String> {
        match self {
            Self::Gemini => std::env::var("GEMINI_API_KEY")
                .ok()
                .or_else(|| std::env::var("GOOGLE_API_KEY").ok()),
            Self::OpenRouter => std::env::var("OPENROUTER_API_KEY").ok(),
        }
    }

    fn headers(self, api_key: &str) -> anyhow::Result<reqwest::header::HeaderMap> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(CONTENT_TYPE, "application/json".parse()?);
        headers.insert(AUTHORIZATION, format!("Bearer {api_key}").parse()?);
        if self == Self::OpenRouter {
            headers.insert(REFERER, "http://localhost/cua".parse()?);
        }
        Ok(headers)
    }

    fn should_pin_temperature(self, model: &str) -> bool {
        self == Self::OpenRouter && !model.contains("gemini-3")
    }

    fn planning_tool_label(self, attempt_index: usize, formatted_attempt: String) -> String {
        let provider = match self {
            Self::Gemini => "Gemini",
            Self::OpenRouter => "OpenRouter",
        };
        if attempt_index == 1 {
            format!("{provider} Vision")
        } else {
            format!("{provider} repair {formatted_attempt}")
        }
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

fn model_visible_prior_attempts(prior_attempts: &[PlanAttemptContext]) -> Vec<PlanAttemptContext> {
    prior_attempts
        .iter()
        .skip(
            prior_attempts
                .len()
                .saturating_sub(MODEL_VISIBLE_PRIOR_ATTEMPTS_MAX),
        )
        .cloned()
        .map(|mut attempt| {
            if let Some(action) = attempt.action.as_mut() {
                strip_runtime_only_action_args(action);
            }
            if let Some(evidence) = attempt.evidence.as_mut() {
                compact_model_visible_evidence(evidence);
            }
            normalize_model_visible_attempt_status(&mut attempt);
            attempt
        })
        .collect()
}

fn normalize_model_visible_attempt_status(attempt: &mut PlanAttemptContext) {
    if attempt.effect.as_deref() == Some("confirmed") {
        attempt.effect = Some("accepted".to_string());
    }
    if let Some(evidence) = attempt.evidence.as_mut() {
        normalize_model_visible_evidence_status(evidence);
    }
}

fn normalize_model_visible_evidence_status(evidence: &mut serde_json::Value) {
    match evidence {
        serde_json::Value::Object(object) => {
            if object
                .get("effect")
                .and_then(|effect| effect.as_str())
                .is_some_and(|effect| effect == "confirmed")
            {
                object.insert("effect".to_string(), serde_json::json!("accepted"));
            }
            for value in object.values_mut() {
                normalize_model_visible_evidence_status(value);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                normalize_model_visible_evidence_status(item);
            }
        }
        _ => {}
    }
}

fn strip_runtime_only_action_args(action: &mut serde_json::Value) {
    match action.get("kind").and_then(|kind| kind.as_str()) {
        Some("aegis") => strip_runtime_only_aegis_args(action),
        Some("sequence") => {
            if let Some(actions) = action
                .get_mut("actions")
                .and_then(|actions| actions.as_array_mut())
            {
                for action in actions {
                    strip_runtime_only_action_args(action);
                }
            }
        }
        _ => {}
    }
}

fn strip_runtime_only_aegis_args(action: &mut serde_json::Value) {
    let Some(args) = action.get_mut("args").and_then(|args| args.as_array_mut()) else {
        return;
    };
    let mut cleaned = Vec::with_capacity(args.len());
    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index].as_str();
        let runtime_option = matches!(arg, Some("--server-addr" | "--profile"));
        if runtime_option {
            index += 2;
        } else {
            cleaned.push(args[index].clone());
            index += 1;
        }
    }
    *args = cleaned;
}

fn compact_model_visible_evidence(evidence: &mut serde_json::Value) {
    match evidence {
        serde_json::Value::Object(object) => {
            for value in object.values_mut() {
                compact_model_visible_evidence(value);
            }
            if let Some(items) = object
                .get_mut("evidence")
                .and_then(|items| items.as_array_mut())
            {
                if items.len() > MODEL_VISIBLE_EVIDENCE_ITEMS_MAX {
                    let keep_from = items.len() - MODEL_VISIBLE_EVIDENCE_ITEMS_MAX;
                    items.drain(0..keep_from);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                compact_model_visible_evidence(item);
            }
        }
        serde_json::Value::String(text) => {
            *text = truncate_model_visible_text(text, MODEL_VISIBLE_TEXT_MAX_CHARS);
        }
        _ => {}
    }
}

fn truncate_model_visible_text(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }
    let head_len = max_chars.saturating_sub(3);
    let mut truncated = text.chars().take(head_len).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn planner_request_timeout() -> Duration {
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

fn planner_max_tokens(transcript: &str) -> u32 {
    let env_name = if planner_transcript_requests_text_payload(transcript) {
        "CUA_VOICE_PLANNER_TEXT_MAX_TOKENS"
    } else {
        "CUA_VOICE_PLANNER_MAX_TOKENS"
    };
    let default = if env_name == "CUA_VOICE_PLANNER_TEXT_MAX_TOKENS" {
        DEFAULT_PLANNER_TEXT_MAX_TOKENS
    } else {
        DEFAULT_PLANNER_MAX_TOKENS
    };
    std::env::var(env_name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|value| (64..=4_000).contains(value))
        .unwrap_or(default)
}

fn planner_transcript_requests_text_payload(transcript: &str) -> bool {
    transcript
        .split_whitespace()
        .map(normalize_command_token)
        .any(|word| {
            matches!(
                word.as_str(),
                "write"
                    | "type"
                    | "paste"
                    | "leave"
                    | "message"
                    | "says"
                    | "content"
                    | "contents"
                    | "story"
                    | "draft"
                    | "marker"
                    | "shell"
                    | "file"
                    | "directory"
                    | "input"
                    | "output"
                    | "transform"
                    | "transformed"
                    | "uppercased"
                    | "uppercase"
                    | "readback"
            )
        })
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

fn empty_planner_response_error(value: &serde_json::Value) -> anyhow::Error {
    let choice = &value["choices"][0];
    let message = &choice["message"];
    let finish_reason = choice["finish_reason"]
        .as_str()
        .or_else(|| value["finish_reason"].as_str())
        .unwrap_or("unknown");
    let message_keys = message
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>().join(","))
        .unwrap_or_else(|| "unavailable".to_string());
    let refusal = message["refusal"]
        .as_str()
        .map(planner_output_preview)
        .unwrap_or_else(|| "none".to_string());
    let tool_call_count = message["tool_calls"]
        .as_array()
        .map(|calls| calls.len())
        .unwrap_or(0);
    let provider_error = value["error"]["message"]
        .as_str()
        .map(planner_output_preview)
        .unwrap_or_else(|| "none".to_string());

    anyhow::anyhow!(
        "planning model returned empty content: finish_reason={finish_reason} message_keys={message_keys} refusal={refusal} tool_calls={tool_call_count} provider_error={provider_error}"
    )
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
    let action = serde_json::from_value(value).context("parse action")?;
    validate_model_action(&action)?;
    Ok(action)
}

fn validate_model_action(action: &InputAction) -> anyhow::Result<()> {
    match action {
        InputAction::Aegis { args, .. } => validate_aegis_args(args),
        InputAction::Sequence { actions, .. } => {
            if actions.is_empty() {
                bail!("sequence action must contain at least one action");
            }
            for action in actions {
                validate_model_action(action)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_aegis_args(args: &[String]) -> anyhow::Result<()> {
    if args.is_empty() {
        bail!("aegis action requires args");
    }
    if args.iter().any(|arg| arg.trim().is_empty()) {
        bail!("aegis action contains an empty argument");
    }

    let Some(command_index) = aegis_command_index(args)? else {
        bail!("aegis action must include a command");
    };
    match args[command_index].as_str() {
        "search" => {
            if command_index + 1 >= args.len() {
                bail!("aegis search requires a query");
            }
            Ok(())
        }
        "navigate" => {
            if command_index + 2 != args.len() {
                bail!("aegis navigate accepts exactly one URL");
            }
            validate_aegis_navigation_target(args.get(command_index + 1))
        }
        "page" => validate_aegis_page_args(&args[command_index + 1..]),
        command => bail!("unsupported aegis command `{command}`"),
    }
}

fn validate_aegis_navigation_target(target: Option<&String>) -> anyhow::Result<()> {
    let Some(target) = target else {
        bail!("aegis navigate requires a URL");
    };
    let url = reqwest::Url::parse(target).context("parse aegis navigate URL")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("aegis navigate requires an absolute http(s) URL with a host");
    }
    Ok(())
}

fn aegis_command_index(args: &[String]) -> anyhow::Result<Option<usize>> {
    let mut index = 0usize;
    while index < args.len() {
        let arg = args[index].as_str();
        if !arg.starts_with("--") {
            return Ok(Some(index));
        }
        let step = match arg {
            "--exact" => 1,
            "--mode" | "--profile" | "--host-lib" | "--start-url" | "--download-dir"
            | "--upload-dir" | "--server-addr" | "--href-contains" | "--index" | "--scope" => 2,
            _ => bail!("unsupported aegis option `{arg}`"),
        };
        if index + step > args.len() {
            bail!("aegis option `{arg}` requires a value");
        }
        index += step;
    }
    Ok(None)
}

fn validate_aegis_page_args(args: &[String]) -> anyhow::Result<()> {
    let Some(subcommand) = args.first().map(String::as_str) else {
        bail!("aegis page requires a subcommand");
    };
    match subcommand {
        "actions" | "links" if args.len() == 1 => Ok(()),
        "actions" | "links" => bail!("aegis page {subcommand} accepts no extra arguments"),
        "text" | "markdown" => validate_optional_scope_args(&args[1..], subcommand),
        "find" => {
            if args.len() < 2 {
                bail!("aegis page find requires text");
            }
            Ok(())
        }
        "open-link" => validate_aegis_open_link_args(&args[1..]),
        _ => bail!("unsupported aegis page subcommand `{subcommand}`"),
    }
}

fn validate_optional_scope_args(args: &[String], subcommand: &str) -> anyhow::Result<()> {
    match args {
        [] => Ok(()),
        [flag, _scope] if flag == "--scope" => Ok(()),
        _ => bail!("aegis page {subcommand} only supports optional --scope"),
    }
}

fn validate_aegis_open_link_args(args: &[String]) -> anyhow::Result<()> {
    let mut index = 0usize;
    let mut text_parts = 0usize;
    while index < args.len() {
        match args[index].as_str() {
            "--exact" => index += 1,
            "--href-contains" | "--index" => {
                if index + 1 >= args.len() {
                    bail!(
                        "aegis page open-link option `{}` requires a value",
                        args[index]
                    );
                }
                index += 2;
            }
            arg if arg.starts_with("--") => {
                bail!("unsupported aegis page open-link option `{arg}`")
            }
            _ => {
                text_parts += 1;
                index += 1;
            }
        }
    }
    if text_parts == 0 {
        bail!("aegis page open-link requires link text");
    }
    Ok(())
}

fn normalize_action_value(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if !object.contains_key("kind") {
        if object
            .get("actions")
            .and_then(|value| value.as_array())
            .is_some()
        {
            object.insert("kind".to_string(), serde_json::json!("sequence"));
        } else if object
            .get("args")
            .and_then(|value| value.as_array())
            .is_some_and(|args| args_look_like_aegis(args.as_slice()))
        {
            object.insert("kind".to_string(), serde_json::json!("aegis"));
        }
    }
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

fn args_look_like_aegis(args: &[serde_json::Value]) -> bool {
    args.iter().any(|arg| arg.as_str() == Some("--mode"))
        || args
            .first()
            .and_then(|arg| arg.as_str())
            .is_some_and(|arg| matches!(arg, "search" | "navigate" | "page"))
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

pub fn browser_research_bootstrap_plan(transcript: &str) -> Option<PlannedTurn> {
    let query = browser_research_query(transcript);
    let words = transcript
        .split_whitespace()
        .map(normalize_command_token)
        .map(|word| word.to_ascii_lowercase())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let wants_browser = words
        .iter()
        .any(|word| matches!(word.as_str(), "safari" | "browser" | "web" | "google"));
    let wants_research = words
        .iter()
        .any(|word| matches!(word.as_str(), "research" | "search" | "browse" | "browsing"));
    let wants_aegis = words
        .iter()
        .any(|word| matches!(word.as_str(), "aegis" | "headless"));
    let aegis_mode = if words.iter().any(|word| word == "headful") {
        "headful"
    } else {
        "headless"
    };
    if wants_aegis {
        if let Some((url, phrase)) = first_url(transcript).zip(exact_page_find_query(transcript)) {
            return Some(turn(
                "Checking the exact phrase with Aegis.",
                Some(InputAction::Sequence {
                    actions: vec![
                        InputAction::Aegis {
                            args: vec![
                                "--mode".to_string(),
                                aegis_mode.to_string(),
                                "navigate".to_string(),
                                url,
                            ],
                            timeout_ms: 15_000,
                        },
                        InputAction::Aegis {
                            args: vec![
                                "--mode".to_string(),
                                aegis_mode.to_string(),
                                "page".to_string(),
                                "find".to_string(),
                                phrase,
                            ],
                            timeout_ms: 15_000,
                        },
                    ],
                    inter_action_delay_ms: 120,
                }),
            ));
        }
    }
    if wants_aegis && words.iter().any(|word| word == "navigate") {
        if let Some(url) = first_url(transcript) {
            return Some(turn(
                "Navigating with Aegis.",
                Some(InputAction::Aegis {
                    args: vec![
                        "--mode".to_string(),
                        aegis_mode.to_string(),
                        "navigate".to_string(),
                        url,
                    ],
                    timeout_ms: 15_000,
                }),
            ));
        }
    }
    if wants_aegis && wants_research {
        return Some(turn(
            "Searching with Aegis.",
            Some(InputAction::Aegis {
                args: vec![
                    "--mode".to_string(),
                    aegis_mode.to_string(),
                    "search".to_string(),
                    query,
                ],
                timeout_ms: 15_000,
            }),
        ));
    }
    if !wants_browser || !wants_research {
        return None;
    }

    Some(turn(
        "Opening Safari for research.",
        Some(InputAction::Sequence {
            actions: browser_research_bootstrap_actions(query),
            inter_action_delay_ms: 120,
        }),
    ))
}

fn first_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|token| token.trim_matches(|ch: char| matches!(ch, '"' | '\'' | ',' | '.')))
        .find(|token| token.starts_with("https://") || token.starts_with("http://"))
        .map(ToString::to_string)
}

fn exact_page_find_query(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let marker_index = lower
        .find("exact phrase")
        .or_else(|| lower.find("exact text"))?;
    quoted_text_after_marker(&text[marker_index..]).map(ToString::to_string)
}

fn quoted_text_after_marker(text: &str) -> Option<&str> {
    let mut chars = text.char_indices();
    while let Some((start, ch)) = chars.next() {
        if !matches!(ch, '\'' | '"') {
            continue;
        }
        let content_start = start + ch.len_utf8();
        for (end, next) in chars.by_ref() {
            if next == ch {
                let candidate = text[content_start..end].trim();
                return (!candidate.is_empty()).then_some(candidate);
            }
        }
        return None;
    }
    None
}

fn browser_research_bootstrap_actions(query: String) -> Vec<InputAction> {
    vec![
        InputAction::OpenApp {
            app_name: "Safari".to_string(),
        },
        InputAction::KeyPress {
            combo: "cmd+l".to_string(),
        },
        InputAction::KeyPaste { text: query },
        InputAction::KeyPress {
            combo: "enter".to_string(),
        },
    ]
}

fn browser_research_query(transcript: &str) -> String {
    let cleaned = transcript
        .trim()
        .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == '.')
        .to_string();
    let lower = cleaned.to_ascii_lowercase();
    for marker in [
        "search the web for ",
        "search for ",
        "google ",
        "look up ",
        "lookup ",
        "research ",
        "browse for ",
    ] {
        if let Some(index) = lower.find(marker) {
            let start = index + marker.len();
            let candidate = cleaned[start..]
                .trim()
                .trim_matches(|ch: char| ch == '"' || ch == '\'' || ch == '.');
            if !candidate.is_empty() && !candidate.to_ascii_lowercase().starts_with("while ") {
                return trim_browser_research_tail(candidate).to_string();
            }
        }
    }
    trim_browser_research_tail(&cleaned).to_string()
}

fn trim_browser_research_tail(query: &str) -> &str {
    let lower = query.to_ascii_lowercase();
    let mut end = query.len();
    for marker in [
        ", inspect ",
        " and inspect ",
        ", open ",
        " and open ",
        ", read ",
        " and read ",
        ", then ",
        " and report ",
    ] {
        if let Some(index) = lower.find(marker) {
            end = end.min(index);
        }
    }
    query[..end].trim()
}

pub fn extract_planner_hints(transcript: &str) -> PlannerHints {
    let lower = transcript.trim().to_ascii_lowercase();
    let words = lower
        .split_whitespace()
        .map(normalize_command_token)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    let mut detected_actions = Vec::new();
    let asks_for_long_range_work = transcript_requests_long_range_work_words(&words);
    if !asks_for_long_range_work
        && words
            .iter()
            .any(|word| matches!(word.as_str(), "open" | "launch"))
    {
        for app_name in detected_app_names(&words) {
            detected_actions.push(InputAction::OpenApp {
                app_name: app_name.to_string(),
            });
        }
    }
    let mut notes = Vec::new();
    if words.iter().any(|word| {
        matches!(
            word.as_str(),
            "write" | "type" | "paste" | "leave" | "message" | "note" | "says" | "content"
        )
    }) {
        notes.push(
            "If the user wants text left inside an app, prefer key_paste after focusing or creating the target field; key_paste is more reliable for exact content than key_type."
                .to_string(),
        );
    }
    if detected_actions.len() > 1 {
        notes.push(
            "Multiple app opens were detected; a sequence can batch these opens when that matches the user's intent."
                .to_string(),
        );
    }
    if asks_for_long_range_work {
        notes.push(
            "Opening an app or browser is only setup for this request; continue with browser, shell, UI, reading, search, or verification actions until the actual long-range goal is complete."
                .to_string(),
        );
    }
    PlannerHints {
        detected_actions,
        notes,
    }
}

fn transcript_requests_long_range_work_words(words: &[String]) -> bool {
    words.iter().any(|word| {
        matches!(
            word.as_str(),
            "research"
                | "search"
                | "browse"
                | "browsing"
                | "web"
                | "google"
                | "lookup"
                | "look"
                | "find"
                | "read"
                | "investigate"
                | "compare"
                | "summarize"
                | "while"
                | "watching"
        )
    })
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

fn detected_app_names(words: &[String]) -> Vec<&'static str> {
    let mut apps = Vec::new();
    for (aliases, app_name) in [
        (&["messages", "imessage"][..], "Messages"),
        (&["safari"][..], "Safari"),
        (&["calculator"][..], "Calculator"),
        (&["terminal"][..], "Terminal"),
        (&["notes"][..], "Notes"),
        (&["mail"][..], "Mail"),
        (&["twitter", "x"][..], "Twitter"),
    ] {
        if words
            .iter()
            .any(|word| aliases.iter().any(|alias| word == alias))
            && !apps.contains(&app_name)
        {
            apps.push(app_name);
        }
    }
    apps
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
        assert!(PLANNER_SYSTEM_PROMPT.contains("normal browser/search work"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("visible browser UI actions are valid"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("page text --scope main"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("page open-link"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("page links"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("--href-contains"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("Do not emit incomplete options"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("runtime-only"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("--server-addr"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("unsupported commands"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("ctx"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("scratchpad"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("Native Skill.md support"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("read or inspect a local file"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("complete tool protocol"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("Prefer sequence"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("one valid JSON object"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("may contain a sequence action with many actions"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("runtime-governed RLM loop"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("default loop budget is unbounded"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("do not invent a five-turn"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("continue until the goal is complete"));
        assert!(PLANNER_SYSTEM_PROMPT
            .contains("do not collapse a multi-step goal into a polite one-shot reply"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("open_app"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("Prefer key_paste, not key_type"));
        assert!(PLANNER_SYSTEM_PROMPT
            .contains("Returning only open_app for a text-writing command is invalid"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("take or use a fresh screenshot/reobserve pass"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("Do not echo internal effect labels"));
        assert!(PLANNER_SYSTEM_PROMPT.contains("Do not invent a separate skill runtime"));
    }

    #[test]
    fn planner_provider_defaults_to_direct_gemini() {
        let provider = PlannerProvider::from_model("gemini-3.7-flash");

        assert_eq!(provider, PlannerProvider::Gemini);
        assert_eq!(
            provider.request_model("gemini-3.7-flash"),
            "gemini-3.7-flash"
        );
        assert_eq!(
            provider.chat_completions_url(),
            GEMINI_OPENAI_CHAT_COMPLETIONS_URL
        );
        assert_eq!(
            provider.required_api_key_name(),
            "GEMINI_API_KEY or GOOGLE_API_KEY"
        );
        assert!(!provider.should_pin_temperature("gemini-3.7-flash"));
        let headers = provider.headers("test-gemini-key").unwrap();
        assert_eq!(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-gemini-key")
        );
        assert!(headers.get(REFERER).is_none());
    }

    #[test]
    fn planner_provider_supports_explicit_openrouter_models() {
        let provider = PlannerProvider::from_model("openrouter/google/gemini-3.7-flash");

        assert_eq!(provider, PlannerProvider::OpenRouter);
        assert_eq!(
            provider.request_model("openrouter/google/gemini-3.7-flash"),
            "google/gemini-3.7-flash"
        );
        assert_eq!(
            provider.chat_completions_url(),
            OPENROUTER_CHAT_COMPLETIONS_URL
        );
        assert_eq!(provider.required_api_key_name(), "OPENROUTER_API_KEY");
        let headers = provider.headers("test-router-key").unwrap();
        assert_eq!(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-router-key")
        );
        assert_eq!(
            headers.get(REFERER).and_then(|value| value.to_str().ok()),
            Some("http://localhost/cua")
        );
    }

    #[test]
    fn planner_provider_allows_explicit_chat_completions_endpoint_override() {
        let previous = std::env::var(PLANNER_CHAT_COMPLETIONS_URL_ENV).ok();

        std::env::remove_var(PLANNER_CHAT_COMPLETIONS_URL_ENV);
        assert_eq!(
            PlannerProvider::Gemini.chat_completions_url(),
            GEMINI_OPENAI_CHAT_COMPLETIONS_URL
        );

        std::env::set_var(PLANNER_CHAT_COMPLETIONS_URL_ENV, "   ");
        assert_eq!(
            PlannerProvider::OpenRouter.chat_completions_url(),
            OPENROUTER_CHAT_COMPLETIONS_URL
        );

        std::env::set_var(
            PLANNER_CHAT_COMPLETIONS_URL_ENV,
            "http://127.0.0.1:18080/v1/chat/completions",
        );
        assert_eq!(
            PlannerProvider::Gemini.chat_completions_url(),
            "http://127.0.0.1:18080/v1/chat/completions"
        );
        assert_eq!(
            PlannerProvider::OpenRouter.chat_completions_url(),
            "http://127.0.0.1:18080/v1/chat/completions"
        );

        if let Some(previous) = previous {
            std::env::set_var(PLANNER_CHAT_COMPLETIONS_URL_ENV, previous);
        } else {
            std::env::remove_var(PLANNER_CHAT_COMPLETIONS_URL_ENV);
        }
    }

    #[test]
    fn planner_provider_routes_provider_slugs_through_openrouter() {
        let provider = PlannerProvider::from_model("openai/gpt-5.4-mini");

        assert_eq!(provider, PlannerProvider::OpenRouter);
        assert_eq!(
            provider.request_model("openai/gpt-5.4-mini"),
            "openai/gpt-5.4-mini"
        );
    }

    #[test]
    fn planner_request_body_uses_provider_model_and_temperature_contract() {
        let content = vec![serde_json::json!({"type": "text", "text": "Transcript: test"})];
        let gemini =
            Planner::new("gemini-3.7-flash").request_body("open calculator", content.clone());
        let openrouter =
            Planner::new("openrouter/openai/gpt-5.4-mini").request_body("open calculator", content);

        assert_eq!(gemini["model"], "gemini-3.7-flash");
        assert!(gemini.get("temperature").is_none());
        assert_eq!(gemini["response_format"]["type"], "json_object");
        assert_eq!(openrouter["model"], "openai/gpt-5.4-mini");
        assert_eq!(openrouter["temperature"], 0);
        assert_eq!(openrouter["response_format"]["type"], "json_object");
    }

    #[test]
    fn model_visible_prior_attempts_strip_runtime_only_aegis_wiring() {
        let attempts = vec![PlanAttemptContext {
            attempt_index: 1,
            response: "Checking links.".to_string(),
            action: Some(serde_json::json!({
                "kind": "sequence",
                "actions": [
                    {
                        "kind": "aegis",
                        "args": [
                            "--server-addr",
                            "127.0.0.1:19969",
                            "--profile",
                            "cua-test-profile",
                            "--mode",
                            "headless",
                            "page",
                            "links"
                        ],
                        "timeout_ms": 15000
                    },
                    {
                        "kind": "shell_exec",
                        "command": "pwd",
                        "timeout_ms": 5000
                    }
                ],
                "inter_action_delay_ms": 120
            })),
            effect: Some("confirmed".to_string()),
            evidence: Some(serde_json::json!({
                "effect": "confirmed",
                "evidence": [{"kind": "value_readback", "message": "aegis exited 0; stdout=[]; stderr="}]
            })),
        }];

        let visible = model_visible_prior_attempts(&attempts);

        let action = visible[0].action.as_ref().unwrap();
        let args = action["actions"][0]["args"].as_array().unwrap();
        assert_eq!(
            args,
            &[
                serde_json::json!("--mode"),
                serde_json::json!("headless"),
                serde_json::json!("page"),
                serde_json::json!("links"),
            ]
        );
        assert_eq!(action["actions"][1]["kind"], "shell_exec");
        assert_eq!(visible[0].effect.as_deref(), Some("accepted"));
        assert_eq!(visible[0].evidence.as_ref().unwrap()["effect"], "accepted");
        assert_eq!(
            visible[0].evidence.as_ref().unwrap()["evidence"][0]["message"],
            attempts[0].evidence.as_ref().unwrap()["evidence"][0]["message"],
            "readback evidence stays truthful even when model-visible status wording is softened"
        );
    }

    #[test]
    fn model_visible_prior_attempts_bound_repair_context_growth() {
        let attempts = (0..10)
            .map(|index| PlanAttemptContext {
                attempt_index: index + 1,
                response: format!("attempt {index}"),
                action: Some(serde_json::json!({
                    "kind": "aegis",
                    "args": ["--mode", "headless", "page", "find", "Foreign Key"],
                    "timeout_ms": 15000
                })),
                effect: Some("confirmed".to_string()),
                evidence: Some(serde_json::json!({
                    "effect": "confirmed",
                    "evidence": [
                        {"kind": "value_readback", "message": "old"},
                        {"kind": "value_readback", "message": "older"},
                        {"kind": "value_readback", "message": "oldest"},
                        {"kind": "value_readback", "message": "still old"},
                        {"kind": "value_readback", "message": "x".repeat(MODEL_VISIBLE_TEXT_MAX_CHARS + 32)}
                    ]
                })),
            })
            .collect::<Vec<_>>();

        let visible = model_visible_prior_attempts(&attempts);

        assert_eq!(visible.len(), MODEL_VISIBLE_PRIOR_ATTEMPTS_MAX);
        assert_eq!(visible[0].attempt_index, 3);
        assert!(visible
            .iter()
            .all(|attempt| attempt.effect.as_deref() == Some("accepted")));
        let evidence_items = visible[0].evidence.as_ref().unwrap()["evidence"]
            .as_array()
            .unwrap();
        assert_eq!(evidence_items.len(), MODEL_VISIBLE_EVIDENCE_ITEMS_MAX);
        let message = evidence_items
            .last()
            .unwrap()
            .get("message")
            .and_then(|message| message.as_str())
            .unwrap();
        assert_eq!(message.chars().count(), MODEL_VISIBLE_TEXT_MAX_CHARS);
        assert!(message.ends_with("..."));
    }

    #[test]
    fn model_visible_prior_attempts_preserve_verification_observation() {
        let attempts = vec![PlanAttemptContext {
            attempt_index: 1,
            response: "Writing the note.".to_string(),
            action: Some(serde_json::json!({"kind": "key_paste", "text": "hello"})),
            effect: Some("unverifiable".to_string()),
            evidence: Some(serde_json::json!({
                "effect": "unverifiable",
                "verification_observation": {
                    "has_frame": true,
                    "has_desktop": true,
                    "errors": []
                },
                "evidence": [
                    {"kind": "model_observation", "message": "visible input dispatched"}
                ]
            })),
        }];

        let visible = model_visible_prior_attempts(&attempts);

        let evidence = visible[0].evidence.as_ref().unwrap();
        assert_eq!(
            evidence["verification_observation"],
            serde_json::json!({
                "has_frame": true,
                "has_desktop": true,
                "errors": []
            })
        );
        assert_eq!(evidence["effect"], "unverifiable");
    }

    #[test]
    fn model_visible_prior_attempts_soften_nested_confirmed_effects() {
        let attempts = vec![PlanAttemptContext {
            attempt_index: 1,
            response: "Reading back the result.".to_string(),
            action: Some(serde_json::json!({
                "kind": "shell_exec",
                "command": "printf done",
                "timeout_ms": 5000
            })),
            effect: Some("confirmed".to_string()),
            evidence: Some(serde_json::json!({
                "effect": "confirmed",
                "final_evidence": {"effect": "confirmed"},
                "attempts": [
                    {"effect": "confirmed", "evidence": {"effect": "confirmed"}},
                    {"effect": "partial"}
                ],
                "evidence": [
                    {"kind": "value_readback", "message": "shell exited 0; stdout=done; stderr="}
                ]
            })),
        }];

        let visible = model_visible_prior_attempts(&attempts);
        let evidence_text = serde_json::to_string(&visible).unwrap();

        assert_eq!(visible[0].effect.as_deref(), Some("accepted"));
        assert!(!evidence_text.contains(r#""effect":"confirmed""#));
        assert!(evidence_text.contains("stdout=done"));
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
    fn parses_model_aegis_action_with_missing_kind_from_args() {
        let plan = parse_model_plan(
            r#"{"response":"Navigating.","action":{"args":["--mode","headless","navigate","https://www.iana.org/help"]}}"#,
        )
        .unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Aegis {
                ref args,
                timeout_ms: 15_000
            }) if args == &[
                "--mode".to_string(),
                "headless".to_string(),
                "navigate".to_string(),
                "https://www.iana.org/help".to_string()
            ]
        ));
    }

    #[test]
    fn parses_model_sequence_with_missing_kind_from_actions() {
        let plan = parse_model_plan(
            r#"{"response":"Inspecting.","action":{"actions":[{"args":["--mode","headless","page","links"]},{"kind":"aegis","args":["--mode","headless","page","actions"]}]}}"#,
        )
        .unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Sequence {
                ref actions,
                inter_action_delay_ms: 120
            }) if matches!(
                actions.as_slice(),
                [
                    InputAction::Aegis { args: first, timeout_ms: 15_000 },
                    InputAction::Aegis { args: second, timeout_ms: 15_000 },
                ] if first == &[
                    "--mode".to_string(),
                    "headless".to_string(),
                    "page".to_string(),
                    "links".to_string()
                ] && second == &[
                    "--mode".to_string(),
                    "headless".to_string(),
                    "page".to_string(),
                    "actions".to_string()
                ]
            )
        ));
    }

    #[test]
    fn rejects_empty_aegis_page_subcommand_before_dispatch() {
        let error = parse_model_plan(
            r#"{"response":"Inspecting.","action":{"kind":"sequence","actions":[{"kind":"aegis","args":["--mode","headless","page",""]}]}}"#,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("aegis action contains an empty argument"));
    }

    #[test]
    fn validates_aegis_open_link_refinement_options() {
        let plan = parse_model_plan(
            r#"{"response":"Opening result.","action":{"kind":"aegis","args":["--mode","headless","page","open-link","--href-contains","iana.org","--exact","Learn more"]}}"#,
        )
        .unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Aegis { ref args, .. })
                if args == &[
                    "--mode".to_string(),
                    "headless".to_string(),
                    "page".to_string(),
                    "open-link".to_string(),
                    "--href-contains".to_string(),
                    "iana.org".to_string(),
                    "--exact".to_string(),
                    "Learn more".to_string(),
                ]
        ));
    }

    #[test]
    fn rejects_aegis_open_link_without_link_text() {
        let error = parse_model_plan(
            r#"{"response":"Opening result.","action":{"kind":"aegis","args":["--mode","headless","page","open-link","--href-contains","iana.org"]}}"#,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("aegis page open-link requires link text"));
    }

    #[test]
    fn rejects_partial_aegis_navigation_url_before_dispatch() {
        let error = parse_model_plan(
            r#"{"response":"Navigating.","action":{"kind":"aegis","args":["--mode","headless","navigate","https://"]}}"#,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("parse aegis navigate URL"));
    }

    #[test]
    fn rejects_non_http_aegis_navigation_target_before_dispatch() {
        let error = parse_model_plan(
            r#"{"response":"Navigating.","action":{"kind":"aegis","args":["--mode","headless","navigate","file:///tmp/page.html"]}}"#,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("aegis navigate requires an absolute http(s) URL"));
    }

    #[test]
    fn rejects_aegis_navigation_with_trailing_junk_before_dispatch() {
        let error = parse_model_plan(
            r#"{"response":"Navigating.","action":{"kind":"aegis","args":["--mode","headless","navigate","https://example.com","then","inspect"]}}"#,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("aegis navigate accepts exactly one URL"));
    }

    #[test]
    fn rejects_aegis_links_with_trailing_junk_before_dispatch() {
        let error = parse_model_plan(
            r#"{"response":"Inspecting.","action":{"kind":"aegis","args":["--mode","headless","page","links","--url","https://example.com"]}}"#,
        )
        .unwrap_err();

        assert!(format!("{error:#}").contains("aegis page links accepts no extra arguments"));
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
    fn compound_requests_expose_advisory_planner_hints() {
        let hints = extract_planner_hints(
            "Open Notes and open Messages and leave me a note that says cua works",
        );

        assert!(matches!(
            hints.detected_actions.as_slice(),
            [
                InputAction::OpenApp { app_name: notes },
                InputAction::OpenApp { app_name: messages },
            ] if notes == "Messages" || messages == "Messages"
        ));
        assert!(hints.detected_actions.iter().any(
            |action| matches!(action, InputAction::OpenApp { app_name } if app_name == "Notes")
        ));
        assert!(hints.detected_actions.iter().any(
            |action| matches!(action, InputAction::OpenApp { app_name } if app_name == "Messages")
        ));
        assert!(hints
            .notes
            .iter()
            .any(|note| note.contains("prefer key_paste")));
    }

    #[test]
    fn simple_fast_open_still_has_no_hint_overhead_requirement() {
        let plan = parse_fast_command("Open Calculator").unwrap();
        assert!(matches!(
            plan.action,
            Some(InputAction::OpenApp { ref app_name }) if app_name == "Calculator"
        ));
        let hints = extract_planner_hints("Open Calculator");
        assert_eq!(hints.detected_actions.len(), 1);
    }

    #[test]
    fn research_requests_do_not_emit_open_only_hints() {
        let hints =
            extract_planner_hints("Open Safari browser and do some research while I am watching");

        assert!(hints.detected_actions.is_empty());
        assert!(hints.notes.iter().any(|note| note.contains("only setup")));
    }

    #[test]
    fn browser_research_bootstrap_handles_visible_browsing_command() {
        let plan = browser_research_bootstrap_plan(
            "Open Safari browser and do some research while I am watching",
        )
        .unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Sequence { ref actions, .. }) if matches!(
                actions.as_slice(),
                [
                    InputAction::OpenApp { app_name },
                    InputAction::KeyPress { combo },
                    InputAction::KeyPaste { text },
                    InputAction::KeyPress { combo: enter },
                ] if app_name == "Safari"
                    && combo == "cmd+l"
                    && text == "Open Safari browser and do some research while I am watching"
                    && enter == "enter"
            )
        ));
    }

    #[test]
    fn generic_web_search_bootstrap_uses_visible_browser_not_aegis() {
        let plan = browser_research_bootstrap_plan(
            "Search the web for official SQLite foreign key documentation and report the verified page title.",
        )
        .unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Sequence { ref actions, .. }) if matches!(
                actions.as_slice(),
                [
                    InputAction::OpenApp { app_name },
                    InputAction::KeyPress { combo },
                    InputAction::KeyPaste { text },
                    InputAction::KeyPress { combo: enter },
                ] if app_name == "Safari"
                    && combo == "cmd+l"
                    && text == "official SQLite foreign key documentation"
                    && enter == "enter"
            )
        ));
    }

    #[test]
    fn browser_research_bootstrap_uses_headless_aegis_when_requested() {
        let plan = browser_research_bootstrap_plan(
            "Use Aegis in headless mode to search the web for OpenAI Codex CLI GitHub, inspect the current page text or actions, and report one concrete result title.",
        )
        .unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Aegis { ref args, timeout_ms })
                if args == &[
                    "--mode".to_string(),
                    "headless".to_string(),
                    "search".to_string(),
                    "OpenAI Codex CLI GitHub".to_string(),
                ] && timeout_ms == 15_000
        ));
    }

    #[test]
    fn browser_research_bootstrap_trims_open_followup_from_aegis_query() {
        let plan = browser_research_bootstrap_plan(
            "Use Aegis in headless mode to search the web for Example Domain IANA, open the most relevant result if needed, inspect the page actions or text, and report the verified page title and one link label.",
        )
        .unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Aegis { ref args, timeout_ms })
                if args == &[
                    "--mode".to_string(),
                    "headless".to_string(),
                    "search".to_string(),
                    "Example Domain IANA".to_string(),
                ] && timeout_ms == 15_000
        ));
    }

    #[test]
    fn browser_research_bootstrap_preserves_explicit_headful_aegis() {
        let plan = browser_research_bootstrap_plan("Use Aegis headful to search for cloud agents")
            .unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Aegis { ref args, .. })
                if args == &[
                    "--mode".to_string(),
                    "headful".to_string(),
                    "search".to_string(),
                    "cloud agents".to_string(),
                ]
        ));
    }

    #[test]
    fn browser_research_bootstrap_uses_aegis_navigation_for_explicit_url() {
        let plan = browser_research_bootstrap_plan(
            "Use Aegis in headless mode to navigate to https://example.com, read the main page text, and report the verified page heading.",
        )
        .unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Aegis { ref args, timeout_ms })
                if args == &[
                    "--mode".to_string(),
                    "headless".to_string(),
                    "navigate".to_string(),
                    "https://example.com".to_string(),
                ] && timeout_ms == 15_000
        ));
    }

    #[test]
    fn browser_research_bootstrap_batches_aegis_url_exact_phrase_find() {
        let plan = browser_research_bootstrap_plan(
            "Inbound cua message received via unix_socket.\nMessage: Using Aegis headless only, open https://example.com and inspect the page. Find the exact phrase 'cua impossible phrase 1788025372'. If it is not present, report that it was not found and include the verified page title.",
        )
        .unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Sequence {
                ref actions,
                inter_action_delay_ms
            }) if inter_action_delay_ms == 120
                && matches!(
                    actions.as_slice(),
                    [
                        InputAction::Aegis {
                            args: navigate_args,
                            timeout_ms: navigate_timeout
                        },
                        InputAction::Aegis {
                            args: find_args,
                            timeout_ms: find_timeout
                        },
                    ] if navigate_args == &[
                        "--mode".to_string(),
                        "headless".to_string(),
                        "navigate".to_string(),
                        "https://example.com".to_string(),
                    ]
                    && *navigate_timeout == 15_000
                    && find_args == &[
                        "--mode".to_string(),
                        "headless".to_string(),
                        "page".to_string(),
                        "find".to_string(),
                        "cua impossible phrase 1788025372".to_string(),
                    ]
                    && *find_timeout == 15_000
                )
        ));
    }

    #[test]
    fn browser_research_bootstrap_extracts_search_query_before_followup_instructions() {
        let plan = browser_research_bootstrap_plan(
            "Open Safari and search for the official Gemini 3.7 Flash documentation, read the page title, and report the verified title.",
        )
        .unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Sequence { ref actions, .. }) if matches!(
                actions.as_slice(),
                [
                    InputAction::OpenApp { app_name },
                    InputAction::KeyPress { combo },
                    InputAction::KeyPaste { text },
                    InputAction::KeyPress { combo: enter },
                ] if app_name == "Safari"
                    && combo == "cmd+l"
                    && text == "the official Gemini 3.7 Flash documentation"
                    && enter == "enter"
            )
        ));
    }

    #[test]
    fn browser_research_bootstrap_trims_open_followup_from_visible_browser_query() {
        let plan = browser_research_bootstrap_plan(
            "Open Safari and search for Example Domain IANA, open the most relevant result if needed, inspect the page actions or text, and report the verified title.",
        )
        .unwrap();

        assert!(matches!(
            plan.action,
            Some(InputAction::Sequence { ref actions, .. }) if matches!(
                actions.as_slice(),
                [
                    InputAction::OpenApp { app_name },
                    InputAction::KeyPress { combo },
                    InputAction::KeyPaste { text },
                    InputAction::KeyPress { combo: enter },
                ] if app_name == "Safari"
                    && combo == "cmd+l"
                    && text == "Example Domain IANA"
                    && enter == "enter"
            )
        ));
    }

    #[test]
    fn browser_research_bootstrap_ignores_simple_open_app() {
        assert!(browser_research_bootstrap_plan("Open Safari").is_none());
        assert!(browser_research_bootstrap_plan("Research local restaurants").is_none());
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
    fn planner_uses_larger_output_budget_for_text_payloads() {
        std::env::remove_var("CUA_VOICE_PLANNER_MAX_TOKENS");
        std::env::remove_var("CUA_VOICE_PLANNER_TEXT_MAX_TOKENS");

        assert_eq!(planner_max_tokens("Open Notes"), DEFAULT_PLANNER_MAX_TOKENS);
        assert_eq!(
            planner_max_tokens("Open Notes and write me a short story"),
            DEFAULT_PLANNER_TEXT_MAX_TOKENS
        );
        assert_eq!(
            planner_max_tokens(
                "Use the local shell to create input.txt, transform it into output.txt, and read output.txt back"
            ),
            DEFAULT_PLANNER_TEXT_MAX_TOKENS
        );

        std::env::set_var("CUA_VOICE_PLANNER_TEXT_MAX_TOKENS", "1200");
        assert_eq!(planner_max_tokens("leave a note that says hello"), 1_200);
        std::env::remove_var("CUA_VOICE_PLANNER_TEXT_MAX_TOKENS");
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
