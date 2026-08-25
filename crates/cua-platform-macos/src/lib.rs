//! macOS backend crate.
//!
//! This crate owns the macOS capture/input/permission boundary. Until
//! ScreenCaptureKit, CGEvent, and signing are wired through the shared traits,
//! callers must use the synthetic capture backend and refusal-only input backend.

use cua_core::{PermissionReport, PermissionState};

pub const BACKEND_NAME: &str = "macos";

pub fn support_status() -> &'static str {
    "unsupported_until_native_backend_is_enabled"
}

pub fn permission_report() -> PermissionReport {
    PermissionReport {
        screen_recording: screen_recording_permission(),
        accessibility_input: accessibility_permission(),
        automation: automation_permission(),
        clipboard: clipboard_permission(),
        portal: PermissionState::NotApplicable,
    }
}

#[cfg(target_os = "macos")]
fn screen_recording_permission() -> PermissionState {
    if unsafe { CGPreflightScreenCaptureAccess() } {
        PermissionState::Granted
    } else {
        PermissionState::Missing
    }
}

#[cfg(not(target_os = "macos"))]
fn screen_recording_permission() -> PermissionState {
    PermissionState::NotApplicable
}

#[cfg(target_os = "macos")]
fn accessibility_permission() -> PermissionState {
    if unsafe { AXIsProcessTrusted() } {
        PermissionState::Granted
    } else {
        PermissionState::Missing
    }
}

#[cfg(not(target_os = "macos"))]
fn accessibility_permission() -> PermissionState {
    PermissionState::NotApplicable
}

fn automation_permission() -> PermissionState {
    PermissionState::Unknown
}

fn clipboard_permission() -> PermissionState {
    PermissionState::Unknown
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn AXIsProcessTrusted() -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_report_has_macos_shape() {
        let report = permission_report();
        assert_eq!(report.portal, PermissionState::NotApplicable);
        assert_ne!(report.screen_recording, PermissionState::Unknown);
        assert_ne!(report.accessibility_input, PermissionState::Unknown);
    }
}
