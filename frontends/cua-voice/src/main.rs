use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait};
use cua_core::{PermissionState, UiMode};
use cua_voice::activation::ControlDoubleTap;
use cua_voice::agent_events::{
    agent_reply_from_daemon_event, agent_step_from_daemon_event,
    agent_ui_event_from_daemon_event_advancing_cursor,
};
use cua_voice::client::CuaClient;
use cua_voice::daemon::{profile_daemon_is_alive, spawn_profile_daemon};
use cua_voice::hud::{
    compact_label, HudDisplay, HudMetrics, COMPACT_HEIGHT, EXPANDED_HEIGHT, TOP_MARGIN,
    WINDOW_HEIGHT, WINDOW_WIDTH,
};
use cua_voice::orb::paint_orb;
use cua_voice::stt::{DEFAULT_STT_BACKEND, DEFAULT_STT_MODEL};
use cua_voice::ui_state::{HudPhase, HudSnapshot, VoiceUiEvent};
use cua_voice::{
    run_text_turn_checked, run_voice_turn_checked, run_voice_turn_until, run_wav_turn_checked,
    VoiceConfig,
};
use gpui::{
    canvas, div, hsla, point, prelude::*, px, rgb, size, AnyElement, App, Application, Bounds,
    BoxShadow, Context, Div, IntoElement, MouseButton as GpuiMouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement, Pixels, Point, Render, Styled, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CENTER_LABEL_WIDTH: f32 = 270.0;
const MARQUEE_START_DELAY_SECS: f32 = 1.6;
const MARQUEE_END_HOLD_SECS: f32 = 0.9;
const MARQUEE_SCROLL_SPEED_PX_PER_SEC: f32 = 24.0;
const MARQUEE_CHAR_WIDTH_PX: f32 = 6.2;
const ACTIVITY_DOT_COUNT: usize = 6;
const CONTROL_SHORTCUT_POLL_INTERVAL: Duration = Duration::from_millis(4);
const EDGE_SNAP_MARGIN_PX: f32 = 96.0;
const MINIMIZED_WIDTH: f32 = 38.0;
const MINIMIZED_HEIGHT: f32 = 28.0;
const MINIMIZED_RADIUS: f32 = 14.0;
const MINIMIZED_RIGHT_OFFSET: f32 = 220.0;
const UI_TEXT_PX: f32 = 12.0;
const UI_META_PX: f32 = 11.0;

#[derive(Debug, Parser)]
#[command(name = "cua-voice", version, about = "Rust voice HUD for cua")]
struct Args {
    #[arg(long, default_value = "default")]
    profile: String,
    #[arg(long, default_value_t = 4500)]
    record_ms: u64,
    #[arg(long)]
    list_input_devices: bool,
    #[arg(long, default_value = DEFAULT_STT_BACKEND)]
    stt_backend: String,
    #[arg(long, default_value = DEFAULT_STT_MODEL)]
    stt_model: String,
    #[arg(long, default_value = "anthropic/claude-sonnet-5")]
    planner_model: String,
    #[arg(long, env = "CUA_VOICE_DEBUG_TRACE")]
    debug_trace: bool,
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
    snapshot: HudSnapshot,
    started: Instant,
    last_frame: Instant,
    center_text_key: String,
    center_text_since: Instant,
    response_progress: f32,
    expansion_progress: f32,
    minimized_progress: f32,
    expanded: bool,
    minimized: bool,
    chrome_visible: bool,
    drag: Option<IslandDrag>,
    model_label: String,
    island_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    last_island_toggle_at: Option<Instant>,
}

#[derive(Clone, Copy, Debug)]
struct IslandDrag {
    start_cursor: Point<Pixels>,
    start_bounds: Bounds<Pixels>,
}

impl VoiceHud {
    fn new(
        rx: Receiver<VoiceUiEvent>,
        mode: UiMode,
        model_label: String,
        island_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    ) -> Self {
        let mut snapshot = HudSnapshot::default();
        let source = initial_ui_source(&mode).to_string();
        snapshot.apply(VoiceUiEvent::UiMode {
            mode,
            source: Some(source),
        });
        Self {
            rx,
            snapshot,
            started: Instant::now(),
            last_frame: Instant::now(),
            center_text_key: String::new(),
            center_text_since: Instant::now(),
            response_progress: 0.0,
            expansion_progress: 0.0,
            minimized_progress: 0.0,
            expanded: false,
            minimized: false,
            chrome_visible: false,
            drag: None,
            model_label,
            island_bounds,
            last_island_toggle_at: None,
        }
    }

    fn drain_events(&mut self) -> Option<bool> {
        let mut expansion_command = None;
        while let Ok(event) = self.rx.try_recv() {
            match event {
                VoiceUiEvent::ToggleExpanded => {
                    if self.accept_island_toggle(Instant::now()) {
                        expansion_command = Some(!expansion_command.unwrap_or(self.expanded));
                    }
                }
                VoiceUiEvent::SetExpanded(expanded) => {
                    expansion_command = Some(expanded);
                }
                VoiceUiEvent::AutomationActivity {
                    label,
                    source,
                    tool,
                } if self.automation_activity_toggles_island(&label) => {
                    if self.accept_island_toggle(Instant::now()) {
                        expansion_command = Some(!expansion_command.unwrap_or(self.expanded));
                    }
                    self.snapshot.apply(VoiceUiEvent::AutomationActivity {
                        label,
                        source,
                        tool,
                    });
                }
                event => self.snapshot.apply(event),
            }
        }
        expansion_command
    }

    fn accept_island_toggle(&mut self, now: Instant) -> bool {
        if self
            .last_island_toggle_at
            .is_some_and(|previous| now.duration_since(previous) < Duration::from_millis(260))
        {
            return false;
        }
        self.last_island_toggle_at = Some(now);
        true
    }

    fn automation_activity_toggles_island(&self, label: &str) -> bool {
        let Some(point) = automation_double_click_point(label) else {
            return false;
        };
        let bounds = self.island_bounds.lock().ok().and_then(|current| *current);
        bounds.is_some_and(|bounds| point_inside_bounds(point, bounds))
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

    fn render_surface(
        &self,
        display: &HudDisplay,
        metrics: HudMetrics,
        center_text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if minimized_content_visible(self.minimized_progress) {
            self.minimized_icon(cx).into_any_element()
        } else {
            self.island_surface(display, metrics, center_text, cx)
                .into_any_element()
        }
    }

    fn chip(label: impl Into<String>) -> impl IntoElement {
        div()
            .px_1()
            .py_0p5()
            .rounded(px(4.0))
            .bg(hsla(0.0, 0.0, 1.0, 0.10))
            .text_color(rgb(0xb9b9c0))
            .text_size(px(UI_TEXT_PX))
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
        let expansion_target = if self.expanded && !self.minimized {
            1.0
        } else {
            0.0
        };
        let expansion_step = (dt * 13.0).clamp(0.0, 1.0);
        self.expansion_progress += (expansion_target - self.expansion_progress) * expansion_step;
        if (self.expansion_progress - expansion_target).abs() < 0.006 {
            self.expansion_progress = expansion_target;
        }
        let minimized_target = if self.minimized { 1.0 } else { 0.0 };
        let minimized_step = (dt * 14.0).clamp(0.0, 1.0);
        self.minimized_progress += (minimized_target - self.minimized_progress) * minimized_step;
        if (self.minimized_progress - minimized_target).abs() < 0.006 {
            self.minimized_progress = minimized_target;
        }
    }

    fn sync_center_text(&mut self, center_text: &str) {
        if self.center_text_key != center_text {
            self.center_text_key = center_text.to_string();
            self.center_text_since = Instant::now();
        }
    }

    fn island_surface(
        &self,
        display: &HudDisplay,
        metrics: HudMetrics,
        center: String,
        cx: &mut Context<Self>,
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
            display.target.clone()
        };

        div()
            .w(px(island_width(metrics)))
            .h(px(island_height(metrics)))
            .rounded(px(island_radius(metrics)))
            .overflow_hidden()
            .group("cua-island")
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
            .relative()
            .id("cua-island-shell")
            .on_mouse_down(
                GpuiMouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.drag = Some(IslandDrag {
                        start_cursor: current_cursor_point(),
                        start_bounds: window.bounds(),
                    });
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if event.pressed_button == Some(GpuiMouseButton::Left) {
                    if let Some(drag) = this.drag {
                        let cursor = current_cursor_point();
                        let dx = cursor.x - drag.start_cursor.x;
                        let dy = cursor.y - drag.start_cursor.y;
                        let mut bounds = drag.start_bounds;
                        bounds.origin.x = drag.start_bounds.origin.x + dx;
                        bounds.origin.y = drag.start_bounds.origin.y + dy;
                        window.set_bounds(bounds);
                        cx.notify();
                        cx.stop_propagation();
                    }
                }
            }))
            .on_mouse_up(
                GpuiMouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.finish_drag(window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up_out(
                GpuiMouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.finish_drag(window, cx);
                    cx.stop_propagation();
                }),
            )
            .px_3()
            .flex()
            .flex_col()
            .child(self.stoplights(cx))
            .child(
                div()
                    .h(px(COMPACT_HEIGHT))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(self.orb())
                    .child(
                        div()
                            .w(px(190.0))
                            .truncate()
                            .text_color(rgb(0x9f9fa6))
                            .text_size(px(UI_TEXT_PX))
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
                    .child(self.activity_dots()),
            )
            .child(self.expanded_body(display, metrics))
    }

    fn stoplights(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .left(px(8.0))
            .top(px(3.0))
            .flex()
            .items_center()
            .gap_0p5()
            .opacity(if self.chrome_visible { 0.92 } else { 0.0 })
            .hover(|style| style.opacity(1.0))
            .child(stoplight(0xff5f57).on_mouse_down(
                GpuiMouseButton::Left,
                cx.listener(|_, _, _, cx| {
                    cx.quit();
                    cx.stop_propagation();
                }),
            ))
            .child(stoplight(0xffbd2e).on_mouse_down(
                GpuiMouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.minimized = true;
                    this.expanded = false;
                    this.drag = None;
                    cx.notify();
                    cx.stop_propagation();
                }),
            ))
            .child(stoplight(0x28c840).on_mouse_down(
                GpuiMouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.toggle_expanded(window, cx);
                    cx.stop_propagation();
                }),
            ))
    }

    fn toggle_expanded(&mut self, window: &mut Window, cx: &mut App) {
        self.expanded = !self.expanded;
        self.minimized = false;
        self.drag = None;
        window.refresh();
        if let Some(display) = window.display(cx) {
            let bounds = animated_island_bounds(
                window.bounds(),
                HudMetrics::with_expansion(self.response_progress, self.expansion_progress),
                self.minimized_progress,
                display.bounds(),
            );
            window.set_bounds(bounds);
        }
    }

    fn set_expanded_from_render(
        &mut self,
        expanded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.expanded = expanded;
        self.minimized = false;
        self.drag = None;
        cx.notify();
        if let Some(display) = window.display(cx) {
            let bounds = animated_island_bounds(
                window.bounds(),
                HudMetrics::with_expansion(self.response_progress, self.expansion_progress),
                self.minimized_progress,
                display.bounds(),
            );
            window.set_bounds(bounds);
            self.remember_island_bounds(bounds);
        }
    }

    fn remember_island_bounds(&self, bounds: Bounds<Pixels>) {
        if let Ok(mut current) = self.island_bounds.lock() {
            *current = Some(bounds);
        }
    }

    fn minimized_icon(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(MINIMIZED_WIDTH))
            .h(px(MINIMIZED_HEIGHT))
            .rounded(px(MINIMIZED_RADIUS))
            .overflow_hidden()
            .bg(hsla(0.0, 0.0, 0.0, 0.90))
            .border_1()
            .border_color(hsla(0.0, 0.0, 1.0, 0.16))
            .shadow(vec![BoxShadow {
                color: hsla(0.0, 0.0, 0.0, 0.48),
                blur_radius: px(14.0),
                spread_radius: px(0.0),
                offset: point(px(0.0), px(4.0)),
            }])
            .flex()
            .items_center()
            .justify_center()
            .id("cua-island-restore")
            .on_click(cx.listener(|this, _, window, cx| {
                this.minimized = false;
                this.expanded = false;
                this.drag = None;
                cx.notify();
                if let Some(display) = window.display(cx) {
                    let bounds = animated_island_bounds(
                        window.bounds(),
                        HudMetrics::with_expansion(this.response_progress, this.expansion_progress),
                        this.minimized_progress,
                        display.bounds(),
                    );
                    window.set_bounds(bounds);
                }
                cx.stop_propagation();
            }))
            .child(self.orb())
    }

    fn expanded_body(&self, display: &HudDisplay, metrics: HudMetrics) -> impl IntoElement {
        let step_total = self.snapshot.step.total.max(1);
        let step_index = self.snapshot.step.index.min(step_total);
        let response = expanded_response_text(display);
        div()
            .opacity(metrics.expansion_opacity)
            .h(px((EXPANDED_HEIGHT - COMPACT_HEIGHT).max(0.0)))
            .border_t_1()
            .border_color(hsla(0.0, 0.0, 1.0, 0.08))
            .px_3()
            .pb_4()
            .pt_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(index_tab("01", "Task", true))
                            .child(
                                div()
                                    .w(px(540.0))
                                    .truncate()
                                    .text_color(rgb(0xd8d8de))
                                    .text_size(px(UI_TEXT_PX))
                                    .child(display.prompt.clone()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .flex_none()
                            .child(index_tab("02", "Step", true))
                            .child(
                                div()
                                    .h(px(15.0))
                                    .px_1()
                                    .rounded(px(3.0))
                                    .bg(hsla(0.0, 0.0, 1.0, 0.045))
                                    .flex()
                                    .items_center()
                                    .text_color(rgb(0x8d8d96))
                                    .text_size(px(UI_META_PX))
                                    .child(format!("{step_index}/{step_total}")),
                            )
                            .child(step_segments(step_index, step_total)),
                    ),
            )
            .child(
                div()
                    .h(px(205.0))
                    .border_t_1()
                    .border_b_1()
                    .border_color(hsla(0.0, 0.0, 1.0, 0.075))
                    .py_3()
                    .overflow_hidden()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(index_tab("03", "Response", true))
                    .child(
                        div()
                            .whitespace_normal()
                            .line_height(px(16.0))
                            .text_color(rgb(0xd2d2d8))
                            .text_size(px(UI_TEXT_PX))
                            .child(response),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_6()
                    .child(self.current_action_panel(display))
                    .child(self.tools_panel(display)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(UI_META_PX))
                    .text_color(rgb(0x74747d))
                    .child(format!("Elapsed {}", elapsed_label(self.started.elapsed())))
                    .child(format!("Model {}", compact_label(&self.model_label, 30)))
                    .child(format!("Transport {}", display.tool)),
            )
    }

    fn current_action_panel(&self, display: &HudDisplay) -> impl IntoElement {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_1()
            .child(info_row(
                "04",
                "Action",
                compact_label(&self.snapshot.step.label, 56),
                true,
            ))
            .child(info_row("05", "Phase", display.phase.to_string(), false))
            .child(info_row(
                "06",
                "State",
                if dots_are_active(&self.snapshot) {
                    "Live"
                } else {
                    "Idle"
                },
                dots_are_active(&self.snapshot),
            ))
    }

    fn tools_panel(&self, display: &HudDisplay) -> impl IntoElement {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_1()
            .child(tool_row("07", &display.rows[0]))
            .child(tool_row("08", &display.rows[1]))
    }

    fn finish_drag(&mut self, window: &mut Window, cx: &mut App) {
        self.drag = None;
        if let Some(display) = window.display(cx) {
            let snapped = snap_island_bounds(window.bounds(), display.bounds());
            window.set_bounds(snapped);
        }
    }
}

