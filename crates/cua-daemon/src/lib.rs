use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocketUpgrade},
        Path as AxumPath, Query, Request, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use chrono::Utc;
use cua_capture::{
    encode_image, CaptureRequest, CaptureSource, CapturedFrame, CapturedFrameTimings, FrameBus,
    FrameLookup,
};
use cua_computer::{ComputerBackend, SyntheticComputerBackend};
#[cfg(not(test))]
use cua_computer::{RemoteCuaComputerBackend, RemoteCuaConfig, UnavailableComputerBackend};
use cua_core::{
    load_or_create_machine_identity, now_wall_ms, profile_daemon_trace_dir,
    profile_scratchpads_dir, profile_socket_path, profile_token_path, schema_bundle,
    sign_machine_attestation, ApiErrorBody, AttestationChallenge, AttestationChallengeRequest,
    AttestationSignRequest, CapabilityManifest, CapabilityState, ClipboardReadRequest,
    ClipboardResult, ClipboardWriteRequest, ConfigInventory, DeliveryMode, DesktopContextSnapshot,
    DesktopState, Effect, Evidence, EvidenceKind, FrameActionRequest, FrameEncoding, FramePayload,
    HealthReport, InboundDeliveryMethod, InboundMessage, InboundMessageRequest,
    InboundMessageState, InboundReplyMode, InboundStatus, InputAction, InputRequest, InputResult,
    InputRoute, MachineAttestation, MachineIdentityStatus, Manifest, MetricBucket, MetricHistogram,
    MetricsSnapshot, PermissionReport, PermissionState, ProfilePolicy, RuntimeControlState,
    RuntimeIdentityClaims, RuntimeInventory, RuntimeMode, RuntimeSessionInfo, RuntimeSessionRole,
    SafetyState, ScratchpadDeleteRequest, ScratchpadDeleteResult, ScratchpadEntry, ScratchpadKind,
    ScratchpadListRequest, ScratchpadListResult, ScratchpadReadRequest, ScratchpadSummary,
    ScratchpadWriteRequest, SessionCancelRequest, SessionHeartbeatRequest, SessionLeaseRequest,
    SessionLeaseResult, UiIslandRequest, UiIslandResult, UiMode, UiModeRequest, UiModeResult,
    UiReplyRequest, UiReplyResult, UiSceneBackgroundRequest, UiScenePatchRequest, UiSceneRequest,
    UiSceneResetRequest, UiSceneResult, UiSceneThemeRequest, UiStepRequest, UiStepResult,
    VisualSessionRequest, WebhookSourceStatus, WebhookSubscribeRequest, WindowInfo, SCHEMA_VERSION,
};
use cua_input::InputBackend;
use cua_model::{run_eval_report, EvalConfig, EvalReport};
use cua_trace::{ActionTurnRecord, TraceRecord, TraceWriter};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex as StdMutex,
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, Notify, RwLock, Semaphore};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;
const INBOX_REPLY_MAX_CHARS: usize = 8_192;

#[derive(Clone)]
pub struct DaemonState {
    pub profile: String,
    pub started_at: chrono::DateTime<Utc>,
    computer: Arc<dyn ComputerBackend>,
    pub frame_bus: Arc<FrameBus>,
    encode_lane: EncodeLane,
    input_lane: InputLane,
    model_lane: ModelLane,
    permission_lane: PermissionLane,
    pub active_streams: Arc<AtomicU32>,
    pub bearer_token: Arc<String>,
    pub control: Arc<RwLock<RuntimeControlState>>,
    pub clipboard: Arc<RwLock<Option<String>>>,
    metrics: Arc<RuntimeMetrics>,
    events: EventLane,
    trace_lane: Option<TraceLane>,
    ui_step_context: Arc<StdMutex<Option<UiStepContext>>>,
    hud_supervisor: HudSupervisor,
    hud_mode: UiMode,
    http_addr: Arc<StdMutex<String>>,
    profile_socket: Arc<StdMutex<String>>,
    sessions: SessionRegistry,
    inbox: InboxRegistry,
    attestation_challenges: Arc<StdMutex<BTreeMap<String, AttestationChallenge>>>,
}

#[derive(Debug, Clone)]
struct UiStepContext {
    expires_at: Option<Instant>,
}

impl DaemonState {
    pub fn default(profile: impl Into<String>, bearer_token: impl Into<String>) -> Self {
        Self::default_with_hud_mode(profile, bearer_token, UiMode::Headful)
    }

    pub fn default_with_hud_mode(
        profile: impl Into<String>,
        bearer_token: impl Into<String>,
        hud_mode: UiMode,
    ) -> Self {
        Self::with_computer_backend(profile, bearer_token, hud_mode, default_computer_backend())
    }

    pub fn synthetic(profile: impl Into<String>, bearer_token: impl Into<String>) -> Self {
        Self::with_computer_backend(
            profile,
            bearer_token,
            UiMode::Headful,
            Arc::new(SyntheticComputerBackend::default()),
        )
    }

    pub fn with_computer_backend(
        profile: impl Into<String>,
        bearer_token: impl Into<String>,
        hud_mode: UiMode,
        computer: Arc<dyn ComputerBackend>,
    ) -> Self {
        let profile = profile.into();
        let capture = computer.capture_backend();
        let input = computer.input_backend();
        let events = EventLane::spawn(event_lane_capacity(), event_lane_retention());
        let state = Self {
            profile: profile.clone(),
            started_at: Utc::now(),
            computer: computer.clone(),
            frame_bus: Arc::new(FrameBus::new(capture)),
            encode_lane: EncodeLane::spawn(encode_lane_capacity()),
            input_lane: InputLane::spawn(input.clone(), input_lane_capacity()),
            model_lane: ModelLane::spawn(model_lane_capacity()),
            permission_lane: PermissionLane::spawn(computer, permission_lane_capacity()),
            active_streams: Arc::new(AtomicU32::new(0)),
            bearer_token: Arc::new(bearer_token.into()),
            control: Arc::new(RwLock::new(default_control_state(&profile))),
            clipboard: Arc::new(RwLock::new(None)),
            metrics: Arc::new(RuntimeMetrics::default()),
            events,
            trace_lane: trace_dir_from_env_or_profile(&profile)
                .and_then(build_trace_writer)
                .map(|writer| TraceLane::spawn(writer, trace_lane_capacity())),
            ui_step_context: Arc::new(StdMutex::new(None)),
            hud_supervisor: HudSupervisor::default(),
            hud_mode,
            http_addr: Arc::new(StdMutex::new(String::new())),
            profile_socket: Arc::new(StdMutex::new(String::new())),
            sessions: SessionRegistry::default(),
            inbox: InboxRegistry::default(),
            attestation_challenges: Arc::new(StdMutex::new(BTreeMap::new())),
        };
        state.publish_event("daemon_started", serde_json::json!({}));
        state
    }

    pub async fn health(&self) -> HealthReport {
        let permissions = self.permission_report().await;
        let control = self.control.read().await;
        let latest_frame = self.frame_bus.latest_envelope().await;
        let status = health_status(&permissions, latest_frame.is_some(), &control.safety_state);
        HealthReport {
            schema_version: SCHEMA_VERSION.to_string(),
            status,
            version: env!("CARGO_PKG_VERSION").to_string(),
            profile: self.profile.clone(),
            started_at: self.started_at,
            permissions,
            latest_frame,
            safety_state: control.safety_state.clone(),
            active_profile: control.active_profile.name.clone(),
            active_streams: self.active_streams.load(Ordering::Relaxed),
            model_sessions: self.model_lane.active_count(),
            computer_backend: self.computer.descriptor(),
            inventory: self.runtime_inventory().await,
            last_error: None,
        }
    }

    async fn runtime_inventory(&self) -> RuntimeInventory {
        let session_snapshot = self.sessions.snapshot();
        RuntimeInventory {
            schema_version: SCHEMA_VERSION.to_string(),
            daemon_pid: std::process::id(),
            http_addr: self
                .http_addr
                .lock()
                .map(|value| value.clone())
                .unwrap_or_default(),
            profile_socket: self
                .profile_socket
                .lock()
                .map(|value| value.clone())
                .unwrap_or_default(),
            computer_backend: self.computer.descriptor(),
            config: config_inventory_state(self),
            hud_pid: self.hud_supervisor.pid().or_else(discover_hud_pid),
            connected_clients: session_snapshot.sessions.len() as u32,
            owner_session_id: session_snapshot.owner_session_id,
            sessions: session_snapshot.sessions,
        }
    }
}

fn health_status(
    permissions: &PermissionReport,
    has_latest_frame: bool,
    safety_state: &SafetyState,
) -> CapabilityState {
    if matches!(safety_state, SafetyState::Killed) {
        return CapabilityState::Refused;
    }
    let required_permissions_ready =
        matches!(permissions.screen_recording, PermissionState::Granted)
            && matches!(
                permissions.accessibility_input,
                PermissionState::Granted | PermissionState::NotApplicable
            )
            && matches!(
                permissions.input_monitoring,
                PermissionState::Granted | PermissionState::NotApplicable
            );
    if required_permissions_ready && has_latest_frame {
        CapabilityState::Ready
    } else {
        CapabilityState::Degraded
    }
}

#[cfg(test)]
fn default_computer_backend() -> Arc<dyn ComputerBackend> {
    Arc::new(SyntheticComputerBackend::default())
}

#[cfg(not(test))]
fn default_computer_backend() -> Arc<dyn ComputerBackend> {
    match std::env::var("CUA_COMPUTER_BACKEND")
        .unwrap_or_else(|_| "local".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "local" | "macos" => configured_local_computer_backend(),
        "remote" | "remote_cua" | "remote-cua" => configured_remote_cua_backend(
            cua_core::ComputerBackendKind::RemoteCua,
            std::env::var("CUA_REMOTE_CUA_PROVIDER").unwrap_or_else(|_| "remote-cua".to_string()),
        ),
        "oracle-vm" => configured_cloud_node_computer_backend(
            cua_core::ComputerBackendKind::OracleVm,
            "oracle-vm",
        ),
        "quilt" | "quilt_vm" | "quilt-vm" => configured_cloud_node_computer_backend(
            cua_core::ComputerBackendKind::QuiltVm,
            "quilt-vm",
        ),
        other => Arc::new(UnavailableComputerBackend::new(format!(
            "unknown CUA_COMPUTER_BACKEND '{other}'; valid values are local, remote-cua, oracle-vm, and quilt-vm"
        ))),
    }
}

#[cfg(not(test))]
fn configured_cloud_node_computer_backend(
    kind: cua_core::ComputerBackendKind,
    provider: &'static str,
) -> Arc<dyn ComputerBackend> {
    configured_qgui_computer_backend(kind, provider)
}

#[cfg(not(test))]
#[cfg(target_os = "macos")]
fn configured_local_computer_backend() -> Arc<dyn ComputerBackend> {
    cua_platform_macos::computer_backend()
}

#[cfg(not(test))]
#[cfg(target_os = "linux")]
fn configured_qgui_computer_backend(
    kind: cua_core::ComputerBackendKind,
    provider: impl Into<String>,
) -> Arc<dyn ComputerBackend> {
    Arc::new(cua_platform_qgui::QguiComputerBackend::new(
        cua_platform_qgui::QguiBackendConfig::from_env(kind, provider),
    ))
}

#[cfg(not(test))]
#[cfg(not(target_os = "linux"))]
fn configured_qgui_computer_backend(
    _kind: cua_core::ComputerBackendKind,
    _provider: impl Into<String>,
) -> Arc<dyn ComputerBackend> {
    Arc::new(UnavailableComputerBackend::new(
        "qgui computer backend is only implemented for Linux VM hosts",
    ))
}

#[cfg(not(test))]
#[cfg(not(target_os = "macos"))]
fn configured_local_computer_backend() -> Arc<dyn ComputerBackend> {
    Arc::new(UnavailableComputerBackend::new(
        "local computer backend is only implemented for macOS; set CUA_COMPUTER_BACKEND=remote-cua, CUA_COMPUTER_BACKEND=oracle-vm, or CUA_COMPUTER_BACKEND=quilt-vm as appropriate for this host",
    ))
}

#[cfg(not(test))]
fn configured_remote_cua_backend(
    kind: cua_core::ComputerBackendKind,
    provider: impl Into<String>,
) -> Arc<dyn ComputerBackend> {
    let endpoint = match std::env::var("CUA_REMOTE_CUA_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            return Arc::new(UnavailableComputerBackend::new(
                "CUA_REMOTE_CUA_URL is required for remote computer backends",
            ))
        }
    };
    let bearer_token = match std::env::var("CUA_REMOTE_CUA_TOKEN") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            return Arc::new(UnavailableComputerBackend::new(
                "CUA_REMOTE_CUA_TOKEN is required for remote computer backends",
            ))
        }
    };
    match RemoteCuaComputerBackend::new(RemoteCuaConfig {
        kind,
        endpoint,
        bearer_token,
        owner_session_id: std::env::var("CUA_REMOTE_CUA_OWNER_SESSION_ID").ok(),
        provider: provider.into(),
        instance_id: std::env::var("CUA_REMOTE_CUA_INSTANCE_ID").ok(),
        pool_id: std::env::var("CUA_REMOTE_CUA_POOL_ID").ok(),
        region: std::env::var("CUA_REMOTE_CUA_REGION")
            .ok()
            .or_else(|| std::env::var("OCI_REGION").ok()),
        os: std::env::var("CUA_REMOTE_CUA_OS").unwrap_or_else(|_| "linux".to_string()),
    }) {
        Ok(backend) => Arc::new(backend),
        Err(error) => Arc::new(UnavailableComputerBackend::new(format!(
            "remote computer backend is misconfigured: {error}"
        ))),
    }
}

impl DaemonState {
    fn publish_event(&self, kind: &'static str, data: serde_json::Value) {
        if hud_wake_event(kind) {
            let hud_mode = hud_mode_for_event(kind, &data).unwrap_or_else(|| self.hud_mode.clone());
            self.hud_supervisor
                .ensure_running(&self.profile, &self.bearer_token, hud_mode);
        }
        if !self.events.publish(kind, data) {
            self.metrics.increment(CounterKind::EventDrops);
        }
    }

    fn record_programmed_step(&self, result: &UiStepResult) {
        if result.source.as_deref() == Some("cua-runtime") {
            return;
        }
        if let Ok(mut context) = self.ui_step_context.lock() {
            *context = Some(UiStepContext {
                expires_at: result
                    .ttl_ms
                    .map(|ttl_ms| Instant::now() + Duration::from_millis(ttl_ms)),
            });
        }
    }

    fn has_active_programmed_step(&self) -> bool {
        let Ok(mut context) = self.ui_step_context.lock() else {
            return false;
        };
        match context.as_ref().and_then(|context| context.expires_at) {
            Some(deadline) if Instant::now() >= deadline => {
                *context = None;
                false
            }
            Some(_) | None if context.is_some() => true,
            _ => false,
        }
    }

    async fn permission_report(&self) -> PermissionReport {
        let result = self.permission_lane.report().await;
        self.metrics
            .observe(MetricKind::PermissionQueueWait, result.queue_wait);
        self.metrics
            .observe(MetricKind::PermissionProbe, result.probe_duration);
        if result.fallback {
            self.metrics.increment(CounterKind::PermissionFallbacks);
        }
        result.report
    }
}

#[derive(Clone, Default)]
struct HudSupervisor {
    last_attempt_wall_ms: Arc<AtomicI64>,
    pid: Arc<AtomicU32>,
}

impl HudSupervisor {
    fn pid(&self) -> Option<u32> {
        match self.pid.load(Ordering::Relaxed) {
            0 => None,
            pid if process_is_alive(pid) => Some(pid),
            _ => {
                self.pid.store(0, Ordering::Relaxed);
                None
            }
        }
    }

    fn ensure_running(&self, profile: &str, token: &str, mode: UiMode) {
        if std::env::var("CUA_HUD_AUTOSTART").ok().as_deref() == Some("0") {
            return;
        }
        #[cfg(test)]
        {
            let _ = self.last_attempt_wall_ms.load(Ordering::Relaxed);
            let _ = (profile, token, mode);
        }
        #[cfg(not(test))]
        {
            let now = now_wall_ms();
            let last = self.last_attempt_wall_ms.load(Ordering::Relaxed);
            if last > 0 && now.saturating_sub(last) < 2_000 {
                return;
            }
            if self
                .last_attempt_wall_ms
                .compare_exchange(last, now, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
            {
                return;
            }
            let profile = profile.to_string();
            let token = token.to_string();
            let pid = self.pid.clone();
            std::thread::spawn(move || {
                let Some(binary) = hud_binary_path() else {
                    return;
                };
                let mode_arg = match mode {
                    UiMode::Headful => "--headful",
                    UiMode::Headless => "--headless",
                };
                let mut command = Command::new(&binary);
                command
                    .args(["--profile", &profile, mode_arg])
                    .env("CUA_HTTP_TOKEN", token)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                if let Ok(child) = command.spawn() {
                    pid.store(child.id(), Ordering::Relaxed);
                }
            });
        }
    }
}

fn process_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let output = std::process::Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "stat=,command="])
        .output();
    output.is_ok_and(|output| {
        if !output.status.success() || output.stdout.is_empty() {
            return false;
        }
        let state = String::from_utf8_lossy(&output.stdout);
        let trimmed = state.trim_start();
        !trimmed.starts_with('Z') && !state.contains("<defunct>")
    })
}

#[derive(Clone, Default)]
struct SessionRegistry {
    inner: Arc<StdMutex<SessionRegistryState>>,
}

#[derive(Default)]
struct SessionRegistryState {
    sessions: BTreeMap<String, RuntimeSessionInfo>,
    owner_session_id: Option<String>,
}

struct SessionSnapshot {
    sessions: Vec<RuntimeSessionInfo>,
    owner_session_id: Option<String>,
}

impl SessionRegistry {
    fn acquire(&self, request: SessionLeaseRequest) -> Result<SessionLeaseResult, ApiError> {
        if request.schema_version != SCHEMA_VERSION {
            return Err(ApiError::bad_request(
                "schema_version",
                format!("expected {SCHEMA_VERSION}"),
            ));
        }
        let session_id = normalize_session_field(request.session_id, 96, "session_id")?;
        let client_name = normalize_session_field(request.client_name, 80, "client_name")?;
        let now = now_wall_ms();
        let expires_wall_ms = request
            .ttl_ms
            .map(|ttl_ms| now + ttl_ms.clamp(1_000, 86_400_000));
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow::anyhow!("session registry poisoned")))?;
        prune_expired_sessions(&mut inner, now);
        if request.role == RuntimeSessionRole::Owner {
            match inner.owner_session_id.as_deref() {
                Some(owner) if owner != session_id => {
                    return Err(ApiError::conflict(
                        "session_owner",
                        format!("owner session {owner} already holds the write lease"),
                    ));
                }
                _ => inner.owner_session_id = Some(session_id.clone()),
            }
        }
        let session = RuntimeSessionInfo {
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: session_id.clone(),
            role: request.role,
            client_name,
            connected_wall_ms: inner
                .sessions
                .get(&session_id)
                .map(|session| session.connected_wall_ms)
                .unwrap_or(now),
            last_seen_wall_ms: now,
            expires_wall_ms,
            active: true,
        };
        inner.sessions.insert(session_id, session.clone());
        Ok(SessionLeaseResult {
            schema_version: SCHEMA_VERSION.to_string(),
            accepted: true,
            session,
            owner_session_id: inner.owner_session_id.clone(),
        })
    }

    fn cancel(&self, request: SessionCancelRequest) -> Result<(), ApiError> {
        if request.schema_version != SCHEMA_VERSION {
            return Err(ApiError::bad_request(
                "schema_version",
                format!("expected {SCHEMA_VERSION}"),
            ));
        }
        let session_id = normalize_session_field(request.session_id, 96, "session_id")?;
        let target = request
            .target_session_id
            .map(|value| normalize_session_field(value, 96, "target_session_id"))
            .transpose()?
            .unwrap_or_else(|| session_id.clone());
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow::anyhow!("session registry poisoned")))?;
        prune_expired_sessions(&mut inner, now_wall_ms());
        let caller_is_owner = inner.owner_session_id.as_deref() == Some(session_id.as_str());
        if target != session_id && !caller_is_owner {
            return Err(ApiError::forbidden(
                "session_cancel",
                "only the owner can cancel another session",
            ));
        }
        inner.sessions.remove(&target);
        if inner.owner_session_id.as_deref() == Some(target.as_str()) {
            inner.owner_session_id = None;
        }
        Ok(())
    }

    fn heartbeat(&self, request: SessionHeartbeatRequest) -> Result<SessionLeaseResult, ApiError> {
        if request.schema_version != SCHEMA_VERSION {
            return Err(ApiError::bad_request(
                "schema_version",
                format!("expected {SCHEMA_VERSION}"),
            ));
        }
        let session_id = normalize_session_field(request.session_id, 96, "session_id")?;
        let now = now_wall_ms();
        let expires_wall_ms = request
            .ttl_ms
            .map(|ttl_ms| now + ttl_ms.clamp(1_000, 86_400_000));
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow::anyhow!("session registry poisoned")))?;
        prune_expired_sessions(&mut inner, now);
        let owner_session_id = inner.owner_session_id.clone();
        let Some(session) = inner.sessions.get_mut(&session_id) else {
            return Err(ApiError::forbidden(
                "session_lease",
                "session does not hold an active lease",
            ));
        };
        session.last_seen_wall_ms = now;
        session.expires_wall_ms = expires_wall_ms;
        Ok(SessionLeaseResult {
            schema_version: SCHEMA_VERSION.to_string(),
            accepted: true,
            session: session.clone(),
            owner_session_id,
        })
    }

    fn remove(&self, session_id: &str) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.sessions.remove(session_id);
        if inner.owner_session_id.as_deref() == Some(session_id) {
            inner.owner_session_id = None;
        }
    }

    fn authorize_write(&self, session_id: Option<&str>) -> Result<(), ApiError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow::anyhow!("session registry poisoned")))?;
        prune_expired_sessions(&mut inner, now_wall_ms());
        let Some(owner) = inner.owner_session_id.clone() else {
            return Err(ApiError::forbidden(
                "session_owner",
                "write requires an active owner session",
            ));
        };
        let Some(session_id) = session_id else {
            return Err(ApiError::forbidden(
                "session_owner",
                format!("write lease is held by owner session {owner}"),
            ));
        };
        if owner != session_id {
            return Err(ApiError::forbidden(
                "session_owner",
                format!("write lease is held by owner session {owner}"),
            ));
        }
        match inner.sessions.get_mut(session_id) {
            Some(session) if session.role == RuntimeSessionRole::Owner => {
                session.last_seen_wall_ms = now_wall_ms();
                Ok(())
            }
            _ => Err(ApiError::forbidden(
                "session_owner",
                "session does not hold an active owner lease",
            )),
        }
    }

    fn authorize_required_owner(&self, session_id: Option<&str>) -> Result<(), ApiError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow::anyhow!("session registry poisoned")))?;
        prune_expired_sessions(&mut inner, now_wall_ms());
        let Some(owner) = inner.owner_session_id.clone() else {
            return Err(ApiError::forbidden(
                "session_owner_required",
                "HTTP writes require an active owner session; acquire one with /session/acquire and send x-cua-session-id",
            ));
        };
        let Some(session_id) = session_id else {
            return Err(ApiError::forbidden(
                "session_owner",
                format!("write lease is held by owner session {owner}"),
            ));
        };
        if owner != session_id {
            return Err(ApiError::forbidden(
                "session_owner",
                format!("write lease is held by owner session {owner}"),
            ));
        }
        match inner.sessions.get_mut(session_id) {
            Some(session) if session.role == RuntimeSessionRole::Owner => {
                session.last_seen_wall_ms = now_wall_ms();
                Ok(())
            }
            _ => Err(ApiError::forbidden(
                "session_owner",
                "session does not hold an active owner lease",
            )),
        }
    }

    fn snapshot(&self) -> SessionSnapshot {
        let mut inner = self.inner.lock().expect("session registry lock");
        prune_expired_sessions(&mut inner, now_wall_ms());
        SessionSnapshot {
            sessions: inner.sessions.values().cloned().collect(),
            owner_session_id: inner.owner_session_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct InboxRegistry {
    inner: Arc<StdMutex<InboxState>>,
}

#[derive(Debug, Default)]
struct InboxState {
    next_sequence: u64,
    order: VecDeque<String>,
    statuses: BTreeMap<String, InboundStatus>,
    idempotency: BTreeMap<String, String>,
    webhooks: BTreeMap<String, WebhookConfig>,
}

#[derive(Debug, Clone)]
struct WebhookConfig {
    shared_secret: Option<String>,
    reply_url: Option<String>,
    updated_wall_ms: i64,
}

impl InboxRegistry {
    fn publish(
        &self,
        request: InboundMessageRequest,
        delivery_method: InboundDeliveryMethod,
    ) -> Result<InboundStatus, ApiError> {
        validate_inbound_request(&request)?;
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow::anyhow!("inbox registry poisoned")))?;
        if let Some(existing_id) = inner.idempotency.get(&request.idempotency_key).cloned() {
            let mut duplicate = inner.statuses.get(&existing_id).cloned().ok_or_else(|| {
                ApiError::internal(anyhow::anyhow!("inbox idempotency index lost status"))
            })?;
            duplicate.state = InboundMessageState::Duplicate;
            duplicate.updated_wall_ms = now_wall_ms();
            duplicate.message.duplicate_of = Some(existing_id);
            return Ok(duplicate);
        }

        let now = now_wall_ms();
        inner.next_sequence = inner.next_sequence.saturating_add(1);
        let sequence = inner.next_sequence;
        let message_id = Uuid::new_v4().to_string();
        let expires_wall_ms = request
            .ttl_ms
            .map(|ttl_ms| now + ttl_ms.clamp(1_000, 86_400_000));
        let message = InboundMessage {
            schema_version: SCHEMA_VERSION.to_string(),
            sequence,
            message_id: message_id.clone(),
            idempotency_key: request.idempotency_key.clone(),
            source: normalize_inbound_field(&request.source, 96, "source")?,
            text: normalize_inbound_field(&request.text, 8_192, "text")?,
            payload: request.payload,
            reply_mode: request.reply_mode,
            reply_url: request.reply_url,
            delivery_method,
            received_wall_ms: now,
            expires_wall_ms,
            attestation: request.attestation,
            duplicate_of: None,
        };
        let status = InboundStatus {
            schema_version: SCHEMA_VERSION.to_string(),
            message_id: message_id.clone(),
            state: InboundMessageState::Accepted,
            message,
            reply: None,
            error: None,
            updated_wall_ms: now,
        };
        inner
            .idempotency
            .insert(request.idempotency_key, message_id.clone());
        inner.statuses.insert(message_id.clone(), status.clone());
        inner.order.push_back(message_id);
        prune_inbox(&mut inner);
        Ok(status)
    }

    fn after(&self, after_sequence: Option<u64>) -> Result<Vec<InboundStatus>, ApiError> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow::anyhow!("inbox registry poisoned")))?;
        let after = after_sequence.unwrap_or(0);
        Ok(inner
            .order
            .iter()
            .filter_map(|id| inner.statuses.get(id))
            .filter(|status| status.message.sequence > after)
            .cloned()
            .collect())
    }

    fn status(&self, message_id: &str) -> Result<InboundStatus, ApiError> {
        let inner = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow::anyhow!("inbox registry poisoned")))?;
        inner.statuses.get(message_id).cloned().ok_or_else(|| {
            ApiError::bad_request("message_id", "inbox message_id is unknown or expired")
        })
    }

    fn set_running(&self, message_id: &str) -> Result<InboundStatus, ApiError> {
        self.update(message_id, InboundMessageState::Running, None, None)
    }

    fn set_done(&self, message_id: &str, reply: Option<String>) -> Result<InboundStatus, ApiError> {
        self.update(message_id, InboundMessageState::Done, reply, None)
    }

    fn set_failed(&self, message_id: &str, error: String) -> Result<InboundStatus, ApiError> {
        self.update(message_id, InboundMessageState::Failed, None, Some(error))
    }

    fn update(
        &self,
        message_id: &str,
        state: InboundMessageState,
        reply: Option<String>,
        error: Option<String>,
    ) -> Result<InboundStatus, ApiError> {
        let mut inner = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow::anyhow!("inbox registry poisoned")))?;
        let status = inner.statuses.get_mut(message_id).ok_or_else(|| {
            ApiError::bad_request("message_id", "inbox message_id is unknown or expired")
        })?;
        status.state = state;
        status.reply = reply;
        status.error = error;
        status.updated_wall_ms = now_wall_ms();
        Ok(status.clone())
    }

    fn subscribe(&self, request: WebhookSubscribeRequest) -> Result<WebhookSourceStatus, ApiError> {
        if request.schema_version != SCHEMA_VERSION {
            return Err(ApiError::bad_request(
                "schema_version",
                format!("expected {SCHEMA_VERSION}"),
            ));
        }
        let source = normalize_inbound_field(&request.source, 96, "source")?;
        let config = WebhookConfig {
            shared_secret: request
                .shared_secret
                .map(|secret| normalize_inbound_field(&secret, 512, "shared_secret"))
                .transpose()?,
            reply_url: request.reply_url,
            updated_wall_ms: now_wall_ms(),
        };
        let status = webhook_status_from_config(&source, Some(&config));
        self.inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow::anyhow!("inbox registry poisoned")))?
            .webhooks
            .insert(source, config);
        Ok(status)
    }

    fn webhook_status(&self, source: &str) -> Result<WebhookSourceStatus, ApiError> {
        let source = normalize_inbound_field(source, 96, "source")?;
        let inner = self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow::anyhow!("inbox registry poisoned")))?;
        Ok(webhook_status_from_config(
            &source,
            inner.webhooks.get(&source),
        ))
    }

    fn webhook_config(&self, source: &str) -> Result<Option<WebhookConfig>, ApiError> {
        let source = normalize_inbound_field(source, 96, "source")?;
        Ok(self
            .inner
            .lock()
            .map_err(|_| ApiError::internal(anyhow::anyhow!("inbox registry poisoned")))?
            .webhooks
            .get(&source)
            .cloned())
    }
}

fn validate_inbound_request(request: &InboundMessageRequest) -> Result<(), ApiError> {
    if request.schema_version != SCHEMA_VERSION {
        return Err(ApiError::bad_request(
            "schema_version",
            format!("expected {SCHEMA_VERSION}"),
        ));
    }
    normalize_inbound_field(&request.idempotency_key, 128, "idempotency_key")?;
    normalize_inbound_field(&request.source, 96, "source")?;
    normalize_inbound_field(&request.text, 8_192, "text")?;
    if request.reply_mode == InboundReplyMode::Webhook && request.reply_url.is_none() {
        return Err(ApiError::bad_request(
            "reply_url",
            "reply_mode webhook requires reply_url",
        ));
    }
    Ok(())
}

