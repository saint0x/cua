use clap::Parser;
use cua_voice::activation::ControlDoubleTap;
use cua_voice::client::CuaClient;
use cua_voice::hud::{
    HudDisplay, HudMetrics, COMPACT_HEIGHT, COMPACT_RADIUS, COMPACT_WIDTH, TOP_MARGIN,
    WINDOW_HEIGHT, WINDOW_WIDTH,
};
use cua_voice::orb::paint_orb;
use cua_voice::ui_state::{HudSnapshot, VoiceUiEvent};
use cua_voice::{
    run_text_turn_checked, run_voice_turn, run_voice_turn_checked, run_wav_turn_checked,
    VoiceConfig,
};
use gpui::{
    canvas, div, hsla, point, prelude::*, px, rgb, size, App, Application, Bounds, BoxShadow,
    Context, IntoElement, ParentElement, Render, Styled, Window, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions,
};
use rdev::{listen, EventType, Key};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Parser)]
#[command(name = "cua-voice", version, about = "Rust voice HUD for CUA")]
struct Args {
    #[arg(long, default_value = "default")]
    profile: String,
    #[arg(long, default_value_t = 4500)]
    record_ms: u64,
    #[arg(long, default_value = "openai/whisper-1")]
    stt_model: String,
    #[arg(long, default_value = "openai/gpt-5.4-mini")]
    planner_model: String,
    #[arg(long)]
    demo: bool,
    #[arg(long)]
    once_transcript: Option<String>,
    #[arg(long)]
    once_wav: Option<PathBuf>,
    #[arg(long)]
    once_record: bool,
}

struct VoiceHud {
    rx: Receiver<VoiceUiEvent>,
    tx: Sender<VoiceUiEvent>,
    config: VoiceConfig,
    runtime: Arc<tokio::runtime::Runtime>,
    snapshot: HudSnapshot,
    busy: bool,
    execute_turns: bool,
    started: Instant,
    last_frame: Instant,
    response_progress: f32,
}

impl VoiceHud {
    fn new(
        rx: Receiver<VoiceUiEvent>,
        tx: Sender<VoiceUiEvent>,
        config: VoiceConfig,
        runtime: Arc<tokio::runtime::Runtime>,
        execute_turns: bool,
    ) -> Self {
        Self {
            rx,
            tx,
            config,
            runtime,
            snapshot: HudSnapshot::default(),
            busy: false,
            execute_turns,
            started: Instant::now(),
            last_frame: Instant::now(),
            response_progress: 0.0,
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                VoiceUiEvent::Armed if self.execute_turns => {
                    if self.busy {
                        continue;
                    }
                    self.busy = true;
                    self.snapshot.apply(VoiceUiEvent::Armed);
                    let tx = self.tx.clone();
                    let config = self.config.clone();
                    self.runtime.spawn(run_voice_turn(config, tx));
                }
                VoiceUiEvent::Reply(_) | VoiceUiEvent::Error(_) => {
                    self.busy = false;
                    self.snapshot.apply(event);
                }
                event => self.snapshot.apply(event),
            }
        }
    }

    fn orb(&self) -> impl IntoElement {
        let phase = self.snapshot.phase.clone();
        let elapsed = self.started.elapsed().as_secs_f32();
        canvas(
            move |_, _, _| (phase, elapsed),
            move |bounds, (phase, elapsed), window, _| {
                paint_orb(window, bounds, &phase, elapsed);
            },
        )
        .size(px(13.0))
    }

    fn chip(label: impl Into<String>) -> impl IntoElement {
        div()
            .px_1()
            .py_0p5()
            .rounded(px(4.0))
            .bg(hsla(0.0, 0.0, 1.0, 0.10))
            .text_color(rgb(0xb9b9c0))
            .text_xs()
            .child(label.into())
    }

    fn divider() -> impl IntoElement {
        div().w(px(1.0)).h(px(14.0)).bg(hsla(0.0, 0.0, 1.0, 0.16))
    }

    fn activity_dots() -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(dot(1.0))
            .child(dot(0.88))
            .child(dot(0.76))
            .child(dot(0.64))
            .child(dot(0.52))
            .child(dot(0.40))
    }

    fn tick_animation(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        let target = if self.snapshot.is_expanded() {
            1.0
        } else {
            0.0
        };
        let step = (dt * 10.5).clamp(0.0, 1.0);
        self.response_progress += (target - self.response_progress) * step;
        if (self.response_progress - target).abs() < 0.01 {
            self.response_progress = target;
        }
    }

    fn compact_bar(&self, display: &HudDisplay, metrics: HudMetrics) -> impl IntoElement {
        let reply_visible = response_flash_visible(metrics);
        let title = if reply_visible {
            "Reply".to_string()
        } else {
            display.title.clone()
        };
        let center = if reply_visible {
            display.result.clone()
        } else {
            step_label(
                self.snapshot.step.index,
                self.snapshot.step.total,
                &display.rows[1].label,
            )
        };
        let tool = if reply_visible {
            "CUA".to_string()
        } else {
            display.tool.clone()
        };
        let app = if reply_visible {
            display.phase.to_string()
        } else {
            display.rows[1].app.clone()
        };

        div()
            .w(px(compact_bar_width(metrics)))
            .h(px(compact_bar_height(metrics)))
            .rounded(px(compact_bar_radius(metrics)))
            .overflow_hidden()
            .opacity(metrics.bar_opacity)
            .bg(hsla(0.0, 0.0, 0.0, 0.92))
            .border_1()
            .border_color(hsla(0.0, 0.0, 1.0, 0.15))
            .shadow(vec![BoxShadow {
                color: hsla(0.0, 0.0, 0.0, 0.58),
                blur_radius: px(18.0),
                spread_radius: px(0.0),
                offset: point(px(0.0), px(6.0)),
            }])
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .child(self.orb())
            .child(
                div()
                    .w(px(190.0))
                    .truncate()
                    .text_color(rgb(0x9f9fa6))
                    .text_xs()
                    .child(title),
            )
            .child(Self::divider())
            .child(
                div()
                    .w(px(270.0))
                    .truncate()
                    .text_color(if reply_visible {
                        rgb(0xf1f1f4)
                    } else {
                        rgb(0xb9b9c0)
                    })
                    .text_xs()
                    .child(center),
            )
            .child(Self::divider())
            .child(Self::chip(tool))
            .child(Self::chip(app))
            .child(div().flex_1())
            .child(Self::activity_dots())
    }
}

