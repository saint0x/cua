//! Backend-neutral computer runtime contracts.
//!
//! A computer backend is the complete environment the agent can observe and
//! control. Local macOS is the default implementation, while cloud providers
//! and Quilt VM fleets can plug in by implementing the same surface.

use anyhow::Context;
use async_trait::async_trait;
use base64::Engine;
use cua_capture::{
    CaptureBackend, CaptureRequest, CaptureSource, CapturedFrame, CapturedFrameTimings,
    SyntheticCaptureBackend,
};
use cua_core::{
    CapabilityManifest, ComputerBackendDescriptor, ComputerBackendKind, CursorState, DeliveryMode,
    Effect, Evidence, EvidenceKind, FramePayload, InputRequest, InputResult, InputRoute,
    PermissionReport, PermissionState, RuntimeSessionRole, SessionLeaseRequest, SessionLeaseResult,
    WindowInfo, SCHEMA_VERSION,
};
use cua_input::{InputBackend, RefusingInputBackend};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComputerInstanceState {
    Pending,
    Ready,
    Releasing,
    Released,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComputerAllocationRequest {
    pub provider: String,
    pub pool_id: Option<String>,
    pub region: Option<String>,
    pub ttl_ms: Option<i64>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComputerLease {
    pub lease_id: String,
    pub descriptor: ComputerBackendDescriptor,
    pub expires_wall_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ComputerInstanceStatus {
    pub state: ComputerInstanceState,
    pub descriptor: ComputerBackendDescriptor,
    pub lease_id: Option<String>,
    pub message: Option<String>,
}

pub struct ComputerAllocation {
    pub lease: ComputerLease,
    pub computer: Option<Arc<dyn ComputerBackend>>,
}

#[async_trait]
pub trait ComputerProvider: Send + Sync {
    fn provider_name(&self) -> &'static str;
    async fn allocate(
        &self,
        request: ComputerAllocationRequest,
    ) -> anyhow::Result<ComputerAllocation>;
    async fn status(&self, lease_id: &str) -> anyhow::Result<ComputerInstanceStatus>;
    async fn release(&self, lease_id: &str) -> anyhow::Result<ComputerInstanceStatus>;
}

#[async_trait]
pub trait ComputerBackend: Send + Sync {
    fn descriptor(&self) -> ComputerBackendDescriptor;
    fn capture_backend(&self) -> Arc<dyn CaptureBackend>;
    fn input_backend(&self) -> Arc<dyn InputBackend>;
    async fn permission_report(&self) -> PermissionReport;
    async fn request_accessibility_input_access(&self) -> PermissionState;
    async fn cursor_state(&self) -> CursorState;
    async fn window_list(&self) -> anyhow::Result<Vec<WindowInfo>>;
}

#[derive(Debug, Clone)]
pub struct RemoteCuaConfig {
    pub kind: ComputerBackendKind,
    pub endpoint: String,
    pub bearer_token: String,
    pub owner_session_id: Option<String>,
    pub provider: String,
    pub instance_id: Option<String>,
    pub pool_id: Option<String>,
    pub region: Option<String>,
    pub os: String,
}

impl RemoteCuaConfig {
    pub fn new(endpoint: impl Into<String>, bearer_token: impl Into<String>) -> Self {
        Self {
            kind: ComputerBackendKind::RemoteCua,
            endpoint: endpoint.into(),
            bearer_token: bearer_token.into(),
            owner_session_id: None,
            provider: "remote-cua".to_string(),
            instance_id: None,
            pool_id: None,
            region: None,
            os: "unknown".to_string(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RemoteCuaComputerBackend {
    descriptor: ComputerBackendDescriptor,
    client: reqwest::Client,
    endpoint: String,
    bearer_token: String,
    owner_session_id: Option<String>,
}

impl RemoteCuaComputerBackend {
    pub fn new(config: RemoteCuaConfig) -> anyhow::Result<Self> {
        let endpoint = normalize_endpoint(&config.endpoint)?;
        let capabilities = remote_cua_capabilities();
        Ok(Self {
            descriptor: ComputerBackendDescriptor {
                kind: config.kind,
                provider: config.provider,
                runtime: "cua".to_string(),
                instance_id: config.instance_id,
                pool_id: config.pool_id,
                region: config.region,
                os: config.os,
                capabilities,
            },
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .context("build remote cua HTTP client")?,
            endpoint,
            bearer_token: config.bearer_token,
            owner_session_id: config.owner_session_id,
        })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let response = self
            .client
            .get(self.url(path))
            .bearer_auth(&self.bearer_token)
            .send()
            .await
            .with_context(|| format!("GET remote cua {path}"))?;
        ensure_success(response, "GET", path)
            .await?
            .json()
            .await
            .with_context(|| format!("decode remote cua {path} response"))
    }

    async fn post_json<B: Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        owner_session_id: Option<&str>,
    ) -> anyhow::Result<T> {
        let mut request = self
            .client
            .post(self.url(path))
            .bearer_auth(&self.bearer_token)
            .json(body);
        if let Some(owner_session_id) = owner_session_id {
            request = request.header("x-cua-session-id", owner_session_id);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("POST remote cua {path}"))?;
        ensure_success(response, "POST", path)
            .await?
            .json()
            .await
            .with_context(|| format!("decode remote cua {path} response"))
    }

    async fn acquire_owner_session(&self) -> anyhow::Result<String> {
        if let Some(session_id) = self.owner_session_id.as_deref() {
            return Ok(session_id.to_string());
        }
        let session_id = Uuid::new_v4().to_string();
        let lease: SessionLeaseResult = self
            .post_json(
                "/session/acquire",
                &SessionLeaseRequest {
                    schema_version: SCHEMA_VERSION.to_string(),
                    session_id: session_id.clone(),
                    role: RuntimeSessionRole::Owner,
                    client_name: "remote-cua-computer-backend".to_string(),
                    ttl_ms: Some(300_000),
                },
                None,
            )
            .await?;
        Ok(lease.owner_session_id.unwrap_or(session_id))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.endpoint, path)
    }
}

async fn ensure_success(
    response: reqwest::Response,
    method: &str,
    path: &str,
) -> anyhow::Result<reqwest::Response> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    anyhow::bail!("{method} remote cua {path} returned HTTP {status}: {body}");
}

#[async_trait]
impl ComputerBackend for RemoteCuaComputerBackend {
    fn descriptor(&self) -> ComputerBackendDescriptor {
        self.descriptor.clone()
    }

    fn capture_backend(&self) -> Arc<dyn CaptureBackend> {
        Arc::new(RemoteCuaCaptureBackend {
            backend: self.clone(),
        })
    }

    fn input_backend(&self) -> Arc<dyn InputBackend> {
        Arc::new(RemoteCuaInputBackend {
            backend: self.clone(),
        })
    }

    async fn permission_report(&self) -> PermissionReport {
        self.get_json::<cua_core::DesktopState>("/observe/desktop")
            .await
            .map(|desktop| desktop.permissions)
            .unwrap_or_else(|_| PermissionReport::conservative_unknown())
    }

    async fn request_accessibility_input_access(&self) -> PermissionState {
        PermissionState::NotApplicable
    }

    async fn cursor_state(&self) -> CursorState {
        self.get_json::<CursorState>("/observe/cursor")
            .await
            .unwrap_or(CursorState {
                x: 0.0,
                y: 0.0,
                visible: false,
                included_in_frame: false,
            })
    }

    async fn window_list(&self) -> anyhow::Result<Vec<WindowInfo>> {
        Ok(self
            .get_json::<cua_core::DesktopState>("/observe/desktop")
            .await?
            .windows)
    }
}

struct RemoteCuaCaptureBackend {
    backend: RemoteCuaComputerBackend,
}

#[async_trait]
impl CaptureBackend for RemoteCuaCaptureBackend {
    async fn capture_latest(&self, request: CaptureRequest) -> anyhow::Result<CapturedFrame> {
        let started = Instant::now();
        let payload: FramePayload = self
            .backend
            .post_json(
                "/capture/screenshot",
                &serde_json::json!({
                    "max_width": request.max_width,
                    "encoding": request.encoding,
                    "force_fresh": request.force_fresh,
                    "include_bytes": true
                }),
                None,
            )
            .await?;
        let bytes_base64 = payload
            .bytes_base64
            .as_deref()
            .context("remote cua screenshot omitted bytes")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(bytes_base64)
            .context("decode remote cua screenshot bytes")?;
        Ok(CapturedFrame {
            envelope: payload.envelope,
            bytes: Arc::new(bytes),
            timings: CapturedFrameTimings {
                capture_ns: started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                encode_ns: 0,
                source: CaptureSource::Resident,
            },
        })
    }

    async fn displays(&self) -> anyhow::Result<Vec<cua_core::DisplayInfo>> {
        Ok(self
            .backend
            .get_json::<cua_core::DesktopState>("/observe/desktop")
            .await?
            .displays)
    }

    fn name(&self) -> &'static str {
        "remote-cua"
    }
}

struct RemoteCuaInputBackend {
    backend: RemoteCuaComputerBackend,
}

#[async_trait]
impl InputBackend for RemoteCuaInputBackend {
    async fn execute(&self, request: InputRequest) -> InputResult {
        let started = Instant::now();
        let owner_session_id = match self.backend.acquire_owner_session().await {
            Ok(owner_session_id) => owner_session_id,
            Err(error) => return refused_remote_input(request, started, error.to_string()),
        };
        match self
            .backend
            .post_json::<_, InputResult>(
                "/input/dispatch",
                &request.action,
                Some(owner_session_id.as_str()),
            )
            .await
        {
            Ok(result) => result,
            Err(error) => refused_remote_input(request, started, error.to_string()),
        }
    }

    fn name(&self) -> &'static str {
        "remote-cua"
    }
}

fn refused_remote_input(request: InputRequest, started: Instant, message: String) -> InputResult {
    InputResult {
        schema_version: SCHEMA_VERSION.to_string(),
        idempotency_key: request.idempotency_key,
        effect: Effect::Refused,
        route: InputRoute::Unavailable,
        delivery_mode: DeliveryMode::Unknown,
        started_mono_ns: 0,
        ended_mono_ns: started.elapsed().as_nanos(),
        evidence: vec![Evidence {
            kind: EvidenceKind::Refusal,
            message,
            frame_id: None,
        }],
    }
}

fn normalize_endpoint(endpoint: &str) -> anyhow::Result<String> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    anyhow::ensure!(!endpoint.is_empty(), "remote cua endpoint is required");
    anyhow::ensure!(
        endpoint.starts_with("http://") || endpoint.starts_with("https://"),
        "remote cua endpoint must start with http:// or https://"
    );
    Ok(endpoint.to_string())
}

fn remote_cua_capabilities() -> CapabilityManifest {
    CapabilityManifest {
        actions: vec![
            "observe".to_string(),
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
            "pause".to_string(),
            "resume".to_string(),
            "kill_switch".to_string(),
        ],
        displays: vec!["primary".to_string()],
        apps: Vec::new(),
        clipboard: true,
        model_egress: false,
        max_fps: 30,
    }
}

#[derive(Debug, Clone)]
pub struct OciCliProvider {
    pub config_file: Option<PathBuf>,
    pub profile: String,
    pub cli_path: PathBuf,
}

impl Default for OciCliProvider {
    fn default() -> Self {
        Self {
            config_file: None,
            profile: "DEFAULT".to_string(),
            cli_path: PathBuf::from("oci"),
        }
    }
}

impl OciCliProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn tenancy_namespace(&self) -> anyhow::Result<String> {
        let output = self
            .run_json(["os", "ns", "get"])
            .await
            .context("query OCI object storage namespace")?;
        output
            .get("data")
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
            .context("OCI namespace response missing data")
    }

    pub async fn availability_domains(&self, compartment_id: &str) -> anyhow::Result<Vec<String>> {
        let output = self
            .run_json([
                "iam",
                "availability-domain",
                "list",
                "--compartment-id",
                compartment_id,
            ])
            .await
            .context("list OCI availability domains")?;
        Ok(output
            .get("data")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|item| item.get("name").and_then(|value| value.as_str()))
            .map(ToString::to_string)
            .collect())
    }

    async fn run_json<I, S>(&self, args: I) -> anyhow::Result<serde_json::Value>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut command = Command::new(&self.cli_path);
        for arg in self.base_args(args) {
            command.arg(arg);
        }
        let output = command.output().await.context("run oci cli")?;
        if !output.status.success() {
            anyhow::bail!(
                "oci cli failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        serde_json::from_slice(&output.stdout).context("decode oci cli JSON output")
    }

    fn base_args<I, S>(&self, args: I) -> Vec<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out = args
            .into_iter()
            .map(|arg| arg.as_ref().to_string())
            .collect::<Vec<_>>();
        if let Some(config_file) = self.config_file.as_ref() {
            out.push("--config-file".to_string());
            out.push(config_file.display().to_string());
        }
        out.push("--profile".to_string());
        out.push(self.profile.clone());
        out.push("--output".to_string());
        out.push("json".to_string());
        out
    }
}

