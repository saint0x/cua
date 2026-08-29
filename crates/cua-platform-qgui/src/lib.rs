//! qgui-backed Linux computer backend.
//!
//! qgui owns the X11/KasmVNC/XFCE session. This crate adapts that session into
//! the backend-neutral CUA computer contract using ordinary Linux GUI tools.

use anyhow::{anyhow, bail, Context};
use async_trait::async_trait;
use cua_capture::{
    encode_image, CaptureBackend, CaptureRequest, CaptureSource, CapturedFrame,
    CapturedFrameTimings,
};
use cua_computer::ComputerBackend;
use cua_core::{
    now_wall_ms, CapabilityManifest, ComputerBackendDescriptor, ComputerBackendKind, CursorState,
    DeliveryMode, DisplayInfo, Effect, Evidence, EvidenceKind, FrameEnvelope, InputAction,
    InputRequest, InputResult, InputRoute, MouseButton, PermissionReport, PermissionState, Rect,
    WindowInfo, SCHEMA_VERSION,
};
use cua_input::InputBackend;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const DEFAULT_SESSION_PATH: &str = "/run/qgui/session.json";

#[derive(Debug, Clone)]
pub struct QguiBackendConfig {
    pub kind: ComputerBackendKind,
    pub provider: String,
    pub instance_id: Option<String>,
    pub pool_id: Option<String>,
    pub region: Option<String>,
    pub session_path: PathBuf,
    pub qgui_binary: String,
    pub capture_binary: String,
    pub input_binary: String,
    pub window_binary: String,
    pub clipboard_binary: String,
}

