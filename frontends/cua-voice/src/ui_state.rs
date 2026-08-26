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
    pub programmed_step_expires_at: Option<Instant>,
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
            programmed_step_expires_at: None,
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
                self.programmed_step_expires_at = None;
            }
            VoiceUiEvent::Listening { ms } => {
                self.phase = HudPhase::Listening;
                self.step = HudStep::new(1, 4, format!("Recording {ms} ms"));
                self.tool = "Microphone".to_string();
                self.programmed_step_expires_at = None;
            }
            VoiceUiEvent::Transcribing => {
                self.phase = HudPhase::Transcribing;
                self.step = HudStep::new(2, 4, "Speech to text");
                self.tool = "OpenRouter STT".to_string();
                self.programmed_step_expires_at = None;
            }
            VoiceUiEvent::Transcript(text) => {
                self.transcript = Some(text);
            }
            VoiceUiEvent::Planning { tool } => {
                self.phase = HudPhase::Planning;
                self.step = HudStep::new(3, 4, "Choosing action");
                self.tool = tool;
                self.programmed_step_expires_at = None;
            }
            VoiceUiEvent::Dispatching(action) => {
                self.phase = HudPhase::Dispatching;
                self.step = HudStep::new(4, 4, action);
                self.tool = "Unix socket".to_string();
                self.programmed_step_expires_at = None;
            }
            VoiceUiEvent::AgentStep {
                label,
                source,
                task,
                tool,
                step_index,
                step_total,
                ttl_ms,
            } => {
                self.phase = HudPhase::Planning;
                self.step = HudStep::new(
                    step_index.unwrap_or(3).into(),
                    step_total.unwrap_or(4).into(),
                    label,
                );
                if let Some(task) = task {
                    self.task = task;
                }
                self.tool = tool.or(source).unwrap_or_else(|| "Agent".to_string());
                self.programmed_step_expires_at =
                    ttl_ms.map(|ttl_ms| Instant::now() + Duration::from_millis(ttl_ms));
            }
            VoiceUiEvent::Reply(text) => {
                self.phase = HudPhase::Reply;
                self.step = HudStep::new(4, 4, "Done");
                self.response = Some(text);
                self.expanded_until = Some(Instant::now() + Duration::from_secs(5));
                self.programmed_step_expires_at = None;
            }
            VoiceUiEvent::Error(text) => {
                self.phase = HudPhase::Error;
                self.step = HudStep::new(0, 4, "Stopped");
                self.response = Some(text);
                self.expanded_until = Some(Instant::now() + Duration::from_secs(5));
                self.programmed_step_expires_at = None;
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

    pub fn expire_programmed_step(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.programmed_step_expires_at else {
            return false;
        };
        if now < deadline {
            return false;
        }
        *self = Self::default();
        true
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
    Listening {
        ms: u64,
    },
    Transcribing,
    Transcript(String),
    Planning {
        tool: String,
    },
    Dispatching(String),
    AgentStep {
        label: String,
        source: Option<String>,
        task: Option<String>,
        tool: Option<String>,
        step_index: Option<u16>,
        step_total: Option<u16>,
        ttl_ms: Option<u64>,
    },
    Reply(String),
    Error(String),
    Metric {
        name: String,
        ms: u64,
    },
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

    #[test]
    fn agent_step_programs_visible_step_without_overriding_transcript() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::Transcript("find the red button".to_string()));

        state.apply(VoiceUiEvent::AgentStep {
            label: "checking target position".to_string(),
            source: Some("planner".to_string()),
            task: Some("Click target".to_string()),
            tool: Some("vision".to_string()),
            step_index: Some(2),
            step_total: Some(5),
            ttl_ms: Some(1_500),
        });

        assert_eq!(state.phase, HudPhase::Planning);
        assert_eq!(state.task, "Click target");
        assert_eq!(state.step.label, "checking target position");
        assert_eq!(state.step.index, 2);
        assert_eq!(state.step.total, 5);
        assert_eq!(state.tool, "vision");
        assert_eq!(state.transcript.as_deref(), Some("find the red button"));
        assert!(state.programmed_step_expires_at.is_some());
    }

    #[test]
    fn programmed_agent_step_expires_back_to_ready_state() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::AgentStep {
            label: "checking current focus".to_string(),
            source: Some("agent".to_string()),
            task: Some("Use browser".to_string()),
            tool: Some("browser".to_string()),
            step_index: Some(1),
            step_total: Some(3),
            ttl_ms: Some(250),
        });

        assert!(!state.expire_programmed_step(Instant::now()));
        assert!(state.expire_programmed_step(Instant::now() + Duration::from_millis(251)));
        assert_eq!(state, HudSnapshot::default());
    }

    #[test]
    fn persistent_agent_step_does_not_expire_without_ttl() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::AgentStep {
            label: "waiting on tool".to_string(),
            source: Some("agent".to_string()),
            task: None,
            tool: None,
            step_index: None,
            step_total: None,
            ttl_ms: None,
        });

        assert!(!state.expire_programmed_step(Instant::now() + Duration::from_secs(60)));
        assert_eq!(state.step.label, "waiting on tool");
    }
}
