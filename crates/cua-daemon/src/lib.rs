use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocketUpgrade},
        Query, Request, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use chrono::Utc;
#[cfg(test)]
use cua_capture::SyntheticCaptureBackend;
use cua_capture::{
    encode_image, CaptureRequest, CaptureSource, CapturedFrame, CapturedFrameTimings, FrameBus,
    FrameLookup,
};
use cua_core::{
    now_wall_ms, schema_bundle, ApiErrorBody, CapabilityManifest, CapabilityState,
    ClipboardReadRequest, ClipboardResult, ClipboardWriteRequest, DeliveryMode,
    DesktopContextSnapshot, DesktopState, Effect, Evidence, EvidenceKind, FrameActionRequest,
    FrameEncoding, FramePayload, HealthReport, InputAction, InputRequest, InputResult, InputRoute,
    Manifest, MetricBucket, MetricHistogram, MetricsSnapshot, PermissionReport, ProfilePolicy,
    RuntimeControlState, RuntimeInventory, RuntimeMode, RuntimeSessionInfo, RuntimeSessionRole,
    SafetyState, SessionCancelRequest, SessionLeaseRequest, SessionLeaseResult, UiIslandRequest,
    UiIslandResult, UiMode, UiModeRequest, UiModeResult, UiReplyRequest, UiReplyResult,
    UiStepRequest, UiStepResult, VisualSessionRequest, WindowInfo, SCHEMA_VERSION,
};
use cua_input::InputBackend;
use cua_model::{run_eval_report, EvalConfig, EvalReport};
use cua_trace::{ActionTurnRecord, TraceRecord, TraceWriter};
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
use tokio::sync::{mpsc, oneshot, Notify, RwLock};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

#[derive(Clone)]
pub struct DaemonState {
    pub profile: String,
    pub started_at: chrono::DateTime<Utc>,
    pub frame_bus: Arc<FrameBus>,
    pub input: Arc<dyn InputBackend>,
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
}

#[derive(Debug, Clone)]
struct UiStepContext {
    expires_at: Option<Instant>,
}

impl DaemonState {
    pub fn synthetic(profile: impl Into<String>, bearer_token: impl Into<String>) -> Self {
        Self::synthetic_with_hud_mode(profile, bearer_token, UiMode::Headful)
    }

    pub fn synthetic_with_hud_mode(
        profile: impl Into<String>,
        bearer_token: impl Into<String>,
        hud_mode: UiMode,
    ) -> Self {
        let profile = profile.into();
        let input = cua_platform_macos::input_backend();
        let events = EventLane::spawn(event_lane_capacity(), event_lane_retention());
        let state = Self {
            profile: profile.clone(),
            started_at: Utc::now(),
            frame_bus: Arc::new(FrameBus::new(default_capture_backend())),
            encode_lane: EncodeLane::spawn(encode_lane_capacity()),
            input_lane: InputLane::spawn(input.clone(), input_lane_capacity()),
            model_lane: ModelLane::spawn(model_lane_capacity()),
            permission_lane: PermissionLane::spawn(permission_lane_capacity()),
            input,
            active_streams: Arc::new(AtomicU32::new(0)),
            bearer_token: Arc::new(bearer_token.into()),
            control: Arc::new(RwLock::new(default_control_state(&profile))),
            clipboard: Arc::new(RwLock::new(None)),
            metrics: Arc::new(RuntimeMetrics::default()),
            events,
            trace_lane: trace_dir_from_env()
                .and_then(|dir| TraceWriter::from_dir(dir).ok())
                .map(|writer| TraceLane::spawn(writer, trace_lane_capacity())),
            ui_step_context: Arc::new(StdMutex::new(None)),
            hud_supervisor: HudSupervisor::default(),
            hud_mode,
            http_addr: Arc::new(StdMutex::new(String::new())),
            profile_socket: Arc::new(StdMutex::new(String::new())),
            sessions: SessionRegistry::default(),
        };
        state.publish_event("daemon_started", serde_json::json!({}));
        state
    }

    pub async fn health(&self) -> HealthReport {
        let permissions = self.permission_report().await;
        let control = self.control.read().await;
        HealthReport {
            schema_version: SCHEMA_VERSION.to_string(),
            status: CapabilityState::Degraded,
            version: env!("CARGO_PKG_VERSION").to_string(),
            profile: self.profile.clone(),
            started_at: self.started_at,
            permissions,
            latest_frame: self.frame_bus.latest_envelope().await,
            safety_state: control.safety_state.clone(),
            active_profile: control.active_profile.name.clone(),
            active_streams: self.active_streams.load(Ordering::Relaxed),
            model_sessions: self.model_lane.active_count(),
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
            hud_pid: self.hud_supervisor.pid().or_else(discover_hud_pid),
            connected_clients: session_snapshot.sessions.len() as u32,
            owner_session_id: session_snapshot.owner_session_id,
            sessions: session_snapshot.sessions,
        }
    }
}

