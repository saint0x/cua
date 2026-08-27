use anyhow::Context;
use base64::Engine;
use clap::{Args, Parser, Subcommand};
use cua_capture::{CaptureRequest, FrameBus, SyntheticCaptureBackend};
use cua_client::CuaClient;
use cua_core::{
    config_env_path, profile_ctx_dir, profile_socket_path, profile_token_path, schema_bundle,
    CapabilityManifest, ClipboardReadRequest, ClipboardWriteRequest, DesktopContextSnapshot,
    FrameEncoding, FramePayload, InputAction, MouseButton, RuntimeMode, RuntimeSessionRole,
    SessionCancelRequest, SessionLeaseRequest, UiIslandRequest, UiIslandState, UiMode,
    UiModeRequest, UiReplyRequest, UiStepRequest, SCHEMA_VERSION,
};
use cua_model::{run_eval_report, EvalConfig};
use cua_trace::{ActionTurnRecord, TraceRecord, TraceWriter};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, Parser)]
#[command(name = "cua", version, about = "CLI-first local computer-use runtime")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:8765", global = true)]
    server_addr: SocketAddr,
    #[arg(long, default_value = "default", global = true)]
    profile: String,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve(ServeArgs),
    Status(JsonFlag),
    Doctor(JsonFlag),
    Permissions {
        #[command(subcommand)]
        command: PermissionCommand,
    },
    Perf {
        #[command(subcommand)]
        command: PerfCommand,
    },
    Context(ContextArgs),
    Run(RunebookArgs),
    Manifest(JsonFlag),
    Metrics(JsonFlag),
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Events(EventsArgs),
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    Stream(StreamArgs),
    Ui {
        #[command(subcommand)]
        command: UiCommand,
    },
    Screenshot(ScreenshotArgs),
    WindowCapture(WindowCaptureArgs),
    Observe(JsonFlag),
    Mouse {
        #[command(subcommand)]
        command: MouseCommand,
    },
    Key {
        #[command(subcommand)]
        command: KeyCommand,
    },
    Shell(ShellArgs),
    Aegis(AegisArgs),
    Ctx(CtxArgs),
    Clipboard {
        #[command(subcommand)]
        command: ClipboardCommand,
    },
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    Schema {
        #[command(subcommand)]
        command: SchemaCommand,
    },
    Trace {
        #[command(subcommand)]
        command: TraceCommand,
    },
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    Pause(JsonFlag),
    Resume(JsonFlag),
    KillSwitch(JsonFlag),
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long, default_value = "127.0.0.1:8765")]
    addr: SocketAddr,
    #[arg(long)]
    allow_lan: bool,
    #[arg(long, value_enum, default_value_t = UiModeArg::Headful)]
    hud_mode: UiModeArg,
}

#[derive(Debug, Args)]
struct JsonFlag {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct EventsArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    after: Option<u64>,
}

