use cua_core::{
    profile_socket_path, profile_token_path, ApiErrorBody, AttestationChallenge,
    AttestationChallengeRequest, AttestationSignRequest, ClipboardReadRequest, ClipboardResult,
    ClipboardWriteRequest, ConfigInventory, DesktopContextSnapshot, DesktopState,
    FrameActionRequest, FrameEncoding, FrameEnvelope, FramePayload, HealthReport,
    InboundMessageRequest, InboundStatus, InputAction, MachineAttestation, MachineIdentityStatus,
    Manifest, RuntimeControlState, RuntimeInventory, RuntimeSessionRole, SchemaBundle,
    ScratchpadDeleteRequest, ScratchpadDeleteResult, ScratchpadEntry, ScratchpadListRequest,
    ScratchpadListResult, ScratchpadReadRequest, ScratchpadWriteRequest, SessionCancelRequest,
    SessionHeartbeatRequest, SessionLeaseRequest, SessionLeaseResult, UiIslandRequest,
    UiIslandResult, UiIslandState, UiMode, UiModeRequest, UiReplyRequest, UiStepRequest,
    VisualSessionRequest, WebhookSourceStatus, WebhookSubscribeRequest, SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{
    unix::{OwnedReadHalf, OwnedWriteHalf},
    UnixStream,
};

pub type Result<T> = std::result::Result<T, CuaClientError>;

#[derive(Debug, thiserror::Error)]
pub enum CuaClientError {
    #[error("connect {path}: {source}")]
    Connect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("write unix request: {0}")]
    Write(std::io::Error),
    #[error("flush unix stream: {0}")]
    Flush(std::io::Error),
    #[error("read unix response: {0}")]
    Read(std::io::Error),
    #[error("empty unix response for {method}")]
    EmptyResponse { method: String },
    #[error("decode unix response envelope: {0}")]
    DecodeEnvelope(serde_json::Error),
    #[error("unix request {method} failed with {}: {}", error.code, error.message)]
    Protocol { method: String, error: ApiErrorBody },
    #[error("decode unix result for {method}: {source}")]
    DecodeResult {
        method: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("encode unix request for {method}: {source}")]
    EncodeRequest {
        method: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("json serialization: {0}")]
    Json(#[from] serde_json::Error),
    #[error("filesystem operation failed: {0}")]
    Fs(#[from] std::io::Error),
    #[error("clipboard actions require explicit clipboard endpoints")]
    ClipboardActionRequiresEndpoint,
    #[error("{field} must not be empty")]
    EmptyField { field: &'static str },
    #[error(transparent)]
    Core(#[from] anyhow::Error),
}

#[derive(Debug, Clone)]
pub struct CuaClient {
    profile: String,
    token: String,
    socket_path: PathBuf,
}

impl CuaClient {
    pub async fn connect(profile: impl Into<String>) -> Result<Self> {
        let profile = profile.into();
        let token = load_or_create_profile_token(&profile).await?;
        let socket_path = profile_socket_path(&profile)?;
        Ok(Self {
            profile,
            token,
            socket_path,
        })
    }

    pub async fn new(profile: impl Into<String>) -> Result<Self> {
        Self::connect(profile).await
    }

    pub async fn screenshot(&self, include_bytes: bool) -> Result<FramePayload> {
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

    pub async fn status(&self) -> Result<HealthReport> {
        self.request("status", None).await
    }

    pub async fn manifest(&self) -> Result<Manifest> {
        self.request("manifest", None).await
    }

    pub async fn schemas(&self) -> Result<SchemaBundle> {
        self.request("schemas", None).await
    }

    pub async fn observe(&self) -> Result<DesktopState> {
        self.request("observe.desktop", None).await
    }

    pub async fn config_status(&self) -> Result<ConfigInventory> {
        self.request("config.status", None).await
    }

    pub async fn context(&self, include_bytes: bool) -> Result<DesktopContextSnapshot> {
        self.request(
            "context.snapshot",
            Some(context_request_body(include_bytes)),
        )
        .await
    }

    pub async fn events(&self) -> Result<Vec<Value>> {
        self.request("events.snapshot", None).await
    }

    pub async fn events_after(&self, sequence: u64) -> Result<Vec<Value>> {
        self.request(
            "events.after",
            Some(serde_json::json!({ "after_sequence": sequence })),
        )
        .await
    }

    pub async fn events_wait(&self, sequence: u64, timeout_ms: u64) -> Result<Vec<Value>> {
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
    ) -> Result<SessionLeaseResult> {
        self.acquire_session(RuntimeSessionRole::Owner, client_name, ttl_ms)
            .await
    }

    pub async fn acquire_observer(
        &self,
        client_name: impl Into<String>,
        ttl_ms: Option<i64>,
    ) -> Result<SessionLeaseResult> {
        self.acquire_session(RuntimeSessionRole::Observer, client_name, ttl_ms)
            .await
    }

    pub async fn acquire_session(
        &self,
        role: RuntimeSessionRole,
        client_name: impl Into<String>,
        ttl_ms: Option<i64>,
    ) -> Result<SessionLeaseResult> {
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
    ) -> Result<RuntimeInventory> {
        let request = SessionCancelRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: session_id.into(),
            target_session_id,
        };
        self.request("session.cancel", Some(serde_json::to_value(request)?))
            .await
    }

    pub async fn heartbeat_session(
        &self,
        session_id: impl Into<String>,
        ttl_ms: Option<i64>,
    ) -> Result<SessionLeaseResult> {
        let request = SessionHeartbeatRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: session_id.into(),
            ttl_ms,
        };
        self.request("session.heartbeat", Some(serde_json::to_value(request)?))
            .await
    }

    pub async fn session_status(&self) -> Result<RuntimeInventory> {
        self.request("session.status", None).await
    }

    pub async fn attestation_identity(&self) -> Result<MachineIdentityStatus> {
        self.request("attestation.identity", None).await
    }

    pub async fn attestation_challenge(
        &self,
        audience: impl Into<String>,
    ) -> Result<AttestationChallenge> {
        self.request(
            "attestation.challenge",
            Some(serde_json::to_value(AttestationChallengeRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                audience: audience.into(),
                profile: Some(self.profile.clone()),
                requested_claims: Vec::new(),
            })?),
        )
        .await
    }

    pub async fn attestation_sign(
        &self,
        audience: impl Into<String>,
        nonce: impl Into<String>,
        challenge_id: Option<String>,
        session_id: Option<String>,
    ) -> Result<MachineAttestation> {
        self.request(
            "attestation.sign",
            Some(serde_json::to_value(AttestationSignRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                audience: audience.into(),
                nonce: nonce.into(),
                challenge_id,
                profile: Some(self.profile.clone()),
                session_id,
            })?),
        )
        .await
    }

    pub async fn inbox_publish(&self, request: InboundMessageRequest) -> Result<InboundStatus> {
        self.request("inbox.publish", Some(serde_json::to_value(request)?))
            .await
    }

    pub async fn inbox_after(&self, after_sequence: u64) -> Result<Vec<InboundStatus>> {
        self.request(
            "inbox.after",
            Some(serde_json::json!({ "after_sequence": after_sequence })),
        )
        .await
    }

    pub async fn inbox_status(&self, message_id: impl Into<String>) -> Result<InboundStatus> {
        self.request(
            "inbox.status",
            Some(serde_json::json!({ "message_id": message_id.into() })),
        )
        .await
    }

    pub async fn inbox_running(&self, message_id: impl Into<String>) -> Result<InboundStatus> {
        self.request(
            "inbox.running",
            Some(serde_json::json!({ "message_id": message_id.into() })),
        )
        .await
    }

    pub async fn inbox_done(
        &self,
        message_id: impl Into<String>,
        reply: Option<String>,
    ) -> Result<InboundStatus> {
        self.request(
            "inbox.done",
            Some(serde_json::json!({ "message_id": message_id.into(), "reply": reply })),
        )
        .await
    }

    pub async fn inbox_failed(
        &self,
        message_id: impl Into<String>,
        error: impl Into<String>,
    ) -> Result<InboundStatus> {
        self.request(
            "inbox.failed",
            Some(serde_json::json!({ "message_id": message_id.into(), "error": error.into() })),
        )
        .await
    }

    pub async fn webhook_publish(&self, request: InboundMessageRequest) -> Result<InboundStatus> {
        self.request("webhook.publish", Some(serde_json::to_value(request)?))
            .await
    }

    pub async fn webhook_subscribe(
        &self,
        request: WebhookSubscribeRequest,
    ) -> Result<WebhookSourceStatus> {
        self.request("webhook.subscribe", Some(serde_json::to_value(request)?))
            .await
    }

    pub async fn webhook_status(&self, source: impl Into<String>) -> Result<WebhookSourceStatus> {
        self.request(
            "webhook.status",
            Some(serde_json::json!({ "source": source.into() })),
        )
        .await
    }

    pub async fn scratchpad_write(
        &self,
        request: ScratchpadWriteRequest,
        owner_session_id: &str,
    ) -> Result<ScratchpadEntry> {
        self.request_with_session(
            "scratchpad.write",
            Some(serde_json::to_value(request)?),
            Some(owner_session_id),
        )
        .await
    }

    pub async fn scratchpad_read(&self, request: ScratchpadReadRequest) -> Result<ScratchpadEntry> {
        self.request("scratchpad.read", Some(serde_json::to_value(request)?))
            .await
    }

    pub async fn scratchpad_list(
        &self,
        request: ScratchpadListRequest,
    ) -> Result<ScratchpadListResult> {
        self.request("scratchpad.list", Some(serde_json::to_value(request)?))
            .await
    }

    pub async fn scratchpad_delete(
        &self,
        request: ScratchpadDeleteRequest,
        owner_session_id: &str,
    ) -> Result<ScratchpadDeleteResult> {
        self.request_with_session(
            "scratchpad.delete",
            Some(serde_json::to_value(request)?),
            Some(owner_session_id),
        )
        .await
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
    ) -> Result<Value> {
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
    ) -> Result<Value> {
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

    pub async fn ui_mode(&self, mode: UiMode, source: Option<String>) -> Result<Value> {
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
    ) -> Result<UiIslandResult> {
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

    pub async fn clipboard_read(&self, allow_sensitive: bool) -> Result<ClipboardResult> {
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
        owner_session_id: &str,
    ) -> Result<ClipboardResult> {
        self.request_with_session(
            "clipboard.write",
            Some(serde_json::to_value(ClipboardWriteRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                text: text.into(),
            })?),
            Some(owner_session_id),
        )
        .await
    }

    pub async fn pause(&self, owner_session_id: &str) -> Result<RuntimeControlState> {
        self.request_with_session("control.pause", None, Some(owner_session_id))
            .await
    }

    pub async fn resume(&self, owner_session_id: &str) -> Result<RuntimeControlState> {
        self.request_with_session("control.resume", None, Some(owner_session_id))
            .await
    }

    pub async fn kill_switch(&self, owner_session_id: &str) -> Result<RuntimeControlState> {
        self.request_with_session("control.kill_switch", None, Some(owner_session_id))
            .await
    }

    pub async fn visual_session(
        &self,
        max_width: Option<u32>,
        fps: Option<u32>,
        include_bytes: bool,
        session_id: Option<&str>,
    ) -> Result<CuaVisualSession> {
        self.visual_session_with_options(max_width, fps, include_bytes, session_id, None, None)
            .await
    }

    pub async fn visual_session_with_options(
        &self,
        max_width: Option<u32>,
        fps: Option<u32>,
        include_bytes: bool,
        session_id: Option<&str>,
        duration_ms: Option<u64>,
        queue_depth: Option<usize>,
    ) -> Result<CuaVisualSession> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|source| CuaClientError::Connect {
                path: self.socket_path.clone(),
                source,
            })?;
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
                duration_ms,
                queue_depth,
            }
        });
        write
            .write_all(request.to_string().as_bytes())
            .await
            .map_err(CuaClientError::Write)?;
        write
            .write_all(b"\n")
            .await
            .map_err(CuaClientError::Write)?;
        write.flush().await.map_err(CuaClientError::Flush)?;
        Ok(CuaVisualSession {
            read: BufReader::new(read),
            write,
            closed: false,
        })
    }

    pub async fn dispatch(&self, action: &InputAction, owner_session_id: &str) -> Result<Value> {
        ensure_dispatchable(action)?;
        self.request_with_session(
            "input.dispatch",
            Some(serde_json::to_value(action)?),
            Some(owner_session_id),
        )
        .await
    }

    pub async fn dispatch_frame(
        &self,
        source_frame: FrameEnvelope,
        action: &InputAction,
        owner_session_id: &str,
    ) -> Result<Value> {
        ensure_dispatchable(action)?;
        self.request_with_session(
            "input.dispatch_frame",
            Some(serde_json::to_value(FrameActionRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                source_frame,
                action: action.clone(),
            })?),
            Some(owner_session_id),
        )
        .await
    }

    pub async fn session(&self) -> Result<CuaSession> {
        CuaSession::connect(self).await
    }

    pub async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: Option<Value>,
    ) -> Result<T> {
        self.request_with_session(method, body, None).await
    }

    pub async fn request_with_session<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: Option<Value>,
        session_id: Option<&str>,
    ) -> Result<T> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|source| CuaClientError::Connect {
                path: self.socket_path.clone(),
                source,
            })?;
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
            .map_err(CuaClientError::Write)?;
        write
            .write_all(b"\n")
            .await
            .map_err(CuaClientError::Write)?;
        write.flush().await.map_err(CuaClientError::Flush)?;
        let line = lines
            .next_line()
            .await
            .map_err(CuaClientError::Read)?
            .ok_or_else(|| CuaClientError::EmptyResponse {
                method: method.to_string(),
            })?;
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
    pub async fn next_message(&mut self) -> Result<Option<VisualSessionMessage>> {
        let mut line = String::new();
        let bytes = self
            .read
            .read_line(&mut line)
            .await
            .map_err(CuaClientError::Read)?;
        if bytes == 0 {
            return Ok(None);
        }
        if line.trim().is_empty() {
            return Ok(Some(VisualSessionMessage::Empty));
        }
        Ok(Some(
            serde_json::from_str(&line).map_err(CuaClientError::DecodeEnvelope)?,
        ))
    }

    pub async fn next_frame(&mut self) -> Result<Option<Value>> {
        while let Some(message) = self.next_message().await? {
            match message {
                VisualSessionMessage::Frame { frame, .. } => return Ok(Some(frame)),
                VisualSessionMessage::Error { error, .. } => {
                    return Err(CuaClientError::Protocol {
                        method: "visual.session".to_string(),
                        error: ApiErrorBody {
                            schema_version: SCHEMA_VERSION.to_string(),
                            code: "visual_session".to_string(),
                            message: error,
                            details: Default::default(),
                        },
                    });
                }
                VisualSessionMessage::Closed { .. } => return Ok(None),
                VisualSessionMessage::Started { .. }
                | VisualSessionMessage::Diagnostic { .. }
                | VisualSessionMessage::Empty => {}
            }
        }
        Ok(None)
    }

    pub async fn close(&mut self) -> Result<()> {
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
            .map_err(CuaClientError::Write)?;
        self.write
            .write_all(b"\n")
            .await
            .map_err(CuaClientError::Write)?;
        self.write.flush().await.map_err(CuaClientError::Flush)?;
        self.closed = true;
        Ok(())
    }

    pub async fn cancel(&mut self) -> Result<()> {
        self.close().await
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
    Diagnostic {
        schema_version: String,
        message: String,
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
    async fn connect(client: &CuaClient) -> Result<Self> {
        let stream = UnixStream::connect(&client.socket_path)
            .await
            .map_err(|source| CuaClientError::Connect {
                path: client.socket_path.clone(),
                source,
            })?;
        let (read, write) = stream.into_split();
        Ok(Self {
            token: client.token.clone(),
            read: BufReader::new(read),
            write,
        })
    }

    pub async fn context(&mut self, include_bytes: bool) -> Result<DesktopContextSnapshot> {
        self.request(
            "context.snapshot",
            Some(context_request_body(include_bytes)),
        )
        .await
    }

    pub async fn dispatch(
        &mut self,
        action: &InputAction,
        owner_session_id: &str,
    ) -> Result<Value> {
        ensure_dispatchable(action)?;
        self.request_with_session(
            "input.dispatch",
            Some(serde_json::to_value(action)?),
            Some(owner_session_id),
        )
        .await
    }

    pub async fn dispatch_frame(
        &mut self,
        source_frame: FrameEnvelope,
        action: &InputAction,
        owner_session_id: &str,
    ) -> Result<Value> {
        ensure_dispatchable(action)?;
        self.request_with_session(
            "input.dispatch_frame",
            Some(serde_json::to_value(FrameActionRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                source_frame,
                action: action.clone(),
            })?),
            Some(owner_session_id),
        )
        .await
    }

    pub async fn events_after(&mut self, sequence: u64) -> Result<Vec<Value>> {
        self.request(
            "events.after",
            Some(serde_json::json!({ "after_sequence": sequence })),
        )
        .await
    }

    pub async fn events_snapshot(&mut self) -> Result<Vec<Value>> {
        self.request("events.snapshot", None).await
    }

    pub async fn events_wait(&mut self, sequence: u64, timeout_ms: u64) -> Result<Vec<Value>> {
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
    ) -> Result<Value> {
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
    ) -> Result<Value> {
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

    pub async fn ui_mode(&mut self, mode: UiMode, source: Option<String>) -> Result<Value> {
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
    ) -> Result<T> {
        self.request_with_session(method, body, None).await
    }

    pub async fn request_with_session<T: serde::de::DeserializeOwned>(
        &mut self,
        method: &str,
        body: Option<Value>,
        session_id: Option<&str>,
    ) -> Result<T> {
        let request = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "token": self.token,
            "session_id": session_id,
            "method": method,
            "params": body.unwrap_or_else(|| serde_json::json!({}))
        });
        self.write
            .write_all(request.to_string().as_bytes())
            .await
            .map_err(CuaClientError::Write)?;
        self.write
            .write_all(b"\n")
            .await
            .map_err(CuaClientError::Write)?;
        self.write.flush().await.map_err(CuaClientError::Flush)?;
        let mut line = String::new();
        self.read
            .read_line(&mut line)
            .await
            .map_err(CuaClientError::Read)?;
        if line.trim().is_empty() {
            return Err(CuaClientError::EmptyResponse {
                method: method.to_string(),
            });
        }
        decode_unix_response(method, &line)
    }
}