#[cfg(test)]
fn default_capture_backend() -> Arc<dyn cua_capture::CaptureBackend> {
    Arc::new(SyntheticCaptureBackend::default())
}

#[cfg(not(test))]
fn default_capture_backend() -> Arc<dyn cua_capture::CaptureBackend> {
    cua_platform_macos::capture_backend_or_unavailable()
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
            pid => Some(pid),
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
            return;
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
            return Ok(());
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
            | "input_started"
            | "input_completed"
            | "input_refused"
            | "control_paused"
            | "control_resumed"
            | "kill_switch"
            | "visual_session_started"
    )
}

fn hud_mode_for_event(kind: &str, data: &serde_json::Value) -> Option<UiMode> {
    if kind != "ui_mode" {
        return None;
    }
    match data.get("mode").and_then(|value| value.as_str()) {
        Some("headful") => Some(UiMode::Headful),
        Some("headless") => Some(UiMode::Headless),
        _ => None,
    }
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
                        Effect::Refused,
                        InputRoute::Unavailable,
                        DeliveryMode::NotApplicable,
                        EvidenceKind::Refusal,
                        "input lane worker stopped",
                    ),
                ),
            },
            Err(mpsc::error::TrySendError::Full(job)) => (
                job.enqueued_at.elapsed(),
                input_result_with_id(
                    idempotency_key,
                    Effect::Refused,
                    InputRoute::Unavailable,
                    DeliveryMode::NotApplicable,
                    EvidenceKind::Refusal,
                    "input lane queue is full",
                ),
            ),
            Err(mpsc::error::TrySendError::Closed(job)) => (
                job.enqueued_at.elapsed(),
                input_result_with_id(
                    idempotency_key,
                    Effect::Refused,
                    InputRoute::Unavailable,
                    DeliveryMode::NotApplicable,
                    EvidenceKind::Refusal,
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
        let worker_active = active.clone();
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
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
    fn spawn(capacity: usize) -> Self {
        let (sender, mut receiver) = mpsc::channel::<PermissionJob>(capacity);
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                let queue_wait = job.enqueued_at.elapsed();
                let probe_started = Instant::now();
                let report = cua_platform_macos::permission_report();
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
        let (sender, mut receiver) = mpsc::channel::<TraceJob>(capacity);
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                match job {
                    TraceJob::Artifact {
                        relative_path,
                        bytes,
                    } => {
                        let _ = writer.write_artifact(relative_path, bytes.as_ref()).await;
                    }
                    TraceJob::Record(record) => {
                        let _ = writer.append(&record).await;
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

fn trace_lane_capacity() -> usize {
    std::env::var("CUA_TRACE_QUEUE_CAPACITY")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(256)
}

fn trace_dir_from_env() -> Option<PathBuf> {
    std::env::var_os("CUA_TRACE_DIR").map(platform_artifact_path)
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
    Router::new()
        .route("/", get(root))
        .route("/manifest", get(manifest))
        .route("/schemas", get(schemas))
        .route("/version", get(version))
        .route("/status", get(status))
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
        .route("/session/cancel", post(session_cancel))
        .route("/session/status", get(session_status))
        .route("/ui/step", post(ui_step))
        .route("/ui/reply", post(ui_reply))
        .route("/ui/mode", post(ui_mode))
        .route("/ui/island", post(ui_island))
        .route("/profile/create", post(profile_create))
        .route("/profile/activate", post(profile_activate))
        .route("/profile/status", get(profile_status))
        .route("/control/pause", post(control_pause))
        .route("/control/resume", post(control_resume))
        .route("/control/kill-switch", post(control_kill_switch))
        .route("/input/mouse", post(input_action))
        .route("/input/keyboard", post(input_action))
        .route("/input/clipboard", post(input_action))
        .route("/input/frame", post(input_frame_action))
        .route("/clipboard/read", post(clipboard_read))
        .route("/clipboard/write", post(clipboard_write))
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
    let state = DaemonState::synthetic_with_hud_mode(profile, token, hud_mode);
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
    let token = format!("cua-{}", Uuid::new_v4());
    tokio::fs::write(&path, format!("{token}\n")).await?;
    Ok(token)
}

fn profile_token_path(profile: &str) -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home)
        .join(".cua")
        .join("profiles")
        .join(profile)
        .join("http.token"))
}

pub fn profile_socket_path(profile: &str) -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home)
        .join(".cua")
        .join("profiles")
        .join(profile)
        .join("daemon.sock"))
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
    loop {
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
            _ = interval.tick() => {
                let started = Instant::now();
                let message = match state
                    .frame_bus
                    .latest_or_capture_timed(CaptureRequest {
                        max_width: visual.max_width.or(Some(1280)),
                        encoding: FrameEncoding::Jpeg,
                        force_fresh: false,
                    })
                    .await
                {
                    Ok(lookup) => {
                        observe_frame_lookup(&state.metrics, &lookup);
                        state.metrics.observe(MetricKind::StreamUnixTick, started.elapsed());
                        state.metrics.increment(CounterKind::UnixFrames);
                        if !announced_first_frame {
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
                        VisualSessionMessage::Frame {
                            schema_version: SCHEMA_VERSION.to_string(),
                            frame: lookup.frame.as_payload(visual.include_bytes),
                        }
                    }
                    Err(error) => VisualSessionMessage::Error {
                        schema_version: SCHEMA_VERSION.to_string(),
                        error: error.to_string(),
                    },
                };
                write_json_line(&mut write, &message).await?;
            }
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum VisualSessionMessage {
    Started {
        schema_version: String,
        fps: u32,
    },
    Frame {
        schema_version: String,
        frame: FramePayload,
    },
    Error {
        schema_version: String,
        error: String,
    },
    Closed {
        schema_version: String,
    },
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
        "session.status" => Ok(serde_json::to_value(state.runtime_inventory().await)),
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
        "observe.desktop" => desktop_state(state).await.map(serde_json::to_value),
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
    serde_json::json!({
        "id": id,
        "ok": false,
        "error": {
            "schema_version": SCHEMA_VERSION,
            "code": code.into(),
            "message": message.into(),
            "status": status.map(|status| status.as_u16())
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
            "POST /session/cancel".to_string(),
            "GET /session/status".to_string(),
            "UNIX session.acquire".to_string(),
            "UNIX session.cancel".to_string(),
            "UNIX session.status".to_string(),
            "POST /ui/step".to_string(),
            "POST /ui/reply".to_string(),
            "POST /ui/mode".to_string(),
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
            "cua manifest --json".to_string(),
            "cua metrics --json".to_string(),
            "cua events --json [--after <sequence>]".to_string(),
            "cua permissions request-accessibility --json".to_string(),
            "cua session acquire <session-id> --role owner|observer --json".to_string(),
            "cua session cancel <session-id> --json".to_string(),
            "cua session status --json".to_string(),
            "cua stream --unix --json".to_string(),
            "cua ui step <label> --step-index <n> --step-total <n> --json".to_string(),
            "cua ui reply <text> --json".to_string(),
            "cua ui mode headless|headful --json".to_string(),
            "cua perf live --json".to_string(),
            "cua screenshot --out <path>".to_string(),
            "cua window-capture <window-id> --out <path>".to_string(),
            "cua context --json".to_string(),
            "cua observe --json".to_string(),
            "cua profile status --json".to_string(),
            "cua clipboard read --allow-sensitive --json".to_string(),
            "cua clipboard write <text> --json".to_string(),
            "cua pause --json".to_string(),
            "cua resume --json".to_string(),
            "cua kill-switch --json".to_string(),
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
    let requested = cua_platform_macos::request_accessibility_input_access();
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
    let window = cua_platform_macos::window_list()
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
    let encoded = state
        .encode_lane
        .payload(frame, request.include_bytes.unwrap_or(true))
        .await
        .map_err(|error| {
            state.metrics.increment(CounterKind::EncodeDrops);
            ApiError::busy(format!("encode lane unavailable: {error:?}"))
        })?;
    observe_encode_result(&state.metrics, &encoded);
    Ok(encoded.value)
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
                    .map_err(ApiError::internal)?,
                cache_hit: true,
                wait_ns: started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            },
            None => state
                .frame_bus
                .latest_or_capture_timed(capture_request)
                .await
                .map_err(ApiError::internal)?,
        }
    } else {
        state
            .frame_bus
            .latest_or_capture_timed(capture_request)
            .await
            .map_err(ApiError::internal)?
    };
    observe_frame_lookup(&state.metrics, &lookup);
    state
        .metrics
        .observe(MetricKind::CaptureScreenshot, started.elapsed());
    let encoded = state
        .encode_lane
        .payload(lookup.frame, request.include_bytes.unwrap_or(true))
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
    let permissions = state.permission_report().await;
    let displays = state
        .frame_bus
        .displays()
        .await
        .map_err(ApiError::internal)?;
    let latest_frame = state.frame_bus.latest_envelope().await;
    let cursor = cua_platform_macos::cursor_state();
    let windows = cua_platform_macos::window_list().map_err(ApiError::internal)?;
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
            .map_err(ApiError::internal)?,
    ))
}