fn dot(alpha: f32) -> impl IntoElement {
    div()
        .w(px(4.0))
        .h(px(4.0))
        .rounded_full()
        .bg(hsla(244.0 / 360.0, 0.92, 0.70, alpha))
}

fn compact_bar_width(_: HudMetrics) -> f32 {
    COMPACT_WIDTH
}

fn compact_bar_height(_: HudMetrics) -> f32 {
    COMPACT_HEIGHT
}

fn compact_bar_radius(_: HudMetrics) -> f32 {
    COMPACT_RADIUS
}

fn response_flash_visible(metrics: HudMetrics) -> bool {
    metrics.response_opacity >= 0.35
}

fn step_label(index: usize, total: usize, label: &str) -> String {
    format!("Step {index}/{total}   {label}")
}

fn should_reset_after_reply_collapse(reply_window_expired: bool, response_progress: f32) -> bool {
    reply_window_expired && response_progress == 0.0
}

impl Render for VoiceHud {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_events();
        let reply_window_expired =
            self.snapshot.expanded_until.is_some() && !self.snapshot.is_expanded();
        self.tick_animation();
        if should_reset_after_reply_collapse(reply_window_expired, self.response_progress) {
            self.snapshot.apply(VoiceUiEvent::Idle);
        }
        self.snapshot.expire_programmed_step(Instant::now());
        window.request_animation_frame();
        let display = HudDisplay::from_snapshot(&self.snapshot);
        let metrics = HudMetrics::interpolate(self.response_progress);
        window.resize(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));
        div()
            .size_full()
            .relative()
            .child(self.compact_bar(&display, metrics).into_any_element())
    }
}

fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    let demo = demo_should_run(args.demo);
    let once_transcript = args.once_transcript;
    let once_wav = args.once_wav;
    let once_record = args.once_record;
    let config = VoiceConfig {
        profile: args.profile,
        record_ms: args.record_ms,
        stt_model: args.stt_model,
        planner_model: args.planner_model,
    };
    let runtime = Arc::new(tokio::runtime::Runtime::new()?);
    let (tx, rx) = channel::<VoiceUiEvent>();
    if let Some(transcript) = once_transcript {
        let result = runtime.block_on(run_text_turn_checked(config, transcript, tx));
        print_headless_events(rx);
        return result;
    } else if let Some(path) = once_wav {
        let wav_bytes = std::fs::read(&path)?;
        let result = runtime.block_on(run_wav_turn_checked(config, wav_bytes, tx));
        print_headless_events(rx);
        return result;
    } else if once_record {
        let result = runtime.block_on(run_voice_turn_checked(config, tx));
        print_headless_events(rx);
        return result;
    }

    let _single_instance = match SingleInstance::acquire(&config.profile)? {
        Some(instance) => instance,
        None => return Ok(()),
    };

    if demo {
        start_demo_cycle(tx.clone());
    } else {
        start_double_control_listener(tx.clone());
        start_agent_step_poll(config.profile.clone(), runtime.clone(), tx.clone());
    }
    Application::new().run(move |cx: &mut App| {
        let bounds = top_centered_bounds(cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: false,
                kind: WindowKind::PopUp,
                is_resizable: false,
                is_minimizable: false,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            },
            {
                let runtime = runtime.clone();
                let tx = tx.clone();
                let config = config.clone();
                let execute_turns = !demo;
                move |_, cx| cx.new(|_| VoiceHud::new(rx, tx, config, runtime, execute_turns))
            },
        )
        .unwrap();
        cx.activate(true);
    });
    Ok(())
}

fn print_headless_events(rx: Receiver<VoiceUiEvent>) {
    for event in rx.try_iter() {
        let value = match event {
            VoiceUiEvent::Armed => serde_json::json!({"event": "armed"}),
            VoiceUiEvent::Listening { ms } => serde_json::json!({"event": "listening", "ms": ms}),
            VoiceUiEvent::Transcribing => serde_json::json!({"event": "transcribing"}),
            VoiceUiEvent::Transcript(text) => {
                serde_json::json!({"event": "transcript", "text": text})
            }
            VoiceUiEvent::Planning { tool } => {
                serde_json::json!({"event": "planning", "tool": tool})
            }
            VoiceUiEvent::Dispatching(action) => {
                serde_json::json!({"event": "dispatching", "action": action})
            }
            VoiceUiEvent::AgentStep {
                label,
                source,
                task,
                tool,
                step_index,
                step_total,
                ttl_ms,
            } => {
                serde_json::json!({"event": "agent_step", "label": label, "source": source, "task": task, "tool": tool, "step_index": step_index, "step_total": step_total, "ttl_ms": ttl_ms})
            }
            VoiceUiEvent::Reply(text) => serde_json::json!({"event": "reply", "text": text}),
            VoiceUiEvent::Error(text) => serde_json::json!({"event": "error", "text": text}),
            VoiceUiEvent::Metric { name, ms } => {
                serde_json::json!({"event": "metric", "name": name, "ms": ms})
            }
            VoiceUiEvent::Idle => serde_json::json!({"event": "idle"}),
        };
        println!("{value}");
    }
}

struct SingleInstance {
    _listener: UnixListener,
    path: PathBuf,
}