impl QguiBackendConfig {
    pub fn from_env(kind: ComputerBackendKind, provider: impl Into<String>) -> Self {
        Self {
            kind,
            provider: provider.into(),
            instance_id: std::env::var("CUA_QGUI_INSTANCE_ID")
                .ok()
                .or_else(|| std::env::var("OCI_INSTANCE_ID").ok()),
            pool_id: std::env::var("CUA_QGUI_POOL_ID").ok(),
            region: std::env::var("CUA_QGUI_REGION")
                .ok()
                .or_else(|| std::env::var("OCI_REGION").ok()),
            session_path: std::env::var("QGUI_SESSION_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(DEFAULT_SESSION_PATH)),
            qgui_binary: std::env::var("QGUI_BIN").unwrap_or_else(|_| "qgui".to_string()),
            capture_binary: std::env::var("CUA_QGUI_CAPTURE_BIN")
                .unwrap_or_else(|_| "cua-qgui-tool".to_string()),
            input_binary: std::env::var("CUA_QGUI_INPUT_BIN")
                .unwrap_or_else(|_| "cua-qgui-tool".to_string()),
            window_binary: std::env::var("CUA_QGUI_WINDOW_BIN")
                .unwrap_or_else(|_| "cua-qgui-tool".to_string()),
            clipboard_binary: std::env::var("CUA_QGUI_CLIPBOARD_BIN")
                .unwrap_or_else(|_| "cua-qgui-tool".to_string()),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct QguiSessionState {
    display: String,
    res: String,
    depth: u16,
    backend_bind: String,
    backend_port: u16,
    dbus_addr: String,
    dbus_socket: String,
    xdg_runtime_dir: String,
    auth_username: String,
    auth_password: String,
}

#[derive(Debug, Clone)]
pub struct QguiComputerBackend {
    config: Arc<QguiBackendConfig>,
    input: Arc<QguiInputBackend>,
}

impl QguiComputerBackend {
    pub fn new(config: QguiBackendConfig) -> Self {
        let config = Arc::new(config);
        Self {
            input: Arc::new(QguiInputBackend::new(config.clone())),
            config,
        }
    }

    fn session(&self) -> anyhow::Result<QguiSessionState> {
        read_session(&self.config.session_path)
    }

    async fn health(&self) -> QguiHealth {
        let tool = command_exists(&self.config.input_binary).await;
        QguiHealth {
            session: self.session().is_ok(),
            capture_tool: tool || command_exists(&self.config.capture_binary).await,
            input_tool: tool,
            window_tool: tool || command_exists(&self.config.window_binary).await,
            clipboard_tool: tool || command_exists(&self.config.clipboard_binary).await,
        }
    }
}

#[derive(Debug, Clone)]
struct QguiHealth {
    session: bool,
    capture_tool: bool,
    input_tool: bool,
    window_tool: bool,
    clipboard_tool: bool,
}

#[async_trait]
impl ComputerBackend for QguiComputerBackend {
    fn descriptor(&self) -> ComputerBackendDescriptor {
        ComputerBackendDescriptor {
            kind: self.config.kind.clone(),
            provider: self.config.provider.clone(),
            runtime: "qgui+cua".to_string(),
            instance_id: self.config.instance_id.clone(),
            pool_id: self.config.pool_id.clone(),
            region: self.config.region.clone(),
            os: "linux".to_string(),
            capabilities: qgui_capabilities(),
        }
    }

    fn capture_backend(&self) -> Arc<dyn CaptureBackend> {
        Arc::new(QguiCaptureBackend {
            config: self.config.clone(),
            started: Instant::now(),
        })
    }

    fn input_backend(&self) -> Arc<dyn InputBackend> {
        self.input.clone()
    }

    async fn permission_report(&self) -> PermissionReport {
        let health = self.health().await;
        PermissionReport {
            screen_recording: granted_if(health.session && health.capture_tool),
            accessibility_input: granted_if(health.session && health.input_tool),
            input_monitoring: PermissionState::NotApplicable,
            automation: granted_if(health.session && health.window_tool),
            clipboard: granted_if(health.session && health.clipboard_tool),
            portal: PermissionState::NotApplicable,
        }
    }

    async fn request_accessibility_input_access(&self) -> PermissionState {
        self.permission_report().await.accessibility_input
    }

    async fn cursor_state(&self) -> CursorState {
        let Ok(session) = self.session() else {
            return hidden_cursor();
        };
        let output = gui_command(&self.config, &session, &self.config.input_binary)
            .arg("cursor-json")
            .output()
            .await;
        match output {
            Ok(output) if output.status.success() => parse_cursor_json(&output.stdout),
            _ => hidden_cursor(),
        }
    }

    async fn window_list(&self) -> anyhow::Result<Vec<WindowInfo>> {
        let session = self.session()?;
        let output = gui_command(&self.config, &session, &self.config.window_binary)
            .arg("windows-json")
            .output()
            .await
            .context("run wmctrl -lG")?;
        if !output.status.success() {
            bail!("wmctrl -lG failed: {}", stderr_text(&output.stderr));
        }
        serde_json::from_slice(&output.stdout).context("decode cua-qgui-tool windows-json")
    }
}

#[derive(Debug)]
struct QguiCaptureBackend {
    config: Arc<QguiBackendConfig>,
    started: Instant,
}

#[async_trait]
impl CaptureBackend for QguiCaptureBackend {
    async fn capture_latest(&self, request: CaptureRequest) -> anyhow::Result<CapturedFrame> {
        let capture_started = Instant::now();
        let session = read_session(&self.config.session_path)?;
        let output = gui_command(&self.config, &session, &self.config.capture_binary)
            .arg("capture-png")
            .output()
            .await
            .context("capture qgui root window with bundled cua-qgui-tool")?;
        if !output.status.success() {
            bail!("qgui capture failed: {}", stderr_text(&output.stderr));
        }
        let raw_png = output.stdout;
        let image = image::load_from_memory(&raw_png)
            .context("decode qgui capture png")?
            .to_rgba8();
        let (display_width, display_height) =
            parse_resolution(&session.res).unwrap_or(image.dimensions());
        let encode_started = Instant::now();
        let bytes = encode_image(&image, request.encoding.clone())?;
        let encode_ns = elapsed_ns(encode_started);
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        let byte_len = bytes.len();
        let cursor = QguiComputerBackend::new((*self.config).clone())
            .cursor_state()
            .await;
        let mut frame = CapturedFrame {
            envelope: FrameEnvelope {
                schema_version: SCHEMA_VERSION.to_string(),
                frame_id: self.started.elapsed().as_millis() as u64,
                timestamp_mono_ns: self.started.elapsed().as_nanos(),
                timestamp_wall_ms: now_wall_ms(),
                display_id: session.display.clone(),
                display_x: 0,
                display_y: 0,
                display_width,
                display_height,
                frame_origin_x: 0,
                frame_origin_y: 0,
                width: image.width(),
                height: image.height(),
                scale_factor: 1.0,
                pixel_format: "rgba8".to_string(),
                encoding: request.encoding.clone(),
                byte_len,
                sha256,
                cursor,
                damage_rects: vec![Rect {
                    x: 0,
                    y: 0,
                    width: image.width(),
                    height: image.height(),
                }],
            },
            bytes: Arc::new(bytes),
            timings: CapturedFrameTimings {
                capture_ns: elapsed_ns(capture_started),
                encode_ns,
                source: CaptureSource::Resident,
            },
        };
        frame = frame.transformed(&request)?;
        Ok(frame)
    }

    async fn displays(&self) -> anyhow::Result<Vec<DisplayInfo>> {
        let session = read_session(&self.config.session_path)?;
        let (width, height) = parse_resolution(&session.res).unwrap_or((0, 0));
        Ok(vec![DisplayInfo {
            id: session.display,
            name: "qgui display".to_string(),
            x: 0,
            y: 0,
            width,
            height,
            scale_factor: 1.0,
            active: width > 0 && height > 0,
        }])
    }

    fn name(&self) -> &'static str {
        "qgui"
    }
}

struct QguiInputBackend {
    config: Arc<QguiBackendConfig>,
    clipboard_owner: Mutex<Option<Child>>,
}

impl std::fmt::Debug for QguiInputBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QguiInputBackend")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl QguiInputBackend {
    fn new(config: Arc<QguiBackendConfig>) -> Self {
        Self {
            config,
            clipboard_owner: Mutex::new(None),
        }
    }
}

#[async_trait]
impl InputBackend for QguiInputBackend {
    async fn execute(&self, request: InputRequest) -> InputResult {
        let started = Instant::now();
        let result = self.execute_action(request.action).await;
        match result {
            Ok(message) => input_result(
                request.idempotency_key,
                started,
                Effect::Confirmed,
                EvidenceKind::ValueReadback,
                message,
            ),
            Err(error) => input_result(
                request.idempotency_key,
                started,
                Effect::Refused,
                EvidenceKind::Refusal,
                error.to_string(),
            ),
        }
    }

    fn name(&self) -> &'static str {
        "qgui"
    }
}

impl QguiInputBackend {
    async fn execute_action(&self, action: InputAction) -> anyhow::Result<String> {
        let session = read_session(&self.config.session_path)?;
        match action {
            InputAction::MouseMove { x, y, duration_ms } => {
                run_checked(
                    gui_command(&self.config, &session, &self.config.input_binary)
                        .arg("mouse-move")
                        .arg(x.to_string())
                        .arg(y.to_string())
                        .arg(duration_ms.to_string()),
                )
                .await
            }
            InputAction::MouseClick {
                x,
                y,
                button,
                count,
            } => {
                run_checked(
                    gui_command(&self.config, &session, &self.config.input_binary)
                        .arg("mouse-click")
                        .arg(x.to_string())
                        .arg(y.to_string())
                        .arg(mouse_button_name(button))
                        .arg(count.max(1).to_string()),
                )
                .await
            }
            InputAction::MouseDrag {
                from_x,
                from_y,
                to_x,
                to_y,
                duration_ms,
            } => {
                run_checked(
                    gui_command(&self.config, &session, &self.config.input_binary)
                        .arg("mouse-drag")
                        .arg(from_x.to_string())
                        .arg(from_y.to_string())
                        .arg(to_x.to_string())
                        .arg(to_y.to_string())
                        .arg(duration_ms.to_string()),
                )
                .await
            }
            InputAction::KeyPress { combo } => {
                run_checked(
                    gui_command(&self.config, &session, &self.config.input_binary)
                        .arg("key")
                        .arg(combo),
                )
                .await
            }
            InputAction::KeyType { text } => {
                if requires_clipboard_text_path(&text) {
                    self.paste_text(&session, text, "qgui text inserted through clipboard paste")
                        .await
                } else {
                    run_checked(
                        gui_command(&self.config, &session, &self.config.input_binary)
                            .arg("type")
                            .arg(text),
                    )
                    .await
                }
            }
            InputAction::KeyPaste { text } => {
                self.paste_text(&session, text, "qgui paste delivered through clipboard")
                    .await
            }
            InputAction::Sequence {
                actions,
                inter_action_delay_ms,
            } => {
                for action in actions {
                    Box::pin(self.execute_action(action)).await?;
                    if inter_action_delay_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(inter_action_delay_ms))
                            .await;
                    }
                }
                Ok("qgui sequence executed".to_string())
            }
            InputAction::OpenApp { app_name } => {
                run_checked(
                    qgui_run_command(&self.config, &session)
                        .arg("sh")
                        .arg("-lc")
                        .arg(format!("{app_name} >/tmp/qgui-open-app.log 2>&1 &")),
                )
                .await
            }
            InputAction::ShellExec {
                command,
                timeout_ms,
            } => {
                run_checked_with_timeout(
                    qgui_run_command(&self.config, &session)
                        .arg("sh")
                        .arg("-lc")
                        .arg(command),
                    timeout_ms,
                )
                .await
            }
            InputAction::Aegis { args, timeout_ms } => {
                run_checked_with_timeout(
                    qgui_run_command(&self.config, &session)
                        .arg("aegis")
                        .args(args),
                    timeout_ms,
                )
                .await
            }
            InputAction::Ctx {
                args,
                timeout_ms,
                workspace_root,
            } => {
                let mut command = qgui_run_command(&self.config, &session);
                if let Some(workspace_root) = workspace_root {
                    command.current_dir(workspace_root);
                }
                run_checked_with_timeout(command.arg("ctx").args(args), timeout_ms).await
            }
            InputAction::ClipboardRead { allow_sensitive } => {
                if !allow_sensitive {
                    bail!("clipboard read requires allow_sensitive=true");
                }
                run_checked(
                    gui_command(&self.config, &session, &self.config.clipboard_binary)
                        .arg("clipboard-read"),
                )
                .await
            }
            InputAction::ClipboardWrite { text } => {
                self.install_clipboard_owner(&session, &text).await?;
                Ok("qgui clipboard owner updated".to_string())
            }
            InputAction::Pause | InputAction::Resume | InputAction::KillSwitch => {
                let mut owner = self.clipboard_owner.lock().await;
                if let Some(mut child) = owner.take() {
                    let _ = child.kill().await;
                }
                Ok("safety action accepted by qgui coordinator".to_string())
            }
        }
    }

    async fn paste_text(
        &self,
        session: &QguiSessionState,
        text: String,
        message: &'static str,
    ) -> anyhow::Result<String> {
        self.install_clipboard_owner(session, &text).await?;
        run_checked(
            gui_command(&self.config, session, &self.config.input_binary)
                .arg("key")
                .arg("Control+v"),
        )
        .await?;
        Ok(message.to_string())
    }

    async fn install_clipboard_owner(
        &self,
        session: &QguiSessionState,
        text: &str,
    ) -> anyhow::Result<()> {
        let mut command = gui_command(&self.config, session, &self.config.clipboard_binary);
        command.arg("clipboard-serve");
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn bundled cua-qgui-tool clipboard owner")?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(text.as_bytes())
                .await
                .context("write clipboard stdin")?;
        }
        let stdout = child
            .stdout
            .take()
            .context("clipboard owner stdout unavailable")?;
        let mut ready = String::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            BufReader::new(stdout).read_line(&mut ready),
        )
        .await
        .map_err(|_| anyhow!("timed out waiting for clipboard owner readiness"))?
        .context("read clipboard owner readiness")?;
        if read == 0 || ready.trim() != "ready" {
            let _ = child.kill().await;
            bail!("clipboard owner did not become ready");
        }
        let mut owner = self.clipboard_owner.lock().await;
        if let Some(mut old_owner) = owner.take() {
            let _ = old_owner.kill().await;
        }
        *owner = Some(child);
        Ok(())
    }
}