#[derive(Debug, Args)]
struct StreamArgs {
    #[arg(long)]
    unix: bool,
    #[arg(long, default_value_t = 3)]
    frames: usize,
    #[arg(long, default_value_t = 10)]
    fps: u32,
    #[arg(long, default_value_t = 1280)]
    max_width: u32,
    #[arg(long)]
    include_bytes: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum PermissionCommand {
    Status(JsonFlag),
    Preflight(JsonFlag),
    RequestAccessibility(JsonFlag),
}

#[derive(Debug, Subcommand)]
enum PerfCommand {
    Live(JsonFlag),
    Bench(PerfBenchArgs),
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Status(JsonFlag),
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    Acquire {
        session_id: String,
        #[arg(long)]
        client_name: Option<String>,
        #[arg(long, value_enum, default_value_t = SessionRoleArg::Owner)]
        role: SessionRoleArg,
        #[arg(long)]
        ttl_ms: Option<i64>,
        #[arg(long)]
        json: bool,
    },
    Cancel {
        session_id: String,
        #[arg(long)]
        target_session_id: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, clap::ValueEnum, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionRoleArg {
    Owner,
    Observer,
}

impl From<SessionRoleArg> for RuntimeSessionRole {
    fn from(value: SessionRoleArg) -> Self {
        match value {
            SessionRoleArg::Owner => RuntimeSessionRole::Owner,
            SessionRoleArg::Observer => RuntimeSessionRole::Observer,
        }
    }
}

#[derive(Debug, Args)]
struct PerfBenchArgs {
    #[arg(value_enum)]
    target: PerfBenchTarget,
    #[arg(long, default_value_t = 5)]
    iterations: usize,
    #[arg(long, default_value_t = 1)]
    warmup: usize,
    #[arg(long)]
    budget_ms: Option<u128>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct ContextArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    include_bytes: bool,
    #[arg(long)]
    force_fresh: bool,
    #[arg(long, default_value_t = 640)]
    max_width: u32,
}

#[derive(Debug, Args)]
struct RunebookArgs {
    file: PathBuf,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    trace_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum PerfBenchTarget {
    Screenshot,
    Stream,
    Input,
    ModelPrep,
}

#[derive(Debug, Args)]
struct ScreenshotArgs {
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    force_fresh: bool,
    #[arg(long, default_value_t = 1280)]
    max_width: u32,
}

#[derive(Debug, Args)]
struct WindowCaptureArgs {
    window_id: u32,
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    json: bool,
    #[arg(long, default_value_t = 1280)]
    max_width: u32,
}

#[derive(Debug, Subcommand)]
enum MouseCommand {
    Move {
        x: i32,
        y: i32,
        #[arg(long, default_value_t = 120)]
        duration_ms: u64,
    },
    Click {
        x: i32,
        y: i32,
        #[arg(long, default_value = "left")]
        button: String,
        #[arg(long, default_value_t = 1)]
        count: u8,
    },
}

#[derive(Debug, Subcommand)]
enum KeyCommand {
    Press { combo: String },
    Type { text: String },
    Paste { text: String },
}

#[derive(Debug, Args)]
struct ShellArgs {
    command: String,
    #[arg(long, default_value_t = 5_000)]
    timeout_ms: u64,
}

#[derive(Debug, Args)]
struct AegisArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    args: Vec<String>,
    #[arg(long, default_value_t = 15_000)]
    timeout_ms: u64,
}

#[derive(Debug, Args)]
struct CtxArgs {
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    args: Vec<String>,
    #[arg(long, default_value_t = 5_000)]
    timeout_ms: u64,
}

#[derive(Debug, Subcommand)]
enum ClipboardCommand {
    Read {
        #[arg(long)]
        allow_sensitive: bool,
        #[arg(long)]
        json: bool,
    },
    Write {
        text: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum UiCommand {
    Step {
        label: String,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        tool: Option<String>,
        #[arg(long)]
        step_index: Option<u16>,
        #[arg(long)]
        step_total: Option<u16>,
        #[arg(long)]
        ttl_ms: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    Reply {
        text: String,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        ttl_ms: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    Mode {
        #[arg(value_enum)]
        mode: UiModeArg,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Island {
        #[arg(value_enum)]
        state: UiIslandStateArg,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Debug, clap::ValueEnum)]
enum UiIslandStateArg {
    Expanded,
    Collapsed,
    Toggle,
}

impl From<UiIslandStateArg> for UiIslandState {
    fn from(value: UiIslandStateArg) -> Self {
        match value {
            UiIslandStateArg::Expanded => UiIslandState::Expanded,
            UiIslandStateArg::Collapsed => UiIslandState::Collapsed,
            UiIslandStateArg::Toggle => UiIslandState::Toggle,
        }
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum UiModeArg {
    Headful,
    Headless,
}

impl From<&UiModeArg> for UiMode {
    fn from(value: &UiModeArg) -> Self {
        match *value {
            UiModeArg::Headful => UiMode::Headful,
            UiModeArg::Headless => UiMode::Headless,
        }
    }
}

impl From<UiModeArg> for UiMode {
    fn from(value: UiModeArg) -> Self {
        UiMode::from(&value)
    }
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    Eval {
        #[arg(long)]
        live: bool,
        #[arg(long)]
        max_calls: Option<usize>,
        #[arg(long)]
        max_output_tokens: Option<u32>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum SchemaCommand {
    Export {
        #[arg(long)]
        out: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum TraceCommand {
    Start {
        dir: PathBuf,
    },
    Inspect {
        dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Verify {
        dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Replay {
        dir: PathBuf,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ProfileCommand {
    Create {
        name: String,
        #[arg(long, default_value = "observe")]
        mode: RuntimeModeArg,
        #[arg(long)]
        duration_ms: Option<i64>,
        #[arg(long)]
        clipboard: bool,
        #[arg(long)]
        json: bool,
    },
    Activate {
        #[arg(long)]
        json: bool,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum RuntimeModeArg {
    Observe,
    Supervised,
    Autonomous,
}

impl From<RuntimeModeArg> for RuntimeMode {
    fn from(value: RuntimeModeArg) -> Self {
        match value {
            RuntimeModeArg::Observe => RuntimeMode::Observe,
            RuntimeModeArg::Supervised => RuntimeMode::Supervised,
            RuntimeModeArg::Autonomous => RuntimeMode::Autonomous,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_cua_dotenv();
    tracing_subscriber::fmt().with_env_filter("info").init();
    let cli = Cli::parse();
    match cli.command {
        None => print_usage_and_status().await,
        Some(Command::Serve(args)) => {
            cua_daemon::serve(args.addr, cli.profile, args.allow_lan, args.hud_mode.into()).await
        }
        Some(Command::Status(flag)) => unix_get(&cli.profile, "status", flag.json).await,
        Some(Command::Doctor(flag)) => doctor(flag.json).await,
        Some(Command::Permissions { command }) => permissions(&cli.profile, command).await,
        Some(Command::Perf { command }) => perf(&cli.profile, command).await,
        Some(Command::Context(args)) => context(&cli.profile, args).await,
        Some(Command::Run(args)) => runebook_command(&cli.profile, args).await,
        Some(Command::Manifest(flag)) => unix_get(&cli.profile, "manifest", flag.json).await,
        Some(Command::Metrics(flag)) => unix_get(&cli.profile, "metrics", flag.json).await,
        Some(Command::Config { command }) => config(&cli.profile, command).await,
        Some(Command::Events(args)) => events(&cli.profile, args).await,
        Some(Command::Session { command }) => session(&cli.profile, command).await,
        Some(Command::Stream(args)) => stream(&cli.profile, args).await,
        Some(Command::Ui { command }) => ui(cli.server_addr, &cli.profile, command).await,
        Some(Command::Screenshot(args)) => screenshot(&cli.profile, args).await,
        Some(Command::WindowCapture(args)) => window_capture(&cli.profile, args).await,
        Some(Command::Observe(flag)) => unix_get(&cli.profile, "observe.desktop", flag.json).await,
        Some(Command::Mouse { command }) => daemon_input(&cli.profile, mouse_action(command)).await,
        Some(Command::Key { command }) => daemon_input(&cli.profile, key_action(command)).await,
        Some(Command::Shell(args)) => daemon_input(&cli.profile, shell_action(args)).await,
        Some(Command::Aegis(args)) => daemon_input(&cli.profile, aegis_action(args)).await,
        Some(Command::Ctx(args)) => {
            daemon_input(&cli.profile, ctx_action(args, &cli.profile)).await
        }
        Some(Command::Clipboard { command }) => clipboard(&cli.profile, command).await,
        Some(Command::Model { command }) => model(command).await,
        Some(Command::Schema { command }) => schema(command).await,
        Some(Command::Trace { command }) => trace(&cli.profile, command).await,
        Some(Command::Profile { command }) => profile(&cli.profile, command).await,
        Some(Command::Pause(flag)) => unix_get(&cli.profile, "control.pause", flag.json).await,
        Some(Command::Resume(flag)) => unix_get(&cli.profile, "control.resume", flag.json).await,
        Some(Command::KillSwitch(flag)) => {
            unix_get(&cli.profile, "control.kill_switch", flag.json).await
        }
    }
}

fn load_cua_dotenv() {
    dotenvy::dotenv().ok();
    if let Ok(path) = std::env::var("CUA_ENV_FILE") {
        load_dotenv_path(Path::new(&path));
    }
    if let Ok(path) = config_env_path() {
        load_dotenv_path(&path);
    }
}

fn load_dotenv_path(path: &Path) {
    let Ok(iter) = dotenvy::from_path_iter(path) else {
        return;
    };
    for item in iter.flatten() {
        let (key, value) = item;
        if std::env::var_os(&key).is_none() {
            std::env::set_var(key, value);
        }
    }
}

async fn print_usage_and_status() -> anyhow::Result<()> {
    println!("cua: CLI/profile-socket computer-use runtime");
    println!("usage: cua serve --addr 127.0.0.1:0 --hud-mode headful");
    println!("       cua status --json");
    println!("       cua manifest --json");
    println!("       cua metrics --json");
    println!("       cua events --json [--after <sequence>]");
    println!("       cua stream --unix --frames 3 --json");
    println!("       cua ui step <label> --step-index 2 --step-total 5 --json");
    println!("       cua ui reply <text> --json");
    println!("       cua ui mode headless|headful --json");
    println!("       cua perf live --json");
    println!("       cua context --json");
    println!("       cua screenshot --out /tmp/screen.png");
    println!("       cua clipboard read --allow-sensitive --json");
    Ok(())
}

async fn ui(_addr: SocketAddr, profile: &str, command: UiCommand) -> anyhow::Result<()> {
    match command {
        UiCommand::Step {
            label,
            source,
            task,
            tool,
            step_index,
            step_total,
            ttl_ms,
            json,
        } => {
            let value = unix_request_json(
                profile,
                "ui.step",
                Some(serde_json::to_value(UiStepRequest {
                    schema_version: SCHEMA_VERSION.to_string(),
                    label,
                    source,
                    task,
                    tool,
                    step_index,
                    step_total,
                    ttl_ms,
                })?),
            )
            .await?;
            print_json_value(&value, json)
        }
        UiCommand::Reply {
            text,
            source,
            ttl_ms,
            json,
        } => {
            let value = unix_request_json(
                profile,
                "ui.reply",
                Some(serde_json::to_value(UiReplyRequest {
                    schema_version: SCHEMA_VERSION.to_string(),
                    text,
                    source,
                    ttl_ms,
                })?),
            )
            .await?;
            print_json_value(&value, json)
        }
        UiCommand::Mode { mode, source, json } => {
            let value = unix_request_json(
                profile,
                "ui.mode",
                Some(serde_json::to_value(UiModeRequest {
                    schema_version: SCHEMA_VERSION.to_string(),
                    mode: mode.into(),
                    source,
                })?),
            )
            .await?;
            print_json_value(&value, json)
        }
        UiCommand::Island {
            state,
            source,
            json,
        } => {
            let value = unix_request_json(
                profile,
                "ui.island",
                Some(serde_json::to_value(UiIslandRequest {
                    schema_version: SCHEMA_VERSION.to_string(),
                    state: state.into(),
                    source,
                })?),
            )
            .await?;
            print_json_value(&value, json)
        }
    }
}

async fn unix_get(profile: &str, method: &str, json: bool) -> anyhow::Result<()> {
    let value = unix_request_json(profile, method, None).await?;
    print_json_value(&value, json)
}

async fn events(profile: &str, args: EventsArgs) -> anyhow::Result<()> {
    let value = match args.after {
        Some(sequence) => {
            unix_request_json(
                profile,
                "events.after",
                Some(serde_json::json!({ "after_sequence": sequence })),
            )
            .await?
        }
        None => unix_request_json(profile, "events.snapshot", None).await?,
    };
    print_json_value(&value, args.json)
}

fn print_json_value(value: &serde_json::Value, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("{value}");
    }
    Ok(())
}

async fn doctor(json: bool) -> anyhow::Result<()> {
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "status": "degraded",
        "checks": {
            "rust_workspace": "ready",
            "capture_backend": "synthetic_fallback",
            "input_backend": "refusing_until_platform_backend_enabled",
            "openrouter_configured": std::env::var("OPENROUTER_API_KEY").is_ok(),
            "public_surfaces": ["cli", "local_http", "local_unix_socket"]
        }
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{report}");
    }
    Ok(())
}

async fn stream(profile: &str, args: StreamArgs) -> anyhow::Result<()> {
    if !args.unix {
        anyhow::bail!("stream currently uses the local Unix visual session; pass --unix");
    }
    let token = load_profile_token(profile).await?;
    let socket_path = profile_socket_path(profile)?;
    let stream = UnixStream::connect(&socket_path)
        .await
        .with_context(|| format!("connect {}", socket_path.display()))?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let request = serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "token": token,
        "method": "visual.session",
        "params": {
            "schema_version": SCHEMA_VERSION,
            "max_width": args.max_width,
            "fps": args.fps,
            "include_bytes": args.include_bytes
        }
    });
    write.write_all(request.to_string().as_bytes()).await?;
    write.write_all(b"\n").await?;
    write.flush().await?;
    let mut frames = 0usize;
    while let Some(line) = lines.next_line().await? {
        let value: serde_json::Value = serde_json::from_str(&line)?;
        if args.json {
            println!("{line}");
        } else if value.get("type").and_then(|kind| kind.as_str()) == Some("frame") {
            let frame = &value["frame"]["envelope"];
            println!(
                "frame {} {}x{} display={}x{}",
                frame["frame_id"],
                frame["width"],
                frame["height"],
                frame["display_width"],
                frame["display_height"]
            );
        } else {
            println!("{value}");
        }
        if value.get("type").and_then(|kind| kind.as_str()) == Some("frame") {
            frames += 1;
            if frames >= args.frames {
                let close = serde_json::json!({
                    "id": uuid::Uuid::new_v4().to_string(),
                    "token": token,
                    "method": "visual.close",
                    "params": {}
                });
                write.write_all(close.to_string().as_bytes()).await?;
                write.write_all(b"\n").await?;
                write.flush().await?;
                break;
            }
        }
    }
    Ok(())
}

async fn unix_visual_first_frame(
    profile: &str,
    max_width: Option<u32>,
    fps: Option<u32>,
    include_bytes: bool,
) -> anyhow::Result<serde_json::Value> {
    let token = load_profile_token(profile).await?;
    let socket_path = profile_socket_path(profile)?;
    let stream = UnixStream::connect(&socket_path)
        .await
        .with_context(|| format!("connect {}", socket_path.display()))?;
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let request = serde_json::json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "token": token,
        "method": "visual.session",
        "params": {
            "schema_version": SCHEMA_VERSION,
            "max_width": max_width,
            "fps": fps,
            "include_bytes": include_bytes
        }
    });
    write.write_all(request.to_string().as_bytes()).await?;
    write.write_all(b"\n").await?;
    write.flush().await?;
    while let Some(line) = tokio::time::timeout(Duration::from_millis(750), lines.next_line())
        .await
        .context("timed out waiting for unix visual frame")??
    {
        let value: serde_json::Value = serde_json::from_str(&line)?;
        if value.get("type").and_then(|kind| kind.as_str()) == Some("frame") {
            let close = serde_json::json!({
                "id": uuid::Uuid::new_v4().to_string(),
                "token": token,
                "method": "visual.close",
                "params": {}
            });
            write.write_all(close.to_string().as_bytes()).await?;
            write.write_all(b"\n").await?;
            write.flush().await?;
            return Ok(value);
        }
    }
    anyhow::bail!("unix visual session closed before first frame")
}

async fn session(profile: &str, command: SessionCommand) -> anyhow::Result<()> {
    match command {
        SessionCommand::Acquire {
            session_id,
            client_name,
            role,
            ttl_ms,
            json,
        } => {
            let request = SessionLeaseRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                client_name: client_name.unwrap_or_else(|| "cua cli".to_string()),
                session_id,
                role: role.into(),
                ttl_ms,
            };
            let value = unix_request_json(
                profile,
                "session.acquire",
                Some(serde_json::to_value(request)?),
            )
            .await?;
            print_json_value(&value, json)
        }
        SessionCommand::Cancel {
            session_id,
            target_session_id,
            json,
        } => {
            let request = SessionCancelRequest {
                schema_version: SCHEMA_VERSION.to_string(),
                session_id,
                target_session_id,
            };
            let value = unix_request_json(
                profile,
                "session.cancel",
                Some(serde_json::to_value(request)?),
            )
            .await?;
            print_json_value(&value, json)
        }
        SessionCommand::Status { json } => unix_get(profile, "session.status", json).await,
    }
}

async fn load_profile_token(profile: &str) -> anyhow::Result<String> {
    if let Ok(token) = std::env::var("CUA_HTTP_TOKEN") {
        if !token.trim().is_empty() {
            return Ok(token);
        }
    }
    let path = profile_token_path(profile)?;
    let token = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read profile token {}", path.display()))?;
    Ok(token.trim().to_string())
}

async fn permissions(profile: &str, command: PermissionCommand) -> anyhow::Result<()> {
    if let PermissionCommand::RequestAccessibility(flag) = command {
        let value = unix_request_json(profile, "permissions.request_accessibility", None).await?;
        if flag.json {
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            println!("{value}");
        }
        return Ok(());
    }
    let preflight = matches!(command, PermissionCommand::Preflight(_));
    let json = match command {
        PermissionCommand::Status(flag) | PermissionCommand::Preflight(flag) => flag.json,
        PermissionCommand::RequestAccessibility(_) => unreachable!(),
    };
    if preflight {
        request_missing_desktop_permissions();
    }
    let permission_report = cua_platform_macos::permission_report();
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "screen_recording": permission_report.screen_recording,
        "accessibility_input": permission_report.accessibility_input,
        "input_monitoring": permission_report.input_monitoring,
        "automation": permission_report.automation,
        "clipboard": permission_report.clipboard,
        "portal": permission_report.portal,
        "ready_for_zero_touch_agent": false
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{report}");
    }
    Ok(())
}

fn request_missing_desktop_permissions() {
    let report = cua_platform_macos::permission_report();
    if should_request_permission(report.screen_recording) {
        let _ = cua_platform_macos::request_screen_recording_access();
    }
    if should_request_permission(report.accessibility_input) {
        let _ = cua_platform_macos::request_accessibility_input_access();
    }
}

fn should_request_permission(state: cua_core::PermissionState) -> bool {
    matches!(
        state,
        cua_core::PermissionState::Missing | cua_core::PermissionState::Denied
    )
}

async fn unix_request_json(
    profile: &str,
    method: &str,
    params: Option<serde_json::Value>,
) -> anyhow::Result<serde_json::Value> {
    unix_request_json_with_session(profile, method, params, None).await
}

async fn unix_request_json_with_session(
    profile: &str,
    method: &str,
    params: Option<serde_json::Value>,
    session_id: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    let client = CuaClient::connect(profile.to_string()).await?;
    client
        .request_with_session(method, params, session_id)
        .await
}

async fn perf(profile: &str, command: PerfCommand) -> anyhow::Result<()> {
    match command {
        PerfCommand::Live(flag) => unix_get(profile, "metrics", flag.json).await,
        PerfCommand::Bench(args) => perf_bench(profile, args).await,
    }
}

async fn config(profile: &str, command: ConfigCommand) -> anyhow::Result<()> {
    match command {
        ConfigCommand::Status(flag) => unix_get(profile, "config.status", flag.json).await,
    }
}

#[derive(Debug, Deserialize)]
struct Runebook {
    schema: String,
    run: Option<RunebookRun>,
    session: Option<RunebookSession>,
    trace: Option<RunebookTraceConfig>,
    vars: Option<BTreeMap<String, toml::Value>>,
    #[serde(default, rename = "macro")]
    macros: BTreeMap<String, RunebookMacro>,
    steps: Vec<RunebookStep>,
}

#[derive(Debug, Deserialize)]
struct RunebookRun {
    name: Option<String>,
    profile: Option<String>,
    on_error: Option<RunebookErrorPolicy>,
    trace: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RunebookSession {
    role: Option<SessionRoleArg>,
    client: Option<String>,
    ttl_ms: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RunebookTraceConfig {
    dir: Option<String>,
    verify_on_complete: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct RunebookMacro {
    #[serde(default)]
    items: Vec<RunebookStep>,
    delay_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
struct RunebookStep {
    id: Option<String>,
    #[serde(rename = "do")]
    action: String,
    save_as: Option<String>,
    on_error: Option<RunebookErrorPolicy>,
    #[serde(flatten)]
    fields: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RunebookErrorPolicy {
    Stop,
    Continue,
    Ask,
    Rollback,
}

#[derive(Debug, Serialize)]
struct RunebookReport {
    schema_version: String,
    name: String,
    profile: String,
    trace_path: Option<String>,
    steps: usize,
    ok: bool,
    results: BTreeMap<String, serde_json::Value>,
}

struct RunebookRuntime {
    profile: String,
    session_id: Option<String>,
    vars: HashMap<String, serde_json::Value>,
    results: BTreeMap<String, serde_json::Value>,
    trace: Option<RunebookTrace>,
}

struct RunebookTrace {
    path: PathBuf,
}

#[derive(Debug, Serialize)]
struct RunebookTraceRecord<'a> {
    schema_version: &'static str,
    event: &'a str,
    step_id: Option<&'a str>,
    step_do: Option<&'a str>,
    ok: bool,
    data: serde_json::Value,
}

async fn runebook_command(default_profile: &str, args: RunebookArgs) -> anyhow::Result<()> {
    let content = tokio::fs::read_to_string(&args.file)
        .await
        .with_context(|| format!("read runebook {}", args.file.display()))?;
    let runebook: Runebook = toml::from_str(&content)
        .with_context(|| format!("parse runebook TOML {}", args.file.display()))?;
    validate_runebook(&runebook)?;

    let profile = runebook
        .run
        .as_ref()
        .and_then(|run| run.profile.clone())
        .unwrap_or_else(|| default_profile.to_string());
    let name = runebook
        .run
        .as_ref()
        .and_then(|run| run.name.clone())
        .or_else(|| {
            args.file
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| "runebook".to_string());
    let trace_enabled = runebook
        .run
        .as_ref()
        .and_then(|run| run.trace)
        .unwrap_or(true);
    let trace = runebook_trace(
        &profile,
        &name,
        args.trace_dir,
        runebook.trace.as_ref(),
        trace_enabled,
    )
    .await?;
    let mut runtime = RunebookRuntime {
        profile: profile.clone(),
        session_id: None,
        vars: runebook_vars(runebook.vars.as_ref())?,
        results: BTreeMap::new(),
        trace,
    };
    runtime
        .trace(
            "run_start",
            None,
            None,
            true,
            serde_json::json!({"file": args.file.display().to_string()}),
        )
        .await?;

    if let Some(session) = runebook.session.as_ref() {
        let role = session.role.clone().unwrap_or(SessionRoleArg::Owner);
        let session_id = format!("runebook-{}", uuid::Uuid::new_v4());
        let request = SessionLeaseRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            session_id: session_id.clone(),
            client_name: session
                .client
                .clone()
                .unwrap_or_else(|| format!("runebook:{name}")),
            role: role.into(),
            ttl_ms: session.ttl_ms,
        };
        let result = runtime
            .request(
                "session.acquire",
                Some(serde_json::to_value(request)?),
                None,
            )
            .await?;
        runtime.session_id = Some(session_id);
        runtime.results.insert("session".to_string(), result);
    }

    let default_policy = runebook
        .run
        .as_ref()
        .and_then(|run| run.on_error)
        .unwrap_or(RunebookErrorPolicy::Stop);
    for step in &runebook.steps {
        let policy = step.on_error.unwrap_or(default_policy);
        match runtime.execute_step(step, &runebook.macros).await {
            Ok(value) => {
                if let Some(save_as) = &step.save_as {
                    runtime.results.insert(save_as.clone(), value);
                }
            }
            Err(error) => {
                let error_text = format!("{error:#}");
                runtime
                    .trace(
                        "step_error",
                        step.id.as_deref(),
                        Some(&step.action),
                        false,
                        serde_json::json!({"error": error_text.clone()}),
                    )
                    .await?;
                match policy {
                    RunebookErrorPolicy::Continue => continue,
                    RunebookErrorPolicy::Stop => anyhow::bail!("{error_text}"),
                    RunebookErrorPolicy::Ask => {
                        anyhow::bail!("runebook on_error=ask is not implemented yet")
                    }
                    RunebookErrorPolicy::Rollback => {
                        anyhow::bail!("runebook on_error=rollback is not implemented yet")
                    }
                }
            }
        }
    }

    runtime
        .trace(
            "run_complete",
            None,
            None,
            true,
            serde_json::json!({"steps": runebook.steps.len()}),
        )
        .await?;
    let report = RunebookReport {
        schema_version: "cua.runebook.report.v1".to_string(),
        name,
        profile,
        trace_path: runtime
            .trace
            .as_ref()
            .map(|trace| trace.path.display().to_string()),
        steps: runebook.steps.len(),
        ok: true,
        results: runtime.results,
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "runebook {} ok steps={} trace={}",
            report.name,
            report.steps,
            report.trace_path.as_deref().unwrap_or("off")
        );
    }
    Ok(())
}

impl RunebookRuntime {
    fn execute_step<'a>(
        &'a mut self,
        step: &'a RunebookStep,
        macros: &'a BTreeMap<String, RunebookMacro>,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<serde_json::Value>> + 'a>> {
        Box::pin(async move {
            self.trace(
                "step_start",
                step.id.as_deref(),
                Some(&step.action),
                true,
                serde_json::json!({}),
            )
            .await?;
            let value = self
                .execute_step_inner(step, macros)
                .await
                .with_context(|| {
                    format!(
                        "runebook step {} ({}) failed",
                        step.id.as_deref().unwrap_or("<anonymous>"),
                        step.action
                    )
                })?;
            self.trace(
                "step_complete",
                step.id.as_deref(),
                Some(&step.action),
                true,
                value.clone(),
            )
            .await?;
            Ok(value)
        })
    }

    async fn execute_step_inner(
        &mut self,
        step: &RunebookStep,
        macros: &BTreeMap<String, RunebookMacro>,
    ) -> anyhow::Result<serde_json::Value> {
        if let Some(name) = step.action.strip_prefix("macro.") {
            let mac = macros
                .get(name)
                .with_context(|| format!("unknown runebook macro {name}"))?;
            let mut last = serde_json::Value::Null;
            for item in &mac.items {
                last = self.execute_step(item, macros).await?;
                if let Some(delay) = mac.delay_ms {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
            return Ok(last);
        }

        match step.action.as_str() {
            "seq" => {
                let items = step_array(step, "items")?;
                let delay = step_u64(step, "delay_ms")?.unwrap_or(0);
                let mut last = serde_json::Value::Null;
                for item in items {
                    last = self.execute_step(&item, macros).await?;
                    if delay > 0 {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                    }
                }
                Ok(last)
            }
            "status" => self.request("status", None, None).await,
            "config.status" | "config" => self.request("config.status", None, None).await,
            "manifest" => self.request("manifest", None, None).await,
            "schemas" => self.request("schemas", None, None).await,
            "metrics" => self.request("metrics", None, None).await,
            "permissions.request_accessibility" | "permissions.accessibility" => {
                self.request("permissions.request_accessibility", None, None)
                    .await
            }
            "rpc" => {
                let method = self.required_string(step, "method")?;
                let params = step
                    .fields
                    .get("params")
                    .map(toml_value_to_json)
                    .transpose()?;
                let session_id = self
                    .string_field(step, "session_id")?
                    .or_else(|| self.session_id.clone());
                self.request(&method, params, session_id.as_deref()).await
            }
            "session.acquire" => {
                let session_id = self
                    .string_field(step, "session_id")?
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                self.request(
                    "session.acquire",
                    Some(serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "session_id": session_id,
                        "client_name": self.string_field(step, "client_name")?.unwrap_or_else(|| "cua runebook".to_string()),
                        "role": self.string_field(step, "role")?.unwrap_or_else(|| "owner".to_string()),
                        "ttl_ms": step_i64(step, "ttl_ms")?,
                    })),
                    None,
                )
                .await
            }
            "session.cancel" => {
                self.request(
                    "session.cancel",
                    Some(serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "session_id": self.string_field(step, "session_id")?.or_else(|| self.session_id.clone()).context("session.cancel requires session_id or top-level [session]")?,
                        "target_session_id": self.string_field(step, "target_session_id")?,
                    })),
                    None,
                )
                .await
            }
            "session.status" => self.request("session.status", None, None).await,
            "profile.status" => self.request("profile.status", None, None).await,
            "profile.create" => {
                self.request(
                    "profile.create",
                    Some(serde_json::json!({
                        "name": self.required_string(step, "name")?,
                        "mode": self.string_field(step, "mode")?.unwrap_or_else(|| "supervised".to_string()),
                        "duration_ms": step_u64(step, "duration_ms")?,
                        "capabilities": step.fields.get("capabilities").map(toml_value_to_json).transpose()?,
                    })),
                    self.session_id.as_deref(),
                )
                .await
            }
            "profile.activate" => {
                self.request("profile.activate", None, self.session_id.as_deref())
                    .await
            }
            "observe" => self.request("observe.desktop", None, None).await,
            "screenshot" | "capture.screenshot" => {
                self.request(
                    "capture.screenshot",
                    Some(serde_json::json!({
                        "max_width": step_u64(step, "max_width")?.unwrap_or(1280) as u32,
                        "encoding": self.string_field(step, "encoding")?.unwrap_or_else(|| "png".to_string()),
                        "force_fresh": step_bool(step, "force_fresh")?.unwrap_or(true),
                        "include_bytes": step_bool(step, "include_bytes")?.unwrap_or(false),
                    })),
                    None,
                )
                .await
            }
            "window.capture" | "capture.window" => {
                self.request(
                    "capture.window",
                    Some(serde_json::json!({
                        "window_id": step_u64(step, "window_id")?.context("capture.window requires window_id")? as u32,
                        "max_width": step_u64(step, "max_width")?.unwrap_or(1280) as u32,
                        "encoding": self.string_field(step, "encoding")?.unwrap_or_else(|| "png".to_string()),
                        "include_bytes": step_bool(step, "include_bytes")?.unwrap_or(false),
                    })),
                    None,
                )
                .await
            }
            "context" => {
                self.request(
                    "context.snapshot",
                    Some(serde_json::json!({
                        "max_width": step_u64(step, "max_width")?.unwrap_or(1280) as u32,
                        "encoding": self.string_field(step, "encoding")?.unwrap_or_else(|| "png".to_string()),
                        "force_fresh": step_bool(step, "force_fresh")?.unwrap_or(true),
                        "include_bytes": step_bool(step, "include_bytes")?.unwrap_or(false),
                    })),
                    None,
                )
                .await
            }
            "events" => {
                if let Some(timeout_ms) = step_u64(step, "timeout_ms")? {
                    self.request(
                        "events.wait",
                        Some(serde_json::json!({
                            "after_sequence": step_u64(step, "after")?.unwrap_or(0),
                            "timeout_ms": timeout_ms,
                        })),
                        None,
                    )
                    .await
                } else if let Some(after) = step_u64(step, "after")? {
                    self.request(
                        "events.after",
                        Some(serde_json::json!({"after_sequence": after})),
                        None,
                    )
                    .await
                } else {
                    self.request("events.snapshot", None, None).await
                }
            }
            "ui.step" => {
                self.request(
                    "ui.step",
                    Some(serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "label": self.required_string(step, "label")?,
                        "source": self.string_field(step, "source")?,
                        "task": self.string_field(step, "task")?,
                        "tool": self.string_field(step, "tool")?,
                        "step_index": step_u64(step, "step_index")?,
                        "step_total": step_u64(step, "step_total")?,
                        "ttl_ms": step_u64(step, "ttl_ms")?,
                    })),
                    None,
                )
                .await
            }
            "ui.island" => {
                self.request(
                    "ui.island",
                    Some(serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "state": self.required_string(step, "state")?,
                        "source": self.string_field(step, "source")?,
                    })),
                    None,
                )
                .await
            }
            "ui.reply" => {
                self.request(
                    "ui.reply",
                    Some(serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "text": self.required_string(step, "text")?,
                        "source": self.string_field(step, "source")?,
                        "ttl_ms": step_u64(step, "ttl_ms")?,
                    })),
                    None,
                )
                .await
            }
            "ui.mode" => {
                self.request(
                    "ui.mode",
                    Some(serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "mode": self.required_string(step, "mode")?,
                        "source": self.string_field(step, "source")?,
                    })),
                    None,
                )
                .await
            }
            "clipboard.read" => {
                self.request(
                    "clipboard.read",
                    Some(serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "allow_sensitive": step_bool(step, "allow_sensitive")?.unwrap_or(false),
                    })),
                    None,
                )
                .await
            }
            "clipboard.write" => {
                self.request(
                    "clipboard.write",
                    Some(serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "text": self.required_string(step, "text")?,
                    })),
                    self.session_id.as_deref(),
                )
                .await
            }
            "pause" => self.request("control.pause", None, self.session_id.as_deref()).await,
            "resume" => self.request("control.resume", None, self.session_id.as_deref()).await,
            "kill_switch" => {
                self.request("control.kill_switch", None, self.session_id.as_deref())
                    .await
            }
            "input" => {
                let action = step_json_field(step, "action")?;
                self.request("input.dispatch", Some(action), self.session_id.as_deref())
                    .await
            }
            "input.frame" => {
                let source_frame = step_json_field(step, "source_frame")?;
                let action = step_json_field(step, "action")?;
                self.request(
                    "input.dispatch_frame",
                    Some(serde_json::json!({
                        "schema_version": SCHEMA_VERSION,
                        "source_frame": source_frame,
                        "action": action
                    })),
                    self.session_id.as_deref(),
                )
                .await
            }
            "mouse_move" | "click" | "drag" | "key" | "type" | "paste" | "open_app" | "shell"
            | "aegis" | "ctx" => {
                let action = self.compact_input_action(step)?;
                self.request("input.dispatch", Some(action), self.session_id.as_deref())
                    .await
            }
            other => anyhow::bail!("unsupported runebook step do={other}"),
        }
    }

    async fn request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        session_id: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        unix_request_json_with_session(&self.profile, method, params, session_id).await
    }

    async fn trace(
        &self,
        event: &str,
        step_id: Option<&str>,
        step_do: Option<&str>,
        ok: bool,
        data: serde_json::Value,
    ) -> anyhow::Result<()> {
        let Some(trace) = &self.trace else {
            return Ok(());
        };
        let record = RunebookTraceRecord {
            schema_version: "cua.runebook.trace.v1",
            event,
            step_id,
            step_do,
            ok,
            data,
        };
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&trace.path)
            .await?;
        file.write_all(&line).await?;
        file.flush().await?;
        Ok(())
    }