#[async_trait]
impl ComputerProvider for OciCliProvider {
    fn provider_name(&self) -> &'static str {
        "oracle-vm"
    }

    async fn allocate(
        &self,
        request: ComputerAllocationRequest,
    ) -> anyhow::Result<ComputerAllocation> {
        let metadata = &request.metadata;
        let compartment_id = required_metadata(metadata, "compartment_id")?;
        let availability_domain = required_metadata(metadata, "availability_domain")?;
        let subnet_id = required_metadata(metadata, "subnet_id")?;
        let image_id = required_metadata(metadata, "image_id")?;
        let shape = metadata
            .get("shape")
            .map(String::as_str)
            .unwrap_or("VM.Standard.A1.Flex");
        let display_name = metadata
            .get("display_name")
            .cloned()
            .unwrap_or_else(|| format!("cua-{}", Uuid::new_v4().simple()));
        let mut args = vec![
            "compute".to_string(),
            "instance".to_string(),
            "launch".to_string(),
            "--compartment-id".to_string(),
            compartment_id.to_string(),
            "--availability-domain".to_string(),
            availability_domain.to_string(),
            "--subnet-id".to_string(),
            subnet_id.to_string(),
            "--image-id".to_string(),
            image_id.to_string(),
            "--shape".to_string(),
            shape.to_string(),
            "--display-name".to_string(),
            display_name,
            "--freeform-tags".to_string(),
            serde_json::json!({
                "cua_provider": "oracle-vm",
                "cua_pool_id": request.pool_id,
            })
            .to_string(),
        ];
        if let (Some(ocpus), Some(memory_gbs)) = (metadata.get("ocpus"), metadata.get("memory_gbs"))
        {
            args.push("--shape-config".to_string());
            args.push(
                serde_json::json!({
                    "ocpus": ocpus.parse::<f32>().context("parse OCI shape ocpus")?,
                    "memoryInGBs": memory_gbs
                        .parse::<f32>()
                        .context("parse OCI shape memory_gbs")?,
                })
                .to_string(),
            );
        }
        if let Some(ssh_keys) = metadata.get("ssh_authorized_keys") {
            args.push("--ssh-authorized-keys-file".to_string());
            args.push(ssh_keys.clone());
        }
        if let Some(cloud_init) = metadata.get("cloud_init_file") {
            args.push("--user-data-file".to_string());
            args.push(cloud_init.clone());
        }
        let output = self.run_json(args).await.context("launch OCI instance")?;
        let instance = output
            .get("data")
            .context("OCI launch response missing data")?;
        let instance_id = instance
            .get("id")
            .and_then(|value| value.as_str())
            .context("OCI launch response missing instance id")?
            .to_string();
        let region = request
            .region
            .or_else(|| metadata.get("region").cloned())
            .or_else(|| std::env::var("OCI_REGION").ok());
        let descriptor = ComputerBackendDescriptor {
            kind: ComputerBackendKind::OracleVm,
            provider: "oracle-vm".to_string(),
            runtime: "cua".to_string(),
            instance_id: Some(instance_id.clone()),
            pool_id: request.pool_id,
            region,
            os: metadata
                .get("os")
                .cloned()
                .unwrap_or_else(|| "linux".to_string()),
            capabilities: oci_lifecycle_capabilities(),
        };
        let remote = match (metadata.get("cua_endpoint"), metadata.get("cua_token")) {
            (Some(endpoint), Some(token)) => {
                Some(Arc::new(RemoteCuaComputerBackend::new(RemoteCuaConfig {
                    kind: ComputerBackendKind::OracleVm,
                    endpoint: endpoint.clone(),
                    bearer_token: token.clone(),
                    owner_session_id: metadata.get("cua_owner_session_id").cloned(),
                    provider: "oracle-vm".to_string(),
                    instance_id: Some(instance_id.clone()),
                    pool_id: descriptor.pool_id.clone(),
                    region: descriptor.region.clone(),
                    os: descriptor.os.clone(),
                })?) as Arc<dyn ComputerBackend>)
            }
            _ => None,
        };
        Ok(ComputerAllocation {
            lease: ComputerLease {
                lease_id: instance_id,
                descriptor,
                expires_wall_ms: request
                    .ttl_ms
                    .map(|ttl_ms| cua_core::now_wall_ms() + ttl_ms),
            },
            computer: remote,
        })
    }

    async fn status(&self, lease_id: &str) -> anyhow::Result<ComputerInstanceStatus> {
        let output = self
            .run_json(["compute", "instance", "get", "--instance-id", lease_id])
            .await
            .context("get OCI instance")?;
        let instance = output
            .get("data")
            .context("OCI get response missing data")?;
        let state = match instance
            .get("lifecycle-state")
            .and_then(|value| value.as_str())
            .unwrap_or("UNKNOWN")
        {
            "RUNNING" => ComputerInstanceState::Ready,
            "TERMINATING" | "STOPPING" => ComputerInstanceState::Releasing,
            "TERMINATED" | "STOPPED" => ComputerInstanceState::Released,
            "PROVISIONING" | "STARTING" | "MOVING" => ComputerInstanceState::Pending,
            _ => ComputerInstanceState::Failed,
        };
        Ok(ComputerInstanceStatus {
            state,
            descriptor: ComputerBackendDescriptor {
                kind: ComputerBackendKind::OracleVm,
                provider: "oracle-vm".to_string(),
                runtime: "cua".to_string(),
                instance_id: Some(lease_id.to_string()),
                pool_id: None,
                region: None,
                os: "linux".to_string(),
                capabilities: oci_lifecycle_capabilities(),
            },
            lease_id: Some(lease_id.to_string()),
            message: instance
                .get("lifecycle-state")
                .and_then(|value| value.as_str())
                .map(ToString::to_string),
        })
    }

    async fn release(&self, lease_id: &str) -> anyhow::Result<ComputerInstanceStatus> {
        let _ = self
            .run_json([
                "compute",
                "instance",
                "terminate",
                "--instance-id",
                lease_id,
                "--force",
            ])
            .await
            .context("terminate OCI instance")?;
        self.status(lease_id).await
    }
}

