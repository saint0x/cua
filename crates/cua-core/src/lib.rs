use chrono::{DateTime, Utc};
use schemars::schema_for;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

pub const SCHEMA_VERSION: &str = "cua.v1";

pub fn cua_home() -> anyhow::Result<PathBuf> {
    if let Some(home) = std::env::var_os("CUA_HOME") {
        if !home.is_empty() {
            return Ok(PathBuf::from(home));
        }
    }
    Ok(PathBuf::from(std::env::var("HOME")?).join(".cua"))
}

pub fn config_dir() -> anyhow::Result<PathBuf> {
    Ok(cua_home()?.join("config"))
}

pub fn config_env_path() -> anyhow::Result<PathBuf> {
    Ok(config_dir()?.join("env"))
}

pub fn legacy_config_env_path() -> anyhow::Result<PathBuf> {
    Ok(cua_home()?.join(".env"))
}

pub fn profile_dir(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(cua_home()?.join("profiles").join(profile))
}

pub fn profile_token_path(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(profile_dir(profile)?.join("http.token"))
}

pub fn profile_socket_path(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(profile_dir(profile)?.join("daemon.sock"))
}

pub fn profile_chat_db_path(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(profile_dir(profile)?.join("chat.db"))
}

pub fn profile_ctx_dir(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(profile_dir(profile)?.join("ctx"))
}

pub fn profile_trace_dir(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(profile_dir(profile)?.join("traces"))
}

pub fn profile_voice_trace_path(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(profile_trace_dir(profile)?.join("voice.jsonl"))
}

pub fn profile_daemon_trace_dir(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(profile_trace_dir(profile)?.join("daemon"))
}

pub fn identity_dir() -> anyhow::Result<PathBuf> {
    Ok(cua_home()?.join("identity"))
}

pub fn cloud_dir() -> anyhow::Result<PathBuf> {
    Ok(cua_home()?.join("cloud"))
}

pub fn artifact_dir(concern: &str) -> anyhow::Result<PathBuf> {
    Ok(cua_home()?.join("artifacts").join(concern))
}

pub fn cache_dir(concern: &str) -> anyhow::Result<PathBuf> {
    Ok(cua_home()?.join("cache").join(concern))
}

pub fn log_dir(concern: &str) -> anyhow::Result<PathBuf> {
    Ok(cua_home()?.join("logs").join(concern))
}

pub fn bin_dir() -> anyhow::Result<PathBuf> {
    Ok(cua_home()?.join("bin"))
}