    fn required_string(&self, step: &RunebookStep, key: &str) -> anyhow::Result<String> {
        self.string_field(step, key)?
            .with_context(|| format!("step {} requires {key}", step.action))
    }

    fn string_field(&self, step: &RunebookStep, key: &str) -> anyhow::Result<Option<String>> {
        let Some(value) = step_string(step, key)? else {
            return Ok(None);
        };
        Ok(Some(self.interpolate_string(&value)))
    }

    fn string_array_field(&self, step: &RunebookStep, key: &str) -> anyhow::Result<Vec<String>> {
        step_string_array(step, key).map(|values| {
            values
                .into_iter()
                .map(|value| self.interpolate_string(&value))
                .collect()
        })
    }

    fn interpolate_string(&self, input: &str) -> String {
        let mut out = input.to_string();
        for (key, value) in self.vars.iter().chain(self.results.iter()) {
            let replacement = scalar_json_to_string(value);
            out = out.replace(&format!("${{{key}}}"), &replacement);
            out = out.replace(&format!("${key}"), &replacement);
        }
        out
    }

    fn compact_input_action(&self, step: &RunebookStep) -> anyhow::Result<serde_json::Value> {
        let action = match step.action.as_str() {
            "mouse_move" => serde_json::json!({
                "kind": "mouse_move",
                "x": step_i64(step, "x")?.context("mouse_move requires x")? as i32,
                "y": step_i64(step, "y")?.context("mouse_move requires y")? as i32,
                "duration_ms": step_u64(step, "duration_ms")?.unwrap_or(0),
            }),
            "click" => serde_json::json!({
                "kind": "mouse_click",
                "x": step_i64(step, "x")?.context("click requires x")? as i32,
                "y": step_i64(step, "y")?.context("click requires y")? as i32,
                "button": self.string_field(step, "button")?.unwrap_or_else(|| "left".to_string()),
                "count": step_u64(step, "count")?.unwrap_or(1) as u8,
            }),
            "drag" => serde_json::json!({
                "kind": "mouse_drag",
                "from_x": step_i64(step, "from_x")?.context("drag requires from_x")? as i32,
                "from_y": step_i64(step, "from_y")?.context("drag requires from_y")? as i32,
                "to_x": step_i64(step, "to_x")?.context("drag requires to_x")? as i32,
                "to_y": step_i64(step, "to_y")?.context("drag requires to_y")? as i32,
                "duration_ms": step_u64(step, "duration_ms")?.unwrap_or(220),
            }),
            "key" => {
                serde_json::json!({"kind": "key_press", "combo": self.required_string(step, "combo")?})
            }
            "type" => {
                serde_json::json!({"kind": "key_type", "text": self.required_string(step, "text")?})
            }
            "paste" => {
                serde_json::json!({"kind": "key_paste", "text": self.required_string(step, "text")?})
            }
            "open_app" => {
                serde_json::json!({"kind": "open_app", "app_name": self.required_string(step, "app")?})
            }
            "shell" => serde_json::json!({
                "kind": "shell_exec",
                "command": self.required_string(step, "cmd")?,
                "timeout_ms": step_u64(step, "timeout_ms")?.unwrap_or(5_000),
            }),
            "aegis" => serde_json::json!({
                "kind": "aegis",
                "args": self.string_array_field(step, "args")?,
                "timeout_ms": step_u64(step, "timeout_ms")?.unwrap_or(15_000),
            }),
            "ctx" => serde_json::json!({
                "kind": "ctx",
                "args": self.string_array_field(step, "args")?,
                "timeout_ms": step_u64(step, "timeout_ms")?.unwrap_or(5_000),
                "workspace_root": self.string_field(step, "workspace_root")?,
            }),
            _ => unreachable!("checked by caller"),
        };
        Ok(action)
    }
}

fn validate_runebook(runebook: &Runebook) -> anyhow::Result<()> {
    if runebook.schema != "cua.runebook.v1" {
        anyhow::bail!("unsupported runebook schema {}", runebook.schema);
    }
    if runebook.steps.is_empty() {
        anyhow::bail!("runebook must contain at least one [[steps]] entry");
    }
    for (name, mac) in &runebook.macros {
        if mac.items.is_empty() {
            anyhow::bail!("runebook macro {name} must contain at least one item");
        }
    }
    Ok(())
}

fn runebook_vars(
    vars: Option<&BTreeMap<String, toml::Value>>,
) -> anyhow::Result<HashMap<String, serde_json::Value>> {
    let mut out = HashMap::new();
    if let Some(vars) = vars {
        for (key, value) in vars {
            out.insert(key.clone(), toml_value_to_json(value)?);
        }
    }
    Ok(out)
}

async fn runebook_trace(
    profile: &str,
    name: &str,
    override_dir: Option<PathBuf>,
    config: Option<&RunebookTraceConfig>,
    trace_enabled: bool,
) -> anyhow::Result<Option<RunebookTrace>> {
    if config
        .and_then(|config| config.verify_on_complete)
        .unwrap_or(false)
    {
        anyhow::bail!("runebook trace.verify_on_complete is not implemented yet");
    }
    let enabled = override_dir.is_some()
        || config.and_then(|config| config.dir.as_ref()).is_some()
        || config.is_some()
        || trace_enabled;
    if !enabled {
        return Ok(None);
    }
    let dir = if let Some(dir) = override_dir {
        dir
    } else if let Some(dir) = config.and_then(|config| config.dir.as_ref()) {
        expand_home_path(dir)?
    } else {
        profile_ctx_dir(profile)?
            .parent()
            .unwrap_or(Path::new("."))
            .join("traces")
            .join("runebooks")
            .join(sanitize_path_segment(name))
    };
    tokio::fs::create_dir_all(&dir).await?;
    Ok(Some(RunebookTrace {
        path: dir.join("run.jsonl"),
    }))
}

fn expand_home_path(path: &str) -> anyhow::Result<PathBuf> {
    if path == "~" {
        return Ok(PathBuf::from(std::env::var("HOME")?));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return Ok(PathBuf::from(std::env::var("HOME")?).join(rest));
    }
    Ok(PathBuf::from(path))
}

fn sanitize_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn step_array(step: &RunebookStep, key: &str) -> anyhow::Result<Vec<RunebookStep>> {
    let Some(value) = step.fields.get(key) else {
        anyhow::bail!("step {} requires {key}", step.action);
    };
    value
        .clone()
        .try_into()
        .with_context(|| format!("decode step {} field {key}", step.action))
}

fn step_json_field(step: &RunebookStep, key: &str) -> anyhow::Result<serde_json::Value> {
    let Some(value) = step.fields.get(key) else {
        anyhow::bail!("step {} requires {key}", step.action);
    };
    toml_value_to_json(value)
}

fn step_string(step: &RunebookStep, key: &str) -> anyhow::Result<Option<String>> {
    let Some(value) = step.fields.get(key) else {
        return Ok(None);
    };
    Ok(match value {
        toml::Value::String(value) => Some(value.clone()),
        _ => anyhow::bail!("step {} field {key} must be a string", step.action),
    })
}

fn step_string_array(step: &RunebookStep, key: &str) -> anyhow::Result<Vec<String>> {
    let Some(value) = step.fields.get(key) else {
        return Ok(Vec::new());
    };
    match value {
        toml::Value::Array(values) => values
            .iter()
            .map(|value| match value {
                toml::Value::String(value) => Ok(value.clone()),
                _ => anyhow::bail!("step {} field {key} must be a string array", step.action),
            })
            .collect(),
        _ => anyhow::bail!("step {} field {key} must be an array", step.action),
    }
}

fn step_bool(step: &RunebookStep, key: &str) -> anyhow::Result<Option<bool>> {
    let Some(value) = step.fields.get(key) else {
        return Ok(None);
    };
    Ok(match value {
        toml::Value::Boolean(value) => Some(*value),
        _ => anyhow::bail!("step {} field {key} must be a bool", step.action),
    })
}

fn step_i64(step: &RunebookStep, key: &str) -> anyhow::Result<Option<i64>> {
    let Some(value) = step.fields.get(key) else {
        return Ok(None);
    };
    Ok(match value {
        toml::Value::Integer(value) => Some(*value),
        _ => anyhow::bail!("step {} field {key} must be an integer", step.action),
    })
}

fn step_u64(step: &RunebookStep, key: &str) -> anyhow::Result<Option<u64>> {
    step_i64(step, key)?
        .map(|value| {
            if value < 0 {
                anyhow::bail!("step {} field {key} must be non-negative", step.action);
            }
            Ok(value as u64)
        })
        .transpose()
}

fn toml_value_to_json(value: &toml::Value) -> anyhow::Result<serde_json::Value> {
    serde_json::to_value(value).context("convert TOML value to JSON")
}

fn scalar_json_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

async fn perf_bench(profile: &str, args: PerfBenchArgs) -> anyhow::Result<()> {
    let iterations = args.iterations.max(1);
    let warmup = args.warmup;
    let budget_ms = args.budget_ms.unwrap_or(match args.target {
        PerfBenchTarget::Screenshot => 5_000,
        PerfBenchTarget::Stream => 2_000,
        PerfBenchTarget::Input => 250,
        PerfBenchTarget::ModelPrep => 5_000,
    });
    let mut samples = Vec::with_capacity(iterations);
    for index in 0..(iterations + warmup) {
        let started = Instant::now();
        match args.target {
            PerfBenchTarget::Screenshot => {
                let value = unix_request_json(
                    profile,
                    "capture.screenshot",
                    Some(serde_json::json!({
                        "max_width": 1280,
                        "encoding": FrameEncoding::Png,
                        "force_fresh": true,
                        "include_bytes": false
                    })),
                )
                .await?;
                let _: FramePayload = serde_json::from_value(value)?;
            }
            PerfBenchTarget::Stream => {
                let _frame = unix_visual_first_frame(profile, Some(1280), Some(30), false).await?;
            }
            PerfBenchTarget::Input => {
                let _: serde_json::Value = unix_request_json(
                    profile,
                    "input.dispatch",
                    Some(serde_json::to_value(InputAction::MouseMove {
                        x: 5,
                        y: 5,
                        duration_ms: 0,
                    })?),
                )
                .await?;
            }
            PerfBenchTarget::ModelPrep => {
                let _: serde_json::Value = unix_request_json(
                    profile,
                    "capture.screenshot",
                    Some(serde_json::json!({
                        "max_width": 640,
                        "encoding": FrameEncoding::Png,
                        "force_fresh": true,
                        "include_bytes": true
                    })),
                )
                .await?;
                let _payload = serde_json::json!({
                    "model": "openai/gpt-5-mini",
                    "messages": [{
                        "role": "user",
                        "content": "Return only the next desktop action as JSON."
                    }],
                    "max_tokens": 128
                });
            }
        }
        if index >= warmup {
            samples.push(started.elapsed().as_millis());
        }
    }
    samples.sort_unstable();
    let total_ms: u128 = samples.iter().sum();
    let avg_ms = total_ms as f64 / samples.len() as f64;
    let p95_index = ((samples.len() as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(samples.len() - 1);
    let p95_ms = samples[p95_index];
    let max_ms = *samples.last().unwrap_or(&0);
    let passed = p95_ms <= budget_ms;
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "target": format!("{:?}", args.target).to_ascii_lowercase(),
        "iterations": samples.len(),
        "warmup": warmup,
        "budget_ms": budget_ms,
        "avg_ms": avg_ms,
        "p95_ms": p95_ms,
        "max_ms": max_ms,
        "passed": passed,
        "samples_ms": samples
    });
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{report}");
    }
    if !passed {
        anyhow::bail!(
            "perf bench {:?} p95={}ms exceeded budget {}ms",
            args.target,
            p95_ms,
            budget_ms
        );
    }
    Ok(())
}