fn required_metadata<'a>(
    metadata: &'a BTreeMap<String, String>,
    key: &str,
) -> anyhow::Result<&'a str> {
    metadata
        .get(key)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("OCI allocation metadata {key} is required"))
}

fn oci_lifecycle_capabilities() -> CapabilityManifest {
    CapabilityManifest {
        actions: Vec::new(),
        displays: Vec::new(),
        apps: Vec::new(),
        clipboard: false,
        model_egress: false,
        max_fps: 0,
    }
}

#[derive(Debug, Default)]
pub struct SyntheticComputerBackend {
    input: Arc<RefusingInputBackend>,
}

#[async_trait]
impl ComputerBackend for SyntheticComputerBackend {
    fn descriptor(&self) -> ComputerBackendDescriptor {
        ComputerBackendDescriptor::synthetic()
    }

    fn capture_backend(&self) -> Arc<dyn CaptureBackend> {
        Arc::new(SyntheticCaptureBackend::default())
    }

    fn input_backend(&self) -> Arc<dyn InputBackend> {
        self.input.clone()
    }

    async fn permission_report(&self) -> PermissionReport {
        PermissionReport {
            screen_recording: PermissionState::NotApplicable,
            accessibility_input: PermissionState::NotApplicable,
            input_monitoring: PermissionState::NotApplicable,
            automation: PermissionState::NotApplicable,
            clipboard: PermissionState::NotApplicable,
            portal: PermissionState::NotApplicable,
        }
    }