fn requires_clipboard_text_path(text: &str) -> bool {
    !text.is_ascii()
}

fn qgui_capabilities() -> CapabilityManifest {
    CapabilityManifest {
        actions: vec![
            "mouse_move".to_string(),
            "mouse_click".to_string(),
            "mouse_drag".to_string(),
            "key_press".to_string(),
            "key_type".to_string(),
            "key_paste".to_string(),
            "sequence".to_string(),
            "open_app".to_string(),
            "shell_exec".to_string(),
            "aegis".to_string(),
            "ctx".to_string(),
            "clipboard_read".to_string(),
            "clipboard_write".to_string(),
        ],
        displays: vec!["qgui".to_string()],
        apps: vec!["shell".to_string()],
        clipboard: true,
        model_egress: true,
        max_fps: 10,
    }
}

fn read_session(path: &Path) -> anyhow::Result<QguiSessionState> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read qgui session {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("decode qgui session {}", path.display()))
}

fn gui_command(config: &QguiBackendConfig, session: &QguiSessionState, program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env("DISPLAY", &session.display)
        .env("DBUS_SESSION_BUS_ADDRESS", &session.dbus_addr)
        .env("XDG_RUNTIME_DIR", &session.xdg_runtime_dir)
        .env("QT_X11_NO_MITSHM", "1")
        .env("QGUI_SESSION_PATH", &config.session_path);
    command
}