async fn context(profile: &str, args: ContextArgs) -> anyhow::Result<()> {
    let value = unix_request_json(
        profile,
        "context.snapshot",
        Some(serde_json::json!({
            "max_width": args.max_width,
            "encoding": FrameEncoding::Png,
            "force_fresh": args.force_fresh,
            "include_bytes": args.include_bytes
        })),
    )
    .await?;
    let snapshot: DesktopContextSnapshot = serde_json::from_value(value.clone())?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "frame_id={} {}x{} displays={} windows={}",
            snapshot.frame.envelope.frame_id,
            snapshot.frame.envelope.width,
            snapshot.frame.envelope.height,
            snapshot.desktop.displays.len(),
            snapshot.desktop.windows.len()
        );
    }
    Ok(())
}

async fn screenshot(profile: &str, args: ScreenshotArgs) -> anyhow::Result<()> {
    let value = unix_request_json(
        profile,
        "capture.screenshot",
        Some(serde_json::json!({
            "max_width": args.max_width,
            "encoding": FrameEncoding::Png,
            "force_fresh": args.force_fresh,
            "include_bytes": true
        })),
    )
    .await?;
    let frame: FramePayload = serde_json::from_value(value)?;
    write_frame_payload(&frame, &args.out, args.json, "screenshot").await
}

