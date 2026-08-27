use anyhow::Context;
use base64::Engine;
use clap::{Args, Parser, Subcommand};
use cua_capture::{CaptureRequest, FrameBus, SyntheticCaptureBackend};
use cua_core::{
    schema_bundle, CapabilityManifest, ClipboardReadRequest, ClipboardWriteRequest,
    DesktopContextSnapshot, FrameEncoding, FramePayload, InputAction, MouseButton, RuntimeMode,
    RuntimeSessionRole, SessionCancelRequest, SessionLeaseRequest, UiIslandRequest, UiIslandState,
    UiMode, UiModeRequest, UiReplyRequest, UiStepRequest, SCHEMA_VERSION,
};
use cua_model::{run_eval_report, EvalConfig};
use cua_trace::{ActionTurnRecord, TraceRecord, TraceWriter};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
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
    Manifest(JsonFlag),
    Metrics(JsonFlag),
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

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
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
        Some(Command::Manifest(flag)) => unix_get(&cli.profile, "manifest", flag.json).await,
        Some(Command::Metrics(flag)) => unix_get(&cli.profile, "metrics", flag.json).await,
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
    if let Ok(home) = std::env::var("HOME") {
        load_dotenv_path(&PathBuf::from(home).join(".cua").join(".env"));
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

fn profile_token_path(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(std::env::var("HOME")?)
        .join(".cua")
        .join("profiles")
        .join(profile)
        .join("http.token"))
}

fn profile_socket_path(profile: &str) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(std::env::var("HOME")?)
        .join(".cua")
        .join("profiles")
        .join(profile)
        .join("daemon.sock"))
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
        "method": method,
        "params": params.unwrap_or_else(|| serde_json::json!({}))
    });
    write.write_all(request.to_string().as_bytes()).await?;
    write.write_all(b"\n").await?;
    write.flush().await?;
    let line = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow::anyhow!("empty unix response for {method}"))?;
    let response: serde_json::Value = serde_json::from_str(&line)?;
    if response.get("ok").and_then(|ok| ok.as_bool()) != Some(true) {
        anyhow::bail!("unix request {method} failed: {}", response["error"]);
    }
    Ok(response
        .get("result")
        .cloned()
        .unwrap_or(serde_json::Value::Null))
}

async fn perf(profile: &str, command: PerfCommand) -> anyhow::Result<()> {
    match command {
        PerfCommand::Live(flag) => unix_get(profile, "metrics", flag.json).await,
        PerfCommand::Bench(args) => perf_bench(profile, args).await,
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
    std::env::var("HOME")
        .map(|home| format!("{home}/.cua/profiles/{profile}/ctx"))
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
}
