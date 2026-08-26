use clap::Parser;
use cua_core::{PermissionState, UiMode};
use cua_voice::activation::ControlDoubleTap;
use cua_voice::agent_events::{
    agent_reply_from_daemon_event, agent_step_from_daemon_event,
    agent_ui_event_from_daemon_event_advancing_cursor,
};
use cua_voice::client::CuaClient;
use cua_voice::daemon::profile_daemon_is_alive;
use cua_voice::hud::{
    HudDisplay, HudMetrics, COMPACT_HEIGHT, COMPACT_RADIUS, COMPACT_WIDTH, TOP_MARGIN,
    WINDOW_HEIGHT, WINDOW_WIDTH,
};
use cua_voice::orb::paint_orb;
use cua_voice::ui_state::{HudPhase, HudSnapshot, VoiceUiEvent};
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
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CENTER_LABEL_WIDTH: f32 = 270.0;
const MARQUEE_START_DELAY_SECS: f32 = 1.6;
const MARQUEE_END_HOLD_SECS: f32 = 0.9;
const MARQUEE_SCROLL_SPEED_PX_PER_SEC: f32 = 24.0;
const MARQUEE_CHAR_WIDTH_PX: f32 = 6.2;
const ACTIVITY_DOT_COUNT: usize = 6;

#[derive(Debug, Parser)]
#[command(name = "cua-voice", version, about = "Rust voice HUD for cua")]
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
    #[arg(long, conflicts_with = "headless")]
    headful: bool,
    #[arg(long, conflicts_with = "headful")]
    headless: bool,
    #[arg(long)]
    once_transcript: Option<String>,
    #[arg(long)]
    once_wav: Option<PathBuf>,
    #[arg(long)]
    once_record: bool,
    #[arg(long)]
    once_agent_step_wait_ms: Option<u64>,
    #[arg(long, default_value_t = 0)]
    once_agent_step_after: u64,
    #[arg(long)]
    once_agent_reply_wait_ms: Option<u64>,
    #[arg(long, default_value_t = 0)]
    once_agent_reply_after: u64,
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
    center_text_key: String,
    center_text_since: Instant,
    response_progress: f32,
}