async fn window_capture(profile: &str, args: WindowCaptureArgs) -> anyhow::Result<()> {
    let value = unix_request_json(
        profile,
        "capture.window",
        Some(serde_json::json!({
            "window_id": args.window_id,
            "max_width": args.max_width,
            "encoding": FrameEncoding::Png,
            "include_bytes": true
        })),
    )
    .await?;
    let frame: FramePayload = serde_json::from_value(value)?;
    write_frame_payload(&frame, &args.out, args.json, "window").await
}

async fn write_frame_payload(
    frame: &FramePayload,
    out: &Path,
    json: bool,
    label: &str,
) -> anyhow::Result<()> {
    let bytes_base64 = frame
        .bytes_base64
        .as_deref()
        .context("capture response did not include bytes")?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(bytes_base64)
        .context("decode capture bytes")?;
    if let Some(parent) = out.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create capture directory {}", parent.display()))?;
    }
    tokio::fs::write(out, bytes)
        .await
        .with_context(|| format!("write capture {}", out.display()))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&frame.envelope)?);
    } else {
        println!(
            "wrote {label} {} frame_id={}",
            out.display(),
            frame.envelope.frame_id
        );
    }
    Ok(())
}

fn mouse_action(command: MouseCommand) -> InputAction {
    match command {
        MouseCommand::Move { x, y, duration_ms } => InputAction::MouseMove { x, y, duration_ms },
        MouseCommand::Click {
            x,
            y,
            button,
            count,
        } => InputAction::MouseClick {
            x,
            y,
            button: match button.as_str() {
                "right" => MouseButton::Right,
                "middle" => MouseButton::Middle,
                _ => MouseButton::Left,
            },
            count,
        },
    }
}