fn decode_unix_response<T: serde::de::DeserializeOwned>(method: &str, line: &str) -> Result<T> {
    let response: UnixResponse =
        serde_json::from_str(line).map_err(CuaClientError::DecodeEnvelope)?;
    if !response.ok {
        return Err(CuaClientError::Protocol {
            method: method.to_string(),
            error: decode_api_error(response.error),
        });
    }
    serde_json::from_value(response.result.unwrap_or(Value::Null)).map_err(|source| {
        CuaClientError::DecodeResult {
            method: method.to_string(),
            source,
        }
    })
}

fn ensure_dispatchable(action: &InputAction) -> Result<()> {
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
            return Err(CuaClientError::ClipboardActionRequiresEndpoint)
        }
    }
    Ok(())
}

fn decode_api_error(error: Option<Value>) -> ApiErrorBody {
    match error {
        Some(value) => match serde_json::from_value::<ApiErrorBody>(value.clone()) {
            Ok(error) => error,
            Err(source) => {
                let mut details = BTreeMap::new();
                details.insert("decode_error".to_string(), source.to_string());
                details.insert("raw_error".to_string(), value.to_string());
                ApiErrorBody {
                    schema_version: SCHEMA_VERSION.to_string(),
                    code: "protocol_error".to_string(),
                    message: "daemon returned an unstructured protocol error".to_string(),
                    details,
                }
            }
        },
        None => ApiErrorBody {
            schema_version: SCHEMA_VERSION.to_string(),
            code: "protocol_error".to_string(),
            message: "daemon returned an unstructured protocol error".to_string(),
            details: Default::default(),
        },
    }
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

async fn load_or_create_profile_token(profile: &str) -> Result<String> {
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
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    #[test]
    fn context_request_uses_realtime_jpeg_frame() {
        let body = context_request_body(true);

        assert_eq!(body["max_width"], 1280);
        assert_eq!(body["encoding"], "jpeg");
        assert_eq!(body["force_fresh"], true);
        assert_eq!(body["include_bytes"], true);
    }

    #[test]
    fn unix_protocol_errors_decode_to_typed_client_error() {
        let line = serde_json::json!({
            "id": "test",
            "ok": false,
            "error": {
                "schema_version": SCHEMA_VERSION,
                "code": "session_owner",
                "message": "write requires an active owner session",
                "details": {}
            }
        })
        .to_string();

        let error = decode_unix_response::<Value>("control.pause", &line).unwrap_err();
        match error {
            CuaClientError::Protocol { method, error } => {
                assert_eq!(method, "control.pause");
                assert_eq!(error.code, "session_owner");
                assert_eq!(error.message, "write requires an active owner session");
            }
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[test]
    fn unix_protocol_errors_preserve_unstructured_payload() {
        let line = serde_json::json!({
            "id": "test",
            "ok": false,
            "error": {
                "message": "capture backend timed out after 10000ms",
                "status": 500
            }
        })
        .to_string();

        let error = decode_unix_response::<Value>("context.snapshot", &line).unwrap_err();
        match error {
            CuaClientError::Protocol { method, error } => {
                assert_eq!(method, "context.snapshot");
                assert_eq!(error.code, "protocol_error");
                assert!(error
                    .details
                    .get("raw_error")
                    .is_some_and(|raw| raw.contains("capture backend timed out")));
                assert!(error.details.contains_key("decode_error"));
            }
            other => panic!("expected protocol error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn visual_session_next_frame_skips_stream_diagnostics() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let (read, write) = client.into_split();
        let mut session = CuaVisualSession {
            read: BufReader::new(read),
            write,
            closed: false,
        };

        server
            .write_all(
                br#"{"type":"started","schema_version":"cua.v1","fps":10}
{"type":"diagnostic","schema_version":"cua.v1","message":"transient capture miss"}
{"type":"frame","schema_version":"cua.v1","frame":{"ok":true}}
"#,
            )
            .await
            .unwrap();

        let frame = session.next_frame().await.unwrap().unwrap();

        assert_eq!(frame["ok"], true);
    }
}
