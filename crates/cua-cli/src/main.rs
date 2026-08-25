use anyhow::Context;
use clap::{Args, Parser, Subcommand};
use cua_capture::{CaptureRequest, FrameBus, SyntheticCaptureBackend};
use cua_core::{schema_bundle, FrameEncoding, InputAction, MouseButton, SCHEMA_VERSION};
use cua_input::InputBackend;
use cua_model::{run_eval_report, EvalConfig};
use cua_trace::{TraceRecord, TraceWriter};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

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
    Screenshot(ScreenshotArgs),
    Observe(JsonFlag),
    Mouse {
        #[command(subcommand)]
        command: MouseCommand,
    },
    Key {
        #[command(subcommand)]
        command: KeyCommand,
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
}

#[derive(Debug, Args)]
struct ServeArgs {
    #[arg(long, default_value = "127.0.0.1:8765")]
    addr: SocketAddr,
    #[arg(long)]
    allow_lan: bool,
}

#[derive(Debug, Args)]
struct JsonFlag {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum PermissionCommand {
    Status(JsonFlag),
    Preflight(JsonFlag),
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt().with_env_filter("info").init();
    let cli = Cli::parse();
    match cli.command {
        None => print_usage_and_status(cli.server_addr).await,
        Some(Command::Serve(args)) => {
            cua_daemon::serve(args.addr, cli.profile, args.allow_lan).await
        }
        Some(Command::Status(flag)) => {
            get_json(cli.server_addr, &cli.profile, "/status", flag.json).await
        }
        Some(Command::Doctor(flag)) => doctor(flag.json).await,
        Some(Command::Permissions { command }) => permissions(command).await,
        Some(Command::Screenshot(args)) => screenshot(args).await,
        Some(Command::Observe(flag)) => {
            get_json(cli.server_addr, &cli.profile, "/observe/desktop", flag.json).await
        }
        Some(Command::Mouse { command }) => local_input(mouse_action(command)).await,
        Some(Command::Key { command }) => local_input(key_action(command)).await,
        Some(Command::Model { command }) => model(command).await,
        Some(Command::Schema { command }) => schema(command).await,
        Some(Command::Trace { command }) => trace(command).await,
    }
}

async fn print_usage_and_status(server_addr: SocketAddr) -> anyhow::Result<()> {
    println!("cua: CLI/local-HTTP computer-use runtime");
    println!("usage: cua serve --addr {server_addr}");
    println!("       cua status --json");
    println!("       cua screenshot --out /tmp/screen.png");
    Ok(())
}

async fn get_json(addr: SocketAddr, profile: &str, path: &str, json: bool) -> anyhow::Result<()> {
    let url = format!("http://{addr}{path}");
    let token = cua_daemon::load_or_create_profile_token(profile).await?;
    let value: serde_json::Value = reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await?
        .json()
        .await?;
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
            "public_surfaces": ["cli", "local_http"]
        }
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{report}");
    }
    Ok(())
}

async fn permissions(command: PermissionCommand) -> anyhow::Result<()> {
    let json = match command {
        PermissionCommand::Status(flag) | PermissionCommand::Preflight(flag) => flag.json,
    };
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "screen_recording": "unknown",
        "accessibility_input": "unknown",
        "automation": "unknown",
        "clipboard": "unknown",
        "ready_for_zero_touch_agent": false,
        "reason": "platform permission probes are not implemented yet"
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{report}");
    }
    Ok(())
}

async fn screenshot(args: ScreenshotArgs) -> anyhow::Result<()> {
    let bus = FrameBus::new(Arc::new(SyntheticCaptureBackend::default()));
    let frame = bus
        .latest_or_capture(CaptureRequest {
            max_width: Some(args.max_width),
            encoding: FrameEncoding::Png,
            force_fresh: args.force_fresh,
        })
        .await?;
    if let Some(parent) = args.out.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create screenshot directory {}", parent.display()))?;
    }
    tokio::fs::write(&args.out, &*frame.bytes)
        .await
        .with_context(|| format!("write screenshot {}", args.out.display()))?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&frame.envelope)?);
    } else {
        println!(
            "wrote {} frame_id={}",
            args.out.display(),
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

async fn local_input(action: InputAction) -> anyhow::Result<()> {
    let result = cua_input::RefusingInputBackend
        .execute(cua_core::InputRequest {
            schema_version: SCHEMA_VERSION.to_string(),
            idempotency_key: uuid::Uuid::new_v4(),
            deadline_mono_ns: None,
            action,
        })
        .await;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
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

async fn trace(command: TraceCommand) -> anyhow::Result<()> {
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
    }
}

async fn inspect_trace(dir: PathBuf, json: bool, verify: bool) -> anyhow::Result<()> {
    let path = dir.join("trajectory.jsonl");
    let content = tokio::fs::read_to_string(&path)
        .await
        .with_context(|| format!("read trace {}", path.display()))?;
    let mut records = 0usize;
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        serde_json::from_str::<TraceRecord>(line)
            .with_context(|| format!("parse trace record {} in {}", index + 1, path.display()))?;
        records += 1;
    }
    let report = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "path": path,
        "records": records,
        "ok": !verify || records > 0
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{report}");
    }
    Ok(())
}