pub fn cua_bin_path(name: &str) -> anyhow::Result<PathBuf> {
    Ok(bin_dir()?.join(name))
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigMigrationState {
    Current,
    LegacyOnly,
    Conflict,
    Missing,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ConfigInventory {
    pub schema_version: String,
    pub cua_home: String,
    pub config_dir: String,
    pub config_env: String,
    pub legacy_config_env: String,
    pub legacy_config_env_present: bool,
    pub config_env_present: bool,
    pub migration_state: ConfigMigrationState,
    pub profile_root: String,
    pub profile_socket: String,
    pub profile_token_present: bool,
    pub chat_db: String,
    pub ctx_workspace: String,
    pub trace_root: String,
    pub voice_trace: String,
    pub daemon_trace_root: String,
    pub identity_root: String,
    pub cloud_root: String,
    pub artifact_root: String,
    pub cache_root: String,
    pub log_root: String,
    pub bin_root: String,
}

impl ConfigInventory {
    pub fn for_profile(profile: &str) -> anyhow::Result<Self> {
        let cua_home = cua_home()?;
        let config_dir = config_dir()?;
        let config_env = config_env_path()?;
        let legacy_config_env = legacy_config_env_path()?;
        let profile_root = profile_dir(profile)?;
        let profile_socket = profile_socket_path(profile)?;
        let profile_token = profile_token_path(profile)?;
        let chat_db = profile_chat_db_path(profile)?;
        let ctx_workspace = profile_ctx_dir(profile)?;
        let trace_root = profile_trace_dir(profile)?;
        let voice_trace = profile_voice_trace_path(profile)?;
        let daemon_trace_root = profile_daemon_trace_dir(profile)?;
        let identity_root = identity_dir()?;
        let cloud_root = cloud_dir()?;
        let artifact_root = cua_home.join("artifacts");
        let cache_root = cua_home.join("cache");
        let log_root = cua_home.join("logs");
        let bin_root = bin_dir()?;
        let config_env_present = config_env.exists();
        let legacy_config_env_present = legacy_config_env.exists();
        let migration_state = match (config_env_present, legacy_config_env_present) {
            (true, false) => ConfigMigrationState::Current,
            (false, true) => ConfigMigrationState::LegacyOnly,
            (true, true) => ConfigMigrationState::Conflict,
            (false, false) => ConfigMigrationState::Missing,
        };

        Ok(Self {
            schema_version: SCHEMA_VERSION.to_string(),
            cua_home: path_string(cua_home),
            config_dir: path_string(config_dir),
            config_env: path_string(config_env),
            legacy_config_env: path_string(legacy_config_env),
            legacy_config_env_present,
            config_env_present,
            migration_state,
            profile_root: path_string(profile_root),
            profile_socket: path_string(profile_socket),
            profile_token_present: profile_token.exists(),
            chat_db: path_string(chat_db),
            ctx_workspace: path_string(ctx_workspace),
            trace_root: path_string(trace_root),
            voice_trace: path_string(voice_trace),
            daemon_trace_root: path_string(daemon_trace_root),
            identity_root: path_string(identity_root),
            cloud_root: path_string(cloud_root),
            artifact_root: path_string(artifact_root),
            cache_root: path_string(cache_root),
            log_root: path_string(log_root),
            bin_root: path_string(bin_root),
        })
    }
}

fn path_string(path: PathBuf) -> String {
    path.display().to_string()
}

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
    pub input_monitoring: PermissionState,
    pub automation: PermissionState,
    pub clipboard: PermissionState,
    pub portal: PermissionState,
}

impl PermissionReport {
    pub fn conservative_unknown() -> Self {
        Self {
            screen_recording: PermissionState::Unknown,
            accessibility_input: PermissionState::Unknown,
            input_monitoring: PermissionState::Unknown,
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
    #[serde(default)]
    pub layer: i32,
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
    #[serde(default)]
    pub display_x: i32,
    #[serde(default)]
    pub display_y: i32,
    pub display_width: u32,
    pub display_height: u32,
    #[serde(default)]
    pub frame_origin_x: i32,
    #[serde(default)]
    pub frame_origin_y: i32,
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

impl FrameEnvelope {
    pub fn frame_to_display_x(&self, x: i32) -> i32 {
        self.display_x.saturating_add(remap_axis(
            x.saturating_sub(self.frame_origin_x),
            self.width,
            self.display_width,
        ))
    }

    pub fn frame_to_display_y(&self, y: i32) -> i32 {
        self.display_y.saturating_add(remap_axis(
            y.saturating_sub(self.frame_origin_y),
            self.height,
            self.display_height,
        ))
    }
}

fn remap_axis(value: i32, from: u32, to: u32) -> i32 {
    if from == 0 || to == 0 {
        return value;
    }
    ((value as f64) * (to as f64 / from as f64)).round() as i32
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
#[serde(rename_all = "snake_case")]
pub enum RuntimeSessionRole {
    Owner,
    Observer,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeSessionInfo {
    pub schema_version: String,
    pub session_id: String,
    pub role: RuntimeSessionRole,
    pub client_name: String,
    pub connected_wall_ms: i64,
    pub last_seen_wall_ms: i64,
    pub expires_wall_ms: Option<i64>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct RuntimeInventory {
    pub schema_version: String,
    pub daemon_pid: u32,
    pub http_addr: String,
    pub profile_socket: String,
    pub config: ConfigInventory,
    pub hud_pid: Option<u32>,
    pub connected_clients: u32,
    pub owner_session_id: Option<String>,
    pub sessions: Vec<RuntimeSessionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AttestationChallengeRequest {
    pub schema_version: String,
    pub audience: String,
    pub profile: Option<String>,
    pub requested_claims: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AttestationChallenge {
    pub schema_version: String,
    pub challenge_id: String,
    pub nonce: String,
    pub audience: String,
    pub issued_wall_ms: i64,
    pub expires_wall_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MachineKeyBackend {
    SecureEnclave,
    Keychain,
    FileForTests,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MachineIdentity {
    pub schema_version: String,
    pub machine_key_id: String,
    pub machine_public_key: String,
    pub machine_id_hash: String,
    pub created_wall_ms: i64,
    pub key_backend: MachineKeyBackend,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RuntimeIdentityClaims {
    pub schema_version: String,
    pub runtime_name: String,
    pub runtime_version: String,
    pub daemon_pid: u32,
    pub profile: String,
    pub socket_path: String,
    pub http_addr: String,
    pub bundle_id: Option<String>,
    pub designated_requirement: Option<String>,
    pub code_signature_summary: Option<String>,
    pub binary_sha256: Option<String>,
    pub permissions: PermissionReport,
    pub active_profile: ProfilePolicy,
    pub safety_state: SafetyState,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttestationSignatureAlgorithm {
    Ed25519,
    P256Sha256,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct MachineAttestation {
    pub schema_version: String,
    pub challenge: AttestationChallenge,
    pub identity: MachineIdentity,
    pub claims: RuntimeIdentityClaims,
    pub signature_algorithm: AttestationSignatureAlgorithm,
    pub signature: String,
    pub signed_wall_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SessionLeaseRequest {
    pub schema_version: String,
    pub session_id: String,
    pub client_name: String,
    pub role: RuntimeSessionRole,
    pub ttl_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SessionLeaseResult {
    pub schema_version: String,
    pub accepted: bool,
    pub session: RuntimeSessionInfo,
    pub owner_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct SessionCancelRequest {
    pub schema_version: String,
    pub session_id: String,
    pub target_session_id: Option<String>,
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
pub struct UiStepRequest {
    pub schema_version: String,
    pub label: String,
    pub source: Option<String>,
    pub task: Option<String>,
    pub tool: Option<String>,
    pub step_index: Option<u16>,
    pub step_total: Option<u16>,
    pub ttl_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct UiStepResult {
    pub schema_version: String,
    pub accepted: bool,
    pub label: String,
    pub source: Option<String>,
    pub task: Option<String>,
    pub tool: Option<String>,
    pub step_index: Option<u16>,
    pub step_total: Option<u16>,
    pub ttl_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct UiReplyRequest {
    pub schema_version: String,
    pub text: String,
    pub source: Option<String>,
    pub ttl_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct UiReplyResult {
    pub schema_version: String,
    pub accepted: bool,
    pub text: String,
    pub source: Option<String>,
    pub ttl_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiMode {
    Headful,
    Headless,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct UiModeRequest {
    pub schema_version: String,
    pub mode: UiMode,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct UiModeResult {
    pub schema_version: String,
    pub accepted: bool,
    pub mode: UiMode,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UiIslandState {
    Expanded,
    Collapsed,
    Toggle,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct UiIslandRequest {
    pub schema_version: String,
    pub state: UiIslandState,
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct UiIslandResult {
    pub schema_version: String,
    pub accepted: bool,
    pub state: UiIslandState,
    pub source: Option<String>,
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
    Sequence {
        actions: Vec<InputAction>,
        inter_action_delay_ms: u64,
    },
    OpenApp {
        app_name: String,
    },
    ShellExec {
        command: String,
        timeout_ms: u64,
    },
    Aegis {
        args: Vec<String>,
        timeout_ms: u64,
    },
    Ctx {
        args: Vec<String>,
        timeout_ms: u64,
        workspace_root: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct FrameActionRequest {
    pub schema_version: String,
    pub source_frame: FrameEnvelope,
    pub action: InputAction,
}

impl FrameActionRequest {
    pub fn into_display_action(self) -> InputAction {
        remap_action_from_frame(self.action, &self.source_frame)
    }
}

pub fn remap_action_from_frame(action: InputAction, frame: &FrameEnvelope) -> InputAction {
    match action {
        InputAction::MouseMove { x, y, duration_ms } => InputAction::MouseMove {
            x: frame.frame_to_display_x(x),
            y: frame.frame_to_display_y(y),
            duration_ms,
        },
        InputAction::MouseClick {
            x,
            y,
            button,
            count,
        } => InputAction::MouseClick {
            x: frame.frame_to_display_x(x),
            y: frame.frame_to_display_y(y),
            button,
            count,
        },
        InputAction::MouseDrag {
            from_x,
            from_y,
            to_x,
            to_y,
            duration_ms,
        } => InputAction::MouseDrag {
            from_x: frame.frame_to_display_x(from_x),
            from_y: frame.frame_to_display_y(from_y),
            to_x: frame.frame_to_display_x(to_x),
            to_y: frame.frame_to_display_y(to_y),
            duration_ms,
        },
        InputAction::Sequence {
            actions,
            inter_action_delay_ms,
        } => InputAction::Sequence {
            actions: actions
                .into_iter()
                .map(|action| remap_action_from_frame(action, frame))
                .collect(),
            inter_action_delay_ms,
        },
        action => action,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct VisualSessionRequest {
    pub schema_version: String,
    pub max_width: Option<u32>,
    pub fps: Option<u32>,
    pub include_bytes: bool,
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
    pub inventory: RuntimeInventory,
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
        "RuntimeInventory".to_string(),
        serde_json::json!(schema_for!(RuntimeInventory)),
    );
    schemas.insert(
        "ConfigInventory".to_string(),
        serde_json::json!(schema_for!(ConfigInventory)),
    );
    schemas.insert(
        "AttestationChallengeRequest".to_string(),
        serde_json::json!(schema_for!(AttestationChallengeRequest)),
    );
    schemas.insert(
        "AttestationChallenge".to_string(),
        serde_json::json!(schema_for!(AttestationChallenge)),
    );
    schemas.insert(
        "MachineIdentity".to_string(),
        serde_json::json!(schema_for!(MachineIdentity)),
    );
    schemas.insert(
        "RuntimeIdentityClaims".to_string(),
        serde_json::json!(schema_for!(RuntimeIdentityClaims)),
    );
    schemas.insert(
        "MachineAttestation".to_string(),
        serde_json::json!(schema_for!(MachineAttestation)),
    );
    schemas.insert(
        "RuntimeSessionInfo".to_string(),
        serde_json::json!(schema_for!(RuntimeSessionInfo)),
    );
    schemas.insert(
        "SessionLeaseRequest".to_string(),
        serde_json::json!(schema_for!(SessionLeaseRequest)),
    );
    schemas.insert(
        "SessionLeaseResult".to_string(),
        serde_json::json!(schema_for!(SessionLeaseResult)),
    );
    schemas.insert(
        "SessionCancelRequest".to_string(),
        serde_json::json!(schema_for!(SessionCancelRequest)),
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
        "UiStepRequest".to_string(),
        serde_json::json!(schema_for!(UiStepRequest)),
    );
    schemas.insert(
        "UiStepResult".to_string(),
        serde_json::json!(schema_for!(UiStepResult)),
    );
    schemas.insert(
        "UiReplyRequest".to_string(),
        serde_json::json!(schema_for!(UiReplyRequest)),
    );
    schemas.insert(
        "UiReplyResult".to_string(),
        serde_json::json!(schema_for!(UiReplyResult)),
    );
    schemas.insert(
        "UiModeRequest".to_string(),
        serde_json::json!(schema_for!(UiModeRequest)),
    );
    schemas.insert(
        "UiModeResult".to_string(),
        serde_json::json!(schema_for!(UiModeResult)),
    );
    schemas.insert(
        "UiIslandRequest".to_string(),
        serde_json::json!(schema_for!(UiIslandRequest)),
    );
    schemas.insert(
        "UiIslandResult".to_string(),
        serde_json::json!(schema_for!(UiIslandResult)),
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
        "FrameActionRequest".to_string(),
        serde_json::json!(schema_for!(FrameActionRequest)),
    );
    schemas.insert(
        "VisualSessionRequest".to_string(),
        serde_json::json!(schema_for!(VisualSessionRequest)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_action_remaps_mouse_coordinates_to_display_space() {
        let frame = FrameEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            frame_id: 7,
            timestamp_mono_ns: 0,
            timestamp_wall_ms: 0,
            display_id: "main".to_string(),
            display_x: 0,
            display_y: 0,
            display_width: 1512,
            display_height: 982,
            frame_origin_x: 0,
            frame_origin_y: 0,
            width: 1280,
            height: 831,
            scale_factor: 1.0,
            pixel_format: "rgba8".to_string(),
            encoding: FrameEncoding::Jpeg,
            byte_len: 0,
            sha256: String::new(),
            cursor: CursorState {
                x: 0.0,
                y: 0.0,
                visible: true,
                included_in_frame: false,
            },
            damage_rects: Vec::new(),
        };

        let action = remap_action_from_frame(
            InputAction::MouseClick {
                x: 100,
                y: 100,
                button: MouseButton::Left,
                count: 1,
            },
            &frame,
        );

        assert!(matches!(
            action,
            InputAction::MouseClick { x: 118, y: 118, .. }
        ));
    }

    #[test]
    fn frame_action_respects_display_and_frame_origins() {
        let frame = FrameEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            frame_id: 8,
            timestamp_mono_ns: 0,
            timestamp_wall_ms: 0,
            display_id: "secondary".to_string(),
            display_x: -1512,
            display_y: 25,
            display_width: 1512,
            display_height: 982,
            frame_origin_x: 20,
            frame_origin_y: 10,
            width: 1280,
            height: 831,
            scale_factor: 1.0,
            pixel_format: "rgba8".to_string(),
            encoding: FrameEncoding::Jpeg,
            byte_len: 0,
            sha256: String::new(),
            cursor: CursorState {
                x: 0.0,
                y: 0.0,
                visible: true,
                included_in_frame: false,
            },
            damage_rects: Vec::new(),
        };

        let action = remap_action_from_frame(
            InputAction::MouseMove {
                x: 120,
                y: 110,
                duration_ms: 0,
            },
            &frame,
        );

        assert!(matches!(
            action,
            InputAction::MouseMove {
                x: -1394,
                y: 143,
                ..
            }
        ));
    }

    #[test]
    fn frame_action_remaps_mouse_coordinates_inside_sequences() {
        let frame = FrameEnvelope {
            schema_version: SCHEMA_VERSION.to_string(),
            frame_id: 9,
            timestamp_mono_ns: 0,
            timestamp_wall_ms: 0,
            display_id: "main".to_string(),
            display_x: 0,
            display_y: 0,
            display_width: 1512,
            display_height: 982,
            frame_origin_x: 0,
            frame_origin_y: 0,
            width: 1280,
            height: 831,
            scale_factor: 1.0,
            pixel_format: "rgba8".to_string(),
            encoding: FrameEncoding::Jpeg,
            byte_len: 0,
            sha256: String::new(),
            cursor: CursorState {
                x: 0.0,
                y: 0.0,
                visible: true,
                included_in_frame: false,
            },
            damage_rects: Vec::new(),
        };

        let action = remap_action_from_frame(
            InputAction::Sequence {
                actions: vec![
                    InputAction::MouseClick {
                        x: 100,
                        y: 100,
                        button: MouseButton::Left,
                        count: 1,
                    },
                    InputAction::KeyPress {
                        combo: "enter".to_string(),
                    },
                ],
                inter_action_delay_ms: 120,
            },
            &frame,
        );

        assert!(matches!(
            action,
            InputAction::Sequence { actions, .. }
                if matches!(
                    actions.as_slice(),
                    [
                        InputAction::MouseClick { x: 118, y: 118, .. },
                        InputAction::KeyPress { combo },
                    ] if combo == "enter"
                )
        ));
    }

    #[test]
    fn cua_home_paths_respect_cua_home_override() {
        let old = std::env::var_os("CUA_HOME");
        std::env::set_var("CUA_HOME", "/tmp/cua-home-test");

        assert_eq!(cua_home().unwrap(), PathBuf::from("/tmp/cua-home-test"));
        assert_eq!(
            config_env_path().unwrap(),
            PathBuf::from("/tmp/cua-home-test/config/env")
        );
        assert_eq!(
            legacy_config_env_path().unwrap(),
            PathBuf::from("/tmp/cua-home-test/.env")
        );
        assert_eq!(
            profile_token_path("p").unwrap(),
            PathBuf::from("/tmp/cua-home-test/profiles/p/http.token")
        );
        assert_eq!(
            profile_socket_path("p").unwrap(),
            PathBuf::from("/tmp/cua-home-test/profiles/p/daemon.sock")
        );
        assert_eq!(
            profile_chat_db_path("p").unwrap(),
            PathBuf::from("/tmp/cua-home-test/profiles/p/chat.db")
        );
        assert_eq!(
            profile_ctx_dir("p").unwrap(),
            PathBuf::from("/tmp/cua-home-test/profiles/p/ctx")
        );
        assert_eq!(
            profile_voice_trace_path("p").unwrap(),
            PathBuf::from("/tmp/cua-home-test/profiles/p/traces/voice.jsonl")
        );
        assert_eq!(
            profile_daemon_trace_dir("p").unwrap(),
            PathBuf::from("/tmp/cua-home-test/profiles/p/traces/daemon")
        );
        assert_eq!(
            cua_bin_path("ctx").unwrap(),
            PathBuf::from("/tmp/cua-home-test/bin/ctx")
        );

        if let Some(old) = old {
            std::env::set_var("CUA_HOME", old);
        } else {
            std::env::remove_var("CUA_HOME");
        }
    }

    #[test]
    fn config_inventory_redacts_token_and_reports_paths() {
        let temp_root =
            std::env::temp_dir().join(format!("cua-config-inventory-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(temp_root.join("config")).unwrap();
        std::fs::create_dir_all(temp_root.join("profiles/default")).unwrap();
        std::fs::write(
            temp_root.join("config/env"),
            "OPENROUTER_API_KEY=env-secret",
        )
        .unwrap();
        std::fs::write(
            temp_root.join("profiles/default/http.token"),
            "bearer-secret",
        )
        .unwrap();

        let old = std::env::var_os("CUA_HOME");
        std::env::set_var("CUA_HOME", &temp_root);
        let inventory = ConfigInventory::for_profile("default").unwrap();

        assert_eq!(inventory.cua_home, temp_root.display().to_string());
        assert_eq!(inventory.config_env_present, true);
        assert_eq!(inventory.legacy_config_env_present, false);
        assert_eq!(inventory.migration_state, ConfigMigrationState::Current);
        assert_eq!(inventory.profile_token_present, true);
        let encoded = serde_json::to_string(&inventory).unwrap();
        assert!(!encoded.contains("env-secret"));
        assert!(!encoded.contains("bearer-secret"));

        if let Some(old) = old {
            std::env::set_var("CUA_HOME", old);
        } else {
            std::env::remove_var("CUA_HOME");
        }
        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn attestation_schemas_are_exported() {
        let bundle = schema_bundle();

        for key in [
            "AttestationChallengeRequest",
            "AttestationChallenge",
            "MachineIdentity",
            "RuntimeIdentityClaims",
            "MachineAttestation",
        ] {
            assert!(bundle.schemas.contains_key(key), "missing schema {key}");
        }
    }

    #[test]
    fn machine_identity_uses_salted_hash_without_raw_hardware_ids() {
        let identity = MachineIdentity {
            schema_version: SCHEMA_VERSION.to_string(),
            machine_key_id: "key_123".to_string(),
            machine_public_key: "pub_abc".to_string(),
            machine_id_hash: "audience_salted_hash".to_string(),
            created_wall_ms: 42,
            key_backend: MachineKeyBackend::Keychain,
        };

        let value = serde_json::to_value(identity).unwrap();
        assert_eq!(value["machine_id_hash"], "audience_salted_hash");
        assert!(value.get("serial").is_none());
        assert!(value.get("platform_uuid").is_none());
        assert!(value.get("hardware_uuid").is_none());
    }

    #[test]
    fn machine_attestation_serializes_runtime_claims() {
        let profile = ProfilePolicy {
            schema_version: SCHEMA_VERSION.to_string(),
            name: "default".to_string(),
            mode: RuntimeMode::Supervised,
            capabilities: CapabilityManifest::default(),
            created_wall_ms: 1,
            expires_wall_ms: None,
            active: true,
        };
        let claims = RuntimeIdentityClaims {
            schema_version: SCHEMA_VERSION.to_string(),
            runtime_name: "cua".to_string(),
            runtime_version: "0.1.0".to_string(),
            daemon_pid: 123,
            profile: "default".to_string(),
            socket_path: "/tmp/cua.sock".to_string(),
            http_addr: "127.0.0.1:0".to_string(),
            bundle_id: Some("app.cua".to_string()),
            designated_requirement: None,
            code_signature_summary: None,
            binary_sha256: Some("sha256".to_string()),
            permissions: PermissionReport::conservative_unknown(),
            active_profile: profile,
            safety_state: SafetyState::Running,
            session_id: Some("session".to_string()),
        };
        let attestation = MachineAttestation {
            schema_version: SCHEMA_VERSION.to_string(),
            challenge: AttestationChallenge {
                schema_version: SCHEMA_VERSION.to_string(),
                challenge_id: "challenge".to_string(),
                nonce: "nonce".to_string(),
                audience: "quilt-cloud".to_string(),
                issued_wall_ms: 1,
                expires_wall_ms: 2,
            },
            identity: MachineIdentity {
                schema_version: SCHEMA_VERSION.to_string(),
                machine_key_id: "key".to_string(),
                machine_public_key: "public".to_string(),
                machine_id_hash: "hash".to_string(),
                created_wall_ms: 1,
                key_backend: MachineKeyBackend::FileForTests,
            },
            claims,
            signature_algorithm: AttestationSignatureAlgorithm::Ed25519,
            signature: "signature".to_string(),
            signed_wall_ms: 2,
        };

        let value = serde_json::to_value(attestation).unwrap();
        assert_eq!(value["challenge"]["audience"], "quilt-cloud");
        assert_eq!(value["claims"]["runtime_name"], "cua");
        assert_eq!(value["signature_algorithm"], "ed25519");
    }
}
