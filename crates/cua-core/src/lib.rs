use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;
use schemars::schema_for;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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

pub fn profile_scratchpads_dir(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(profile_dir(profile)?.join("scratchpads"))
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

pub fn machine_identity_path() -> anyhow::Result<PathBuf> {
    Ok(identity_dir()?.join("machine.json"))
}

pub fn machine_current_key_path() -> anyhow::Result<PathBuf> {
    Ok(identity_dir()?.join("keys").join("current.json"))
}

pub fn machine_previous_keys_dir() -> anyhow::Result<PathBuf> {
    Ok(identity_dir()?.join("keys").join("previous"))
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
    pub scratchpads: String,
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
        let scratchpads = profile_scratchpads_dir(profile)?;
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
            scratchpads: path_string(scratchpads),
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

pub fn production_machine_key_backend() -> MachineKeyBackend {
    MachineKeyBackend::Keychain
}

pub fn cloud_enrollment_owner() -> &'static str {
    "local_cua_client"
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct MachineIdentityStatus {
    pub schema_version: String,
    pub identity: MachineIdentity,
    pub metadata_path: String,
    pub key_path: String,
    pub previous_keys_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AttestationSignRequest {
    pub schema_version: String,
    pub audience: String,
    pub nonce: String,
    pub challenge_id: Option<String>,
    pub profile: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct AttestationVerifyRequest {
    pub schema_version: String,
    pub audience: String,
    pub attestation: MachineAttestation,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct AttestationVerifyResult {
    pub schema_version: String,
    pub accepted: bool,
    pub reason: String,
    pub machine_key_id: String,
    pub audience: String,
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
#[serde(rename_all = "snake_case")]
pub enum InboundDeliveryMethod {
    LocalHttp,
    UnixSocket,
    Webhook,
    Sdk,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InboundReplyMode {
    Ui,
    Poll,
    Webhook,
}

impl Default for InboundReplyMode {
    fn default() -> Self {
        Self::Ui
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InboundMessageState {
    Received,
    Accepted,
    Running,
    Done,
    Failed,
    Expired,
    Duplicate,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct InboundMessageRequest {
    pub schema_version: String,
    pub idempotency_key: String,
    pub source: String,
    pub text: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub reply_mode: InboundReplyMode,
    pub reply_url: Option<String>,
    pub ttl_ms: Option<i64>,
    #[serde(default)]
    pub attestation: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct InboundMessage {
    pub schema_version: String,
    pub sequence: u64,
    pub message_id: String,
    pub idempotency_key: String,
    pub source: String,
    pub text: String,
    pub payload: serde_json::Value,
    pub reply_mode: InboundReplyMode,
    pub reply_url: Option<String>,
    pub delivery_method: InboundDeliveryMethod,
    pub received_wall_ms: i64,
    pub expires_wall_ms: Option<i64>,
    pub attestation: Option<serde_json::Value>,
    pub duplicate_of: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct InboundStatus {
    pub schema_version: String,
    pub message_id: String,
    pub state: InboundMessageState,
    pub message: InboundMessage,
    pub reply: Option<String>,
    pub error: Option<String>,
    pub updated_wall_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct WebhookSubscribeRequest {
    pub schema_version: String,
    pub source: String,
    pub shared_secret: Option<String>,
    pub reply_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct WebhookSourceStatus {
    pub schema_version: String,
    pub source: String,
    pub configured: bool,
    pub requires_signature: bool,
    pub reply_url: Option<String>,
    pub updated_wall_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScratchpadKind {
    Durable,
    Ephemeral,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ScratchpadEntry {
    pub schema_version: String,
    pub profile: String,
    pub name: String,
    pub kind: ScratchpadKind,
    pub text: String,
    pub created_wall_ms: i64,
    pub updated_wall_ms: i64,
    pub expires_wall_ms: Option<i64>,
    pub bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ScratchpadSummary {
    pub schema_version: String,
    pub profile: String,
    pub name: String,
    pub kind: ScratchpadKind,
    pub updated_wall_ms: i64,
    pub expires_wall_ms: Option<i64>,
    pub bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ScratchpadWriteRequest {
    pub schema_version: String,
    pub name: String,
    pub text: String,
    #[serde(default = "default_true")]
    pub durable: bool,
    #[serde(default)]
    pub append: bool,
    pub ttl_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ScratchpadReadRequest {
    pub schema_version: String,
    pub name: String,
    #[serde(default)]
    pub durable: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ScratchpadListRequest {
    pub schema_version: String,
    #[serde(default = "default_true")]
    pub include_durable: bool,
    #[serde(default = "default_true")]
    pub include_ephemeral: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ScratchpadListResult {
    pub schema_version: String,
    pub profile: String,
    pub entries: Vec<ScratchpadSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ScratchpadDeleteRequest {
    pub schema_version: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub durable: bool,
    #[serde(default = "default_true")]
    pub ephemeral: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ScratchpadDeleteResult {
    pub schema_version: String,
    pub profile: String,
    pub deleted: usize,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredMachineMetadata {
    schema_version: String,
    created_wall_ms: i64,
    current_key_id: String,
    previous_key_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredMachineKey {
    schema_version: String,
    key_id: String,
    signing_key_base64: String,
    created_wall_ms: i64,
    revoked_wall_ms: Option<i64>,
}

pub fn load_or_create_machine_identity(audience: &str) -> anyhow::Result<MachineIdentityStatus> {
    load_or_create_machine_identity_at(
        audience,
        &machine_identity_path()?,
        &machine_current_key_path()?,
        &machine_previous_keys_dir()?,
    )
}

pub fn rotate_machine_identity(audience: &str) -> anyhow::Result<MachineIdentityStatus> {
    rotate_machine_identity_at(
        audience,
        &machine_identity_path()?,
        &machine_current_key_path()?,
        &machine_previous_keys_dir()?,
    )
}

pub fn sign_machine_attestation(
    challenge: AttestationChallenge,
    claims: RuntimeIdentityClaims,
) -> anyhow::Result<MachineAttestation> {
    let identity = load_or_create_machine_identity(&challenge.audience)?.identity;
    let key = read_or_create_machine_key(&machine_identity_path()?, &machine_current_key_path()?)?;
    let mut attestation = MachineAttestation {
        schema_version: SCHEMA_VERSION.to_string(),
        challenge,
        identity,
        claims,
        signature_algorithm: AttestationSignatureAlgorithm::Ed25519,
        signature: String::new(),
        signed_wall_ms: now_wall_ms(),
    };
    let signing_key = signing_key_from_stored(&key)?;
    let signature = signing_key.sign(&attestation_payload(&attestation)?);
    attestation.signature = URL_SAFE_NO_PAD.encode(signature.to_bytes());
    Ok(attestation)
}

pub fn verify_machine_attestation(
    attestation: &MachineAttestation,
    audience: &str,
    now_ms: i64,
) -> anyhow::Result<AttestationVerifyResult> {
    if attestation.schema_version != SCHEMA_VERSION
        || attestation.challenge.schema_version != SCHEMA_VERSION
        || attestation.identity.schema_version != SCHEMA_VERSION
        || attestation.claims.schema_version != SCHEMA_VERSION
    {
        return Ok(attestation_rejected(
            attestation,
            audience,
            "schema_version_mismatch",
        ));
    }
    if attestation.challenge.audience != audience {
        return Ok(attestation_rejected(
            attestation,
            audience,
            "audience_mismatch",
        ));
    }
    if now_ms > attestation.challenge.expires_wall_ms {
        return Ok(attestation_rejected(
            attestation,
            audience,
            "challenge_expired",
        ));
    }
    if attestation.identity.machine_id_hash
        != salted_machine_id_hash(&attestation.identity.machine_public_key, audience)
    {
        return Ok(attestation_rejected(
            attestation,
            audience,
            "machine_id_hash_mismatch",
        ));
    }
    if attestation.signature_algorithm != AttestationSignatureAlgorithm::Ed25519 {
        return Ok(attestation_rejected(
            attestation,
            audience,
            "signature_algorithm_unsupported",
        ));
    }

    let public_bytes = URL_SAFE_NO_PAD
        .decode(&attestation.identity.machine_public_key)
        .map_err(|error| anyhow::anyhow!("invalid machine public key base64: {error}"))?;
    let public_array: [u8; 32] = public_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid machine public key length"))?;
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(&attestation.signature)
        .map_err(|error| anyhow::anyhow!("invalid attestation signature base64: {error}"))?;
    let signature_array: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid attestation signature length"))?;
    let verifying_key = VerifyingKey::from_bytes(&public_array)?;
    let signature = Signature::from_bytes(&signature_array);
    match verifying_key.verify(&attestation_payload(attestation)?, &signature) {
        Ok(()) => Ok(AttestationVerifyResult {
            schema_version: SCHEMA_VERSION.to_string(),
            accepted: true,
            reason: "ok".to_string(),
            machine_key_id: attestation.identity.machine_key_id.clone(),
            audience: audience.to_string(),
        }),
        Err(_) => Ok(attestation_rejected(
            attestation,
            audience,
            "signature_invalid",
        )),
    }
}

fn load_or_create_machine_identity_at(
    audience: &str,
    metadata_path: &Path,
    key_path: &Path,
    previous_keys_dir: &Path,
) -> anyhow::Result<MachineIdentityStatus> {
    let key = read_or_create_machine_key(metadata_path, key_path)?;
    Ok(machine_identity_status(
        audience,
        &key,
        metadata_path,
        key_path,
        previous_keys_dir,
    )?)
}

fn rotate_machine_identity_at(
    audience: &str,
    metadata_path: &Path,
    key_path: &Path,
    previous_keys_dir: &Path,
) -> anyhow::Result<MachineIdentityStatus> {
    if key_path.exists() {
        let current = read_machine_key(key_path)?;
        std::fs::create_dir_all(previous_keys_dir)?;
        let previous_path = previous_keys_dir.join(format!("{}.json", current.key_id));
        std::fs::rename(key_path, previous_path)?;
    }
    let key = create_machine_key(metadata_path, key_path, previous_keys_dir)?;
    machine_identity_status(audience, &key, metadata_path, key_path, previous_keys_dir)
}

fn read_or_create_machine_key(
    metadata_path: &Path,
    key_path: &Path,
) -> anyhow::Result<StoredMachineKey> {
    if key_path.exists() {
        return read_machine_key(key_path);
    }
    create_machine_key(
        metadata_path,
        key_path,
        &key_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("previous"),
    )
}

fn create_machine_key(
    metadata_path: &Path,
    key_path: &Path,
    previous_keys_dir: &Path,
) -> anyhow::Result<StoredMachineKey> {
    let signing_key = SigningKey::generate(&mut OsRng);
    let created_wall_ms = now_wall_ms();
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes());
    let key_id = machine_key_id(&public_key);
    let key = StoredMachineKey {
        schema_version: SCHEMA_VERSION.to_string(),
        key_id: key_id.clone(),
        signing_key_base64: URL_SAFE_NO_PAD.encode(signing_key.to_bytes()),
        created_wall_ms,
        revoked_wall_ms: None,
    };
    write_json_private(key_path, &key)?;
    let previous_key_ids = previous_key_ids(previous_keys_dir)?;
    let metadata = StoredMachineMetadata {
        schema_version: SCHEMA_VERSION.to_string(),
        created_wall_ms,
        current_key_id: key_id,
        previous_key_ids,
    };
    write_json_public(metadata_path, &metadata)?;
    Ok(key)
}

fn read_machine_key(path: &Path) -> anyhow::Result<StoredMachineKey> {
    let bytes = std::fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn machine_identity_status(
    audience: &str,
    key: &StoredMachineKey,
    metadata_path: &Path,
    key_path: &Path,
    previous_keys_dir: &Path,
) -> anyhow::Result<MachineIdentityStatus> {
    let signing_key = signing_key_from_stored(key)?;
    let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes());
    Ok(MachineIdentityStatus {
        schema_version: SCHEMA_VERSION.to_string(),
        identity: MachineIdentity {
            schema_version: SCHEMA_VERSION.to_string(),
            machine_key_id: key.key_id.clone(),
            machine_public_key: public_key.clone(),
            machine_id_hash: salted_machine_id_hash(&public_key, audience),
            created_wall_ms: key.created_wall_ms,
            key_backend: MachineKeyBackend::FileForTests,
        },
        metadata_path: metadata_path.display().to_string(),
        key_path: key_path.display().to_string(),
        previous_keys_dir: previous_keys_dir.display().to_string(),
    })
}

fn signing_key_from_stored(key: &StoredMachineKey) -> anyhow::Result<SigningKey> {
    let bytes = URL_SAFE_NO_PAD
        .decode(&key.signing_key_base64)
        .map_err(|error| anyhow::anyhow!("invalid stored machine key base64: {error}"))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid stored machine key length"))?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn machine_key_id(public_key: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"cua-machine-key-id:v1:");
    hasher.update(public_key.as_bytes());
    let digest = hasher.finalize();
    format!("ed25519_{}", URL_SAFE_NO_PAD.encode(&digest[..12]))
}

fn salted_machine_id_hash(public_key: &str, audience: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"cua-machine-id:v1:");
    hasher.update(audience.as_bytes());
    hasher.update(b":");
    hasher.update(public_key.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

fn attestation_payload(attestation: &MachineAttestation) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&serde_json::json!({
        "schema_version": attestation.schema_version,
        "challenge": attestation.challenge,
        "identity": attestation.identity,
        "claims": attestation.claims,
        "signature_algorithm": attestation.signature_algorithm,
    }))?)
}

fn attestation_rejected(
    attestation: &MachineAttestation,
    audience: &str,
    reason: &str,
) -> AttestationVerifyResult {
    AttestationVerifyResult {
        schema_version: SCHEMA_VERSION.to_string(),
        accepted: false,
        reason: reason.to_string(),
        machine_key_id: attestation.identity.machine_key_id.clone(),
        audience: audience.to_string(),
    }
}

fn previous_key_ids(previous_keys_dir: &Path) -> anyhow::Result<Vec<String>> {
    if !previous_keys_dir.exists() {
        return Ok(Vec::new());
    }
    let mut keys = Vec::new();
    for entry in std::fs::read_dir(previous_keys_dir)? {
        let entry = entry?;
        if let Some(stem) = entry.path().file_stem().and_then(|stem| stem.to_str()) {
            keys.push(stem.to_string());
        }
    }
    keys.sort();
    Ok(keys)
}

fn write_json_public<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn write_json_private<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    write_json_public(path, value)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
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
pub struct SessionHeartbeatRequest {
    pub schema_version: String,
    pub session_id: String,
    pub ttl_ms: Option<i64>,
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
    pub duration_ms: Option<u64>,
    pub queue_depth: Option<usize>,
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
        "MachineIdentityStatus".to_string(),
        serde_json::json!(schema_for!(MachineIdentityStatus)),
    );
    schemas.insert(
        "AttestationSignRequest".to_string(),
        serde_json::json!(schema_for!(AttestationSignRequest)),
    );
    schemas.insert(
        "AttestationVerifyRequest".to_string(),
        serde_json::json!(schema_for!(AttestationVerifyRequest)),
    );
    schemas.insert(
        "AttestationVerifyResult".to_string(),
        serde_json::json!(schema_for!(AttestationVerifyResult)),
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
        "InboundDeliveryMethod".to_string(),
        serde_json::json!(schema_for!(InboundDeliveryMethod)),
    );
    schemas.insert(
        "InboundReplyMode".to_string(),
        serde_json::json!(schema_for!(InboundReplyMode)),
    );
    schemas.insert(
        "InboundMessageState".to_string(),
        serde_json::json!(schema_for!(InboundMessageState)),
    );
    schemas.insert(
        "InboundMessageRequest".to_string(),
        serde_json::json!(schema_for!(InboundMessageRequest)),
    );
    schemas.insert(
        "InboundMessage".to_string(),
        serde_json::json!(schema_for!(InboundMessage)),
    );
    schemas.insert(
        "InboundStatus".to_string(),
        serde_json::json!(schema_for!(InboundStatus)),
    );
    schemas.insert(
        "WebhookSubscribeRequest".to_string(),
        serde_json::json!(schema_for!(WebhookSubscribeRequest)),
    );
    schemas.insert(
        "WebhookSourceStatus".to_string(),
        serde_json::json!(schema_for!(WebhookSourceStatus)),
    );
    schemas.insert(
        "ScratchpadKind".to_string(),
        serde_json::json!(schema_for!(ScratchpadKind)),
    );
    schemas.insert(
        "ScratchpadEntry".to_string(),
        serde_json::json!(schema_for!(ScratchpadEntry)),
    );
    schemas.insert(
        "ScratchpadSummary".to_string(),
        serde_json::json!(schema_for!(ScratchpadSummary)),
    );
    schemas.insert(
        "ScratchpadWriteRequest".to_string(),
        serde_json::json!(schema_for!(ScratchpadWriteRequest)),
    );
    schemas.insert(
        "ScratchpadReadRequest".to_string(),
        serde_json::json!(schema_for!(ScratchpadReadRequest)),
    );
    schemas.insert(
        "ScratchpadListRequest".to_string(),
        serde_json::json!(schema_for!(ScratchpadListRequest)),
    );
    schemas.insert(
        "ScratchpadListResult".to_string(),
        serde_json::json!(schema_for!(ScratchpadListResult)),
    );
    schemas.insert(
        "ScratchpadDeleteRequest".to_string(),
        serde_json::json!(schema_for!(ScratchpadDeleteRequest)),
    );
    schemas.insert(
        "ScratchpadDeleteResult".to_string(),
        serde_json::json!(schema_for!(ScratchpadDeleteResult)),
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
        "SessionHeartbeatRequest".to_string(),
        serde_json::json!(schema_for!(SessionHeartbeatRequest)),
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
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
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
            "MachineIdentityStatus",
            "AttestationSignRequest",
            "AttestationVerifyRequest",
            "AttestationVerifyResult",
            "RuntimeIdentityClaims",
            "MachineAttestation",
        ] {
            assert!(bundle.schemas.contains_key(key), "missing schema {key}");
        }
    }

    #[test]
    fn production_policy_decisions_are_canonical() {
        assert_eq!(
            production_machine_key_backend(),
            MachineKeyBackend::Keychain
        );
        assert_eq!(cloud_enrollment_owner(), "local_cua_client");
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

    #[test]
    fn machine_identity_persists_and_rotates_key_files() {
        let temp_root = std::env::temp_dir().join(format!("cua-identity-test-{}", Uuid::new_v4()));
        let metadata = temp_root.join("machine.json");
        let current = temp_root.join("keys/current.json");
        let previous = temp_root.join("keys/previous");

        let first =
            load_or_create_machine_identity_at("audience-a", &metadata, &current, &previous)
                .unwrap();
        let second =
            load_or_create_machine_identity_at("audience-a", &metadata, &current, &previous)
                .unwrap();
        assert_eq!(
            first.identity.machine_key_id,
            second.identity.machine_key_id
        );
        assert!(metadata.exists());
        assert!(current.exists());

        let rotated =
            rotate_machine_identity_at("audience-a", &metadata, &current, &previous).unwrap();
        assert_ne!(
            first.identity.machine_key_id,
            rotated.identity.machine_key_id
        );
        assert!(previous
            .join(format!("{}.json", first.identity.machine_key_id))
            .exists());

        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn machine_attestation_signs_and_rejects_bad_audience() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp_root = std::env::temp_dir().join(format!("cua-attest-test-{}", Uuid::new_v4()));
        let old = std::env::var_os("CUA_HOME");
        std::env::set_var("CUA_HOME", &temp_root);

        let challenge = AttestationChallenge {
            schema_version: SCHEMA_VERSION.to_string(),
            challenge_id: "challenge".to_string(),
            nonce: "nonce".to_string(),
            audience: "audience-a".to_string(),
            issued_wall_ms: 1,
            expires_wall_ms: now_wall_ms() + 60_000,
        };
        let claims = RuntimeIdentityClaims {
            schema_version: SCHEMA_VERSION.to_string(),
            runtime_name: "cua".to_string(),
            runtime_version: "0.1.0".to_string(),
            daemon_pid: 123,
            profile: "default".to_string(),
            socket_path: "/tmp/cua.sock".to_string(),
            http_addr: "127.0.0.1:0".to_string(),
            bundle_id: None,
            designated_requirement: None,
            code_signature_summary: None,
            binary_sha256: None,
            permissions: PermissionReport::conservative_unknown(),
            active_profile: ProfilePolicy {
                schema_version: SCHEMA_VERSION.to_string(),
                name: "default".to_string(),
                mode: RuntimeMode::Supervised,
                capabilities: CapabilityManifest::default(),
                created_wall_ms: 1,
                expires_wall_ms: None,
                active: true,
            },
            safety_state: SafetyState::Running,
            session_id: None,
        };
        let attestation = sign_machine_attestation(challenge, claims).unwrap();
        let accepted =
            verify_machine_attestation(&attestation, "audience-a", now_wall_ms()).unwrap();
        assert!(accepted.accepted);
        assert_eq!(accepted.reason, "ok");

        let rejected =
            verify_machine_attestation(&attestation, "audience-b", now_wall_ms()).unwrap();
        assert!(!rejected.accepted);
        assert_eq!(rejected.reason, "audience_mismatch");

        if let Some(old) = old {
            std::env::set_var("CUA_HOME", old);
        } else {
            std::env::remove_var("CUA_HOME");
        }
        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn machine_attestation_rejects_expired_challenge() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp_root =
            std::env::temp_dir().join(format!("cua-attest-expired-test-{}", Uuid::new_v4()));
        let old = std::env::var_os("CUA_HOME");
        std::env::set_var("CUA_HOME", &temp_root);

        let now = now_wall_ms();
        let challenge = AttestationChallenge {
            schema_version: SCHEMA_VERSION.to_string(),
            challenge_id: "expired-challenge".to_string(),
            nonce: "nonce".to_string(),
            audience: "audience-a".to_string(),
            issued_wall_ms: now - 120_000,
            expires_wall_ms: now - 60_000,
        };
        let claims = RuntimeIdentityClaims {
            schema_version: SCHEMA_VERSION.to_string(),
            runtime_name: "cua".to_string(),
            runtime_version: "0.1.0".to_string(),
            daemon_pid: 123,
            profile: "default".to_string(),
            socket_path: "/tmp/cua.sock".to_string(),
            http_addr: "127.0.0.1:0".to_string(),
            bundle_id: None,
            designated_requirement: None,
            code_signature_summary: None,
            binary_sha256: None,
            permissions: PermissionReport::conservative_unknown(),
            active_profile: ProfilePolicy {
                schema_version: SCHEMA_VERSION.to_string(),
                name: "default".to_string(),
                mode: RuntimeMode::Supervised,
                capabilities: CapabilityManifest::default(),
                created_wall_ms: 1,
                expires_wall_ms: None,
                active: true,
            },
            safety_state: SafetyState::Running,
            session_id: None,
        };
        let attestation = sign_machine_attestation(challenge, claims).unwrap();
        let rejected =
            verify_machine_attestation(&attestation, "audience-a", now_wall_ms()).unwrap();

        assert!(!rejected.accepted);
        assert_eq!(rejected.reason, "challenge_expired");

        if let Some(old) = old {
            std::env::set_var("CUA_HOME", old);
        } else {
            std::env::remove_var("CUA_HOME");
        }
        let _ = std::fs::remove_dir_all(temp_root);
    }

    #[test]
    fn visual_session_request_carries_lifecycle_and_backpressure_options() {
        let request = VisualSessionRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            max_width: Some(640),
            fps: Some(20),
            include_bytes: false,
            duration_ms: Some(1_500),
            queue_depth: Some(1),
        };

        let value = serde_json::to_value(request).unwrap();

        assert_eq!(value["duration_ms"], 1_500);
        assert_eq!(value["queue_depth"], 1);
    }
}
