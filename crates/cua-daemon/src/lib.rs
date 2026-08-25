use axum::{
    extract::{
        ws::{Message, WebSocketUpgrade},
        State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use cua_capture::{CaptureRequest, FrameBus, SyntheticCaptureBackend};
use cua_core::{
    schema_bundle, ApiErrorBody, CapabilityState, DesktopState, FrameEncoding, HealthReport,
    InputAction, InputRequest, Manifest, PermissionReport, SCHEMA_VERSION,
};
use cua_input::{InputBackend, RefusingInputBackend};
use cua_model::{run_eval_report, EvalConfig, EvalReport};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::trace::TraceLayer;
use uuid::Uuid;

#[derive(Clone)]
pub struct DaemonState {
    pub profile: String,
    pub started_at: chrono::DateTime<Utc>,
    pub frame_bus: Arc<FrameBus>,
    pub input: Arc<dyn InputBackend>,
}

impl DaemonState {
    pub fn synthetic(profile: impl Into<String>) -> Self {
        Self {
            profile: profile.into(),
            started_at: Utc::now(),
            frame_bus: Arc::new(FrameBus::new(Arc::new(SyntheticCaptureBackend::default()))),
            input: Arc::new(RefusingInputBackend),
        }
    }

    pub async fn health(&self) -> HealthReport {
        HealthReport {
            schema_version: SCHEMA_VERSION.to_string(),
            status: CapabilityState::Degraded,
            version: env!("CARGO_PKG_VERSION").to_string(),
            profile: self.profile.clone(),
            started_at: self.started_at,
            permissions: PermissionReport::conservative_unknown(),
            latest_frame: self.frame_bus.latest_envelope().await,
            active_streams: 0,
            model_sessions: 0,
            last_error: None,
        }
    }
}

pub fn router(state: DaemonState) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/manifest", get(manifest))
        .route("/schemas", get(schemas))
        .route("/version", get(version))
        .route("/status", get(status))
        .route("/healthz", get(healthz))
        .route("/capture/screenshot", post(screenshot))
        .route("/capture/stream.mjpeg", get(stream_mjpeg))
        .route("/capture/stream.ws", get(stream_ws))
        .route("/observe/desktop", get(observe_desktop))
        .route("/observe/displays", get(observe_displays))
        .route("/observe/cursor", get(observe_cursor))
        .route("/events", get(events))
        .route("/events/live", get(events_live))
        .route("/input/mouse", post(input_action))
        .route("/input/keyboard", post(input_action))
        .route("/input/clipboard", post(input_action))
        .route("/model/eval", post(model_eval))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

pub async fn serve(addr: SocketAddr, profile: String) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router(DaemonState::synthetic(profile))).await?;
    Ok(())
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
            "POST /capture/screenshot".to_string(),
            "GET /capture/stream.mjpeg".to_string(),
            "GET /capture/stream.ws".to_string(),
            "GET /observe/desktop".to_string(),
            "GET /events".to_string(),
            "GET /events/live".to_string(),
            "POST /input/mouse".to_string(),
            "POST /input/keyboard".to_string(),
            "POST /model/eval".to_string(),
        ],
        commands: vec![
            "cua serve".to_string(),
            "cua status --json".to_string(),
            "cua screenshot --out <path>".to_string(),
            "cua observe --json".to_string(),
            "cua model eval".to_string(),
        ],
    })
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
    let frame = state
        .frame_bus
        .latest_or_capture(CaptureRequest {
            max_width: request.max_width,
            encoding: request.encoding.unwrap_or(FrameEncoding::Png),
            force_fresh: request.force_fresh.unwrap_or(false),
        })
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(
        frame.as_payload(request.include_bytes.unwrap_or(true)),
    ))
}

async fn stream_mjpeg(State(state): State<DaemonState>) -> Result<Response, ApiError> {
    let frame = state
        .frame_bus
        .latest_or_capture(CaptureRequest {
            max_width: Some(1280),
            encoding: FrameEncoding::Jpeg,
            force_fresh: true,
        })
        .await
        .map_err(ApiError::internal)?;
    let mut body = Vec::new();
    body.extend_from_slice(b"--cua-frame\r\nContent-Type: image/jpeg\r\n\r\n");
    body.extend_from_slice(&frame.bytes);
    body.extend_from_slice(b"\r\n--cua-frame--\r\n");
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("multipart/x-mixed-replace; boundary=cua-frame"),
    );
    Ok(response)
}

async fn stream_ws(ws: WebSocketUpgrade, State(state): State<DaemonState>) -> impl IntoResponse {
    ws.on_upgrade(move |mut socket| async move {
        if let Ok(frame) = state
            .frame_bus
            .latest_or_capture(CaptureRequest {
                max_width: Some(1280),
                encoding: FrameEncoding::Jpeg,
                force_fresh: true,
            })
            .await
        {
            if let Ok(text) = serde_json::to_string(&frame.envelope) {
                let _ = socket.send(Message::Text(text)).await;
                let _ = socket.send(Message::Binary((*frame.bytes).clone())).await;
            }
        }
    })
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
        permissions: PermissionReport::conservative_unknown(),
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

async fn input_action(
    State(state): State<DaemonState>,
    Json(action): Json<InputAction>,
) -> Json<cua_core::InputResult> {
    Json(
        state
            .input
            .execute(InputRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                idempotency_key: Uuid::new_v4(),
                deadline_mono_ns: None,
                action,
            })
            .await,
    )
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
