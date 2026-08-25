use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocketUpgrade},
        Request, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use bytes::Bytes;
use chrono::Utc;
use cua_capture::{CaptureRequest, FrameBus, FrameLookup};
use cua_core::{
    now_wall_ms, schema_bundle, ApiErrorBody, CapabilityManifest, CapabilityState,
    ClipboardReadRequest, ClipboardResult, ClipboardWriteRequest, DeliveryMode, DesktopState,
    Effect, Evidence, EvidenceKind, FrameEncoding, HealthReport, InputAction, InputRequest,
    InputResult, InputRoute, Manifest, MetricBucket, MetricHistogram, MetricsSnapshot,
    ProfilePolicy, RuntimeControlState, RuntimeMode, SafetyState, SCHEMA_VERSION,
};
use cua_input::InputBackend;
use cua_model::{run_eval_report, EvalConfig, EvalReport};
use cua_trace::{ActionTurnRecord, TraceRecord, TraceWriter};
use serde::Deserialize;
use std::collections::{BTreeMap, VecDeque};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU32, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, RwLock};
use tower_http::trace::TraceLayer;
use uuid::Uuid;

#[derive(Clone)]
pub struct DaemonState {
    pub profile: String,
    pub started_at: chrono::DateTime<Utc>,
    pub frame_bus: Arc<FrameBus>,
    pub input: Arc<dyn InputBackend>,
    input_lane: InputLane,
    pub active_streams: Arc<AtomicU32>,
    pub bearer_token: Arc<String>,
    pub control: Arc<RwLock<RuntimeControlState>>,
    pub clipboard: Arc<RwLock<Option<String>>>,
    metrics: Arc<RuntimeMetrics>,
    events: EventLane,
    trace: Option<TraceWriter>,
}

impl DaemonState {
    pub fn synthetic(profile: impl Into<String>, bearer_token: impl Into<String>) -> Self {
        let profile = profile.into();
        let input = cua_platform_macos::input_backend_or_refusing();
        let events = EventLane::spawn(event_lane_capacity(), event_lane_retention());
        let state = Self {
            profile: profile.clone(),
            started_at: Utc::now(),
            frame_bus: Arc::new(FrameBus::new(
                cua_platform_macos::capture_backend_or_synthetic(),
            )),
            input_lane: InputLane::spawn(input.clone(), input_lane_capacity()),
            input,
            active_streams: Arc::new(AtomicU32::new(0)),
            bearer_token: Arc::new(bearer_token.into()),
            control: Arc::new(RwLock::new(default_control_state(&profile))),
            clipboard: Arc::new(RwLock::new(None)),
            metrics: Arc::new(RuntimeMetrics::default()),
            events,
            trace: std::env::var("CUA_TRACE_DIR")
                .ok()
                .and_then(|dir| TraceWriter::from_dir(dir).ok()),
        };
        state.publish_event("daemon_started", serde_json::json!({}));
        state
    }

    pub async fn health(&self) -> HealthReport {
        let control = self.control.read().await;
        HealthReport {
            schema_version: SCHEMA_VERSION.to_string(),
            status: CapabilityState::Degraded,
            version: env!("CARGO_PKG_VERSION").to_string(),
            profile: self.profile.clone(),
            started_at: self.started_at,
            permissions: cua_platform_macos::permission_report(),
            latest_frame: self.frame_bus.latest_envelope().await,
            safety_state: control.safety_state.clone(),
            active_profile: control.active_profile.name.clone(),
            active_streams: self.active_streams.load(Ordering::Relaxed),
            model_sessions: 0,
            last_error: None,
        }
    }
}