fn dots_are_active(snapshot: &HudSnapshot) -> bool {
    !matches!(snapshot.phase, HudPhase::Idle)
}

fn dot_pulse_speed(phase: &HudPhase) -> f32 {
    match phase {
        HudPhase::Listening | HudPhase::RecordingStopped | HudPhase::Dispatching => 8.0,
        HudPhase::Accepted | HudPhase::Planning | HudPhase::Transcribing => 6.0,
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
        .text_size(px(UI_TEXT_PX))
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

fn stoplight(color: u32) -> Div {
    div()
        .w(px(12.0))
        .h(px(9.0))
        .rounded_full()
        .opacity(0.9)
        .hover(|style| style.opacity(1.0))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(5.0))
                .h(px(5.0))
                .rounded_full()
                .bg(rgb(color))
                .border_1()
                .border_color(hsla(0.0, 0.0, 0.0, 0.35)),
        )
}

fn index_tab(index: &'static str, label: &'static str, active: bool) -> impl IntoElement {
    div()
        .h(px(16.0))
        .px_1p5()
        .rounded(px(3.0))
        .bg(if active {
            hsla(210.0 / 360.0, 1.0, 0.50, 0.10)
        } else {
            hsla(0.0, 0.0, 1.0, 0.04)
        })
        .flex()
        .items_center()
        .gap_1()
        .child(
            div()
                .text_size(px(UI_META_PX))
                .text_color(if active { rgb(0x66c7ff) } else { rgb(0x74747d) })
                .child(index),
        )
        .child(
            div()
                .text_size(px(UI_META_PX))
                .text_color(if active { rgb(0xb9b9c0) } else { rgb(0x85858d) })
                .child(label),
        )
}

