//! macOS backend crate.
//!
//! This crate owns the macOS capture/input/permission boundary.

use async_trait::async_trait;
use cua_capture::{
    encode_image, CaptureBackend, CaptureRequest, CaptureSource, CapturedFrame,
    CapturedFrameTimings, UnavailableCaptureBackend,
};
use cua_core::{
    now_wall_ms, CursorState, DeliveryMode, DisplayInfo, Effect, Evidence, EvidenceKind,
    FrameEnvelope, InputAction, InputRequest, InputResult, InputRoute, MouseButton,
    PermissionReport, PermissionState, Rect, WindowInfo, SCHEMA_VERSION,
};
use cua_input::InputBackend;
use image::{ImageBuffer, Rgba};
use sha2::{Digest, Sha256};
#[cfg(target_os = "macos")]
use std::ffi::CStr;
#[cfg(target_os = "macos")]
use std::os::raw::c_char;
#[cfg(target_os = "macos")]
use std::os::raw::c_void;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const BACKEND_NAME: &str = "macos";

pub fn support_status() -> &'static str {
    "macos_native_capture_and_input_enabled"
}

pub fn capture_backend_or_unavailable() -> Arc<dyn CaptureBackend> {
    if permission_report().screen_recording == PermissionState::Granted {
        Arc::new(MacosCaptureBackend::default())
    } else {
        Arc::new(UnavailableCaptureBackend::new(
            "macOS Screen Recording permission is required for native capture",
        ))
    }
}

pub fn input_backend() -> Arc<dyn InputBackend> {
    Arc::new(MacosInputBackend)
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
        let started = self.started;
        tokio::task::spawn_blocking(move || capture_main_display(started, request))
            .await
            .map_err(|error| anyhow::anyhow!("macOS capture worker failed: {error}"))?
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

struct MacosActionOutcome {
    message: String,
    route: InputRoute,
    delivery_mode: DeliveryMode,
    evidence_kind: EvidenceKind,
}

impl MacosActionOutcome {
    fn desktop(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            route: InputRoute::Accessibility,
            delivery_mode: DeliveryMode::Desktop,
            evidence_kind: EvidenceKind::ValueReadback,
        }
    }

    fn system(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            route: InputRoute::SystemApi,
            delivery_mode: DeliveryMode::Background,
            evidence_kind: EvidenceKind::ValueReadback,
        }
    }
}