impl DaemonState {
    fn publish_event(&self, kind: &'static str, data: serde_json::Value) {
        if !self.events.publish(kind, data) {
            self.metrics.increment(CounterKind::EventDrops);
        }
    }
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
struct EventLane {
    sender: mpsc::Sender<EventJob>,
    recent: Arc<RwLock<VecDeque<serde_json::Value>>>,
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
        tokio::spawn(async move {
            while let Some(job) = receiver.recv().await {
                let mut recent = worker_recent.write().await;
                if recent.len() >= retention {
                    recent.pop_front();
                }
                recent.push_back(job.event);
            }
        });
        Self { sender, recent }
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
        .route("/capture/stream.mjpeg", get(stream_mjpeg))
        .route("/capture/stream.ws", get(stream_ws))
        .route("/observe/desktop", get(observe_desktop))
        .route("/observe/displays", get(observe_displays))
        .route("/observe/cursor", get(observe_cursor))
        .route("/events", get(events))
        .route("/events/live", get(events_live))
        .route("/profile/create", post(profile_create))
        .route("/profile/activate", post(profile_activate))
        .route("/profile/status", get(profile_status))
        .route("/control/pause", post(control_pause))
        .route("/control/resume", post(control_resume))
        .route("/control/kill-switch", post(control_kill_switch))
        .route("/input/mouse", post(input_action))
        .route("/input/keyboard", post(input_action))
        .route("/input/clipboard", post(input_action))
        .route("/clipboard/read", post(clipboard_read))
        .route("/clipboard/write", post(clipboard_write))
        .route("/model/eval", post(model_eval))
        .layer(middleware::from_fn_with_state(auth_state, require_auth))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub async fn serve(addr: SocketAddr, profile: String, allow_lan: bool) -> anyhow::Result<()> {
    if !allow_lan && !addr.ip().is_loopback() {
        anyhow::bail!(
            "refusing non-loopback bind {addr}; pass --allow-lan to expose the local HTTP API"
        );
    }
    let token = load_or_create_profile_token(&profile).await?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let state = DaemonState::synthetic(profile, token);
    state.frame_bus.clone().spawn_capture_lane(
        CaptureRequest {
            max_width: Some(1280),
            encoding: FrameEncoding::Jpeg,
            force_fresh: true,
        },
        Duration::from_millis(200),
    );
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
        "control_surfaces": ["cli", "local_http"],
    }))
}