fn qgui_run_command(config: &QguiBackendConfig, session: &QguiSessionState) -> Command {
    let mut command = Command::new(&config.qgui_binary);
    command
        .arg("run")
        .arg("--")
        .env("DISPLAY", &session.display)
        .env("DBUS_SESSION_BUS_ADDRESS", &session.dbus_addr)
        .env("XDG_RUNTIME_DIR", &session.xdg_runtime_dir)
        .env("QT_X11_NO_MITSHM", "1")
        .env("QGUI_SESSION_PATH", &config.session_path);
    command
}

async fn command_exists(program: &str) -> bool {
    Command::new("sh")
        .arg("-lc")
        .arg(format!("command -v -- {}", shell_quote(program)))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn run_checked(command: &mut Command) -> anyhow::Result<String> {
    let output = command.output().await.context("run qgui desktop command")?;
    checked_output(output, "qgui desktop command")
}

async fn run_checked_with_timeout(
    command: &mut Command,
    timeout_ms: u64,
) -> anyhow::Result<String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms.clamp(100, 300_000)),
        command.output(),
    )
    .await
    .map_err(|_| anyhow!("qgui desktop command timed out after {timeout_ms}ms"))?
    .context("run timed qgui desktop command")?;
    checked_output(output, "qgui desktop command")
}

