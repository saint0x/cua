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
use cua_capture::{CaptureRequest, FrameBus};
use cua_core::{
    now_wall_ms, schema_bundle, ApiErrorBody, CapabilityManifest, CapabilityState,
    ClipboardReadRequest, ClipboardResult, ClipboardWriteRequest, DeliveryMode, DesktopState,
    Effect, Evidence, EvidenceKind, FrameEncoding, HealthReport, InputAction, InputRequest,
    InputResult, InputRoute, Manifest, MetricBucket, MetricHistogram, MetricsSnapshot,
    ProfilePolicy, RuntimeControlState, RuntimeMode, SafetyState, SCHEMA_VERSION,
};
use cua_input::InputBackend;
use cua_model::{run_eval_report, EvalConfig, EvalReport};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU32, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

#[derive(Clone)]
pub struct DaemonState {
    pub profile: String,
    pub started_at: chrono::DateTime<Utc>,
    pub frame_bus: Arc<FrameBus>,
    pub input: Arc<dyn InputBackend>,
    pub active_streams: Arc<AtomicU32>,
    pub bearer_token: Arc<String>,
    pub control: Arc<RwLock<RuntimeControlState>>,
    pub clipboard: Arc<RwLock<Option<String>>>,
    metrics: Arc<RuntimeMetrics>,
}

