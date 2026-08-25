pub const COMPACT_WIDTH: f32 = 815.0;
pub const COMPACT_HEIGHT: f32 = 42.0;
pub const COMPACT_RADIUS: f32 = 21.0;
pub const WINDOW_WIDTH: f32 = COMPACT_WIDTH;
pub const WINDOW_HEIGHT: f32 = COMPACT_HEIGHT;
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
        Self {
            width: COMPACT_WIDTH,
            height: COMPACT_HEIGHT,
            radius: COMPACT_RADIUS,
            bar_opacity: 1.0,
            response_opacity: progress,
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
    pub label: String,
    pub tool: String,
    pub app: String,
    pub age: String,
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
                .unwrap_or_else(|| snapshot.task.clone()),
            prompt,
            result,
            phase: snapshot.phase.label(),
            tool: short_tool(&snapshot.tool),
            rows: action_rows(snapshot),
        }
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
                crate::ui_state::HudPhase::Listening => "Mic".to_string(),
                crate::ui_state::HudPhase::Transcribing => "STT".to_string(),
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
                "CUA".to_string()
            },
            age: if snapshot.response.is_some() {
                "done".to_string()
            } else {
                "live".to_string()
            },
        },
    ]
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
        assert_eq!(short_tool("Microphone"), "Mic");
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
    }
}
