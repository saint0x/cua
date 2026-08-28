use crate::ui_state::{HudPhase, HudSnapshot};
use cua_core::{
    default_island_background, validate_island_scene, IslandItem, IslandLayout, IslandPalette,
    IslandRegion, IslandScene, IslandSceneError, IslandTheme, ISLAND_SCHEMA_VERSION,
};
use std::collections::BTreeMap;
use std::time::Duration;

pub const COMPACT_WIDTH: f32 = 636.0;
pub const COMPACT_HEIGHT: f32 = 42.0;
pub const COMPACT_RADIUS: f32 = 21.0;
pub const COMPACT_FILLET: f32 = 22.0;
pub const EXPANDED_WIDTH: f32 = 828.0;
pub const EXPANDED_HEIGHT: f32 = 258.0;
pub const EXPANDED_RADIUS: f32 = 26.0;
pub const EXPANDED_FILLET: f32 = 26.0;
pub const WINDOW_WIDTH: f32 = COMPACT_WIDTH + (COMPACT_FILLET * 2.0);
pub const WINDOW_HEIGHT: f32 = COMPACT_HEIGHT;
pub const TOP_MARGIN: f32 = 0.0;

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct HudMetrics {
    pub width: f32,
    pub height: f32,
    pub radius: f32,
    pub bar_opacity: f32,
    pub response_opacity: f32,
    pub expansion_opacity: f32,
}

impl HudMetrics {
    pub fn interpolate(progress: f32) -> Self {
        Self::with_expansion(progress, 0.0)
    }