impl DaemonState {
    pub fn synthetic(profile: impl Into<String>, bearer_token: impl Into<String>) -> Self {
        let profile = profile.into();
        Self {
            profile: profile.clone(),
            started_at: Utc::now(),
            frame_bus: Arc::new(FrameBus::new(
                cua_platform_macos::capture_backend_or_synthetic(),
            )),
            input: cua_platform_macos::input_backend_or_refusing(),
            active_streams: Arc::new(AtomicU32::new(0)),
            bearer_token: Arc::new(bearer_token.into()),
            control: Arc::new(RwLock::new(default_control_state(&profile))),
            clipboard: Arc::new(RwLock::new(None)),
            metrics: Arc::new(RuntimeMetrics::default()),
        }
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
    if is_auth_exempt(request.uri().path()) {
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
        next.run(request).await
    } else {
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
    let frame = state
        .frame_bus
        .latest_or_capture(CaptureRequest {
            max_width: request.max_width,
            encoding: request.encoding.unwrap_or(FrameEncoding::Png),
            force_fresh: request.force_fresh.unwrap_or(false),
        })
        .await
        .map_err(ApiError::internal)?;
    state
        .metrics
        .observe(MetricKind::CaptureScreenshot, started.elapsed());
    Ok(Json(
        frame.as_payload(request.include_bytes.unwrap_or(true)),
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
                .latest_or_capture(CaptureRequest {
                    max_width: Some(1280),
                    encoding: FrameEncoding::Jpeg,
                    force_fresh: false,
                })
                .await
            {
                Ok(frame) => {
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
                .latest_or_capture(CaptureRequest {
                    max_width: Some(1280),
                    encoding: FrameEncoding::Jpeg,
                    force_fresh: false,
                })
                .await
            {
                Ok(frame) => {
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
    let cursor = latest_frame
        .as_ref()
        .map(|f| f.cursor.clone())
        .unwrap_or(cua_core::CursorState {
            x: 0.0,
            y: 0.0,
            visible: false,
            included_in_frame: false,
        });
    Ok(Json(DesktopState {
        schema_version: SCHEMA_VERSION.to_string(),
        displays,
        windows: Vec::new(),
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
    let cursor = state
        .frame_bus
        .latest_envelope()
        .await
        .map(|f| f.cursor)
        .unwrap_or(cua_core::CursorState {
            x: 0.0,
            y: 0.0,
            visible: false,
            included_in_frame: false,
        });
    Json(cursor)
}

async fn events() -> Json<Vec<serde_json::Value>> {
    Json(vec![serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "sequence": 1,
        "kind": "daemon_started"
    })])
}

async fn events_live() -> Json<Vec<serde_json::Value>> {
    events().await
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
    Json(control.clone())
}

async fn profile_activate(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
    let mut control = state.control.write().await;
    if control.safety_state != SafetyState::Killed {
        control.active_profile.active = true;
        control.safety_state = SafetyState::Running;
        control.generation += 1;
    }
    Json(control.clone())
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
    Json(control.clone())
}

async fn control_resume(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
    let mut control = state.control.write().await;
    if control.safety_state != SafetyState::Killed {
        control.safety_state = SafetyState::Running;
        control.generation += 1;
    }
    Json(control.clone())
}

async fn control_kill_switch(State(state): State<DaemonState>) -> Json<RuntimeControlState> {
    let mut control = state.control.write().await;
    control.safety_state = SafetyState::Killed;
    control.generation += 1;
    Json(control.clone())
}

async fn input_action(
    State(state): State<DaemonState>,
    Json(action): Json<InputAction>,
) -> Json<cua_core::InputResult> {
    let started = Instant::now();
    if matches!(
        action,
        InputAction::ClipboardRead { .. } | InputAction::ClipboardWrite { .. }
    ) {
        state.metrics.increment(CounterKind::InputRefusals);
        state
            .metrics
            .observe(MetricKind::InputDispatch, started.elapsed());
        return Json(refused_input_result(
            "clipboard actions must use /clipboard/read or /clipboard/write for explicit grants",
        ));
    }
    let result = state
        .input
        .execute(InputRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            idempotency_key: Uuid::new_v4(),
            deadline_mono_ns: None,
            action,
        })
        .await;
    if result.effect == Effect::Refused {
        state.metrics.increment(CounterKind::InputRefusals);
    }
    state
        .metrics
        .observe(MetricKind::InputDispatch, started.elapsed());
    Json(result)
}

async fn clipboard_read(
    State(state): State<DaemonState>,
    Json(request): Json<ClipboardReadRequest>,
) -> Json<ClipboardResult> {
    let started = Instant::now();
    let action = "clipboard_read";
    if request.schema_version != SCHEMA_VERSION {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardRead, started.elapsed());
        return Json(clipboard_refusal(
            action,
            "schema_version must match the daemon schema",
        ));
    }
    if !request.allow_sensitive {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardRead, started.elapsed());
        return Json(clipboard_refusal(
            action,
            "clipboard read requires allow_sensitive=true",
        ));
    }
    if let Some(message) = clipboard_refusal_reason(&state).await {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardRead, started.elapsed());
        return Json(clipboard_refusal(action, message));
    }
    let text = state.clipboard.read().await.clone();
    state
        .metrics
        .observe(MetricKind::ClipboardRead, started.elapsed());
    Json(ClipboardResult {
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
    })
}

async fn clipboard_write(
    State(state): State<DaemonState>,
    Json(request): Json<ClipboardWriteRequest>,
) -> Json<ClipboardResult> {
    let started = Instant::now();
    let action = "clipboard_write";
    if request.schema_version != SCHEMA_VERSION {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardWrite, started.elapsed());
        return Json(clipboard_refusal(
            action,
            "schema_version must match the daemon schema",
        ));
    }
    if let Some(message) = clipboard_refusal_reason(&state).await {
        state.metrics.increment(CounterKind::ClipboardRefusals);
        state
            .metrics
            .observe(MetricKind::ClipboardWrite, started.elapsed());
        return Json(clipboard_refusal(action, message));
    }
    *state.clipboard.write().await = Some(request.text);
    state
        .metrics
        .observe(MetricKind::ClipboardWrite, started.elapsed());
    Json(ClipboardResult {
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
    })
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
    let started_mono_ns = std::time::Instant::now().elapsed().as_nanos();
    InputResult {
        schema_version: SCHEMA_VERSION.to_string(),
        idempotency_key: Uuid::new_v4(),
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
    StreamMjpegTick,
    StreamWsTick,
    InputDispatch,
    ClipboardRead,
    ClipboardWrite,
}

impl MetricKind {
    const ALL: [Self; 6] = [
        Self::CaptureScreenshot,
        Self::StreamMjpegTick,
        Self::StreamWsTick,
        Self::InputDispatch,
        Self::ClipboardRead,
        Self::ClipboardWrite,
    ];

    fn index(self) -> usize {
        match self {
            Self::CaptureScreenshot => 0,
            Self::StreamMjpegTick => 1,
            Self::StreamWsTick => 2,
            Self::InputDispatch => 3,
            Self::ClipboardRead => 4,
            Self::ClipboardWrite => 5,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::CaptureScreenshot => "capture.screenshot",
            Self::StreamMjpegTick => "stream.mjpeg.tick",
            Self::StreamWsTick => "stream.ws.tick",
            Self::InputDispatch => "input.dispatch",
            Self::ClipboardRead => "clipboard.read",
            Self::ClipboardWrite => "clipboard.write",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum CounterKind {
    MjpegFrames,
    WsFrames,
    InputRefusals,
    ClipboardRefusals,
}

impl CounterKind {
    const ALL: [Self; 4] = [
        Self::MjpegFrames,
        Self::WsFrames,
        Self::InputRefusals,
        Self::ClipboardRefusals,
    ];

    fn index(self) -> usize {
        match self {
            Self::MjpegFrames => 0,
            Self::WsFrames => 1,
            Self::InputRefusals => 2,
            Self::ClipboardRefusals => 3,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::MjpegFrames => "stream.mjpeg.frames",
            Self::WsFrames => "stream.ws.frames",
            Self::InputRefusals => "input.refusals",
            Self::ClipboardRefusals => "clipboard.refusals",
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
        let ns = duration.as_nanos().min(u64::MAX as u128) as u64;
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_ns.fetch_add(ns, Ordering::Relaxed);
        self.max_ns.fetch_max(ns, Ordering::Relaxed);

        let ms = duration.as_millis().min(u64::MAX as u128) as u64;
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
    let frame = state
        .frame_bus
        .latest_or_capture(CaptureRequest {
            max_width: Some(640),
            encoding: FrameEncoding::Png,
            force_fresh: true,
        })
        .await
        .map_err(ApiError::internal)?
        .as_payload(true);
    let mut config = EvalConfig::default();
    config.live = request.live.unwrap_or(false);
    config.max_calls = request.max_calls.unwrap_or(config.max_calls);
    if let Some(max_output_tokens) = request.max_output_tokens {
        for candidate in &mut config.candidates {
            candidate.max_output_tokens = max_output_tokens;
        }
    }
    let key = std::env::var("OPENROUTER_API_KEY").ok();
    Ok(Json(run_eval_report(config, Some(frame), key).await))
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
}
