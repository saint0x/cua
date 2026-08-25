use anyhow::{bail, Context};
use cua_core::{FrameEncoding, FramePayload, InputAction};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

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

    pub async fn observe(&self) -> anyhow::Result<Value> {
        self.request("observe.desktop", None).await
    }

    pub async fn preflight(&self) -> anyhow::Result<()> {
        let _stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connect {}", self.socket_path.display()))?;
        Ok(())
    }

    pub async fn dispatch(&self, action: &InputAction) -> anyhow::Result<Value> {
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
        };
        self.request("input.dispatch", Some(serde_json::to_value(action)?))
            .await
    }

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        body: Option<Value>,
    ) -> anyhow::Result<T> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connect {}", self.socket_path.display()))?;
        let request = serde_json::json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "token": self.token,
            "method": method,
            "params": body.unwrap_or_else(|| serde_json::json!({}))
        });
        stream
            .write_all(request.to_string().as_bytes())
            .await
            .context("write unix request")?;
        stream
            .write_all(b"\n")
            .await
            .context("flush unix request")?;
        let mut line = String::new();
        BufReader::new(stream)
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

    pub fn profile(&self) -> &str {
        &self.profile
    }
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