async fn manifest() -> Json<Manifest> {
    Json(Manifest {
        schema_version: SCHEMA_VERSION.to_string(),
        name: "cua".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        public_surfaces: vec!["cli".to_string(), "local_http".to_string()],
        endpoints: vec![
            "GET /manifest".to_string(),
            "GET /schemas".to_string(),
            "GET /status".to_string(),
            "GET /metrics".to_string(),
            "POST /capture/screenshot".to_string(),
            "GET /capture/stream.mjpeg".to_string(),
            "GET /capture/stream.ws".to_string(),
            "GET /observe/desktop".to_string(),
            "GET /events".to_string(),
            "GET /events/live".to_string(),
            "POST /profile/create".to_string(),
            "POST /profile/activate".to_string(),
            "GET /profile/status".to_string(),
            "POST /control/pause".to_string(),
            "POST /control/resume".to_string(),
            "POST /control/kill-switch".to_string(),
            "POST /input/mouse".to_string(),
            "POST /input/keyboard".to_string(),
            "POST /input/clipboard".to_string(),
            "POST /clipboard/read".to_string(),
            "POST /clipboard/write".to_string(),
            "POST /model/eval".to_string(),
        ],
        commands: vec![
            "cua serve".to_string(),
            "cua status --json".to_string(),
            "cua perf live --json".to_string(),
            "cua screenshot --out <path>".to_string(),
            "cua observe --json".to_string(),
            "cua profile status --json".to_string(),
            "cua clipboard read --allow-sensitive --json".to_string(),
            "cua clipboard write <text> --json".to_string(),
            "cua pause --json".to_string(),
            "cua resume --json".to_string(),
            "cua kill-switch --json".to_string(),
            "cua model eval".to_string(),
        ],
    })
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
    Json(
        state
            .metrics
            .snapshot(state.active_streams.load(Ordering::Relaxed)),
    )
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

async fn screenshot(
    State(state): State<DaemonState>,
    Json(request): Json<ScreenshotRequest>,
) -> Result<Json<cua_core::FramePayload>, ApiError> {
    let started = Instant::now();
    let lookup = state
        .frame_bus
        .latest_or_capture_timed(CaptureRequest {
            max_width: request.max_width,
            encoding: request.encoding.unwrap_or(FrameEncoding::Png),
            force_fresh: request.force_fresh.unwrap_or(false),
        })
        .await
        .map_err(ApiError::internal)?;
    observe_frame_lookup(&state.metrics, &lookup);
    state
        .metrics
        .observe(MetricKind::CaptureScreenshot, started.elapsed());
    Ok(Json(
        lookup
            .frame
            .as_payload(request.include_bytes.unwrap_or(true)),
    ))
}

async fn stream_mjpeg(State(state): State<DaemonState>) -> Result<Response, ApiError> {
    let guard = StreamGuard::new(state.active_streams.clone());
    let mut interval = tokio::time::interval(Duration::from_millis(200));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let frame_bus = state.frame_bus.clone();
    let metrics = state.metrics.clone();
    let stream = futures::stream::unfold(
        (frame_bus, metrics, interval, guard),
        |(frame_bus, metrics, mut interval, guard)| async move {
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
                    let frame = lookup.frame;
                    let mut body = Vec::new();
                    body.extend_from_slice(b"--cua-frame\r\nContent-Type: image/jpeg\r\n");
                    body.extend_from_slice(
                        format!("X-CUA-Frame-Id: {}\r\n\r\n", frame.envelope.frame_id).as_bytes(),
                    );
                    body.extend_from_slice(&frame.bytes);
                    body.extend_from_slice(b"\r\n");
                    metrics.increment(CounterKind::MjpegFrames);
                    Ok::<Bytes, Infallible>(Bytes::from(body))
                }
                Err(error) => Ok::<Bytes, Infallible>(Bytes::from(format!(
                    "--cua-frame\r\nContent-Type: application/json\r\n\r\n{{\"error\":\"{}\"}}\r\n",
                    error.to_string().replace('"', "'")
                ))),
            };
            metrics.observe(MetricKind::StreamMjpegTick, started.elapsed());
            Some((chunk, (frame_bus, metrics, interval, guard)))
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
                    let frame = lookup.frame;
                    let text = match serde_json::to_string(&frame.envelope) {
                        Ok(text) => text,
                        Err(_) => break,
                    };
                    if socket.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                    if socket
                        .send(Message::Binary((*frame.bytes).clone()))
                        .await
                        .is_err()
                    {
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
    let displays = state
        .frame_bus
        .displays()
        .await
        .map_err(ApiError::internal)?;
    let latest_frame = state.frame_bus.latest_envelope().await;
    let cursor = cua_platform_macos::cursor_state();
    let windows = cua_platform_macos::window_list().map_err(ApiError::internal)?;
    Ok(Json(DesktopState {
        schema_version: SCHEMA_VERSION.to_string(),
        displays,
        windows,
        cursor,
        permissions: cua_platform_macos::permission_report(),
        latest_frame,
    }))
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

async fn events(State(state): State<DaemonState>) -> Json<Vec<serde_json::Value>> {
    Json(state.events.snapshot().await)
}

async fn events_live(State(state): State<DaemonState>) -> Json<Vec<serde_json::Value>> {
    Json(state.events.snapshot().await)
}

fn observe_frame_lookup(metrics: &RuntimeMetrics, lookup: &FrameLookup) {
    metrics.observe_ns(MetricKind::CaptureQueueWait, lookup.wait_ns);
    if !lookup.cache_hit {
        metrics.observe_ns(MetricKind::CaptureEncode, lookup.frame.timings.encode_ns);
    }
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
    Json(result)
}

async fn profile_activate(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
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
    Json(result)
}

async fn profile_status(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
    Json(state.control.read().await.clone())
}

async fn control_pause(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
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
    Json(result)
}

async fn control_resume(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
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
    Json(result)
}

async fn control_kill_switch(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
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
    Json(result)
}

#[derive(Debug)]
struct TraceSnapshot {
    envelope: cua_core::FrameEnvelope,
    path: String,
}

async fn trace_snapshot(state: &DaemonState, turn_id: &str, phase: &str) -> Option<TraceSnapshot> {
    let writer = state.trace.as_ref()?;
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
    writer
        .write_artifact(&relative, frame.bytes.as_ref().as_slice())
        .await
        .ok()?;
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
    let Some(writer) = state.trace.as_ref() else {
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
            "trace_dir": writer.dir().display().to_string(),
            "capture_backend": state.frame_bus.backend_name(),
            "input_backend": state.input_lane.name()
        }),
    });
    let trace_started = Instant::now();
    let _ = writer.append(&record).await;
    state
        .metrics
        .observe(MetricKind::TraceWrite, trace_started.elapsed());
}

