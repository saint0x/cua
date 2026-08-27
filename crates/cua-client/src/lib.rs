use anyhow::{bail, Context};
use cua_core::{
    profile_socket_path, profile_token_path, ClipboardReadRequest, ClipboardResult,
    ClipboardWriteRequest, ConfigInventory, DesktopContextSnapshot, DesktopState,
    FrameActionRequest, FrameEncoding, FrameEnvelope, FramePayload, HealthReport, InputAction,
    Manifest, RuntimeControlState, RuntimeInventory, RuntimeSessionRole, SchemaBundle,
    SessionCancelRequest, SessionLeaseRequest, SessionLeaseResult, UiIslandRequest, UiIslandResult,
    UiIslandState, UiMode, UiModeRequest, UiReplyRequest, UiStepRequest, VisualSessionRequest,
    SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{
    unix::{OwnedReadHalf, OwnedWriteHalf},
    UnixStream,
};

#[derive(Debug, Clone)]
pub struct CuaClient {
    profile: String,
    token: String,
    socket_path: PathBuf,
}

impl CuaClient {
    pub async fn connect(profile: impl Into<String>) -> anyhow::Result<Self> {
        let profile = profile.into();
        let token = load_or_create_profile_token(&profile).await?;
        let socket_path = profile_socket_path(&profile)?;
        Ok(Self {
            profile,
            token,
            socket_path,
        })
    }

    pub async fn new(profile: impl Into<String>) -> anyhow::Result<Self> {
        Self::connect(profile).await
    }

    pub async fn screenshot(&self, include_bytes: bool) -> anyhow::Result<FramePayload> {
        self.request(
            "capture.screenshot",
            Some(serde_json::json!({
                "max_width": 1280,
                "encoding": FrameEncoding::Png,
                "force_fresh": true,
                "include_bytes": include_bytes
            })),
        )
        .await
    }

    pub async fn status(&self) -> anyhow::Result<HealthReport> {
        self.request("status", None).await
    }

    pub async fn manifest(&self) -> anyhow::Result<Manifest> {
        self.request("manifest", None).await
    }

    pub async fn schemas(&self) -> anyhow::Result<SchemaBundle> {
        self.request("schemas", None).await
    }

    pub async fn observe(&self) -> anyhow::Result<DesktopState> {
        self.request("observe.desktop", None).await
    }

    pub async fn config_status(&self) -> anyhow::Result<ConfigInventory> {
        self.request("config.status", None).await
    }

    pub async fn context(&self, include_bytes: bool) -> anyhow::Result<DesktopContextSnapshot> {
        self.request(
            "context.snapshot",
            Some(context_request_body(include_bytes)),
        )
        .await
    }

    pub async fn events(&self) -> anyhow::Result<Vec<Value>> {
        self.request("events.snapshot", None).await
    }

    pub async fn events_after(&self, sequence: u64) -> anyhow::Result<Vec<Value>> {
        self.request(
            "events.after",
            Some(serde_json::json!({ "after_sequence": sequence })),
        )
        .await
    }

    pub async fn events_wait(&self, sequence: u64, timeout_ms: u64) -> anyhow::Result<Vec<Value>> {
        self.request(
            "events.wait",
            Some(serde_json::json!({
                "after_sequence": sequence,
                "timeout_ms": timeout_ms
            })),
        )
        .await
    }

    pub async fn acquire_owner(
        &self,
        client_name: impl Into<String>,
        ttl_ms: Option<i64>,
    ) -> anyhow::Result<SessionLeaseResult> {
        self.acquire_session(RuntimeSessionRole::Owner, client_name, ttl_ms)
            .await
    }

    pub async fn acquire_observer(
        &self,
        client_name: impl Into<String>,
        ttl_ms: Option<i64>,
    ) -> anyhow::Result<SessionLeaseResult> {
        self.acquire_session(RuntimeSessionRole::Observer, client_name, ttl_ms)
            .await
    }

    pub async fn acquire_session(
        &self,
        role: RuntimeSessionRole,
        client_name: impl Into<String>,
        ttl_ms: Option<i64>,
    ) -> anyhow::Result<SessionLeaseResult> {
        let request = SessionLeaseRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            client_name: client_name.into(),
            role,
            ttl_ms,
        };
        self.request("session.acquire", Some(serde_json::to_value(request)?))
            .await
    }

    pub async fn cancel_session(
        &self,
        session_id: impl Into<String>,
        target_session_id: Option<String>,
    ) -> anyhow::Result<RuntimeInventory> {
        let request = SessionCancelRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: session_id.into(),
            target_session_id,
        };
        self.request("session.cancel", Some(serde_json::to_value(request)?))
            .await
    }

    pub async fn session_status(&self) -> anyhow::Result<RuntimeInventory> {
        self.request("session.status", None).await
    }

    pub async fn ui_step(
        &self,
        label: impl Into<String>,
        source: Option<String>,
        task: Option<String>,
        tool: Option<String>,
        step_index: Option<u16>,
        step_total: Option<u16>,
        ttl_ms: Option<u64>,
    ) -> anyhow::Result<Value> {
        self.request(
            "ui.step",
            Some(serde_json::to_value(UiStepRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                label: label.into(),
                source,
                task,
                tool,
                step_index,
                step_total,
                ttl_ms,
            })?),
        )
        .await
    }

    pub async fn ui_reply(
        &self,
        text: impl Into<String>,
        source: Option<String>,
        ttl_ms: Option<u64>,
    ) -> anyhow::Result<Value> {
        self.request(
            "ui.reply",
            Some(serde_json::to_value(UiReplyRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                text: text.into(),
                source,
                ttl_ms,
            })?),
        )
        .await
    }

    pub async fn ui_mode(&self, mode: UiMode, source: Option<String>) -> anyhow::Result<Value> {
        self.request(
            "ui.mode",
            Some(serde_json::to_value(UiModeRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                mode,
                source,
            })?),
        )
        .await
    }

    pub async fn ui_island(
        &self,
        state: UiIslandState,
        source: Option<String>,
    ) -> anyhow::Result<UiIslandResult> {
        self.request(
            "ui.island",
            Some(serde_json::to_value(UiIslandRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                state,
                source,
            })?),
        )
        .await
    }

    pub async fn clipboard_read(&self, allow_sensitive: bool) -> anyhow::Result<ClipboardResult> {
        self.request(
            "clipboard.read",
            Some(serde_json::to_value(ClipboardReadRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                allow_sensitive,
            })?),
        )
        .await
    }

    pub async fn clipboard_write(
        &self,
        text: impl Into<String>,
    ) -> anyhow::Result<ClipboardResult> {
        self.request(
            "clipboard.write",
            Some(serde_json::to_value(ClipboardWriteRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                text: text.into(),
            })?),
        )
        .await
    }

    pub async fn pause(&self, session_id: Option<&str>) -> anyhow::Result<RuntimeControlState> {
        self.request_with_session("control.pause", None, session_id)
            .await
    }

    pub async fn resume(&self, session_id: Option<&str>) -> anyhow::Result<RuntimeControlState> {
        self.request_with_session("control.resume", None, session_id)
            .await
    }

    pub async fn kill_switch(
        &self,
        session_id: Option<&str>,
    ) -> anyhow::Result<RuntimeControlState> {
        self.request_with_session("control.kill_switch", None, session_id)
            .await
    }

    pub async fn visual_session(
        &self,
        max_width: Option<u32>,
        fps: Option<u32>,
        include_bytes: bool,
        session_id: Option<&str>,
    ) -> anyhow::Result<CuaVisualSession> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connect {}", self.socket_path.display()))?;
        let (read, mut write) = stream.into_split();
        let request = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "token": self.token,
            "session_id": session_id,
            "method": "visual.session",
            "params": VisualSessionRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                max_width,
                fps,
                include_bytes,
            }
        });
        write
            .write_all(request.to_string().as_bytes())
            .await
            .context("write visual session request")?;
        write
            .write_all(b"\n")
            .await
            .context("flush visual session request")?;
        write.flush().await.context("flush visual session stream")?;
        Ok(CuaVisualSession {
            read: BufReader::new(read),
            write,
            closed: false,
        })
    }

    pub async fn dispatch(&self, action: &InputAction) -> anyhow::Result<Value> {
        ensure_dispatchable(action)?;
        self.request("input.dispatch", Some(serde_json::to_value(action)?))
            .await
    }

    pub async fn dispatch_frame(
        &self,
        source_frame: FrameEnvelope,
        action: &InputAction,
    ) -> anyhow::Result<Value> {
        ensure_dispatchable(action)?;
        self.request(
            "input.dispatch_frame",
            Some(serde_json::to_value(FrameActionRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                source_frame,
                action: action.clone(),
            })?),
        )
        .await
    }

    pub async fn session(&self) -> anyhow::Result<CuaSession> {
        CuaSession::connect(self).await
    }

    pub async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: Option<Value>,
    ) -> anyhow::Result<T> {
        self.request_with_session(method, body, None).await
    }

    pub async fn request_with_session<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: Option<Value>,
        session_id: Option<&str>,
    ) -> anyhow::Result<T> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connect {}", self.socket_path.display()))?;
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let request = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "token": self.token,
            "session_id": session_id,
            "method": method,
            "params": body.unwrap_or_else(|| serde_json::json!({}))
        });
        write
            .write_all(request.to_string().as_bytes())
            .await
            .context("write unix request")?;
        write.write_all(b"\n").await.context("flush unix request")?;
        write.flush().await.context("flush unix stream")?;
        let line = lines
            .next_line()
            .await
            .context("read unix response")?
            .ok_or_else(|| anyhow::anyhow!("empty unix response for {method}"))?;
        decode_unix_response(method, &line)
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }
}