impl SingleInstance {
    fn acquire(profile: &str) -> anyhow::Result<Option<Self>> {
        let path = single_instance_socket_path(profile);
        match UnixListener::bind(&path) {
            Ok(listener) => Ok(Some(Self {
                _listener: listener,
                path,
            })),
            Err(error) if error.kind() == ErrorKind::AddrInUse => {
                if UnixStream::connect(&path).is_ok() {
                    return Ok(None);
                }
                std::fs::remove_file(&path).ok();
                let listener = UnixListener::bind(&path)?;
                Ok(Some(Self {
                    _listener: listener,
                    path,
                }))
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

fn start_agent_step_poll(
    profile: String,
    runtime: Arc<tokio::runtime::Runtime>,
    tx: Sender<VoiceUiEvent>,
) {
    runtime.spawn(async move {
        let Ok(client) = CuaClient::new(profile).await else {
            return;
        };
        let Ok(mut session) = client.session().await else {
            return;
        };
        let mut last_sequence = 0_u64;
        loop {
            if let Ok(events) = session.events_wait(last_sequence, 1_000).await {
                for event in events {
                    if let Some((sequence, event)) =
                        agent_step_from_daemon_event(&event, last_sequence)
                    {
                        last_sequence = last_sequence.max(sequence);
                        tx.send(event).ok();
                    }
                }
            } else if let Ok(next_session) = client.session().await {
                session = next_session;
            } else {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
    });
}

fn agent_step_from_daemon_event(event: &Value, last_sequence: u64) -> Option<(u64, VoiceUiEvent)> {
    let sequence = event.get("sequence").and_then(|value| value.as_u64())?;
    if sequence <= last_sequence {
        return None;
    }
    if event.get("kind").and_then(|value| value.as_str()) != Some("ui_step") {
        return None;
    }
    let data = event.get("data")?;
    let label = data.get("label").and_then(|value| value.as_str())?;
    let source = data
        .get("source")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let task = data
        .get("task")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let tool = data
        .get("tool")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let step_index = data
        .get("step_index")
        .and_then(|value| value.as_u64())
        .and_then(|value| u16::try_from(value).ok());
    let step_total = data
        .get("step_total")
        .and_then(|value| value.as_u64())
        .and_then(|value| u16::try_from(value).ok());
    let ttl_ms = data.get("ttl_ms").and_then(|value| value.as_u64());
    if source.as_deref() == Some("voice") {
        return None;
    }
    Some((
        sequence,
        VoiceUiEvent::AgentStep {
            label: label.to_string(),
            source,
            task,
            tool,
            step_index,
            step_total,
            ttl_ms,
        },
    ))
}

fn single_instance_socket_path(profile: &str) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    profile.hash(&mut hasher);
    let hash = hasher.finish();
    let prefix = profile
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .take(24)
        .collect::<String>();
    PathBuf::from("/tmp").join(format!("cua-voice-{prefix}-{hash:016x}.sock"))
}

fn demo_should_run(requested: bool) -> bool {
    requested
        && std::env::current_exe()
            .map(|path| !path_looks_packaged_app(&path))
            .unwrap_or(true)
}

fn path_looks_packaged_app(path: &Path) -> bool {
    let parts = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();

    parts.windows(3).any(|window| {
        window[0].ends_with(".app") && window[1] == "Contents" && window[2] == "MacOS"
    })
}

fn top_centered_bounds(cx: &App) -> Bounds<gpui::Pixels> {
    let window_size = size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT));
    let Some(display) = cx.primary_display() else {
        return Bounds::centered(None, window_size, cx);
    };
    let display_bounds = display.bounds();
    let x = display_bounds.origin.x.to_f64() as f32
        + (display_bounds.size.width.to_f64() as f32 - WINDOW_WIDTH) / 2.0;
    let y = display_bounds.origin.y.to_f64() as f32 + TOP_MARGIN;
    Bounds {
        origin: point(px(x), px(y)),
        size: window_size,
    }
}

fn start_demo_cycle(tx: Sender<VoiceUiEvent>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(900));
        let sequence = [
            VoiceUiEvent::Armed,
            VoiceUiEvent::Listening { ms: 1200 },
            VoiceUiEvent::Transcribing,
            VoiceUiEvent::Transcript("Click 640 360".to_string()),
            VoiceUiEvent::Planning {
                tool: "Command parser".to_string(),
            },
            VoiceUiEvent::Dispatching("click 640 360".to_string()),
            VoiceUiEvent::AgentStep {
                label: "checking target state".to_string(),
                source: Some("planner".to_string()),
                task: Some("Click target".to_string()),
                tool: Some("vision".to_string()),
                step_index: Some(2),
                step_total: Some(4),
                ttl_ms: Some(5_000),
            },
            VoiceUiEvent::Reply("Clicked the center target.".to_string()),
        ];
        for event in sequence {
            tx.send(event).ok();
            std::thread::sleep(Duration::from_millis(650));
        }
        std::thread::sleep(Duration::from_millis(5_500));
        tx.send(VoiceUiEvent::Idle).ok();
    });
}