fn info_row(
    index: &'static str,
    label: &'static str,
    value: impl Into<String>,
    active: bool,
) -> impl IntoElement {
    div()
        .h(px(24.0))
        .flex()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.055))
        .child(index_tab(index, label, active))
        .child(
            div()
                .flex_1()
                .truncate()
                .text_size(px(UI_TEXT_PX))
                .text_color(rgb(0xd8d8de))
                .child(value.into()),
        )
}

fn tool_row(index: &'static str, row: &cua_voice::hud::HudRow) -> impl IntoElement {
    div()
        .h(px(24.0))
        .flex()
        .items_center()
        .gap_3()
        .border_b_1()
        .border_color(hsla(0.0, 0.0, 1.0, 0.055))
        .child(index_tab(index, "Tool", false))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .flex_1()
                .child(
                    div()
                        .truncate()
                        .text_color(rgb(0xd8d8de))
                        .text_size(px(UI_TEXT_PX))
                        .child(row.label.clone()),
                )
                .child(
                    div()
                        .w(px(96.0))
                        .truncate()
                        .text_color(rgb(0x85858d))
                        .text_size(px(UI_META_PX))
                        .child(format!("{}  {}", row.tool, row.app)),
                ),
        )
        .child(
            div()
                .w(px(44.0))
                .text_size(px(UI_META_PX))
                .text_color(rgb(0x74747d))
                .child(row.age.clone()),
        )
}

fn step_segments(index: usize, total: usize) -> impl IntoElement {
    let total = total.clamp(1, 24);
    let complete = index.min(total);
    let mut row = div().flex().items_center().gap_0p5();
    for segment in 0..total {
        row = row.child(
            div()
                .w(px(3.0))
                .h(px(2.0))
                .rounded_full()
                .bg(if segment < complete {
                    hsla(210.0 / 360.0, 1.0, 0.58, 0.86)
                } else {
                    hsla(0.0, 0.0, 1.0, 0.10)
                }),
        );
    }
    row
}

fn expanded_response_text(display: &HudDisplay) -> String {
    let text = if display.result.trim().is_empty() {
        display.prompt.as_str()
    } else {
        display.result.as_str()
    };
    compact_label(text, 460)
}

fn elapsed_label(duration: Duration) -> String {
    let secs = duration.as_secs();
    let minutes = secs / 60;
    let seconds = secs % 60;
    format!("{minutes:02}:{seconds:02}")
}

fn island_width(metrics: HudMetrics) -> f32 {
    metrics.width
}

fn island_height(metrics: HudMetrics) -> f32 {
    metrics.height
}

fn island_radius(metrics: HudMetrics) -> f32 {
    metrics.radius
}

fn minimized_content_visible(progress: f32) -> bool {
    progress >= 0.55
}

#[cfg(test)]
fn compact_bar_width(_: HudMetrics) -> f32 {
    cua_voice::hud::COMPACT_WIDTH
}

#[cfg(test)]
fn compact_bar_height(_: HudMetrics) -> f32 {
    COMPACT_HEIGHT
}