    pub fn with_expansion(response_progress: f32, expansion_progress: f32) -> Self {
        let response_progress = response_progress.clamp(0.0, 1.0);
        let expansion_progress = ease_out_cubic(expansion_progress.clamp(0.0, 1.0));
        Self {
            width: lerp(COMPACT_WIDTH, EXPANDED_WIDTH, expansion_progress),
            height: lerp(COMPACT_HEIGHT, EXPANDED_HEIGHT, expansion_progress),
            radius: lerp(COMPACT_RADIUS, EXPANDED_RADIUS, expansion_progress),
            bar_opacity: 1.0,
            response_opacity: response_progress,
            expansion_opacity: expansion_progress,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HudDisplay {
    pub title: String,
    pub prompt: String,
    pub result: String,
    pub phase: &'static str,
    pub tool: String,
    pub target: String,
    pub rows: [HudRow; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HudRow {
    pub label: String,
    pub tool: String,
    pub app: String,
    pub age: String,
}

pub fn island_scene_from_snapshot(
    snapshot: &HudSnapshot,
    display: &HudDisplay,
    expanded: bool,
    reply_visible: bool,
    model_label: &str,
    elapsed: Duration,
) -> Result<IslandScene, IslandSceneError> {
    let scene = if expanded {
        expanded_scene(snapshot, display, reply_visible, model_label, elapsed)
    } else {
        compact_scene(snapshot, display, reply_visible)
    };
    validate_island_scene(&scene)?;
    Ok(scene)
}

impl HudDisplay {
    pub fn from_snapshot(snapshot: &HudSnapshot) -> Self {
        let prompt = snapshot
            .transcript
            .clone()
            .unwrap_or_else(|| snapshot.step.label.clone());
        let result = snapshot
            .response
            .clone()
            .or_else(|| snapshot.transcript.clone())
            .unwrap_or_else(|| snapshot.step.label.clone());

        Self {
            title: snapshot
                .response
                .as_ref()
                .or(snapshot.transcript.as_ref())
                .map(|text| compact_label(text, 34))
                .unwrap_or_else(|| snapshot.input_label.clone()),
            prompt,
            result,
            phase: snapshot.phase.label(),
            tool: live_transport_label(snapshot),
            target: live_target_label(snapshot),
            rows: action_rows(snapshot),
        }
    }
}

fn live_transport_label(snapshot: &HudSnapshot) -> String {
    match snapshot.phase {
        HudPhase::Listening | HudPhase::RecordingStopped | HudPhase::Accepted => "Mic".to_string(),
        HudPhase::Transcribing => short_tool(&snapshot.tool),
        HudPhase::Planning if snapshot.tool.contains("OpenRouter") => "Router".to_string(),
        HudPhase::Planning | HudPhase::Dispatching => short_tool(&snapshot.tool),
        HudPhase::Reply | HudPhase::Idle => short_tool(&snapshot.tool),
        _ => short_tool(&snapshot.tool),
    }
}

fn live_target_label(snapshot: &HudSnapshot) -> String {
    if snapshot.tool.contains("Screen") || snapshot.tool.contains("Capture") {
        return "Screen".to_string();
    }
    if snapshot.tool.contains("Microphone")
        || matches!(
            snapshot.phase,
            HudPhase::Listening | HudPhase::RecordingStopped | HudPhase::Accepted
        )
    {
        return "Microphone".to_string();
    }
    if snapshot.tool.contains("Safari") {
        return "Safari".to_string();
    }
    if snapshot.tool.contains("Terminal") {
        return "Terminal".to_string();
    }
    if snapshot.tool.contains("Finder") {
        return "Finder".to_string();
    }
    if snapshot.tool.contains("Unix")
        || snapshot.tool.contains("socket")
        || snapshot.tool.contains("Mouse")
        || snapshot.tool.contains("Keyboard")
    {
        return "macOS".to_string();
    }
    match snapshot.phase {
        HudPhase::RecordingStopped | HudPhase::Transcribing => "STT".to_string(),
        HudPhase::Planning => "Model".to_string(),
        HudPhase::Dispatching => "macOS".to_string(),
        HudPhase::Reply => "Result".to_string(),
        _ => "macOS".to_string(),
    }
}

fn action_rows(snapshot: &HudSnapshot) -> [HudRow; 2] {
    let transcript = snapshot
        .transcript
        .as_ref()
        .map(|transcript| compact_label(transcript, 36))
        .unwrap_or_else(|| "listening for voice".to_string());
    let action = snapshot
        .response
        .as_ref()
        .map(|response| compact_label(response, 36))
        .unwrap_or_else(|| compact_label(&snapshot.step.label, 36));
    [
        HudRow {
            label: transcript,
            tool: match snapshot.phase {
                HudPhase::Listening | HudPhase::RecordingStopped | HudPhase::Accepted => {
                    "Mic".to_string()
                }
                HudPhase::Transcribing => "STT".to_string(),
                _ => short_tool(&snapshot.tool),
            },
            app: snapshot.phase.label().to_string(),
            age: "now".to_string(),
        },
        HudRow {
            label: action,
            tool: short_tool(&snapshot.tool),
            app: if snapshot.tool.contains("Unix") || snapshot.tool.contains("socket") {
                "macOS".to_string()
            } else {
                live_target_label(snapshot)
            },
            age: if snapshot.response.is_some() {
                "done".to_string()
            } else {
                "live".to_string()
            },
        },
    ]
}

fn compact_scene(snapshot: &HudSnapshot, display: &HudDisplay, reply_visible: bool) -> IslandScene {
    let title = if reply_visible {
        "Reply".to_string()
    } else {
        display.title.clone()
    };
    let center = center_status_text(snapshot, display, reply_visible);
    let transport = if reply_visible {
        "cua".to_string()
    } else {
        display.tool.clone()
    };
    let target = if reply_visible {
        display.phase.to_string()
    } else {
        display.target.clone()
    };
    let mut regions = BTreeMap::new();
    regions.insert(
        "left".to_string(),
        IslandRegion {
            items: vec![
                IslandItem::Label {
                    id: "orb".to_string(),
                    text: "orb".to_string(),
                },
                IslandItem::Label {
                    id: "input".to_string(),
                    text: title,
                },
            ],
        },
    );
    regions.insert(
        "center".to_string(),
        IslandRegion {
            items: vec![IslandItem::Marquee {
                id: "status".to_string(),
                text: center,
            }],
        },
    );
    regions.insert(
        "right".to_string(),
        IslandRegion {
            items: vec![
                IslandItem::Chip {
                    id: "transport".to_string(),
                    text: transport,
                },
                IslandItem::Chip {
                    id: "target".to_string(),
                    text: target,
                },
                IslandItem::DotChase {
                    id: "activity".to_string(),
                    active: dots_are_active(snapshot),
                    palette: IslandPalette::BlueNeon,
                    count: 6,
                    speed: dot_chase_speed(snapshot),
                },
            ],
        },
    );
    IslandScene {
        schema_version: ISLAND_SCHEMA_VERSION.to_string(),
        layout: IslandLayout::Compact,
        mode: snapshot.mode.clone(),
        background: default_island_background(),
        regions,
        actors: Vec::new(),
        theme: Some(default_island_theme()),
    }
}

fn expanded_scene(
    snapshot: &HudSnapshot,
    display: &HudDisplay,
    reply_visible: bool,
    model_label: &str,
    elapsed: Duration,
) -> IslandScene {
    let step_counter = snapshot.step.counter();
    let mut regions = BTreeMap::new();
    regions.insert(
        "header_left".to_string(),
        IslandRegion {
            items: vec![
                IslandItem::Label {
                    id: "orb".to_string(),
                    text: "orb".to_string(),
                },
                IslandItem::Label {
                    id: "input".to_string(),
                    text: if reply_visible {
                        "Reply".to_string()
                    } else {
                        display.title.clone()
                    },
                },
            ],
        },
    );
    regions.insert(
        "header_center".to_string(),
        IslandRegion {
            items: vec![IslandItem::Marquee {
                id: "status".to_string(),
                text: center_status_text(snapshot, display, reply_visible),
            }],
        },
    );
    regions.insert(
        "header_right".to_string(),
        IslandRegion {
            items: vec![
                IslandItem::Chip {
                    id: "transport".to_string(),
                    text: if reply_visible {
                        "cua".to_string()
                    } else {
                        display.tool.clone()
                    },
                },
                IslandItem::Chip {
                    id: "target".to_string(),
                    text: if reply_visible {
                        display.phase.to_string()
                    } else {
                        display.target.clone()
                    },
                },
                IslandItem::DotChase {
                    id: "activity".to_string(),
                    active: dots_are_active(snapshot),
                    palette: IslandPalette::BlueNeon,
                    count: 6,
                    speed: dot_chase_speed(snapshot),
                },
            ],
        },
    );
    let mut task_items = vec![IslandItem::Row {
        id: "task".to_string(),
        index: "01".to_string(),
        label: "Task".to_string(),
        value: display.prompt.clone(),
        active: true,
    }];
    if let Some((index, total)) = step_counter {
        task_items.push(IslandItem::StepCounter {
            id: "step".to_string(),
            index: index as u16,
            total: total as u16,
        });
    }
    regions.insert("task".to_string(), IslandRegion { items: task_items });
    regions.insert(
        "response".to_string(),
        IslandRegion {
            items: vec![IslandItem::Row {
                id: "response".to_string(),
                index: "03".to_string(),
                label: "Response".to_string(),
                value: expanded_response_text(display),
                active: true,
            }],
        },
    );
    regions.insert(
        "details_left".to_string(),
        IslandRegion {
            items: vec![
                IslandItem::Row {
                    id: "action".to_string(),
                    index: "04".to_string(),
                    label: "Action".to_string(),
                    value: compact_label(&snapshot.step.label, 56),
                    active: true,
                },
                IslandItem::Row {
                    id: "phase".to_string(),
                    index: "05".to_string(),
                    label: "Phase".to_string(),
                    value: display.phase.to_string(),
                    active: false,
                },
                IslandItem::Row {
                    id: "state".to_string(),
                    index: "06".to_string(),
                    label: "State".to_string(),
                    value: if dots_are_active(snapshot) {
                        "Live".to_string()
                    } else {
                        "Idle".to_string()
                    },
                    active: dots_are_active(snapshot),
                },
            ],
        },
    );
    regions.insert(
        "details_right".to_string(),
        IslandRegion {
            items: display
                .rows
                .iter()
                .enumerate()
                .map(|(index, row)| IslandItem::ToolRow {
                    id: format!("tool_{index}"),
                    index: format!("{:02}", index + 7),
                    label: row.label.clone(),
                    tool: row.tool.clone(),
                    app: row.app.clone(),
                    age: row.age.clone(),
                })
                .collect(),
        },
    );
    regions.insert(
        "footer".to_string(),
        IslandRegion {
            items: vec![
                IslandItem::Label {
                    id: "elapsed".to_string(),
                    text: format!("Elapsed {}", elapsed_label(elapsed)),
                },
                IslandItem::Label {
                    id: "model".to_string(),
                    text: format!("Model {}", compact_label(model_label, 30)),
                },
                IslandItem::Label {
                    id: "transport".to_string(),
                    text: format!("Transport {}", display.tool),
                },
            ],
        },
    );
    IslandScene {
        schema_version: ISLAND_SCHEMA_VERSION.to_string(),
        layout: IslandLayout::Expanded,
        mode: snapshot.mode.clone(),
        background: default_island_background(),
        regions,
        actors: Vec::new(),
        theme: Some(default_island_theme()),
    }
}

pub fn center_status_text(
    snapshot: &HudSnapshot,
    display: &HudDisplay,
    reply_visible: bool,
) -> String {
    if reply_visible {
        return display.result.clone();
    }
    if snapshot.phase == HudPhase::Idle {
        return snapshot.step.label.clone();
    }
    if snapshot.phase == HudPhase::Listening {
        return "Listening".to_string();
    }
    if snapshot.phase == HudPhase::RecordingStopped {
        return "Recording Stopped".to_string();
    }
    if snapshot.phase == HudPhase::Transcribing {
        return "Processing".to_string();
    }
    if snapshot.phase == HudPhase::Accepted {
        return "Accepted".to_string();
    }
    if let Some((index, total)) = snapshot.step.counter() {
        return format!("Step {index}/{total}   {}", snapshot.step.label);
    }
    snapshot.step.label.clone()
}

pub fn dots_are_active(snapshot: &HudSnapshot) -> bool {
    !matches!(snapshot.phase, HudPhase::Idle)
}

pub fn dot_chase_speed(snapshot: &HudSnapshot) -> u8 {
    match snapshot.phase {
        HudPhase::Listening | HudPhase::RecordingStopped | HudPhase::Dispatching => 8,
        HudPhase::Accepted | HudPhase::Planning | HudPhase::Transcribing => 6,
        HudPhase::Reply => 5,
        HudPhase::Error => 10,
        HudPhase::Armed => 7,
        HudPhase::Idle => 0,
    }
}

fn expanded_response_text(display: &HudDisplay) -> String {
    let text = if display.result.trim().is_empty() {
        display.prompt.as_str()
    } else {
        display.result.as_str()
    };
    compact_label(text, 460)
}

fn elapsed_label(duration: Duration) -> String {
    let secs = duration.as_secs();
    let minutes = secs / 60;
    let seconds = secs % 60;
    format!("{minutes:02}:{seconds:02}")
}

fn default_island_theme() -> IslandTheme {
    IslandTheme {
        name: "default".to_string(),
        tokens: BTreeMap::from([
            ("background".to_string(), "#000000".to_string()),
            ("text".to_string(), "#e8e8ec".to_string()),
            ("muted".to_string(), "#8b8b95".to_string()),
            ("blue".to_string(), "#1e9bff".to_string()),
            ("chip_background".to_string(), "#1f1f22".to_string()),
            ("divider".to_string(), "#1b1b1f".to_string()),
        ]),
    }
}

pub fn compact_label(value: &str, max_chars: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let label = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{label}...")
    } else {
        label
    }
}

pub fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

pub fn lerp(start: f32, end: f32, progress: f32) -> f32 {
    start + (end - start) * progress
}

pub fn short_tool(tool: &str) -> String {
    if tool.contains("Unix") || tool.contains("socket") {
        "Socket".to_string()
    } else if tool.contains("HTTP") {
        "HTTP".to_string()
    } else if tool.contains("OpenRouter") {
        "Router".to_string()
    } else if tool.contains("Whisper") || tool.contains("STT") {
        "STT".to_string()
    } else if tool.contains("Microphone") {
        "Mic".to_string()
    } else if tool.contains("Screen") || tool.contains("Capture") {
        "Screen".to_string()
    } else {
        tool.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_state::VoiceUiEvent;

    #[test]
    fn display_prefers_transcript_for_prompt_and_reply_for_result() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::Transcript("Click 640 360".to_string()));
        snapshot.apply(VoiceUiEvent::Reply("Clicked.".to_string()));

        let display = HudDisplay::from_snapshot(&snapshot);

        assert_eq!(display.prompt, "Click 640 360");
        assert_eq!(display.result, "Clicked.");
        assert_eq!(display.phase, "Reply");
    }

    #[test]
    fn display_rows_follow_actual_voice_turn_state() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::Transcript("Move 10-20.".to_string()));
        snapshot.apply(VoiceUiEvent::Planning {
            tool: "Command parser".to_string(),
        });
        snapshot.apply(VoiceUiEvent::Dispatching(
            "MouseMove { x: 10, y: 20 }".to_string(),
        ));

        let display = HudDisplay::from_snapshot(&snapshot);

        assert_eq!(display.title, "Move 10-20.");
        assert_eq!(display.rows[0].label, "Move 10-20.");
        assert_eq!(display.rows[1].tool, "Socket");
        assert_eq!(display.rows[1].app, "macOS");
        assert_eq!(display.tool, "Socket");
        assert_eq!(display.target, "macOS");
    }

    #[test]
    fn display_chips_track_live_voice_and_screen_surfaces() {
        let mut listening = HudSnapshot::default();
        listening.apply(VoiceUiEvent::Listening { ms: 120 });
        let display = HudDisplay::from_snapshot(&listening);

        assert_eq!(display.tool, "Mic");
        assert_eq!(display.target, "Microphone");

        let mut screen = HudSnapshot::default();
        screen.apply(VoiceUiEvent::AgentStep {
            label: "capturing full screen".to_string(),
            source: Some("external agent".to_string()),
            task: Some("Observe".to_string()),
            tool: Some("Screen capture".to_string()),
            step_index: Some(1),
            step_total: Some(3),
            ttl_ms: None,
        });
        let display = HudDisplay::from_snapshot(&screen);

        assert_eq!(display.tool, "Screen");
        assert_eq!(display.target, "Screen");
    }

    #[test]
    fn display_chips_track_local_stt_without_router_label() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::Transcribing);

