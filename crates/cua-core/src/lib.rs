use chrono::{DateTime, Utc};
use schemars::schema_for;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use uuid::Uuid;

pub const SCHEMA_VERSION: &str = "cua.v1";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Ready,
    Degraded,
    Refused,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    Granted,
    Missing,
    Denied,
    NotApplicable,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct PermissionReport {
    pub screen_recording: PermissionState,
    pub accessibility_input: PermissionState,
    pub automation: PermissionState,
    pub clipboard: PermissionState,
    pub portal: PermissionState,
}

impl PermissionReport {
    pub fn conservative_unknown() -> Self {
        Self {
            screen_recording: PermissionState::Unknown,
            accessibility_input: PermissionState::Unknown,
            automation: PermissionState::Unknown,
            clipboard: PermissionState::Unknown,
            portal: PermissionState::NotApplicable,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct CursorState {
    pub x: f64,
    pub y: f64,
    pub visible: bool,
    pub included_in_frame: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct WindowInfo {
    pub id: String,
    pub app_name: Option<String>,
    pub title: Option<String>,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub focused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FrameEncoding {
    Png,
    Jpeg,
    RawBgra,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct FrameEnvelope {
    pub schema_version: String,
    pub frame_id: u64,
    pub timestamp_mono_ns: u128,
    pub timestamp_wall_ms: i64,
    pub display_id: String,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f64,
    pub pixel_format: String,
    pub encoding: FrameEncoding,
    pub byte_len: usize,
    pub sha256: String,
    pub cursor: CursorState,
    pub damage_rects: Vec<Rect>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct FramePayload {
    pub envelope: FrameEnvelope,
    pub bytes_base64: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct DesktopState {
    pub schema_version: String,
    pub displays: Vec<DisplayInfo>,
    pub windows: Vec<WindowInfo>,
    pub cursor: CursorState,
    pub permissions: PermissionReport,
    pub latest_frame: Option<FrameEnvelope>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct DesktopContextSnapshot {
    pub schema_version: String,
    pub frame: FramePayload,
    pub desktop: DesktopState,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeMode {
    Observe,
    Supervised,
    Autonomous,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SafetyState {
    Running,
    Paused,
    Killed,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CapabilityManifest {
    pub actions: Vec<String>,
    pub displays: Vec<String>,
    pub apps: Vec<String>,
    pub clipboard: bool,
    pub model_egress: bool,
    pub max_fps: u32,
}

impl Default for CapabilityManifest {
    fn default() -> Self {
        Self {
            actions: vec!["observe".to_string()],
            displays: vec!["primary".to_string()],
            apps: Vec::new(),
            clipboard: false,
            model_egress: false,
            max_fps: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ProfilePolicy {
    pub schema_version: String,
    pub name: String,
    pub mode: RuntimeMode,
    pub capabilities: CapabilityManifest,
    pub created_wall_ms: i64,
    pub expires_wall_ms: Option<i64>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeControlState {
    pub schema_version: String,
    pub active_profile: ProfilePolicy,
    pub safety_state: SafetyState,
    pub generation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MetricBucket {
    pub le_ms: u64,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MetricHistogram {
    pub name: String,
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub buckets: Vec<MetricBucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub schema_version: String,
    pub histograms: Vec<MetricHistogram>,
    pub counters: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Confirmed,
    Partial,
    Unverifiable,
    SuspectedNoop,
    Refused,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InputRoute {
    Accessibility,
    SyntheticEvents,
    GlobalInput,
    Portal,
    TrustedInput,
    Dom,
    SystemApi,
    Simulated,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryMode {
    Foreground,
    Background,
    Desktop,
    NotApplicable,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    FrameChange,
    CursorReadback,
    ValueReadback,
    WindowChange,
    FixtureOracle,
    ModelObservation,
    Refusal,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub message: String,
    pub frame_id: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputAction {
    MouseMove {
        x: i32,
        y: i32,
        duration_ms: u64,
    },
    MouseClick {
        x: i32,
        y: i32,
        button: MouseButton,
        count: u8,
    },
    MouseDrag {
        from_x: i32,
        from_y: i32,
        to_x: i32,
        to_y: i32,
        duration_ms: u64,
    },
    KeyPress {
        combo: String,
    },
    KeyType {
        text: String,
    },
    KeyPaste {
        text: String,
    },
    ClipboardRead {
        allow_sensitive: bool,
    },
    ClipboardWrite {
        text: String,
    },
    Pause,
    Resume,
    KillSwitch,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct InputRequest {
    pub schema_version: String,
    #[schemars(with = "String")]
    pub idempotency_key: Uuid,
    pub deadline_mono_ns: Option<u128>,
    pub action: InputAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct InputResult {
    pub schema_version: String,
    #[schemars(with = "String")]
    pub idempotency_key: Uuid,
    pub effect: Effect,
    pub route: InputRoute,
    pub delivery_mode: DeliveryMode,
    pub started_mono_ns: u128,
    pub ended_mono_ns: u128,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ClipboardReadRequest {
    pub schema_version: String,
    pub allow_sensitive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ClipboardWriteRequest {
    pub schema_version: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ClipboardResult {
    pub schema_version: String,
    pub action: String,
    pub result: InputResult,
    pub text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct HealthReport {
    pub schema_version: String,
    pub status: CapabilityState,
    pub version: String,
    pub profile: String,
    pub started_at: DateTime<Utc>,
    pub permissions: PermissionReport,
    pub latest_frame: Option<FrameEnvelope>,
    pub safety_state: SafetyState,
    pub active_profile: String,
    pub active_streams: u32,
    pub model_sessions: u32,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Manifest {
    pub schema_version: String,
    pub name: String,
    pub version: String,
    pub public_surfaces: Vec<String>,
    pub endpoints: Vec<String>,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ApiErrorBody {
    pub schema_version: String,
    pub code: String,
    pub message: String,
    pub details: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct SchemaBundle {
    pub schema_version: String,
    pub schemas: BTreeMap<String, serde_json::Value>,
}

pub fn schema_bundle() -> SchemaBundle {
    let mut schemas = BTreeMap::new();
    schemas.insert(
        "ApiErrorBody".to_string(),
        serde_json::json!(schema_for!(ApiErrorBody)),
    );
    schemas.insert(
        "DesktopState".to_string(),
        serde_json::json!(schema_for!(DesktopState)),
    );
    schemas.insert(
        "DesktopContextSnapshot".to_string(),
        serde_json::json!(schema_for!(DesktopContextSnapshot)),
    );
    schemas.insert(
        "FrameEnvelope".to_string(),
        serde_json::json!(schema_for!(FrameEnvelope)),
    );
    schemas.insert(
        "FramePayload".to_string(),
        serde_json::json!(schema_for!(FramePayload)),
    );
    schemas.insert(
        "HealthReport".to_string(),
        serde_json::json!(schema_for!(HealthReport)),
    );
    schemas.insert(
        "RuntimeControlState".to_string(),
        serde_json::json!(schema_for!(RuntimeControlState)),
    );
    schemas.insert(
        "ProfilePolicy".to_string(),
        serde_json::json!(schema_for!(ProfilePolicy)),
    );
    schemas.insert(
        "MetricsSnapshot".to_string(),
        serde_json::json!(schema_for!(MetricsSnapshot)),
    );
    schemas.insert(
        "InputAction".to_string(),
        serde_json::json!(schema_for!(InputAction)),
    );
    schemas.insert(
        "InputRequest".to_string(),
        serde_json::json!(schema_for!(InputRequest)),
    );
    schemas.insert(
        "InputResult".to_string(),
        serde_json::json!(schema_for!(InputResult)),
    );
    schemas.insert(
        "ClipboardReadRequest".to_string(),
        serde_json::json!(schema_for!(ClipboardReadRequest)),
    );
    schemas.insert(
        "ClipboardWriteRequest".to_string(),
        serde_json::json!(schema_for!(ClipboardWriteRequest)),
    );
    schemas.insert(
        "ClipboardResult".to_string(),
        serde_json::json!(schema_for!(ClipboardResult)),
    );
    schemas.insert(
        "Manifest".to_string(),
        serde_json::json!(schema_for!(Manifest)),
    );
    SchemaBundle {
        schema_version: SCHEMA_VERSION.to_string(),
        schemas,
    }
}

#[derive(Debug, Error)]
pub enum CuaError {
    #[error("{code}: {message}")]
    Api { code: String, message: String },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub fn now_wall_ms() -> i64 {
    Utc::now().timestamp_millis()
}