#[cfg(test)]
fn compact_bar_radius(_: HudMetrics) -> f32 {
    cua_voice::hud::COMPACT_RADIUS
}

fn response_flash_visible(metrics: HudMetrics) -> bool {
    metrics.response_opacity >= 0.35
}

fn point_inside_bounds(point: Point<Pixels>, bounds: Bounds<Pixels>) -> bool {
    point.x >= bounds.origin.x
        && point.y >= bounds.origin.y
        && point.x <= bounds.origin.x + bounds.size.width
        && point.y <= bounds.origin.y + bounds.size.height
}

fn automation_double_click_point(label: &str) -> Option<Point<Pixels>> {
    let normalized = label.trim();
    let (prefix, count) = normalized.rsplit_once(" x")?;
    if count.parse::<u8>().ok()? < 2 {
        return None;
    }
    let coords = prefix.strip_prefix("Left mouse click at ")?;
    let (x, y) = coords.split_once(',')?;
    Some(point(
        px(x.trim().parse::<f32>().ok()?),
        px(y.trim().parse::<f32>().ok()?),
    ))
}

fn step_label(index: usize, total: usize, label: &str) -> String {
    format!("Step {index}/{total}   {label}")
}

fn center_status_text(snapshot: &HudSnapshot) -> String {
    if snapshot.phase == HudPhase::Idle && snapshot.step.index == 0 {
        return snapshot.step.label.clone();
    }
    if snapshot.phase == HudPhase::Listening {
        return "Listening".to_string();
    }
    if snapshot.phase == HudPhase::RecordingStopped {
        return "Recording Stopped".to_string();
    }
    if snapshot.phase == HudPhase::Transcribing {
        return "Processing".to_string();
    }
    if snapshot.phase == HudPhase::Accepted {
        return "Accepted".to_string();
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let expansion_command = self.drain_events();
        let reply_window_expired =
            self.snapshot.expanded_until.is_some() && !self.snapshot.is_expanded();
        self.tick_animation();
        if self.snapshot.expire_transcript(Instant::now()) {
            cx.notify();
        }
        if let Some(expanded) = expansion_command {
            self.set_expanded_from_render(expanded, window, cx);
        }
        if should_reset_after_reply_collapse(reply_window_expired, self.response_progress) {
            self.snapshot.apply(VoiceUiEvent::Idle);
        }
        let chrome_visible = point_inside_bounds(current_cursor_point(), window.bounds());
        if self.chrome_visible != chrome_visible {
            self.chrome_visible = chrome_visible;
            cx.notify();
        }
        self.snapshot.expire_programmed_step(Instant::now());
        window.request_animation_frame();
        let display = HudDisplay::from_snapshot(&self.snapshot);
        let metrics = HudMetrics::with_expansion(self.response_progress, self.expansion_progress);
        let center_text = center_text_for(&display, &self.snapshot, metrics);
        self.sync_center_text(&center_text);
        if self.drag.is_none() {
            if let Some(display) = window.display(cx) {
                let bounds = animated_island_bounds(
                    window.bounds(),
                    metrics,
                    self.minimized_progress,
                    display.bounds(),
                );
                window.set_bounds(bounds);
                self.remember_island_bounds(bounds);
            }
        } else {
            self.remember_island_bounds(window.bounds());
        }
        div()
            .size_full()
            .relative()
            .child(self.render_surface(&display, metrics, center_text, cx))
            .into_any_element()
    }
}

fn main() -> anyhow::Result<()> {
    load_cua_dotenv();
    let args = Args::parse();
    if args.list_input_devices {
        print_input_devices()?;
        return Ok(());
    }
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
        stt_backend: args.stt_backend,
        stt_model: args.stt_model,
        planner_model: args.planner_model,
        debug_trace: args.debug_trace,
    };
    let model_label = config.planner_model.clone();
    let runtime = Arc::new(tokio::runtime::Runtime::new()?);
    let (tx, rx) = channel::<VoiceUiEvent>();
    let island_bounds = Arc::new(Mutex::new(None::<Bounds<Pixels>>));
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

    let should_request_desktop_access = !demo;
    let desktop_access_profile = config.profile.clone();
    if demo {
        start_demo_cycle(tx.clone());
    } else {
        start_embedded_daemon_if_needed(config.profile.clone(), runtime.clone(), tx.clone());
        start_control_shortcut_controller(config.clone(), runtime.clone(), tx.clone());
        start_island_double_tap_listener(tx.clone(), island_bounds.clone());
        start_agent_step_poll(config.profile.clone(), runtime.clone(), tx.clone());
    }
    Application::new().run(move |cx: &mut App| {
        if should_request_desktop_access {
            request_desktop_access_once_if_packaged_app(&desktop_access_profile);
        }
        let bounds = top_centered_bounds(cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: None,
                focus: true,
                kind: WindowKind::PopUp,
                is_resizable: false,
                is_minimizable: false,
                mouse_passthrough: false,
                window_background: WindowBackgroundAppearance::Transparent,
                ..Default::default()
            },
            move |_, cx| {
                cx.new(|_| {
                    VoiceHud::new(
                        rx,
                        ui_mode.clone(),
                        model_label.clone(),
                        island_bounds.clone(),
                    )
                })
            },
        )
        .unwrap();
    });
    Ok(())
}