pub struct CuaSession {
    token: String,
    read: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
}

pub struct CuaVisualSession {
    read: BufReader<OwnedReadHalf>,
    write: OwnedWriteHalf,
    closed: bool,
}

impl CuaVisualSession {
    pub async fn next_message(&mut self) -> anyhow::Result<Option<VisualSessionMessage>> {
        let mut line = String::new();
        let bytes = self
            .read
            .read_line(&mut line)
            .await
            .context("read visual session message")?;
        if bytes == 0 {
            return Ok(None);
        }
        if line.trim().is_empty() {
            return Ok(Some(VisualSessionMessage::Empty));
        }
        Ok(Some(serde_json::from_str(&line).with_context(|| {
            format!("decode visual session message {}", line.trim())
        })?))
    }

    pub async fn next_frame(&mut self) -> anyhow::Result<Option<Value>> {
        while let Some(message) = self.next_message().await? {
            match message {
                VisualSessionMessage::Frame { frame, .. } => return Ok(Some(frame)),
                VisualSessionMessage::Error { error, .. } => {
                    anyhow::bail!("visual session error: {error}");
                }
                VisualSessionMessage::Closed { .. } => return Ok(None),
                VisualSessionMessage::Started { .. } | VisualSessionMessage::Empty => {}
            }
        }
        Ok(None)
    }

