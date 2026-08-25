//! macOS backend crate.
//!
//! This crate owns the macOS capture/input/permission boundary. Until
//! ScreenCaptureKit, CGEvent, and signing are wired through the shared traits,
//! callers must use the synthetic capture backend and refusal-only input backend.

use async_trait::async_trait;
use cua_capture::{
    encode_image, CaptureBackend, CaptureRequest, CapturedFrame, SyntheticCaptureBackend,
};
use cua_core::{
    now_wall_ms, CursorState, DisplayInfo, FrameEnvelope, PermissionReport, PermissionState, Rect,
    SCHEMA_VERSION,
};
use image::{imageops::FilterType, ImageBuffer, Rgba};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Instant;

pub const BACKEND_NAME: &str = "macos";

pub fn support_status() -> &'static str {
    "unsupported_until_native_backend_is_enabled"
}

pub fn capture_backend_or_synthetic() -> Arc<dyn CaptureBackend> {
    if permission_report().screen_recording == PermissionState::Granted {
        Arc::new(MacosCaptureBackend::default())
    } else {
        Arc::new(SyntheticCaptureBackend::default())
    }
}

#[derive(Debug)]
pub struct MacosCaptureBackend {
    started: Instant,
}

impl Default for MacosCaptureBackend {
    fn default() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

#[async_trait]
impl CaptureBackend for MacosCaptureBackend {
    async fn capture_latest(&self, request: CaptureRequest) -> anyhow::Result<CapturedFrame> {
        capture_main_display(self.started, request)
    }

    async fn displays(&self) -> anyhow::Result<Vec<DisplayInfo>> {
        native_displays()
    }

    fn name(&self) -> &'static str {
        BACKEND_NAME
    }
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
fn capture_main_display(
    started: Instant,
    request: CaptureRequest,
) -> anyhow::Result<CapturedFrame> {
    let display_id = unsafe { CGMainDisplayID() };
    let image = unsafe { CGDisplayCreateImage(display_id) };
    if image.is_null() {
        anyhow::bail!(
            "CGDisplayCreateImage returned null; Screen Recording permission may be missing"
        );
    }
    let result = unsafe { image_to_frame(started, display_id, image, request) };
    unsafe { CFRelease(image.cast()) };
    result
}

#[cfg(not(target_os = "macos"))]
fn capture_main_display(
    _started: Instant,
    _request: CaptureRequest,
) -> anyhow::Result<CapturedFrame> {
    anyhow::bail!("macOS capture backend is only available on macOS")
}

#[cfg(target_os = "macos")]
unsafe fn image_to_frame(
    started: Instant,
    display_id: u32,
    image: *const std::ffi::c_void,
    request: CaptureRequest,
) -> anyhow::Result<CapturedFrame> {
    let source_width = CGImageGetWidth(image) as u32;
    let source_height = CGImageGetHeight(image) as u32;
    let bits_per_pixel = CGImageGetBitsPerPixel(image);
    if bits_per_pixel != 32 {
        anyhow::bail!("unsupported macOS capture pixel depth {bits_per_pixel}");
    }

    let provider = CGImageGetDataProvider(image);
    if provider.is_null() {
        anyhow::bail!("CGImageGetDataProvider returned null");
    }
    let data = CGDataProviderCopyData(provider);
    if data.is_null() {
        anyhow::bail!("CGDataProviderCopyData returned null");
    }
    let data_ptr = CFDataGetBytePtr(data);
    let data_len = CFDataGetLength(data);
    let bytes_per_row = CGImageGetBytesPerRow(image);
    let source = std::slice::from_raw_parts(data_ptr, data_len);
    let mut rgba = Vec::with_capacity((source_width * source_height * 4) as usize);
    for y in 0..source_height as usize {
        let row_start = y * bytes_per_row;
        for x in 0..source_width as usize {
            let offset = row_start + x * 4;
            let b = source[offset];
            let g = source[offset + 1];
            let r = source[offset + 2];
            let a = source[offset + 3];
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    CFRelease(data.cast());

    let mut buffer = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(source_width, source_height, rgba)
        .ok_or_else(|| anyhow::anyhow!("failed to build macOS capture image buffer"))?;
    if let Some(max_width) = request.max_width {
        if max_width < source_width {
            let target_width = max_width.max(64);
            let target_height = ((source_height as f64)
                * (target_width as f64 / source_width as f64))
                .round() as u32;
            buffer = image::imageops::resize(
                &buffer,
                target_width,
                target_height.max(1),
                FilterType::Triangle,
            );
        }
    }

    let bytes = encode_image(&buffer, request.encoding.clone())?;
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let byte_len = bytes.len();
    let frame_id = started.elapsed().as_millis() as u64;
    let width = buffer.width();
    let height = buffer.height();
    Ok(CapturedFrame {
        envelope: FrameEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            frame_id,
            timestamp_mono_ns: started.elapsed().as_nanos(),
            timestamp_wall_ms: now_wall_ms(),
            display_id: display_id.to_string(),
            width,
            height,
            scale_factor: 1.0,
            pixel_format: "rgba8".to_string(),
            encoding: request.encoding,
            byte_len,
            sha256,
            cursor: CursorState {
                x: (width / 2) as f64,
                y: (height / 2) as f64,
                visible: true,
                included_in_frame: false,
            },
            damage_rects: vec![Rect {
                x: 0,
                y: 0,
                width,
                height,
            }],
        },
        bytes: Arc::new(bytes),
    })
}

#[cfg(target_os = "macos")]
fn native_displays() -> anyhow::Result<Vec<DisplayInfo>> {
    let display_id = unsafe { CGMainDisplayID() };
    let bounds = unsafe { CGDisplayBounds(display_id) };
    Ok(vec![DisplayInfo {
        id: display_id.to_string(),
        name: "Main Display".to_string(),
        x: bounds.origin.x.round() as i32,
        y: bounds.origin.y.round() as i32,
        width: unsafe { CGDisplayPixelsWide(display_id) } as u32,
        height: unsafe { CGDisplayPixelsHigh(display_id) } as u32,
        scale_factor: 1.0,
        active: true,
    }])
}

#[cfg(not(target_os = "macos"))]
fn native_displays() -> anyhow::Result<Vec<DisplayInfo>> {
    anyhow::bail!("macOS display backend is only available on macOS")
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
    fn CGMainDisplayID() -> u32;
    fn CGDisplayCreateImage(display: u32) -> *const std::ffi::c_void;
    fn CGImageGetWidth(image: *const std::ffi::c_void) -> usize;
    fn CGImageGetHeight(image: *const std::ffi::c_void) -> usize;
    fn CGImageGetBitsPerPixel(image: *const std::ffi::c_void) -> usize;
    fn CGImageGetBytesPerRow(image: *const std::ffi::c_void) -> usize;
    fn CGImageGetDataProvider(image: *const std::ffi::c_void) -> *const std::ffi::c_void;
    fn CGDataProviderCopyData(provider: *const std::ffi::c_void) -> *const std::ffi::c_void;
    fn CFDataGetBytePtr(data: *const std::ffi::c_void) -> *const u8;
    fn CFDataGetLength(data: *const std::ffi::c_void) -> usize;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CFRelease(cf: *const std::ffi::c_void);
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
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

    #[test]
    fn capture_backend_selection_is_available() {
        let backend = capture_backend_or_synthetic();
        assert!(matches!(backend.name(), "macos" | "synthetic"));
    }
}