    async fn request_accessibility_input_access(&self) -> PermissionState {
        PermissionState::NotApplicable
    }

    async fn cursor_state(&self) -> CursorState {
        CursorState {
            x: 640.0,
            y: 360.0,
            visible: true,
            included_in_frame: false,
        }
    }

    async fn window_list(&self) -> anyhow::Result<Vec<WindowInfo>> {
        Ok(Vec::new())
    }
}

#[derive(Debug)]
pub struct UnavailableComputerBackend {
    reason: String,
    input: Arc<UnavailableComputerInputBackend>,
}

impl UnavailableComputerBackend {
    pub fn new(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            input: Arc::new(UnavailableComputerInputBackend {
                reason: reason.clone(),
            }),
            reason,
        }
    }
}

#[async_trait]
impl ComputerBackend for UnavailableComputerBackend {
    fn descriptor(&self) -> ComputerBackendDescriptor {
        ComputerBackendDescriptor {
            kind: ComputerBackendKind::Unavailable,
            provider: "unavailable".to_string(),
            runtime: "cua".to_string(),
            instance_id: None,
            pool_id: None,
            region: None,
            os: "unknown".to_string(),
            capabilities: oci_lifecycle_capabilities(),
        }
    }

    fn capture_backend(&self) -> Arc<dyn CaptureBackend> {
        Arc::new(cua_capture::UnavailableCaptureBackend::new(
            self.reason.clone(),
        ))
    }