    pub async fn close(&mut self) -> anyhow::Result<()> {
        if self.closed {
            return Ok(());
        }
        let request = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "method": "visual.close",
            "params": {}
        });
        self.write
            .write_all(request.to_string().as_bytes())
            .await
            .context("write visual close request")?;
        self.write
            .write_all(b"\n")
            .await
            .context("flush visual close request")?;
        self.write
            .flush()
            .await
            .context("flush visual close stream")?;
        self.closed = true;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VisualSessionMessage {
    Started {
        schema_version: String,
        fps: u32,
    },
    Frame {
        schema_version: String,
        frame: Value,
    },
    Error {
        schema_version: String,
        error: String,
    },
    Closed {
        schema_version: String,
    },
    #[serde(other)]
    Empty,
}

impl CuaSession {
    async fn connect(client: &CuaClient) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(&client.socket_path)
            .await
            .with_context(|| format!("connect {}", client.socket_path.display()))?;
        let (read, write) = stream.into_split();
        Ok(Self {
            token: client.token.clone(),
            read: BufReader::new(read),
            write,
        })
    }

    pub async fn context(&mut self, include_bytes: bool) -> anyhow::Result<DesktopContextSnapshot> {
        self.request(
            "context.snapshot",
            Some(context_request_body(include_bytes)),
        )
        .await
    }

    pub async fn dispatch(&mut self, action: &InputAction) -> anyhow::Result<Value> {
        ensure_dispatchable(action)?;
        self.request("input.dispatch", Some(serde_json::to_value(action)?))
            .await
    }

    pub async fn dispatch_frame(
        &mut self,
        source_frame: FrameEnvelope,
        action: &InputAction,
    ) -> anyhow::Result<Value> {
        ensure_dispatchable(action)?;
        self.request(
            "input.dispatch_frame",
            Some(serde_json::to_value(FrameActionRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                source_frame,
                action: action.clone(),
            })?),
        )
        .await
    }

    pub async fn events_after(&mut self, sequence: u64) -> anyhow::Result<Vec<Value>> {
        self.request(
            "events.after",
            Some(serde_json::json!({ "after_sequence": sequence })),
        )
        .await
    }

    pub async fn events_snapshot(&mut self) -> anyhow::Result<Vec<Value>> {
        self.request("events.snapshot", None).await
    }

    pub async fn events_wait(
        &mut self,
        sequence: u64,
        timeout_ms: u64,
    ) -> anyhow::Result<Vec<Value>> {
        self.request(
            "events.wait",
            Some(serde_json::json!({
                "after_sequence": sequence,
                "timeout_ms": timeout_ms
            })),
        )
        .await
    }

    pub async fn ui_step(
        &mut self,
        label: impl Into<String>,
        source: Option<String>,
        task: Option<String>,
        tool: Option<String>,
        step_index: Option<u16>,
        step_total: Option<u16>,
        ttl_ms: Option<u64>,
    ) -> anyhow::Result<Value> {
        self.request(
            "ui.step",
            Some(serde_json::to_value(UiStepRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                label: label.into(),
                source,
                task,
                tool,
                step_index,
                step_total,
                ttl_ms,
            })?),
        )
        .await
    }

    pub async fn ui_reply(
        &mut self,
        text: impl Into<String>,
        source: Option<String>,
        ttl_ms: Option<u64>,
    ) -> anyhow::Result<Value> {
        self.request(
            "ui.reply",
            Some(serde_json::to_value(UiReplyRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                text: text.into(),
                source,
                ttl_ms,
            })?),
        )
        .await
    }

    pub async fn ui_mode(&mut self, mode: UiMode, source: Option<String>) -> anyhow::Result<Value> {
        self.request(
            "ui.mode",
            Some(serde_json::to_value(UiModeRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                mode,
                source,
            })?),
        )
        .await
    }

    pub async fn request<T: serde::de::DeserializeOwned>(
        &mut self,
        method: &str,
        body: Option<Value>,
    ) -> anyhow::Result<T> {
        let request = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "token": self.token,
            "method": method,
            "params": body.unwrap_or_else(|| serde_json::json!({}))
        });
        self.write
            .write_all(request.to_string().as_bytes())
            .await
            .context("write unix request")?;
        self.write
            .write_all(b"\n")
            .await
            .context("flush unix request")?;
        self.write.flush().await.context("flush unix stream")?;
        let mut line = String::new();
        self.read
            .read_line(&mut line)
            .await
            .context("read unix response")?;
        if line.trim().is_empty() {
            bail!("empty unix response for {method}");
        }
        decode_unix_response(method, &line)
    }
}