fn print_input_devices() -> anyhow::Result<()> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().map(|device| device.to_string());
    let devices = host
        .input_devices()?
        .map(|device| {
            let name = device.to_string();
            let default = default_name.as_deref() == Some(name.as_str());
            let config = device.default_input_config().ok();
            serde_json::json!({
                "name": name,
                "default": default,
                "channels": config.as_ref().map(|config| config.channels()),
                "sample_rate": config.as_ref().map(|config| config.sample_rate()),
                "sample_format": config.as_ref().map(|config| format!("{:?}", config.sample_format())),
            })
        })
        .collect::<Vec<_>>();
    println!(
        "{}",
        serde_json::json!({
            "event": "input_devices",
            "default": default_name,
            "devices": devices
        })
    );
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
            VoiceUiEvent::RecordingStopped => serde_json::json!({"event": "recording_stopped"}),
            VoiceUiEvent::Accepted => serde_json::json!({"event": "accepted"}),
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
            VoiceUiEvent::AutomationReply(text) => {
                serde_json::json!({"event": "automation_reply", "text": text})
            }
            VoiceUiEvent::Error(text) => serde_json::json!({"event": "error", "text": text}),
            VoiceUiEvent::AudioDiagnostic {
                device_name,
                channels,
                sample_format,
                sample_rate,
                duration_ms,
                peak_amplitude,
                rms_amplitude_ppm,
                wav_bytes,
            } => serde_json::json!({
                "event": "audio_diagnostic",
                "device_name": device_name,
                "channels": channels,
                "sample_format": sample_format,
                "sample_rate": sample_rate,
                "duration_ms": duration_ms,
                "peak_amplitude": peak_amplitude,
                "rms_amplitude_ppm": rms_amplitude_ppm,
                "wav_bytes": wav_bytes
            }),
            VoiceUiEvent::SttDiagnostic {
                backend,
                model,
                generation_id,
                audio_ms,
                transcript_class,
            } => serde_json::json!({
                "event": "stt_diagnostic",
                "backend": backend,
                "model": model,
                "generation_id": generation_id,
                "audio_ms": audio_ms,
                "transcript_class": transcript_class
            }),
            VoiceUiEvent::Metric { name, ms } => {
                serde_json::json!({"event": "metric", "name": name, "ms": ms})
            }
            VoiceUiEvent::ToggleExpanded => serde_json::json!({"event": "toggle_expanded"}),
            VoiceUiEvent::SetExpanded(expanded) => {
                serde_json::json!({"event": "set_expanded", "expanded": expanded})
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
            tx.send(VoiceUiEvent::Error("Invalid cua profile path".to_string()))
                .ok();
            return;
        };
        let mut last_sequence = 0_u64;
        loop {
            let Ok(mut session) = client.session().await else {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            };
            loop {
                match session.events_wait(last_sequence, 1_000).await {
                    Ok(events) => {
                        for event in events {
                            if let Some(event) = agent_ui_event_from_daemon_event_advancing_cursor(
                                &event,
                                &mut last_sequence,
                            ) {
                                tx.send(event).ok();
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
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

fn animated_island_bounds(
    current: Bounds<Pixels>,
    metrics: HudMetrics,
    minimized_progress: f32,
    display_bounds: Bounds<Pixels>,
) -> Bounds<Pixels> {
    let minimized_progress = cua_voice::hud::ease_out_cubic(minimized_progress.clamp(0.0, 1.0));
    let display_left = display_bounds.origin.x.to_f64() as f32;
    let display_right =
        display_bounds.origin.x.to_f64() as f32 + display_bounds.size.width.to_f64() as f32;
    let width = cua_voice::hud::lerp(metrics.width, MINIMIZED_WIDTH, minimized_progress);
    let height = cua_voice::hud::lerp(metrics.height, MINIMIZED_HEIGHT, minimized_progress);
    let current_x = current.origin.x.to_f64() as f32;
    let current_width = current.size.width.to_f64() as f32;
    let current_center = current_x + current_width / 2.0;
    let max_x = (display_right - width).max(display_left);
    let normal_x = (current_center - width / 2.0).clamp(display_left, max_x);
    let minimized_x = (display_right - MINIMIZED_RIGHT_OFFSET - width).clamp(display_left, max_x);
    let x = cua_voice::hud::lerp(normal_x, minimized_x, minimized_progress);
    Bounds {
        origin: point(
            px(x),
            px(display_bounds.origin.y.to_f64() as f32 + TOP_MARGIN),
        ),
        size: size(px(width), px(height)),
    }
}

fn current_cursor_point() -> Point<Pixels> {
    let cursor = cua_platform_macos::cursor_state();
    point(px(cursor.x as f32), px(cursor.y as f32))
}

fn snap_island_bounds(bounds: Bounds<Pixels>, display_bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    let left = display_bounds.origin.x.to_f64() as f32;
    let right = display_bounds.origin.x.to_f64() as f32 + display_bounds.size.width.to_f64() as f32;
    let top = display_bounds.origin.y.to_f64() as f32 + TOP_MARGIN;
    let width = bounds.size.width.to_f64() as f32;
    let raw_x = bounds.origin.x.to_f64() as f32;
    let max_x = (right - width).max(left);
    let mut x = raw_x.clamp(left, max_x);
    if (x - left).abs() <= EDGE_SNAP_MARGIN_PX {
        x = left;
    } else if (max_x - x).abs() <= EDGE_SNAP_MARGIN_PX {
        x = max_x;
    }
    Bounds {
        origin: point(px(x), px(top)),
        size: bounds.size,
    }
}

fn start_demo_cycle(tx: Sender<VoiceUiEvent>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(900));
        let sequence = [
            VoiceUiEvent::Armed,
            VoiceUiEvent::Listening { ms: 1200 },
            VoiceUiEvent::Accepted,
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

fn start_control_shortcut_controller(
    config: VoiceConfig,
    runtime: Arc<tokio::runtime::Runtime>,
    tx: Sender<VoiceUiEvent>,
) {
    let (shortcut_tx, shortcut_rx) = channel::<()>();
    start_double_control_listener(shortcut_tx);
    std::thread::spawn(move || {
        let active_stop = Arc::new(Mutex::new(None::<Arc<AtomicBool>>));
        while shortcut_rx.recv().is_ok() {
            let Some(stop_requested) = shortcut_trigger_requests_start(&active_stop) else {
                continue;
            };
            let run_config = config.clone();
            let run_tx = tx.clone();
            let run_active_stop = active_stop.clone();
            let run_stop_requested = stop_requested.clone();
            runtime.spawn(async move {
                run_voice_turn_until(run_config, run_tx, run_stop_requested.clone()).await;
                shortcut_turn_finished(&run_active_stop, &run_stop_requested);
            });
        }
    });
}

fn shortcut_trigger_requests_start(
    active_stop: &Mutex<Option<Arc<AtomicBool>>>,
) -> Option<Arc<AtomicBool>> {
    let mut guard = active_stop.lock().unwrap();
    if let Some(stop_requested) = guard.as_ref() {
        stop_requested.store(true, Ordering::Release);
        return None;
    }
    let stop_requested = Arc::new(AtomicBool::new(false));
    *guard = Some(stop_requested.clone());
    Some(stop_requested)
}

fn shortcut_turn_finished(
    active_stop: &Mutex<Option<Arc<AtomicBool>>>,
    stop_requested: &Arc<AtomicBool>,
) {
    let mut guard = active_stop.lock().unwrap();
    if guard
        .as_ref()
        .is_some_and(|active| Arc::ptr_eq(active, stop_requested))
    {
        *guard = None;
    }
}

fn start_double_control_listener(tx: Sender<()>) {
    std::thread::spawn(move || {
        let mut detector = ControlDoubleTap::default();
        let mut was_down = false;
        loop {
            if control_poll_sample_triggers_arm(
                &mut detector,
                &mut was_down,
                cua_platform_macos::control_key_is_down(),
                Instant::now(),
            ) {
                tx.send(()).ok();
            }
            std::thread::sleep(CONTROL_SHORTCUT_POLL_INTERVAL);
        }
    });
}

fn start_island_double_tap_listener(
    tx: Sender<VoiceUiEvent>,
    island_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
) {
    let (mouse_tx, mouse_rx) = channel();
    if cua_platform_macos::start_left_mouse_event_monitor(mouse_tx).is_ok() {
        std::thread::spawn(move || {
            let mut detector = ControlDoubleTap::default();
            let mut tap_started_inside = false;
            while let Ok(event) = mouse_rx.recv() {
                let bounds = island_bounds.lock().ok().and_then(|current| *current);
                if island_mouse_event_triggers_toggle(
                    &mut detector,
                    &mut tap_started_inside,
                    event,
                    bounds,
                    Instant::now(),
                ) {
                    tx.send(VoiceUiEvent::ToggleExpanded).ok();
                }
            }
        });
        return;
    }
    std::thread::spawn(move || {
        let mut detector = ControlDoubleTap::default();
        let mut was_down = false;
        let mut tap_started_inside = false;
        loop {
            let bounds = island_bounds.lock().ok().and_then(|current| *current);
            let cursor = current_cursor_point();
            if island_mouse_sample_triggers_toggle(
                &mut detector,
                &mut was_down,
                &mut tap_started_inside,
                cua_platform_macos::left_mouse_button_is_down(),
                cursor,
                bounds,
                Instant::now(),
            ) {
                tx.send(VoiceUiEvent::ToggleExpanded).ok();
            }
            std::thread::sleep(CONTROL_SHORTCUT_POLL_INTERVAL);
        }
    });
}

fn island_mouse_event_triggers_toggle(
    detector: &mut ControlDoubleTap,
    tap_started_inside: &mut bool,
    event: cua_platform_macos::LeftMouseEvent,
    bounds: Option<Bounds<Pixels>>,
    now: Instant,
) -> bool {
    let cursor = point(px(event.x as f32), px(event.y as f32));
    let inside = bounds.is_some_and(|bounds| point_inside_bounds(cursor, bounds));
    match event.kind {
        cua_platform_macos::LeftMouseEventKind::Down => {
            *tap_started_inside = inside;
            if inside {
                detector.key_down();
            }
            false
        }
        cua_platform_macos::LeftMouseEventKind::Up => {
            let valid_tap = *tap_started_inside && inside;
            *tap_started_inside = false;
            valid_tap && detector.key_up(now)
        }
    }
}

fn island_mouse_sample_triggers_toggle(
    detector: &mut ControlDoubleTap,
    was_down: &mut bool,
    tap_started_inside: &mut bool,
    is_down: bool,
    cursor: Point<Pixels>,
    bounds: Option<Bounds<Pixels>>,
    now: Instant,
) -> bool {
    let inside = bounds.is_some_and(|bounds| point_inside_bounds(cursor, bounds));
    let triggered = if is_down && !*was_down {
        *tap_started_inside = inside;
        if inside {
            detector.key_down();
        }
        false
    } else if !is_down && *was_down {
        let valid_tap = *tap_started_inside && inside;
        *tap_started_inside = false;
        valid_tap && detector.key_up(now)
    } else {
        false
    };
    *was_down = is_down;
    triggered
}

fn control_poll_sample_triggers_arm(
    detector: &mut ControlDoubleTap,
    was_down: &mut bool,
    is_down: bool,
    now: Instant,
) -> bool {
    let triggered = if is_down && !*was_down {
        detector.key_down();
        false
    } else {
        !is_down && *was_down && detector.key_up(now)
    };
    *was_down = is_down;
    triggered
}

fn start_embedded_daemon_if_needed(
    profile: String,
    _runtime: Arc<tokio::runtime::Runtime>,
    tx: Sender<VoiceUiEvent>,
) {
    if profile_daemon_is_alive(&profile) {
        return;
    }
    if let Err(error) = spawn_profile_daemon(&profile) {
        tx.send(VoiceUiEvent::Error(format!("Daemon start failed: {error}")))
            .ok();
    }
}

fn request_desktop_access_once_if_packaged_app(profile: &str) {
    if !launched_from_app_bundle() {
        return;
    }
    if cua_platform_macos::permission_report().screen_recording != PermissionState::Granted {
        let _ = request_desktop_permission_once(
            profile,
            "screen-recording",
            || cua_platform_macos::permission_report().screen_recording,
            cua_platform_macos::request_screen_recording_access,
        );
    }
    if cua_platform_macos::permission_report().accessibility_input != PermissionState::Granted {
        let _ = request_desktop_permission_once(
            profile,
            "accessibility-input",
            || cua_platform_macos::permission_report().accessibility_input,
            cua_platform_macos::request_accessibility_input_access,
        );
    }
    if cua_platform_macos::microphone_permission() != PermissionState::Granted {
        let _ = cua_platform_macos::request_microphone_access();
    }
}

fn launched_from_app_bundle() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(|path| path.to_string()))
        .is_some_and(|path| path.contains(".app/Contents/MacOS/"))
}

fn request_desktop_permission_once(
    profile: &str,
    permission: &str,
    current: impl FnOnce() -> PermissionState,
    request: impl FnOnce() -> PermissionState,
) -> PermissionState {
    let path = desktop_permission_prompt_marker_path(profile, permission);
    if path.is_file() {
        return current();
    }
    if legacy_desktop_permission_prompt_marker_path(profile, permission).is_file() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let result = current();
        let _ = std::fs::write(&path, format!("{:?}\n", result));
        return result;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let result = request();
    let _ = std::fs::write(&path, format!("{:?}\n", result));
    result
}

fn desktop_permission_prompt_marker_path(_profile: &str, permission: &str) -> PathBuf {
    desktop_permission_prompt_marker_path_under(
        Path::new(&std::env::var("HOME").unwrap_or_else(|_| ".".to_string())),
        permission,
    )
}

fn desktop_permission_prompt_marker_path_under(home: &Path, permission: &str) -> PathBuf {
    home.join(".cua")
        .join("permission-prompts")
        .join("io.saint0x.cua")
        .join(permission)
}

fn legacy_desktop_permission_prompt_marker_path(profile: &str, permission: &str) -> PathBuf {
    legacy_desktop_permission_prompt_marker_path_under(
        Path::new(&std::env::var("HOME").unwrap_or_else(|_| ".".to_string())),
        profile,
        permission,
    )
}

fn legacy_desktop_permission_prompt_marker_path_under(
    home: &Path,
    profile: &str,
    permission: &str,
) -> PathBuf {
    home.join(".cua")
        .join("profiles")
        .join(profile)
        .join("permission-prompts")
        .join(permission)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cua_voice::hud::{COMPACT_RADIUS, COMPACT_WIDTH};

    #[test]
    fn shortcut_controller_toggles_active_voice_turn_stop() {
        let active_stop = Mutex::new(None::<Arc<AtomicBool>>);

        let first = shortcut_trigger_requests_start(&active_stop).unwrap();
        assert!(!first.load(Ordering::Acquire));

        assert!(shortcut_trigger_requests_start(&active_stop).is_none());
        assert!(first.load(Ordering::Acquire));

        shortcut_turn_finished(&active_stop, &first);
        let second = shortcut_trigger_requests_start(&active_stop).unwrap();
        assert!(!second.load(Ordering::Acquire));
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
    fn snap_island_bounds_attaches_to_top_edge_and_clamps_x() {
        let display = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(1512.0), px(982.0)),
        };
        let dropped = Bounds {
            origin: point(px(900.0), px(220.0)),
            size: size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
        };

        let snapped = snap_island_bounds(dropped, display);

        assert_eq!(snapped.origin.x, px(697.0));
        assert_eq!(snapped.origin.y, px(TOP_MARGIN));
        assert_eq!(snapped.size, dropped.size);
    }

    #[test]
    fn snap_island_bounds_magnetizes_near_left_and_right_edges() {
        let display = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(1512.0), px(982.0)),
        };
        let left_drop = Bounds {
            origin: point(px(40.0), px(80.0)),
            size: size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
        };
        let right_drop = Bounds {
            origin: point(px(660.0), px(80.0)),
            size: size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
        };

        assert_eq!(snap_island_bounds(left_drop, display).origin.x, px(0.0));
        assert_eq!(snap_island_bounds(right_drop, display).origin.x, px(697.0));
    }

    #[test]
    fn reply_progress_switches_bar_to_response_flash_mode() {
        assert!(!response_flash_visible(HudMetrics::interpolate(0.20)));
        assert!(response_flash_visible(HudMetrics::interpolate(0.35)));
    }

    #[test]
    fn island_chrome_visibility_tracks_cursor_bounds() {
        let bounds = Bounds {
            origin: point(px(100.0), px(20.0)),
            size: size(px(300.0), px(40.0)),
        };

        assert!(point_inside_bounds(point(px(100.0), px(20.0)), bounds));
        assert!(point_inside_bounds(point(px(250.0), px(40.0)), bounds));
        assert!(!point_inside_bounds(point(px(99.0), px(40.0)), bounds));
        assert!(!point_inside_bounds(point(px(250.0), px(61.0)), bounds));
    }

    #[test]
    fn island_mouse_poll_toggles_only_for_inside_double_tap() {
        let start = Instant::now();
        let bounds = Bounds {
            origin: point(px(100.0), px(20.0)),
            size: size(px(300.0), px(40.0)),
        };
        let inside = point(px(250.0), px(40.0));
        let outside = point(px(80.0), px(40.0));
        let mut detector = ControlDoubleTap::default();
        let mut was_down = false;
        let mut tap_started_inside = false;

        assert!(!island_mouse_sample_triggers_toggle(
            &mut detector,
            &mut was_down,
            &mut tap_started_inside,
            true,
            inside,
            Some(bounds),
            start,
        ));
        assert!(!island_mouse_sample_triggers_toggle(
            &mut detector,
            &mut was_down,
            &mut tap_started_inside,
            false,
            inside,
            Some(bounds),
            start + Duration::from_millis(40),
        ));
        assert!(!island_mouse_sample_triggers_toggle(
            &mut detector,
            &mut was_down,
            &mut tap_started_inside,
            true,
            outside,
            Some(bounds),
            start + Duration::from_millis(120),
        ));
        assert!(!island_mouse_sample_triggers_toggle(
            &mut detector,
            &mut was_down,
            &mut tap_started_inside,
            false,
            inside,
            Some(bounds),
            start + Duration::from_millis(160),
        ));
        assert!(!island_mouse_sample_triggers_toggle(
            &mut detector,
            &mut was_down,
            &mut tap_started_inside,
            true,
            inside,
            Some(bounds),
            start + Duration::from_millis(220),
        ));
        assert!(island_mouse_sample_triggers_toggle(
            &mut detector,
            &mut was_down,
            &mut tap_started_inside,
            false,
            inside,
            Some(bounds),
            start + Duration::from_millis(260),
        ));
    }

    #[test]
    fn island_mouse_event_tap_toggles_only_for_inside_double_tap() {
        let start = Instant::now();
        let bounds = Bounds {
            origin: point(px(100.0), px(20.0)),
            size: size(px(300.0), px(40.0)),
        };
        let mut detector = ControlDoubleTap::default();
        let mut tap_started_inside = false;

        assert!(!island_mouse_event_triggers_toggle(
            &mut detector,
            &mut tap_started_inside,
            cua_platform_macos::LeftMouseEvent {
                kind: cua_platform_macos::LeftMouseEventKind::Down,
                x: 250.0,
                y: 40.0,
            },
            Some(bounds),
            start,
        ));
        assert!(!island_mouse_event_triggers_toggle(
            &mut detector,
            &mut tap_started_inside,
            cua_platform_macos::LeftMouseEvent {
                kind: cua_platform_macos::LeftMouseEventKind::Up,
                x: 250.0,
                y: 40.0,
            },
            Some(bounds),
            start + Duration::from_millis(40),
        ));
        assert!(!island_mouse_event_triggers_toggle(
            &mut detector,
            &mut tap_started_inside,
            cua_platform_macos::LeftMouseEvent {
                kind: cua_platform_macos::LeftMouseEventKind::Down,
                x: 80.0,
                y: 40.0,
            },
            Some(bounds),
            start + Duration::from_millis(120),
        ));
        assert!(!island_mouse_event_triggers_toggle(
            &mut detector,
            &mut tap_started_inside,
            cua_platform_macos::LeftMouseEvent {
                kind: cua_platform_macos::LeftMouseEventKind::Up,
                x: 250.0,
                y: 40.0,
            },
            Some(bounds),
            start + Duration::from_millis(160),
        ));
        assert!(!island_mouse_event_triggers_toggle(
            &mut detector,
            &mut tap_started_inside,
            cua_platform_macos::LeftMouseEvent {
                kind: cua_platform_macos::LeftMouseEventKind::Down,
                x: 250.0,
                y: 40.0,
            },
            Some(bounds),
            start + Duration::from_millis(220),
        ));
        assert!(island_mouse_event_triggers_toggle(
            &mut detector,
            &mut tap_started_inside,
            cua_platform_macos::LeftMouseEvent {
                kind: cua_platform_macos::LeftMouseEventKind::Up,
                x: 250.0,
                y: 40.0,
            },
            Some(bounds),
            start + Duration::from_millis(260),
        ));
    }

    #[test]
    fn automation_double_click_label_parses_only_double_left_clicks() {
        assert_eq!(
            automation_double_click_point("Left mouse click at 756,38 x2"),
            Some(point(px(756.0), px(38.0)))
        );
        assert_eq!(
            automation_double_click_point("Left mouse click at 756,38"),
            None
        );
        assert_eq!(
            automation_double_click_point("Right mouse click at 756,38 x2"),
            None
        );
        assert_eq!(
            automation_double_click_point("Left mouse click at nope x2"),
            None
        );
    }

    #[test]
    fn automation_double_click_on_island_toggles_once() {
        let (tx, rx) = channel();
        let island_bounds = Arc::new(Mutex::new(Some(Bounds {
            origin: point(px(100.0), px(20.0)),
            size: size(px(300.0), px(40.0)),
        })));
        let mut hud = VoiceHud::new(
            rx,
            UiMode::Headless,
            "anthropic/claude-sonnet-5".to_string(),
            island_bounds,
        );

        tx.send(VoiceUiEvent::AutomationActivity {
            label: "Left mouse click at 250,40 x2".to_string(),
            source: Some("Computer control".to_string()),
            tool: Some("Unix socket".to_string()),
        })
        .unwrap();
        assert_eq!(hud.drain_events(), Some(true));

        tx.send(VoiceUiEvent::AutomationActivity {
            label: "Left mouse click at 250,40 x2".to_string(),
            source: Some("Computer control".to_string()),
            tool: Some("Unix socket".to_string()),
        })
        .unwrap();
        assert_eq!(hud.drain_events(), None);
    }

    #[test]
    fn animated_island_bounds_preserves_center_and_top_attachment() {
        let display = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(1512.0), px(982.0)),
        };
        let compact = Bounds {
            origin: point(px(348.5), px(0.0)),
            size: size(px(COMPACT_WIDTH), px(COMPACT_HEIGHT)),
        };
        let expanded_metrics = HudMetrics::with_expansion(0.0, 1.0);

        let expanded = animated_island_bounds(compact, expanded_metrics, 0.0, display);

        assert_eq!(expanded.origin.y, px(TOP_MARGIN));
        assert_eq!(expanded.size, size(px(930.0), px(520.0)));
        assert_eq!(expanded.origin.x, px(291.0));
    }

    #[test]
    fn minimized_island_moves_to_top_right_status_icon() {
        let display = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(1512.0), px(982.0)),
        };
        let compact = Bounds {
            origin: point(px(348.5), px(0.0)),
            size: size(px(COMPACT_WIDTH), px(COMPACT_HEIGHT)),
        };

        let minimized = animated_island_bounds(compact, HudMetrics::interpolate(0.0), 1.0, display);

        assert_eq!(minimized.origin.y, px(TOP_MARGIN));
        assert_eq!(
            minimized.size,
            size(px(MINIMIZED_WIDTH), px(MINIMIZED_HEIGHT))
        );
        assert_eq!(
            minimized.origin.x,
            px(1512.0 - MINIMIZED_RIGHT_OFFSET - MINIMIZED_WIDTH)
        );
        assert!(!minimized_content_visible(0.54));
        assert!(minimized_content_visible(0.55));
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
    fn recording_center_text_is_plain_listening() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::Listening { ms: 1_250 });

        assert_eq!(center_status_text(&snapshot), "Listening");
    }

    #[test]
    fn accepted_center_text_is_plain_accepted() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::Accepted);

        assert_eq!(center_status_text(&snapshot), "Accepted");
    }

    #[test]
    fn recording_stopped_center_text_is_plain_stopped() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::RecordingStopped);

        assert_eq!(center_status_text(&snapshot), "Recording Stopped");
    }

    #[test]
    fn transcribing_center_text_is_plain_processing() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::Transcribing);

        assert_eq!(center_status_text(&snapshot), "Processing");
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
        let hud = VoiceHud::new(
            rx,
            UiMode::Headless,
            "anthropic/claude-sonnet-5".to_string(),
            Arc::new(Mutex::new(None)),
        );

        assert_eq!(hud.snapshot.mode, UiMode::Headless);
        assert_eq!(hud.snapshot.input_label, "Automation");
    }

    #[test]
    fn agent_step_poll_retries_until_daemon_socket_exists() {
        let profile = format!("delayed-socket-{}", uuid::Uuid::new_v4());
        let socket_path = PathBuf::from(std::env::var("HOME").unwrap())
            .join(".cua")
            .join("profiles")
            .join(&profile)
            .join("daemon.sock");
        std::fs::remove_file(&socket_path).ok();
        std::fs::create_dir_all(socket_path.parent().unwrap()).unwrap();

        let (tx, rx) = channel::<VoiceUiEvent>();
        let runtime = Arc::new(tokio::runtime::Runtime::new().unwrap());
        start_agent_step_poll(profile, runtime.clone(), tx);

        std::thread::sleep(Duration::from_millis(150));
        runtime.block_on(async {
            tokio::fs::remove_file(&socket_path).await.ok();
            let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(read);
            let mut line = String::new();
            tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line)
                .await
                .unwrap();
            assert!(line.contains("\"events.wait\""));
            tokio::io::AsyncWriteExt::write_all(
                &mut write,
                br#"{"ok":true,"result":[{"sequence":1,"kind":"ui_step","data":{"label":"remote delayed step","source":"external agent","task":"remote task","tool":"Unix socket","step_index":2,"step_total":8,"ttl_ms":1500}}]}"#,
            )
            .await
            .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut write, b"\n")
                .await
                .unwrap();
        });

        let event = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let VoiceUiEvent::AgentStep {
            label,
            source,
            task,
            tool,
            step_index,
            step_total,
            ttl_ms,
        } = event
        else {
            panic!("expected delayed agent step");
        };
        assert_eq!(label, "remote delayed step");
        assert_eq!(source.as_deref(), Some("external agent"));
        assert_eq!(task.as_deref(), Some("remote task"));
        assert_eq!(tool.as_deref(), Some("Unix socket"));
        assert_eq!(step_index, Some(2));
        assert_eq!(step_total, Some(8));
        assert_eq!(ttl_ms, Some(1500));
        std::fs::remove_file(socket_path).ok();
    }

    #[test]
    fn control_polling_bridge_triggers_on_quick_double_control_release() {
        let start = Instant::now();
        let mut detector = ControlDoubleTap::default();
        let mut was_down = false;
        let samples = [
            (true, 0, false),
            (false, 40, false),
            (true, 120, false),
            (false, 160, true),
            (false, 200, false),
        ];

        for (is_down, ms, expected) in samples {
            assert_eq!(
                control_poll_sample_triggers_arm(
                    &mut detector,
                    &mut was_down,
                    is_down,
                    start + Duration::from_millis(ms),
                ),
                expected
            );
        }
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

    #[test]
    fn desktop_permission_prompt_marker_is_app_scoped_not_profile_scoped() {
        let home = Path::new("/tmp/cua-test-home");
        let first = desktop_permission_prompt_marker_path_under(home, "screen-recording");
        let second = desktop_permission_prompt_marker_path_under(home, "screen-recording");

        assert_eq!(first, second);
        assert_eq!(
            first,
            Path::new("/tmp/cua-test-home")
                .join(".cua")
                .join("permission-prompts")
                .join("io.saint0x.cua")
                .join("screen-recording")
        );
    }

    #[test]
    fn legacy_desktop_permission_prompt_marker_stays_profile_scoped_for_migration_only() {
        let home = Path::new("/tmp/cua-test-home");
        let first =
            legacy_desktop_permission_prompt_marker_path_under(home, "default", "screen-recording");
        let second =
            legacy_desktop_permission_prompt_marker_path_under(home, "other", "screen-recording");

        assert_ne!(first, second);
        assert_eq!(
            first,
            Path::new("/tmp/cua-test-home")
                .join(".cua")
                .join("profiles")
                .join("default")
                .join("permission-prompts")
                .join("screen-recording")
        );
    }
}