async fn input_action(
    State(state): State<DaemonState>,
    Json(action): Json<InputAction>,
) -> Json<cua_core::InputResult> {
    let started = Instant::now();
    let turn_id = Uuid::new_v4().to_string();
    let action_json = serde_json::to_value(&action).unwrap_or_else(|_| serde_json::json!(null));
    let before = trace_snapshot(&state, &turn_id, "before").await;
    if matches!(
        action,
        InputAction::ClipboardRead { .. } | InputAction::ClipboardWrite { .. }
    ) {
        state.metrics.increment(CounterKind::InputRefusals);
        state
            .metrics
            .observe(MetricKind::InputDispatch, started.elapsed());
        let result = refused_input_result(
            "clipboard actions must use /clipboard/read or /clipboard/write for explicit grants",
        );
        let after = trace_snapshot(&state, &turn_id, "after").await;
        append_action_turn(
            &state,
            turn_id,
            action_json,
            serde_json::to_value(&result).unwrap_or_else(|_| serde_json::json!(null)),
            before,
            after,
        )
        .await;
        publish_input_event(&state, "input_refused", &result);
        return Json(result);
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
    if result.effect == Effect::Refused {
        state.metrics.increment(CounterKind::InputRefusals);
    }
    state
        .metrics
        .observe(MetricKind::InputDispatch, dispatch_started.elapsed());
    let after = trace_snapshot(&state, &turn_id, "after").await;
    append_action_turn(
        &state,
        turn_id,
        action_json,
        serde_json::to_value(&result).unwrap_or_else(|_| serde_json::json!(null)),
        before,
        after,
    )
    .await;
    publish_input_event(&state, "input_completed", &result);
    Json(result)
}

async fn clipboard_read(
    State(state): State<DaemonState>,
    Json(request): Json<ClipboardReadRequest>,
) -> Json<ClipboardResult> {
    let started = Instant::now();
    let action = "clipboard_read";
    let turn_id = Uuid::new_v4().to_string();
    let action_json = serde_json::to_value(&request).unwrap_or_else(|_| serde_json::json!(null));
    let before = trace_snapshot(&state, &turn_id, "before").await;
    if request.schema_version != SCHEMA_VERSION {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardRead, started.elapsed());
        let result = clipboard_refusal(action, "schema_version must match the daemon schema");
        let after = trace_snapshot(&state, &turn_id, "after").await;
        append_clipboard_turn(&state, turn_id, action_json, &result, before, after).await;
        publish_clipboard_event(&state, &result);
        return Json(result);
    }
    if !request.allow_sensitive {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardRead, started.elapsed());
        let result = clipboard_refusal(action, "clipboard read requires allow_sensitive=true");
        let after = trace_snapshot(&state, &turn_id, "after").await;
        append_clipboard_turn(&state, turn_id, action_json, &result, before, after).await;
        publish_clipboard_event(&state, &result);
        return Json(result);
    }
    if let Some(message) = clipboard_refusal_reason(&state).await {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardRead, started.elapsed());
        let result = clipboard_refusal(action, message);
        let after = trace_snapshot(&state, &turn_id, "after").await;
        append_clipboard_turn(&state, turn_id, action_json, &result, before, after).await;
        publish_clipboard_event(&state, &result);
        return Json(result);
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
    let after = trace_snapshot(&state, &turn_id, "after").await;
    append_clipboard_turn(&state, turn_id, action_json, &result, before, after).await;
    publish_clipboard_event(&state, &result);
    Json(result)
}