fn key_action(command: KeyCommand) -> InputAction {
    match command {
        KeyCommand::Press { combo } => InputAction::KeyPress { combo },
        KeyCommand::Type { text } => InputAction::KeyType { text },
        KeyCommand::Paste { text } => InputAction::KeyPaste { text },
    }
}

fn shell_action(args: ShellArgs) -> InputAction {
    InputAction::ShellExec {
        command: args.command,
        timeout_ms: args.timeout_ms,
    }
}

fn aegis_action(args: AegisArgs) -> InputAction {
    InputAction::Aegis {
        args: args.args,
        timeout_ms: args.timeout_ms,
    }
}

fn ctx_action(args: CtxArgs, profile: &str) -> InputAction {
    InputAction::Ctx {
        args: args.args,
        timeout_ms: args.timeout_ms,
        workspace_root: Some(ctx_workspace_root(profile)),
    }
}

fn ctx_workspace_root(profile: &str) -> String {
    profile_ctx_dir(profile)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| format!(".cua/profiles/{profile}/ctx"))
}

async fn daemon_input(profile: &str, action: InputAction) -> anyhow::Result<()> {
    let value = unix_request_json(
        profile,
        "input.dispatch",
        Some(serde_json::to_value(action)?),
    )
    .await?;
    print_json_value(&value, true)
}

