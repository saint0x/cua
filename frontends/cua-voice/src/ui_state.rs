use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HudPhase {
    Idle,
    Armed,
    Listening,
    Transcribing,
    Planning,
    Dispatching,
    Reply,
    Error,
}

impl HudPhase {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Idle => "Ready",
            Self::Armed => "Armed",
            Self::Listening => "Listening",
            Self::Transcribing => "Transcribing",
            Self::Planning => "Planning",
            Self::Dispatching => "Dispatching",
            Self::Reply => "Reply",
            Self::Error => "Needs attention",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HudStep {
    pub index: usize,
    pub total: usize,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HudSnapshot {
    pub phase: HudPhase,
    pub task: String,
    pub step: HudStep,
    pub tool: String,
    pub transcript: Option<String>,
    pub response: Option<String>,
    pub expanded_until: Option<Instant>,
}

impl Default for HudSnapshot {
    fn default() -> Self {
        Self {
            phase: HudPhase::Idle,
            task: "Voice control".to_string(),
            step: HudStep {
                index: 0,
                total: 4,
                label: "Ready".to_string(),
            },
            tool: "Unix socket".to_string(),
            transcript: None,
            response: None,
            expanded_until: None,
        }
    }
}

impl HudSnapshot {
    pub fn apply(&mut self, event: VoiceUiEvent) {
        match event {
            VoiceUiEvent::Armed => {
                self.phase = HudPhase::Armed;
                self.step = HudStep::new(1, 4, "Starting recorder");
                self.tool = "Keyboard".to_string();
                self.transcript = None;
                self.response = None;
                self.expanded_until = None;
            }
            VoiceUiEvent::Listening { ms } => {
                self.phase = HudPhase::Listening;
                self.step = HudStep::new(1, 4, format!("Recording {ms} ms"));
                self.tool = "Microphone".to_string();
            }
            VoiceUiEvent::Transcribing => {
                self.phase = HudPhase::Transcribing;
                self.step = HudStep::new(2, 4, "Speech to text");
                self.tool = "OpenRouter STT".to_string();
            }
            VoiceUiEvent::Transcript(text) => {
                self.transcript = Some(text);
            }
            VoiceUiEvent::Planning { tool } => {
                self.phase = HudPhase::Planning;
                self.step = HudStep::new(3, 4, "Choosing action");
                self.tool = tool;
            }
            VoiceUiEvent::Dispatching(action) => {
                self.phase = HudPhase::Dispatching;
                self.step = HudStep::new(4, 4, action);
                self.tool = "Unix socket".to_string();
            }
            VoiceUiEvent::Reply(text) => {
                self.phase = HudPhase::Reply;
                self.step = HudStep::new(4, 4, "Done");
                self.response = Some(text);
                self.expanded_until = Some(Instant::now() + Duration::from_secs(5));
            }
            VoiceUiEvent::Error(text) => {
                self.phase = HudPhase::Error;
                self.step = HudStep::new(0, 4, "Stopped");
                self.response = Some(text);
                self.expanded_until = Some(Instant::now() + Duration::from_secs(5));
            }
            VoiceUiEvent::Metric { .. } => {}
            VoiceUiEvent::Idle => {
                *self = Self::default();
            }
        }
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded_until
            .map(|deadline| Instant::now() < deadline)
            .unwrap_or(false)
    }
}

impl HudStep {
    pub fn new(index: usize, total: usize, label: impl Into<String>) -> Self {
        Self {
            index,
            total,
            label: label.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceUiEvent {
    Armed,
    Listening { ms: u64 },
    Transcribing,
    Transcript(String),
    Planning { tool: String },
    Dispatching(String),
    Reply(String),
    Error(String),
    Metric { name: String, ms: u64 },
    Idle,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_expands_for_a_short_window() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::Reply("ready".to_string()));
        assert!(state.is_expanded());
        assert_eq!(state.phase, HudPhase::Reply);
    }

    #[test]
    fn planning_event_sets_actual_tool_label() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::Planning {
            tool: "Command parser".to_string(),
        });

        assert_eq!(state.phase, HudPhase::Planning);
        assert_eq!(state.tool, "Command parser");
    }

    #[test]
    fn armed_starts_a_fresh_visible_turn() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::Transcript("old command".to_string()));
        state.apply(VoiceUiEvent::Reply("old reply".to_string()));

        state.apply(VoiceUiEvent::Armed);

        assert_eq!(state.phase, HudPhase::Armed);
        assert_eq!(state.transcript, None);
        assert_eq!(state.response, None);
        assert_eq!(state.expanded_until, None);
    }
}