fn start_double_control_listener(tx: Sender<VoiceUiEvent>) {
    std::thread::spawn(move || {
        let detector = Arc::new(Mutex::new(ControlDoubleTap::default()));
        let callback_detector = detector.clone();
        let event_tx = tx.clone();
        let result = listen(move |event| {
            let is_control = matches!(
                event.event_type,
                EventType::KeyPress(Key::ControlLeft | Key::ControlRight)
                    | EventType::KeyRelease(Key::ControlLeft | Key::ControlRight)
            );
            if !is_control {
                return;
            }
            let mut detector = callback_detector.lock().unwrap();
            match event.event_type {
                EventType::KeyPress(_) => {
                    detector.key_down();
                }
                EventType::KeyRelease(_) => {
                    if detector.key_up(Instant::now()) {
                        event_tx.send(VoiceUiEvent::Armed).ok();
                    }
                }
                _ => {}
            }
        });
        if let Err(error) = result {
            tx.send(VoiceUiEvent::Error(format!(
                "Control listener failed: {error:?}"
            )))
            .ok();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_voice_turn_ignores_duplicate_arm_event() {
        let (tx, rx) = channel::<VoiceUiEvent>();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let mut hud = VoiceHud::new(rx, tx.clone(), VoiceConfig::default(), runtime, true);
        hud.busy = true;
        hud.snapshot
            .apply(VoiceUiEvent::Dispatching("mouse_move".to_string()));

        tx.send(VoiceUiEvent::Armed).unwrap();
        hud.drain_events();

        assert!(hud.busy);
        assert_eq!(hud.snapshot.step.label, "mouse_move");
        assert_eq!(hud.snapshot.tool, "Unix socket");
    }

    #[test]
    fn packaged_app_ignores_inherited_demo_arg() {
        assert!(path_looks_packaged_app(Path::new(
            "/Applications/CUA.app/Contents/MacOS/cua-voice"
        )));
        assert!(!demo_should_run_for_path(
            true,
            Path::new("/Applications/CUA.app/Contents/MacOS/cua-voice")
        ));
        assert!(demo_should_run_for_path(true, Path::new("/tmp/cua-voice")));
        assert!(!demo_should_run_for_path(
            false,
            Path::new("/tmp/cua-voice")
        ));
    }

    fn demo_should_run_for_path(requested: bool, path: &Path) -> bool {
        requested && !path_looks_packaged_app(path)
    }

    #[test]
    fn reply_snapshot_survives_until_collapse_is_invisible() {
        assert!(!should_reset_after_reply_collapse(true, 0.42));
        assert!(should_reset_after_reply_collapse(true, 0.0));
        assert!(!should_reset_after_reply_collapse(false, 0.0));
    }

    #[test]
    fn compact_bar_keeps_island_height_during_response_transition() {
        let transitioning = HudMetrics::interpolate(0.45);

        assert_eq!(transitioning.height, COMPACT_HEIGHT);
        assert_eq!(transitioning.width, COMPACT_WIDTH);
        assert_eq!(compact_bar_width(transitioning), COMPACT_WIDTH);
        assert_eq!(compact_bar_height(transitioning), COMPACT_HEIGHT);
        assert_eq!(compact_bar_radius(transitioning), COMPACT_RADIUS);
    }

    #[test]
    fn reply_progress_switches_bar_to_response_flash_mode() {
        assert!(!response_flash_visible(HudMetrics::interpolate(0.20)));
        assert!(response_flash_visible(HudMetrics::interpolate(0.35)));
    }

    #[test]
    fn step_label_stays_compact_and_structured() {
        assert_eq!(
            step_label(2, 5, "checking target"),
            "Step 2/5   checking target"
        );
    }

    #[test]
    fn daemon_ui_step_event_maps_to_visible_agent_step() {
        let event = serde_json::json!({
            "sequence": 42,
            "kind": "ui_step",
            "data": {
                "label": "checking current focus",
                "source": "agent",
                "task": "debug auth",
                "tool": "browser",
                "step_index": 2,
                "step_total": 6,
                "ttl_ms": 1750
            }
        });

        let Some((
            sequence,
            VoiceUiEvent::AgentStep {
                label,
                source,
                task,
                tool,
                step_index,
                step_total,
                ttl_ms,
            },
        )) = agent_step_from_daemon_event(&event, 41)
        else {
            panic!("expected agent step event");
        };

        assert_eq!(sequence, 42);
        assert_eq!(label, "checking current focus");
        assert_eq!(source.as_deref(), Some("agent"));
        assert_eq!(task.as_deref(), Some("debug auth"));
        assert_eq!(tool.as_deref(), Some("browser"));
        assert_eq!(step_index, Some(2));
        assert_eq!(step_total, Some(6));
        assert_eq!(ttl_ms, Some(1750));
        assert!(agent_step_from_daemon_event(&event, 42).is_none());
    }

    #[test]
    fn daemon_ui_step_event_ignores_voice_telemetry_echoes() {
        let event = serde_json::json!({
            "sequence": 43,
            "kind": "ui_step",
            "data": {
                "label": "reply: done",
                "source": "voice"
            }
        });

        assert!(agent_step_from_daemon_event(&event, 42).is_none());
    }

    #[test]
    fn second_voice_hud_instance_exits_before_windowing() {
        let profile = format!("test-{}", uuid::Uuid::new_v4());
        let first = SingleInstance::acquire(&profile).unwrap();
        let second = SingleInstance::acquire(&profile).unwrap();

        assert!(first.is_some());
        assert!(second.is_none());
    }

    #[test]
    fn single_instance_socket_path_sanitizes_profile() {
        let path = single_instance_socket_path("profile/with spaces");
        let name = path.file_name().unwrap().to_string_lossy();

        assert!(name.starts_with("cua-voice-profile_with_spaces-"));
        assert!(name.ends_with(".sock"));
        assert!(path.to_string_lossy().len() < 104);
    }
}
