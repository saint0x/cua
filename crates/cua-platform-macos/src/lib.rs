//! macOS backend crate.
//!
//! This crate owns the macOS capture/input boundary. Until ScreenCaptureKit,
//! CGEvent, signing, and TCC probes are wired through the shared traits, callers
//! must use the synthetic capture backend and refusal-only input backend.

pub const BACKEND_NAME: &str = "macos";

pub fn support_status() -> &'static str {
    "unsupported_until_native_backend_is_enabled"
}
