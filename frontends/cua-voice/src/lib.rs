pub mod activation;
pub mod agent_events;
pub mod audio;
pub mod client;
pub mod daemon;
#[cfg(feature = "ui")]
pub mod hud;
#[cfg(feature = "ui")]
pub mod orb;
pub mod orchestrator;
pub mod planner;
pub mod stt;
pub mod ui_state;

pub use activation::ControlDoubleTap;
pub use orchestrator::{
    run_text_turn, run_text_turn_checked, run_voice_turn, run_voice_turn_checked,
    run_voice_turn_until, run_wav_turn, run_wav_turn_checked, VoiceConfig,
};
pub use ui_state::{HudPhase, HudSnapshot, HudStep, VoiceUiEvent};
