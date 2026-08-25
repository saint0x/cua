use anyhow::{bail, Context};
use cua_core::{FrameEncoding, FramePayload, InputAction};
use reqwest::Method;
use serde_json::Value;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CuaClient {
    addr: SocketAddr,
    profile: String,
    token: String,
    client: reqwest::Client,
}

impl CuaClient {
    pub async fn new(addr: SocketAddr, profile: impl Into<String>) -> anyhow::Result<Self> {
        let profile = profile.into();
        let token = load_or_create_profile_token(&profile).await?;
        Ok(Self {
            addr,
            profile,
            token,
            client: reqwest::Client::new(),
        })
    }

    pub async fn screenshot(&self, include_bytes: bool) -> anyhow::Result<FramePayload> {
        self.request(
            Method::POST,
            "/capture/screenshot",
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
        self.request(Method::GET, "/observe/desktop", None).await
    }

    pub async fn dispatch(&self, action: &InputAction) -> anyhow::Result<Value> {
        let path = match action {
            InputAction::MouseMove { .. }
            | InputAction::MouseClick { .. }
            | InputAction::MouseDrag { .. } => "/input/mouse",
            InputAction::KeyPress { .. }
            | InputAction::KeyType { .. }
            | InputAction::KeyPaste { .. } => "/input/keyboard",
            InputAction::Pause | InputAction::Resume | InputAction::KillSwitch => "/input/keyboard",
            InputAction::ClipboardRead { .. } | InputAction::ClipboardWrite { .. } => {
                bail!("clipboard actions require explicit clipboard endpoints")
            }
        };
        self.request(Method::POST, path, Some(serde_json::to_value(action)?))
            .await
    }

    async fn request<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> anyhow::Result<T> {
        let mut request = self
            .client
            .request(method, format!("http://{}{}", self.addr, path))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.context("send local request")?;
        let status = response.status();
        let value = response
            .json::<Value>()
            .await
            .context("decode local response")?;
        if !status.is_success() {
            bail!("local request {path} failed with {status}: {value}");
        }
        Ok(serde_json::from_value(value)?)
    }

    pub fn profile(&self) -> &str {
        &self.profile
    }
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