#[async_trait]
impl InputBackend for MacosInputBackend {
    async fn execute(&self, request: InputRequest) -> InputResult {
        let started = Instant::now();
        let idempotency_key = request.idempotency_key;
        let result = execute_macos_input_action(request.action).await;
        match result {
            Ok(outcome) => input_result(
                idempotency_key,
                Effect::Confirmed,
                outcome.route,
                outcome.delivery_mode,
                outcome.evidence_kind,
                outcome.message,
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

async fn execute_macos_input_action(action: InputAction) -> Result<MacosActionOutcome, String> {
    match action {
        InputAction::Sequence {
            actions,
            inter_action_delay_ms,
        } => {
            if actions.is_empty() {
                return Err("sequence must contain at least one action".to_string());
            }
            let delay = Duration::from_millis(inter_action_delay_ms.min(2_000));
            let last_index = actions.len().saturating_sub(1);
            let action_count = actions.len();
            let mut messages = Vec::with_capacity(action_count);
            for (index, action) in actions.into_iter().enumerate() {
                let outcome = execute_macos_input_leaf(action).await?;
                messages.push(outcome.message);
                if index < last_index && !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
            Ok(MacosActionOutcome::system(sequence_message(
                action_count,
                &messages,
            )))
        }
        action => execute_macos_input_leaf(action).await,
    }
}

async fn execute_macos_input_leaf(action: InputAction) -> Result<MacosActionOutcome, String> {
    match action {
        InputAction::MouseMove { x, y, .. } => {
            post_mouse_move(x, y).map(MacosActionOutcome::desktop)
        }
        InputAction::MouseClick {
            x,
            y,
            button,
            count,
        } => post_mouse_click(x, y, button, count).map(MacosActionOutcome::desktop),
        InputAction::MouseDrag {
            from_x,
            from_y,
            to_x,
            to_y,
            ..
        } => post_mouse_drag(from_x, from_y, to_x, to_y).map(MacosActionOutcome::desktop),
        InputAction::KeyPress { combo } => post_key_combo(&combo).map(MacosActionOutcome::desktop),
        InputAction::KeyType { text } => post_text(&text).map(MacosActionOutcome::desktop),
        InputAction::KeyPaste { text } => post_text(&text).map(MacosActionOutcome::desktop),
        InputAction::OpenApp { app_name } => open_app(&app_name).map(MacosActionOutcome::system),
        InputAction::ShellExec {
            command,
            timeout_ms,
        } => run_shell_command(command, timeout_ms)
            .await
            .map(MacosActionOutcome::system),
        InputAction::Aegis { args, timeout_ms } => run_aegis_command(args, timeout_ms)
            .await
            .map(MacosActionOutcome::system),
        InputAction::Ctx {
            args,
            timeout_ms,
            workspace_root,
        } => run_ctx_command(args, timeout_ms, workspace_root)
            .await
            .map(MacosActionOutcome::system),
        InputAction::Pause | InputAction::Resume | InputAction::KillSwitch => Ok(
            MacosActionOutcome::system("safety action accepted by local coordinator"),
        ),
        InputAction::Sequence { .. } => Err("nested sequences are not supported".to_string()),
        InputAction::ClipboardRead { .. } | InputAction::ClipboardWrite { .. } => {
            Err("clipboard actions must use explicit clipboard/profile endpoints".to_string())
        }
    }
}

#[cfg(target_os = "macos")]
fn open_app(app_name: &str) -> Result<String, String> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg("-a")
        .arg(app_name)
        .status()
        .map_err(|error| format!("open app failed to launch /usr/bin/open: {error}"))?;
    if status.success() {
        Ok(format!("opened app {app_name}"))
    } else {
        Err(format!("open app failed for {app_name}: {status}"))
    }
}

#[cfg(not(target_os = "macos"))]
fn open_app(_app_name: &str) -> Result<String, String> {
    Err("open_app is only available on macOS".to_string())
}

async fn run_shell_command(command: String, timeout_ms: u64) -> Result<String, String> {
    if command.trim().is_empty() {
        return Err("shell command must not be empty".to_string());
    }
    let timeout = Duration::from_millis(timeout_ms.clamp(100, 30_000));
    let child = tokio::process::Command::new("/bin/zsh")
        .arg("-lc")
        .arg(&command)
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(timeout, child)
        .await
        .map_err(|_| format!("shell command timed out after {}ms", timeout.as_millis()))?
        .map_err(|error| format!("shell command failed to launch: {error}"))?;
    command_output_message(
        "shell",
        output.status.code(),
        &output.stdout,
        &output.stderr,
    )
}

async fn run_aegis_command(args: Vec<String>, timeout_ms: u64) -> Result<String, String> {
    if args.iter().any(|arg| arg.trim().is_empty()) {
        return Err("aegis args must not contain empty values".to_string());
    }
    let binary = aegis_binary();
    let timeout = Duration::from_millis(timeout_ms.clamp(100, 60_000));
    let child = tokio::process::Command::new(&binary)
        .args(&args)
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(timeout, child)
        .await
        .map_err(|_| format!("aegis command timed out after {}ms", timeout.as_millis()))?
        .map_err(|error| {
            format!(
                "aegis command failed to launch {}: {error}",
                binary.display()
            )
        })?;
    command_output_message(
        "aegis",
        output.status.code(),
        &output.stdout,
        &output.stderr,
    )
}

async fn run_ctx_command(
    args: Vec<String>,
    timeout_ms: u64,
    workspace_root: Option<String>,
) -> Result<String, String> {
    if args.is_empty() {
        return Err("ctx args must contain a command".to_string());
    }
    if args.iter().any(|arg| arg.trim().is_empty()) {
        return Err("ctx args must not contain empty values".to_string());
    }
    let binary = ctx_binary();
    let timeout = Duration::from_millis(timeout_ms.clamp(100, 60_000));
    let mut command = tokio::process::Command::new(&binary);
    if let Some(workspace_root) = workspace_root {
        if workspace_root.trim().is_empty() {
            return Err("ctx workspace_root must not be empty".to_string());
        }
        command.env("CUA_CTX_WORKSPACE_ROOT", workspace_root);
    }
    let child = command.args(&args).kill_on_drop(true).output();
    let output = tokio::time::timeout(timeout, child)
        .await
        .map_err(|_| format!("ctx command timed out after {}ms", timeout.as_millis()))?
        .map_err(|error| {
            format!(
                "ctx command failed to launch {}: {error}",
                binary.display()
            )
        })?;
    command_output_message(
        "ctx",
        output.status.code(),
        &output.stdout,
        &output.stderr,
    )
}

fn aegis_binary() -> std::path::PathBuf {
    let mut candidates = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(std::path::PathBuf::from(home).join(".local/bin/aegis"));
    }
    candidates.push(std::path::PathBuf::from("/opt/homebrew/bin/aegis"));
    candidates.push(std::path::PathBuf::from("/usr/local/bin/aegis"));
    candidates
        .into_iter()
        .find(|candidate| candidate.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("aegis"))
}

fn ctx_binary() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("CUA_CTX_BIN") {
        if !path.trim().is_empty() {
            return std::path::PathBuf::from(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let sibling = parent.join("ctx");
            if sibling.exists() {
                return sibling;
            }
        }
    }
    std::path::PathBuf::from("vendor/ctx/ctx")
}

fn command_output_message(
    tool: &str,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<String, String> {
    let stdout = compact_command_output(stdout);
    let stderr = compact_command_output(stderr);
    let status = code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string());
    let message = format!("{tool} exited {status}; stdout={stdout}; stderr={stderr}");
    if code == Some(0) {
        Ok(message)
    } else {
        Err(message)
    }
}

fn compact_command_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let limit = 1_200;
    if compact.chars().count() <= limit {
        return compact;
    }
    let mut truncated = compact.chars().take(limit).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn sequence_message(action_count: usize, messages: &[String]) -> String {
    let joined = messages
        .iter()
        .map(|message| compact_text(message, 160))
        .collect::<Vec<_>>()
        .join(" | ");
    compact_text(
        &format!("sequence posted {action_count} actions: {joined}"),
        1_200,
    )
}

fn compact_text(value: &str, limit: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= limit {
        return compact;
    }
    let mut truncated = compact.chars().take(limit).collect::<String>();
    truncated.push_str("...");
    truncated
}

pub fn permission_report() -> PermissionReport {
    PermissionReport {
        screen_recording: screen_recording_permission(),
        accessibility_input: accessibility_permission(),
        input_monitoring: input_monitoring_permission(),
        automation: automation_permission(),
        clipboard: clipboard_permission(),
        portal: PermissionState::NotApplicable,
    }
}

pub fn request_screen_recording_access() -> PermissionState {
    native_request_screen_recording_access()
}

pub fn input_monitoring_permission() -> PermissionState {
    native_input_monitoring_permission()
}

pub fn request_accessibility_input_access() -> PermissionState {
    native_request_accessibility_input_access()
}

pub fn microphone_permission() -> PermissionState {
    native_microphone_permission()
}

pub fn request_microphone_access() -> PermissionState {
    native_request_microphone_access()
}

pub fn cursor_state() -> CursorState {
    native_cursor_state()
}

pub fn control_key_is_down() -> bool {
    native_control_key_is_down()
}

pub fn left_mouse_button_is_down() -> bool {
    native_left_mouse_button_is_down()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LeftMouseEventKind {
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeftMouseEvent {
    pub kind: LeftMouseEventKind,
    pub x: f64,
    pub y: f64,
}

pub fn start_left_mouse_event_monitor(tx: Sender<LeftMouseEvent>) -> Result<(), String> {
    native_start_left_mouse_event_monitor(tx)
}

pub fn window_list() -> anyhow::Result<Vec<WindowInfo>> {
    native_window_list()
}

#[cfg(target_os = "macos")]
fn capture_main_display(
    started: Instant,
    request: CaptureRequest,
) -> anyhow::Result<CapturedFrame> {
    let mut errors = Vec::new();
    match capture_main_display_sck(started, request.clone()) {
        Ok(frame) => return Ok(frame),
        Err(error) => errors.push(format!("sck: {error}")),
    }
    match capture_main_display_core_graphics(started, request) {
        Ok(frame) => Ok(frame),
        Err(error) => {
            errors.push(format!("core_graphics: {error}"));
            anyhow::bail!("macOS capture failed: {}", errors.join("; "))
        }
    }
}

#[cfg(target_os = "macos")]
fn capture_main_display_sck(
    started: Instant,
    request: CaptureRequest,
) -> anyhow::Result<CapturedFrame> {
    let capture_started = Instant::now();
    let display_id = unsafe { CGMainDisplayID() };
    let display_bounds = unsafe { CGDisplayBounds(display_id) };
    let rect = objc2_core_foundation::CGRect {
        origin: objc2_core_foundation::CGPoint {
            x: display_bounds.origin.x,
            y: display_bounds.origin.y,
        },
        size: objc2_core_foundation::CGSize {
            width: display_bounds.size.width,
            height: display_bounds.size.height,
        },
    };
    let (sender, receiver) = std::sync::mpsc::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let callback_sender = sender.clone();
    let callback_request = request.clone();
    let block = block2::RcBlock::new(
        move |image: *mut objc2_core_graphics::CGImage, error: *mut objc2_foundation::NSError| {
            let result = if !error.is_null() {
                Err(anyhow::anyhow!("ScreenCaptureKit returned an error"))
            } else if image.is_null() {
                Err(anyhow::anyhow!("ScreenCaptureKit returned null image"))
            } else {
                unsafe {
                    image_to_frame(
                        started,
                        capture_started,
                        display_id,
                        image.cast(),
                        callback_request.clone(),
                        FrameGeometry {
                            display_bounds,
                            frame_origin_x: display_bounds.origin.x.round() as i32,
                            frame_origin_y: display_bounds.origin.y.round() as i32,
                        },
                        CaptureSource::ScreenCaptureKit,
                    )
                }
            };
            if let Some(sender) = callback_sender
                .lock()
                .ok()
                .and_then(|mut sender| sender.take())
            {
                let _ = sender.send(result);
            }
        },
    );
    unsafe {
        objc2_screen_capture_kit::SCScreenshotManager::captureImageInRect_completionHandler(
            rect,
            Some(&block),
        );
    }
    recv_sck_result(receiver, sck_capture_timeout(), "ScreenCaptureKit capture")?
}

#[cfg(target_os = "macos")]
fn sck_capture_timeout() -> Duration {
    let millis = std::env::var("CUA_SCK_CAPTURE_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1_500)
        .clamp(250, 30_000);
    Duration::from_millis(millis)
}

#[cfg(target_os = "macos")]
fn capture_main_display_core_graphics(
    started: Instant,
    request: CaptureRequest,
) -> anyhow::Result<CapturedFrame> {
    let capture_started = Instant::now();
    let display_id = unsafe { CGMainDisplayID() };
    let image = unsafe { CGDisplayCreateImage(display_id) };
    if image.is_null() {
        anyhow::bail!("CGDisplayCreateImage returned null");
    }
    let result = unsafe {
        image_to_frame(
            started,
            capture_started,
            display_id,
            image,
            request,
            FrameGeometry::display(display_id),
            CaptureSource::CoreGraphics,
        )
    };
    unsafe {
        CFRelease(image.cast());
    }
    result
}

#[cfg(target_os = "macos")]
fn recv_sck_result<T>(
    receiver: std::sync::mpsc::Receiver<T>,
    timeout: Duration,
    label: &str,
) -> anyhow::Result<T> {
    let started = Instant::now();
    loop {
        match receiver.try_recv() {
            Ok(value) => return Ok(value),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                anyhow::bail!("{label} channel disconnected");
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if started.elapsed() >= timeout {
                    anyhow::bail!("{label} timed out");
                }
                unsafe {
                    CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.01, true);
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn capture_main_display(
    _started: Instant,
    _request: CaptureRequest,
) -> anyhow::Result<CapturedFrame> {
    anyhow::bail!("macOS capture backend is only available on macOS")
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct FrameGeometry {
    display_bounds: CGRect,
    frame_origin_x: i32,
    frame_origin_y: i32,
}

#[cfg(target_os = "macos")]
impl FrameGeometry {
    fn display(display_id: u32) -> Self {
        let bounds = unsafe { CGDisplayBounds(display_id) };
        Self {
            display_bounds: bounds,
            frame_origin_x: bounds.origin.x.round() as i32,
            frame_origin_y: bounds.origin.y.round() as i32,
        }
    }
}

#[cfg(target_os = "macos")]
unsafe fn image_to_frame(
    started: Instant,
    capture_started: Instant,
    display_id: u32,
    image: *const std::ffi::c_void,
    request: CaptureRequest,
    geometry: FrameGeometry,
    capture_source: CaptureSource,
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
    let target_width = request
        .max_width
        .filter(|max_width| *max_width < source_width)
        .map(|max_width| max_width.max(64))
        .unwrap_or(source_width);
    let target_height = if target_width == source_width {
        source_height
    } else {
        ((source_height as f64) * (target_width as f64 / source_width as f64)).round() as u32
    }
    .max(1);
    let buffer = scaled_bgra_source_to_rgba(
        source,
        bytes_per_row,
        source_width,
        source_height,
        target_width,
        target_height,
    )?;
    CFRelease(data.cast());

    let encode_started = Instant::now();
    let bytes = encode_image(&buffer, request.encoding.clone())?;
    let encode_ns = elapsed_ns(encode_started);
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let byte_len = bytes.len();
    let frame_id = started.elapsed().as_millis() as u64;
    let width = buffer.width();
    let height = buffer.height();
    let display_width = unsafe { CGDisplayPixelsWide(display_id) } as u32;
    let display_height = unsafe { CGDisplayPixelsHigh(display_id) } as u32;
    Ok(CapturedFrame {
        envelope: FrameEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            frame_id,
            timestamp_mono_ns: started.elapsed().as_nanos(),
            timestamp_wall_ms: now_wall_ms(),
            display_id: display_id.to_string(),
            display_x: geometry.display_bounds.origin.x.round() as i32,
            display_y: geometry.display_bounds.origin.y.round() as i32,
            display_width,
            display_height,
            frame_origin_x: geometry.frame_origin_x,
            frame_origin_y: geometry.frame_origin_y,
            width,
            height,
            scale_factor: 1.0,
            pixel_format: "rgba8".to_string(),
            encoding: request.encoding,
            byte_len,
            sha256,
            cursor: native_cursor_state(),
            damage_rects: vec![Rect {
                x: 0,
                y: 0,
                width,
                height,
            }],
        },
        bytes: Arc::new(bytes),
        timings: CapturedFrameTimings {
            capture_ns: elapsed_ns(capture_started),
            encode_ns,
            source: capture_source,
        },
    })
}

#[cfg(target_os = "macos")]
fn scaled_bgra_source_to_rgba(
    source: &[u8],
    bytes_per_row: usize,
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> anyhow::Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let mut rgba = Vec::with_capacity((target_width * target_height * 4) as usize);
    for y in 0..target_height as usize {
        let source_y = y * source_height as usize / target_height as usize;
        let row_start = source_y * bytes_per_row;
        for x in 0..target_width as usize {
            let source_x = x * source_width as usize / target_width as usize;
            let offset = row_start + source_x * 4;
            let b = source[offset];
            let g = source[offset + 1];
            let r = source[offset + 2];
            let a = source[offset + 3];
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(target_width, target_height, rgba)
        .ok_or_else(|| anyhow::anyhow!("failed to build macOS capture image buffer"))
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
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

#[cfg(target_os = "macos")]
fn native_cursor_state() -> CursorState {
    let event = unsafe { CGEventCreate(std::ptr::null()) };
    if event.is_null() {
        return CursorState {
            x: 0.0,
            y: 0.0,
            visible: false,
            included_in_frame: false,
        };
    }
    let point = unsafe { CGEventGetLocation(event) };
    unsafe { CFRelease(event.cast()) };
    CursorState {
        x: point.x,
        y: point.y,
        visible: true,
        included_in_frame: false,
    }
}

#[cfg(target_os = "macos")]
fn native_control_key_is_down() -> bool {
    unsafe {
        CGEventSourceKeyState(K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE, K_VK_CONTROL)
            || CGEventSourceKeyState(K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE, K_VK_RIGHT_CONTROL)
            || current_event_flags_include(CG_EVENT_FLAG_MASK_CONTROL)
    }
}

#[cfg(target_os = "macos")]
fn native_left_mouse_button_is_down() -> bool {
    unsafe {
        CGEventSourceButtonState(
            K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE,
            CG_MOUSE_BUTTON_LEFT,
        )
    }
}

#[cfg(target_os = "macos")]
fn native_start_left_mouse_event_monitor(tx: Sender<LeftMouseEvent>) -> Result<(), String> {
    let (start_tx, start_rx) = channel();
    std::thread::Builder::new()
        .name("cua-left-mouse-event-tap".to_string())
        .spawn(move || unsafe {
            let boxed_tx = Box::new(tx);
            let user_info = Box::into_raw(boxed_tx).cast::<c_void>();
            let mask = (1_u64 << CG_EVENT_LEFT_MOUSE_DOWN) | (1_u64 << CG_EVENT_LEFT_MOUSE_UP);
            let tap = CGEventTapCreate(
                CG_HID_EVENT_TAP,
                CG_HEAD_INSERT_EVENT_TAP,
                CG_EVENT_TAP_OPTION_LISTEN_ONLY,
                mask,
                left_mouse_event_tap_callback,
                user_info,
            );
            if tap.is_null() {
                start_tx
                    .send(Err("CGEventTapCreate returned null".to_string()))
                    .ok();
                drop(Box::from_raw(user_info.cast::<Sender<LeftMouseEvent>>()));
                return;
            }
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            if source.is_null() {
                start_tx
                    .send(Err(
                        "CFMachPortCreateRunLoopSource returned null".to_string()
                    ))
                    .ok();
                CFMachPortInvalidate(tap);
                CFRelease(tap.cast());
                drop(Box::from_raw(user_info.cast::<Sender<LeftMouseEvent>>()));
                return;
            }
            CGEventTapEnable(tap, true);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
            CFRelease(source.cast());
            start_tx.send(Ok(())).ok();
            CFRunLoopRun();
            CFMachPortInvalidate(tap);
            CFRelease(tap.cast());
            drop(Box::from_raw(user_info.cast::<Sender<LeftMouseEvent>>()));
        })
        .map_err(|error| format!("failed to start left mouse event monitor: {error}"))?;
    start_rx
        .recv_timeout(Duration::from_secs(1))
        .unwrap_or_else(|_| Err("left mouse event monitor did not start".to_string()))
}

#[cfg(target_os = "macos")]
unsafe extern "C" fn left_mouse_event_tap_callback(
    _proxy: *mut c_void,
    event_type: u32,
    event: *mut c_void,
    user_info: *mut c_void,
) -> *mut c_void {
    if user_info.is_null() || event.is_null() {
        return event;
    }
    let kind = match event_type {
        CG_EVENT_LEFT_MOUSE_DOWN => LeftMouseEventKind::Down,
        CG_EVENT_LEFT_MOUSE_UP => LeftMouseEventKind::Up,
        _ => return event,
    };
    let point = CGEventGetLocation(event.cast_const());
    let tx = &*(user_info.cast::<Sender<LeftMouseEvent>>());
    tx.send(LeftMouseEvent {
        kind,
        x: point.x,
        y: point.y,
    })
    .ok();
    event
}

#[cfg(not(target_os = "macos"))]
fn native_control_key_is_down() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
fn native_left_mouse_button_is_down() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
fn native_start_left_mouse_event_monitor(_tx: Sender<LeftMouseEvent>) -> Result<(), String> {
    Err("left mouse event monitor is only available on macOS".to_string())
}

#[cfg(not(target_os = "macos"))]
fn native_cursor_state() -> CursorState {
    CursorState {
        x: 0.0,
        y: 0.0,
        visible: false,
        included_in_frame: false,
    }
}

#[cfg(target_os = "macos")]
fn native_window_list() -> anyhow::Result<Vec<WindowInfo>> {
    let array = unsafe { CGWindowListCopyWindowInfo(CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY, 0) };
    if array.is_null() {
        return Ok(Vec::new());
    }
    let count = unsafe { CFArrayGetCount(array) };
    let mut windows = Vec::new();
    let mut focused_assigned = false;
    for index in 0..count {
        let dict = unsafe { CFArrayGetValueAtIndex(array, index) };
        if dict.is_null() {
            continue;
        }
        let layer = cf_i64(dict, unsafe { kCGWindowLayer }.cast()).unwrap_or_default();
        let Some(bounds_dict) = cf_value(dict, unsafe { kCGWindowBounds }.cast()) else {
            continue;
        };
        let mut rect = CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize {
                width: 0.0,
                height: 0.0,
            },
        };
        if !unsafe { CGRectMakeWithDictionaryRepresentation(bounds_dict.cast(), &mut rect) } {
            continue;
        }
        if rect.size.width <= 0.0 || rect.size.height <= 0.0 {
            continue;
        }
        let id = cf_i64(dict, unsafe { kCGWindowNumber }.cast())
            .map(|value| value.to_string())
            .unwrap_or_else(|| format!("window-{index}"));
        let focused = !focused_assigned;
        focused_assigned |= focused;
        windows.push(WindowInfo {
            id,
            app_name: cf_string(dict, unsafe { kCGWindowOwnerName }.cast()),
            title: cf_string(dict, unsafe { kCGWindowName }.cast()),
            layer: layer.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            x: rect.origin.x.round() as i32,
            y: rect.origin.y.round() as i32,
            width: rect.size.width.round().max(0.0) as u32,
            height: rect.size.height.round().max(0.0) as u32,
            focused,
        });
    }
    unsafe { CFRelease(array.cast()) };
    Ok(windows)
}

#[cfg(not(target_os = "macos"))]
fn native_window_list() -> anyhow::Result<Vec<WindowInfo>> {
    anyhow::bail!("macOS window backend is only available on macOS")
}

#[cfg(target_os = "macos")]
fn cf_value(
    dict: *const std::ffi::c_void,
    key: *const std::ffi::c_void,
) -> Option<*const std::ffi::c_void> {
    let mut value = std::ptr::null();
    let found = unsafe { CFDictionaryGetValueIfPresent(dict, key, &mut value) };
    found.then_some(value).filter(|value| !value.is_null())
}

#[cfg(target_os = "macos")]
fn cf_i64(dict: *const std::ffi::c_void, key: *const std::ffi::c_void) -> Option<i64> {
    let value = cf_value(dict, key)?;
    let mut out = 0_i64;
    unsafe {
        CFNumberGetValue(
            value,
            K_CF_NUMBER_SINT64_TYPE,
            (&mut out as *mut i64).cast(),
        )
    }
    .then_some(out)
}

#[cfg(target_os = "macos")]
fn cf_string(dict: *const std::ffi::c_void, key: *const std::ffi::c_void) -> Option<String> {
    let value = cf_value(dict, key)?;
    let direct = unsafe { CFStringGetCStringPtr(value, K_CF_STRING_ENCODING_UTF8) };
    if !direct.is_null() {
        return Some(
            unsafe { CStr::from_ptr(direct) }
                .to_string_lossy()
                .into_owned(),
        );
    }
    let len = unsafe { CFStringGetLength(value) };
    let capacity = (len.saturating_mul(4) + 1).max(1) as usize;
    let mut buffer = vec![0_i8; capacity];
    let ok = unsafe {
        CFStringGetCString(
            value,
            buffer.as_mut_ptr(),
            buffer.len() as isize,
            K_CF_STRING_ENCODING_UTF8,
        )
    };
    ok.then(|| {
        unsafe { CStr::from_ptr(buffer.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    })
}

#[cfg(not(target_os = "macos"))]
fn native_displays() -> anyhow::Result<Vec<DisplayInfo>> {
    anyhow::bail!("macOS display backend is only available on macOS")
}

#[cfg(target_os = "macos")]
fn post_mouse_move(x: i32, y: i32) -> Result<String, String> {
    let before = native_cursor_state();
    post_mouse_event(CG_EVENT_MOUSE_MOVED, x, y, CG_MOUSE_BUTTON_LEFT)?;
    let deadline = Instant::now() + Duration::from_millis(120);
    loop {
        let cursor = native_cursor_state();
        if point_near(cursor.x, cursor.y, x as f64, y as f64, 2.0) {
            return Ok("mouse move delivered through CGEvent with cursor readback".to_string());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "CGEvent mouse move was not observed; cursor stayed at {:.0},{:.0} after targeting {x},{y}",
                before.x, before.y
            ));
        }
        std::thread::sleep(Duration::from_millis(4));
    }
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
    for click_index in 0..count.max(1) {
        let click_state = u8::min(click_index + 1, 2);
        post_mouse_click_event(down, x, y, native_button, click_state)?;
        std::thread::sleep(Duration::from_millis(12));
        post_mouse_click_event(up, x, y, native_button, click_state)?;
        std::thread::sleep(Duration::from_millis(24));
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
    post_mouse_click_event(event_type, x, y, button, 1)
}

#[cfg(target_os = "macos")]
fn post_mouse_click_event(
    event_type: u32,
    x: i32,
    y: i32,
    button: u32,
    click_state: u8,
) -> Result<(), String> {
    ensure_accessibility_trusted()?;
    let point = CGPoint {
        x: x as f64,
        y: y as f64,
    };
    let event = unsafe { CGEventCreateMouseEvent(std::ptr::null(), event_type, point, button) };
    if event.is_null() {
        return Err("CGEventCreateMouseEvent returned null".to_string());
    }
    unsafe {
        CGEventSetIntegerValueField(event, K_CG_MOUSE_EVENT_CLICK_STATE, click_state as i64);
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
    ensure_accessibility_trusted()?;
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
    ensure_accessibility_trusted()?;
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

#[cfg(target_os = "macos")]
fn ensure_accessibility_trusted() -> Result<(), String> {
    if unsafe { AXIsProcessTrusted() } {
        Ok(())
    } else {
        Err("macOS Accessibility permission is required for CGEvent input".to_string())
    }
}

#[cfg(target_os = "macos")]
fn point_near(x: f64, y: f64, target_x: f64, target_y: f64, tolerance: f64) -> bool {
    (x - target_x).abs() <= tolerance && (y - target_y).abs() <= tolerance
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
fn native_request_screen_recording_access() -> PermissionState {
    if screen_recording_permission() == PermissionState::Granted {
        return PermissionState::Granted;
    }
    if unsafe { CGRequestScreenCaptureAccess() } {
        PermissionState::Granted
    } else {
        PermissionState::Missing
    }
}

#[cfg(not(target_os = "macos"))]
fn native_request_screen_recording_access() -> PermissionState {
    PermissionState::NotApplicable
}

#[cfg(target_os = "macos")]
fn native_input_monitoring_permission() -> PermissionState {
    match unsafe { IOHIDCheckAccess(K_IOHID_REQUEST_TYPE_LISTEN_EVENT) } {
        K_IOHID_ACCESS_TYPE_GRANTED => PermissionState::Granted,
        K_IOHID_ACCESS_TYPE_DENIED => PermissionState::Denied,
        K_IOHID_ACCESS_TYPE_UNKNOWN => PermissionState::Missing,
        _ => PermissionState::Unknown,
    }
}

#[cfg(not(target_os = "macos"))]
fn native_input_monitoring_permission() -> PermissionState {
    PermissionState::NotApplicable
}

#[cfg(target_os = "macos")]
fn native_request_accessibility_input_access() -> PermissionState {
    if unsafe { AXIsProcessTrusted() } {
        return PermissionState::Granted;
    }
    let key = unsafe { kAXTrustedCheckOptionPrompt };
    let value = unsafe { kCFBooleanTrue };
    let keys = [key];
    let values = [value];
    let options = unsafe {
        CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if options.is_null() {
        return accessibility_permission();
    }
    let trusted = unsafe { AXIsProcessTrustedWithOptions(options) };
    unsafe {
        CFRelease(options.cast());
    }
    if trusted {
        PermissionState::Granted
    } else {
        accessibility_permission()
    }
}

#[cfg(not(target_os = "macos"))]
fn native_request_accessibility_input_access() -> PermissionState {
    PermissionState::NotApplicable
}

#[cfg(target_os = "macos")]
fn native_microphone_permission() -> PermissionState {
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

    let Some(media_type) = (unsafe { AVMediaTypeAudio }) else {
        return PermissionState::Unknown;
    };
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(media_type) } {
        AVAuthorizationStatus::Authorized => PermissionState::Granted,
        AVAuthorizationStatus::Denied | AVAuthorizationStatus::Restricted => {
            PermissionState::Denied
        }
        AVAuthorizationStatus::NotDetermined => PermissionState::Missing,
        _ => PermissionState::Unknown,
    }
}

#[cfg(not(target_os = "macos"))]
fn native_microphone_permission() -> PermissionState {
    PermissionState::NotApplicable
}

#[cfg(target_os = "macos")]
fn native_request_microphone_access() -> PermissionState {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_av_foundation::{AVCaptureDevice, AVMediaTypeAudio};
    use std::sync::mpsc;
    use std::time::Duration;

    if native_microphone_permission() == PermissionState::Granted {
        return PermissionState::Granted;
    }
    let Some(media_type) = (unsafe { AVMediaTypeAudio }) else {
        return PermissionState::Unknown;
    };
    let (tx, rx) = mpsc::channel();
    let block = RcBlock::new(move |granted: Bool| {
        let _ = tx.send(granted.as_bool());
    });
    unsafe {
        AVCaptureDevice::requestAccessForMediaType_completionHandler(media_type, &block);
    }
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(true) => PermissionState::Granted,
        Ok(false) => native_microphone_permission(),
        Err(_) => native_microphone_permission(),
    }
}

#[cfg(not(target_os = "macos"))]
fn native_request_microphone_access() -> PermissionState {
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
    fn CGRequestScreenCaptureAccess() -> bool;
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const std::ffi::c_void) -> bool;
    fn CGMainDisplayID() -> u32;
    fn CGEventCreate(source: *const std::ffi::c_void) -> *const std::ffi::c_void;
    fn CGEventGetLocation(event: *const std::ffi::c_void) -> CGPoint;
    fn CGEventGetFlags(event: *const std::ffi::c_void) -> u64;
    fn CGEventSourceKeyState(state_id: i32, virtual_key: u16) -> bool;
    fn CGEventSourceButtonState(state_id: i32, button: u32) -> bool;
    fn CGDisplayCreateImage(display: u32) -> *const std::ffi::c_void;
    fn CGWindowListCopyWindowInfo(option: u32, relative_to_window: u32) -> *const std::ffi::c_void;
    fn CGRectMakeWithDictionaryRepresentation(
        dict: *const std::ffi::c_void,
        rect: *mut CGRect,
    ) -> bool;
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
    fn CGEventSetIntegerValueField(event: *const std::ffi::c_void, field: u32, value: i64);
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: unsafe extern "C" fn(
            proxy: *mut c_void,
            event_type: u32,
            event: *mut c_void,
            user_info: *mut c_void,
        ) -> *mut c_void,
        user_info: *mut c_void,
    ) -> *mut std::ffi::c_void;
    fn CGEventTapEnable(tap: *const std::ffi::c_void, enable: bool);
    fn CFMachPortCreateRunLoopSource(
        allocator: *const std::ffi::c_void,
        port: *const std::ffi::c_void,
        order: isize,
    ) -> *const std::ffi::c_void;
    fn CFMachPortInvalidate(port: *const std::ffi::c_void);
    fn CFRunLoopGetCurrent() -> *const std::ffi::c_void;
    fn CFRunLoopAddSource(
        run_loop: *const std::ffi::c_void,
        source: *const std::ffi::c_void,
        mode: *const std::ffi::c_void,
    );
    fn CFRunLoopRun();
    fn CFDataGetBytePtr(data: *const std::ffi::c_void) -> *const u8;
    fn CFDataGetLength(data: *const std::ffi::c_void) -> usize;
    fn CFArrayGetCount(array: *const std::ffi::c_void) -> isize;
    fn CFArrayGetValueAtIndex(
        array: *const std::ffi::c_void,
        index: isize,
    ) -> *const std::ffi::c_void;
    fn CFDictionaryGetValueIfPresent(
        dict: *const std::ffi::c_void,
        key: *const std::ffi::c_void,
        value: *mut *const std::ffi::c_void,
    ) -> bool;
    fn CFDictionaryCreate(
        allocator: *const std::ffi::c_void,
        keys: *const *const std::ffi::c_void,
        values: *const *const std::ffi::c_void,
        num_values: isize,
        key_callbacks: *const std::ffi::c_void,
        value_callbacks: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;
    fn CFNumberGetValue(
        number: *const std::ffi::c_void,
        the_type: i32,
        value: *mut std::ffi::c_void,
    ) -> bool;
    fn CFStringGetCStringPtr(string: *const std::ffi::c_void, encoding: u32) -> *const c_char;
    fn CFStringGetCString(
        string: *const std::ffi::c_void,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
    fn CFStringGetLength(string: *const std::ffi::c_void) -> isize;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CFRunLoopRunInMode(
        mode: *const std::ffi::c_void,
        seconds: f64,
        return_after_source_handled: bool,
    ) -> i32;
    fn CFRelease(cf: *const std::ffi::c_void);

    static kCFRunLoopDefaultMode: *const std::ffi::c_void;
    static kCFRunLoopCommonModes: *const std::ffi::c_void;
    static kCFBooleanTrue: *const std::ffi::c_void;
    static kAXTrustedCheckOptionPrompt: *const std::ffi::c_void;
    static kCGWindowNumber: *const std::ffi::c_void;
    static kCGWindowOwnerName: *const std::ffi::c_void;
    static kCGWindowName: *const std::ffi::c_void;
    static kCGWindowBounds: *const std::ffi::c_void;
    static kCGWindowLayer: *const std::ffi::c_void;
}

#[cfg(target_os = "macos")]
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDCheckAccess(request_type: u32) -> i32;
}

#[cfg(target_os = "macos")]
const K_IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;
#[cfg(target_os = "macos")]
const K_IOHID_ACCESS_TYPE_GRANTED: i32 = 0;
#[cfg(target_os = "macos")]
const K_IOHID_ACCESS_TYPE_DENIED: i32 = 1;
#[cfg(target_os = "macos")]
const K_IOHID_ACCESS_TYPE_UNKNOWN: i32 = 2;

const K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE: i32 = 1;
#[cfg(target_os = "macos")]
const K_CG_MOUSE_EVENT_CLICK_STATE: u32 = 1;
const K_VK_CONTROL: u16 = 0x3B;
const K_VK_RIGHT_CONTROL: u16 = 0x3E;

#[cfg(target_os = "macos")]
fn current_event_flags_include(mask: u64) -> bool {
    let event = unsafe { CGEventCreate(std::ptr::null()) };
    if event.is_null() {
        return false;
    }
    let flags = unsafe { CGEventGetFlags(event) };
    unsafe { CFRelease(event.cast()) };
    flags & mask != 0
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
const CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
#[cfg(target_os = "macos")]
const CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
#[cfg(target_os = "macos")]
const CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1;
#[cfg(target_os = "macos")]
const K_CF_NUMBER_SINT64_TYPE: i32 = 4;
#[cfg(target_os = "macos")]
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
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
        assert_ne!(report.input_monitoring, PermissionState::Unknown);
    }

    #[test]
    fn capture_backend_selection_never_fabricates_macos_frames() {
        let backend = capture_backend_or_unavailable();
        assert!(matches!(backend.name(), "macos" | "unavailable"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn native_display_capture_smoke() {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = capture_main_display(
                Instant::now(),
                CaptureRequest {
                    max_width: Some(640),
                    encoding: cua_core::FrameEncoding::Png,
                    force_fresh: true,
                },
            )
            .map(|frame| frame.envelope);
            let _ = sender.send(result);
        });
        let envelope = receiver
            .recv_timeout(sck_capture_timeout() + Duration::from_secs(1))
            .expect("native display capture timed out")
            .expect("native display capture failed");
        assert!(envelope.width > 0);
        assert!(envelope.height > 0);
        assert!(envelope.byte_len > 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn scaled_bgra_source_samples_directly_to_target_size() {
        let source = [
            10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255,
        ];
        let image = scaled_bgra_source_to_rgba(&source, 8, 2, 2, 1, 1).unwrap();

        assert_eq!(image.width(), 1);
        assert_eq!(image.height(), 1);
        assert_eq!(image.as_raw(), &[30, 20, 10, 255]);
    }

    #[test]
    fn input_backend_selection_is_available() {
        let backend = input_backend();
        assert_eq!(backend.name(), "macos-cgevent");
    }

    #[test]
    fn compact_command_output_bounds_long_text() {
        let long = "x ".repeat(1_000);
        let compact = compact_command_output(long.as_bytes());

        assert!(compact.chars().count() <= 1_203);
        assert!(compact.ends_with("..."));
    }

    #[test]
    fn sequence_message_reports_bounded_leaf_evidence() {
        let message = sequence_message(
            2,
            &[
                "opened app Messages".to_string(),
                format!("shell exited 0; stdout={}; stderr=", "x ".repeat(200)),
            ],
        );

        assert!(
            message.starts_with("sequence posted 2 actions: opened app Messages | shell exited 0")
        );
        assert!(message.chars().count() <= 1_200);
    }

    #[tokio::test]
    async fn shell_exec_uses_system_route() {
        let backend = MacosInputBackend;
        let result = backend
            .execute(InputRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                idempotency_key: uuid::Uuid::new_v4(),
                deadline_mono_ns: None,
                action: InputAction::ShellExec {
                    command: "printf cua-shell-ok".to_string(),
                    timeout_ms: 2_000,
                },
            })
            .await;

        assert_eq!(result.effect, Effect::Confirmed);
        assert_eq!(result.route, InputRoute::SystemApi);
        assert_eq!(result.delivery_mode, DeliveryMode::Background);
        assert!(result.evidence[0].message.contains("cua-shell-ok"));
    }

    #[tokio::test]
    async fn sequence_reports_leaf_evidence() {
        let backend = MacosInputBackend;
        let result = backend
            .execute(InputRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                idempotency_key: uuid::Uuid::new_v4(),
                deadline_mono_ns: None,
                action: InputAction::Sequence {
                    actions: vec![
                        InputAction::ShellExec {
                            command: "printf one".to_string(),
                            timeout_ms: 2_000,
                        },
                        InputAction::ShellExec {
                            command: "printf two".to_string(),
                            timeout_ms: 2_000,
                        },
                    ],
                    inter_action_delay_ms: 0,
                },
            })
            .await;

        assert_eq!(result.effect, Effect::Confirmed);
        assert_eq!(result.route, InputRoute::SystemApi);
        assert!(result.evidence[0]
            .message
            .contains("sequence posted 2 actions"));
        assert!(result.evidence[0].message.contains("stdout=one"));
        assert!(result.evidence[0].message.contains("stdout=two"));
    }

    #[test]
    fn native_cursor_observation_is_finite() {
        let cursor = cursor_state();
        assert!(cursor.x.is_finite());
        assert!(cursor.y.is_finite());
    }

    #[test]
    fn native_control_key_state_is_observable() {
        let _ = control_key_is_down();
    }

    #[test]
    fn native_left_mouse_button_state_is_observable() {
        let _ = left_mouse_button_is_down();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn current_event_flag_query_is_observable() {
        let _ = current_event_flags_include(CG_EVENT_FLAG_MASK_CONTROL);
    }

    #[test]
    fn native_window_observation_has_valid_geometry() {
        let windows = window_list().unwrap();
        assert!(windows
            .iter()
            .all(|window| window.width > 0 && window.height > 0));
    }
}