fn checked_output(output: std::process::Output, label: &str) -> anyhow::Result<String> {
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if stdout.is_empty() {
            Ok(format!("{label} completed"))
        } else {
            Ok(stdout)
        }
    } else {
        bail!("{label} failed: {}", stderr_text(&output.stderr))
    }
}

fn input_result(
    idempotency_key: uuid::Uuid,
    started: Instant,
    effect: Effect,
    kind: EvidenceKind,
    message: String,
) -> InputResult {
    InputResult {
        schema_version: SCHEMA_VERSION.to_string(),
        idempotency_key,
        effect,
        route: InputRoute::GlobalInput,
        delivery_mode: DeliveryMode::Desktop,
        started_mono_ns: 0,
        ended_mono_ns: started.elapsed().as_nanos(),
        evidence: vec![Evidence {
            kind,
            message,
            frame_id: None,
        }],
    }
}

fn granted_if(value: bool) -> PermissionState {
    if value {
        PermissionState::Granted
    } else {
        PermissionState::Missing
    }
}

fn hidden_cursor() -> CursorState {
    CursorState {
        x: 0.0,
        y: 0.0,
        visible: false,
        included_in_frame: false,
    }
}

fn parse_cursor_json(stdout: &[u8]) -> CursorState {
    serde_json::from_slice(stdout).unwrap_or_else(|_| hidden_cursor())
}

fn parse_resolution(resolution: &str) -> Option<(u32, u32)> {
    let (width, height) = resolution.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn mouse_button_name(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Middle => "middle",
        MouseButton::Right => "right",
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn stderr_text(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr).trim().to_string();
    if text.is_empty() {
        "no stderr".to_string()
    } else {
        text
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resolution() {
        assert_eq!(parse_resolution("1440x900"), Some((1440, 900)));
        assert_eq!(parse_resolution("bad"), None);
    }

    #[test]
    fn parses_cursor_shell_output() {
        let cursor =
            parse_cursor_json(br#"{"x":12.0,"y":34.0,"visible":true,"included_in_frame":false}"#);
        assert_eq!(cursor.x, 12.0);
        assert_eq!(cursor.y, 34.0);
        assert!(cursor.visible);
    }

    #[test]
    fn descriptor_uses_requested_kind_and_provider() {
        let backend = QguiComputerBackend::new(QguiBackendConfig {
            kind: ComputerBackendKind::OracleVm,
            provider: "oracle-vm".to_string(),
            instance_id: Some("vm-1".to_string()),
            pool_id: None,
            region: Some("us-ashburn-1".to_string()),
            session_path: PathBuf::from("/tmp/missing-qgui-session.json"),
            qgui_binary: "qgui".to_string(),
            capture_binary: "cua-qgui-tool".to_string(),
            input_binary: "cua-qgui-tool".to_string(),
            window_binary: "cua-qgui-tool".to_string(),
            clipboard_binary: "cua-qgui-tool".to_string(),
        });
        let descriptor = backend.descriptor();
        assert_eq!(descriptor.kind, ComputerBackendKind::OracleVm);
        assert_eq!(descriptor.provider, "oracle-vm");
        assert_eq!(descriptor.runtime, "qgui+cua");
    }

    #[test]
    fn non_ascii_text_uses_clipboard_paste_path() {
        assert!(!requires_clipboard_text_path("plain ascii text"));
        assert!(requires_clipboard_text_path("hello 🌍"));
        assert!(requires_clipboard_text_path("cafe\u{301}"));
    }
}