    fn input_backend(&self) -> Arc<dyn InputBackend> {
        self.input.clone()
    }

    async fn permission_report(&self) -> PermissionReport {
        PermissionReport::conservative_unknown()
    }

    async fn request_accessibility_input_access(&self) -> PermissionState {
        PermissionState::Unknown
    }

    async fn cursor_state(&self) -> CursorState {
        CursorState {
            x: 0.0,
            y: 0.0,
            visible: false,
            included_in_frame: false,
        }
    }

    async fn window_list(&self) -> anyhow::Result<Vec<WindowInfo>> {
        anyhow::bail!("{}", self.reason)
    }
}

#[derive(Debug)]
struct UnavailableComputerInputBackend {
    reason: String,
}

#[async_trait]
impl InputBackend for UnavailableComputerInputBackend {
    async fn execute(&self, request: InputRequest) -> InputResult {
        let started = Instant::now();
        InputResult {
            schema_version: SCHEMA_VERSION.to_string(),
            idempotency_key: request.idempotency_key,
            effect: Effect::Refused,
            route: InputRoute::Unavailable,
            delivery_mode: DeliveryMode::NotApplicable,
            started_mono_ns: 0,
            ended_mono_ns: started.elapsed().as_nanos(),
            evidence: vec![Evidence {
                kind: EvidenceKind::Refusal,
                message: self.reason.clone(),
                frame_id: None,
            }],
        }
    }