fn normalize_inbound_field(
    value: &str,
    max_chars: usize,
    field: &'static str,
) -> Result<String, ApiError> {
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect::<String>();
    if normalized.is_empty() {
        return Err(ApiError::bad_request(
            field,
            format!("{field} must not be empty"),
        ));
    }
    Ok(normalized)
}

fn prune_inbox(inner: &mut InboxState) {
    let retention = env_usize("CUA_INBOX_RETENTION", 256, 16, 4096);
    while inner.order.len() > retention {
        if let Some(id) = inner.order.pop_front() {
            if let Some(status) = inner.statuses.remove(&id) {
                inner.idempotency.remove(&status.message.idempotency_key);
            }
        }
    }
}

fn webhook_status_from_config(source: &str, config: Option<&WebhookConfig>) -> WebhookSourceStatus {
    WebhookSourceStatus {
        schema_version: SCHEMA_VERSION.to_string(),
        source: source.to_string(),
        configured: config.is_some(),
        requires_signature: config
            .and_then(|config| config.shared_secret.as_ref())
            .is_some(),
        reply_url: config.and_then(|config| config.reply_url.clone()),
        updated_wall_ms: config
            .map(|config| config.updated_wall_ms)
            .unwrap_or_else(now_wall_ms),
    }
}

fn normalize_session_field(
    value: String,
    max_chars: usize,
    field: &'static str,
) -> Result<String, ApiError> {
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect::<String>();
    if normalized.is_empty() {
        return Err(ApiError::bad_request(
            field,
            format!("{field} must not be empty"),
        ));
    }
    Ok(normalized)
}

fn prune_expired_sessions(inner: &mut SessionRegistryState, now: i64) {
    inner
        .sessions
        .retain(|_, session| session.expires_wall_ms.is_none_or(|expires| expires > now));
    if let Some(owner) = inner.owner_session_id.clone() {
        if !inner.sessions.contains_key(&owner) {
            inner.owner_session_id = None;
        }
    }
}

fn hud_wake_event(kind: &str) -> bool {
    matches!(
        kind,
        "ui_step"
            | "ui_reply"
            | "ui_island"
            | "ui_scene"
            | "ui_scene_reset"
            | "ui_scene_theme"
            | "input_started"
            | "input_completed"
            | "input_refused"
            | "control_paused"
            | "control_resumed"
            | "kill_switch"
            | "inbound_message"
            | "visual_session_started"
    )
}

fn hud_mode_for_event(kind: &str, data: &serde_json::Value) -> Option<UiMode> {
    if kind == "ui_scene" {
        return match data
            .get("scene")
            .and_then(|scene| scene.get("mode"))
            .and_then(|value| value.as_str())
        {
            Some("headful") => Some(UiMode::Headful),
            Some("headless") => Some(UiMode::Headless),
            _ => None,
        };
    }
    if kind == "ui_mode" {
        return match data.get("mode").and_then(|value| value.as_str()) {
            Some("headful") => Some(UiMode::Headful),
            Some("headless") => Some(UiMode::Headless),
            _ => None,
        };
    }
    None
}

#[cfg(not(test))]
fn hud_binary_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CUA_VOICE_BIN") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let current = std::env::current_exe().ok()?;
    hud_binary_candidates(&current)
        .into_iter()
        .find(|candidate| candidate.is_file())
}

#[cfg(not(test))]
fn hud_binary_candidates(current: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(parent) = current.parent() {
        candidates.push(parent.join("cua-voice"));
        candidates.push(
            parent
                .join("cua-voice.app")
                .join("Contents/MacOS/cua-voice"),
        );
    }
    candidates
}

fn parse_hud_pid_from_ps(ps_output: &str, candidates: &[PathBuf], current_pid: u32) -> Option<u32> {
    let candidates: Vec<String> = candidates
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    ps_output.lines().find_map(|line| {
        let trimmed = line.trim_start();
        let split_at = trimmed.find(char::is_whitespace)?;
        let (pid, command) = trimmed.split_at(split_at);
        let command = command.trim_start();
        let pid = pid.parse::<u32>().ok()?;
        if pid == current_pid {
            return None;
        }
        candidates
            .iter()
            .any(|candidate| command == candidate || command.starts_with(&format!("{candidate} ")))
            .then_some(pid)
    })
}

#[cfg(not(test))]
fn discover_hud_pid() -> Option<u32> {
    let current = std::env::current_exe().ok()?;
    let candidates = hud_binary_candidates(&current);
    if candidates.is_empty() {
        return None;
    }
    let output = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ps_output = String::from_utf8_lossy(&output.stdout);
    parse_hud_pid_from_ps(&ps_output, &candidates, std::process::id())
}

#[cfg(test)]
fn discover_hud_pid() -> Option<u32> {
    None
}

#[derive(Clone)]
struct InputLane {
    sender: mpsc::Sender<InputJob>,
    backend_name: &'static str,
}

struct InputJob {
    enqueued_at: Instant,
    request: InputRequest,
    reply: oneshot::Sender<(Duration, InputResult)>,
}

impl InputLane {
    fn spawn(backend: Arc<dyn InputBackend>, capacity: usize) -> Self {
        let backend_name = backend.name();
        let (sender, mut receiver) = mpsc::channel::<InputJob>(capacity);
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                let queue_wait = job.enqueued_at.elapsed();
                let result = backend.execute(job.request).await;
                let _ = job.reply.send((queue_wait, result));
            }
        });
        Self {
            sender,
            backend_name,
        }
    }

    async fn execute(&self, request: InputRequest) -> (Duration, InputResult) {
        let idempotency_key = request.idempotency_key;
        let (reply, wait) = oneshot::channel();
        let job = InputJob {
            enqueued_at: Instant::now(),
            request,
            reply,
        };
        match self.sender.try_send(job) {
            Ok(()) => match wait.await {
                Ok(result) => result,
                Err(_) => (
                    Duration::ZERO,
                    input_result_with_id(
                        idempotency_key,
                        Effect::Failed,
                        InputRoute::Unavailable,
                        DeliveryMode::NotApplicable,
                        EvidenceKind::Error,
                        "input lane worker stopped",
                    ),
                ),
            },
            Err(mpsc::error::TrySendError::Full(job)) => (
                job.enqueued_at.elapsed(),
                input_result_with_id(
                    idempotency_key,
                    Effect::Failed,
                    InputRoute::Unavailable,
                    DeliveryMode::NotApplicable,
                    EvidenceKind::Error,
                    "input lane queue is full",
                ),
            ),
            Err(mpsc::error::TrySendError::Closed(job)) => (
                job.enqueued_at.elapsed(),
                input_result_with_id(
                    idempotency_key,
                    Effect::Failed,
                    InputRoute::Unavailable,
                    DeliveryMode::NotApplicable,
                    EvidenceKind::Error,
                    "input lane is closed",
                ),
            ),
        }
    }

    fn name(&self) -> &'static str {
        self.backend_name
    }
}

fn input_lane_capacity() -> usize {
    std::env::var("CUA_INPUT_QUEUE_CAPACITY")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(64)
}

#[derive(Clone)]
struct EncodeLane {
    sender: mpsc::Sender<EncodeJob>,
}

enum EncodeJob {
    Payload {
        enqueued_at: Instant,
        frame: CapturedFrame,
        include_bytes: bool,
        reply: oneshot::Sender<EncodeLaneResult<FramePayload>>,
    },
    MjpegChunk {
        enqueued_at: Instant,
        frame: CapturedFrame,
        reply: oneshot::Sender<EncodeLaneResult<Bytes>>,
    },
    WsFrame {
        enqueued_at: Instant,
        frame: CapturedFrame,
        reply: oneshot::Sender<EncodeLaneResult<(String, Vec<u8>)>>,
    },
}

struct EncodeLaneResult<T> {
    queue_wait: Duration,
    encode_duration: Duration,
    value: T,
}

#[derive(Debug)]
enum EncodeLaneError {
    Full,
    Closed,
    WorkerStopped,
}

impl EncodeLane {
    fn spawn(capacity: usize) -> Self {
        let (sender, mut receiver) = mpsc::channel::<EncodeJob>(capacity);
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                match job {
                    EncodeJob::Payload {
                        enqueued_at,
                        frame,
                        include_bytes,
                        reply,
                    } => {
                        let queue_wait = enqueued_at.elapsed();
                        let started = Instant::now();
                        let value = frame.as_payload(include_bytes);
                        let _ = reply.send(EncodeLaneResult {
                            queue_wait,
                            encode_duration: started.elapsed(),
                            value,
                        });
                    }
                    EncodeJob::MjpegChunk {
                        enqueued_at,
                        frame,
                        reply,
                    } => {
                        let queue_wait = enqueued_at.elapsed();
                        let started = Instant::now();
                        let mut body = Vec::new();
                        body.extend_from_slice(b"--cua-frame\r\nContent-Type: image/jpeg\r\n");
                        body.extend_from_slice(
                            format!("x-cua-frame-id: {}\r\n\r\n", frame.envelope.frame_id)
                                .as_bytes(),
                        );
                        body.extend_from_slice(&frame.bytes);
                        body.extend_from_slice(b"\r\n");
                        let _ = reply.send(EncodeLaneResult {
                            queue_wait,
                            encode_duration: started.elapsed(),
                            value: Bytes::from(body),
                        });
                    }
                    EncodeJob::WsFrame {
                        enqueued_at,
                        frame,
                        reply,
                    } => {
                        let queue_wait = enqueued_at.elapsed();
                        let started = Instant::now();
                        if let Ok(text) = serde_json::to_string(&frame.envelope) {
                            let _ = reply.send(EncodeLaneResult {
                                queue_wait,
                                encode_duration: started.elapsed(),
                                value: (text, (*frame.bytes).clone()),
                            });
                        }
                    }
                }
            }
        });
        Self { sender }
    }

    async fn payload(
        &self,
        frame: CapturedFrame,
        include_bytes: bool,
    ) -> Result<EncodeLaneResult<FramePayload>, EncodeLaneError> {
        let (reply, wait) = oneshot::channel();
        let job = EncodeJob::Payload {
            enqueued_at: Instant::now(),
            frame,
            include_bytes,
            reply,
        };
        self.send(job, wait).await
    }

    async fn mjpeg_chunk(
        &self,
        frame: CapturedFrame,
    ) -> Result<EncodeLaneResult<Bytes>, EncodeLaneError> {
        let (reply, wait) = oneshot::channel();
        let job = EncodeJob::MjpegChunk {
            enqueued_at: Instant::now(),
            frame,
            reply,
        };
        self.send(job, wait).await
    }

    async fn ws_frame(
        &self,
        frame: CapturedFrame,
    ) -> Result<EncodeLaneResult<(String, Vec<u8>)>, EncodeLaneError> {
        let (reply, wait) = oneshot::channel();
        let job = EncodeJob::WsFrame {
            enqueued_at: Instant::now(),
            frame,
            reply,
        };
        self.send(job, wait).await
    }

    async fn send<T>(
        &self,
        job: EncodeJob,
        wait: oneshot::Receiver<EncodeLaneResult<T>>,
    ) -> Result<EncodeLaneResult<T>, EncodeLaneError> {
        match self.sender.try_send(job) {
            Ok(()) => wait.await.map_err(|_| EncodeLaneError::WorkerStopped),
            Err(mpsc::error::TrySendError::Full(_)) => Err(EncodeLaneError::Full),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(EncodeLaneError::Closed),
        }
    }
}

fn encode_lane_capacity() -> usize {
    std::env::var("CUA_ENCODE_QUEUE_CAPACITY")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(64)
}

#[derive(Clone)]
struct ModelLane {
    sender: mpsc::Sender<ModelJob>,
    active: Arc<AtomicU32>,
}

struct ModelJob {
    enqueued_at: Instant,
    config: EvalConfig,
    frame: Option<cua_core::FramePayload>,
    api_key: Option<String>,
    reply: oneshot::Sender<ModelLaneResult>,
}

struct ModelLaneResult {
    queue_wait: Duration,
    run_duration: Duration,
    report: EvalReport,
}

#[derive(Debug)]
enum ModelLaneError {
    Full,
    Closed,
    WorkerStopped,
}

impl ModelLane {
    fn spawn(capacity: usize) -> Self {
        let (sender, mut receiver) = mpsc::channel::<ModelJob>(capacity);
        let active = Arc::new(AtomicU32::new(0));
        let worker_pool_active = active.clone();
        let semaphore = Arc::new(Semaphore::new(model_lane_workers()));
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                let permit = semaphore
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("model semaphore is never closed");
                let worker_active = worker_pool_active.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let queue_wait = job.enqueued_at.elapsed();
                    worker_active.fetch_add(1, Ordering::Relaxed);
                    let run_started = Instant::now();
                    let report = run_eval_report(job.config, job.frame, job.api_key).await;
                    worker_active.fetch_sub(1, Ordering::Relaxed);
                    let _ = job.reply.send(ModelLaneResult {
                        queue_wait,
                        run_duration: run_started.elapsed(),
                        report,
                    });
                });
            }
        });
        Self { sender, active }
    }

    async fn evaluate(
        &self,
        config: EvalConfig,
        frame: Option<cua_core::FramePayload>,
        api_key: Option<String>,
    ) -> Result<ModelLaneResult, ModelLaneError> {
        let (reply, wait) = oneshot::channel();
        let job = ModelJob {
            enqueued_at: Instant::now(),
            config,
            frame,
            api_key,
            reply,
        };
        match self.sender.try_send(job) {
            Ok(()) => wait.await.map_err(|_| ModelLaneError::WorkerStopped),
            Err(mpsc::error::TrySendError::Full(_)) => Err(ModelLaneError::Full),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(ModelLaneError::Closed),
        }
    }

    fn active_count(&self) -> u32 {
        self.active.load(Ordering::Relaxed)
    }
}

fn model_lane_capacity() -> usize {
    std::env::var("CUA_MODEL_QUEUE_CAPACITY")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(8)
}

fn model_lane_workers() -> usize {
    env_usize("CUA_MODEL_WORKERS", 4, 1, 32)
}

#[derive(Clone)]
struct PermissionLane {
    sender: mpsc::Sender<PermissionJob>,
}

struct PermissionJob {
    enqueued_at: Instant,
    reply: oneshot::Sender<PermissionLaneResult>,
}

struct PermissionLaneResult {
    queue_wait: Duration,
    probe_duration: Duration,
    report: PermissionReport,
    fallback: bool,
}

impl PermissionLane {
    fn spawn(computer: Arc<dyn ComputerBackend>, capacity: usize) -> Self {
        let (sender, mut receiver) = mpsc::channel::<PermissionJob>(capacity);
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                let queue_wait = job.enqueued_at.elapsed();
                let probe_started = Instant::now();
                let report = computer.permission_report().await;
                let _ = job.reply.send(PermissionLaneResult {
                    queue_wait,
                    probe_duration: probe_started.elapsed(),
                    report,
                    fallback: false,
                });
            }
        });
        Self { sender }
    }

    async fn report(&self) -> PermissionLaneResult {
        let (reply, wait) = oneshot::channel();
        let job = PermissionJob {
            enqueued_at: Instant::now(),
            reply,
        };
        match self.sender.try_send(job) {
            Ok(()) => wait
                .await
                .unwrap_or_else(|_| permission_fallback(Duration::ZERO)),
            Err(mpsc::error::TrySendError::Full(job)) => {
                permission_fallback(job.enqueued_at.elapsed())
            }
            Err(mpsc::error::TrySendError::Closed(job)) => {
                permission_fallback(job.enqueued_at.elapsed())
            }
        }
    }
}

fn permission_fallback(queue_wait: Duration) -> PermissionLaneResult {
    PermissionLaneResult {
        queue_wait,
        probe_duration: Duration::ZERO,
        report: PermissionReport::conservative_unknown(),
        fallback: true,
    }
}

fn permission_lane_capacity() -> usize {
    std::env::var("CUA_PERMISSION_QUEUE_CAPACITY")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(32)
}

#[derive(Clone)]
struct EventLane {
    sender: mpsc::Sender<EventJob>,
    recent: Arc<RwLock<VecDeque<serde_json::Value>>>,
    notify: Arc<Notify>,
}

#[derive(Debug)]
struct EventJob {
    event: serde_json::Value,
}

impl EventLane {
    fn spawn(capacity: usize, retention: usize) -> Self {
        let (sender, mut receiver) = mpsc::channel::<EventJob>(capacity);
        let recent = Arc::new(RwLock::new(VecDeque::with_capacity(retention)));
        let worker_recent = recent.clone();
        let notify = Arc::new(Notify::new());
        let worker_notify = notify.clone();
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                let mut recent = worker_recent.write().await;
                if recent.len() >= retention {
                    recent.pop_front();
                }
                recent.push_back(job.event);
                drop(recent);
                worker_notify.notify_waiters();
                worker_notify.notify_one();
            }
        });
        Self {
            sender,
            recent,
            notify,
        }
    }

    fn publish(&self, kind: &'static str, data: serde_json::Value) -> bool {
        self.sender
            .try_send(EventJob {
                event: serde_json::json!({
                    "schema_version": SCHEMA_VERSION,
                    "sequence": monotonic_event_sequence(),
                    "at_wall_ms": now_wall_ms(),
                    "kind": kind,
                    "data": data
                }),
            })
            .is_ok()
    }

    async fn snapshot(&self) -> Vec<serde_json::Value> {
        self.recent.read().await.iter().cloned().collect()
    }

    async fn after(&self, sequence: u64) -> Vec<serde_json::Value> {
        self.recent
            .read()
            .await
            .iter()
            .filter(|event| {
                event
                    .get("sequence")
                    .and_then(|value| value.as_u64())
                    .map(|value| value > sequence)
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    async fn wait_after(&self, sequence: u64, timeout: Duration) -> Vec<serde_json::Value> {
        loop {
            let events = self.after(sequence).await;
            if !events.is_empty() {
                return events;
            }
            if tokio::time::timeout(timeout, self.notify.notified())
                .await
                .is_err()
            {
                let events = self.after(sequence).await;
                if events.is_empty() {
                    return Vec::new();
                }
                return events;
            }
        }
    }
}

fn event_lane_capacity() -> usize {
    std::env::var("CUA_EVENT_QUEUE_CAPACITY")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(256)
}

fn event_lane_retention() -> usize {
    std::env::var("CUA_EVENT_RETENTION")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(1024)
}

fn monotonic_event_sequence() -> u64 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone)]
struct TraceLane {
    sender: mpsc::Sender<TraceJob>,
    dir: PathBuf,
}

#[allow(clippy::large_enum_variant)]
enum TraceJob {
    Artifact {
        relative_path: String,
        bytes: Arc<Vec<u8>>,
    },
    Record(TraceRecord),
}

impl TraceLane {
    fn spawn(writer: TraceWriter, capacity: usize) -> Self {
        let dir = writer.dir().to_path_buf();
        let worker_dir = dir.clone();
        let (sender, mut receiver) = mpsc::channel::<TraceJob>(capacity);
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                match job {
                    TraceJob::Artifact {
                        relative_path,
                        bytes,
                    } => {
                        if let Err(error) = writer
                            .write_artifact(relative_path.clone(), bytes.as_ref())
                            .await
                        {
                            eprintln!(
                                "cua daemon trace artifact write failed for {} in {}: {error}",
                                relative_path,
                                worker_dir.display()
                            );
                        }
                    }
                    TraceJob::Record(record) => {
                        if let Err(error) = writer.append(&record).await {
                            eprintln!(
                                "cua daemon trace append failed in {}: {error}",
                                worker_dir.display()
                            );
                        }
                    }
                }
            }
        });
        Self { sender, dir }
    }

    fn enqueue_artifact(&self, relative_path: String, bytes: Arc<Vec<u8>>) -> bool {
        self.sender
            .try_send(TraceJob::Artifact {
                relative_path,
                bytes,
            })
            .is_ok()
    }

    fn enqueue_record(&self, record: TraceRecord) -> bool {
        self.sender.try_send(TraceJob::Record(record)).is_ok()
    }

    fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}

fn build_trace_writer(dir: PathBuf) -> Option<TraceWriter> {
    match TraceWriter::from_dir(&dir) {
        Ok(writer) => Some(writer),
        Err(error) => {
            eprintln!(
                "cua daemon trace initialization failed for {}: {error}",
                dir.display()
            );
            None
        }
    }
}

fn trace_lane_capacity() -> usize {
    std::env::var("CUA_TRACE_QUEUE_CAPACITY")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(256)
}

fn trace_dir_from_env_or_profile(profile: &str) -> Option<PathBuf> {
    std::env::var_os("CUA_TRACE_DIR")
        .map(platform_artifact_path)
        .or_else(|| profile_daemon_trace_dir(profile).ok())
}

fn platform_artifact_path(path: impl Into<PathBuf>) -> PathBuf {
    let path = path.into();
    let artifact_root = PathBuf::from("artifacts").join("cua");
    let platform_root = artifact_root.join(host_platform_name());
    if path.starts_with(&platform_root) {
        return path;
    }
    if path.starts_with(&artifact_root) {
        return platform_root.join(path.strip_prefix(&artifact_root).unwrap_or(&path));
    }
    path
}

fn host_platform_name() -> &'static str {
    "macos"
}

