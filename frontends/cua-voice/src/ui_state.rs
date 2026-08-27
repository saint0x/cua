use cua_core::UiMode;
use std::time::{Duration, Instant};

const TRANSCRIPT_VISIBLE_FOR: Duration = Duration::from_millis(1_500);
const REPLY_VISIBLE_MIN: Duration = Duration::from_secs(8);
const REPLY_MARQUEE_VIEWPORT_PX: f32 = 270.0;
const REPLY_MARQUEE_CHAR_WIDTH_PX: f32 = 6.2;
const REPLY_MARQUEE_START_DELAY_SECS: f32 = 1.6;
const REPLY_MARQUEE_END_HOLD_SECS: f32 = 0.9;
const REPLY_MARQUEE_SCROLL_SPEED_PX_PER_SEC: f32 = 24.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HudPhase {
    Idle,
    Armed,
    Listening,
    RecordingStopped,
    Accepted,
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
            Self::RecordingStopped => "Recording Stopped",
            Self::Accepted => "Accepted",
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
    pub index: Option<usize>,
    pub total: Option<usize>,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HudSnapshot {
    pub phase: HudPhase,
    pub mode: UiMode,
    pub input_label: String,
    pub task: String,
    pub step: HudStep,
    pub tool: String,
    pub transcript: Option<String>,
    pub transcript_until: Option<Instant>,
    pub response: Option<String>,
    pub expanded_until: Option<Instant>,
    pub programmed_step_expires_at: Option<Instant>,
    programmed_step_restore: Option<Box<HudSnapshot>>,
}

impl Default for HudSnapshot {
    fn default() -> Self {
        Self {
            phase: HudPhase::Idle,
            mode: UiMode::Headful,
            input_label: "Voice control".to_string(),
            task: "Voice control".to_string(),
            step: HudStep {
                index: None,
                total: None,
                label: "Ready".to_string(),
            },
            tool: "Unix socket".to_string(),
            transcript: None,
            transcript_until: None,
            response: None,
            expanded_until: None,
            programmed_step_expires_at: None,
            programmed_step_restore: None,
        }
    }
}

impl HudSnapshot {
    pub fn apply(&mut self, event: VoiceUiEvent) {
        match event {
            VoiceUiEvent::Armed => {
                self.phase = HudPhase::Armed;
                self.mark_voice_control();
                self.step = HudStep::plain("Starting recorder");
                self.tool = "Keyboard".to_string();
                self.transcript = None;
                self.transcript_until = None;
                self.response = None;
                self.expanded_until = None;
                self.programmed_step_expires_at = None;
                self.programmed_step_restore = None;
            }
            VoiceUiEvent::Listening { .. } => {
                self.phase = HudPhase::Listening;
                self.mark_voice_control();
                self.step = HudStep::plain("Listening");
                self.tool = "Microphone".to_string();
                self.programmed_step_expires_at = None;
                self.programmed_step_restore = None;
            }
            VoiceUiEvent::RecordingStopped => {
                self.phase = HudPhase::RecordingStopped;
                self.mark_voice_control();
                self.step = HudStep::plain("Recording Stopped");
                self.tool = "Microphone".to_string();
                self.programmed_step_expires_at = None;
                self.programmed_step_restore = None;
            }
            VoiceUiEvent::Accepted => {
                self.phase = HudPhase::Accepted;
                self.mark_voice_control();
                self.step = HudStep::plain("Accepted");
                self.tool = "Microphone".to_string();
                self.programmed_step_expires_at = None;
                self.programmed_step_restore = None;
            }
            VoiceUiEvent::Transcribing => {
                self.phase = HudPhase::Transcribing;
                self.mark_voice_control();
                self.step = HudStep::plain("Processing");
                self.tool = "Whisper STT".to_string();
                self.programmed_step_expires_at = None;
                self.programmed_step_restore = None;
            }
            VoiceUiEvent::Transcript(text) => {
                self.transcript = Some(text);
                self.transcript_until = Some(Instant::now() + TRANSCRIPT_VISIBLE_FOR);
            }
            VoiceUiEvent::Planning { tool } => {
                self.phase = HudPhase::Planning;
                self.mark_voice_control();
                self.step = HudStep::plain("Choosing action");
                self.tool = tool;
                self.programmed_step_expires_at = None;
                self.programmed_step_restore = None;
            }
            VoiceUiEvent::Dispatching(action) => {
                self.phase = HudPhase::Dispatching;
                self.mark_voice_control();
                self.step = HudStep::plain(action);
                self.tool = "Unix socket".to_string();
                self.programmed_step_expires_at = None;
                self.programmed_step_restore = None;
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
                let restore = self
                    .programmed_step_restore
                    .take()
                    .map(|snapshot| *snapshot);
                let restore = restore.unwrap_or_else(|| {
                    let mut snapshot = self.clone();
                    snapshot.programmed_step_expires_at = None;
                    snapshot.programmed_step_restore = None;
                    if source.as_deref() != Some("voice") {
                        snapshot.mark_automation_control();
                    }
                    snapshot
                });
                self.phase = HudPhase::Planning;
                self.input_label = if source.as_deref() == Some("voice") {
                    "Voice control".to_string()
                } else {
                    "Automation".to_string()
                };
                self.step = HudStep::protocol(step_index, step_total, label);
                if let Some(task) = task {
                    self.task = task;
                }
                self.tool = tool.or(source).unwrap_or_else(|| "Agent".to_string());
                self.programmed_step_expires_at =
                    ttl_ms.map(|ttl_ms| Instant::now() + Duration::from_millis(ttl_ms));
                self.programmed_step_restore =
                    self.programmed_step_expires_at.map(|_| Box::new(restore));
            }
            VoiceUiEvent::UiMode { mode, source } => {
                let headless = mode == UiMode::Headless;
                self.mode = mode;
                if headless || source.as_deref() != Some("voice") {
                    self.input_label = "Automation".to_string();
                    self.task = "Computer control".to_string();
                } else {
                    self.input_label = "Voice control".to_string();
                    self.task = "Voice control".to_string();
                }
            }
            VoiceUiEvent::AutomationActivity {
                label,
                source,
                tool,
            } => {
                if self.programmed_step_expires_at.is_some() {
                    return;
                }
                self.phase = HudPhase::Dispatching;
                self.mark_automation_control();
                self.task = source.unwrap_or_else(|| "Computer control".to_string());
                self.step = HudStep::plain(label);
                self.tool = tool.unwrap_or_else(|| "Unix socket".to_string());
            }
            VoiceUiEvent::Reply(text) => {
                self.phase = HudPhase::Reply;
                self.mark_voice_control();
                self.step = HudStep::plain("Done");
                self.expanded_until = Some(Instant::now() + reply_visible_for(&text));
                self.response = Some(text);
                self.programmed_step_expires_at = None;
                self.programmed_step_restore = None;
            }
            VoiceUiEvent::AutomationReply(text) => {
                self.phase = HudPhase::Reply;
                self.mark_automation_control();
                self.step = HudStep::plain("Done");
                self.expanded_until = Some(Instant::now() + reply_visible_for(&text));
                self.response = Some(text);
                self.programmed_step_expires_at = None;
                self.programmed_step_restore = None;
            }
            VoiceUiEvent::Error(text) => {
                self.phase = HudPhase::Error;
                self.step = HudStep::plain("Stopped");
                self.expanded_until = Some(Instant::now() + reply_visible_for(&text));
                self.response = Some(text);
                self.programmed_step_expires_at = None;
                self.programmed_step_restore = None;
            }
            VoiceUiEvent::AudioDiagnostic { .. } => {}
            VoiceUiEvent::SttDiagnostic { .. } => {}
            VoiceUiEvent::Metric { .. } => {}
            VoiceUiEvent::ToggleExpanded => {}
            VoiceUiEvent::SetExpanded(_) => {}
            VoiceUiEvent::Idle => {
                let mode = self.mode.clone();
                let input_label = self.input_label.clone();
                *self = Self::default();
                self.mode = mode;
                if input_label == "Automation" || self.mode == UiMode::Headless {
                    self.mark_automation_control();
                }
            }
        }
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded_until
            .map(|deadline| Instant::now() < deadline)
            .unwrap_or(false)
    }

    pub fn is_headful(&self) -> bool {
        self.mode == UiMode::Headful
    }

    pub fn expire_programmed_step(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.programmed_step_expires_at else {
            return false;
        };
        if now < deadline {
            return false;
        }
        *self = self
            .programmed_step_restore
            .take()
            .map(|snapshot| *snapshot)
            .unwrap_or_default();
        true
    }

    pub fn expire_transcript(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.transcript_until else {
            return false;
        };
        if now < deadline {
            return false;
        }
        self.transcript = None;
        self.transcript_until = None;
        true
    }

    fn mark_voice_control(&mut self) {
        self.mode = UiMode::Headful;
        self.input_label = "Voice control".to_string();
        self.task = "Voice control".to_string();
    }

    fn mark_automation_control(&mut self) {
        self.input_label = "Automation".to_string();
        self.task = "Computer control".to_string();
    }
}

fn reply_visible_for(text: &str) -> Duration {
    let text_width = text.chars().count() as f32 * REPLY_MARQUEE_CHAR_WIDTH_PX;
    let overflow = (text_width - REPLY_MARQUEE_VIEWPORT_PX).max(0.0);
    if overflow <= 0.0 {
        return REPLY_VISIBLE_MIN;
    }
    let seconds = REPLY_MARQUEE_START_DELAY_SECS
        + (overflow / REPLY_MARQUEE_SCROLL_SPEED_PX_PER_SEC).max(0.5)
        + REPLY_MARQUEE_END_HOLD_SECS;
    REPLY_VISIBLE_MIN.max(Duration::from_secs_f32(seconds))
}

impl HudStep {
    pub fn plain(label: impl Into<String>) -> Self {
        Self {
            index: None,
            total: None,
            label: label.into(),
        }
    }

    pub fn protocol(index: Option<u16>, total: Option<u16>, label: impl Into<String>) -> Self {
        let total = total.map(|value| value.max(1) as usize);
        let index = index.map(|value| {
            let value = value as usize;
            total.map(|total| value.min(total)).unwrap_or(value)
        });
        Self {
            index,
            total,
            label: label.into(),
        }
    }

    pub fn counter(&self) -> Option<(usize, usize)> {
        Some((self.index?, self.total?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceUiEvent {
    Armed,
    Listening {
        ms: u64,
    },
    RecordingStopped,
    Accepted,
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
    UiMode {
        mode: UiMode,
        source: Option<String>,
    },
    AutomationActivity {
        label: String,
        source: Option<String>,
        tool: Option<String>,
    },
    Reply(String),
    AutomationReply(String),
    Error(String),
    AudioDiagnostic {
        device_name: String,
        channels: u16,
        sample_format: String,
        sample_rate: u32,
        duration_ms: u64,
        peak_amplitude: i16,
        rms_amplitude_ppm: u32,
        wav_bytes: usize,
    },
    SttDiagnostic {
        backend: String,
        model: String,
        generation_id: Option<String>,
        audio_ms: Option<u64>,
        transcript_class: String,
    },
    Metric {
        name: String,
        ms: u64,
    },
    ToggleExpanded,
    SetExpanded(bool),
    Idle,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_expands_for_eight_second_minimum() {
        let mut state = HudSnapshot::default();
        let before = Instant::now();
        state.apply(VoiceUiEvent::Reply("ready".to_string()));

        assert!(state.is_expanded());
        assert_eq!(state.phase, HudPhase::Reply);
        assert_eq!(state.input_label, "Voice control");
        let deadline = state.expanded_until.expect("reply deadline");
        assert!(deadline >= before + Duration::from_millis(7_900));
        assert!(deadline <= Instant::now() + Duration::from_millis(8_100));
    }

    #[test]
    fn long_reply_persists_until_collapsed_marquee_can_finish() {
        let mut state = HudSnapshot::default();
        let before = Instant::now();
        let long_reply = "The agent response is intentionally long enough that the collapsed dynamic island center slot must scroll for more than eight seconds before returning to Ready.";
        state.apply(VoiceUiEvent::Reply(long_reply.to_string()));

        let deadline = state.expanded_until.expect("reply deadline");
        assert!(deadline > before + Duration::from_secs(8));
        assert!(deadline >= before + reply_visible_for(long_reply));
    }

    #[test]
    fn transcript_expires_after_short_visibility_window() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::Transcript("what do you see".to_string()));

        let deadline = state.transcript_until.expect("transcript deadline");
        assert_eq!(state.transcript.as_deref(), Some("what do you see"));
        assert!(!state.expire_transcript(deadline - Duration::from_millis(1)));
        assert_eq!(state.transcript.as_deref(), Some("what do you see"));
        assert!(state.expire_transcript(deadline));
        assert_eq!(state.transcript, None);
        assert_eq!(state.transcript_until, None);
    }

    #[test]
    fn automation_reply_holds_automation_as_last_use() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::AutomationReply("ready".to_string()));

        assert!(state.is_expanded());
        assert_eq!(state.phase, HudPhase::Reply);
        assert_eq!(state.input_label, "Automation");

        state.apply(VoiceUiEvent::Idle);

        assert_eq!(state.phase, HudPhase::Idle);
        assert_eq!(state.step.label, "Ready");
        assert_eq!(state.input_label, "Automation");
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
        assert_eq!(state.step.index, Some(2));
        assert_eq!(state.step.total, Some(5));
        assert_eq!(state.tool, "vision");
        assert_eq!(state.input_label, "Automation");
        assert_eq!(state.transcript.as_deref(), Some("find the red button"));
        assert!(state.programmed_step_expires_at.is_some());
    }

    #[test]
    fn voice_and_automation_events_set_live_input_label() {
        let mut state = HudSnapshot::default();

        state.apply(VoiceUiEvent::Listening { ms: 120 });
        assert_eq!(state.input_label, "Voice control");

        state.apply(VoiceUiEvent::AgentStep {
            label: "checking target".to_string(),
            source: Some("external agent".to_string()),
            task: Some("Browser task".to_string()),
            tool: Some("Unix socket".to_string()),
            step_index: Some(1),
            step_total: Some(3),
            ttl_ms: None,
        });
        assert_eq!(state.input_label, "Automation");
        assert_eq!(state.task, "Browser task");
    }

    #[test]
    fn agent_step_preserves_large_declarative_step_counts() {
        let mut state = HudSnapshot::default();

        state.apply(VoiceUiEvent::AgentStep {
            label: "validating candidate window".to_string(),
            source: Some("agent".to_string()),
            task: Some("Desktop automation".to_string()),
            tool: Some("Unix socket".to_string()),
            step_index: Some(37),
            step_total: Some(120),
            ttl_ms: None,
        });

        assert_eq!(state.step.index, Some(37));
        assert_eq!(state.step.total, Some(120));
        assert_eq!(state.step.label, "validating candidate window");
    }

    #[test]
    fn headless_mode_marks_hud_as_automation() {
        let mut state = HudSnapshot::default();

        state.apply(VoiceUiEvent::UiMode {
            mode: UiMode::Headless,
            source: Some("cli".to_string()),
        });

        assert_eq!(state.input_label, "Automation");
        assert_eq!(state.task, "Computer control");
    }

    #[test]
    fn idle_preserves_automation_source_after_remote_use() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::UiMode {
            mode: UiMode::Headless,
            source: Some("remote".to_string()),
        });
        state.apply(VoiceUiEvent::AutomationActivity {
            label: "mouse click at 10,10".to_string(),
            source: Some("Computer control".to_string()),
            tool: Some("Unix socket".to_string()),
        });

        state.apply(VoiceUiEvent::Idle);

        assert_eq!(state.mode, UiMode::Headless);
        assert_eq!(state.input_label, "Automation");
        assert_eq!(state.task, "Computer control");
        assert_eq!(state.phase, HudPhase::Idle);
    }

    #[test]
    fn non_voice_headful_mode_keeps_hud_labeled_as_automation() {
        let mut state = HudSnapshot::default();

        state.apply(VoiceUiEvent::UiMode {
            mode: UiMode::Headful,
            source: Some("remote".to_string()),
        });

        assert_eq!(state.input_label, "Automation");
        assert_eq!(state.task, "Computer control");

        state.apply(VoiceUiEvent::UiMode {
            mode: UiMode::Headful,
            source: Some("voice".to_string()),
        });

        assert_eq!(state.input_label, "Voice control");
        assert_eq!(state.task, "Voice control");
    }

    #[test]
    fn raw_automation_activity_sets_live_label_without_overriding_programmed_steps() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::AutomationActivity {
            label: "confirmed remote action".to_string(),
            source: Some("Computer control".to_string()),
            tool: Some("Unix socket".to_string()),
        });

        assert_eq!(state.input_label, "Automation");
        assert_eq!(state.phase, HudPhase::Dispatching);
        assert_eq!(state.step.label, "confirmed remote action");

        state.apply(VoiceUiEvent::AgentStep {
            label: "custom visible step".to_string(),
            source: Some("external agent".to_string()),
            task: None,
            tool: None,
            step_index: Some(2),
            step_total: Some(5),
            ttl_ms: Some(1_500),
        });
        state.apply(VoiceUiEvent::AutomationActivity {
            label: "late input completion".to_string(),
            source: None,
            tool: None,
        });

        assert_eq!(state.step.label, "custom visible step");
    }

    #[test]
    fn voice_programmed_step_expires_back_to_prior_voice_state() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::Transcript("open settings".to_string()));
        state.apply(VoiceUiEvent::Planning {
            tool: "OpenRouter Vision".to_string(),
        });
        state.apply(VoiceUiEvent::AgentStep {
            label: "checking current focus".to_string(),
            source: Some("voice".to_string()),
            task: Some("Use browser".to_string()),
            tool: Some("browser".to_string()),
            step_index: Some(1),
            step_total: Some(3),
            ttl_ms: Some(250),
        });

        assert!(!state.expire_programmed_step(Instant::now()));
        assert!(state.expire_programmed_step(Instant::now() + Duration::from_millis(251)));
        assert_eq!(state.phase, HudPhase::Planning);
        assert_eq!(state.step.label, "Choosing action");
        assert_eq!(state.tool, "OpenRouter Vision");
        assert_eq!(state.input_label, "Voice control");
        assert_eq!(state.transcript.as_deref(), Some("open settings"));
    }

    #[test]
    fn external_programmed_step_expires_to_ready_automation() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::AgentStep {
            label: "automation permission cleanup proof".to_string(),
            source: Some("external agent".to_string()),
            task: Some("Programmatic control".to_string()),
            tool: Some("Unix socket".to_string()),
            step_index: Some(6),
            step_total: Some(9),
            ttl_ms: Some(250),
        });

        assert_eq!(state.input_label, "Automation");
        assert!(state.expire_programmed_step(Instant::now() + Duration::from_millis(251)));
        assert_eq!(state.phase, HudPhase::Idle);
        assert_eq!(state.input_label, "Automation");
        assert_eq!(state.task, "Computer control");
        assert_eq!(state.step.label, "Ready");
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

    #[test]
    fn newer_programmed_step_replaces_overlay_without_losing_original_restore_state() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::Dispatching("click 10 20".to_string()));
        state.apply(VoiceUiEvent::AgentStep {
            label: "first overlay".to_string(),
            source: Some("agent".to_string()),
            task: None,
            tool: None,
            step_index: None,
            step_total: None,
            ttl_ms: Some(1_000),
        });
        state.apply(VoiceUiEvent::AgentStep {
            label: "second overlay".to_string(),
            source: Some("agent".to_string()),
            task: None,
            tool: None,
            step_index: None,
            step_total: None,
            ttl_ms: Some(1_000),
        });

        assert_eq!(state.step.label, "second overlay");
        assert!(state.expire_programmed_step(Instant::now() + Duration::from_millis(1_001)));
        assert_eq!(state.phase, HudPhase::Dispatching);
        assert_eq!(state.step.label, "click 10 20");
        assert_eq!(state.tool, "Unix socket");
    }

    #[test]
    fn ui_mode_event_toggles_headful_state_without_resetting_work() {
        let mut state = HudSnapshot::default();
        state.apply(VoiceUiEvent::Dispatching(
            "mouse click at 10,10".to_string(),
        ));
        state.apply(VoiceUiEvent::UiMode {
            mode: UiMode::Headless,
            source: Some("cli".to_string()),
        });

        assert!(!state.is_headful());
        assert_eq!(state.phase, HudPhase::Dispatching);
        assert_eq!(state.step.label, "mouse click at 10,10");

        state.apply(VoiceUiEvent::UiMode {
            mode: UiMode::Headful,
            source: Some("voice".to_string()),
        });

        assert!(state.is_headful());
        assert_eq!(state.phase, HudPhase::Dispatching);
    }
}