async fn observe_cursor(State(state): State<DaemonState>) -> Json<cua_core::CursorState> {
    drop(state);
    Json(cua_platform_macos::cursor_state())
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
) -> Json<RuntimeControlState> {
    Json(profile_create_state(&state, request).await)
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

async fn profile_activate(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
    Json(profile_activate_state(&state).await)
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

async fn control_pause(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
    Json(control_pause_state(&state).await)
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

async fn control_resume(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
    Json(control_resume_state(&state).await)
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

async fn control_kill_switch(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
    Json(control_kill_switch_state(&state).await)
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
) -> Json<cua_core::InputResult> {
    Json(dispatch_input_action(&state, action).await)
}

async fn input_frame_action(
    State(state): State<DaemonState>,
    Json(request): Json<FrameActionRequest>,
) -> Json<cua_core::InputResult> {
    Json(dispatch_input_action(&state, request.into_display_action()).await)
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
    publish_protocol_step(
        state,
        1,
        3,
        format!("Preparing {action_label}"),
        "Unix socket",
        5_000,
    );
    let action_json = serde_json::to_value(&action).unwrap_or_else(|_| serde_json::json!(null));
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
    if matches!(
        action,
        InputAction::ClipboardRead { .. } | InputAction::ClipboardWrite { .. }
    ) {
        publish_protocol_step(
            state,
            3,
            3,
            format!("Refused {action_label}"),
            "Unix socket",
            3_500,
        );
        state.metrics.increment(CounterKind::InputRefusals);
        state
            .metrics
            .observe(MetricKind::InputDispatch, started.elapsed());
        let result = refused_input_result(
            "clipboard actions must use /clipboard/read or /clipboard/write for explicit grants",
        );
        let after = if capture_trace_snapshots {
            trace_snapshot(state, &turn_id, "after").await
        } else {
            None
        };
        append_action_turn(
            state,
            turn_id,
            action_json,
            serde_json::to_value(&result).unwrap_or_else(|_| serde_json::json!(null)),
            before,
            after,
        )
        .await;
        publish_input_event(state, "input_refused", &result);
        return result;
    }
    let dispatch_started = Instant::now();
    publish_protocol_step(
        state,
        2,
        3,
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
        serde_json::to_value(&result).unwrap_or_else(|_| serde_json::json!(null)),
        before,
        after,
    )
    .await;
    let final_prefix = if result.effect == Effect::Refused {
        "Refused"
    } else {
        "Confirmed"
    };
    publish_protocol_step(
        state,
        3,
        3,
        format!("{final_prefix} {action_label}"),
        "Unix socket",
        3_500,
    );
    publish_input_event(state, "input_completed", &result);
    result
}

async fn dispatch_control_action(
    state: &DaemonState,
    action: InputAction,
    turn_id: String,
    action_json: serde_json::Value,
    started: Instant,
) -> cua_core::InputResult {
    let action_label = input_action_label(&action);
    publish_protocol_step(
        state,
        2,
        3,
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
        serde_json::to_value(&result).unwrap_or_else(|_| serde_json::json!(null)),
        None,
        None,
    )
    .await;
    publish_protocol_step(
        state,
        3,
        3,
        format!("Confirmed {action_label}"),
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
    let action_json = serde_json::to_value(&request).unwrap_or_else(|_| serde_json::json!(null));
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
    let text = state.clipboard.read().await.clone();
    state
        .metrics
        .observe(MetricKind::ClipboardRead, started.elapsed());
    let result = ClipboardResult {
        schema_version: SCHEMA_VERSION.to_string(),
        action: action.to_string(),
        result: input_result(
            Effect::Confirmed,
            InputRoute::SystemApi,
            DeliveryMode::NotApplicable,
            EvidenceKind::ValueReadback,
            "clipboard value returned from daemon-owned clipboard store",
        ),
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
) -> Json<ClipboardResult> {
    Json(clipboard_write_state(&state, request).await)
}

async fn clipboard_write_state(
    state: &DaemonState,
    request: ClipboardWriteRequest,
) -> ClipboardResult {
    let started = Instant::now();
    let action = "clipboard_write";
    let turn_id = Uuid::new_v4().to_string();
    let action_json = serde_json::to_value(&request).unwrap_or_else(|_| serde_json::json!(null));
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
    *state.clipboard.write().await = Some(request.text);
    state
        .metrics
        .observe(MetricKind::ClipboardWrite, started.elapsed());
    let result = ClipboardResult {
        schema_version: SCHEMA_VERSION.to_string(),
        action: action.to_string(),
        result: input_result(
            Effect::Confirmed,
            InputRoute::SystemApi,
            DeliveryMode::NotApplicable,
            EvidenceKind::ValueReadback,
            "clipboard value written to daemon-owned clipboard store",
        ),
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
        serde_json::to_value(result).unwrap_or_else(|_| serde_json::json!(null)),
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
}

impl CounterKind {
    const ALL: [Self; 13] = [
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
        let Json(result) = clipboard_write(
            State(state),
            Json(ClipboardWriteRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                text: "denied".to_string(),
            }),
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
        let Json(result) = clipboard_write(
            State(state.clone()),
            Json(ClipboardWriteRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                text: "overwrite".to_string(),
            }),
        )
        .await;

        assert_eq!(result.result.effect, Effect::Refused);
        assert_eq!(state.clipboard.read().await.as_deref(), Some("original"));
    }

    #[tokio::test]
    async fn refused_input_clipboard_action_does_not_mutate_clipboard() {
        let state = DaemonState::synthetic("test", "token");
        *state.clipboard.write().await = Some("original".to_string());
        let Json(result) = input_action(
            State(state.clone()),
            Json(InputAction::ClipboardWrite {
                text: "overwrite".to_string(),
            }),
        )
        .await;

        assert_eq!(result.effect, Effect::Refused);
        assert_eq!(state.clipboard.read().await.as_deref(), Some("original"));
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
        let Json(write_result) = clipboard_write(
            State(state.clone()),
            Json(ClipboardWriteRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                text: "hello from test".to_string(),
            }),
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
        assert_eq!(read_result.text.as_deref(), Some("hello from test"));
    }

    #[tokio::test]
    async fn input_lane_refuses_when_queue_is_full() {
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

        assert_eq!(result.effect, Effect::Refused);
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

        let manifest = unix_result(
            handle_unix_request(&state, unix_request("manifest", serde_json::json!({}))).await,
        );
        assert!(manifest["public_surfaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|surface| surface == "local_unix_socket"));

        let pause = unix_result(
            handle_unix_request(&state, unix_request("control.pause", serde_json::json!({}))).await,
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
                unix_request(
                    "profile.create",
                    serde_json::json!({
                        "name": "voice",
                        "mode": "supervised",
                        "duration_ms": 60000
                    }),
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
        let Json(_) = profile_create(
            State(state.clone()),
            Json(ProfileCreateRequest {
                name: "events".to_string(),
                mode: RuntimeMode::Supervised,
                capabilities: None,
                duration_ms: Some(60_000),
            }),
        )
        .await;
        let Json(_) = profile_activate(State(state.clone())).await;
        let Json(_) = control_pause(State(state.clone())).await;
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
        assert_eq!(steps[0]["data"]["step_index"], 1);
        assert_eq!(steps[0]["data"]["step_total"], 3);
        assert_eq!(steps[0]["data"]["label"], "Preparing pause control");
        assert_eq!(steps[1]["data"]["step_index"], 2);
        assert_eq!(steps[1]["data"]["label"], "Dispatching pause control");
        assert_eq!(steps[2]["data"]["step_index"], 3);
        assert_eq!(steps[2]["data"]["label"], "Confirmed pause control");
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
        let state = DaemonState::synthetic("test", "token");
        let Json(_) = profile_create(
            State(state.clone()),
            Json(ProfileCreateRequest {
                name: "clip".to_string(),
                mode: RuntimeMode::Supervised,
                capabilities: Some(CapabilityManifest {
                    clipboard: true,
                    ..CapabilityManifest::default()
                }),
                duration_ms: Some(60_000),
            }),
        )
        .await;
        let Json(_) = profile_activate(State(state.clone())).await;
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
