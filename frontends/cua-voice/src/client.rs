use anyhow::{bail, Context};
use cua_core::{
    DesktopContextSnapshot, DesktopState, FrameActionRequest, FrameEncoding, FrameEnvelope,
    FramePayload, InputAction, UiMode, UiModeRequest, UiReplyRequest, UiStepRequest,
    SCHEMA_VERSION,
};
use serde::Deserialize;
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
    pub async fn new(profile: impl Into<String>) -> anyhow::Result<Self> {
        let profile = profile.into();
        let token = load_or_create_profile_token(&profile).await?;
        let socket_path = profile_socket_path(&profile)?;
        Ok(Self {
            profile,
            token,
            socket_path,
        })
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

    pub async fn observe(&self) -> anyhow::Result<DesktopState> {
        self.request("observe.desktop", None).await
    }

    pub async fn context(&self, include_bytes: bool) -> anyhow::Result<DesktopContextSnapshot> {
        self.request(
            "context.snapshot",
            Some(serde_json::json!({
                "max_width": 1280,
                "encoding": FrameEncoding::Png,
                "force_fresh": true,
                "include_bytes": include_bytes
            })),
        )
        .await
    }

    pub async fn preflight(&self) -> anyhow::Result<()> {
        let _stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connect {}", self.socket_path.display()))?;
        Ok(())
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

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: Option<Value>,
    ) -> anyhow::Result<T> {
        let mut session = self.session().await?;
        session.request(method, body).await
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
            Some(serde_json::json!({
                "max_width": 1280,
                "encoding": FrameEncoding::Png,
                "force_fresh": true,
                "include_bytes": include_bytes
            })),
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

    async fn request<T: serde::de::DeserializeOwned>(
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
        let response: UnixResponse =
            serde_json::from_str(&line).context("decode unix response envelope")?;
        if !response.ok {
            let error = response.error.unwrap_or_else(|| serde_json::json!({}));
            bail!("unix request {method} failed: {error}");
        }
        Ok(serde_json::from_value(
            response.result.unwrap_or(Value::Null),
        )?)
    }
}

fn ensure_dispatchable(action: &InputAction) -> anyhow::Result<()> {
    match action {
        InputAction::MouseMove { .. }
        | InputAction::MouseClick { .. }
        | InputAction::MouseDrag { .. } => {}
        InputAction::KeyPress { .. }
        | InputAction::KeyType { .. }
        | InputAction::KeyPaste { .. } => {}
        InputAction::Pause | InputAction::Resume | InputAction::KillSwitch => {}
        InputAction::ClipboardRead { .. } | InputAction::ClipboardWrite { .. } => {
            bail!("clipboard actions require explicit clipboard endpoints")
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct UnixResponse {
    ok: bool,
    result: Option<Value>,
    error: Option<Value>,
}

async fn load_or_create_profile_token(profile: &str) -> anyhow::Result<String> {
    if let Ok(token) = std::env::var("CUA_HTTP_TOKEN") {
        if !token.trim().is_empty() {
            return Ok(token);
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

fn profile_token_path(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(std::env::var("HOME")?)
        .join(".cua")
        .join("profiles")
        .join(profile)
        .join("http.token"))
}

fn profile_socket_path(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(std::env::var("HOME")?)
        .join(".cua")
        .join("profiles")
        .join(profile)
        .join("daemon.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn preflight_fails_when_profile_socket_is_absent() {
        let profile = format!("missing-socket-{}", uuid::Uuid::new_v4());
        let client = CuaClient::new(profile).await.unwrap();

        let error = client.preflight().await.unwrap_err().to_string();

        assert!(error.contains("daemon.sock"));
    }
}
