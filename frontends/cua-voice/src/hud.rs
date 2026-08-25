pub const COMPACT_WIDTH: f32 = 500.0;
pub const COMPACT_HEIGHT: f32 = 42.0;
pub const EXPANDED_WIDTH: f32 = 500.0;
pub const EXPANDED_HEIGHT: f32 = 176.0;
pub const WINDOW_WIDTH: f32 = COMPACT_WIDTH;
pub const WINDOW_HEIGHT: f32 = COMPACT_HEIGHT;
pub const PANEL_RADIUS: f32 = 11.0;
pub const TOP_MARGIN: f32 = 0.0;

use crate::ui_state::HudSnapshot;

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct HudMetrics {
    pub width: f32,
    pub height: f32,
    pub radius: f32,
    pub bar_opacity: f32,
    pub response_opacity: f32,
}

impl HudMetrics {
    pub fn interpolate(progress: f32) -> Self {
        let progress = progress.clamp(0.0, 1.0);
        let eased = ease_out_cubic(progress);
        Self {
            width: lerp(COMPACT_WIDTH, EXPANDED_WIDTH, eased),
            height: lerp(COMPACT_HEIGHT, EXPANDED_HEIGHT, eased),
            radius: lerp(21.0, PANEL_RADIUS, eased),
            bar_opacity: (1.0 - progress * 1.8).clamp(0.0, 1.0),
            response_opacity: ((progress - 0.24) / 0.76).clamp(0.0, 1.0),
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
    pub rows: [HudRow; 2],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HudRow {
    pub label: &'static str,
    pub tool: &'static str,
    pub app: &'static str,
    pub age: &'static str,
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
            title: snapshot.task.clone(),
            prompt,
            result,
            phase: snapshot.phase.label(),
            tool: short_tool(&snapshot.tool),
            rows: [
                HudRow {
                    label: "voice listener",
                    tool: "Rust",
                    app: "Mic",
                    age: "live",
                },
                HudRow {
                    label: "local action",
                    tool: "Socket",
                    app: "macOS",
                    age: "fast",
                },
            ],
        }
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
    } else if tool.contains("Microphone") {
        "Mic".to_string()
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
    fn tool_labels_stay_chip_sized() {
        assert_eq!(short_tool("Unix socket"), "Socket");
        assert_eq!(short_tool("OpenRouter Vision"), "Router");
        assert_eq!(short_tool("Microphone"), "Mic");
    }

    #[test]
    fn metrics_morph_between_compact_and_response_card() {
        let compact = HudMetrics::interpolate(0.0);
        let expanded = HudMetrics::interpolate(1.0);

        assert_eq!(compact.width, COMPACT_WIDTH);
        assert_eq!(compact.height, COMPACT_HEIGHT);
        assert_eq!(expanded.width, COMPACT_WIDTH);
        assert_eq!(expanded.height, EXPANDED_HEIGHT);
        assert!(compact.bar_opacity > expanded.bar_opacity);
        assert!(expanded.response_opacity > compact.response_opacity);
    }
}