async fn clipboard_write(
    State(state): State<DaemonState>,
    Json(request): Json<ClipboardWriteRequest>,
) -> Json<ClipboardResult> {
    let started = Instant::now();
    let action = "clipboard_write";
    let turn_id = Uuid::new_v4().to_string();
    let action_json = serde_json::to_value(&request).unwrap_or_else(|_| serde_json::json!(null));
    let before = trace_snapshot(&state, &turn_id, "before").await;
    if request.schema_version != SCHEMA_VERSION {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardWrite, started.elapsed());
        let result = clipboard_refusal(action, "schema_version must match the daemon schema");
        let after = trace_snapshot(&state, &turn_id, "after").await;
        append_clipboard_turn(&state, turn_id, action_json, &result, before, after).await;
        publish_clipboard_event(&state, &result);
        return Json(result);
    }
    if let Some(message) = clipboard_refusal_reason(&state).await {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardWrite, started.elapsed());
        let result = clipboard_refusal(action, message);
        let after = trace_snapshot(&state, &turn_id, "after").await;
        append_clipboard_turn(&state, turn_id, action_json, &result, before, after).await;
        publish_clipboard_event(&state, &result);
        return Json(result);
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
    let after = trace_snapshot(&state, &turn_id, "after").await;
    append_clipboard_turn(&state, turn_id, action_json, &result, before, after).await;
    publish_clipboard_event(&state, &result);
    Json(result)
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
    StreamMjpegTick,
    StreamWsTick,
    InputQueueWait,
    InputDispatch,
    ClipboardRead,
    ClipboardWrite,
    ModelSend,
    ModelResponse,
    ModelParse,
    PolicyCheck,
    Verification,
    TraceWrite,
    KillSwitchPropagation,
}

impl MetricKind {
    const ALL: [Self; 16] = [
        Self::CaptureScreenshot,
        Self::CaptureQueueWait,
        Self::CaptureEncode,
        Self::StreamMjpegTick,
        Self::StreamWsTick,
        Self::InputQueueWait,
        Self::InputDispatch,
        Self::ClipboardRead,
        Self::ClipboardWrite,
        Self::ModelSend,
        Self::ModelResponse,
        Self::ModelParse,
        Self::PolicyCheck,
        Self::Verification,
        Self::TraceWrite,
        Self::KillSwitchPropagation,
    ];

    fn index(self) -> usize {
        match self {
            Self::CaptureScreenshot => 0,
            Self::CaptureQueueWait => 1,
            Self::CaptureEncode => 2,
            Self::StreamMjpegTick => 3,
            Self::StreamWsTick => 4,
            Self::InputQueueWait => 5,
            Self::InputDispatch => 6,
            Self::ClipboardRead => 7,
            Self::ClipboardWrite => 8,
            Self::ModelSend => 9,
            Self::ModelResponse => 10,
            Self::ModelParse => 11,
            Self::PolicyCheck => 12,
            Self::Verification => 13,
            Self::TraceWrite => 14,
            Self::KillSwitchPropagation => 15,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::CaptureScreenshot => "capture.screenshot",
            Self::CaptureQueueWait => "capture.queue_wait",
            Self::CaptureEncode => "capture.encode",
            Self::StreamMjpegTick => "stream.mjpeg.tick",
            Self::StreamWsTick => "stream.ws.tick",
            Self::InputQueueWait => "input.queue_wait",
            Self::InputDispatch => "input.dispatch",
            Self::ClipboardRead => "clipboard.read",
            Self::ClipboardWrite => "clipboard.write",
            Self::ModelSend => "model.send",
            Self::ModelResponse => "model.response",
            Self::ModelParse => "model.parse",
            Self::PolicyCheck => "policy.check",
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
    InputRefusals,
    ClipboardRefusals,
    EventDrops,
}

impl CounterKind {
    const ALL: [Self; 5] = [
        Self::MjpegFrames,
        Self::WsFrames,
        Self::InputRefusals,
        Self::ClipboardRefusals,
        Self::EventDrops,
    ];

    fn index(self) -> usize {
        match self {
            Self::MjpegFrames => 0,
            Self::WsFrames => 1,
            Self::InputRefusals => 2,
            Self::ClipboardRefusals => 3,
            Self::EventDrops => 4,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::MjpegFrames => "stream.mjpeg.frames",
            Self::WsFrames => "stream.ws.frames",
            Self::InputRefusals => "input.refusals",
            Self::ClipboardRefusals => "clipboard.refusals",
            Self::EventDrops => "events.dropped",
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
    let send_started = Instant::now();
    let report = run_eval_report(config, Some(frame), key).await;
    state
        .metrics
        .observe(MetricKind::ModelSend, send_started.elapsed());
    let response_started = Instant::now();
    let response = Json(report);
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
        };

        assert!(!lane.publish("overflow", serde_json::json!({})));
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
}