async fn clipboard(profile: &str, command: ClipboardCommand) -> anyhow::Result<()> {
    match command {
        ClipboardCommand::Read {
            allow_sensitive,
            json,
        } => {
            let value = unix_request_json(
                profile,
                "clipboard.read",
                Some(serde_json::to_value(ClipboardReadRequest {
                    schema_version: SCHEMA_VERSION.to_string(),
                    allow_sensitive,
                })?),
            )
            .await?;
            print_json_value(&value, json)
        }
        ClipboardCommand::Write { text, json } => {
            let value = unix_request_json(
                profile,
                "clipboard.write",
                Some(serde_json::to_value(ClipboardWriteRequest {
                    schema_version: SCHEMA_VERSION.to_string(),
                    text,
                })?),
            )
            .await?;
            print_json_value(&value, json)
        }
    }
}

async fn model(command: ModelCommand) -> anyhow::Result<()> {
    let ModelCommand::Eval {
        live,
        max_calls,
        max_output_tokens,
        json,
    } = command;
    let bus = FrameBus::new(Arc::new(SyntheticCaptureBackend::default()));
    let frame = bus
        .latest_or_capture(CaptureRequest {
            max_width: Some(640),
            encoding: FrameEncoding::Png,
            force_fresh: true,
        })
        .await?
        .as_payload(true);
    let mut config = EvalConfig::default();
    config.live = live || std::env::var("CUA_MODEL_EVAL_LIVE").ok().as_deref() == Some("1");
    config.max_calls = max_calls
        .or_else(|| {
            std::env::var("CUA_MODEL_EVAL_MAX_CALLS")
                .ok()
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(config.max_calls);
    let max_output_tokens = max_output_tokens
        .or_else(|| {
            std::env::var("CUA_MODEL_EVAL_MAX_TOKENS")
                .ok()
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(256);
    for candidate in &mut config.candidates {
        candidate.max_output_tokens = max_output_tokens;
    }
    let key = std::env::var("OPENROUTER_API_KEY").ok();
    let report = run_eval_report(config, Some(frame), key).await;
    let value = serde_json::to_value(report)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("{value}");
    }
    Ok(())
}

async fn schema(command: SchemaCommand) -> anyhow::Result<()> {
    let SchemaCommand::Export { out } = command;
    if let Some(parent) = out.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create schema directory {}", parent.display()))?;
    }
    tokio::fs::write(&out, serde_json::to_vec_pretty(&schema_bundle())?)
        .await
        .with_context(|| format!("write schema bundle {}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(())
}

async fn trace(profile: &str, command: TraceCommand) -> anyhow::Result<()> {
    match command {
        TraceCommand::Start { dir } => {
            let writer = TraceWriter::create(&dir).await?;
            writer
                .append(&TraceRecord::Marker {
                    name: "trace_started".to_string(),
                    at_wall_ms: cua_core::now_wall_ms(),
                })
                .await?;
            println!("started trace {}", dir.display());
            Ok(())
        }
        TraceCommand::Inspect { dir, json } => inspect_trace(dir, json, false).await,
        TraceCommand::Verify { dir, json } => inspect_trace(dir, json, true).await,
        TraceCommand::Replay { dir, dry_run, json } => {
            replay_trace(profile, dir, dry_run, json).await
        }
    }
}

async fn replay_trace(
    profile: &str,
    dir: PathBuf,
    dry_run: bool,
    json: bool,
) -> anyhow::Result<()> {
    let turns = read_action_turns(&dir).await?;
    let mut replayed = 0usize;
    let mut skipped = Vec::new();
    let mut resnapshots = 0usize;
    for turn in turns {
        let before = fresh_frame(profile).await?;
        resnapshots += 1;
        let mut action = turn.action.clone();
        remap_action_coordinates(&mut action, turn.before.as_ref(), before.get("envelope"));
        if !dry_run {
            if let Some((method, body)) = replay_request(&turn, action)? {
                unix_request_json(profile, method, Some(body)).await?;
                replayed += 1;
            } else {
                skipped.push(turn.turn_id.clone());
            }
        } else if replay_request(&turn, action)?.is_some() {
            replayed += 1;
        } else {
            skipped.push(turn.turn_id.clone());
        }
        let _after = fresh_frame(profile).await?;
        resnapshots += 1;
    }
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "path": dir,
        "dry_run": dry_run,
        "action_turns": replayed + skipped.len(),
        "replayed": replayed,
        "skipped": skipped,
        "resnapshots": resnapshots,
        "ok": skipped.is_empty()
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{report}");
    }
    if !report["ok"].as_bool().unwrap_or(false) {
        anyhow::bail!("trace replay skipped unsupported action turns");
    }
    Ok(())
}

async fn read_action_turns(dir: &PathBuf) -> anyhow::Result<Vec<ActionTurnRecord>> {
    let path = dir.join("trajectory.jsonl");
    let content = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read trace {}", path.display()))?;
    let mut turns = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str::<TraceRecord>(line)
            .with_context(|| format!("parse trace record {} in {}", index + 1, path.display()))?;
        if let TraceRecord::ActionTurn(turn) = record {
            turns.push(turn);
        }
    }
    Ok(turns)
}

async fn fresh_frame(profile: &str) -> anyhow::Result<serde_json::Value> {
    unix_request_json(
        profile,
        "capture.screenshot",
        Some(serde_json::json!({
            "max_width": 1280,
            "include_bytes": false,
            "force_fresh": true,
            "encoding": "png"
        })),
    )
    .await
}

fn replay_request(
    turn: &ActionTurnRecord,
    action: serde_json::Value,
) -> anyhow::Result<Option<(&'static str, serde_json::Value)>> {
    if action.get("kind").and_then(|kind| kind.as_str()).is_some() {
        let input = serde_json::from_value::<InputAction>(action)?;
        match input {
            InputAction::MouseMove { .. }
            | InputAction::MouseClick { .. }
            | InputAction::MouseDrag { .. }
            | InputAction::KeyPress { .. }
            | InputAction::KeyType { .. }
            | InputAction::KeyPaste { .. }
            | InputAction::Sequence { .. }
            | InputAction::OpenApp { .. }
            | InputAction::ShellExec { .. }
            | InputAction::Aegis { .. }
            | InputAction::Ctx { .. } => {
                return Ok(Some(("input.dispatch", serde_json::to_value(input)?)));
            }
            InputAction::ClipboardRead { .. } | InputAction::ClipboardWrite { .. } => {
                return Ok(None);
            }
            InputAction::Pause => return Ok(Some(("control.pause", serde_json::json!({})))),
            InputAction::Resume => return Ok(Some(("control.resume", serde_json::json!({})))),
            InputAction::KillSwitch => {
                return Ok(Some(("control.kill_switch", serde_json::json!({}))));
            }
        }
    }
    match turn.result.get("action").and_then(|action| action.as_str()) {
        Some("clipboard_read") => Ok(Some(("clipboard.read", action))),
        Some("clipboard_write") => Ok(Some(("clipboard.write", action))),
        _ => Ok(None),
    }
}

fn remap_action_coordinates(
    action: &mut serde_json::Value,
    recorded_frame: Option<&serde_json::Value>,
    current_frame: Option<&serde_json::Value>,
) {
    if let Some(actions) = action
        .get_mut("actions")
        .and_then(|actions| actions.as_array_mut())
    {
        for action in actions {
            remap_action_coordinates(action, recorded_frame, current_frame);
        }
    }
    let Some((scale_x, scale_y)) = coordinate_scale(recorded_frame, current_frame) else {
        return;
    };
    for key in ["x", "from_x", "to_x"] {
        remap_i32(action, key, scale_x);
    }
    for key in ["y", "from_y", "to_y"] {
        remap_i32(action, key, scale_y);
    }
}

fn coordinate_scale(
    recorded_frame: Option<&serde_json::Value>,
    current_frame: Option<&serde_json::Value>,
) -> Option<(f64, f64)> {
    let recorded_width = recorded_frame?.get("width")?.as_f64()?;
    let recorded_height = recorded_frame?.get("height")?.as_f64()?;
    let current_width = current_frame?.get("width")?.as_f64()?;
    let current_height = current_frame?.get("height")?.as_f64()?;
    if recorded_width <= 0.0 || recorded_height <= 0.0 {
        return None;
    }
    Some((
        current_width / recorded_width,
        current_height / recorded_height,
    ))
}

fn remap_i32(action: &mut serde_json::Value, key: &str, scale: f64) {
    if let Some(value) = action.get_mut(key) {
        if let Some(number) = value.as_i64() {
            *value = serde_json::json!(((number as f64) * scale).round() as i32);
        }
    }
}

async fn profile(active_profile: &str, command: ProfileCommand) -> anyhow::Result<()> {
    match command {
        ProfileCommand::Create {
            name,
            mode,
            duration_ms,
            clipboard,
            json,
        } => {
            let mut capabilities = CapabilityManifest::default();
            capabilities.clipboard = clipboard;
            let value = unix_request_json(
                active_profile,
                "profile.create",
                Some(serde_json::json!({
                    "name": name,
                    "mode": RuntimeMode::from(mode),
                    "duration_ms": duration_ms,
                    "capabilities": capabilities,
                })),
            )
            .await?;
            print_json_value(&value, json)
        }
        ProfileCommand::Activate { json } => {
            let value = unix_request_json(active_profile, "profile.activate", None).await?;
            print_json_value(&value, json)
        }
        ProfileCommand::Status { json } => unix_get(active_profile, "profile.status", json).await,
    }
}

async fn inspect_trace(dir: PathBuf, json: bool, verify: bool) -> anyhow::Result<()> {
    let path = dir.join("trajectory.jsonl");
    let content = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read trace {}", path.display()))?;
    let mut records = 0usize;
    let mut action_turns = 0usize;
    let mut missing_artifacts = Vec::new();
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record = serde_json::from_str::<TraceRecord>(line)
            .with_context(|| format!("parse trace record {} in {}", index + 1, path.display()))?;
        if let TraceRecord::ActionTurn(turn) = record {
            action_turns += 1;
            if verify {
                for relative in [&turn.before_image_path, &turn.after_image_path]
                    .into_iter()
                    .flatten()
                {
                    if !dir.join(relative).is_file() {
                        missing_artifacts.push(relative.clone());
                    }
                }
                if turn.before.is_none() || turn.after.is_none() {
                    missing_artifacts.push(format!("{}:missing_frame_metadata", turn.turn_id));
                }
                if turn.evidence.is_null() {
                    missing_artifacts.push(format!("{}:missing_evidence", turn.turn_id));
                }
            }
        }
        records += 1;
    }
    let ok = !verify || (records > 0 && missing_artifacts.is_empty());
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "path": path,
        "records": records,
        "action_turns": action_turns,
        "missing_artifacts": missing_artifacts,
        "ok": ok
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{report}");
    }
    if !ok {
        anyhow::bail!("trace verification failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaps_mouse_coordinates_between_frame_sizes() {
        let mut action = serde_json::json!({
            "kind": "mouse_drag",
            "from_x": 100,
            "from_y": 50,
            "to_x": 200,
            "to_y": 100,
            "duration_ms": 10
        });
        let recorded = serde_json::json!({ "width": 400, "height": 200 });
        let current = serde_json::json!({ "width": 800, "height": 100 });

        remap_action_coordinates(&mut action, Some(&recorded), Some(&current));

        assert_eq!(action["from_x"], 200);
        assert_eq!(action["from_y"], 25);
        assert_eq!(action["to_x"], 400);
        assert_eq!(action["to_y"], 50);
    }

    #[test]
    fn remaps_sequence_mouse_coordinates_between_frame_sizes() {
        let mut action = serde_json::json!({
            "kind": "sequence",
            "actions": [
                {
                    "kind": "mouse_click",
                    "x": 100,
                    "y": 50,
                    "button": "left",
                    "count": 1
                },
                {
                    "kind": "mouse_drag",
                    "from_x": 100,
                    "from_y": 50,
                    "to_x": 200,
                    "to_y": 100,
                    "duration_ms": 10
                }
            ],
            "inter_action_delay_ms": 0
        });
        let recorded = serde_json::json!({ "width": 400, "height": 200 });
        let current = serde_json::json!({ "width": 800, "height": 100 });

        remap_action_coordinates(&mut action, Some(&recorded), Some(&current));

        assert_eq!(action["actions"][0]["x"], 200);
        assert_eq!(action["actions"][0]["y"], 25);
        assert_eq!(action["actions"][1]["from_x"], 200);
        assert_eq!(action["actions"][1]["from_y"], 25);
        assert_eq!(action["actions"][1]["to_x"], 400);
        assert_eq!(action["actions"][1]["to_y"], 50);
    }

    #[test]
    fn preflight_does_not_request_granted_permissions() {
        assert!(!should_request_permission(
            cua_core::PermissionState::Granted
        ));
        assert!(!should_request_permission(
            cua_core::PermissionState::NotApplicable
        ));
        assert!(!should_request_permission(
            cua_core::PermissionState::Unknown
        ));
        assert!(should_request_permission(
            cua_core::PermissionState::Missing
        ));
        assert!(should_request_permission(cua_core::PermissionState::Denied));
    }

    #[test]
    fn runebook_parses_steps_vars_and_macros() {
        let raw = r#"
schema = "cua.runebook.v1"

[run]
name = "smoke"
trace = false

[vars]
message = "hello"

[macro.say]
items = [
  { do = "ui.reply", text = "${message}" },
]

[[steps]]
id = "config"
do = "config.status"
save_as = "config"

[[steps]]
do = "macro.say"
"#;
        let runebook: Runebook = toml::from_str(raw).unwrap();

        validate_runebook(&runebook).unwrap();
        assert_eq!(runebook.schema, "cua.runebook.v1");
        assert_eq!(runebook.steps.len(), 2);
        assert_eq!(runebook.macros["say"].items.len(), 1);
    }

    #[test]
    fn runebook_interpolates_vars_and_saved_scalars() {
        let runtime = RunebookRuntime {
            profile: "default".to_string(),
            session_id: None,
            vars: HashMap::from([(
                "app".to_string(),
                serde_json::Value::String("Safari".to_string()),
            )]),
            results: BTreeMap::from([(
                "count".to_string(),
                serde_json::Value::Number(serde_json::Number::from(3)),
            )]),
            trace: None,
        };

        assert_eq!(
            runtime.interpolate_string("open ${app} $count times"),
            "open Safari 3 times"
        );
    }

    #[test]
    fn runebook_compiles_compact_open_app_alias() {
        let runtime = RunebookRuntime {
            profile: "default".to_string(),
            session_id: None,
            vars: HashMap::from([(
                "target".to_string(),
                serde_json::Value::String("Notes".to_string()),
            )]),
            results: BTreeMap::new(),
            trace: None,
        };
        let step: RunebookStep = toml::from_str(
            r#"
do = "open_app"
app = "${target}"
"#,
        )
        .unwrap();

        let action = runtime.compact_input_action(&step).unwrap();

        assert_eq!(action["kind"], "open_app");
        assert_eq!(action["app_name"], "Notes");
    }

    #[test]
    fn runebook_parses_protocol_adapter_steps() {
        let raw = include_str!("../../../tests/fixtures/runebook-protocol.cua.toml");
        let runebook: Runebook = toml::from_str(raw).unwrap();

        validate_runebook(&runebook).unwrap();
        let actions = runebook
            .steps
            .iter()
            .map(|step| step.action.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            actions,
            vec!["schemas", "rpc", "events", "ui.island", "screenshot"]
        );
        assert_eq!(
            runebook.steps[1].fields["method"].as_str(),
            Some("manifest")
        );
    }
}