fn decode_unix_response<T: serde::de::DeserializeOwned>(
    method: &str,
    line: &str,
) -> anyhow::Result<T> {
    let response: UnixResponse =
        serde_json::from_str(line).context("decode unix response envelope")?;
    if !response.ok {
        let error = response.error.unwrap_or_else(|| serde_json::json!({}));
        bail!("unix request {method} failed: {error}");
    }
    Ok(serde_json::from_value(
        response.result.unwrap_or(Value::Null),
    )?)
}

fn ensure_dispatchable(action: &InputAction) -> anyhow::Result<()> {
    match action {
        InputAction::MouseMove { .. }
        | InputAction::MouseClick { .. }
        | InputAction::MouseDrag { .. } => {}
        InputAction::KeyPress { .. }
        | InputAction::KeyType { .. }
        | InputAction::KeyPaste { .. }
        | InputAction::Sequence { .. }
        | InputAction::OpenApp { .. }
        | InputAction::ShellExec { .. }
        | InputAction::Aegis { .. }
        | InputAction::Ctx { .. } => {}
        InputAction::Pause | InputAction::Resume | InputAction::KillSwitch => {}
        InputAction::ClipboardRead { .. } | InputAction::ClipboardWrite { .. } => {
            bail!("clipboard actions require explicit clipboard endpoints")
        }
    }
    Ok(())
}

fn context_request_body(include_bytes: bool) -> Value {
    serde_json::json!({
        "max_width": 1280,
        "encoding": FrameEncoding::Jpeg,
        "force_fresh": true,
        "include_bytes": include_bytes
    })
}

#[derive(Debug, Deserialize)]
struct UnixResponse {
    ok: bool,
    result: Option<Value>,
    error: Option<Value>,
}

async fn load_or_create_profile_token(profile: &str) -> anyhow::Result<String> {
    if http_token_override_allowed() {
        if let Ok(token) = std::env::var("CUA_HTTP_TOKEN") {
            if !token.trim().is_empty() {
                return Ok(token);
            }
        }
    }
    let path = profile_token_path(profile)?;
    if let Ok(token) = tokio::fs::read_to_string(&path).await {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let token = format!("cua-{}", uuid::Uuid::new_v4());
    tokio::fs::write(path, format!("{token}\n")).await?;
    Ok(token)
}

fn http_token_override_allowed() -> bool {
    cfg!(test)
        || std::env::var("CUA_DEV_HTTP_TOKEN_OVERRIDE")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_request_uses_realtime_jpeg_frame() {
        let body = context_request_body(true);

        assert_eq!(body["max_width"], 1280);
        assert_eq!(body["encoding"], "jpeg");
        assert_eq!(body["force_fresh"], true);
        assert_eq!(body["include_bytes"], true);
    }
}
