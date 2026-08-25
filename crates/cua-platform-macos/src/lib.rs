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
    now_wall_ms, CursorState, DeliveryMode, DisplayInfo, Effect, Evidence, EvidenceKind,
    FrameEnvelope, InputAction, InputRequest, InputResult, InputRoute, MouseButton,
    PermissionReport, PermissionState, Rect, SCHEMA_VERSION,
};
use cua_input::{InputBackend, RefusingInputBackend};
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

pub fn input_backend_or_refusing() -> Arc<dyn InputBackend> {
    if permission_report().accessibility_input == PermissionState::Granted {
        Arc::new(MacosInputBackend)
    } else {
        Arc::new(RefusingInputBackend)
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

#[derive(Debug, Default)]
pub struct MacosInputBackend;

#[async_trait]
impl InputBackend for MacosInputBackend {
    async fn execute(&self, request: InputRequest) -> InputResult {
        let started = Instant::now();
        let idempotency_key = request.idempotency_key;
        let result = match request.action {
            InputAction::MouseMove { x, y, .. } => post_mouse_move(x, y),
            InputAction::MouseClick {
                x,
                y,
                button,
                count,
            } => post_mouse_click(x, y, button, count),
            InputAction::MouseDrag {
                from_x,
                from_y,
                to_x,
                to_y,
                ..
            } => post_mouse_drag(from_x, from_y, to_x, to_y),
            InputAction::KeyPress { combo } => post_key_combo(&combo),
            InputAction::KeyType { text } => post_text(&text),
            InputAction::Pause | InputAction::Resume | InputAction::KillSwitch => {
                Ok("safety action accepted by local coordinator".to_string())
            }
            InputAction::KeyPaste { .. }
            | InputAction::ClipboardRead { .. }
            | InputAction::ClipboardWrite { .. } => Err(
                "clipboard and paste actions must use explicit clipboard/profile endpoints"
                    .to_string(),
            ),
        };
        match result {
            Ok(message) => input_result(
                idempotency_key,
                Effect::Confirmed,
                InputRoute::Accessibility,
                DeliveryMode::Desktop,
                EvidenceKind::ValueReadback,
                message,
                started.elapsed().as_nanos(),
            ),
            Err(message) => input_result(
                idempotency_key,
                Effect::Refused,
                InputRoute::Unavailable,
                DeliveryMode::NotApplicable,
                EvidenceKind::Refusal,
                message,
                started.elapsed().as_nanos(),
            ),
        }
    }

    fn name(&self) -> &'static str {
        "macos-cgevent"
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
fn post_mouse_move(x: i32, y: i32) -> Result<String, String> {
    post_mouse_event(CG_EVENT_MOUSE_MOVED, x, y, CG_MOUSE_BUTTON_LEFT)?;
    Ok("mouse move posted through CGEvent".to_string())
}

#[cfg(not(target_os = "macos"))]
fn post_mouse_move(_x: i32, _y: i32) -> Result<String, String> {
    Err("macOS CGEvent input is only available on macOS".to_string())
}

#[cfg(target_os = "macos")]
fn post_mouse_click(x: i32, y: i32, button: MouseButton, count: u8) -> Result<String, String> {
    let (down, up, native_button) = match button {
        MouseButton::Left => (
            CG_EVENT_LEFT_MOUSE_DOWN,
            CG_EVENT_LEFT_MOUSE_UP,
            CG_MOUSE_BUTTON_LEFT,
        ),
        MouseButton::Right => (
            CG_EVENT_RIGHT_MOUSE_DOWN,
            CG_EVENT_RIGHT_MOUSE_UP,
            CG_MOUSE_BUTTON_RIGHT,
        ),
        MouseButton::Middle => (
            CG_EVENT_OTHER_MOUSE_DOWN,
            CG_EVENT_OTHER_MOUSE_UP,
            CG_MOUSE_BUTTON_CENTER,
        ),
    };
    for _ in 0..count.max(1) {
        post_mouse_event(down, x, y, native_button)?;
        post_mouse_event(up, x, y, native_button)?;
    }
    Ok("mouse click posted through CGEvent".to_string())
}

#[cfg(not(target_os = "macos"))]
fn post_mouse_click(_x: i32, _y: i32, _button: MouseButton, _count: u8) -> Result<String, String> {
    Err("macOS CGEvent input is only available on macOS".to_string())
}

#[cfg(target_os = "macos")]
fn post_mouse_drag(from_x: i32, from_y: i32, to_x: i32, to_y: i32) -> Result<String, String> {
    post_mouse_event(
        CG_EVENT_LEFT_MOUSE_DOWN,
        from_x,
        from_y,
        CG_MOUSE_BUTTON_LEFT,
    )?;
    post_mouse_event(
        CG_EVENT_LEFT_MOUSE_DRAGGED,
        to_x,
        to_y,
        CG_MOUSE_BUTTON_LEFT,
    )?;
    post_mouse_event(CG_EVENT_LEFT_MOUSE_UP, to_x, to_y, CG_MOUSE_BUTTON_LEFT)?;
    Ok("mouse drag posted through CGEvent".to_string())
}

#[cfg(not(target_os = "macos"))]
fn post_mouse_drag(_from_x: i32, _from_y: i32, _to_x: i32, _to_y: i32) -> Result<String, String> {
    Err("macOS CGEvent input is only available on macOS".to_string())
}

#[cfg(target_os = "macos")]
fn post_mouse_event(event_type: u32, x: i32, y: i32, button: u32) -> Result<(), String> {
    let point = CGPoint {
        x: x as f64,
        y: y as f64,
    };
    let event = unsafe { CGEventCreateMouseEvent(std::ptr::null(), event_type, point, button) };
    if event.is_null() {
        return Err("CGEventCreateMouseEvent returned null".to_string());
    }
    unsafe {
        CGEventPost(CG_HID_EVENT_TAP, event);
        CFRelease(event.cast());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn post_key_combo(combo: &str) -> Result<String, String> {
    let mut flags = 0u64;
    let mut key = None;
    for part in combo.split('+') {
        let normalized = part.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "cmd" | "command" | "meta" => flags |= CG_EVENT_FLAG_MASK_COMMAND,
            "ctrl" | "control" => flags |= CG_EVENT_FLAG_MASK_CONTROL,
            "alt" | "option" => flags |= CG_EVENT_FLAG_MASK_ALTERNATE,
            "shift" => flags |= CG_EVENT_FLAG_MASK_SHIFT,
            value => key = virtual_key(value),
        }
    }
    let key = key.ok_or_else(|| format!("unsupported key combo {combo}"))?;
    post_key(key, flags)?;
    Ok("key combo posted through CGEvent".to_string())
}

#[cfg(not(target_os = "macos"))]
fn post_key_combo(_combo: &str) -> Result<String, String> {
    Err("macOS CGEvent input is only available on macOS".to_string())
}

#[cfg(target_os = "macos")]
fn post_text(text: &str) -> Result<String, String> {
    for ch in text.chars() {
        let utf16: Vec<u16> = ch.encode_utf16(&mut [0; 2]).to_vec();
        post_unicode_key(&utf16, true)?;
        post_unicode_key(&utf16, false)?;
    }
    Ok("text posted through CGEvent unicode keyboard events".to_string())
}

#[cfg(not(target_os = "macos"))]
fn post_text(_text: &str) -> Result<String, String> {
    Err("macOS CGEvent input is only available on macOS".to_string())
}

#[cfg(target_os = "macos")]
fn post_key(key: u16, flags: u64) -> Result<(), String> {
    for down in [true, false] {
        let event = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), key, down) };
        if event.is_null() {
            return Err("CGEventCreateKeyboardEvent returned null".to_string());
        }
        unsafe {
            CGEventSetFlags(event, flags);
            CGEventPost(CG_HID_EVENT_TAP, event);
            CFRelease(event.cast());
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn post_unicode_key(utf16: &[u16], down: bool) -> Result<(), String> {
    let event = unsafe { CGEventCreateKeyboardEvent(std::ptr::null(), 0, down) };
    if event.is_null() {
        return Err("CGEventCreateKeyboardEvent returned null".to_string());
    }
    unsafe {
        CGEventKeyboardSetUnicodeString(event, utf16.len(), utf16.as_ptr());
        CGEventPost(CG_HID_EVENT_TAP, event);
        CFRelease(event.cast());
    }
    Ok(())
}

fn input_result(
    idempotency_key: uuid::Uuid,
    effect: Effect,
    route: InputRoute,
    delivery_mode: DeliveryMode,
    evidence_kind: EvidenceKind,
    message: impl Into<String>,
    ended_mono_ns: u128,
) -> InputResult {
    InputResult {
        schema_version: SCHEMA_VERSION.to_string(),
        idempotency_key,
        effect,
        route,
        delivery_mode,
        started_mono_ns: 0,
        ended_mono_ns,
        evidence: vec![Evidence {
            kind: evidence_kind,
            message: message.into(),
            frame_id: None,
        }],
    }
}

#[cfg(target_os = "macos")]
fn virtual_key(value: &str) -> Option<u16> {
    let key = match value {
        "a" => 0x00,
        "s" => 0x01,
        "d" => 0x02,
        "f" => 0x03,
        "h" => 0x04,
        "g" => 0x05,
        "z" => 0x06,
        "x" => 0x07,
        "c" => 0x08,
        "v" => 0x09,
        "b" => 0x0B,
        "q" => 0x0C,
        "w" => 0x0D,
        "e" => 0x0E,
        "r" => 0x0F,
        "y" => 0x10,
        "t" => 0x11,
        "1" => 0x12,
        "2" => 0x13,
        "3" => 0x14,
        "4" => 0x15,
        "6" => 0x16,
        "5" => 0x17,
        "=" | "equal" => 0x18,
        "9" => 0x19,
        "7" => 0x1A,
        "-" | "minus" => 0x1B,
        "8" => 0x1C,
        "0" => 0x1D,
        "]" | "rightbracket" => 0x1E,
        "o" => 0x1F,
        "u" => 0x20,
        "[" | "leftbracket" => 0x21,
        "i" => 0x22,
        "p" => 0x23,
        "return" | "enter" => 0x24,
        "l" => 0x25,
        "j" => 0x26,
        "'" | "quote" => 0x27,
        "k" => 0x28,
        ";" | "semicolon" => 0x29,
        "\\" | "backslash" => 0x2A,
        "," | "comma" => 0x2B,
        "/" | "slash" => 0x2C,
        "n" => 0x2D,
        "m" => 0x2E,
        "." | "period" => 0x2F,
        "tab" => 0x30,
        "space" => 0x31,
        "delete" | "backspace" => 0x33,
        "escape" | "esc" => 0x35,
        "left" => 0x7B,
        "right" => 0x7C,
        "down" => 0x7D,
        "up" => 0x7E,
        _ => return None,
    };
    Some(key)
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
    fn CGEventCreateKeyboardEvent(
        source: *const std::ffi::c_void,
        virtual_key: u16,
        key_down: bool,
    ) -> *const std::ffi::c_void;
    fn CGEventCreateMouseEvent(
        source: *const std::ffi::c_void,
        mouse_type: u32,
        mouse_cursor_position: CGPoint,
        mouse_button: u32,
    ) -> *const std::ffi::c_void;
    fn CGEventKeyboardSetUnicodeString(
        event: *const std::ffi::c_void,
        string_length: usize,
        unicode_string: *const u16,
    );
    fn CGEventPost(tap: u32, event: *const std::ffi::c_void);
    fn CGEventSetFlags(event: *const std::ffi::c_void, flags: u64);
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

#[cfg(target_os = "macos")]
const CG_HID_EVENT_TAP: u32 = 0;
#[cfg(target_os = "macos")]
const CG_EVENT_LEFT_MOUSE_DOWN: u32 = 1;
#[cfg(target_os = "macos")]
const CG_EVENT_LEFT_MOUSE_UP: u32 = 2;
#[cfg(target_os = "macos")]
const CG_EVENT_RIGHT_MOUSE_DOWN: u32 = 3;
#[cfg(target_os = "macos")]
const CG_EVENT_RIGHT_MOUSE_UP: u32 = 4;
#[cfg(target_os = "macos")]
const CG_EVENT_MOUSE_MOVED: u32 = 5;
#[cfg(target_os = "macos")]
const CG_EVENT_LEFT_MOUSE_DRAGGED: u32 = 6;
#[cfg(target_os = "macos")]
const CG_EVENT_OTHER_MOUSE_DOWN: u32 = 25;
#[cfg(target_os = "macos")]
const CG_EVENT_OTHER_MOUSE_UP: u32 = 26;
#[cfg(target_os = "macos")]
const CG_MOUSE_BUTTON_LEFT: u32 = 0;
#[cfg(target_os = "macos")]
const CG_MOUSE_BUTTON_RIGHT: u32 = 1;
#[cfg(target_os = "macos")]
const CG_MOUSE_BUTTON_CENTER: u32 = 2;
#[cfg(target_os = "macos")]
const CG_EVENT_FLAG_MASK_SHIFT: u64 = 0x0002_0000;
#[cfg(target_os = "macos")]
const CG_EVENT_FLAG_MASK_CONTROL: u64 = 0x0004_0000;
#[cfg(target_os = "macos")]
const CG_EVENT_FLAG_MASK_ALTERNATE: u64 = 0x0008_0000;
#[cfg(target_os = "macos")]
const CG_EVENT_FLAG_MASK_COMMAND: u64 = 0x0010_0000;

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

    #[test]
    fn input_backend_selection_is_available() {
        let backend = input_backend_or_refusing();
        assert!(matches!(backend.name(), "macos-cgevent" | "refusing"));
    }
}