        let display = HudDisplay::from_snapshot(&snapshot);

        assert_eq!(display.tool, "STT");
        assert_eq!(display.target, "STT");
        assert_eq!(display.rows[0].tool, "STT");
        assert_eq!(display.rows[1].tool, "STT");
    }

    #[test]
    fn protocol_step_fields_propagate_to_hud_labels_and_chips() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::AgentStep {
            label: "Inspecting Safari address bar".to_string(),
            source: Some("external agent".to_string()),
            task: Some("Browse test".to_string()),
            tool: Some("Safari".to_string()),
            step_index: Some(7),
            step_total: Some(11),
            ttl_ms: None,
        });

        let display = HudDisplay::from_snapshot(&snapshot);

        assert_eq!(display.title, "Automation");
        assert_eq!(display.tool, "Safari");
        assert_eq!(display.target, "Safari");
        assert_eq!(display.rows[1].label, "Inspecting Safari address bar");
        assert_eq!(display.rows[1].tool, "Safari");
        assert_eq!(display.rows[1].app, "Safari");
        assert_eq!(snapshot.task, "Browse test");
        assert_eq!(snapshot.step.index, Some(7));
        assert_eq!(snapshot.step.total, Some(11));
    }

    #[test]
    fn display_title_uses_live_input_label_without_transcript() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::AgentStep {
            label: "checking browser".to_string(),
            source: Some("external agent".to_string()),
            task: Some("Browser task".to_string()),
            tool: Some("Unix socket".to_string()),
            step_index: Some(1),
            step_total: Some(4),
            ttl_ms: None,
        });

        let display = HudDisplay::from_snapshot(&snapshot);

        assert_eq!(display.title, "Automation");
    }

    #[test]
    fn compact_labels_are_bounded() {
        let label = compact_label("one two three four five six", 13);

        assert_eq!(label, "one two three...");
    }

    #[test]
    fn tool_labels_stay_chip_sized() {
        assert_eq!(short_tool("Unix socket"), "Socket");
        assert_eq!(short_tool("OpenRouter Vision"), "Router");
        assert_eq!(short_tool("Whisper STT"), "STT");
        assert_eq!(short_tool("Microphone"), "Mic");
        assert_eq!(short_tool("Screen capture"), "Screen");
    }

    #[test]
    fn metrics_keep_the_island_frame_during_reply_progress() {
        let compact = HudMetrics::interpolate(0.0);
        let reply = HudMetrics::interpolate(1.0);

        assert_eq!(compact.width, COMPACT_WIDTH);
        assert_eq!(compact.height, COMPACT_HEIGHT);
        assert_eq!(reply.width, COMPACT_WIDTH);
        assert_eq!(reply.height, COMPACT_HEIGHT);
        assert_eq!(reply.radius, COMPACT_RADIUS);
        assert_eq!(compact.bar_opacity, reply.bar_opacity);
        assert_eq!(compact.response_opacity, 0.0);
        assert_eq!(reply.response_opacity, 1.0);
        assert_eq!(compact.expansion_opacity, 0.0);
        assert_eq!(reply.expansion_opacity, 0.0);
    }

    #[test]
    fn metrics_expand_the_island_shell_separately_from_reply_flash() {
        let expanded = HudMetrics::with_expansion(0.0, 1.0);

        assert_eq!(expanded.width, EXPANDED_WIDTH);
        assert_eq!(expanded.height, EXPANDED_HEIGHT);
        assert_eq!(expanded.radius, EXPANDED_RADIUS);
        assert_eq!(expanded.response_opacity, 0.0);
        assert_eq!(expanded.expansion_opacity, 1.0);
    }

    #[test]
    fn compact_scene_reproduces_current_hud_baseline() {
        let snapshot = HudSnapshot::default();
        let display = HudDisplay::from_snapshot(&snapshot);

        let scene = island_scene_from_snapshot(
            &snapshot,
            &display,
            false,
            false,
            "anthropic/claude-sonnet-5",
            Duration::ZERO,
        )
        .unwrap();

        assert_eq!(scene.schema_version, cua_core::ISLAND_SCHEMA_VERSION);
        assert_eq!(scene.layout, cua_core::IslandLayout::Compact);
        let left = &scene.regions["left"].items;
        let center = &scene.regions["center"].items;
        let right = &scene.regions["right"].items;
        assert!(matches!(
            &left[1],
            cua_core::IslandItem::Label { id, text }
                if id == "input" && text == "Voice control"
        ));
        assert!(matches!(
            &center[0],
            cua_core::IslandItem::Marquee { id, text } if id == "status" && text == "Ready"
        ));
        assert!(matches!(
            &right[2],
            cua_core::IslandItem::DotChase {
                id,
                active: false,
                count: 6,
                speed: 0,
                ..
            } if id == "activity"
        ));
    }

    #[test]
    fn compact_scene_uses_reply_as_center_marquee() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::Reply(
            "I can see the current desktop and the app is ready.".to_string(),
        ));
        let display = HudDisplay::from_snapshot(&snapshot);

        let scene = island_scene_from_snapshot(
            &snapshot,
            &display,
            false,
            true,
            "anthropic/claude-sonnet-5",
            Duration::ZERO,
        )
        .unwrap();

        assert!(matches!(
            &scene.regions["center"].items[0],
            cua_core::IslandItem::Marquee { text, .. }
                if text == "I can see the current desktop and the app is ready."
        ));
        assert!(matches!(
            &scene.regions["right"].items[2],
            cua_core::IslandItem::DotChase {
                active: true,
                speed: 5,
                ..
            }
        ));
    }

    #[test]
    fn expanded_scene_carries_task_rows_and_step_counter() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::AgentStep {
            label: "Checking Safari results".to_string(),
            source: Some("automation".to_string()),
            task: Some("Research".to_string()),
            tool: Some("Safari".to_string()),
            step_index: Some(3),
            step_total: Some(12),
            ttl_ms: None,
        });
        let display = HudDisplay::from_snapshot(&snapshot);

        let scene = island_scene_from_snapshot(
            &snapshot,
            &display,
            true,
            false,
            "anthropic/claude-sonnet-5",
            Duration::from_secs(67),
        )
        .unwrap();

        assert_eq!(scene.layout, cua_core::IslandLayout::Expanded);
        assert!(matches!(
            &scene.regions["task"].items[1],
            cua_core::IslandItem::StepCounter {
                id,
                index: 3,
                total: 12
            } if id == "step"
        ));
        assert!(matches!(
            &scene.regions["details_left"].items[0],
            cua_core::IslandItem::Row { value, .. } if value == "Checking Safari results"
        ));
        assert!(matches!(
            &scene.regions["footer"].items[0],
            cua_core::IslandItem::Label { text, .. } if text == "Elapsed 01:07"
        ));
    }
}