impl VoiceHud {
    fn new(
        rx: Receiver<VoiceUiEvent>,
        tx: Sender<VoiceUiEvent>,
        config: VoiceConfig,
        runtime: Arc<tokio::runtime::Runtime>,
        execute_turns: bool,
        mode: UiMode,
    ) -> Self {
        let mut snapshot = HudSnapshot::default();
        let source = initial_ui_source(&mode).to_string();
        snapshot.apply(VoiceUiEvent::UiMode {
            mode,
            source: Some(source),
        });
        Self {
            rx,
            tx,
            config,
            runtime,
            snapshot,
            busy: false,
            execute_turns,
            started: Instant::now(),
            last_frame: Instant::now(),
            center_text_key: String::new(),
            center_text_since: Instant::now(),
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

    fn activity_dots(&self) -> impl IntoElement {
        let elapsed = self.started.elapsed().as_secs_f32();
        let active = dots_are_active(&self.snapshot);
        let speed = dot_pulse_speed(&self.snapshot.phase);
        let mut row = div().flex().items_center().gap_1();
        for index in 0..ACTIVITY_DOT_COUNT {
            let style = activity_dot_style(index, elapsed, active, speed);
            row = row.child(dot(style));
        }
        row
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

    fn sync_center_text(&mut self, center_text: &str) {
        if self.center_text_key != center_text {
            self.center_text_key = center_text.to_string();
            self.center_text_since = Instant::now();
        }
    }

    fn compact_bar(
        &self,
        display: &HudDisplay,
        metrics: HudMetrics,
        center: String,
    ) -> impl IntoElement {
        let reply_visible = response_flash_visible(metrics);
        let title = if reply_visible {
            "Reply".to_string()
        } else {
            display.title.clone()
        };
        let tool = if reply_visible {
            "cua".to_string()
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
            .child(center_text_slot(
                center,
                reply_visible,
                self.center_text_since.elapsed().as_secs_f32(),
            ))
            .child(Self::divider())
            .child(Self::chip(tool))
            .child(Self::chip(app))
            .child(div().flex_1())
            .child(self.activity_dots())
    }
}

fn dots_are_active(snapshot: &HudSnapshot) -> bool {
    !matches!(snapshot.phase, HudPhase::Idle)
}

fn dot_pulse_speed(phase: &HudPhase) -> f32 {
    match phase {
        HudPhase::Listening | HudPhase::Dispatching => 8.0,
        HudPhase::Planning | HudPhase::Transcribing => 6.0,
        HudPhase::Reply => 5.0,
        HudPhase::Error => 10.0,
        HudPhase::Armed => 7.0,
        HudPhase::Idle => 0.0,
    }
}

#[cfg(test)]
fn activity_dot_alpha(index: usize, elapsed_secs: f32, active: bool, speed: f32) -> f32 {
    activity_dot_style(index, elapsed_secs, active, speed).alpha
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ActivityDotStyle {
    alpha: f32,
    lightness: f32,
}

fn activity_dot_style(
    index: usize,
    elapsed_secs: f32,
    active: bool,
    steps_per_second: f32,
) -> ActivityDotStyle {
    if !active || steps_per_second <= 0.0 {
        return ActivityDotStyle {
            alpha: 0.24,
            lightness: 0.46,
        };
    }

    let head = ((elapsed_secs * steps_per_second).floor() as usize) % ACTIVITY_DOT_COUNT;
    let distance_behind = (head + ACTIVITY_DOT_COUNT - index) % ACTIVITY_DOT_COUNT;
    match distance_behind {
        0 => ActivityDotStyle {
            alpha: 1.0,
            lightness: 0.50,
        },
        1 => ActivityDotStyle {
            alpha: 0.72,
            lightness: 0.58,
        },
        2 => ActivityDotStyle {
            alpha: 0.44,
            lightness: 0.66,
        },
        _ => ActivityDotStyle {
            alpha: 0.18,
            lightness: 0.74,
        },
    }
}

fn center_text_for(display: &HudDisplay, snapshot: &HudSnapshot, metrics: HudMetrics) -> String {
    if response_flash_visible(metrics) {
        display.result.clone()
    } else {
        center_status_text(snapshot)
    }
}

fn center_text_slot(center: String, reply_visible: bool, visible_secs: f32) -> impl IntoElement {
    let offset = marquee_offset_px(&center, CENTER_LABEL_WIDTH, visible_secs);
    div()
        .w(px(CENTER_LABEL_WIDTH))
        .overflow_hidden()
        .whitespace_nowrap()
        .text_color(if reply_visible {
            rgb(0xf1f1f4)
        } else {
            rgb(0xb9b9c0)
        })
        .text_xs()
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .ml(px(-offset))
                .child(center),
        )
}

fn marquee_offset_px(text: &str, viewport_width_px: f32, visible_secs: f32) -> f32 {
    let text_width = estimated_center_text_width_px(text);
    let overflow = (text_width - viewport_width_px).max(0.0);
    if overflow <= 0.0 || visible_secs < MARQUEE_START_DELAY_SECS {
        return 0.0;
    }
    let scroll_duration = (overflow / MARQUEE_SCROLL_SPEED_PX_PER_SEC).max(0.5);
    let cycle_duration = scroll_duration + MARQUEE_END_HOLD_SECS;
    let cycle_pos = (visible_secs - MARQUEE_START_DELAY_SECS).rem_euclid(cycle_duration);
    if cycle_pos >= scroll_duration {
        overflow
    } else {
        (cycle_pos / scroll_duration) * overflow
    }
}

fn estimated_center_text_width_px(text: &str) -> f32 {
    text.chars().count() as f32 * MARQUEE_CHAR_WIDTH_PX
}

fn dot(style: ActivityDotStyle) -> impl IntoElement {
    div().w(px(4.0)).h(px(4.0)).rounded_full().bg(hsla(
        210.0 / 360.0,
        1.0,
        style.lightness,
        style.alpha,
    ))
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

fn center_status_text(snapshot: &HudSnapshot) -> String {
    if snapshot.phase == HudPhase::Idle && snapshot.step.index == 0 {
        return snapshot.step.label.clone();
    }
    step_label(
        snapshot.step.index,
        snapshot.step.total,
        &snapshot.step.label,
    )
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
        let center_text = center_text_for(&display, &self.snapshot, metrics);
        self.sync_center_text(&center_text);
        window.resize(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));
        div()
            .size_full()
            .relative()
            .child(
                self.compact_bar(&display, metrics, center_text)
                    .into_any_element(),
            )
            .into_any_element()
    }
}

fn main() -> anyhow::Result<()> {
    load_cua_dotenv();
    let args = Args::parse();
    let demo = demo_should_run(args.demo);
    let ui_mode = ui_mode_from_flags(args.headful, args.headless);
    let once_transcript = args.once_transcript;
    let once_wav = args.once_wav;
    let once_record = args.once_record;
    let once_agent_step_wait_ms = args.once_agent_step_wait_ms;
    let once_agent_step_after = args.once_agent_step_after;
    let once_agent_reply_wait_ms = args.once_agent_reply_wait_ms;
    let once_agent_reply_after = args.once_agent_reply_after;
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
    } else if let Some(wait_ms) = once_agent_step_wait_ms {
        let result = runtime.block_on(run_once_agent_ui_event_wait(
            config.profile.clone(),
            once_agent_step_after,
            wait_ms,
            tx,
            DaemonUiEventKind::Step,
        ));
        print_headless_events(rx);
        return result;
    } else if let Some(wait_ms) = once_agent_reply_wait_ms {
        let result = runtime.block_on(run_once_agent_ui_event_wait(
            config.profile.clone(),
            once_agent_reply_after,
            wait_ms,
            tx,
            DaemonUiEventKind::Reply,
        ));
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
        request_screen_recording_access_if_packaged_app();
        start_embedded_daemon_if_needed(config.profile.clone(), runtime.clone(), tx.clone());
        start_double_control_listener_if_allowed(tx.clone());
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
                mouse_passthrough: true,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            },
            {
                let runtime = runtime.clone();
                let tx = tx.clone();
                let config = config.clone();
                let execute_turns = !demo;
                move |_, cx| {
                    cx.new(|_| {
                        VoiceHud::new(rx, tx, config, runtime, execute_turns, ui_mode.clone())
                    })
                }
            },
        )
        .unwrap();
    });
    Ok(())
}