    fn name(&self) -> &'static str {
        "unavailable-computer"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cua_core::{InputAction, MouseButton};
    use uuid::Uuid;

    #[tokio::test]
    async fn synthetic_computer_is_observable_but_refuses_real_input() {
        let backend = SyntheticComputerBackend::default();
        let descriptor = backend.descriptor();

        assert_eq!(descriptor.kind, ComputerBackendKind::Synthetic);
        assert_eq!(
            backend.window_list().await.unwrap(),
            Vec::<WindowInfo>::new()
        );

        let result = backend
            .input_backend()
            .execute(InputRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                idempotency_key: Uuid::new_v4(),
                deadline_mono_ns: None,
                action: InputAction::MouseClick {
                    x: 10,
                    y: 10,
                    button: MouseButton::Left,
                    count: 1,
                },
            })
            .await;

        assert_eq!(result.effect, Effect::Refused);
    }

    #[tokio::test]
    async fn unavailable_computer_reports_unavailable_without_remote_claims() {
        let backend = UnavailableComputerBackend::new("cloud computer is not connected");
        let descriptor = backend.descriptor();

        assert_eq!(descriptor.kind, ComputerBackendKind::Unavailable);
        assert_eq!(descriptor.provider, "unavailable");
        assert!(descriptor.capabilities.actions.is_empty());
        assert!(descriptor.capabilities.displays.is_empty());
        assert!(!descriptor.capabilities.clipboard);
        assert_eq!(descriptor.capabilities.max_fps, 0);
        assert!(backend
            .window_list()
            .await
            .unwrap_err()
            .to_string()
            .contains("not connected"));
    }

    #[test]
    fn remote_cua_config_rejects_non_http_endpoint() {
        let error = RemoteCuaComputerBackend::new(RemoteCuaConfig::new("localhost:8765", "token"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("http:// or https://"));
    }

    #[test]
    fn remote_cua_descriptor_preserves_cloud_identity() {
        let backend = RemoteCuaComputerBackend::new(RemoteCuaConfig {
            kind: ComputerBackendKind::RemoteCua,
            endpoint: "https://cua.example.test/".to_string(),
            bearer_token: "token".to_string(),
            owner_session_id: Some("owner".to_string()),
            provider: "oracle-vm".to_string(),
            instance_id: Some("ocid1.instance.example".to_string()),
            pool_id: Some("pool-a".to_string()),
            region: Some("us-ashburn-1".to_string()),
            os: "linux".to_string(),
        })
        .unwrap();

        let descriptor = backend.descriptor();
        assert_eq!(descriptor.kind, ComputerBackendKind::RemoteCua);
        assert_eq!(descriptor.provider, "oracle-vm");
        assert_eq!(
            descriptor.instance_id.as_deref(),
            Some("ocid1.instance.example")
        );
        assert!(descriptor
            .capabilities
            .actions
            .contains(&"mouse_click".to_string()));
    }

    #[test]
    fn oci_provider_base_args_preserve_profile_config_and_json_output() {
        let provider = OciCliProvider {
            config_file: Some(PathBuf::from("/tmp/oci-config")),
            profile: "quilt".to_string(),
            cli_path: PathBuf::from("oci"),
        };

        assert_eq!(
            provider.base_args(["os", "ns", "get"]),
            vec![
                "os",
                "ns",
                "get",
                "--config-file",
                "/tmp/oci-config",
                "--profile",
                "quilt",
                "--output",
                "json"
            ]
        );
    }

    #[test]
    fn oci_allocation_requires_real_launch_metadata() {
        let metadata = BTreeMap::new();
        let error = required_metadata(&metadata, "subnet_id")
            .unwrap_err()
            .to_string();
        assert!(error.contains("subnet_id"));
    }
}