pub fn router(state: DaemonState) -> Router {
    let auth_state = state.clone();
    let write_state = state.clone();
    Router::new()
        .route("/", get(root))
        .route("/manifest", get(manifest))
        .route("/schemas", get(schemas))
        .route("/version", get(version))
        .route("/status", get(status))
        .route("/config/status", get(config_status))
        .route("/metrics", get(metrics))
        .route("/healthz", get(healthz))
        .route("/capture/screenshot", post(screenshot))
        .route("/capture/window", post(capture_window))
        .route("/context/snapshot", post(context_snapshot))
        .route("/capture/stream.mjpeg", get(stream_mjpeg))
        .route("/capture/stream.ws", get(stream_ws))
        .route("/observe/desktop", get(observe_desktop))
        .route("/observe/displays", get(observe_displays))
        .route("/observe/cursor", get(observe_cursor))
        .route("/events", get(events))
        .route("/events/live", get(events_live))
        .route(
            "/permissions/accessibility/request",
            post(request_accessibility),
        )
        .route("/session/acquire", post(session_acquire))
        .route("/session/heartbeat", post(session_heartbeat))
        .route("/session/cancel", post(session_cancel))
        .route("/session/status", get(session_status))
        .route("/inbox/message", post(inbox_message))
        .route("/inbox/messages", get(inbox_messages))
        .route("/inbox/status/:message_id", get(inbox_status))
        .route(
            "/inbox/status/:message_id/running",
            post(inbox_mark_running),
        )
        .route("/inbox/status/:message_id/done", post(inbox_mark_done))
        .route("/inbox/status/:message_id/failed", post(inbox_mark_failed))
        .route("/webhooks/:source", post(webhook_publish))
        .route("/webhooks/:source/subscribe", post(webhook_subscribe))
        .route("/webhooks/:source/status", get(webhook_status))
        .route(
            "/scratchpads/write",
            post(scratchpad_write).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route("/scratchpads/read", post(scratchpad_read))
        .route("/scratchpads/list", post(scratchpad_list))
        .route(
            "/scratchpads/delete",
            post(scratchpad_delete).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route("/attestation/identity", get(attestation_identity))
        .route("/attestation/challenge", post(attestation_challenge))
        .route("/attestation/sign", post(attestation_sign))
        .route("/ui/step", post(ui_step))
        .route("/ui/reply", post(ui_reply))
        .route("/ui/mode", post(ui_mode))
        .route("/ui/island", post(ui_island))
        .route("/ui/scene", post(ui_scene))
        .route("/ui/scene/patch", post(ui_scene_patch))
        .route("/ui/scene/reset", post(ui_scene_reset))
        .route("/ui/scene/theme", post(ui_scene_theme))
        .route("/ui/scene/background", post(ui_scene_background))
        .route(
            "/profile/create",
            post(profile_create).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route(
            "/profile/activate",
            post(profile_activate).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route("/profile/status", get(profile_status))
        .route(
            "/control/pause",
            post(control_pause).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route(
            "/control/resume",
            post(control_resume).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route(
            "/control/kill-switch",
            post(control_kill_switch).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route(
            "/input/mouse",
            post(input_action).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route(
            "/input/dispatch",
            post(input_action).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route(
            "/input/keyboard",
            post(input_action).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route(
            "/input/clipboard",
            post(input_action).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route(
            "/input/frame",
            post(input_frame_action).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route("/clipboard/read", post(clipboard_read))
        .route(
            "/clipboard/write",
            post(clipboard_write).route_layer(middleware::from_fn_with_state(
                write_state.clone(),
                require_http_owner_write,
            )),
        )
        .route("/model/eval", post(model_eval))
        .layer(middleware::from_fn_with_state(auth_state, require_auth))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub async fn serve(
    addr: SocketAddr,
    profile: String,
    allow_lan: bool,
    hud_mode: UiMode,
) -> anyhow::Result<()> {
    if !allow_lan && !addr.ip().is_loopback() {
        anyhow::bail!(
            "refusing non-loopback bind {addr}; pass --allow-lan to expose the local HTTP API"
        );
    }
    let token = load_or_create_profile_token(&profile).await?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;
    let state = DaemonState::default_with_hud_mode(profile, token, hud_mode);
    if let Ok(mut http_addr) = state.http_addr.lock() {
        *http_addr = bound_addr.to_string();
    }
    state.frame_bus.clone().spawn_capture_lane(
        CaptureRequest {
            max_width: Some(1280),
            encoding: FrameEncoding::Jpeg,
            force_fresh: true,
        },
        Duration::from_millis(200),
    );
    spawn_unix_socket(state.clone()).await?;
    axum::serve(listener, router(state)).await?;
    Ok(())
}

pub async fn load_or_create_profile_token(profile: &str) -> anyhow::Result<String> {
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
        if !token.trim().is_empty() {
            return Ok(token);
        }
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let token = format!("cua-{}", Uuid::new_v4());
    tokio::fs::write(&path, format!("{token}\n")).await?;
    Ok(token)
}

fn http_token_override_allowed() -> bool {
    cfg!(test)
        || std::env::var("CUA_DEV_HTTP_TOKEN_OVERRIDE")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
}

async fn spawn_unix_socket(state: DaemonState) -> anyhow::Result<()> {
    let path = profile_socket_path(&state.profile)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        if profile_socket_is_live(&path).await {
            anyhow::bail!(
                "cua daemon for profile '{}' is already running at {}",
                state.profile,
                path.display()
            );
        }
        tokio::fs::remove_file(&path).await?;
    }
    let listener = UnixListener::bind(&path)?;
    if let Ok(mut profile_socket) = state.profile_socket.lock() {
        *profile_socket = path.display().to_string();
    }
    tracing::info!(socket = %path.display(), "listening on cua unix socket");
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let state = state.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_unix_stream(stream, state).await {
                            tracing::debug!(%error, "unix socket stream ended");
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "unix socket accept failed");
                    break;
                }
            }
        }
    });
    Ok(())
}

async fn profile_socket_is_live(path: &Path) -> bool {
    tokio::time::timeout(Duration::from_millis(150), UnixStream::connect(path))
        .await
        .is_ok_and(|result| result.is_ok())
}

#[derive(Debug, Deserialize)]
struct UnixRequest {
    id: Option<serde_json::Value>,
    token: Option<String>,
    session_id: Option<String>,
    method: String,
    params: Option<serde_json::Value>,
}

async fn handle_unix_stream(
    stream: tokio::net::UnixStream,
    state: DaemonState,
) -> anyhow::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<UnixRequest>(&line) {
            Ok(request) if request.method == "visual.session" => {
                return handle_visual_session(&state, request, lines, write).await;
            }
            Ok(request) => handle_unix_request(&state, request).await,
            Err(error) => unix_error(None, "bad_request", error.to_string(), None),
        };
        write.write_all(response.to_string().as_bytes()).await?;
        write.write_all(b"\n").await?;
    }
    Ok(())
}

async fn handle_visual_session(
    state: &DaemonState,
    request: UnixRequest,
    mut lines: tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    mut write: tokio::net::unix::OwnedWriteHalf,
) -> anyhow::Result<()> {
    let id = request.id.clone();
    if request.token.as_deref() != Some(state.bearer_token.as_str()) {
        let response = unix_error(
            id,
            "unauthorized",
            "missing or invalid token",
            Some(StatusCode::UNAUTHORIZED),
        );
        write.write_all(response.to_string().as_bytes()).await?;
        write.write_all(b"\n").await?;
        return Ok(());
    }
    let params = request.params.unwrap_or_else(|| serde_json::json!({}));
    let visual = match serde_json::from_value::<VisualSessionRequest>(params) {
        Ok(request) if request.schema_version == SCHEMA_VERSION => request,
        Ok(_) => {
            let response = unix_error(
                id,
                "bad_request",
                format!("expected schema_version {SCHEMA_VERSION}"),
                Some(StatusCode::BAD_REQUEST),
            );
            write.write_all(response.to_string().as_bytes()).await?;
            write.write_all(b"\n").await?;
            return Ok(());
        }
        Err(error) => {
            let response = unix_error(
                id,
                "bad_request",
                error.to_string(),
                Some(StatusCode::BAD_REQUEST),
            );
            write.write_all(response.to_string().as_bytes()).await?;
            write.write_all(b"\n").await?;
            return Ok(());
        }
    };
    let fps = visual.fps.unwrap_or(10).clamp(1, 30);
    let queue_depth = visual.queue_depth.unwrap_or(2).clamp(1, 8);
    let duration = visual.duration_ms.map(Duration::from_millis);
    let session_id = request
        .session_id
        .as_ref()
        .map(|session_id| session_id.trim())
        .filter(|session_id| !session_id.is_empty())
        .map(ToOwned::to_owned);
    if let Some(session_id) = session_id.as_ref() {
        let _ = state.sessions.acquire(SessionLeaseRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: session_id.clone(),
            client_name: "unix visual session".to_string(),
            role: RuntimeSessionRole::Observer,
            ttl_ms: None,
        });
    }
    let mut interval = tokio::time::interval(Duration::from_millis(1_000 / u64::from(fps)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let _guard = StreamGuard::new(state.active_streams.clone());
    let _session_guard =
        session_id.map(|session_id| SessionGuard::new(state.sessions.clone(), session_id));
    state.publish_event(
        "visual_session_started",
        serde_json::json!({
            "fps": fps,
            "max_width": visual.max_width,
            "include_bytes": visual.include_bytes,
            "duration_ms": visual.duration_ms,
            "queue_depth": queue_depth,
        }),
    );
    publish_protocol_step(
        state,
        1,
        2,
        "Opening visual stream".to_string(),
        "Unix socket",
        5_000,
    );
    write_json_line(
        &mut write,
        &VisualSessionMessage::Started {
            schema_version: SCHEMA_VERSION.to_string(),
            fps,
        },
    )
    .await?;
    let mut announced_first_frame = false;
    let (frame_tx, mut frame_rx) = mpsc::channel(queue_depth);
    let frame_bus = state.frame_bus.clone();
    let metrics = state.metrics.clone();
    let event_state = state.clone();
    let max_width = visual.max_width;
    let include_bytes = visual.include_bytes;
    let _producer = TaskAbortGuard::new(tokio::spawn(async move {
        loop {
            interval.tick().await;
            let started = Instant::now();
            let message = match frame_bus
                .latest_or_capture_timed(CaptureRequest {
                    max_width: max_width.or(Some(1280)),
                    encoding: FrameEncoding::Jpeg,
                    force_fresh: false,
                })
                .await
            {
                Ok(lookup) => {
                    observe_frame_lookup(&metrics, &lookup);
                    metrics.observe(MetricKind::StreamUnixTick, started.elapsed());
                    metrics.increment(CounterKind::UnixFrames);
                    VisualSessionMessage::Frame {
                        schema_version: SCHEMA_VERSION.to_string(),
                        frame: lookup.frame.as_payload(include_bytes),
                    }
                }
                Err(error) => {
                    metrics.increment(CounterKind::UnixFrameDrops);
                    let error = error.to_string();
                    event_state.publish_event(
                        "visual_session_frame_miss",
                        serde_json::json!({ "error": error }),
                    );
                    VisualSessionMessage::Diagnostic {
                        schema_version: SCHEMA_VERSION.to_string(),
                        message: error,
                    }
                }
            };
            if frame_tx.try_send(message).is_err() {
                metrics.increment(CounterKind::UnixFrameDrops);
            }
        }
    }));
    let close_at = duration.map(|duration| Instant::now() + duration);
    loop {
        if close_at.is_some_and(|close_at| Instant::now() >= close_at) {
            write_json_line(
                &mut write,
                &VisualSessionMessage::Closed {
                    schema_version: SCHEMA_VERSION.to_string(),
                },
            )
            .await?;
            return Ok(());
        }
        tokio::select! {
            maybe_line = lines.next_line() => {
                let Some(line) = maybe_line? else {
                    return Ok(());
                };
                if line.trim().is_empty() {
                    continue;
                }
                let response = match serde_json::from_str::<UnixRequest>(&line) {
                    Ok(request) if request.method == "visual.close" => {
                        write_json_line(
                            &mut write,
                            &VisualSessionMessage::Closed {
                                schema_version: SCHEMA_VERSION.to_string(),
                            },
                        ).await?;
                        return Ok(());
                    }
                    Ok(request) => handle_unix_request(state, request).await,
                    Err(error) => unix_error(None, "bad_request", error.to_string(), None),
                };
                write.write_all(response.to_string().as_bytes()).await?;
                write.write_all(b"\n").await?;
            }
            Some(message) = frame_rx.recv() => {
                if matches!(message, VisualSessionMessage::Frame { .. }) && !announced_first_frame {
                    announced_first_frame = true;
                    publish_protocol_step(
                        state,
                        2,
                        2,
                        format!(
                            "Streaming desktop frames at {} fps",
                            fps
                        ),
                        "Unix socket",
                        5_000,
                    );
                }
                write_json_line(&mut write, &message).await?;
            }
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
enum VisualSessionMessage {
    Started {
        schema_version: String,
        fps: u32,
    },
    Frame {
        schema_version: String,
        frame: FramePayload,
    },
    Diagnostic {
        schema_version: String,
        message: String,
    },
    Closed {
        schema_version: String,
    },
}

struct TaskAbortGuard {
    handle: tokio::task::JoinHandle<()>,
}

impl TaskAbortGuard {
    fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self { handle }
    }
}

impl Drop for TaskAbortGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

struct SessionGuard {
    sessions: SessionRegistry,
    session_id: String,
}

impl SessionGuard {
    fn new(sessions: SessionRegistry, session_id: String) -> Self {
        Self {
            sessions,
            session_id,
        }
    }
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.sessions.remove(&self.session_id);
    }
}

async fn write_json_line<T: Serialize>(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    value: &T,
) -> anyhow::Result<()> {
    write
        .write_all(serde_json::to_string(value)?.as_bytes())
        .await?;
    write.write_all(b"\n").await?;
    write.flush().await?;
    Ok(())
}

async fn handle_unix_request(state: &DaemonState, request: UnixRequest) -> serde_json::Value {
    let id = request.id.clone();
    let session_id = request.session_id.clone();
    if request.token.as_deref() != Some(state.bearer_token.as_str()) {
        return unix_error(
            id,
            "unauthorized",
            "missing or invalid token",
            Some(StatusCode::UNAUTHORIZED),
        );
    }
    let result = match request.method.as_str() {
        "session.acquire" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<SessionLeaseRequest>(params) {
                Ok(request) => state.sessions.acquire(request).map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "session.cancel" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<SessionCancelRequest>(params) {
                Ok(request) => match state.sessions.cancel(request) {
                    Ok(()) => Ok(serde_json::to_value(state.runtime_inventory().await)),
                    Err(error) => Err(error),
                },
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "session.heartbeat" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<SessionHeartbeatRequest>(params) {
                Ok(request) => state.sessions.heartbeat(request).map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "session.status" => Ok(serde_json::to_value(state.runtime_inventory().await)),
        "inbox.publish" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<InboundMessageRequest>(params) {
                Ok(request) => {
                    publish_inbound_message(state, request, InboundDeliveryMethod::UnixSocket)
                        .map(serde_json::to_value)
                }
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "inbox.after" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<InboxQuery>(params) {
                Ok(query) => state
                    .inbox
                    .after(query.after_sequence.or(query.after))
                    .map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "inbox.status" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<InboxMessageIdRequest>(params) {
                Ok(request) => state
                    .inbox
                    .status(&request.message_id)
                    .map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "inbox.running" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<InboxMessageIdRequest>(params) {
                Ok(request) => {
                    let result = state.inbox.set_running(&request.message_id);
                    if let Ok(status) = result.as_ref() {
                        publish_inbox_status_event(state, status);
                    }
                    result.map(serde_json::to_value)
                }
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "inbox.done" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<InboxDoneRpcRequest>(params) {
                Ok(request) => {
                    let result = state.inbox.set_done(
                        &request.message_id,
                        request
                            .reply
                            .map(|reply| bound_preserved_text(&reply, INBOX_REPLY_MAX_CHARS)),
                    );
                    if let Ok(status) = result.as_ref() {
                        publish_inbox_status_event(state, status);
                    }
                    result.map(serde_json::to_value)
                }
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "inbox.failed" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<InboxFailedRpcRequest>(params) {
                Ok(request) => {
                    let result = state.inbox.set_failed(
                        &request.message_id,
                        compact_action_text(&request.error, 480),
                    );
                    if let Ok(status) = result.as_ref() {
                        publish_inbox_status_event(state, status);
                    }
                    result.map(serde_json::to_value)
                }
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "webhook.publish" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<InboundMessageRequest>(params) {
                Ok(request) => {
                    publish_inbound_message(state, request, InboundDeliveryMethod::Webhook)
                        .map(serde_json::to_value)
                }
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "webhook.subscribe" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<WebhookSubscribeRequest>(params) {
                Ok(request) => state.inbox.subscribe(request).map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "webhook.status" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            let source = params
                .get("source")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            state.inbox.webhook_status(source).map(serde_json::to_value)
        }
        "scratchpad.write" => {
            if let Err(error) = state.sessions.authorize_write(session_id.as_deref()) {
                return unix_api_error(id, error);
            }
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<ScratchpadWriteRequest>(params) {
                Ok(request) => scratchpad_write_state(state, request)
                    .await
                    .map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "scratchpad.read" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<ScratchpadReadRequest>(params) {
                Ok(request) => scratchpad_read_state(state, request)
                    .await
                    .map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "scratchpad.list" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<ScratchpadListRequest>(params) {
                Ok(request) => scratchpad_list_state(state, request)
                    .await
                    .map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "scratchpad.delete" => {
            if let Err(error) = state.sessions.authorize_write(session_id.as_deref()) {
                return unix_api_error(id, error);
            }
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<ScratchpadDeleteRequest>(params) {
                Ok(request) => scratchpad_delete_state(state, request)
                    .await
                    .map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "attestation.identity" => attestation_identity_state(state)
            .await
            .map(serde_json::to_value),
        "attestation.challenge" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<AttestationChallengeRequest>(params) {
                Ok(request) => attestation_challenge_state(state, request)
                    .await
                    .map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "attestation.sign" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<AttestationSignRequest>(params) {
                Ok(request) => attestation_sign_state(state, request)
                    .await
                    .map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "capture.screenshot" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<ScreenshotRequest>(params) {
                Ok(request) => screenshot_payload(state, request)
                    .await
                    .map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "capture.window" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<WindowCaptureRequest>(params) {
                Ok(request) => capture_window_payload(state, request)
                    .await
                    .map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "status" => Ok(serde_json::to_value(state.health().await)),
        "schemas" => Ok(serde_json::to_value(schema_bundle())),
        "config.status" => Ok(serde_json::to_value(config_inventory_state(state))),
        "manifest" => Ok(serde_json::to_value(manifest_payload())),
        "metrics" => Ok(serde_json::to_value(metrics_snapshot(state))),
        "permissions.request_accessibility" => Ok(serde_json::to_value(
            request_accessibility_state(state).await,
        )),
        "events.snapshot" => Ok(serde_json::to_value(state.events.snapshot().await)),
        "events.after" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            let after_sequence = params
                .get("after_sequence")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            Ok(serde_json::to_value(
                state.events.after(after_sequence).await,
            ))
        }
        "events.wait" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            let after_sequence = params
                .get("after_sequence")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let timeout =
                event_wait_timeout(params.get("timeout_ms").and_then(|value| value.as_u64()));
            Ok(serde_json::to_value(
                state.events.wait_after(after_sequence, timeout).await,
            ))
        }
        "ui.step" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<UiStepRequest>(params) {
                Ok(request) => ui_step_state(state, request).map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "ui.reply" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<UiReplyRequest>(params) {
                Ok(request) => ui_reply_state(state, request).map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "ui.mode" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<UiModeRequest>(params) {
                Ok(request) => ui_mode_state(state, request).map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "ui.island" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<UiIslandRequest>(params) {
                Ok(request) => ui_island_state(state, request).map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "ui.scene.set" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<UiSceneRequest>(params) {
                Ok(request) => ui_scene_state(state, request).map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "ui.scene.patch" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<UiScenePatchRequest>(params) {
                Ok(request) => ui_scene_patch_state(state, request).map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "ui.scene.reset" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<UiSceneResetRequest>(params) {
                Ok(request) => ui_scene_reset_state(state, request).map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "ui.scene.theme" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<UiSceneThemeRequest>(params) {
                Ok(request) => ui_scene_theme_state(state, request).map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "ui.scene.background" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<UiSceneBackgroundRequest>(params) {
                Ok(request) => ui_scene_background_state(state, request).map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "observe.desktop" => desktop_state(state).await.map(serde_json::to_value),
        "observe.displays" => state
            .frame_bus
            .displays()
            .await
            .map_err(ApiError::internal)
            .map(serde_json::to_value),
        "observe.cursor" => Ok(serde_json::to_value(state.computer.cursor_state().await)),
        "context.snapshot" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<ContextSnapshotRequest>(params) {
                Ok(request) => context_snapshot_payload(state, request)
                    .await
                    .map(serde_json::to_value),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "clipboard.read" => {
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<ClipboardReadRequest>(params) {
                Ok(request) => Ok(serde_json::to_value(
                    clipboard_read_state(state, request).await,
                )),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "clipboard.write" => {
            if let Err(error) = state.sessions.authorize_write(session_id.as_deref()) {
                return unix_api_error(id, error);
            }
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<ClipboardWriteRequest>(params) {
                Ok(request) => Ok(serde_json::to_value(
                    clipboard_write_state(state, request).await,
                )),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "profile.status" => Ok(serde_json::to_value(state.control.read().await.clone())),
        "profile.create" => {
            if let Err(error) = state.sessions.authorize_write(session_id.as_deref()) {
                return unix_api_error(id, error);
            }
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<ProfileCreateRequest>(params) {
                Ok(request) => Ok(serde_json::to_value(
                    profile_create_state(state, request).await,
                )),
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "profile.activate" => {
            if let Err(error) = state.sessions.authorize_write(session_id.as_deref()) {
                return unix_api_error(id, error);
            }
            Ok(serde_json::to_value(profile_activate_state(state).await))
        }
        "control.pause" => {
            if let Err(error) = state.sessions.authorize_write(session_id.as_deref()) {
                return unix_api_error(id, error);
            }
            Ok(serde_json::to_value(control_pause_state(state).await))
        }
        "control.resume" => {
            if let Err(error) = state.sessions.authorize_write(session_id.as_deref()) {
                return unix_api_error(id, error);
            }
            Ok(serde_json::to_value(control_resume_state(state).await))
        }
        "control.kill_switch" => {
            if let Err(error) = state.sessions.authorize_write(session_id.as_deref()) {
                return unix_api_error(id, error);
            }
            Ok(serde_json::to_value(control_kill_switch_state(state).await))
        }
        "input.dispatch" => {
            if let Err(error) = state.sessions.authorize_write(session_id.as_deref()) {
                return unix_api_error(id, error);
            }
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<InputAction>(params) {
                Ok(action) => {
                    match serde_json::to_value(dispatch_input_action(state, action).await) {
                        Ok(value) => Ok(Ok(value)),
                        Err(error) => Ok(Err(error)),
                    }
                }
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        "input.dispatch_frame" => {
            if let Err(error) = state.sessions.authorize_write(session_id.as_deref()) {
                return unix_api_error(id, error);
            }
            let params = request.params.unwrap_or_else(|| serde_json::json!({}));
            match serde_json::from_value::<FrameActionRequest>(params) {
                Ok(request) => match serde_json::to_value(
                    dispatch_input_action(state, request.into_display_action()).await,
                ) {
                    Ok(value) => Ok(Ok(value)),
                    Err(error) => Ok(Err(error)),
                },
                Err(error) => {
                    return unix_error(
                        id,
                        "bad_request",
                        error.to_string(),
                        Some(StatusCode::BAD_REQUEST),
                    )
                }
            }
        }
        method => {
            return unix_error(
                id,
                "not_found",
                format!("unknown method {method}"),
                Some(StatusCode::NOT_FOUND),
            )
        }
    };
    match result {
        Ok(Ok(value)) => serde_json::json!({ "id": id, "ok": true, "result": value }),
        Ok(Err(error)) => unix_error(
            id,
            "serialization_error",
            error.to_string(),
            Some(StatusCode::INTERNAL_SERVER_ERROR),
        ),
        Err(error) => unix_api_error(id, error),
    }
}

fn unix_api_error(id: Option<serde_json::Value>, error: ApiError) -> serde_json::Value {
    let ApiError(body, status) = error;
    unix_error(id, body.code, body.message, Some(status))
}

fn unix_error(
    id: Option<serde_json::Value>,
    code: impl Into<String>,
    message: impl Into<String>,
    status: Option<StatusCode>,
) -> serde_json::Value {
    let mut details = BTreeMap::new();
    if let Some(status) = status {
        details.insert("status".to_string(), status.as_u16().to_string());
    }
    serde_json::json!({
        "id": id,
        "ok": false,
        "error": {
            "schema_version": SCHEMA_VERSION,
            "code": code.into(),
            "message": message.into(),
            "details": details
        }
    })
}

async fn require_auth(State(state): State<DaemonState>, request: Request, next: Next) -> Response {
    let started = Instant::now();
    if is_auth_exempt(request.uri().path()) {
        state
            .metrics
            .observe(MetricKind::PolicyCheck, started.elapsed());
        return next.run(request).await;
    }
    let expected = format!("Bearer {}", state.bearer_token);
    let authorized = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| value == expected)
        .unwrap_or(false);
    if authorized {
        state
            .metrics
            .observe(MetricKind::PolicyCheck, started.elapsed());
        next.run(request).await
    } else {
        state
            .metrics
            .observe(MetricKind::PolicyCheck, started.elapsed());
        (
            StatusCode::UNAUTHORIZED,
            Json(ApiErrorBody {
                schema_version: SCHEMA_VERSION.to_string(),
                code: "unauthorized".to_string(),
                message: "missing or invalid bearer token".to_string(),
                details: BTreeMap::new(),
            }),
        )
            .into_response()
    }
}

async fn require_http_owner_write(
    State(state): State<DaemonState>,
    request: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let session_id = request
        .headers()
        .get("x-cua-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match state.sessions.authorize_required_owner(session_id) {
        Ok(()) => {
            state
                .metrics
                .observe(MetricKind::PolicyCheck, started.elapsed());
            next.run(request).await
        }
        Err(error) => {
            state
                .metrics
                .observe(MetricKind::PolicyCheck, started.elapsed());
            error.into_response()
        }
    }
}

fn is_auth_exempt(path: &str) -> bool {
    matches!(path, "/" | "/healthz" | "/version")
}

async fn root() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "name": "cua",
        "control_surfaces": ["cli", "local_http", "local_unix_socket"],
    }))
}

async fn manifest() -> Json<Manifest> {
    Json(manifest_payload())
}

fn manifest_payload() -> Manifest {
    Manifest {
        schema_version: SCHEMA_VERSION.to_string(),
        name: "cua".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        public_surfaces: vec![
            "cli".to_string(),
            "local_http".to_string(),
            "local_unix_socket".to_string(),
        ],
        endpoints: vec![
            "GET /manifest".to_string(),
            "GET /schemas".to_string(),
            "GET /status".to_string(),
            "GET /config/status".to_string(),
            "GET /metrics".to_string(),
            "POST /capture/screenshot".to_string(),
            "POST /capture/window".to_string(),
            "POST /context/snapshot".to_string(),
            "GET /capture/stream.mjpeg".to_string(),
            "GET /capture/stream.ws".to_string(),
            "UNIX visual.session".to_string(),
            "GET /observe/desktop".to_string(),
            "GET /events".to_string(),
            "GET /events?after=<sequence>".to_string(),
            "GET /events/live?after=<sequence>&timeout_ms=<ms>".to_string(),
            "POST /permissions/accessibility/request".to_string(),
            "POST /session/acquire".to_string(),
            "POST /session/heartbeat".to_string(),
            "POST /session/cancel".to_string(),
            "GET /session/status".to_string(),
            "POST /inbox/message".to_string(),
            "GET /inbox/messages?after=<sequence>".to_string(),
            "GET /inbox/status/<message_id>".to_string(),
            "POST /inbox/status/<message_id>/running".to_string(),
            "POST /inbox/status/<message_id>/done".to_string(),
            "POST /inbox/status/<message_id>/failed".to_string(),
            "POST /webhooks/<source>".to_string(),
            "POST /webhooks/<source>/subscribe".to_string(),
            "GET /webhooks/<source>/status".to_string(),
            "POST /scratchpads/write".to_string(),
            "POST /scratchpads/read".to_string(),
            "POST /scratchpads/list".to_string(),
            "POST /scratchpads/delete".to_string(),
            "GET /attestation/identity".to_string(),
            "POST /attestation/challenge".to_string(),
            "POST /attestation/sign".to_string(),
            "POST /input/dispatch".to_string(),
            "UNIX session.acquire".to_string(),
            "UNIX session.heartbeat".to_string(),
            "UNIX session.cancel".to_string(),
            "UNIX session.status".to_string(),
            "UNIX attestation.identity".to_string(),
            "UNIX attestation.challenge".to_string(),
            "UNIX attestation.sign".to_string(),
            "UNIX inbox.publish".to_string(),
            "UNIX inbox.after".to_string(),
            "UNIX inbox.status".to_string(),
            "UNIX inbox.running".to_string(),
            "UNIX inbox.done".to_string(),
            "UNIX inbox.failed".to_string(),
            "UNIX webhook.publish".to_string(),
            "UNIX webhook.subscribe".to_string(),
            "UNIX webhook.status".to_string(),
            "UNIX scratchpad.write".to_string(),
            "UNIX scratchpad.read".to_string(),
            "UNIX scratchpad.list".to_string(),
            "UNIX scratchpad.delete".to_string(),
            "UNIX schemas".to_string(),
            "UNIX config.status".to_string(),
            "POST /ui/step".to_string(),
            "POST /ui/reply".to_string(),
            "POST /ui/mode".to_string(),
            "POST /ui/island".to_string(),
            "POST /ui/scene".to_string(),
            "POST /ui/scene/patch".to_string(),
            "POST /ui/scene/reset".to_string(),
            "POST /ui/scene/theme".to_string(),
            "POST /ui/scene/background".to_string(),
            "UNIX ui.step".to_string(),
            "UNIX ui.reply".to_string(),
            "UNIX ui.mode".to_string(),
            "UNIX ui.island".to_string(),
            "UNIX ui.scene.set".to_string(),
            "UNIX ui.scene.patch".to_string(),
            "UNIX ui.scene.reset".to_string(),
            "UNIX ui.scene.theme".to_string(),
            "UNIX ui.scene.background".to_string(),
            "POST /profile/create".to_string(),
            "POST /profile/activate".to_string(),
            "GET /profile/status".to_string(),
            "POST /control/pause".to_string(),
            "POST /control/resume".to_string(),
            "POST /control/kill-switch".to_string(),
            "POST /input/mouse".to_string(),
            "POST /input/keyboard".to_string(),
            "POST /input/clipboard".to_string(),
            "POST /input/frame".to_string(),
            "POST /clipboard/read".to_string(),
            "POST /clipboard/write".to_string(),
            "POST /model/eval".to_string(),
        ],
        commands: vec![
            "cua serve".to_string(),
            "cua status --json".to_string(),
            "cua config status --json".to_string(),
            "cua manifest --json".to_string(),
            "cua metrics --json".to_string(),
            "cua events --json [--after <sequence>]".to_string(),
            "cua permissions request-accessibility --json".to_string(),
            "cua session acquire <session-id> --role owner|observer --json".to_string(),
            "cua session heartbeat <session-id> --json".to_string(),
            "cua session cancel <session-id> --json".to_string(),
            "cua session status --json".to_string(),
            "cua attestation identity --json".to_string(),
            "cua attestation challenge --audience <audience> --json".to_string(),
            "cua attestation sign --audience <audience> --nonce <nonce> --json".to_string(),
            "cua attestation verify <attestation.json> --audience <audience> --json".to_string(),
            "cua identity status --json".to_string(),
            "cua identity rotate --json".to_string(),
            "cua inbox publish <text> --source <source> --json".to_string(),
            "cua inbox wait --after <sequence> --json".to_string(),
            "cua inbox status <message-id> --json".to_string(),
            "cua webhook publish <text> --source <source> --json".to_string(),
            "cua webhook subscribe <source> --secret <secret> --json".to_string(),
            "cua webhook status <source> --json".to_string(),
            "cua scratchpad write <name> <text> --session-id <owner-session-id> --json".to_string(),
            "cua scratchpad read <name> --json".to_string(),
            "cua scratchpad list --json".to_string(),
            "cua scratchpad delete <name> --session-id <owner-session-id> --json".to_string(),
            "cua stream --unix --json".to_string(),
            "cua ui step <label> --step-index <n> --step-total <n> --json".to_string(),
            "cua ui reply <text> --json".to_string(),
            "cua ui mode headless|headful --json".to_string(),
            "cua ui island expanded|collapsed|toggle --json".to_string(),
            "cua ui scene-set <scene.json> --json".to_string(),
            "cua ui scene-patch <scene.json> --json".to_string(),
            "cua ui scene-reset --json".to_string(),
            "cua ui scene-theme <theme.json> --json".to_string(),
            "cua ui background <background.json|background.cua.toml> --json".to_string(),
            "cua ui protocol <file.cua.toml|file.json> --json".to_string(),
            "cua perf live --json".to_string(),
            "cua screenshot --out <path>".to_string(),
            "cua window-capture <window-id> --out <path>".to_string(),
            "cua context --json".to_string(),
            "cua observe --json".to_string(),
            "cua profile status --json".to_string(),
            "cua clipboard read --allow-sensitive --json".to_string(),
            "cua clipboard write <text> --session-id <owner-session-id> --json".to_string(),
            "cua pause --session-id <owner-session-id> --json".to_string(),
            "cua resume --session-id <owner-session-id> --json".to_string(),
            "cua kill-switch --session-id <owner-session-id> --json".to_string(),
            "cua model eval".to_string(),
        ],
    }
}

fn default_control_state(profile_name: &str) -> RuntimeControlState {
    RuntimeControlState {
        schema_version: SCHEMA_VERSION.to_string(),
        active_profile: ProfilePolicy {
            schema_version: SCHEMA_VERSION.to_string(),
            name: profile_name.to_string(),
            mode: RuntimeMode::Observe,
            capabilities: CapabilityManifest::default(),
            created_wall_ms: now_wall_ms(),
            expires_wall_ms: None,
            active: true,
        },
        safety_state: SafetyState::Running,
        generation: 0,
    }
}

async fn schemas() -> Json<cua_core::SchemaBundle> {
    Json(schema_bundle())
}

async fn version() -> Json<serde_json::Value> {
    Json(
        serde_json::json!({"schema_version": SCHEMA_VERSION, "version": env!("CARGO_PKG_VERSION")}),
    )
}

async fn status(State(state): State<DaemonState>) -> Json<HealthReport> {
    Json(state.health().await)
}

async fn metrics(State(state): State<DaemonState>) -> Json<MetricsSnapshot> {
    Json(metrics_snapshot(&state))
}

fn metrics_snapshot(state: &DaemonState) -> MetricsSnapshot {
    state
        .metrics
        .snapshot(state.active_streams.load(Ordering::Relaxed))
}

async fn request_accessibility(State(state): State<DaemonState>) -> Json<PermissionReport> {
    Json(request_accessibility_state(&state).await)
}

async fn request_accessibility_state(state: &DaemonState) -> PermissionReport {
    let before = state.permission_report().await;
    let requested = state.computer.request_accessibility_input_access().await;
    let after = state.permission_report().await;
    state.publish_event(
        "permission_request",
        serde_json::json!({
            "permission": "accessibility_input",
            "before": before.accessibility_input,
            "requested": requested,
            "after": after.accessibility_input,
        }),
    );
    after
}

async fn healthz(State(state): State<DaemonState>) -> impl IntoResponse {
    let health = state.health().await;
    (StatusCode::OK, Json(health))
}

#[derive(Debug, Deserialize)]
struct ScreenshotRequest {
    max_width: Option<u32>,
    include_bytes: Option<bool>,
    force_fresh: Option<bool>,
    encoding: Option<FrameEncoding>,
}

#[derive(Debug, Deserialize)]
struct WindowCaptureRequest {
    window_id: u32,
    max_width: Option<u32>,
    include_bytes: Option<bool>,
    encoding: Option<FrameEncoding>,
}

async fn screenshot(
    State(state): State<DaemonState>,
    Json(request): Json<ScreenshotRequest>,
) -> Result<Json<FramePayload>, ApiError> {
    Ok(Json(screenshot_payload(&state, request).await?))
}

async fn capture_window(
    State(state): State<DaemonState>,
    Json(request): Json<WindowCaptureRequest>,
) -> Result<Json<FramePayload>, ApiError> {
    Ok(Json(capture_window_payload(&state, request).await?))
}

async fn capture_window_payload(
    state: &DaemonState,
    request: WindowCaptureRequest,
) -> Result<FramePayload, ApiError> {
    let started = Instant::now();
    publish_protocol_step(
        state,
        1,
        1,
        format!("Capturing window {}", request.window_id),
        "Unix socket",
        2_500,
    );
    let capture_request = CaptureRequest {
        max_width: request.max_width,
        encoding: FrameEncoding::Png,
        force_fresh: false,
    };
    let window_id = request.window_id;
    let timeout = window_capture_timeout();
    let window = state
        .computer
        .window_list()
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .find(|window| window.id == window_id.to_string())
        .ok_or_else(|| {
            ApiError::bad_request("window_id", format!("window {window_id} not found"))
        })?;
    let lookup = tokio::time::timeout(
        timeout,
        state.frame_bus.latest_or_capture_timed(capture_request),
    )
    .await
    .map_err(|_| {
        ApiError::busy(format!(
            "window capture timed out after {} ms",
            timeout.as_millis()
        ))
    })?
    .map_err(ApiError::internal)?;
    observe_frame_lookup(&state.metrics, &lookup);
    let frame = crop_window_frame(
        lookup.frame,
        &window,
        request.max_width,
        request.encoding.unwrap_or(FrameEncoding::Png),
    )
    .map_err(ApiError::internal)?;
    state
        .metrics
        .observe(MetricKind::CaptureScreenshot, started.elapsed());
    frame_payload_for_request(state, frame, request.include_bytes.unwrap_or(true)).await
}

fn window_capture_timeout() -> Duration {
    env_duration_ms("CUA_WINDOW_CAPTURE_TIMEOUT_MS", 2_500, 250, 30_000)
}

fn crop_window_frame(
    frame: CapturedFrame,
    window: &WindowInfo,
    max_width: Option<u32>,
    encoding: FrameEncoding,
) -> anyhow::Result<CapturedFrame> {
    let crop_started = Instant::now();
    let image = image::load_from_memory(frame.bytes.as_ref())
        .map_err(|error| anyhow::anyhow!("decode display frame for window crop: {error}"))?
        .to_rgba8();
    let display_width = frame.envelope.display_width.max(1);
    let display_height = frame.envelope.display_height.max(1);
    let scale_x = image.width() as f64 / f64::from(display_width);
    let scale_y = image.height() as f64 / f64::from(display_height);
    let origin_x = window.x.saturating_sub(frame.envelope.display_x);
    let origin_y = window.y.saturating_sub(frame.envelope.display_y);
    let crop_x = (f64::from(origin_x) * scale_x).round().max(0.0) as u32;
    let crop_y = (f64::from(origin_y) * scale_y).round().max(0.0) as u32;
    let source_width = (f64::from(window.width) * scale_x).round().max(1.0) as u32;
    let source_height = (f64::from(window.height) * scale_y).round().max(1.0) as u32;
    if crop_x >= image.width() || crop_y >= image.height() {
        anyhow::bail!("window {} is outside the captured display frame", window.id);
    }
    let source_width = source_width.min(image.width().saturating_sub(crop_x));
    let source_height = source_height.min(image.height().saturating_sub(crop_y));
    if source_width == 0 || source_height == 0 {
        anyhow::bail!("window {} crop is empty", window.id);
    }
    let crop =
        image::imageops::crop_imm(&image, crop_x, crop_y, source_width, source_height).to_image();
    let target_width = max_width
        .filter(|max_width| *max_width < crop.width())
        .map(|max_width| max_width.max(64))
        .unwrap_or_else(|| crop.width());
    let target_height = if target_width == crop.width() {
        crop.height()
    } else {
        ((crop.height() as f64) * (target_width as f64 / crop.width() as f64)).round() as u32
    }
    .max(1);
    let buffer = if target_width == crop.width() && target_height == crop.height() {
        crop
    } else {
        image::imageops::resize(
            &crop,
            target_width,
            target_height,
            image::imageops::FilterType::Triangle,
        )
    };
    let encode_started = Instant::now();
    let bytes = encode_image(&buffer, encoding.clone())?;
    let encode_ns = elapsed_ns(encode_started);
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let byte_len = bytes.len();
    let width = buffer.width();
    let height = buffer.height();
    let mut envelope = frame.envelope.clone();
    envelope.frame_origin_x = window.x;
    envelope.frame_origin_y = window.y;
    envelope.width = width;
    envelope.height = height;
    envelope.pixel_format = "rgba8".to_string();
    envelope.encoding = encoding;
    envelope.byte_len = byte_len;
    envelope.sha256 = sha256;
    envelope.damage_rects = vec![cua_core::Rect {
        x: 0,
        y: 0,
        width,
        height,
    }];
    Ok(CapturedFrame {
        envelope,
        bytes: Arc::new(bytes),
        timings: CapturedFrameTimings {
            capture_ns: frame
                .timings
                .capture_ns
                .saturating_add(elapsed_ns(crop_started)),
            encode_ns,
            source: frame.timings.source,
        },
    })
}

#[derive(Debug, Deserialize)]
struct ContextSnapshotRequest {
    max_width: Option<u32>,
    include_bytes: Option<bool>,
    force_fresh: Option<bool>,
    encoding: Option<FrameEncoding>,
}

async fn context_snapshot(
    State(state): State<DaemonState>,
    Json(request): Json<ContextSnapshotRequest>,
) -> Result<Json<DesktopContextSnapshot>, ApiError> {
    Ok(Json(context_snapshot_payload(&state, request).await?))
}

async fn context_snapshot_payload(
    state: &DaemonState,
    request: ContextSnapshotRequest,
) -> Result<DesktopContextSnapshot, ApiError> {
    publish_protocol_step(
        state,
        1,
        1,
        "Capturing desktop context".to_string(),
        "HTTP API",
        2_500,
    );
    let screenshot_request = ScreenshotRequest {
        max_width: request.max_width,
        include_bytes: request.include_bytes,
        force_fresh: request.force_fresh,
        encoding: request.encoding,
    };
    let (frame, desktop) = tokio::join!(
        screenshot_payload_with_step(state, screenshot_request, false),
        desktop_state(state)
    );
    Ok(DesktopContextSnapshot {
        schema_version: SCHEMA_VERSION.to_string(),
        frame: frame?,
        desktop: desktop?,
    })
}

async fn screenshot_payload(
    state: &DaemonState,
    request: ScreenshotRequest,
) -> Result<FramePayload, ApiError> {
    screenshot_payload_with_step(state, request, true).await
}

async fn screenshot_payload_with_step(
    state: &DaemonState,
    request: ScreenshotRequest,
    publish_step: bool,
) -> Result<FramePayload, ApiError> {
    if publish_step {
        publish_protocol_step(
            state,
            1,
            1,
            "Capturing screenshot".to_string(),
            "HTTP API",
            2_500,
        );
    }
    let started = Instant::now();
    let capture_request = CaptureRequest {
        max_width: request.max_width,
        encoding: request.encoding.unwrap_or(FrameEncoding::Png),
        force_fresh: request.force_fresh.unwrap_or(false),
    };
    let lookup = if capture_request.force_fresh {
        match state
            .frame_bus
            .latest_within(resident_frame_freshness())
            .await
        {
            Some(frame) => FrameLookup {
                frame: frame
                    .transformed(&capture_request)
                    .map_err(ApiError::computer_backend)?,
                cache_hit: true,
                wait_ns: started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            },
            None => state
                .frame_bus
                .latest_or_capture_timed(capture_request)
                .await
                .map_err(ApiError::computer_backend)?,
        }
    } else {
        state
            .frame_bus
            .latest_or_capture_timed(capture_request)
            .await
            .map_err(ApiError::computer_backend)?
    };
    observe_frame_lookup(&state.metrics, &lookup);
    state
        .metrics
        .observe(MetricKind::CaptureScreenshot, started.elapsed());
    frame_payload_for_request(state, lookup.frame, request.include_bytes.unwrap_or(true)).await
}

async fn frame_payload_for_request(
    state: &DaemonState,
    frame: CapturedFrame,
    include_bytes: bool,
) -> Result<FramePayload, ApiError> {
    if !include_bytes {
        return Ok(frame.as_payload(false));
    }
    let encoded = state
        .encode_lane
        .payload(frame, true)
        .await
        .map_err(|error| {
            state.metrics.increment(CounterKind::EncodeDrops);
            ApiError::busy(format!("encode lane unavailable: {error:?}"))
        })?;
    observe_encode_result(&state.metrics, &encoded);
    Ok(encoded.value)
}

fn resident_frame_freshness() -> Duration {
    env_duration_ms("CUA_RESIDENT_FRAME_FRESH_MS", 1_000, 50, 30_000)
}

fn trace_frame_artifacts_enabled() -> bool {
    env_flag_enabled(std::env::var("CUA_TRACE_FRAME_ARTIFACTS").ok().as_deref())
}

fn env_flag_enabled(value: Option<&str>) -> bool {
    matches!(
        value.map(|value| value.trim().to_ascii_lowercase()),
        Some(value)
            if matches!(
                value.as_str(),
                "1" | "true" | "yes" | "on" | "frames" | "artifacts"
            )
    )
}

fn env_usize(key: &str, default: usize, min: usize, max: usize) -> usize {
    parse_bounded_usize(std::env::var(key).ok().as_deref(), default, min, max)
}

fn parse_bounded_usize(value: Option<&str>, default: usize, min: usize, max: usize) -> usize {
    value
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn env_duration_ms(key: &str, default_ms: u64, min_ms: u64, max_ms: u64) -> Duration {
    let ms = std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default_ms)
        .clamp(min_ms, max_ms);
    Duration::from_millis(ms)
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

async fn stream_mjpeg(State(state): State<DaemonState>) -> Result<Response, ApiError> {
    let guard = StreamGuard::new(state.active_streams.clone());
    let mut interval = tokio::time::interval(Duration::from_millis(200));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let frame_bus = state.frame_bus.clone();
    let metrics = state.metrics.clone();
    let encode_lane = state.encode_lane.clone();
    let stream = futures::stream::unfold(
        (frame_bus, metrics, encode_lane, interval, guard),
        |(frame_bus, metrics, encode_lane, mut interval, guard)| async move {
            interval.tick().await;
            let started = Instant::now();
            let chunk = match frame_bus
                .latest_or_capture_timed(CaptureRequest {
                    max_width: Some(1280),
                    encoding: FrameEncoding::Jpeg,
                    force_fresh: false,
                })
                .await
            {
                Ok(lookup) => {
                    observe_frame_lookup(&metrics, &lookup);
                    match encode_lane.mjpeg_chunk(lookup.frame).await {
                        Ok(encoded) => {
                            observe_encode_result(&metrics, &encoded);
                            metrics.increment(CounterKind::MjpegFrames);
                            Ok::<Bytes, Infallible>(encoded.value)
                        }
                        Err(error) => {
                            metrics.increment(CounterKind::EncodeDrops);
                            Ok::<Bytes, Infallible>(Bytes::from(format!(
                                "--cua-frame\r\nContent-Type: application/json\r\n\r\n{{\"error\":\"encode lane unavailable: {error:?}\"}}\r\n"
                            )))
                        }
                    }
                }
                Err(error) => Ok::<Bytes, Infallible>(Bytes::from(format!(
                    "--cua-frame\r\nContent-Type: application/json\r\n\r\n{{\"error\":\"{}\"}}\r\n",
                    error.to_string().replace('"', "'")
                ))),
            };
            metrics.observe(MetricKind::StreamMjpegTick, started.elapsed());
            Some((chunk, (frame_bus, metrics, encode_lane, interval, guard)))
        },
    );
    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("multipart/x-mixed-replace; boundary=cua-frame"),
    );
    Ok(response)
}

async fn stream_ws(ws: WebSocketUpgrade, State(state): State<DaemonState>) -> impl IntoResponse {
    ws.on_upgrade(move |mut socket| async move {
        let _guard = StreamGuard::new(state.active_streams.clone());
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let started = Instant::now();
            match state
                .frame_bus
                .latest_or_capture_timed(CaptureRequest {
                    max_width: Some(1280),
                    encoding: FrameEncoding::Jpeg,
                    force_fresh: false,
                })
                .await
            {
                Ok(lookup) => {
                    observe_frame_lookup(&state.metrics, &lookup);
                    let encoded = match state.encode_lane.ws_frame(lookup.frame).await {
                        Ok(encoded) => encoded,
                        Err(_) => {
                            state.metrics.increment(CounterKind::EncodeDrops);
                            break;
                        }
                    };
                    observe_encode_result(&state.metrics, &encoded);
                    let (text, bytes) = encoded.value;
                    if socket.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                    if socket.send(Message::Binary(bytes)).await.is_err() {
                        break;
                    }
                    state.metrics.increment(CounterKind::WsFrames);
                }
                Err(error) => {
                    let text = serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "error": error.to_string(),
                    })
                    .to_string();
                    if socket.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
            }
            state
                .metrics
                .observe(MetricKind::StreamWsTick, started.elapsed());
        }
    })
}

struct StreamGuard {
    active_streams: Arc<AtomicU32>,
}

impl StreamGuard {
    fn new(active_streams: Arc<AtomicU32>) -> Self {
        active_streams.fetch_add(1, Ordering::Relaxed);
        Self { active_streams }
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.active_streams.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn observe_desktop(State(state): State<DaemonState>) -> Result<Json<DesktopState>, ApiError> {
    Ok(Json(desktop_state(&state).await?))
}

async fn desktop_state(state: &DaemonState) -> Result<DesktopState, ApiError> {
    let (permissions, displays, latest_frame, cursor, windows) = tokio::join!(
        state.permission_report(),
        state.frame_bus.displays(),
        state.frame_bus.latest_envelope(),
        state.computer.cursor_state(),
        state.computer.window_list()
    );
    let displays = displays.map_err(ApiError::computer_backend)?;
    let windows = windows.map_err(ApiError::computer_backend)?;
    Ok(DesktopState {
        schema_version: SCHEMA_VERSION.to_string(),
        displays,
        windows,
        cursor,
        permissions,
        latest_frame,
    })
}

async fn observe_displays(
    State(state): State<DaemonState>,
) -> Result<Json<Vec<cua_core::DisplayInfo>>, ApiError> {
    Ok(Json(
        state
            .frame_bus
            .displays()
            .await
            .map_err(ApiError::computer_backend)?,
    ))
}

async fn observe_cursor(State(state): State<DaemonState>) -> Json<cua_core::CursorState> {
    Json(state.computer.cursor_state().await)
}

#[derive(Debug, Deserialize)]
struct EventsQuery {
    after: Option<u64>,
    timeout_ms: Option<u64>,
}

async fn events(
    State(state): State<DaemonState>,
    Query(query): Query<EventsQuery>,
) -> Json<Vec<serde_json::Value>> {
    match query.after {
        Some(sequence) => Json(state.events.after(sequence).await),
        None => Json(state.events.snapshot().await),
    }
}

async fn events_live(
    State(state): State<DaemonState>,
    Query(query): Query<EventsQuery>,
) -> Json<Vec<serde_json::Value>> {
    match query.after {
        Some(sequence) => Json(
            state
                .events
                .wait_after(sequence, event_wait_timeout(query.timeout_ms))
                .await,
        ),
        None => Json(state.events.snapshot().await),
    }
}

fn event_wait_timeout(timeout_ms: Option<u64>) -> Duration {
    Duration::from_millis(timeout_ms.unwrap_or(1_000).clamp(25, 30_000))
}

async fn session_acquire(
    State(state): State<DaemonState>,
    Json(request): Json<SessionLeaseRequest>,
) -> Result<Json<SessionLeaseResult>, ApiError> {
    Ok(Json(state.sessions.acquire(request)?))
}

async fn session_heartbeat(
    State(state): State<DaemonState>,
    Json(request): Json<SessionHeartbeatRequest>,
) -> Result<Json<SessionLeaseResult>, ApiError> {
    Ok(Json(state.sessions.heartbeat(request)?))
}

async fn session_cancel(
    State(state): State<DaemonState>,
    Json(request): Json<SessionCancelRequest>,
) -> Result<Json<RuntimeInventory>, ApiError> {
    state.sessions.cancel(request)?;
    Ok(Json(state.runtime_inventory().await))
}

async fn session_status(State(state): State<DaemonState>) -> Json<RuntimeInventory> {
    Json(state.runtime_inventory().await)
}

#[derive(Debug, Deserialize)]
struct InboxQuery {
    after: Option<u64>,
    after_sequence: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct InboxDoneRequest {
    reply: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InboxFailedRequest {
    error: String,
}

#[derive(Debug, Deserialize)]
struct InboxMessageIdRequest {
    message_id: String,
}

#[derive(Debug, Deserialize)]
struct InboxDoneRpcRequest {
    message_id: String,
    reply: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InboxFailedRpcRequest {
    message_id: String,
    error: String,
}

async fn inbox_message(
    State(state): State<DaemonState>,
    Json(request): Json<InboundMessageRequest>,
) -> Result<Json<InboundStatus>, ApiError> {
    Ok(Json(publish_inbound_message(
        &state,
        request,
        InboundDeliveryMethod::LocalHttp,
    )?))
}

async fn inbox_messages(
    State(state): State<DaemonState>,
    Query(query): Query<InboxQuery>,
) -> Result<Json<Vec<InboundStatus>>, ApiError> {
    Ok(Json(
        state.inbox.after(query.after_sequence.or(query.after))?,
    ))
}

async fn inbox_status(
    State(state): State<DaemonState>,
    AxumPath(message_id): AxumPath<String>,
) -> Result<Json<InboundStatus>, ApiError> {
    Ok(Json(state.inbox.status(&message_id)?))
}

async fn inbox_mark_running(
    State(state): State<DaemonState>,
    AxumPath(message_id): AxumPath<String>,
) -> Result<Json<InboundStatus>, ApiError> {
    let status = state.inbox.set_running(&message_id)?;
    publish_inbox_status_event(&state, &status);
    Ok(Json(status))
}

async fn inbox_mark_done(
    State(state): State<DaemonState>,
    AxumPath(message_id): AxumPath<String>,
    Json(request): Json<InboxDoneRequest>,
) -> Result<Json<InboundStatus>, ApiError> {
    let status = state.inbox.set_done(
        &message_id,
        request
            .reply
            .map(|reply| bound_preserved_text(&reply, INBOX_REPLY_MAX_CHARS)),
    )?;
    publish_inbox_status_event(&state, &status);
    Ok(Json(status))
}

async fn inbox_mark_failed(
    State(state): State<DaemonState>,
    AxumPath(message_id): AxumPath<String>,
    Json(request): Json<InboxFailedRequest>,
) -> Result<Json<InboundStatus>, ApiError> {
    let status = state
        .inbox
        .set_failed(&message_id, compact_action_text(&request.error, 480))?;
    publish_inbox_status_event(&state, &status);
    Ok(Json(status))
}

async fn webhook_subscribe(
    State(state): State<DaemonState>,
    AxumPath(source): AxumPath<String>,
    Json(mut request): Json<WebhookSubscribeRequest>,
) -> Result<Json<WebhookSourceStatus>, ApiError> {
    let source = normalize_inbound_field(&source, 96, "source")?;
    if !request.source.trim().is_empty()
        && normalize_inbound_field(&request.source, 96, "source")? != source
    {
        return Err(ApiError::bad_request(
            "source",
            "webhook source path and request source must match",
        ));
    }
    request.source = source;
    Ok(Json(state.inbox.subscribe(request)?))
}

async fn webhook_status(
    State(state): State<DaemonState>,
    AxumPath(source): AxumPath<String>,
) -> Result<Json<WebhookSourceStatus>, ApiError> {
    Ok(Json(state.inbox.webhook_status(&source)?))
}

async fn webhook_publish(
    State(state): State<DaemonState>,
    AxumPath(source): AxumPath<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<InboundStatus>, ApiError> {
    let source = normalize_inbound_field(&source, 96, "source")?;
    let config = state.inbox.webhook_config(&source)?;
    if let Some(secret) = config
        .as_ref()
        .and_then(|config| config.shared_secret.as_ref())
    {
        verify_webhook_signature(secret, &headers, &body)?;
    }
    let mut request: InboundMessageRequest = serde_json::from_slice(&body)
        .map_err(|error| ApiError::bad_request("body", error.to_string()))?;
    request.source = source;
    if request.reply_url.is_none() {
        request.reply_url = config.and_then(|config| config.reply_url);
    }
    Ok(Json(publish_inbound_message(
        &state,
        request,
        InboundDeliveryMethod::Webhook,
    )?))
}

fn publish_inbound_message(
    state: &DaemonState,
    request: InboundMessageRequest,
    delivery_method: InboundDeliveryMethod,
) -> Result<InboundStatus, ApiError> {
    let status = state.inbox.publish(request, delivery_method)?;
    if status.state != InboundMessageState::Duplicate {
        state.publish_event(
            "inbound_message",
            serde_json::json!({
                "message_id": status.message_id,
                "sequence": status.message.sequence,
                "source": status.message.source,
                "text": status.message.text,
                "payload": status.message.payload,
                "reply_mode": status.message.reply_mode,
                "reply_url": status.message.reply_url,
                "delivery_method": status.message.delivery_method,
            }),
        );
    }
    Ok(status)
}

async fn scratchpad_write(
    State(state): State<DaemonState>,
    Json(request): Json<ScratchpadWriteRequest>,
) -> Result<Json<ScratchpadEntry>, ApiError> {
    Ok(Json(scratchpad_write_state(&state, request).await?))
}

async fn scratchpad_read(
    State(state): State<DaemonState>,
    Json(request): Json<ScratchpadReadRequest>,
) -> Result<Json<ScratchpadEntry>, ApiError> {
    Ok(Json(scratchpad_read_state(&state, request).await?))
}

async fn scratchpad_list(
    State(state): State<DaemonState>,
    Json(request): Json<ScratchpadListRequest>,
) -> Result<Json<ScratchpadListResult>, ApiError> {
    Ok(Json(scratchpad_list_state(&state, request).await?))
}

async fn scratchpad_delete(
    State(state): State<DaemonState>,
    Json(request): Json<ScratchpadDeleteRequest>,
) -> Result<Json<ScratchpadDeleteResult>, ApiError> {
    Ok(Json(scratchpad_delete_state(&state, request).await?))
}

async fn scratchpad_write_state(
    state: &DaemonState,
    request: ScratchpadWriteRequest,
) -> Result<ScratchpadEntry, ApiError> {
    validate_schema_version(&request.schema_version)?;
    let name = normalize_scratchpad_name(&request.name)?;
    let text = normalize_scratchpad_text(&request.text)?;
    let kind = if request.durable {
        ScratchpadKind::Durable
    } else {
        ScratchpadKind::Ephemeral
    };
    let path = scratchpad_entry_path(&state.profile, &kind, &name)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| ApiError::internal(error.into()))?;
    }
    prune_expired_scratchpads(&state.profile).await?;
    let now = now_wall_ms();
    let existing = if request.append {
        read_scratchpad_file(&path)
            .await?
            .filter(|entry| !scratchpad_expired(entry, now))
    } else {
        None
    };
    let created_wall_ms = existing
        .as_ref()
        .map(|entry| entry.created_wall_ms)
        .unwrap_or(now);
    let text = match existing {
        Some(entry) if entry.text.trim().is_empty() => text,
        Some(entry) => format!("{}\n{}", entry.text, text),
        None => text,
    };
    let expires_wall_ms = if matches!(kind, ScratchpadKind::Ephemeral) {
        Some(now + request.ttl_ms.unwrap_or(3_600_000).clamp(1_000, 86_400_000))
    } else {
        None
    };
    let entry = ScratchpadEntry {
        schema_version: SCHEMA_VERSION.to_string(),
        profile: state.profile.clone(),
        name,
        kind,
        bytes: text.len(),
        text,
        created_wall_ms,
        updated_wall_ms: now,
        expires_wall_ms,
    };
    let bytes =
        serde_json::to_vec_pretty(&entry).map_err(|error| ApiError::internal(error.into()))?;
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|error| ApiError::internal(error.into()))?;
    state.publish_event(
        "scratchpad_written",
        serde_json::json!({
            "name": entry.name,
            "kind": entry.kind,
            "bytes": entry.bytes,
            "updated_wall_ms": entry.updated_wall_ms,
        }),
    );
    Ok(entry)
}

async fn scratchpad_read_state(
    state: &DaemonState,
    request: ScratchpadReadRequest,
) -> Result<ScratchpadEntry, ApiError> {
    validate_schema_version(&request.schema_version)?;
    let name = normalize_scratchpad_name(&request.name)?;
    prune_expired_scratchpads(&state.profile).await?;
    let kinds = match request.durable {
        Some(true) => vec![ScratchpadKind::Durable],
        Some(false) => vec![ScratchpadKind::Ephemeral],
        None => vec![ScratchpadKind::Durable, ScratchpadKind::Ephemeral],
    };
    for kind in kinds {
        let path = scratchpad_entry_path(&state.profile, &kind, &name)?;
        if let Some(entry) = read_scratchpad_file(&path).await? {
            return Ok(entry);
        }
    }
    Err(ApiError::bad_request(
        "name",
        "scratchpad name is unknown or expired",
    ))
}

async fn scratchpad_list_state(
    state: &DaemonState,
    request: ScratchpadListRequest,
) -> Result<ScratchpadListResult, ApiError> {
    validate_schema_version(&request.schema_version)?;
    prune_expired_scratchpads(&state.profile).await?;
    let mut entries = Vec::new();
    if request.include_durable {
        entries.extend(scratchpad_summaries(&state.profile, ScratchpadKind::Durable).await?);
    }
    if request.include_ephemeral {
        entries.extend(scratchpad_summaries(&state.profile, ScratchpadKind::Ephemeral).await?);
    }
    entries.sort_by(|left, right| {
        right
            .updated_wall_ms
            .cmp(&left.updated_wall_ms)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(ScratchpadListResult {
        schema_version: SCHEMA_VERSION.to_string(),
        profile: state.profile.clone(),
        entries,
    })
}

async fn scratchpad_delete_state(
    state: &DaemonState,
    request: ScratchpadDeleteRequest,
) -> Result<ScratchpadDeleteResult, ApiError> {
    validate_schema_version(&request.schema_version)?;
    let name = normalize_scratchpad_name(&request.name)?;
    let mut deleted = 0;
    if request.durable {
        deleted += remove_scratchpad(&state.profile, ScratchpadKind::Durable, &name).await?;
    }
    if request.ephemeral {
        deleted += remove_scratchpad(&state.profile, ScratchpadKind::Ephemeral, &name).await?;
    }
    state.publish_event(
        "scratchpad_deleted",
        serde_json::json!({
            "name": name,
            "deleted": deleted,
        }),
    );
    Ok(ScratchpadDeleteResult {
        schema_version: SCHEMA_VERSION.to_string(),
        profile: state.profile.clone(),
        deleted,
    })
}

async fn scratchpad_summaries(
    profile: &str,
    kind: ScratchpadKind,
) -> Result<Vec<ScratchpadSummary>, ApiError> {
    let dir = scratchpad_kind_dir(profile, &kind)?;
    let mut summaries = Vec::new();
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(summaries),
        Err(error) => return Err(ApiError::internal(error.into())),
    };
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| ApiError::internal(error.into()))?
    {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        if let Some(entry) = read_scratchpad_file(&path).await? {
            summaries.push(ScratchpadSummary {
                schema_version: SCHEMA_VERSION.to_string(),
                profile: entry.profile,
                name: entry.name,
                kind: entry.kind,
                updated_wall_ms: entry.updated_wall_ms,
                expires_wall_ms: entry.expires_wall_ms,
                bytes: entry.bytes,
            });
        }
    }
    Ok(summaries)
}

async fn prune_expired_scratchpads(profile: &str) -> Result<(), ApiError> {
    let dir = scratchpad_kind_dir(profile, &ScratchpadKind::Ephemeral)?;
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(ApiError::internal(error.into())),
    };
    let now = now_wall_ms();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|error| ApiError::internal(error.into()))?
    {
        let path = entry.path();
        if let Some(stored) = read_scratchpad_file(&path).await? {
            if scratchpad_expired(&stored, now) {
                let _ = tokio::fs::remove_file(path).await;
            }
        }
    }
    Ok(())
}

async fn read_scratchpad_file(path: &Path) -> Result<Option<ScratchpadEntry>, ApiError> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ApiError::internal(error.into())),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| ApiError::internal(error.into()))
}

async fn remove_scratchpad(
    profile: &str,
    kind: ScratchpadKind,
    name: &str,
) -> Result<usize, ApiError> {
    let path = scratchpad_entry_path(profile, &kind, name)?;
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(1),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(ApiError::internal(error.into())),
    }
}

fn scratchpad_expired(entry: &ScratchpadEntry, now: i64) -> bool {
    entry
        .expires_wall_ms
        .map(|expires| expires <= now)
        .unwrap_or(false)
}

fn scratchpad_entry_path(
    profile: &str,
    kind: &ScratchpadKind,
    name: &str,
) -> Result<PathBuf, ApiError> {
    Ok(scratchpad_kind_dir(profile, kind)?.join(format!("{name}.json")))
}

fn scratchpad_kind_dir(profile: &str, kind: &ScratchpadKind) -> Result<PathBuf, ApiError> {
    let root = profile_scratchpads_dir(profile).map_err(ApiError::internal)?;
    Ok(root.join(match kind {
        ScratchpadKind::Durable => "durable",
        ScratchpadKind::Ephemeral => "ephemeral",
    }))
}

fn normalize_scratchpad_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 96 {
        return Err(ApiError::bad_request(
            "name",
            "scratchpad name must be 1-96 bytes",
        ));
    }
    if matches!(name, "." | "..")
        || name
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
    {
        return Err(ApiError::bad_request(
            "name",
            "scratchpad name may only contain letters, numbers, dot, dash, and underscore",
        ));
    }
    Ok(name.to_string())
}

fn normalize_scratchpad_text(text: &str) -> Result<String, ApiError> {
    let text = text.trim();
    if text.is_empty() || text.len() > 32_768 {
        return Err(ApiError::bad_request(
            "text",
            "scratchpad text must be 1-32768 bytes",
        ));
    }
    Ok(text.to_string())
}

fn validate_schema_version(schema_version: &str) -> Result<(), ApiError> {
    if schema_version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "schema_version",
            format!("expected {SCHEMA_VERSION}"),
        ))
    }
}

fn publish_inbox_status_event(state: &DaemonState, status: &InboundStatus) {
    state.publish_event(
        "inbox_status",
        serde_json::json!({
            "message_id": status.message_id,
            "sequence": status.message.sequence,
            "state": status.state,
            "reply": status.reply,
            "error": status.error,
        }),
    );
}

fn verify_webhook_signature(
    secret: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), ApiError> {
    let provided = headers
        .get("x-cua-webhook-signature")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiError::forbidden(
                "webhook_signature_required",
                "x-cua-webhook-signature is required for this webhook source",
            )
        })?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|error| ApiError::internal(anyhow::anyhow!(error)))?;
    mac.update(body);
    let expected = bytes_to_hex(&mac.finalize().into_bytes());
    let provided = provided.strip_prefix("sha256=").unwrap_or(provided);
    if constant_time_ascii_eq(provided.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "webhook_signature_mismatch",
            "webhook signature did not match this source",
        ))
    }
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn constant_time_ascii_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b)
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

async fn attestation_identity(
    State(state): State<DaemonState>,
) -> Result<Json<MachineIdentityStatus>, ApiError> {
    Ok(Json(attestation_identity_state(&state).await?))
}

async fn attestation_challenge(
    State(state): State<DaemonState>,
    Json(request): Json<AttestationChallengeRequest>,
) -> Result<Json<AttestationChallenge>, ApiError> {
    Ok(Json(attestation_challenge_state(&state, request).await?))
}

async fn attestation_sign(
    State(state): State<DaemonState>,
    Json(request): Json<AttestationSignRequest>,
) -> Result<Json<MachineAttestation>, ApiError> {
    Ok(Json(attestation_sign_state(&state, request).await?))
}

async fn attestation_identity_state(
    state: &DaemonState,
) -> Result<MachineIdentityStatus, ApiError> {
    load_or_create_machine_identity(&state.profile).map_err(ApiError::internal)
}

async fn attestation_challenge_state(
    state: &DaemonState,
    request: AttestationChallengeRequest,
) -> Result<AttestationChallenge, ApiError> {
    if request.audience.trim().is_empty() {
        return Err(ApiError::bad_request("audience", "audience is required"));
    }
    let issued_wall_ms = now_wall_ms();
    let challenge = AttestationChallenge {
        schema_version: SCHEMA_VERSION.to_string(),
        challenge_id: Uuid::new_v4().to_string(),
        nonce: Uuid::new_v4().to_string(),
        audience: request.audience,
        issued_wall_ms,
        expires_wall_ms: issued_wall_ms + attestation_challenge_ttl().as_millis() as i64,
    };
    let mut challenges = state.attestation_challenges.lock().map_err(|_| {
        ApiError::internal(anyhow::anyhow!(
            "attestation challenge registry lock poisoned"
        ))
    })?;
    prune_expired_challenges(&mut challenges, issued_wall_ms);
    challenges.insert(challenge.challenge_id.clone(), challenge.clone());
    Ok(challenge)
}

async fn attestation_sign_state(
    state: &DaemonState,
    request: AttestationSignRequest,
) -> Result<MachineAttestation, ApiError> {
    if request.audience.trim().is_empty() {
        return Err(ApiError::bad_request("audience", "audience is required"));
    }
    if request.nonce.trim().is_empty() {
        return Err(ApiError::bad_request("nonce", "nonce is required"));
    }
    let now = now_wall_ms();
    let challenge = if let Some(challenge_id) = request.challenge_id.as_deref() {
        let mut challenges = state.attestation_challenges.lock().map_err(|_| {
            ApiError::internal(anyhow::anyhow!(
                "attestation challenge registry lock poisoned"
            ))
        })?;
        prune_expired_challenges(&mut challenges, now);
        let challenge = challenges.remove(challenge_id).ok_or_else(|| {
            ApiError::bad_request("challenge_id", "challenge_id is unknown or already used")
        })?;
        if challenge.audience != request.audience {
            return Err(ApiError::bad_request(
                "audience",
                "challenge audience mismatch",
            ));
        }
        if challenge.nonce != request.nonce {
            return Err(ApiError::bad_request("nonce", "challenge nonce mismatch"));
        }
        if now > challenge.expires_wall_ms {
            return Err(ApiError::bad_request("challenge_id", "challenge expired"));
        }
        challenge
    } else {
        AttestationChallenge {
            schema_version: SCHEMA_VERSION.to_string(),
            challenge_id: Uuid::new_v4().to_string(),
            nonce: request.nonce,
            audience: request.audience.clone(),
            issued_wall_ms: now,
            expires_wall_ms: now + attestation_challenge_ttl().as_millis() as i64,
        }
    };
    let claims = runtime_identity_claims(state, request.profile, request.session_id).await?;
    sign_machine_attestation(challenge, claims).map_err(ApiError::internal)
}

async fn runtime_identity_claims(
    state: &DaemonState,
    profile: Option<String>,
    session_id: Option<String>,
) -> Result<RuntimeIdentityClaims, ApiError> {
    let permissions = state.permission_report().await;
    let control = state.control.read().await.clone();
    let code_identity = current_exe_code_identity().unwrap_or_default();
    let socket_path = state
        .profile_socket
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    let http_addr = state
        .http_addr
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    Ok(RuntimeIdentityClaims {
        schema_version: SCHEMA_VERSION.to_string(),
        runtime_name: "cua".to_string(),
        runtime_version: env!("CARGO_PKG_VERSION").to_string(),
        daemon_pid: std::process::id(),
        profile: profile.unwrap_or_else(|| state.profile.clone()),
        socket_path,
        http_addr,
        bundle_id: code_identity.bundle_id,
        designated_requirement: code_identity.designated_requirement,
        code_signature_summary: code_identity.code_signature_summary,
        binary_sha256: current_exe_sha256().ok(),
        computer_backend: state.computer.descriptor(),
        permissions,
        active_profile: control.active_profile,
        safety_state: control.safety_state,
        session_id,
    })
}

#[derive(Debug, Default)]
struct CurrentExeCodeIdentity {
    bundle_id: Option<String>,
    designated_requirement: Option<String>,
    code_signature_summary: Option<String>,
}

fn current_exe_code_identity() -> anyhow::Result<CurrentExeCodeIdentity> {
    let exe = std::env::current_exe()?;
    let Some(macos_dir) = exe.parent() else {
        return Ok(CurrentExeCodeIdentity::default());
    };
    if macos_dir.file_name().and_then(|name| name.to_str()) != Some("MacOS") {
        return Ok(CurrentExeCodeIdentity::default());
    }
    let Some(contents_dir) = macos_dir.parent() else {
        return Ok(CurrentExeCodeIdentity::default());
    };
    if contents_dir.file_name().and_then(|name| name.to_str()) != Some("Contents") {
        return Ok(CurrentExeCodeIdentity::default());
    }
    let info_plist = contents_dir.join("Info.plist");
    let bundle_id = command_stdout(
        "/usr/bin/plutil",
        &["-extract", "CFBundleIdentifier", "raw", "-o", "-"],
        Some(&info_plist),
    );
    let code_signature_summary = command_stdout(
        "/usr/bin/codesign",
        &["--display", "--verbose=2"],
        Some(&exe),
    );
    let designated_requirement = command_stdout("/usr/bin/codesign", &["-d", "-r-"], Some(&exe));

    Ok(CurrentExeCodeIdentity {
        bundle_id,
        designated_requirement,
        code_signature_summary,
    })
}

fn command_stdout(command: &str, args: &[&str], final_path: Option<&Path>) -> Option<String> {
    let mut process = std::process::Command::new(command);
    process.args(args);
    if let Some(path) = final_path {
        process.arg(path);
    }
    let output = process.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, _) => Some(stdout),
        (true, false) => Some(stderr),
        (true, true) => None,
    }
}

fn attestation_challenge_ttl() -> Duration {
    Duration::from_secs(5 * 60)
}

fn prune_expired_challenges(
    challenges: &mut BTreeMap<String, AttestationChallenge>,
    now_wall_ms: i64,
) {
    challenges.retain(|_, challenge| challenge.expires_wall_ms >= now_wall_ms);
}

fn current_exe_sha256() -> anyhow::Result<String> {
    let exe = std::env::current_exe()?;
    let bytes = std::fs::read(exe)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

async fn config_status(State(state): State<DaemonState>) -> Json<ConfigInventory> {
    Json(config_inventory_state(&state))
}

fn config_inventory_state(state: &DaemonState) -> ConfigInventory {
    ConfigInventory::for_profile(&state.profile).unwrap_or_else(|_| ConfigInventory {
        schema_version: SCHEMA_VERSION.to_string(),
        cua_home: String::new(),
        config_dir: String::new(),
        config_env: String::new(),
        legacy_config_env: String::new(),
        legacy_config_env_present: false,
        config_env_present: false,
        migration_state: cua_core::ConfigMigrationState::Missing,
        profile_root: String::new(),
        profile_socket: String::new(),
        profile_token_present: false,
        chat_db: String::new(),
        ctx_workspace: String::new(),
        scratchpads: String::new(),
        trace_root: String::new(),
        voice_trace: String::new(),
        daemon_trace_root: String::new(),
        identity_root: String::new(),
        cloud_root: String::new(),
        artifact_root: String::new(),
        cache_root: String::new(),
        log_root: String::new(),
        bin_root: String::new(),
    })
}

async fn ui_step(
    State(state): State<DaemonState>,
    Json(request): Json<UiStepRequest>,
) -> Result<Json<UiStepResult>, ApiError> {
    Ok(Json(ui_step_state(&state, request)?))
}

async fn ui_reply(
    State(state): State<DaemonState>,
    Json(request): Json<UiReplyRequest>,
) -> Result<Json<UiReplyResult>, ApiError> {
    Ok(Json(ui_reply_state(&state, request)?))
}

async fn ui_mode(
    State(state): State<DaemonState>,
    Json(request): Json<UiModeRequest>,
) -> Result<Json<UiModeResult>, ApiError> {
    Ok(Json(ui_mode_state(&state, request)?))
}

async fn ui_island(
    State(state): State<DaemonState>,
    Json(request): Json<UiIslandRequest>,
) -> Result<Json<UiIslandResult>, ApiError> {
    Ok(Json(ui_island_state(&state, request)?))
}

async fn ui_scene(
    State(state): State<DaemonState>,
    Json(request): Json<UiSceneRequest>,
) -> Result<Json<UiSceneResult>, ApiError> {
    Ok(Json(ui_scene_state(&state, request)?))
}

async fn ui_scene_patch(
    State(state): State<DaemonState>,
    Json(request): Json<UiScenePatchRequest>,
) -> Result<Json<UiSceneResult>, ApiError> {
    Ok(Json(ui_scene_patch_state(&state, request)?))
}

async fn ui_scene_reset(
    State(state): State<DaemonState>,
    Json(request): Json<UiSceneResetRequest>,
) -> Result<Json<UiSceneResult>, ApiError> {
    Ok(Json(ui_scene_reset_state(&state, request)?))
}

async fn ui_scene_theme(
    State(state): State<DaemonState>,
    Json(request): Json<UiSceneThemeRequest>,
) -> Result<Json<UiSceneResult>, ApiError> {
    Ok(Json(ui_scene_theme_state(&state, request)?))
}

async fn ui_scene_background(
    State(state): State<DaemonState>,
    Json(request): Json<UiSceneBackgroundRequest>,
) -> Result<Json<UiSceneResult>, ApiError> {
    Ok(Json(ui_scene_background_state(&state, request)?))
}

fn ui_step_state(state: &DaemonState, request: UiStepRequest) -> Result<UiStepResult, ApiError> {
    if request.schema_version != SCHEMA_VERSION {
        return Err(ApiError::bad_request(
            "schema_version",
            format!("expected {SCHEMA_VERSION}"),
        ));
    }
    let label = request
        .label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if label.is_empty() {
        return Err(ApiError::bad_request("label", "label must not be empty"));
    }
    if label.chars().count() > 160 {
        return Err(ApiError::bad_request(
            "label",
            "label must be 160 characters or fewer",
        ));
    }
    let source = normalize_optional_step_field(request.source, 48);
    let task = normalize_optional_step_field(request.task, 80);
    let tool = normalize_optional_step_field(request.tool, 48);
    let step_total = request.step_total.map(|value| value.max(1));
    let step_index = request
        .step_index
        .map(|value| value.min(step_total.unwrap_or(u16::MAX)));
    let ttl_ms = request.ttl_ms.map(|value| value.clamp(250, 60_000));
    let result = UiStepResult {
        schema_version: SCHEMA_VERSION.to_string(),
        accepted: true,
        label,
        source,
        task,
        tool,
        step_index,
        step_total,
        ttl_ms,
    };
    state.record_programmed_step(&result);
    state.publish_event(
        "ui_step",
        serde_json::json!({
            "label": result.label,
            "source": result.source,
            "task": result.task,
            "tool": result.tool,
            "step_index": result.step_index,
            "step_total": result.step_total,
            "ttl_ms": result.ttl_ms,
        }),
    );
    Ok(result)
}

fn publish_protocol_step(
    state: &DaemonState,
    step_index: u16,
    step_total: u16,
    label: String,
    tool: impl Into<String>,
    ttl_ms: u64,
) {
    if state.has_active_programmed_step() {
        return;
    }
    let _ = ui_step_state(
        state,
        UiStepRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            label,
            source: Some("cua-runtime".to_string()),
            task: Some("Computer control".to_string()),
            tool: Some(tool.into()),
            step_index: Some(step_index),
            step_total: Some(step_total),
            ttl_ms: Some(ttl_ms),
        },
    );
}

fn publish_protocol_status(
    state: &DaemonState,
    label: String,
    tool: impl Into<String>,
    ttl_ms: u64,
) {
    if state.has_active_programmed_step() {
        return;
    }
    let _ = ui_step_state(
        state,
        UiStepRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            label,
            source: Some("cua-runtime".to_string()),
            task: Some("Computer control".to_string()),
            tool: Some(tool.into()),
            step_index: None,
            step_total: None,
            ttl_ms: Some(ttl_ms),
        },
    );
}

fn input_action_label(action: &InputAction) -> String {
    match action {
        InputAction::MouseMove { x, y, .. } => format!("mouse move to {x},{y}"),
        InputAction::MouseClick {
            x,
            y,
            button,
            count,
        } => {
            let repeat = if *count > 1 {
                format!(" x{count}")
            } else {
                String::new()
            };
            format!("{button:?} mouse click at {x},{y}{repeat}")
        }
        InputAction::MouseDrag {
            from_x,
            from_y,
            to_x,
            to_y,
            ..
        } => format!("mouse drag from {from_x},{from_y} to {to_x},{to_y}"),
        InputAction::KeyPress { combo } => format!("key press {combo}"),
        InputAction::KeyType { text } => format!("typing {} chars", text.chars().count()),
        InputAction::KeyPaste { text } => format!("pasting {} chars", text.chars().count()),
        InputAction::Sequence { actions, .. } => format!("sequence {} actions", actions.len()),
        InputAction::OpenApp { app_name } => format!("open app {app_name}"),
        InputAction::ShellExec { command, .. } => {
            format!("shell {}", compact_action_text(command, 48))
        }
        InputAction::Aegis { args, .. } => {
            format!("aegis {}", compact_action_text(&args.join(" "), 48))
        }
        InputAction::Ctx { args, .. } => {
            format!("ctx {}", compact_action_text(&args.join(" "), 48))
        }
        InputAction::ClipboardRead { .. } => "clipboard read".to_string(),
        InputAction::ClipboardWrite { text } => {
            format!("clipboard write {} chars", text.chars().count())
        }
        InputAction::Pause => "pause control".to_string(),
        InputAction::Resume => "resume control".to_string(),
        InputAction::KillSwitch => "kill switch".to_string(),
    }
}

fn compact_action_text(value: &str, limit: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= limit {
        return compact;
    }
    let mut truncated = compact.chars().take(limit).collect::<String>();
    truncated.push_str("...");
    truncated
}

fn bound_preserved_text(value: &str, limit: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_string();
    }
    let mut truncated = trimmed
        .chars()
        .take(limit.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

fn input_effect_status_prefix(effect: &Effect) -> &'static str {
    match effect {
        Effect::Confirmed => "Completed",
        Effect::Failed => "Failed",
        Effect::Partial => "Partial",
        Effect::Unverifiable => "Unverifiable",
        Effect::SuspectedNoop => "Suspected",
        Effect::Refused => "Refused",
    }
}

fn ui_island_state(
    state: &DaemonState,
    request: UiIslandRequest,
) -> Result<UiIslandResult, ApiError> {
    if request.schema_version != SCHEMA_VERSION {
        return Err(ApiError::bad_request(
            "schema_version",
            format!("expected {SCHEMA_VERSION}"),
        ));
    }
    let source = normalize_optional_step_field(request.source, 48);
    let result = UiIslandResult {
        schema_version: SCHEMA_VERSION.to_string(),
        accepted: true,
        state: request.state,
        source,
    };
    state.publish_event(
        "ui_island",
        serde_json::json!({
            "state": result.state,
            "source": result.source,
        }),
    );
    Ok(result)
}

fn ui_scene_state(state: &DaemonState, request: UiSceneRequest) -> Result<UiSceneResult, ApiError> {
    validate_ui_scene_request_version(request.schema_version)?;
    request
        .scene
        .validate()
        .map_err(|error| ApiError::bad_request("scene", error.to_string()))?;
    let source = normalize_optional_step_field(request.source, 48);
    let result = UiSceneResult {
        schema_version: SCHEMA_VERSION.to_string(),
        accepted: true,
        source,
        scene: Some(request.scene),
    };
    state.publish_event(
        "ui_scene",
        serde_json::json!({
            "source": result.source,
            "scene": result.scene,
        }),
    );
    Ok(result)
}

fn ui_scene_patch_state(
    state: &DaemonState,
    request: UiScenePatchRequest,
) -> Result<UiSceneResult, ApiError> {
    validate_ui_scene_request_version(request.schema_version)?;
    request
        .scene
        .validate()
        .map_err(|error| ApiError::bad_request("scene", error.to_string()))?;
    let source = normalize_optional_step_field(request.source, 48);
    let result = UiSceneResult {
        schema_version: SCHEMA_VERSION.to_string(),
        accepted: true,
        source,
        scene: Some(request.scene),
    };
    state.publish_event(
        "ui_scene",
        serde_json::json!({
            "source": result.source,
            "scene": result.scene,
            "patch": true,
        }),
    );
    Ok(result)
}

fn ui_scene_reset_state(
    state: &DaemonState,
    request: UiSceneResetRequest,
) -> Result<UiSceneResult, ApiError> {
    validate_ui_scene_request_version(request.schema_version)?;
    let source = normalize_optional_step_field(request.source, 48);
    let result = UiSceneResult {
        schema_version: SCHEMA_VERSION.to_string(),
        accepted: true,
        source,
        scene: None,
    };
    state.publish_event(
        "ui_scene_reset",
        serde_json::json!({
            "source": result.source,
        }),
    );
    Ok(result)
}

fn ui_scene_theme_state(
    state: &DaemonState,
    request: UiSceneThemeRequest,
) -> Result<UiSceneResult, ApiError> {
    validate_ui_scene_request_version(request.schema_version)?;
    cua_core::validate_island_theme(&request.theme)
        .map_err(|error| ApiError::bad_request("theme", error.to_string()))?;
    let source = normalize_optional_step_field(request.source, 48);
    let result = UiSceneResult {
        schema_version: SCHEMA_VERSION.to_string(),
        accepted: true,
        source,
        scene: None,
    };
    state.publish_event(
        "ui_scene_theme",
        serde_json::json!({
            "source": result.source,
            "theme": request.theme,
        }),
    );
    Ok(result)
}

fn ui_scene_background_state(
    state: &DaemonState,
    request: UiSceneBackgroundRequest,
) -> Result<UiSceneResult, ApiError> {
    validate_ui_scene_request_version(request.schema_version)?;
    cua_core::validate_island_background(&request.background)
        .map_err(|error| ApiError::bad_request("background", error.to_string()))?;
    let source = normalize_optional_step_field(request.source, 48);
    let result = UiSceneResult {
        schema_version: SCHEMA_VERSION.to_string(),
        accepted: true,
        source,
        scene: None,
    };
    state.publish_event(
        "ui_scene_background",
        serde_json::json!({
            "source": result.source,
            "background": request.background,
        }),
    );
    Ok(result)
}

fn validate_ui_scene_request_version(schema_version: String) -> Result<(), ApiError> {
    if schema_version != SCHEMA_VERSION {
        return Err(ApiError::bad_request(
            "schema_version",
            format!("expected {SCHEMA_VERSION}"),
        ));
    }
    Ok(())
}

fn ui_reply_state(state: &DaemonState, request: UiReplyRequest) -> Result<UiReplyResult, ApiError> {
    if request.schema_version != SCHEMA_VERSION {
        return Err(ApiError::bad_request(
            "schema_version",
            format!("expected {SCHEMA_VERSION}"),
        ));
    }
    let text = request
        .text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return Err(ApiError::bad_request("text", "text must not be empty"));
    }
    if text.chars().count() > 480 {
        return Err(ApiError::bad_request(
            "text",
            "text must be 480 characters or fewer",
        ));
    }
    let source = normalize_optional_step_field(request.source, 48);
    let ttl_ms = request.ttl_ms.map(|value| value.clamp(250, 60_000));
    let result = UiReplyResult {
        schema_version: SCHEMA_VERSION.to_string(),
        accepted: true,
        text,
        source,
        ttl_ms,
    };
    state.publish_event(
        "ui_reply",
        serde_json::json!({
            "text": result.text,
            "source": result.source,
            "ttl_ms": result.ttl_ms,
        }),
    );
    Ok(result)
}

fn ui_mode_state(state: &DaemonState, request: UiModeRequest) -> Result<UiModeResult, ApiError> {
    if request.schema_version != SCHEMA_VERSION {
        return Err(ApiError::bad_request(
            "schema_version",
            format!("expected {SCHEMA_VERSION}"),
        ));
    }
    let result = UiModeResult {
        schema_version: SCHEMA_VERSION.to_string(),
        accepted: true,
        mode: request.mode,
        source: normalize_optional_step_field(request.source, 48),
    };
    state.publish_event(
        "ui_mode",
        serde_json::json!({
            "mode": result.mode,
            "source": result.source,
        }),
    );
    Ok(result)
}

fn normalize_optional_step_field(value: Option<String>, max_chars: usize) -> Option<String> {
    value.and_then(|value| {
        let normalized = value
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(max_chars)
            .collect::<String>();
        if normalized.is_empty() {
            None
        } else {
            Some(normalized)
        }
    })
}

fn observe_frame_lookup(metrics: &RuntimeMetrics, lookup: &FrameLookup) {
    metrics.observe_ns(MetricKind::CaptureQueueWait, lookup.wait_ns);
    match lookup.frame.timings.source {
        CaptureSource::ScreenCaptureKit => metrics.increment(CounterKind::CaptureSckFrames),
        CaptureSource::CoreGraphics => metrics.increment(CounterKind::CaptureCoreGraphicsFrames),
        CaptureSource::Synthetic => metrics.increment(CounterKind::CaptureSyntheticFrames),
        CaptureSource::Resident | CaptureSource::Unknown => {}
    }
    if !lookup.cache_hit {
        metrics.observe_ns(MetricKind::CaptureEncode, lookup.frame.timings.encode_ns);
    }
}

fn observe_encode_result<T>(metrics: &RuntimeMetrics, result: &EncodeLaneResult<T>) {
    metrics.observe(MetricKind::EncodeQueueWait, result.queue_wait);
    metrics.observe(MetricKind::EncodeDispatch, result.encode_duration);
}

#[derive(Debug, Deserialize)]
struct ProfileCreateRequest {
    name: String,
    mode: RuntimeMode,
    capabilities: Option<CapabilityManifest>,
    duration_ms: Option<i64>,
}

async fn profile_create(
    State(state): State<DaemonState>,
    Json(request): Json<ProfileCreateRequest>,
) -> Result<Json<RuntimeControlState>, ApiError> {
    Ok(Json(profile_create_state(&state, request).await))
}

async fn profile_create_state(
    state: &DaemonState,
    request: ProfileCreateRequest,
) -> RuntimeControlState {
    let mut control = state.control.write().await;
    let now = now_wall_ms();
    control.active_profile = ProfilePolicy {
        schema_version: SCHEMA_VERSION.to_string(),
        name: request.name,
        mode: request.mode,
        capabilities: request.capabilities.unwrap_or_default(),
        created_wall_ms: now,
        expires_wall_ms: request.duration_ms.map(|duration| now + duration.max(0)),
        active: false,
    };
    control.generation += 1;
    let result = control.clone();
    drop(control);
    state.publish_event(
        "profile_created",
        serde_json::json!({
            "profile": result.active_profile.name,
            "generation": result.generation
        }),
    );
    result
}

async fn profile_activate(
    State(state): State<DaemonState>,
) -> Result<Json<RuntimeControlState>, ApiError> {
    Ok(Json(profile_activate_state(&state).await))
}

async fn profile_activate_state(state: &DaemonState) -> RuntimeControlState {
    let mut control = state.control.write().await;
    if control.safety_state != SafetyState::Killed {
        control.active_profile.active = true;
        control.safety_state = SafetyState::Running;
        control.generation += 1;
    }
    let result = control.clone();
    drop(control);
    state.publish_event(
        "profile_activated",
        serde_json::json!({
            "profile": result.active_profile.name,
            "generation": result.generation,
            "safety_state": result.safety_state
        }),
    );
    result
}

async fn profile_status(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
    Json(state.control.read().await.clone())
}

async fn control_pause(
    State(state): State<DaemonState>,
) -> Result<Json<RuntimeControlState>, ApiError> {
    Ok(Json(control_pause_state(&state).await))
}

async fn control_pause_state(state: &DaemonState) -> RuntimeControlState {
    let mut control = state.control.write().await;
    if control.safety_state != SafetyState::Killed {
        control.safety_state = SafetyState::Paused;
        control.generation += 1;
    }
    let result = control.clone();
    drop(control);
    state.publish_event(
        "control_paused",
        serde_json::json!({ "generation": result.generation }),
    );
    result
}

async fn control_resume(
    State(state): State<DaemonState>,
) -> Result<Json<RuntimeControlState>, ApiError> {
    Ok(Json(control_resume_state(&state).await))
}

async fn control_resume_state(state: &DaemonState) -> RuntimeControlState {
    let mut control = state.control.write().await;
    if control.safety_state != SafetyState::Killed {
        control.safety_state = SafetyState::Running;
        control.generation += 1;
    }
    let result = control.clone();
    drop(control);
    state.publish_event(
        "control_resumed",
        serde_json::json!({ "generation": result.generation }),
    );
    result
}

async fn control_kill_switch(
    State(state): State<DaemonState>,
) -> Result<Json<RuntimeControlState>, ApiError> {
    Ok(Json(control_kill_switch_state(&state).await))
}

async fn control_kill_switch_state(state: &DaemonState) -> RuntimeControlState {
    let started = Instant::now();
    let mut control = state.control.write().await;
    control.safety_state = SafetyState::Killed;
    control.generation += 1;
    state
        .metrics
        .observe(MetricKind::KillSwitchPropagation, started.elapsed());
    let result = control.clone();
    drop(control);
    state.publish_event(
        "kill_switch",
        serde_json::json!({ "generation": result.generation }),
    );
    result
}

#[derive(Debug)]
struct TraceSnapshot {
    envelope: cua_core::FrameEnvelope,
    path: String,
}

async fn trace_snapshot(state: &DaemonState, turn_id: &str, phase: &str) -> Option<TraceSnapshot> {
    let trace_lane = state.trace_lane.as_ref()?;
    if !trace_frame_artifacts_enabled() {
        return None;
    }
    let lookup = state
        .frame_bus
        .latest_or_capture_timed(CaptureRequest {
            max_width: Some(1280),
            encoding: FrameEncoding::Png,
            force_fresh: true,
        })
        .await
        .ok()?;
    observe_frame_lookup(&state.metrics, &lookup);
    let frame = lookup.frame;
    let relative = format!("frames/{turn_id}_{phase}.png");
    let trace_started = Instant::now();
    if !trace_lane.enqueue_artifact(relative.clone(), frame.bytes.clone()) {
        state.metrics.increment(CounterKind::TraceDrops);
        return None;
    }
    state
        .metrics
        .observe(MetricKind::TraceWrite, trace_started.elapsed());
    Some(TraceSnapshot {
        envelope: frame.envelope,
        path: relative,
    })
}

async fn append_action_turn(
    state: &DaemonState,
    turn_id: String,
    action: serde_json::Value,
    result: serde_json::Value,
    before: Option<TraceSnapshot>,
    after: Option<TraceSnapshot>,
) {
    let Some(trace_lane) = state.trace_lane.as_ref() else {
        return;
    };
    let evidence = result
        .get("evidence")
        .cloned()
        .or_else(|| {
            result
                .get("result")
                .and_then(|inner| inner.get("evidence"))
                .cloned()
        })
        .unwrap_or_else(|| serde_json::json!([]));
    let record = TraceRecord::ActionTurn(ActionTurnRecord {
        schema_version: SCHEMA_VERSION.to_string(),
        turn_id,
        at_wall_ms: now_wall_ms(),
        action,
        result,
        before: before
            .as_ref()
            .and_then(|snapshot| serde_json::to_value(&snapshot.envelope).ok()),
        after: after
            .as_ref()
            .and_then(|snapshot| serde_json::to_value(&snapshot.envelope).ok()),
        before_image_path: before.map(|snapshot| snapshot.path),
        after_image_path: after.map(|snapshot| snapshot.path),
        evidence,
        session: serde_json::json!({
            "profile": state.profile,
            "trace_dir": trace_lane.dir().display().to_string(),
            "capture_backend": state.frame_bus.backend_name(),
            "input_backend": state.input_lane.name()
        }),
    });
    let trace_started = Instant::now();
    if !trace_lane.enqueue_record(record) {
        state.metrics.increment(CounterKind::TraceDrops);
        return;
    }
    state
        .metrics
        .observe(MetricKind::TraceWrite, trace_started.elapsed());
}

async fn input_action(
    State(state): State<DaemonState>,
    Json(action): Json<InputAction>,
) -> Result<Json<cua_core::InputResult>, ApiError> {
    Ok(Json(dispatch_input_action(&state, action).await))
}

async fn input_frame_action(
    State(state): State<DaemonState>,
    Json(request): Json<FrameActionRequest>,
) -> Result<Json<cua_core::InputResult>, ApiError> {
    Ok(Json(
        dispatch_input_action(&state, request.into_display_action()).await,
    ))
}

async fn dispatch_input_action(state: &DaemonState, action: InputAction) -> cua_core::InputResult {
    let started = Instant::now();
    let turn_id = Uuid::new_v4().to_string();
    let action_label = input_action_label(&action);
    state.publish_event(
        "input_started",
        serde_json::json!({
            "label": action_label,
            "source": "automation",
            "tool": "Unix socket",
        }),
    );
    if let InputAction::Sequence { actions, .. } = &action {
        if let Some(initial_step_total) = sequence_step_total(actions.len()) {
            publish_protocol_step(
                state,
                1,
                initial_step_total,
                format!("Preparing {action_label}"),
                "Unix socket",
                5_000,
            );
        }
    } else {
        publish_protocol_status(
            state,
            format!("Preparing {action_label}"),
            "Unix socket",
            5_000,
        );
    }
    let action_json = serde_json::to_value(&action).unwrap_or(serde_json::Value::Null);
    let capture_trace_snapshots = captures_trace_snapshots(&action);
    let before = if capture_trace_snapshots {
        trace_snapshot(state, &turn_id, "before").await
    } else {
        None
    };
    if matches!(
        action,
        InputAction::Pause | InputAction::Resume | InputAction::KillSwitch
    ) {
        return dispatch_control_action(state, action, turn_id, action_json, started).await;
    }
    if let InputAction::Sequence {
        actions,
        inter_action_delay_ms,
    } = action
    {
        return dispatch_sequence_action(
            state,
            actions,
            inter_action_delay_ms,
            turn_id,
            action_json,
            before,
            capture_trace_snapshots,
            started,
        )
        .await;
    }
    let dispatch_started = Instant::now();
    publish_protocol_status(
        state,
        format!("Dispatching {action_label}"),
        "Unix socket",
        5_000,
    );
    let (queue_wait, result) = state
        .input_lane
        .execute(InputRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            idempotency_key: Uuid::new_v4(),
            deadline_mono_ns: None,
            action,
        })
        .await;
    state
        .metrics
        .observe(MetricKind::InputQueueWait, queue_wait);
    if result.effect == Effect::Refused {
        state.metrics.increment(CounterKind::InputRefusals);
    }
    state
        .metrics
        .observe(MetricKind::InputDispatch, dispatch_started.elapsed());
    let after = if capture_trace_snapshots {
        trace_snapshot(state, &turn_id, "after").await
    } else {
        None
    };
    append_action_turn(
        state,
        turn_id,
        action_json,
        serde_json::to_value(&result).unwrap_or(serde_json::Value::Null),
        before,
        after,
    )
    .await;
    let final_prefix = input_effect_status_prefix(&result.effect);
    publish_protocol_status(
        state,
        format!("{final_prefix} {action_label}"),
        "Unix socket",
        3_500,
    );
    publish_input_event(state, "input_completed", &result);
    result
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_sequence_action(
    state: &DaemonState,
    actions: Vec<InputAction>,
    inter_action_delay_ms: u64,
    turn_id: String,
    action_json: serde_json::Value,
    before: Option<TraceSnapshot>,
    capture_trace_snapshots: bool,
    started: Instant,
) -> cua_core::InputResult {
    let action_count = actions.len();
    if action_count == 0 {
        return refused_traced_input(
            state,
            turn_id,
            action_json,
            before,
            capture_trace_snapshots,
            "sequence must contain at least one action",
            started,
        )
        .await;
    }
    if actions
        .iter()
        .any(|action| matches!(action, InputAction::Sequence { .. }))
    {
        return refused_traced_input(
            state,
            turn_id,
            action_json,
            before,
            capture_trace_snapshots,
            "nested sequences are not supported",
            started,
        )
        .await;
    }

    let step_total = sequence_step_total(action_count);
    let delay = Duration::from_millis(inter_action_delay_ms.min(2_000));
    let last_index = action_count.saturating_sub(1);
    let mut aggregate_route = None;
    let mut aggregate_delivery_mode = None;
    let mut aggregate = input_result_with_id(
        Uuid::new_v4(),
        Effect::Confirmed,
        InputRoute::SystemApi,
        DeliveryMode::NotApplicable,
        EvidenceKind::ValueReadback,
        format!("sequence {action_count} actions"),
    );
    aggregate.started_mono_ns = 0;
    aggregate.evidence.clear();

    for (index, action) in actions.into_iter().enumerate() {
        let action_label = input_action_label(&action);
        if let Some(step_total) = step_total {
            publish_protocol_step(
                state,
                (index + 2).min(u16::MAX as usize) as u16,
                step_total,
                format!("Dispatching {}/{} {action_label}", index + 1, action_count),
                "Unix socket",
                5_000,
            );
        }
        let dispatch_started = Instant::now();
        let (queue_wait, result) = state
            .input_lane
            .execute(InputRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                idempotency_key: Uuid::new_v4(),
                deadline_mono_ns: None,
                action,
            })
            .await;
        state
            .metrics
            .observe(MetricKind::InputQueueWait, queue_wait);
        state
            .metrics
            .observe(MetricKind::InputDispatch, dispatch_started.elapsed());
        if result.effect == Effect::Refused {
            state.metrics.increment(CounterKind::InputRefusals);
        }
        aggregate.effect = aggregate_sequence_effect(&aggregate.effect, &result.effect);
        aggregate_route = merge_sequence_route(aggregate_route, &result.route);
        aggregate_delivery_mode =
            merge_sequence_delivery_mode(aggregate_delivery_mode, &result.delivery_mode);
        aggregate.evidence.extend(result.evidence);
        if index < last_index && !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
    }

    aggregate.route = aggregate_route.unwrap_or(InputRoute::SystemApi);
    aggregate.delivery_mode = aggregate_delivery_mode.unwrap_or(DeliveryMode::NotApplicable);
    aggregate.ended_mono_ns = started.elapsed().as_nanos();
    let after = if capture_trace_snapshots {
        trace_snapshot(state, &turn_id, "after").await
    } else {
        None
    };
    append_action_turn(
        state,
        turn_id,
        action_json,
        serde_json::to_value(&aggregate).unwrap_or(serde_json::Value::Null),
        before,
        after,
    )
    .await;
    state
        .metrics
        .observe(MetricKind::InputDispatch, started.elapsed());
    let final_prefix = input_effect_status_prefix(&aggregate.effect);
    if let Some(step_total) = step_total {
        publish_protocol_step(
            state,
            step_total,
            step_total,
            format!("{final_prefix} sequence {action_count} actions"),
            "Unix socket",
            3_500,
        );
    } else {
        publish_protocol_status(
            state,
            format!("{final_prefix} sequence {action_count} actions"),
            "Unix socket",
            3_500,
        );
    }
    publish_input_event(state, "input_completed", &aggregate);
    aggregate
}

fn aggregate_sequence_effect(current: &Effect, next: &Effect) -> Effect {
    use Effect::*;
    match (current, next) {
        (Refused, _) | (_, Refused) => Refused,
        (Failed, _) | (_, Failed) => Failed,
        (Partial, _) | (_, Partial) => Partial,
        (SuspectedNoop, _) | (_, SuspectedNoop) => SuspectedNoop,
        (Unverifiable, _) | (_, Unverifiable) => Unverifiable,
        _ => Confirmed,
    }
}

fn merge_sequence_route(current: Option<InputRoute>, next: &InputRoute) -> Option<InputRoute> {
    match current {
        None => Some(next.clone()),
        Some(current) if current == *next => Some(current),
        Some(_) => Some(InputRoute::SystemApi),
    }
}

fn merge_sequence_delivery_mode(
    current: Option<DeliveryMode>,
    next: &DeliveryMode,
) -> Option<DeliveryMode> {
    match current {
        None => Some(next.clone()),
        Some(current) if current == *next => Some(current),
        Some(_) => Some(DeliveryMode::Unknown),
    }
}

async fn refused_traced_input(
    state: &DaemonState,
    turn_id: String,
    action_json: serde_json::Value,
    before: Option<TraceSnapshot>,
    capture_trace_snapshots: bool,
    message: impl Into<String>,
    started: Instant,
) -> cua_core::InputResult {
    let message = message.into();
    publish_protocol_step(
        state,
        3,
        3,
        format!("Refused {message}"),
        "Unix socket",
        3_500,
    );
    state.metrics.increment(CounterKind::InputRefusals);
    state
        .metrics
        .observe(MetricKind::InputDispatch, started.elapsed());
    let result = refused_input_result(message);
    let after = if capture_trace_snapshots {
        trace_snapshot(state, &turn_id, "after").await
    } else {
        None
    };
    append_action_turn(
        state,
        turn_id,
        action_json,
        serde_json::to_value(&result).unwrap_or(serde_json::Value::Null),
        before,
        after,
    )
    .await;
    publish_input_event(state, "input_refused", &result);
    result
}

fn sequence_step_total(action_count: usize) -> Option<u16> {
    action_count
        .checked_add(2)
        .and_then(|total| u16::try_from(total).ok())
}

async fn dispatch_control_action(
    state: &DaemonState,
    action: InputAction,
    turn_id: String,
    action_json: serde_json::Value,
    started: Instant,
) -> cua_core::InputResult {
    let action_label = input_action_label(&action);
    publish_protocol_status(
        state,
        format!("Dispatching {action_label}"),
        "Unix socket",
        5_000,
    );
    let mut event_kind = "input_completed";
    let mut evidence_message = "safety action accepted by local coordinator";
    {
        let mut control = state.control.write().await;
        match action {
            InputAction::Pause if control.safety_state != SafetyState::Killed => {
                control.safety_state = SafetyState::Paused;
                control.generation += 1;
                event_kind = "control_paused";
            }
            InputAction::Resume if control.safety_state != SafetyState::Killed => {
                control.safety_state = SafetyState::Running;
                control.generation += 1;
                event_kind = "control_resumed";
            }
            InputAction::KillSwitch => {
                control.safety_state = SafetyState::Killed;
                control.generation += 1;
                event_kind = "kill_switch";
            }
            InputAction::Pause | InputAction::Resume => {
                evidence_message = "safety action ignored because kill switch is active";
            }
            _ => {}
        }
        let generation = control.generation;
        drop(control);
        state.publish_event(event_kind, serde_json::json!({ "generation": generation }));
    }
    if matches!(action, InputAction::KillSwitch) {
        state
            .metrics
            .observe(MetricKind::KillSwitchPropagation, started.elapsed());
    }
    state
        .metrics
        .observe(MetricKind::InputDispatch, started.elapsed());
    let result = InputResult {
        schema_version: SCHEMA_VERSION.to_string(),
        idempotency_key: Uuid::new_v4(),
        effect: Effect::Confirmed,
        route: InputRoute::SystemApi,
        delivery_mode: DeliveryMode::NotApplicable,
        started_mono_ns: 0,
        ended_mono_ns: started.elapsed().as_nanos(),
        evidence: vec![Evidence {
            kind: EvidenceKind::ValueReadback,
            message: evidence_message.to_string(),
            frame_id: None,
        }],
    };
    append_action_turn(
        state,
        turn_id,
        action_json,
        serde_json::to_value(&result).unwrap_or(serde_json::Value::Null),
        None,
        None,
    )
    .await;
    publish_protocol_status(
        state,
        format!("Completed {action_label}"),
        "Unix socket",
        3_500,
    );
    publish_input_event(state, "input_completed", &result);
    result
}

fn captures_trace_snapshots(action: &InputAction) -> bool {
    match action {
        InputAction::MouseClick { .. } | InputAction::MouseDrag { .. } => true,
        InputAction::Sequence { actions, .. } => actions.iter().any(captures_trace_snapshots),
        InputAction::MouseMove { .. }
        | InputAction::KeyPress { .. }
        | InputAction::KeyType { .. }
        | InputAction::KeyPaste { .. }
        | InputAction::OpenApp { .. }
        | InputAction::ShellExec { .. }
        | InputAction::Aegis { .. }
        | InputAction::Ctx { .. }
        | InputAction::ClipboardRead { .. }
        | InputAction::ClipboardWrite { .. }
        | InputAction::Pause
        | InputAction::Resume
        | InputAction::KillSwitch => false,
    }
}

async fn clipboard_read(
    State(state): State<DaemonState>,
    Json(request): Json<ClipboardReadRequest>,
) -> Json<ClipboardResult> {
    Json(clipboard_read_state(&state, request).await)
}

async fn clipboard_read_state(
    state: &DaemonState,
    request: ClipboardReadRequest,
) -> ClipboardResult {
    let started = Instant::now();
    let action = "clipboard_read";
    let turn_id = Uuid::new_v4().to_string();
    let action_json = serde_json::to_value(&request).unwrap_or(serde_json::Value::Null);
    let before = trace_snapshot(state, &turn_id, "before").await;
    if request.schema_version != SCHEMA_VERSION {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardRead, started.elapsed());
        let result = clipboard_refusal(action, "schema_version must match the daemon schema");
        let after = trace_snapshot(state, &turn_id, "after").await;
        append_clipboard_turn(state, turn_id, action_json, &result, before, after).await;
        publish_clipboard_event(state, &result);
        return result;
    }
    if !request.allow_sensitive {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardRead, started.elapsed());
        let result = clipboard_refusal(action, "clipboard read requires allow_sensitive=true");
        let after = trace_snapshot(state, &turn_id, "after").await;
        append_clipboard_turn(state, turn_id, action_json, &result, before, after).await;
        publish_clipboard_event(state, &result);
        return result;
    }
    if let Some(message) = clipboard_refusal_reason(state).await {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardRead, started.elapsed());
        let result = clipboard_refusal(action, message);
        let after = trace_snapshot(state, &turn_id, "after").await;
        append_clipboard_turn(state, turn_id, action_json, &result, before, after).await;
        publish_clipboard_event(state, &result);
        return result;
    }
    let result = state
        .computer
        .input_backend()
        .execute(InputRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            idempotency_key: Uuid::new_v4(),
            deadline_mono_ns: None,
            action: InputAction::ClipboardRead {
                allow_sensitive: true,
            },
        })
        .await;
    if result.effect == Effect::Refused {
        state.metrics.increment(CounterKind::ClipboardRefusals);
    }
    state
        .metrics
        .observe(MetricKind::ClipboardRead, started.elapsed());
    let text = (result.effect == Effect::Confirmed)
        .then(|| {
            result
                .evidence
                .first()
                .map(|evidence| evidence.message.clone())
                .unwrap_or_default()
        })
        .filter(|text| !text.is_empty());
    let result = ClipboardResult {
        schema_version: SCHEMA_VERSION.to_string(),
        action: action.to_string(),
        result,
        text,
    };
    let after = trace_snapshot(state, &turn_id, "after").await;
    append_clipboard_turn(state, turn_id, action_json, &result, before, after).await;
    publish_clipboard_event(state, &result);
    result
}

async fn clipboard_write(
    State(state): State<DaemonState>,
    Json(request): Json<ClipboardWriteRequest>,
) -> Result<Json<ClipboardResult>, ApiError> {
    Ok(Json(clipboard_write_state(&state, request).await))
}

async fn clipboard_write_state(
    state: &DaemonState,
    request: ClipboardWriteRequest,
) -> ClipboardResult {
    let started = Instant::now();
    let action = "clipboard_write";
    let turn_id = Uuid::new_v4().to_string();
    let action_json = serde_json::to_value(&request).unwrap_or(serde_json::Value::Null);
    let before = trace_snapshot(state, &turn_id, "before").await;
    if request.schema_version != SCHEMA_VERSION {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardWrite, started.elapsed());
        let result = clipboard_refusal(action, "schema_version must match the daemon schema");
        let after = trace_snapshot(state, &turn_id, "after").await;
        append_clipboard_turn(state, turn_id, action_json, &result, before, after).await;
        publish_clipboard_event(state, &result);
        return result;
    }
    if let Some(message) = clipboard_refusal_reason(state).await {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardWrite, started.elapsed());
        let result = clipboard_refusal(action, message);
        let after = trace_snapshot(state, &turn_id, "after").await;
        append_clipboard_turn(state, turn_id, action_json, &result, before, after).await;
        publish_clipboard_event(state, &result);
        return result;
    }
    let result = state
        .computer
        .input_backend()
        .execute(InputRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            idempotency_key: Uuid::new_v4(),
            deadline_mono_ns: None,
            action: InputAction::ClipboardWrite { text: request.text },
        })
        .await;
    if result.effect == Effect::Refused {
        state.metrics.increment(CounterKind::ClipboardRefusals);
    }
    state
        .metrics
        .observe(MetricKind::ClipboardWrite, started.elapsed());
    let result = ClipboardResult {
        schema_version: SCHEMA_VERSION.to_string(),
        action: action.to_string(),
        result,
        text: None,
    };
    let after = trace_snapshot(state, &turn_id, "after").await;
    append_clipboard_turn(state, turn_id, action_json, &result, before, after).await;
    publish_clipboard_event(state, &result);
    result
}

fn publish_input_event(state: &DaemonState, kind: &'static str, result: &InputResult) {
    state.publish_event(
        kind,
        serde_json::json!({
            "effect": result.effect,
            "route": result.route,
            "evidence_kind": result.evidence.first().map(|evidence| &evidence.kind),
        }),
    );
}

fn publish_clipboard_event(state: &DaemonState, result: &ClipboardResult) {
    state.publish_event(
        "clipboard_action",
        serde_json::json!({
            "action": result.action,
            "effect": result.result.effect,
            "route": result.result.route,
            "returned_text": result.text.is_some()
        }),
    );
}

async fn append_clipboard_turn(
    state: &DaemonState,
    turn_id: String,
    action_json: serde_json::Value,
    result: &ClipboardResult,
    before: Option<TraceSnapshot>,
    after: Option<TraceSnapshot>,
) {
    append_action_turn(
        state,
        turn_id,
        action_json,
        serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
        before,
        after,
    )
    .await;
}

async fn clipboard_refusal_reason(state: &DaemonState) -> Option<&'static str> {
    let control = state.control.read().await;
    if control.safety_state == SafetyState::Killed {
        return Some("kill-switch is active");
    }
    if control.safety_state == SafetyState::Paused {
        return Some("runtime is paused");
    }
    if !control.active_profile.active {
        return Some("profile is not active");
    }
    if !control.active_profile.capabilities.clipboard {
        return Some("active profile does not grant clipboard access");
    }
    None
}

fn clipboard_refusal(action: &str, message: impl Into<String>) -> ClipboardResult {
    ClipboardResult {
        schema_version: SCHEMA_VERSION.to_string(),
        action: action.to_string(),
        result: input_result(
            Effect::Refused,
            InputRoute::Unavailable,
            DeliveryMode::NotApplicable,
            EvidenceKind::Refusal,
            message,
        ),
        text: None,
    }
}

fn refused_input_result(message: impl Into<String>) -> InputResult {
    input_result(
        Effect::Refused,
        InputRoute::Unavailable,
        DeliveryMode::NotApplicable,
        EvidenceKind::Refusal,
        message,
    )
}

fn input_result(
    effect: Effect,
    route: InputRoute,
    delivery_mode: DeliveryMode,
    evidence_kind: EvidenceKind,
    evidence_message: impl Into<String>,
) -> InputResult {
    input_result_with_id(
        Uuid::new_v4(),
        effect,
        route,
        delivery_mode,
        evidence_kind,
        evidence_message,
    )
}

fn input_result_with_id(
    idempotency_key: Uuid,
    effect: Effect,
    route: InputRoute,
    delivery_mode: DeliveryMode,
    evidence_kind: EvidenceKind,
    evidence_message: impl Into<String>,
) -> InputResult {
    let started_mono_ns = std::time::Instant::now().elapsed().as_nanos();
    InputResult {
        schema_version: SCHEMA_VERSION.to_string(),
        idempotency_key,
        effect,
        route,
        delivery_mode,
        started_mono_ns,
        ended_mono_ns: started_mono_ns,
        evidence: vec![Evidence {
            kind: evidence_kind,
            message: evidence_message.into(),
            frame_id: None,
        }],
    }
}

const METRIC_BUCKETS_MS: [u64; 9] = [1, 2, 5, 10, 16, 33, 50, 100, 250];

#[derive(Debug, Clone, Copy)]
enum MetricKind {
    CaptureScreenshot,
    CaptureQueueWait,
    CaptureEncode,
    EncodeQueueWait,
    EncodeDispatch,
    StreamMjpegTick,
    StreamWsTick,
    StreamUnixTick,
    InputQueueWait,
    InputDispatch,
    ClipboardRead,
    ClipboardWrite,
    ModelSend,
    ModelResponse,
    ModelParse,
    ModelQueueWait,
    PolicyCheck,
    PermissionQueueWait,
    PermissionProbe,
    Verification,
    TraceWrite,
    KillSwitchPropagation,
}

impl MetricKind {
    const ALL: [Self; 22] = [
        Self::CaptureScreenshot,
        Self::CaptureQueueWait,
        Self::CaptureEncode,
        Self::EncodeQueueWait,
        Self::EncodeDispatch,
        Self::StreamMjpegTick,
        Self::StreamWsTick,
        Self::StreamUnixTick,
        Self::InputQueueWait,
        Self::InputDispatch,
        Self::ClipboardRead,
        Self::ClipboardWrite,
        Self::ModelSend,
        Self::ModelResponse,
        Self::ModelParse,
        Self::ModelQueueWait,
        Self::PolicyCheck,
        Self::PermissionQueueWait,
        Self::PermissionProbe,
        Self::Verification,
        Self::TraceWrite,
        Self::KillSwitchPropagation,
    ];

    fn index(self) -> usize {
        match self {
            Self::CaptureScreenshot => 0,
            Self::CaptureQueueWait => 1,
            Self::CaptureEncode => 2,
            Self::EncodeQueueWait => 3,
            Self::EncodeDispatch => 4,
            Self::StreamMjpegTick => 5,
            Self::StreamWsTick => 6,
            Self::StreamUnixTick => 7,
            Self::InputQueueWait => 8,
            Self::InputDispatch => 9,
            Self::ClipboardRead => 10,
            Self::ClipboardWrite => 11,
            Self::ModelSend => 12,
            Self::ModelResponse => 13,
            Self::ModelParse => 14,
            Self::ModelQueueWait => 15,
            Self::PolicyCheck => 16,
            Self::PermissionQueueWait => 17,
            Self::PermissionProbe => 18,
            Self::Verification => 19,
            Self::TraceWrite => 20,
            Self::KillSwitchPropagation => 21,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::CaptureScreenshot => "capture.screenshot",
            Self::CaptureQueueWait => "capture.queue_wait",
            Self::CaptureEncode => "capture.encode",
            Self::EncodeQueueWait => "encode.queue_wait",
            Self::EncodeDispatch => "encode.dispatch",
            Self::StreamMjpegTick => "stream.mjpeg.tick",
            Self::StreamWsTick => "stream.ws.tick",
            Self::StreamUnixTick => "stream.unix.tick",
            Self::InputQueueWait => "input.queue_wait",
            Self::InputDispatch => "input.dispatch",
            Self::ClipboardRead => "clipboard.read",
            Self::ClipboardWrite => "clipboard.write",
            Self::ModelSend => "model.send",
            Self::ModelResponse => "model.response",
            Self::ModelParse => "model.parse",
            Self::ModelQueueWait => "model.queue_wait",
            Self::PolicyCheck => "policy.check",
            Self::PermissionQueueWait => "permission.queue_wait",
            Self::PermissionProbe => "permission.probe",
            Self::Verification => "verification",
            Self::TraceWrite => "trace.write",
            Self::KillSwitchPropagation => "control.kill_switch.propagation",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum CounterKind {
    MjpegFrames,
    WsFrames,
    UnixFrames,
    CaptureSckFrames,
    CaptureCoreGraphicsFrames,
    CaptureSyntheticFrames,
    InputRefusals,
    ClipboardRefusals,
    EventDrops,
    PermissionFallbacks,
    TraceDrops,
    ModelDrops,
    EncodeDrops,
    UnixFrameDrops,
}

impl CounterKind {
    const ALL: [Self; 14] = [
        Self::MjpegFrames,
        Self::WsFrames,
        Self::UnixFrames,
        Self::CaptureSckFrames,
        Self::CaptureCoreGraphicsFrames,
        Self::CaptureSyntheticFrames,
        Self::InputRefusals,
        Self::ClipboardRefusals,
        Self::EventDrops,
        Self::PermissionFallbacks,
        Self::TraceDrops,
        Self::ModelDrops,
        Self::EncodeDrops,
        Self::UnixFrameDrops,
    ];

    fn index(self) -> usize {
        match self {
            Self::MjpegFrames => 0,
            Self::WsFrames => 1,
            Self::UnixFrames => 2,
            Self::CaptureSckFrames => 3,
            Self::CaptureCoreGraphicsFrames => 4,
            Self::CaptureSyntheticFrames => 5,
            Self::InputRefusals => 6,
            Self::ClipboardRefusals => 7,
            Self::EventDrops => 8,
            Self::PermissionFallbacks => 9,
            Self::TraceDrops => 10,
            Self::ModelDrops => 11,
            Self::EncodeDrops => 12,
            Self::UnixFrameDrops => 13,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::MjpegFrames => "stream.mjpeg.frames",
            Self::WsFrames => "stream.ws.frames",
            Self::UnixFrames => "stream.unix.frames",
            Self::CaptureSckFrames => "capture.sck.frames",
            Self::CaptureCoreGraphicsFrames => "capture.core_graphics.frames",
            Self::CaptureSyntheticFrames => "capture.synthetic.frames",
            Self::InputRefusals => "input.refusals",
            Self::ClipboardRefusals => "clipboard.refusals",
            Self::EventDrops => "events.dropped",
            Self::PermissionFallbacks => "permission.fallbacks",
            Self::TraceDrops => "trace.dropped",
            Self::ModelDrops => "model.dropped",
            Self::EncodeDrops => "encode.dropped",
            Self::UnixFrameDrops => "stream.unix.dropped_frames",
        }
    }
}

struct RuntimeMetrics {
    histograms: Vec<MetricSeries>,
    counters: Vec<AtomicU64>,
}

impl Default for RuntimeMetrics {
    fn default() -> Self {
        Self {
            histograms: MetricKind::ALL
                .iter()
                .map(|kind| MetricSeries::new(kind.name()))
                .collect(),
            counters: CounterKind::ALL.iter().map(|_| AtomicU64::new(0)).collect(),
        }
    }
}

impl RuntimeMetrics {
    fn observe(&self, kind: MetricKind, duration: Duration) {
        self.histograms[kind.index()].observe(duration);
    }

    fn observe_ns(&self, kind: MetricKind, ns: u64) {
        self.histograms[kind.index()].observe_ns(ns);
    }

    fn increment(&self, kind: CounterKind) {
        self.counters[kind.index()].fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self, active_streams: u32) -> MetricsSnapshot {
        let mut counters = BTreeMap::new();
        for kind in CounterKind::ALL {
            counters.insert(
                kind.name().to_string(),
                self.counters[kind.index()].load(Ordering::Relaxed),
            );
        }
        counters.insert("streams.active".to_string(), active_streams as u64);

        MetricsSnapshot {
            schema_version: SCHEMA_VERSION.to_string(),
            histograms: MetricKind::ALL
                .iter()
                .map(|kind| self.histograms[kind.index()].snapshot())
                .collect(),
            counters,
        }
    }
}

struct MetricSeries {
    name: &'static str,
    count: AtomicU64,
    total_ns: AtomicU64,
    max_ns: AtomicU64,
    buckets: Vec<AtomicU64>,
}

impl MetricSeries {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            count: AtomicU64::new(0),
            total_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
            buckets: METRIC_BUCKETS_MS
                .iter()
                .map(|_| AtomicU64::new(0))
                .collect(),
        }
    }

    fn observe(&self, duration: Duration) {
        self.observe_ns(duration.as_nanos().min(u64::MAX as u128) as u64);
    }

    fn observe_ns(&self, ns: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_ns.fetch_add(ns, Ordering::Relaxed);
        self.max_ns.fetch_max(ns, Ordering::Relaxed);

        let ms = ns / 1_000_000;
        for (index, upper_bound) in METRIC_BUCKETS_MS.iter().enumerate() {
            if ms <= *upper_bound {
                self.buckets[index].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn snapshot(&self) -> MetricHistogram {
        MetricHistogram {
            name: self.name.to_string(),
            count: self.count.load(Ordering::Relaxed),
            total_ns: self.total_ns.load(Ordering::Relaxed),
            max_ns: self.max_ns.load(Ordering::Relaxed),
            buckets: METRIC_BUCKETS_MS
                .iter()
                .enumerate()
                .map(|(index, le_ms)| MetricBucket {
                    le_ms: *le_ms,
                    count: self.buckets[index].load(Ordering::Relaxed),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ModelEvalRequest {
    live: Option<bool>,
    max_calls: Option<usize>,
    max_output_tokens: Option<u32>,
}

async fn model_eval(
    State(state): State<DaemonState>,
    Json(request): Json<ModelEvalRequest>,
) -> Result<Json<EvalReport>, ApiError> {
    let lookup = state
        .frame_bus
        .latest_or_capture_timed(CaptureRequest {
            max_width: Some(640),
            encoding: FrameEncoding::Png,
            force_fresh: true,
        })
        .await
        .map_err(ApiError::internal)?;
    observe_frame_lookup(&state.metrics, &lookup);
    let frame = lookup.frame.as_payload(true);
    let parse_started = Instant::now();
    let mut config = EvalConfig::default();
    config.live = request.live.unwrap_or(false);
    config.max_calls = request.max_calls.unwrap_or(config.max_calls);
    if let Some(max_output_tokens) = request.max_output_tokens {
        for candidate in &mut config.candidates {
            candidate.max_output_tokens = max_output_tokens;
        }
    }
    state
        .metrics
        .observe(MetricKind::ModelParse, parse_started.elapsed());
    let key = std::env::var("OPENROUTER_API_KEY").ok();
    let lane_result = state
        .model_lane
        .evaluate(config, Some(frame), key)
        .await
        .map_err(|error| {
            state.metrics.increment(CounterKind::ModelDrops);
            ApiError::busy(format!("model lane unavailable: {error:?}"))
        })?;
    state
        .metrics
        .observe(MetricKind::ModelQueueWait, lane_result.queue_wait);
    state
        .metrics
        .observe(MetricKind::ModelSend, lane_result.run_duration);
    let response_started = Instant::now();
    let response = Json(lane_result.report);
    state
        .metrics
        .observe(MetricKind::ModelResponse, response_started.elapsed());
    state
        .metrics
        .observe(MetricKind::Verification, response_started.elapsed());
    Ok(response)
}

#[derive(Debug)]
pub struct ApiError(ApiErrorBody, StatusCode);

impl ApiError {
    fn internal(error: anyhow::Error) -> Self {
        Self(
            ApiErrorBody {
                schema_version: SCHEMA_VERSION.to_string(),
                code: "internal_error".to_string(),
                message: error.to_string(),
                details: BTreeMap::new(),
            },
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    }

    fn busy(message: impl Into<String>) -> Self {
        Self(
            ApiErrorBody {
                schema_version: SCHEMA_VERSION.to_string(),
                code: "busy".to_string(),
                message: message.into(),
                details: BTreeMap::new(),
            },
            StatusCode::SERVICE_UNAVAILABLE,
        )
    }

    fn computer_backend(error: anyhow::Error) -> Self {
        let message = error.to_string();
        if is_backend_unavailable_message(&message) {
            return Self::busy(message);
        }
        Self::internal(error)
    }

    fn bad_request(field: impl Into<String>, message: impl Into<String>) -> Self {
        let mut details = BTreeMap::new();
        details.insert("field".to_string(), field.into());
        Self(
            ApiErrorBody {
                schema_version: SCHEMA_VERSION.to_string(),
                code: "bad_request".to_string(),
                message: message.into(),
                details,
            },
            StatusCode::BAD_REQUEST,
        )
    }

    fn forbidden(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self(
            ApiErrorBody {
                schema_version: SCHEMA_VERSION.to_string(),
                code: code.into(),
                message: message.into(),
                details: BTreeMap::new(),
            },
            StatusCode::FORBIDDEN,
        )
    }

    fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self(
            ApiErrorBody {
                schema_version: SCHEMA_VERSION.to_string(),
                code: code.into(),
                message: message.into(),
                details: BTreeMap::new(),
            },
            StatusCode::CONFLICT,
        )
    }
}

fn is_backend_unavailable_message(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    [
        "unavailable",
        "not connected",
        "not implemented",
        "only implemented",
        "remote cua endpoint is required",
        "remote computer backend is misconfigured",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        (self.1, headers, Json(self.0)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct AcceptingInputBackend;

    #[async_trait::async_trait]
    impl InputBackend for AcceptingInputBackend {
        async fn execute(&self, request: InputRequest) -> InputResult {
            input_result_with_id(
                request.idempotency_key,
                Effect::Confirmed,
                InputRoute::SystemApi,
                DeliveryMode::Background,
                EvidenceKind::ValueReadback,
                "accepted by test computer backend",
            )
        }

        fn name(&self) -> &'static str {
            "test-accepting"
        }
    }

    struct TestComputerBackend {
        input: Arc<AcceptingInputBackend>,
        descriptor: cua_core::ComputerBackendDescriptor,
        cursor: cua_core::CursorState,
        windows: Vec<WindowInfo>,
        permissions: PermissionReport,
    }

    impl Default for TestComputerBackend {
        fn default() -> Self {
            Self {
                input: Arc::new(AcceptingInputBackend),
                descriptor: cua_core::ComputerBackendDescriptor {
                    kind: cua_core::ComputerBackendKind::Synthetic,
                    provider: "test".to_string(),
                    runtime: "cua".to_string(),
                    instance_id: Some("test-instance".to_string()),
                    pool_id: Some("test-pool".to_string()),
                    region: Some("test-region".to_string()),
                    os: "test-os".to_string(),
                    capabilities: CapabilityManifest::default(),
                },
                cursor: cua_core::CursorState {
                    x: 12.0,
                    y: 34.0,
                    visible: true,
                    included_in_frame: false,
                },
                windows: Vec::new(),
                permissions: PermissionReport {
                    screen_recording: PermissionState::NotApplicable,
                    accessibility_input: PermissionState::NotApplicable,
                    input_monitoring: PermissionState::NotApplicable,
                    automation: PermissionState::NotApplicable,
                    clipboard: PermissionState::NotApplicable,
                    portal: PermissionState::NotApplicable,
                },
            }
        }
    }

    #[async_trait::async_trait]
    impl ComputerBackend for TestComputerBackend {
        fn descriptor(&self) -> cua_core::ComputerBackendDescriptor {
            self.descriptor.clone()
        }

        fn capture_backend(&self) -> Arc<dyn cua_capture::CaptureBackend> {
            Arc::new(cua_capture::SyntheticCaptureBackend::default())
        }

        fn input_backend(&self) -> Arc<dyn InputBackend> {
            self.input.clone()
        }

        async fn permission_report(&self) -> PermissionReport {
            self.permissions.clone()
        }

        async fn request_accessibility_input_access(&self) -> PermissionState {
            self.permissions.accessibility_input.clone()
        }

        async fn cursor_state(&self) -> cua_core::CursorState {
            self.cursor.clone()
        }

        async fn window_list(&self) -> anyhow::Result<Vec<WindowInfo>> {
            Ok(self.windows.clone())
        }
    }

    fn accepting_test_state() -> DaemonState {
        DaemonState::with_computer_backend(
            "test",
            "token",
            UiMode::Headful,
            Arc::new(TestComputerBackend::default()),
        )
    }

    fn test_compact_scene(mode: UiMode) -> cua_core::IslandScene {
        cua_core::IslandScene {
            schema_version: cua_core::ISLAND_SCHEMA_VERSION.to_string(),
            layout: cua_core::IslandLayout::Compact,
            mode,
            background: cua_core::default_island_background(),
            regions: BTreeMap::from([
                (
                    "left".to_string(),
                    cua_core::IslandRegion {
                        items: vec![
                            cua_core::IslandItem::Label {
                                id: "orb".to_string(),
                                text: "orb".to_string(),
                            },
                            cua_core::IslandItem::Label {
                                id: "input".to_string(),
                                text: "Automation".to_string(),
                            },
                        ],
                    },
                ),
                (
                    "center".to_string(),
                    cua_core::IslandRegion {
                        items: vec![cua_core::IslandItem::Marquee {
                            id: "status".to_string(),
                            text: "Programmable scene".to_string(),
                        }],
                    },
                ),
                (
                    "right".to_string(),
                    cua_core::IslandRegion {
                        items: vec![
                            cua_core::IslandItem::Chip {
                                id: "transport".to_string(),
                                text: "Socket".to_string(),
                            },
                            cua_core::IslandItem::Chip {
                                id: "target".to_string(),
                                text: "macOS".to_string(),
                            },
                            cua_core::IslandItem::DotChase {
                                id: "activity".to_string(),
                                active: true,
                                palette: cua_core::IslandPalette::BlueNeon,
                                count: 6,
                                speed: 8,
                            },
                        ],
                    },
                ),
            ]),
            ambient: Vec::new(),
            actors: Vec::new(),
            theme: None,
        }
    }

    #[test]
    fn health_ready_ignores_optional_unknown_permissions() {
        let permissions = PermissionReport {
            screen_recording: PermissionState::Granted,
            accessibility_input: PermissionState::Granted,
            input_monitoring: PermissionState::Granted,
            automation: PermissionState::Unknown,
            clipboard: PermissionState::Unknown,
            portal: PermissionState::NotApplicable,
        };

        assert_eq!(
            health_status(&permissions, true, &SafetyState::Running),
            CapabilityState::Ready
        );
    }

    #[test]
    fn health_degrades_without_required_runtime_state() {
        let mut permissions = PermissionReport {
            screen_recording: PermissionState::Granted,
            accessibility_input: PermissionState::Granted,
            input_monitoring: PermissionState::Granted,
            automation: PermissionState::Unknown,
            clipboard: PermissionState::Unknown,
            portal: PermissionState::NotApplicable,
        };

        assert_eq!(
            health_status(&permissions, false, &SafetyState::Running),
            CapabilityState::Degraded
        );
        permissions.screen_recording = PermissionState::Missing;
        assert_eq!(
            health_status(&permissions, true, &SafetyState::Running),
            CapabilityState::Degraded
        );
        assert_eq!(
            health_status(&permissions, true, &SafetyState::Killed),
            CapabilityState::Refused
        );
    }

    #[test]
    fn hud_pid_parser_matches_exact_voice_executable() {
        let candidate = PathBuf::from("/Applications/cua.app/Contents/MacOS/cua-voice");
        let ps_output = "\
        42 /bin/zsh -c ps ax | rg /Applications/cua.app/Contents/MacOS/cua-voice
        99 /Applications/cua.app/Contents/MacOS/cua-voice
       101 /Applications/cua.app/Contents/MacOS/cua --profile default serve
";

        assert_eq!(
            parse_hud_pid_from_ps(ps_output, &[candidate], 101),
            Some(99)
        );
    }

    #[tokio::test]
    async fn clipboard_write_requires_profile_grant() {
        let state = DaemonState::synthetic("test", "token");
        let result = clipboard_write_state(
            &state,
            ClipboardWriteRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                text: "denied".to_string(),
            },
        )
        .await;

        assert_eq!(result.result.effect, Effect::Refused);
        assert_eq!(
            result.result.evidence[0].message,
            "active profile does not grant clipboard access"
        );
    }

    #[tokio::test]
    async fn refused_clipboard_write_does_not_mutate_clipboard() {
        let state = DaemonState::synthetic("test", "token");
        *state.clipboard.write().await = Some("original".to_string());
        let result = clipboard_write_state(
            &state,
            ClipboardWriteRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                text: "overwrite".to_string(),
            },
        )
        .await;

        assert_eq!(result.result.effect, Effect::Refused);
        assert_eq!(state.clipboard.read().await.as_deref(), Some("original"));
    }

    #[tokio::test]
    async fn disabled_profile_refuses_input_clipboard_action() {
        let state = DaemonState::synthetic("test", "token");
        *state.clipboard.write().await = Some("original".to_string());
        let result = dispatch_input_action(
            &state,
            InputAction::ClipboardWrite {
                text: "overwrite".to_string(),
            },
        )
        .await;

        assert_eq!(result.effect, Effect::Refused);
        assert_eq!(state.clipboard.read().await.as_deref(), Some("original"));
    }

    #[tokio::test]
    async fn enabled_profile_dispatches_input_clipboard_action() {
        let state = clipboard_enabled_state().await;
        let result = dispatch_input_action(
            &state,
            InputAction::ClipboardWrite {
                text: "action lane write".to_string(),
            },
        )
        .await;

        assert_eq!(result.effect, Effect::Confirmed);
        assert_eq!(result.route, InputRoute::SystemApi);
        assert_eq!(result.delivery_mode, DeliveryMode::Background);
        assert_eq!(
            result.evidence[0].message,
            "accepted by test computer backend"
        );
    }

    #[tokio::test]
    async fn clipboard_read_requires_sensitive_acknowledgment() {
        let state = clipboard_enabled_state().await;
        let Json(result) = clipboard_read(
            State(state),
            Json(ClipboardReadRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                allow_sensitive: false,
            }),
        )
        .await;

        assert_eq!(result.result.effect, Effect::Refused);
        assert_eq!(
            result.result.evidence[0].message,
            "clipboard read requires allow_sensitive=true"
        );
    }

    #[tokio::test]
    async fn clipboard_write_and_sensitive_read_round_trip() {
        let state = clipboard_enabled_state().await;
        let write_result = clipboard_write_state(
            &state,
            ClipboardWriteRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                text: "hello from test".to_string(),
            },
        )
        .await;
        let Json(read_result) = clipboard_read(
            State(state),
            Json(ClipboardReadRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                allow_sensitive: true,
            }),
        )
        .await;

        assert_eq!(write_result.result.effect, Effect::Confirmed);
        assert_eq!(read_result.result.effect, Effect::Confirmed);
        assert_eq!(
            read_result.text.as_deref(),
            Some("accepted by test computer backend")
        );
    }

    #[tokio::test]
    async fn input_lane_fails_when_queue_is_full() {
        let (sender, _receiver) = mpsc::channel(1);
        let (reply, _wait) = oneshot::channel();
        sender
            .try_send(InputJob {
                enqueued_at: Instant::now(),
                request: sample_mouse_request(),
                reply,
            })
            .unwrap();
        let lane = InputLane {
            sender,
            backend_name: "blocked-test",
        };

        let (_, result) = lane.execute(sample_mouse_request()).await;

        assert_eq!(result.effect, Effect::Failed);
        assert_eq!(result.evidence[0].kind, EvidenceKind::Error);
        assert_eq!(result.evidence[0].message, "input lane queue is full");
    }

    #[tokio::test]
    async fn permission_lane_falls_back_when_queue_is_full() {
        let (sender, _receiver) = mpsc::channel(1);
        let (reply, _wait) = oneshot::channel();
        sender
            .try_send(PermissionJob {
                enqueued_at: Instant::now(),
                reply,
            })
            .unwrap();
        let lane = PermissionLane { sender };

        let result = lane.report().await;

        assert!(result.fallback);
        assert_eq!(
            result.report.portal,
            cua_core::PermissionState::NotApplicable
        );
    }

    #[tokio::test]
    async fn permission_report_records_metrics() {
        let state = DaemonState::synthetic("test", "token");

        let _ = state.permission_report().await;
        let snapshot = state.metrics.snapshot(0);

        assert!(snapshot
            .histograms
            .iter()
            .any(|histogram| histogram.name == "permission.probe" && histogram.count == 1));
    }

    #[tokio::test]
    async fn health_reports_selected_computer_backend() {
        let state = accepting_test_state();

        let health = state.health().await;

        assert_eq!(health.computer_backend.provider, "test");
        assert_eq!(health.inventory.computer_backend.provider, "test");
        assert_eq!(
            health.computer_backend.kind,
            cua_core::ComputerBackendKind::Synthetic
        );
    }

    #[tokio::test]
    async fn desktop_state_uses_selected_computer_backend_observation() {
        let mut backend = TestComputerBackend::default();
        backend.cursor.x = 111.0;
        backend.cursor.y = 222.0;
        backend.windows = vec![WindowInfo {
            id: "remote-window".to_string(),
            app_name: Some("Remote App".to_string()),
            title: Some("Remote Window".to_string()),
            layer: 0,
            x: 10,
            y: 20,
            width: 300,
            height: 200,
            focused: true,
        }];
        let state =
            DaemonState::with_computer_backend("test", "token", UiMode::Headful, Arc::new(backend));

        let desktop = desktop_state(&state).await.unwrap();

        assert_eq!(desktop.cursor.x, 111.0);
        assert_eq!(desktop.cursor.y, 222.0);
        assert_eq!(desktop.windows[0].id, "remote-window");
    }

    #[tokio::test]
    async fn model_lane_refuses_when_queue_is_full() {
        let (sender, _receiver) = mpsc::channel(1);
        let (reply, _wait) = oneshot::channel();
        sender
            .try_send(ModelJob {
                enqueued_at: Instant::now(),
                config: EvalConfig::default(),
                frame: None,
                api_key: None,
                reply,
            })
            .unwrap();
        let lane = ModelLane {
            sender,
            active: Arc::new(AtomicU32::new(0)),
        };

        let result = lane.evaluate(EvalConfig::default(), None, None).await;

        assert!(matches!(result, Err(ModelLaneError::Full)));
    }

    #[tokio::test]
    async fn encode_lane_refuses_when_queue_is_full() {
        let (sender, _receiver) = mpsc::channel(1);
        let (reply, _wait) = oneshot::channel();
        sender
            .try_send(EncodeJob::Payload {
                enqueued_at: Instant::now(),
                frame: sample_frame(),
                include_bytes: true,
                reply,
            })
            .unwrap();
        let lane = EncodeLane { sender };

        let result = lane.payload(sample_frame(), true).await;

        assert!(matches!(result, Err(EncodeLaneError::Full)));
    }

    #[tokio::test]
    async fn encode_lane_records_metrics_for_screenshot_payload() {
        let state = DaemonState::synthetic("test", "token");
        let encoded = state
            .encode_lane
            .payload(sample_frame(), true)
            .await
            .expect("encode lane should return payload");
        observe_encode_result(&state.metrics, &encoded);
        let snapshot = state.metrics.snapshot(0);

        assert!(snapshot
            .histograms
            .iter()
            .any(|histogram| histogram.name == "encode.queue_wait" && histogram.count == 1));
        assert!(snapshot
            .histograms
            .iter()
            .any(|histogram| histogram.name == "encode.dispatch" && histogram.count == 1));
    }

    #[tokio::test]
    async fn metadata_only_screenshot_skips_encode_lane() {
        let state = DaemonState::synthetic("test", "token");

        let payload = screenshot_payload(
            &state,
            ScreenshotRequest {
                max_width: Some(640),
                include_bytes: Some(false),
                force_fresh: Some(true),
                encoding: Some(FrameEncoding::Png),
            },
        )
        .await
        .expect("metadata-only screenshot should succeed");
        let snapshot = state.metrics.snapshot(0);

        assert_eq!(payload.bytes_base64, None);
        assert_eq!(
            snapshot
                .histograms
                .iter()
                .find(|histogram| histogram.name == "encode.dispatch")
                .map(|histogram| histogram.count),
            Some(0)
        );
    }

    #[tokio::test]
    async fn capture_source_counters_are_recorded_for_frame_lookups() {
        let state = DaemonState::synthetic("test", "token");

        let _ = screenshot_payload(
            &state,
            ScreenshotRequest {
                max_width: Some(640),
                include_bytes: Some(false),
                force_fresh: Some(true),
                encoding: Some(FrameEncoding::Png),
            },
        )
        .await
        .expect("synthetic screenshot should succeed");
        let snapshot = state.metrics.snapshot(0);

        assert_eq!(snapshot.counters["capture.synthetic.frames"], 1);
        assert_eq!(snapshot.counters["capture.sck.frames"], 0);
        assert_eq!(snapshot.counters["capture.core_graphics.frames"], 0);
    }

    #[tokio::test]
    async fn context_snapshot_returns_frame_and_desktop_state() {
        let state = DaemonState::synthetic("test", "token");

        let snapshot = context_snapshot_payload(
            &state,
            ContextSnapshotRequest {
                max_width: Some(640),
                include_bytes: Some(false),
                force_fresh: Some(true),
                encoding: Some(FrameEncoding::Png),
            },
        )
        .await
        .expect("context snapshot should succeed");

        assert_eq!(snapshot.schema_version, SCHEMA_VERSION);
        assert_eq!(snapshot.frame.envelope.encoding, FrameEncoding::Png);
        assert!(snapshot.frame.envelope.width > 0);
        assert!(!snapshot.desktop.displays.is_empty());

        let ui_steps = state
            .events
            .snapshot()
            .await
            .into_iter()
            .filter(|event| event["kind"] == "ui_step")
            .collect::<Vec<_>>();
        assert_eq!(ui_steps.len(), 1);
        assert_eq!(ui_steps[0]["data"]["label"], "Capturing desktop context");
    }

    #[tokio::test]
    async fn force_fresh_screenshot_reuses_recent_resident_frame_with_requested_encoding() {
        let state = DaemonState::synthetic("test", "token");
        let seeded = state
            .frame_bus
            .latest_or_capture_timed(CaptureRequest {
                max_width: Some(640),
                encoding: FrameEncoding::Jpeg,
                force_fresh: true,
            })
            .await
            .unwrap();

        let payload = screenshot_payload(
            &state,
            ScreenshotRequest {
                max_width: Some(320),
                include_bytes: Some(false),
                force_fresh: Some(true),
                encoding: Some(FrameEncoding::Png),
            },
        )
        .await
        .unwrap();

        assert_eq!(payload.envelope.frame_id, seeded.frame.envelope.frame_id);
        assert_eq!(payload.envelope.encoding, FrameEncoding::Png);
        assert_eq!(payload.envelope.width, 320);
        assert_eq!(payload.bytes_base64, None);
    }

    #[tokio::test]
    async fn unix_runtime_methods_share_daemon_state_contracts() {
        let state = DaemonState::synthetic("unix-methods", "token");

        let status = unix_result(
            handle_unix_request(&state, unix_request("status", serde_json::json!({}))).await,
        );
        assert_eq!(status["active_profile"], "unix-methods");
        assert!(status["inventory"]["config"]["profile_root"]
            .as_str()
            .unwrap()
            .contains("unix-methods"));
        assert_eq!(
            status["inventory"]["config"]["profile_token_present"],
            false
        );

        let manifest = unix_result(
            handle_unix_request(&state, unix_request("manifest", serde_json::json!({}))).await,
        );
        assert!(manifest["public_surfaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|surface| surface == "local_unix_socket"));
        assert!(manifest["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|endpoint| endpoint == "UNIX schemas"));
        assert!(manifest["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|endpoint| endpoint == "UNIX config.status"));

        let schemas = unix_result(
            handle_unix_request(&state, unix_request("schemas", serde_json::json!({}))).await,
        );
        assert_eq!(schemas["schema_version"], SCHEMA_VERSION);
        assert!(schemas["schemas"].get("RuntimeInventory").is_some());

        let config = unix_result(
            handle_unix_request(&state, unix_request("config.status", serde_json::json!({}))).await,
        );
        assert_eq!(config["schema_version"], SCHEMA_VERSION);
        assert_eq!(config["profile_token_present"], false);
        assert!(config["profile_root"]
            .as_str()
            .unwrap()
            .contains("unix-methods"));
        assert!(config.get("profile_token").is_none());

        let displays = unix_result(
            handle_unix_request(
                &state,
                unix_request("observe.displays", serde_json::json!({})),
            )
            .await,
        );
        assert!(displays.as_array().is_some());

        let cursor = unix_result(
            handle_unix_request(
                &state,
                unix_request("observe.cursor", serde_json::json!({})),
            )
            .await,
        );
        assert!(cursor.get("x").is_some());
        assert!(cursor.get("y").is_some());
        assert!(cursor.get("visible").is_some());

        let owner = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "session.acquire",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "session_id": "methods-owner",
                        "client_name": "methods test",
                        "role": "owner"
                    }),
                ),
            )
            .await,
        );
        assert_eq!(owner["owner_session_id"], "methods-owner");

        let pause = unix_result(
            handle_unix_request(
                &state,
                unix_request_with_session("control.pause", serde_json::json!({}), "methods-owner"),
            )
            .await,
        );
        assert_eq!(pause["safety_state"], "paused");

        let step = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "ui.step",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "label": "checking target state",
                        "source": "planner",
                        "step_index": 1,
                        "step_total": 3,
                        "ttl_ms": 1500
                    }),
                ),
            )
            .await,
        );
        assert_eq!(step["accepted"], true);
        assert_eq!(step["label"], "checking target state");
        assert_eq!(step["step_index"], 1);
        assert_eq!(step["step_total"], 3);

        let mode = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "ui.mode",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "mode": "headless",
                        "source": "unix proof"
                    }),
                ),
            )
            .await,
        );
        assert_eq!(mode["accepted"], true);
        assert_eq!(mode["mode"], "headless");
        assert_eq!(mode["source"], "unix proof");

        let profile = unix_result(
            handle_unix_request(
                &state,
                unix_request_with_session(
                    "profile.create",
                    serde_json::json!({
                        "name": "voice",
                        "mode": "supervised",
                        "duration_ms": 60000
                    }),
                    "methods-owner",
                ),
            )
            .await,
        );
        assert_eq!(profile["active_profile"]["name"], "voice");

        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = unix_result(
            handle_unix_request(
                &state,
                unix_request("events.snapshot", serde_json::json!({})),
            )
            .await,
        );
        let event_kinds = events
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|event| event["kind"].as_str())
            .collect::<Vec<_>>();
        assert!(event_kinds.contains(&"ui_step"));
        assert!(event_kinds.contains(&"ui_mode"));
        assert!(events.as_array().unwrap().len() >= 5);

        let last_sequence = events
            .as_array()
            .unwrap()
            .last()
            .and_then(|event| event["sequence"].as_u64())
            .unwrap();
        let empty_after = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "events.after",
                    serde_json::json!({ "after_sequence": last_sequence }),
                ),
            )
            .await,
        );
        assert_eq!(empty_after.as_array().unwrap().len(), 0);

        let empty_wait = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "events.wait",
                    serde_json::json!({
                        "after_sequence": last_sequence,
                        "timeout_ms": 25
                    }),
                ),
            )
            .await,
        );
        assert_eq!(empty_wait.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn inbox_publish_deduplicates_and_advances_by_sequence() {
        let state = DaemonState::synthetic("inbox-methods", "token");
        let request = serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "idempotency_key": "same-message",
            "source": "test",
            "text": "open notes",
            "payload": {"kind": "smoke"}
        });

        let first = unix_result(
            handle_unix_request(&state, unix_request("inbox.publish", request.clone())).await,
        );
        let duplicate =
            unix_result(handle_unix_request(&state, unix_request("inbox.publish", request)).await);
        let after_zero = unix_result(
            handle_unix_request(
                &state,
                unix_request("inbox.after", serde_json::json!({ "after_sequence": 0 })),
            )
            .await,
        );
        let after_first = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "inbox.after",
                    serde_json::json!({ "after_sequence": first["message"]["sequence"] }),
                ),
            )
            .await,
        );

        assert_eq!(first["state"], "accepted");
        assert_eq!(first["message"]["sequence"], 1);
        assert_eq!(duplicate["state"], "duplicate");
        assert_eq!(duplicate["message_id"], first["message_id"]);
        assert_eq!(after_zero.as_array().unwrap().len(), 1);
        assert!(after_first.as_array().unwrap().is_empty());
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(state
            .events
            .snapshot()
            .await
            .iter()
            .any(
                |event| event["kind"] == "inbound_message" && event["data"]["text"] == "open notes"
            ));
    }

    #[tokio::test]
    async fn inbox_status_transitions_are_pollable() {
        let state = DaemonState::synthetic("inbox-status", "token");
        let published = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "inbox.publish",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "idempotency_key": "status-message",
                        "source": "test",
                        "text": "what do you see"
                    }),
                ),
            )
            .await,
        );
        let message_id = published["message_id"].as_str().unwrap();

        let running = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "inbox.running",
                    serde_json::json!({ "message_id": message_id }),
                ),
            )
            .await,
        );
        let done = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "inbox.done",
                    serde_json::json!({ "message_id": message_id, "reply": "complete" }),
                ),
            )
            .await,
        );
        let status = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "inbox.status",
                    serde_json::json!({ "message_id": message_id }),
                ),
            )
            .await,
        );

        assert_eq!(running["state"], "running");
        assert_eq!(done["state"], "done");
        assert_eq!(status["reply"], "complete");
    }

    #[tokio::test]
    async fn inbox_done_preserves_multiline_reply_text() {
        let state = DaemonState::synthetic("inbox-status-multiline", "token");
        let published = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "inbox.publish",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "idempotency_key": "status-message-multiline",
                        "source": "test",
                        "text": "read output"
                    }),
                ),
            )
            .await,
        );
        let message_id = published["message_id"].as_str().unwrap();

        let done = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "inbox.done",
                    serde_json::json!({
                        "message_id": message_id,
                        "reply": "ALPHA\nBETA\nGAMMA"
                    }),
                ),
            )
            .await,
        );

        assert_eq!(done["state"], "done");
        assert_eq!(done["reply"], "ALPHA\nBETA\nGAMMA");
    }

    #[test]
    fn webhook_signature_accepts_sha256_hmac_header() {
        let body =
            br#"{"schema_version":"cua.v1","idempotency_key":"x","source":"test","text":"hi"}"#;
        let mut mac = HmacSha256::new_from_slice(b"secret").unwrap();
        mac.update(body);
        let signature = format!("sha256={}", bytes_to_hex(&mac.finalize().into_bytes()));
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-cua-webhook-signature",
            HeaderValue::from_str(&signature).unwrap(),
        );

        assert!(verify_webhook_signature("secret", &headers, body).is_ok());
        assert!(verify_webhook_signature("wrong", &headers, body).is_err());
    }

    #[tokio::test]
    async fn unix_owner_lease_blocks_observer_and_anonymous_writes() {
        let state = DaemonState::synthetic("unix-lease", "token");

        let owner = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "session.acquire",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "session_id": "owner-1",
                        "client_name": "owner test",
                        "role": "owner",
                        "ttl_ms": 60000
                    }),
                ),
            )
            .await,
        );
        assert_eq!(owner["accepted"], true);
        assert_eq!(owner["owner_session_id"], "owner-1");

        let observer = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "session.acquire",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "session_id": "observer-1",
                        "client_name": "observer test",
                        "role": "observer"
                    }),
                ),
            )
            .await,
        );
        assert_eq!(observer["session"]["role"], "observer");

        let anonymous =
            handle_unix_request(&state, unix_request("control.pause", serde_json::json!({}))).await;
        assert_eq!(anonymous["ok"], false);
        assert_eq!(anonymous["error"]["code"], "session_owner");

        let observer_write = handle_unix_request(
            &state,
            unix_request_with_session("control.pause", serde_json::json!({}), "observer-1"),
        )
        .await;
        assert_eq!(observer_write["ok"], false);
        assert_eq!(observer_write["error"]["code"], "session_owner");

        let owner_write = unix_result(
            handle_unix_request(
                &state,
                unix_request_with_session("control.pause", serde_json::json!({}), "owner-1"),
            )
            .await,
        );
        assert_eq!(owner_write["safety_state"], "paused");

        let inventory = unix_result(
            handle_unix_request(
                &state,
                unix_request("session.status", serde_json::json!({})),
            )
            .await,
        );
        assert_eq!(inventory["owner_session_id"], "owner-1");
        assert_eq!(inventory["connected_clients"], 2);
    }

    #[tokio::test]
    async fn unix_writes_require_owner_before_any_owner_exists() {
        let state = DaemonState::synthetic("unix-no-owner", "token");

        let anonymous =
            handle_unix_request(&state, unix_request("control.pause", serde_json::json!({}))).await;
        assert_eq!(anonymous["ok"], false);
        assert_eq!(anonymous["error"]["code"], "session_owner");
        assert_eq!(
            anonymous["error"]["message"],
            "write requires an active owner session"
        );
    }

    #[tokio::test]
    async fn unix_owner_heartbeat_renews_lease() {
        let state = DaemonState::synthetic("unix-heartbeat", "token");

        let owner = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "session.acquire",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "session_id": "owner-renew",
                        "client_name": "owner heartbeat test",
                        "role": "owner",
                        "ttl_ms": 1000
                    }),
                ),
            )
            .await,
        );
        let first_expiry = owner["session"]["expires_wall_ms"].as_i64().unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;
        let renewed = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "session.heartbeat",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "session_id": "owner-renew",
                        "ttl_ms": 60000
                    }),
                ),
            )
            .await,
        );
        let renewed_expiry = renewed["session"]["expires_wall_ms"].as_i64().unwrap();
        assert!(renewed_expiry > first_expiry);

        let owner_write = unix_result(
            handle_unix_request(
                &state,
                unix_request_with_session("control.pause", serde_json::json!({}), "owner-renew"),
            )
            .await,
        );
        assert_eq!(owner_write["safety_state"], "paused");
    }

    #[tokio::test]
    async fn unix_expired_owner_lease_refuses_writes() {
        let state = DaemonState::synthetic("unix-expired-owner", "token");

        let owner = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "session.acquire",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "session_id": "owner-expired",
                        "client_name": "owner expiry test",
                        "role": "owner",
                        "ttl_ms": 1000
                    }),
                ),
            )
            .await,
        );
        assert_eq!(owner["owner_session_id"], "owner-expired");

        tokio::time::sleep(Duration::from_millis(1100)).await;
        let expired_write = handle_unix_request(
            &state,
            unix_request_with_session("control.pause", serde_json::json!({}), "owner-expired"),
        )
        .await;
        assert_eq!(expired_write["ok"], false);
        assert_eq!(expired_write["error"]["code"], "session_owner");
        assert_eq!(
            expired_write["error"]["message"],
            "write requires an active owner session"
        );

        let heartbeat = handle_unix_request(
            &state,
            unix_request(
                "session.heartbeat",
                serde_json::json!({
                    "schema_version": SCHEMA_VERSION,
                    "session_id": "owner-expired",
                    "ttl_ms": 60000
                }),
            ),
        )
        .await;
        assert_eq!(heartbeat["ok"], false);
        assert_eq!(heartbeat["error"]["code"], "session_lease");
    }

    #[tokio::test]
    async fn http_writes_require_owner_session_header() {
        let state = DaemonState::synthetic("http-owner", "token");
        let missing = state.sessions.authorize_required_owner(None).unwrap_err();
        assert_eq!(missing.0.code, "session_owner_required");

        state
            .sessions
            .acquire(SessionLeaseRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                session_id: "http-owner-1".to_string(),
                client_name: "http owner test".to_string(),
                role: RuntimeSessionRole::Owner,
                ttl_ms: Some(60_000),
            })
            .unwrap();
        state
            .sessions
            .acquire(SessionLeaseRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                session_id: "http-observer-1".to_string(),
                client_name: "http observer test".to_string(),
                role: RuntimeSessionRole::Observer,
                ttl_ms: Some(60_000),
            })
            .unwrap();

        let observer = state
            .sessions
            .authorize_required_owner(Some("http-observer-1"))
            .unwrap_err();
        assert_eq!(observer.0.code, "session_owner");

        state
            .sessions
            .authorize_required_owner(Some("http-owner-1"))
            .unwrap();
    }

    #[tokio::test]
    async fn live_profile_socket_is_not_treated_as_stale() {
        let dir = PathBuf::from(format!("/tmp/cua-daemon-{}", Uuid::new_v4().simple()));
        let socket = dir.join("daemon.sock");
        tokio::fs::create_dir_all(socket.parent().unwrap())
            .await
            .unwrap();
        let listener = UnixListener::bind(&socket).unwrap();

        assert!(profile_socket_is_live(&socket).await);

        drop(listener);
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn unix_scratchpads_are_profile_scoped_owner_gated_and_pruned() {
        let profile = format!("scratchpad-test-{}", Uuid::new_v4());
        let _ = tokio::fs::remove_dir_all(cua_core::profile_dir(&profile).unwrap()).await;
        let state = DaemonState::synthetic(profile.clone(), "token");

        let owner = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "session.acquire",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "session_id": "scratch-owner",
                        "client_name": "scratchpad test",
                        "role": "owner",
                        "ttl_ms": 60000
                    }),
                ),
            )
            .await,
        );
        assert_eq!(owner["accepted"], true);

        let unauthorized = handle_unix_request(
            &state,
            unix_request(
                "scratchpad.write",
                serde_json::json!({
                    "schema_version": SCHEMA_VERSION,
                    "name": "goal",
                    "text": "blocked"
                }),
            ),
        )
        .await;
        assert_eq!(unauthorized["ok"], false);
        assert_eq!(unauthorized["error"]["code"], "session_owner");

        let invalid = handle_unix_request(
            &state,
            unix_request_with_session(
                "scratchpad.write",
                serde_json::json!({
                    "schema_version": SCHEMA_VERSION,
                    "name": "../escape",
                    "text": "blocked"
                }),
                "scratch-owner",
            ),
        )
        .await;
        assert_eq!(invalid["ok"], false);
        assert_eq!(invalid["error"]["code"], "bad_request");

        let first = unix_result(
            handle_unix_request(
                &state,
                unix_request_with_session(
                    "scratchpad.write",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "name": "goal",
                        "text": "remember the desktop target",
                        "durable": true
                    }),
                    "scratch-owner",
                ),
            )
            .await,
        );
        assert_eq!(first["kind"], "durable");
        assert!(first["text"].as_str().unwrap().contains("desktop target"));

        let appended = unix_result(
            handle_unix_request(
                &state,
                unix_request_with_session(
                    "scratchpad.write",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "name": "goal",
                        "text": "verify with screenshot",
                        "durable": true,
                        "append": true
                    }),
                    "scratch-owner",
                ),
            )
            .await,
        );
        assert!(appended["text"]
            .as_str()
            .unwrap()
            .contains("desktop target"));
        assert!(appended["text"]
            .as_str()
            .unwrap()
            .contains("verify with screenshot"));

        let read = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "scratchpad.read",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "name": "goal"
                    }),
                ),
            )
            .await,
        );
        assert_eq!(read["profile"], profile);
        assert_eq!(read["name"], "goal");

        let _ephemeral = unix_result(
            handle_unix_request(
                &state,
                unix_request_with_session(
                    "scratchpad.write",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "name": "temp",
                        "text": "short lived",
                        "durable": false,
                        "ttl_ms": 1
                    }),
                    "scratch-owner",
                ),
            )
            .await,
        );
        tokio::time::sleep(Duration::from_millis(1_050)).await;
        let list = unix_result(
            handle_unix_request(
                &state,
                unix_request(
                    "scratchpad.list",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "include_durable": true,
                        "include_ephemeral": true
                    }),
                ),
            )
            .await,
        );
        let entries = list["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["name"], "goal");

        let deleted = unix_result(
            handle_unix_request(
                &state,
                unix_request_with_session(
                    "scratchpad.delete",
                    serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "name": "goal"
                    }),
                    "scratch-owner",
                ),
            )
            .await,
        );
        assert_eq!(deleted["deleted"], 1);

        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = unix_result(
            handle_unix_request(
                &state,
                unix_request("events.snapshot", serde_json::json!({})),
            )
            .await,
        );
        let event_kinds = events
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|event| event["kind"].as_str())
            .collect::<Vec<_>>();
        assert!(event_kinds.contains(&"scratchpad_written"));
        assert!(event_kinds.contains(&"scratchpad_deleted"));

        let _ = tokio::fs::remove_dir_all(cua_core::profile_dir(&profile).unwrap()).await;
    }

    #[test]
    fn unix_errors_use_canonical_api_error_body() {
        let response = unix_error(
            Some(serde_json::json!("request-1")),
            "busy",
            "capture backend timed out",
            Some(StatusCode::SERVICE_UNAVAILABLE),
        );
        let body: ApiErrorBody = serde_json::from_value(response["error"].clone())
            .expect("unix error should decode as ApiErrorBody");
        assert_eq!(body.schema_version, SCHEMA_VERSION);
        assert_eq!(body.code, "busy");
        assert_eq!(body.message, "capture backend timed out");
        assert_eq!(body.details["status"], "503");
    }

    #[test]
    fn backend_unavailable_errors_map_to_service_unavailable() {
        let ApiError(body, status) =
            ApiError::computer_backend(anyhow::anyhow!("native capture unavailable"));
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body.code, "busy");
        assert_eq!(body.message, "native capture unavailable");
    }

    fn unix_request(method: &str, params: serde_json::Value) -> UnixRequest {
        unix_request_with_optional_session(method, params, None)
    }

    fn unix_request_with_session(
        method: &str,
        params: serde_json::Value,
        session_id: &str,
    ) -> UnixRequest {
        unix_request_with_optional_session(method, params, Some(session_id.to_string()))
    }

    fn unix_request_with_optional_session(
        method: &str,
        params: serde_json::Value,
        session_id: Option<String>,
    ) -> UnixRequest {
        UnixRequest {
            id: Some(serde_json::json!("test")),
            token: Some("token".to_string()),
            session_id,
            method: method.to_string(),
            params: Some(params),
        }
    }

    fn unix_result(response: serde_json::Value) -> serde_json::Value {
        assert_eq!(response["ok"], true, "{response}");
        response["result"].clone()
    }

    #[test]
    fn event_lane_refuses_when_queue_is_full() {
        let (sender, _receiver) = mpsc::channel(1);
        sender
            .try_send(EventJob {
                event: serde_json::json!({ "kind": "held" }),
            })
            .unwrap();
        let lane = EventLane {
            sender,
            recent: Arc::new(RwLock::new(VecDeque::new())),
            notify: Arc::new(Notify::new()),
        };

        assert!(!lane.publish("overflow", serde_json::json!({})));
    }

    #[tokio::test]
    async fn event_lane_wait_after_wakes_for_new_events() {
        let lane = EventLane::spawn(4, 8);
        let baseline = monotonic_event_sequence();

        let waiter = {
            let lane = lane.clone();
            tokio::spawn(async move { lane.wait_after(baseline, Duration::from_millis(500)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(lane.publish("ui_step", serde_json::json!({ "label": "wake" })));

        let events = waiter.await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "ui_step");
        assert_eq!(events[0]["data"]["label"], "wake");
    }

    #[test]
    fn trace_lane_refuses_when_queue_is_full() {
        let (sender, _receiver) = mpsc::channel(1);
        sender
            .try_send(TraceJob::Record(TraceRecord::Marker {
                name: "held".to_string(),
                at_wall_ms: now_wall_ms(),
            }))
            .unwrap();
        let lane = TraceLane {
            sender,
            dir: PathBuf::from("trace-test"),
        };

        assert!(!lane.enqueue_record(TraceRecord::Marker {
            name: "overflow".to_string(),
            at_wall_ms: now_wall_ms(),
        }));
    }

    #[test]
    fn latency_critical_controls_skip_trace_screenshots() {
        assert!(!captures_trace_snapshots(&InputAction::Pause));
        assert!(!captures_trace_snapshots(&InputAction::Resume));
        assert!(!captures_trace_snapshots(&InputAction::KillSwitch));
        assert!(!captures_trace_snapshots(&InputAction::MouseMove {
            x: 10,
            y: 20,
            duration_ms: 0,
        }));
        assert!(!captures_trace_snapshots(&InputAction::Sequence {
            actions: vec![
                InputAction::OpenApp {
                    app_name: "Messages".to_string(),
                },
                InputAction::KeyPress {
                    combo: "cmd+n".to_string(),
                },
                InputAction::ShellExec {
                    command: "pwd".to_string(),
                    timeout_ms: 5_000,
                },
                InputAction::Aegis {
                    args: vec!["version".to_string()],
                    timeout_ms: 15_000,
                },
            ],
            inter_action_delay_ms: 120,
        }));
        assert!(captures_trace_snapshots(&InputAction::MouseClick {
            x: 10,
            y: 20,
            button: cua_core::MouseButton::Left,
            count: 1,
        }));
        assert!(captures_trace_snapshots(&InputAction::Sequence {
            actions: vec![
                InputAction::KeyPress {
                    combo: "cmd+l".to_string(),
                },
                InputAction::MouseClick {
                    x: 10,
                    y: 20,
                    button: cua_core::MouseButton::Left,
                    count: 1,
                },
            ],
            inter_action_delay_ms: 120,
        }));
    }

    #[test]
    fn trace_frame_artifacts_are_explicitly_opt_in() {
        assert!(!env_flag_enabled(None));
        assert!(!env_flag_enabled(Some("")));
        assert!(!env_flag_enabled(Some("0")));
        assert!(!env_flag_enabled(Some("false")));
        assert!(env_flag_enabled(Some("1")));
        assert!(env_flag_enabled(Some("true")));
        assert!(env_flag_enabled(Some("frames")));
        assert!(env_flag_enabled(Some(" artifacts ")));
    }

    #[test]
    fn bounded_worker_env_values_are_clamped_and_defaulted() {
        assert_eq!(parse_bounded_usize(None, 4, 1, 32), 4);
        assert_eq!(parse_bounded_usize(Some("bad"), 4, 1, 32), 4);
        assert_eq!(parse_bounded_usize(Some("0"), 4, 1, 32), 1);
        assert_eq!(parse_bounded_usize(Some("64"), 4, 1, 32), 32);
        assert_eq!(parse_bounded_usize(Some("8"), 4, 1, 32), 8);
    }

    #[test]
    fn platform_artifact_path_roots_cua_artifacts_under_macos() {
        assert_eq!(
            platform_artifact_path("artifacts/cua/trace-smoke"),
            PathBuf::from("artifacts/cua/macos/trace-smoke")
        );
        assert_eq!(
            platform_artifact_path("artifacts/cua/macos/trace-smoke"),
            PathBuf::from("artifacts/cua/macos/trace-smoke")
        );
        assert_eq!(
            platform_artifact_path("/tmp/cua-trace"),
            PathBuf::from("/tmp/cua-trace")
        );
    }

    #[tokio::test]
    async fn profile_and_control_changes_emit_events() {
        let state = DaemonState::synthetic("test", "token");
        let _ = profile_create_state(
            &state,
            ProfileCreateRequest {
                name: "events".to_string(),
                mode: RuntimeMode::Supervised,
                capabilities: None,
                duration_ms: Some(60_000),
            },
        )
        .await;
        let _ = profile_activate_state(&state).await;
        let _ = control_pause_state(&state).await;
        tokio::time::sleep(Duration::from_millis(20)).await;

        let kinds: Vec<String> = state
            .events
            .snapshot()
            .await
            .into_iter()
            .filter_map(|event| {
                event
                    .get("kind")
                    .and_then(|kind| kind.as_str())
                    .map(str::to_string)
            })
            .collect();

        assert!(kinds.iter().any(|kind| kind == "daemon_started"));
        assert!(kinds.iter().any(|kind| kind == "profile_created"));
        assert!(kinds.iter().any(|kind| kind == "profile_activated"));
        assert!(kinds.iter().any(|kind| kind == "control_paused"));
    }

    #[tokio::test]
    async fn ui_step_normalizes_and_emits_agent_visible_event() {
        let state = DaemonState::synthetic("test", "token");

        let Json(result) = ui_step(
            State(state.clone()),
            Json(UiStepRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                label: "  inspect   cursor target  ".to_string(),
                source: Some("agent planner".to_string()),
                task: Some("  debug   auth flow  ".to_string()),
                tool: Some("  browser   probe ".to_string()),
                step_index: Some(7),
                step_total: Some(3),
                ttl_ms: Some(125),
            }),
        )
        .await
        .unwrap();

        assert!(result.accepted);
        assert_eq!(result.label, "inspect cursor target");
        assert_eq!(result.task.as_deref(), Some("debug auth flow"));
        assert_eq!(result.tool.as_deref(), Some("browser probe"));
        assert_eq!(result.step_index, Some(3));
        assert_eq!(result.step_total, Some(3));
        assert_eq!(result.ttl_ms, Some(250));

        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = state.events.snapshot().await;
        let step = events
            .iter()
            .find(|event| event["kind"] == "ui_step")
            .expect("ui_step event");
        assert_eq!(step["data"]["label"], "inspect cursor target");
        assert_eq!(step["data"]["source"], "agent planner");
        assert_eq!(step["data"]["task"], "debug auth flow");
        assert_eq!(step["data"]["tool"], "browser probe");
        assert_eq!(step["data"]["step_index"], 3);
        assert_eq!(step["data"]["step_total"], 3);
    }

    #[tokio::test]
    async fn ui_step_preserves_large_declarative_step_totals() {
        let state = DaemonState::synthetic("test", "token");

        let Json(result) = ui_step(
            State(state.clone()),
            Json(UiStepRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                label: "validate long plan".to_string(),
                source: Some("agent".to_string()),
                task: Some("computer use".to_string()),
                tool: Some("unix".to_string()),
                step_index: Some(37),
                step_total: Some(120),
                ttl_ms: Some(1_000),
            }),
        )
        .await
        .unwrap();

        assert_eq!(result.step_index, Some(37));
        assert_eq!(result.step_total, Some(120));
        tokio::time::sleep(Duration::from_millis(20)).await;

        let events = state.events.snapshot().await;
        let step = events
            .iter()
            .find(|event| event["kind"] == "ui_step")
            .expect("ui_step event");
        assert_eq!(step["data"]["label"], "validate long plan");
        assert_eq!(step["data"]["step_index"], 37);
        assert_eq!(step["data"]["step_total"], 120);
    }

    #[tokio::test]
    async fn ui_reply_normalizes_and_emits_agent_visible_reply_event() {
        let state = DaemonState::synthetic("test", "token");

        let Json(result) = ui_reply(
            State(state.clone()),
            Json(UiReplyRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                text: "  Done   with   the target. ".to_string(),
                source: Some("external agent".to_string()),
                ttl_ms: Some(125),
            }),
        )
        .await
        .unwrap();

        assert!(result.accepted);
        assert_eq!(result.text, "Done with the target.");
        assert_eq!(result.source.as_deref(), Some("external agent"));
        assert_eq!(result.ttl_ms, Some(250));

        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = state.events.snapshot().await;
        let reply = events
            .iter()
            .find(|event| event["kind"] == "ui_reply")
            .expect("ui_reply event");
        assert_eq!(reply["data"]["text"], "Done with the target.");
        assert_eq!(reply["data"]["source"], "external agent");
        assert_eq!(reply["data"]["ttl_ms"], 250);
    }

    #[tokio::test]
    async fn ui_mode_emits_live_headless_headful_event() {
        let state = DaemonState::synthetic("test", "token");

        let Json(result) = ui_mode(
            State(state.clone()),
            Json(UiModeRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                mode: cua_core::UiMode::Headless,
                source: Some("  cli   ".to_string()),
            }),
        )
        .await
        .unwrap();

        assert!(result.accepted);
        assert_eq!(result.mode, cua_core::UiMode::Headless);
        assert_eq!(result.source.as_deref(), Some("cli"));

        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = state.events.snapshot().await;
        let mode = events
            .iter()
            .find(|event| event["kind"] == "ui_mode")
            .expect("ui_mode event");
        assert_eq!(mode["data"]["mode"], "headless");
        assert_eq!(mode["data"]["source"], "cli");
    }

    #[tokio::test]
    async fn ui_island_emits_live_expansion_event() {
        let state = DaemonState::synthetic("test", "token");

        let Json(result) = ui_island(
            State(state.clone()),
            Json(UiIslandRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                state: cua_core::UiIslandState::Expanded,
                source: Some("  automation  ".to_string()),
            }),
        )
        .await
        .unwrap();

        assert!(result.accepted);
        assert_eq!(result.state, cua_core::UiIslandState::Expanded);
        assert_eq!(result.source.as_deref(), Some("automation"));

        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = state.events.snapshot().await;
        let island = events
            .iter()
            .find(|event| event["kind"] == "ui_island")
            .expect("ui_island event");
        assert_eq!(island["data"]["state"], "expanded");
        assert_eq!(island["data"]["source"], "automation");

        let Json(result) = ui_island(
            State(state.clone()),
            Json(UiIslandRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                state: cua_core::UiIslandState::Minimized,
                source: Some("automation".to_string()),
            }),
        )
        .await
        .unwrap();

        assert!(result.accepted);
        assert_eq!(result.state, cua_core::UiIslandState::Minimized);

        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = state.events.snapshot().await;
        let island = events
            .iter()
            .rev()
            .find(|event| event["kind"] == "ui_island")
            .expect("ui_island event");
        assert_eq!(island["data"]["state"], "minimized");
    }

    #[tokio::test]
    async fn ui_scene_emits_validated_live_scene_event() {
        let state = DaemonState::synthetic("test", "token");
        let scene = test_compact_scene(UiMode::Headless);

        let Json(result) = ui_scene(
            State(state.clone()),
            Json(UiSceneRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                scene: scene.clone(),
                source: Some(" automation ".to_string()),
            }),
        )
        .await
        .unwrap();

        assert!(result.accepted);
        assert_eq!(result.source.as_deref(), Some("automation"));
        assert_eq!(result.scene.as_ref(), Some(&scene));

        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = state.events.snapshot().await;
        let scene_event = events
            .iter()
            .find(|event| event["kind"] == "ui_scene")
            .expect("ui_scene event");
        assert_eq!(scene_event["data"]["source"], "automation");
        assert_eq!(
            scene_event["data"]["scene"]["schema_version"],
            cua_core::ISLAND_SCHEMA_VERSION
        );
        assert_eq!(scene_event["data"]["scene"]["mode"], "headless");
    }

    #[tokio::test]
    async fn ui_scene_rejects_invalid_scene_with_typed_error() {
        let state = DaemonState::synthetic("test", "token");
        let mut scene = test_compact_scene(UiMode::Headful);
        scene.regions.remove("center");

        let error = ui_scene(
            State(state),
            Json(UiSceneRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                scene,
                source: None,
            }),
        )
        .await
        .unwrap_err();

        let ApiError(body, _) = error;
        assert_eq!(body.code, "bad_request");
        assert_eq!(body.details["field"], "scene");
    }

    #[tokio::test]
    async fn ui_scene_reset_and_theme_emit_live_events() {
        let state = DaemonState::synthetic("test", "token");

        let Json(theme_result) = ui_scene_theme(
            State(state.clone()),
            Json(UiSceneThemeRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                theme: cua_core::IslandTheme {
                    name: "default".to_string(),
                    tokens: BTreeMap::from([("blue".to_string(), "#1e9bff".to_string())]),
                },
                source: Some("theme-test".to_string()),
            }),
        )
        .await
        .unwrap();
        assert!(theme_result.accepted);

        let Json(reset_result) = ui_scene_reset(
            State(state.clone()),
            Json(UiSceneResetRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                source: Some("reset-test".to_string()),
            }),
        )
        .await
        .unwrap();
        assert!(reset_result.accepted);

        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = state.events.snapshot().await;
        assert!(events.iter().any(|event| event["kind"] == "ui_scene_theme"));
        assert!(events.iter().any(|event| event["kind"] == "ui_scene_reset"));
    }

    #[tokio::test]
    async fn ui_scene_background_emits_validated_live_event() {
        let state = DaemonState::synthetic("test", "token");

        let Json(result) = ui_scene_background(
            State(state.clone()),
            Json(UiSceneBackgroundRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                background: cua_core::IslandBackground::NeonSweep {
                    base_color: "#000000".to_string(),
                    sweep_color: "#1e9bff".to_string(),
                    opacity: 88,
                    duration_ms: 1400,
                },
                source: Some("background-test".to_string()),
            }),
        )
        .await
        .unwrap();
        assert!(result.accepted);

        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = state.events.snapshot().await;
        let event = events
            .iter()
            .find(|event| event["kind"] == "ui_scene_background")
            .expect("ui_scene_background event");
        assert_eq!(event["data"]["source"], "background-test");
        assert_eq!(event["data"]["background"]["kind"], "neon_sweep");
    }

    #[tokio::test]
    async fn sequence_dispatch_emits_leaf_protocol_steps() {
        let state = accepting_test_state();

        let result = dispatch_input_action(
            &state,
            InputAction::Sequence {
                actions: vec![
                    InputAction::ShellExec {
                        command: "printf one".to_string(),
                        timeout_ms: 1_000,
                    },
                    InputAction::ShellExec {
                        command: "printf two".to_string(),
                        timeout_ms: 1_000,
                    },
                ],
                inter_action_delay_ms: 0,
            },
        )
        .await;

        assert_eq!(result.effect, Effect::Confirmed);
        assert_eq!(result.evidence.len(), 2);
        tokio::time::sleep(Duration::from_millis(20)).await;
        let events = state.events.snapshot().await;
        let labels = events
            .iter()
            .filter(|event| event["kind"] == "ui_step")
            .filter_map(|event| event["data"]["label"].as_str())
            .collect::<Vec<_>>();
        assert!(
            labels.contains(&"Preparing sequence 2 actions"),
            "{labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|label| label.starts_with("Dispatching 1/2 shell printf one")),
            "{labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|label| label.starts_with("Dispatching 2/2 shell printf two")),
            "{labels:?}"
        );
        assert!(
            labels.contains(&"Completed sequence 2 actions"),
            "{labels:?}"
        );
    }

    #[test]
    fn sequence_effect_aggregation_preserves_unverified_delivery() {
        assert_eq!(
            aggregate_sequence_effect(&Effect::Confirmed, &Effect::Unverifiable),
            Effect::Unverifiable
        );
        assert_eq!(
            aggregate_sequence_effect(&Effect::Unverifiable, &Effect::Confirmed),
            Effect::Unverifiable
        );
        assert_eq!(
            aggregate_sequence_effect(&Effect::Unverifiable, &Effect::Failed),
            Effect::Failed
        );
        assert_eq!(
            aggregate_sequence_effect(&Effect::Failed, &Effect::Refused),
            Effect::Refused
        );
    }

    #[test]
    fn sequence_metadata_preserves_homogeneous_paths_and_marks_mixed_paths() {
        assert_eq!(
            merge_sequence_route(None, &InputRoute::GlobalInput),
            Some(InputRoute::GlobalInput)
        );
        assert_eq!(
            merge_sequence_route(Some(InputRoute::GlobalInput), &InputRoute::GlobalInput),
            Some(InputRoute::GlobalInput)
        );
        assert_eq!(
            merge_sequence_route(Some(InputRoute::GlobalInput), &InputRoute::SystemApi),
            Some(InputRoute::SystemApi)
        );

        assert_eq!(
            merge_sequence_delivery_mode(None, &DeliveryMode::Desktop),
            Some(DeliveryMode::Desktop)
        );
        assert_eq!(
            merge_sequence_delivery_mode(Some(DeliveryMode::Desktop), &DeliveryMode::Desktop),
            Some(DeliveryMode::Desktop)
        );
        assert_eq!(
            merge_sequence_delivery_mode(Some(DeliveryMode::Desktop), &DeliveryMode::Background),
            Some(DeliveryMode::Unknown)
        );
    }

    #[test]
    fn ui_mode_events_select_hud_autostart_visibility() {
        assert_eq!(
            hud_mode_for_event("ui_mode", &serde_json::json!({ "mode": "headful" })),
            Some(cua_core::UiMode::Headful)
        );
        assert_eq!(
            hud_mode_for_event("ui_mode", &serde_json::json!({ "mode": "headless" })),
            Some(cua_core::UiMode::Headless)
        );
        assert_eq!(
            hud_mode_for_event(
                "visual_session_started",
                &serde_json::json!({ "mode": "headless" })
            ),
            None
        );
        assert_eq!(
            hud_mode_for_event(
                "ui_scene",
                &serde_json::json!({ "scene": { "mode": "headless" } })
            ),
            Some(cua_core::UiMode::Headless)
        );
    }

    #[tokio::test]
    async fn events_query_returns_only_events_after_sequence() {
        let state = DaemonState::synthetic("test", "token");
        tokio::time::sleep(Duration::from_millis(20)).await;
        let baseline = state.events.snapshot().await;
        let last_sequence = baseline
            .last()
            .and_then(|event| event["sequence"].as_u64())
            .unwrap();

        let Json(_) = ui_step(
            State(state.clone()),
            Json(UiStepRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                label: "new step only".to_string(),
                source: None,
                task: None,
                tool: None,
                step_index: None,
                step_total: None,
                ttl_ms: None,
            }),
        )
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        let Json(events) = events(
            State(state),
            Query(EventsQuery {
                after: Some(last_sequence),
                timeout_ms: None,
            }),
        )
        .await;

        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["kind"], "ui_step");
        assert_eq!(events[0]["data"]["label"], "new step only");
    }

    #[tokio::test]
    async fn dispatched_control_actions_mutate_runtime_state() {
        let state = DaemonState::synthetic("test", "token");

        let pause = dispatch_input_action(&state, InputAction::Pause).await;
        assert_eq!(pause.effect, Effect::Confirmed);
        assert_eq!(pause.route, InputRoute::SystemApi);
        assert_eq!(state.control.read().await.safety_state, SafetyState::Paused);

        let resume = dispatch_input_action(&state, InputAction::Resume).await;
        assert_eq!(resume.effect, Effect::Confirmed);
        assert_eq!(resume.route, InputRoute::SystemApi);
        assert_eq!(
            state.control.read().await.safety_state,
            SafetyState::Running
        );

        let kill = dispatch_input_action(&state, InputAction::KillSwitch).await;
        assert_eq!(kill.effect, Effect::Confirmed);
        assert_eq!(kill.route, InputRoute::SystemApi);
        assert_eq!(state.control.read().await.safety_state, SafetyState::Killed);
    }

    #[tokio::test]
    async fn dispatched_actions_emit_live_protocol_steps() {
        let state = DaemonState::synthetic("test", "token");

        let pause = dispatch_input_action(&state, InputAction::Pause).await;
        assert_eq!(pause.effect, Effect::Confirmed);
        tokio::time::sleep(Duration::from_millis(20)).await;

        let events = state.events.snapshot().await;
        let steps = events
            .iter()
            .filter(|event| event["kind"] == "ui_step")
            .collect::<Vec<_>>();

        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0]["data"]["step_index"], serde_json::Value::Null);
        assert_eq!(steps[0]["data"]["step_total"], serde_json::Value::Null);
        assert_eq!(steps[0]["data"]["label"], "Preparing pause control");
        assert_eq!(steps[1]["data"]["step_index"], serde_json::Value::Null);
        assert_eq!(steps[1]["data"]["step_total"], serde_json::Value::Null);
        assert_eq!(steps[1]["data"]["label"], "Dispatching pause control");
        assert_eq!(steps[2]["data"]["step_index"], serde_json::Value::Null);
        assert_eq!(steps[2]["data"]["step_total"], serde_json::Value::Null);
        assert_eq!(steps[2]["data"]["label"], "Completed pause control");
    }

    #[tokio::test]
    async fn active_programmed_step_suppresses_runtime_substeps() {
        let state = DaemonState::synthetic("test", "token");
        let Json(programmed) = ui_step(
            State(state.clone()),
            Json(UiStepRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                label: "step seven of a ten step demo".to_string(),
                source: Some("agent".to_string()),
                task: Some("TextEdit demo".to_string()),
                tool: Some("Unix socket".to_string()),
                step_index: Some(7),
                step_total: Some(10),
                ttl_ms: Some(5_000),
            }),
        )
        .await
        .unwrap();
        assert_eq!(programmed.step_index, Some(7));
        assert_eq!(programmed.step_total, Some(10));

        let pause = dispatch_input_action(&state, InputAction::Pause).await;
        assert_eq!(pause.effect, Effect::Confirmed);
        tokio::time::sleep(Duration::from_millis(20)).await;

        let events = state.events.snapshot().await;
        let step_labels = events
            .iter()
            .filter(|event| event["kind"] == "ui_step")
            .map(|event| event["data"]["label"].as_str().unwrap_or_default())
            .collect::<Vec<_>>();

        assert_eq!(step_labels, vec!["step seven of a ten step demo"]);
    }

    #[test]
    fn input_action_labels_include_live_action_details() {
        assert_eq!(
            input_action_label(&InputAction::MouseClick {
                x: 425,
                y: 405,
                button: cua_core::MouseButton::Left,
                count: 2,
            }),
            "Left mouse click at 425,405 x2"
        );
        assert_eq!(
            input_action_label(&InputAction::KeyPress {
                combo: "cmd+l".to_string()
            }),
            "key press cmd+l"
        );
        assert_eq!(
            input_action_label(&InputAction::KeyType {
                text: "hello".to_string()
            }),
            "typing 5 chars"
        );
        assert_eq!(
            input_action_label(&InputAction::ShellExec {
                command: "pwd && ls -la".to_string(),
                timeout_ms: 5_000,
            }),
            "shell pwd && ls -la"
        );
        assert_eq!(
            input_action_label(&InputAction::Aegis {
                args: vec![
                    "--mode".to_string(),
                    "headful".to_string(),
                    "page".to_string(),
                    "actions".to_string(),
                ],
                timeout_ms: 15_000,
            }),
            "aegis --mode headful page actions"
        );
    }

    async fn clipboard_enabled_state() -> DaemonState {
        let state = accepting_test_state();
        let _ = profile_create_state(
            &state,
            ProfileCreateRequest {
                name: "clip".to_string(),
                mode: RuntimeMode::Supervised,
                capabilities: Some(CapabilityManifest {
                    clipboard: true,
                    ..CapabilityManifest::default()
                }),
                duration_ms: Some(60_000),
            },
        )
        .await;
        let _ = profile_activate_state(&state).await;
        state
    }

    fn sample_mouse_request() -> InputRequest {
        InputRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            idempotency_key: Uuid::new_v4(),
            deadline_mono_ns: None,
            action: InputAction::MouseClick {
                x: 10,
                y: 10,
                button: cua_core::MouseButton::Left,
                count: 1,
            },
        }
    }

    fn sample_frame() -> CapturedFrame {
        let bytes = vec![1, 2, 3, 4];
        CapturedFrame {
            envelope: cua_core::FrameEnvelope {
                schema_version: SCHEMA_VERSION.to_string(),
                frame_id: 1,
                timestamp_mono_ns: 0,
                timestamp_wall_ms: now_wall_ms(),
                display_id: "test-display".to_string(),
                display_x: 0,
                display_y: 0,
                display_width: 1,
                display_height: 1,
                frame_origin_x: 0,
                frame_origin_y: 0,
                width: 1,
                height: 1,
                scale_factor: 1.0,
                pixel_format: "rgba8".to_string(),
                encoding: FrameEncoding::Png,
                byte_len: bytes.len(),
                sha256: "9f64a747e1b97f131fabb6b447296c9b6f0201e79fb3c5356e6c77e89b6a806a"
                    .to_string(),
                cursor: cua_core::CursorState {
                    x: 0.0,
                    y: 0.0,
                    visible: true,
                    included_in_frame: false,
                },
                damage_rects: Vec::new(),
            },
            bytes: Arc::new(bytes),
            timings: cua_capture::CapturedFrameTimings::default(),
        }
    }
}