fn ui_mode_from_flags(headful: bool, headless: bool) -> UiMode {
    match (headful, headless) {
        (_, true) => UiMode::Headless,
        _ => UiMode::Headful,
    }
}

fn initial_ui_source(mode: &UiMode) -> &'static str {
    match mode {
        UiMode::Headful => "voice",
        UiMode::Headless => "automation",
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
            VoiceUiEvent::UiMode { mode, source } => {
                serde_json::json!({"event": "ui_mode", "mode": mode, "source": source})
            }
            VoiceUiEvent::AutomationActivity {
                label,
                source,
                tool,
            } => serde_json::json!({
                "event": "automation_activity",
                "label": label,
                "source": source,
                "tool": tool
            }),
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
                    if let Some(event) = agent_ui_event_from_daemon_event_advancing_cursor(
                        &event,
                        &mut last_sequence,
                    ) {
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

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum DaemonUiEventKind {
    Step,
    Reply,
}

async fn run_once_agent_ui_event_wait(
    profile: String,
    after_sequence: u64,
    wait_ms: u64,
    tx: Sender<VoiceUiEvent>,
    kind: DaemonUiEventKind,
) -> anyhow::Result<()> {
    let client = CuaClient::new(profile).await?;
    let mut session = client.session().await?;
    let started = Instant::now();
    let timeout = Duration::from_millis(wait_ms.clamp(25, 30_000));
    let mut last_sequence = after_sequence;
    loop {
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            anyhow::bail!("timed out waiting for programmed agent step");
        }
        let remaining_ms = timeout
            .saturating_sub(elapsed)
            .as_millis()
            .clamp(25, u128::from(u64::MAX)) as u64;
        let events = session.events_wait(last_sequence, remaining_ms).await?;
        for event in events {
            let Some(sequence) = event.get("sequence").and_then(|value| value.as_u64()) else {
                continue;
            };
            last_sequence = last_sequence.max(sequence);
            let mapped = match kind {
                DaemonUiEventKind::Step => agent_step_from_daemon_event(&event, after_sequence),
                DaemonUiEventKind::Reply => agent_reply_from_daemon_event(&event, after_sequence),
            };
            if let Some((_, event)) = mapped {
                tx.send(event).ok();
                return Ok(());
            }
        }
    }
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

fn start_embedded_daemon_if_needed(
    profile: String,
    runtime: Arc<tokio::runtime::Runtime>,
    tx: Sender<VoiceUiEvent>,
) {
    if profile_daemon_is_alive(&profile) {
        return;
    }
    runtime.spawn(async move {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        if let Err(error) = cua_daemon::serve(addr, profile, false, UiMode::Headless).await {
            tx.send(VoiceUiEvent::Error(format!("Daemon start failed: {error}")))
                .ok();
        }
    });
}

fn start_double_control_listener_if_allowed(tx: Sender<VoiceUiEvent>) {
    let permission = cua_platform_macos::input_monitoring_permission();
    let permission = if permission == PermissionState::Granted {
        permission
    } else if launched_from_app_bundle() {
        cua_platform_macos::request_input_monitoring_access()
    } else {
        permission
    };
    if permission == PermissionState::Granted {
        start_double_control_listener(tx);
    } else {
        tx.send(VoiceUiEvent::Error(
            "Input Monitoring permission is required for the double-Control shortcut.".to_string(),
        ))
        .ok();
    }
}

fn request_screen_recording_access_if_packaged_app() {
    if launched_from_app_bundle() {
        let _ = cua_platform_macos::request_screen_recording_access();
    }
}

fn launched_from_app_bundle() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(|path| path.to_string()))
        .is_some_and(|path| path.contains(".app/Contents/MacOS/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_voice_turn_ignores_duplicate_arm_event() {
        let (tx, rx) = channel::<VoiceUiEvent>();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let mut hud = VoiceHud::new(
            rx,
            tx.clone(),
            VoiceConfig::default(),
            runtime,
            true,
            UiMode::Headful,
        );
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
            "/Applications/cua.app/Contents/MacOS/cua-voice"
        )));
        assert!(!demo_should_run_for_path(
            true,
            Path::new("/Applications/cua.app/Contents/MacOS/cua-voice")
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
    fn step_label_accepts_declarative_totals_beyond_voice_defaults() {
        assert_eq!(
            step_label(37, 120, "verifying the selected window"),
            "Step 37/120   verifying the selected window"
        );
    }

    #[test]
    fn idle_center_text_does_not_show_zero_step_counter() {
        let snapshot = HudSnapshot::default();

        assert_eq!(center_status_text(&snapshot), "Ready");
    }

    #[test]
    fn ui_mode_flags_default_headful_and_accept_headless() {
        assert_eq!(ui_mode_from_flags(false, false), UiMode::Headful);
        assert_eq!(ui_mode_from_flags(true, false), UiMode::Headful);
        assert_eq!(ui_mode_from_flags(false, true), UiMode::Headless);
    }

    #[test]
    fn hud_constructor_applies_initial_ui_mode() {
        let (_tx, rx) = channel::<VoiceUiEvent>();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        let hud = VoiceHud::new(
            rx,
            channel::<VoiceUiEvent>().0,
            VoiceConfig::default(),
            runtime,
            true,
            UiMode::Headless,
        );

        assert_eq!(hud.snapshot.mode, UiMode::Headless);
        assert_eq!(hud.snapshot.input_label, "automation");
    }

    #[test]
    fn active_center_text_uses_protocol_step_label() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::AgentStep {
            label: "Opening Safari with cua".to_string(),
            source: Some("remote".to_string()),
            task: Some("Web browsing".to_string()),
            tool: Some("Unix socket".to_string()),
            step_index: Some(2),
            step_total: Some(5),
            ttl_ms: Some(2_000),
        });

        assert_eq!(
            center_status_text(&snapshot),
            "Step 2/5   Opening Safari with cua"
        );
    }

    #[test]
    fn marquee_stays_still_for_short_center_text() {
        assert_eq!(
            marquee_offset_px("Step 1/4   Ready", CENTER_LABEL_WIDTH, 10.0),
            0.0
        );
    }

    #[test]
    fn marquee_waits_then_scrolls_long_center_text() {
        let text =
            "Step 7/120   validating a long custom agent step that needs to reveal itself slowly";

        assert_eq!(
            marquee_offset_px(text, CENTER_LABEL_WIDTH, MARQUEE_START_DELAY_SECS - 0.1),
            0.0
        );
        assert_eq!(
            marquee_offset_px(text, CENTER_LABEL_WIDTH, MARQUEE_START_DELAY_SECS),
            0.0
        );
        assert!(marquee_offset_px(text, CENTER_LABEL_WIDTH, MARQUEE_START_DELAY_SECS + 1.0) > 0.0);
    }

    #[test]
    fn marquee_holds_at_end_before_looping() {
        let text =
            "Step 9/42   long custom step label for reviewing every visible change carefully";
        let overflow = (estimated_center_text_width_px(text) - CENTER_LABEL_WIDTH).max(0.0);
        let scroll_duration = overflow / MARQUEE_SCROLL_SPEED_PX_PER_SEC;
        let end_hold_time = MARQUEE_START_DELAY_SECS + scroll_duration + 0.2;

        assert_eq!(
            marquee_offset_px(text, CENTER_LABEL_WIDTH, end_hold_time),
            overflow
        );
    }

    #[test]
    fn activity_dots_are_static_when_idle() {
        let snapshot = HudSnapshot::default();

        assert!(!dots_are_active(&snapshot));
        assert_eq!(activity_dot_alpha(0, 0.0, false, 0.0), 0.24);
        assert_eq!(activity_dot_alpha(0, 10.0, false, 0.0), 0.24);
        assert_eq!(activity_dot_alpha(5, 10.0, false, 0.0), 0.24);
    }

    #[test]
    fn activity_dots_run_a_circular_trailing_chase_when_active() {
        let speed = dot_pulse_speed(&HudPhase::Dispatching);
        let start = (0..ACTIVITY_DOT_COUNT)
            .map(|index| activity_dot_alpha(index, 0.0, true, speed))
            .collect::<Vec<_>>();
        let later = (0..ACTIVITY_DOT_COUNT)
            .map(|index| activity_dot_alpha(index, 1.0 / speed, true, speed))
            .collect::<Vec<_>>();
        let wrapped = (0..ACTIVITY_DOT_COUNT)
            .map(|index| activity_dot_alpha(index, ACTIVITY_DOT_COUNT as f32 / speed, true, speed))
            .collect::<Vec<_>>();
        let brightest_start = start
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.partial_cmp(right).unwrap())
            .map(|(index, _)| index);
        let brightest_later = later
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| left.partial_cmp(right).unwrap())
            .map(|(index, _)| index);

        assert_eq!(brightest_start, Some(0));
        assert_eq!(brightest_later, Some(1));
        assert_eq!(wrapped, start);
        assert_eq!(start[0], 1.0);
        assert_eq!(start[5], 0.72);
        assert_eq!(start[4], 0.44);
        assert!(start[0] > start[3]);
    }

    #[test]
    fn activity_dots_keep_one_neon_blue_family() {
        let head = activity_dot_style(0, 0.0, true, 8.0);
        let immediate_trail = activity_dot_style(5, 0.0, true, 8.0);
        let older_trail = activity_dot_style(4, 0.0, true, 8.0);
        let dormant = activity_dot_style(3, 0.0, true, 8.0);

        assert!(head.alpha > immediate_trail.alpha);
        assert!(immediate_trail.alpha > older_trail.alpha);
        assert!(older_trail.alpha > dormant.alpha);
        assert!(head.lightness < immediate_trail.lightness);
        assert!(immediate_trail.lightness < older_trail.lightness);
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
