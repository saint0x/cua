use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait};
use cua_core::{
    config_env_path, IslandActorKind, IslandAmbientKind, IslandAmbientPattern, IslandBackground,
    IslandColorStop, IslandItem, IslandLayout, IslandMotion, IslandScene, IslandTheme,
    PermissionState, UiMode,
};
use cua_voice::activation::ControlDoubleTap;
use cua_voice::agent_events::{
    agent_reply_from_daemon_event, agent_step_from_daemon_event,
    agent_ui_event_from_daemon_event_advancing_cursor, max_daemon_event_sequence,
};
use cua_voice::client::CuaClient;
use cua_voice::daemon::{profile_daemon_is_alive, spawn_profile_daemon, wait_until_ready};
use cua_voice::hud::{
    island_scene_from_snapshot, HudDisplay, HudMetrics, COMPACT_FILLET, COMPACT_HEIGHT,
    COMPACT_WIDTH, EXPANDED_FILLET, EXPANDED_HEIGHT, EXPANDED_WIDTH, TOP_MARGIN, WINDOW_HEIGHT,
    WINDOW_WIDTH,
};
use cua_voice::orb::paint_orb;
use cua_voice::stt::{DEFAULT_STT_BACKEND, DEFAULT_STT_MODEL};
use cua_voice::ui_state::{HudPhase, HudSnapshot, VoiceUiEvent};
use cua_voice::{
    run_text_turn_checked, run_voice_turn_checked, run_voice_turn_until, run_wav_turn_checked,
    VoiceConfig, DEFAULT_PLANNER_MODEL,
};
use gpui::{
    canvas, div, fill, hsla, linear_color_stop, linear_gradient, point, prelude::*, px, rgb, size,
    AnyElement, App, Application, Background, Bounds, Context, Corners, Div, Hsla, IntoElement,
    MouseButton as GpuiMouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, PathBuilder,
    Pixels, Point, Render, Rgba, Styled, Window, WindowBackgroundAppearance, WindowBounds,
    WindowKind, WindowOptions,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const HEADER_TITLE_MIN_WIDTH_PX: f32 = 0.0;
const HEADER_TITLE_MAX_WIDTH_PX: f32 = 112.0;
const HEADER_CENTER_MIN_WIDTH_PX: f32 = 260.0;
const MARQUEE_START_DELAY_SECS: f32 = 1.6;
const MARQUEE_END_HOLD_SECS: f32 = 0.9;
const MARQUEE_SCROLL_SPEED_PX_PER_SEC: f32 = 24.0;
const MARQUEE_CHAR_WIDTH_PX: f32 = 6.2;
const DRAG_THRESHOLD_PX: f32 = 4.0;
const CONTROL_SHORTCUT_POLL_INTERVAL: Duration = Duration::from_millis(4);
const EDGE_SNAP_MARGIN_PX: f32 = 96.0;
const MINIMIZED_WIDTH: f32 = 74.0;
const MINIMIZED_HEIGHT: f32 = 28.0;
const MINIMIZED_RADIUS: f32 = 14.0;
const MINIMIZED_RIGHT_OFFSET: f32 = 220.0;
const HEADER_PAD_X_PX: f32 = 16.0;
const HEADER_GAP_PX: f32 = 8.0;
const HEADER_TITLE_DIVIDER_GAP_PX: f32 = 2.0;
const HEADER_LEAD_WIDTH_PX: f32 = 28.0;
const HEADER_ORB_PX: f32 = 18.0;
const HEADER_RING_PX: f32 = 16.0;
const TASK_RING_PX: f32 = 22.0;
const BODY_LABEL_WIDTH_PX: f32 = 68.0;
const BODY_PAD_X_PX: f32 = 20.0;
const UI_TEXT_PX: f32 = 12.0;
const UI_META_PX: f32 = UI_TEXT_PX;
const UI_LINE_HEIGHT_PX: f32 = 15.0;
const COMPACT_ROW_ITEM_HEIGHT_PX: f32 = 18.0;
const COMPACT_CONTENT_Y_OFFSET_PX: f32 = 0.0;
const STOPLIGHT_SIZE_PX: f32 = 8.0;
const STOPLIGHT_GAP_PX: f32 = 4.0;
const STOPLIGHT_TOP_PX: f32 = compact_content_axis_y() - (STOPLIGHT_SIZE_PX / 2.0);
const SHELL_MOTION_SECS: f32 = 0.320;
const CONTENT_MOTION_SECS: f32 = 0.210;
const REDUCED_SHELL_MOTION_SECS: f32 = 0.110;
const REDUCED_CONTENT_MOTION_SECS: f32 = 0.110;
const ACTIVE_RING_SWEEP_DEG: f32 = 132.0;

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
    #[arg(long, default_value = DEFAULT_PLANNER_MODEL)]
    planner_model: String,
    #[arg(long, env = "CUA_VOICE_DEBUG_TRACE")]
    debug_trace: bool,
    #[arg(long, env = "CUA_REDUCED_MOTION")]
    reduced_motion: bool,
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
    compact_width_px: f32,
    expanded: bool,
    minimized: bool,
    stoplights_visible: bool,
    drag: Option<IslandDrag>,
    model_label: String,
    island_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    last_island_toggle_at: Option<Instant>,
    custom_scene: Option<IslandScene>,
    custom_theme: Option<IslandTheme>,
    custom_background: Option<IslandBackground>,
    reduced_motion: bool,
}

#[derive(Clone, Copy, Debug)]
struct IslandDrag {
    start_cursor: Point<Pixels>,
    start_bounds: Bounds<Pixels>,
    active: bool,
}

impl VoiceHud {
    fn new(
        rx: Receiver<VoiceUiEvent>,
        mode: UiMode,
        model_label: String,
        island_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
        reduced_motion: bool,
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
            compact_width_px: COMPACT_WIDTH,
            expanded: false,
            minimized: false,
            stoplights_visible: false,
            drag: None,
            model_label,
            island_bounds,
            last_island_toggle_at: None,
            custom_scene: None,
            custom_theme: None,
            custom_background: None,
            reduced_motion,
        }
    }

    fn drain_events(&mut self) -> Option<bool> {
        let mut expansion_command = None;
        while let Ok(event) = self.rx.try_recv() {
            match self.apply_voice_event(event, expansion_command) {
                Some(command) => expansion_command = Some(command),
                None => expansion_command = None,
            }
        }
        expansion_command
    }

    fn apply_voice_event(
        &mut self,
        event: VoiceUiEvent,
        current_expansion_command: Option<bool>,
    ) -> Option<bool> {
        match event {
            VoiceUiEvent::ToggleExpanded => {
                if self.accept_island_toggle(Instant::now()) {
                    Some(!current_expansion_command.unwrap_or(self.expanded))
                } else {
                    current_expansion_command
                }
            }
            VoiceUiEvent::SetExpanded(expanded) => Some(expanded),
            VoiceUiEvent::SetMinimized(minimized) => {
                self.minimized = minimized;
                if minimized {
                    self.expanded = false;
                }
                self.drag = None;
                None
            }
            VoiceUiEvent::SceneSet(scene) => {
                let expanded = scene.layout == IslandLayout::Expanded;
                self.custom_scene = Some(scene);
                Some(expanded)
            }
            VoiceUiEvent::SceneReset => {
                self.custom_scene = None;
                self.custom_theme = None;
                self.custom_background = None;
                current_expansion_command
            }
            VoiceUiEvent::SceneTheme(theme) => {
                self.custom_theme = Some(theme);
                current_expansion_command
            }
            VoiceUiEvent::SceneBackground(background) => {
                self.custom_background = Some(background);
                current_expansion_command
            }
            VoiceUiEvent::AutomationActivity {
                label,
                source,
                tool,
            } if self.automation_activity_toggles_island(&label) => {
                let expansion_command = if self.accept_island_toggle(Instant::now()) {
                    Some(!current_expansion_command.unwrap_or(self.expanded))
                } else {
                    current_expansion_command
                };
                self.snapshot.apply(VoiceUiEvent::AutomationActivity {
                    label,
                    source,
                    tool,
                });
                expansion_command
            }
            event => {
                self.snapshot.apply(event);
                current_expansion_command
            }
        }
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
        let elapsed =
            motion_elapsed_secs(self.started.elapsed().as_secs_f32(), self.reduced_motion);
        canvas(
            move |_, _, _| (phase, elapsed),
            move |bounds, (phase, elapsed), window, _| {
                paint_orb(window, bounds, &phase, elapsed);
            },
        )
        .size(px(HEADER_ORB_PX))
    }

    fn render_surface(
        &self,
        scene: &IslandScene,
        metrics: HudMetrics,
        center_text: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if should_render_minimized_content(self.minimized, self.minimized_progress) {
            self.minimized_icon(cx).into_any_element()
        } else {
            self.island_surface(scene, metrics, center_text, cx)
                .into_any_element()
        }
    }

    fn chip(label: impl Into<String>) -> impl IntoElement {
        div()
            .h(px(23.0))
            .px(px(9.0))
            .rounded(px(8.0))
            .bg(hsla(0.0, 0.0, 1.0, 0.10))
            .flex()
            .items_center()
            .text_color(rgb(0xebebf0))
            .text_size(px(UI_TEXT_PX))
            .line_height(px(UI_LINE_HEIGHT_PX))
            .child(label.into())
    }

    fn divider() -> impl IntoElement {
        div().w(px(1.0)).h(px(15.0)).bg(rule_color(0.16))
    }

    fn activity_ring_from_scene(&self, scene: &IslandScene) -> impl IntoElement {
        let reduced_motion = self.reduced_motion;
        let elapsed = motion_elapsed_secs(self.started.elapsed().as_secs_f32(), reduced_motion);
        let dot_chase = scene_dot_chase(scene).expect("IslandScene must include activity dots");
        let step_counter = scene_step_counter(scene);
        let accent = phase_accent(&self.snapshot.phase);
        canvas(
            move |_, _, _| {
                (
                    dot_chase.active,
                    dot_chase.speed,
                    dot_chase.count,
                    step_counter,
                    elapsed,
                    accent,
                )
            },
            move |bounds, (active, speed, count, step_counter, elapsed, accent), window, _| {
                paint_activity_ring(
                    window,
                    bounds,
                    accent,
                    ring_ambient_active(active, step_counter, reduced_motion),
                    speed as f32,
                    count as usize,
                    step_counter,
                    elapsed,
                );
            },
        )
        .size(px(HEADER_RING_PX))
        .flex_none()
    }

    fn tick_animation(&mut self, compact_width_target_px: f32) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        let target = if self.snapshot.is_expanded() {
            1.0
        } else {
            0.0
        };
        self.response_progress = advance_motion_progress(
            self.response_progress,
            target,
            dt,
            content_motion_secs(self.reduced_motion),
        );
        let expansion_target = if self.expanded && !self.minimized {
            1.0
        } else {
            0.0
        };
        self.expansion_progress = advance_motion_progress(
            self.expansion_progress,
            expansion_target,
            dt,
            shell_motion_secs(self.reduced_motion),
        );
        let minimized_target = if self.minimized { 1.0 } else { 0.0 };
        self.minimized_progress = advance_motion_progress(
            self.minimized_progress,
            minimized_target,
            dt,
            shell_motion_secs(self.reduced_motion),
        );
        self.compact_width_px = advance_scalar_motion(
            self.compact_width_px,
            compact_width_target_px,
            dt,
            shell_motion_secs(self.reduced_motion),
        );
    }

    fn sync_center_text(&mut self, center_text: &str) {
        if self.center_text_key != center_text {
            self.center_text_key = center_text.to_string();
            self.center_text_since = Instant::now();
        }
    }

    fn island_surface(
        &self,
        scene: &IslandScene,
        metrics: HudMetrics,
        center: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let title = scene_text(scene, "left", "input")
            .or_else(|| scene_text(scene, "header_left", "input"))
            .expect("IslandScene must include an input label");
        let tool = scene_text(scene, "right", "transport")
            .or_else(|| scene_text(scene, "header_right", "transport"))
            .expect("IslandScene must include a transport chip");
        let app = scene_text(scene, "right", "target")
            .or_else(|| scene_text(scene, "header_right", "target"))
            .expect("IslandScene must include a target chip");
        let reply_visible = response_flash_visible(metrics);
        let header_widths = header_layout_widths(metrics, &title, &tool, &app);

        div()
            .w(px(island_window_width(metrics)))
            .h(px(island_height(metrics)))
            .overflow_hidden()
            .group("cua-island")
            .opacity(metrics.bar_opacity)
            .bg(hsla(0.0, 0.0, 0.0, 0.0))
            .relative()
            .id("cua-island-shell")
            .on_mouse_down(
                GpuiMouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.drag = Some(IslandDrag {
                        start_cursor: current_cursor_point(),
                        start_bounds: window.bounds(),
                        active: false,
                    });
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if event.pressed_button == Some(GpuiMouseButton::Left) {
                    if let Some(drag) = this.drag {
                        let cursor = current_cursor_point();
                        let Some(mut bounds) = dragged_island_bounds(
                            drag.start_bounds,
                            drag.start_cursor,
                            cursor,
                            drag.active,
                        ) else {
                            cx.stop_propagation();
                            return;
                        };
                        this.drag = Some(IslandDrag {
                            active: true,
                            ..drag
                        });
                        if let Some(display) = window.display(cx) {
                            bounds.origin.y = display.bounds().origin.y + px(TOP_MARGIN);
                        }
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
            .flex()
            .flex_col()
            .child(shell_background_layer(
                metrics,
                &scene.background,
                motion_elapsed_secs(self.started.elapsed().as_secs_f32(), self.reduced_motion),
            ))
            .child(ambient_layer(
                scene,
                metrics,
                motion_elapsed_secs(self.started.elapsed().as_secs_f32(), self.reduced_motion),
            ))
            .child(self.stoplights(metrics, cx))
            .child(self.actor_layer(scene, metrics))
            .child(
                div()
                    .ml(px(island_fillet(metrics)))
                    .w(px(island_shell_width(metrics)))
                    .h(px(COMPACT_HEIGHT))
                    .relative()
                    .top(px(COMPACT_CONTENT_Y_OFFSET_PX))
                    .flex()
                    .items_center()
                    .px(px(HEADER_PAD_X_PX))
                    .child(
                        div()
                            .w(px(HEADER_LEAD_WIDTH_PX))
                            .h(px(HEADER_ORB_PX))
                            .flex_none()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .opacity(if self.stoplights_visible { 0.0 } else { 1.0 })
                                    .child(self.orb()),
                            ),
                    )
                    .child(div().w(px(HEADER_GAP_PX)).flex_none())
                    .child(
                        div()
                            .w(px(header_widths.title))
                            .h(px(COMPACT_ROW_ITEM_HEIGHT_PX))
                            .flex_none()
                            .flex()
                            .items_center()
                            .truncate()
                            .text_color(rgb(0xffffff))
                            .text_size(px(UI_TEXT_PX))
                            .line_height(px(UI_LINE_HEIGHT_PX))
                            .child(title),
                    )
                    .child(div().w(px(HEADER_TITLE_DIVIDER_GAP_PX)).flex_none())
                    .child(Self::divider())
                    .child(div().w(px(HEADER_GAP_PX)).flex_none())
                    .child(center_text_slot(
                        center,
                        reply_visible,
                        header_widths.center,
                        motion_elapsed_secs(
                            self.center_text_since.elapsed().as_secs_f32(),
                            self.reduced_motion,
                        ),
                    ))
                    .child(div().w(px(HEADER_GAP_PX)).flex_none())
                    .child(Self::divider())
                    .child(div().w(px(HEADER_GAP_PX)).flex_none())
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(7.0))
                            .child(Self::chip(tool))
                            .child(Self::chip(app)),
                    )
                    .child(div().w(px(HEADER_GAP_PX)).flex_none())
                    .child(self.activity_ring_from_scene(scene)),
            )
            .when(scene_renders_expanded_body(scene), |element| {
                element.child(self.expanded_body(scene, metrics))
            })
    }

    fn stoplights(&self, metrics: HudMetrics, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .absolute()
            .left(px(island_fillet(metrics) + HEADER_PAD_X_PX))
            .top(px(STOPLIGHT_TOP_PX))
            .flex()
            .items_center()
            .gap(px(STOPLIGHT_GAP_PX))
            .opacity(if self.stoplights_visible { 0.92 } else { 0.0 })
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

    fn actor_layer(&self, scene: &IslandScene, metrics: HudMetrics) -> impl IntoElement {
        let elapsed =
            motion_elapsed_secs(self.started.elapsed().as_secs_f32(), self.reduced_motion);
        let accent = phase_accent(&self.snapshot.phase);
        let mut layer = div().absolute().left_0().top_0().size_full();
        for actor in &scene.actors {
            let style = actor_style(actor, metrics, elapsed);
            let fill = actor_fill_color(&actor.kind, accent);
            layer = layer.child(
                div()
                    .absolute()
                    .left(px(style.x))
                    .top(px(style.y))
                    .w(px(actor.width as f32))
                    .h(px(actor.height as f32))
                    .rounded_full()
                    .opacity(style.opacity)
                    .bg(fill),
            );
        }
        layer
    }

    fn toggle_expanded(&mut self, window: &mut Window, cx: &mut App) {
        self.expanded = !self.expanded;
        self.minimized = false;
        self.drag = None;
        if self.expanded {
            window.activate_window();
        }
        window.refresh();
        if let Some(display) = window.display(cx) {
            let bounds = animated_island_bounds(
                window.bounds(),
                self.metrics(),
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
        if expanded {
            window.activate_window();
        }
        cx.notify();
        if let Some(display) = window.display(cx) {
            let bounds = animated_island_bounds(
                window.bounds(),
                self.metrics(),
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
            .overflow_hidden()
            .relative()
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
                        this.metrics(),
                        this.minimized_progress,
                        display.bounds(),
                    );
                    window.set_bounds(bounds);
                }
                cx.stop_propagation();
            }))
            .child(minimized_background_layer())
            .child(self.orb())
    }

    fn expanded_body(&self, scene: &IslandScene, metrics: HudMetrics) -> impl IntoElement {
        let step_counter = scene_step_counter(scene);
        let Some(task) = scene_row(scene, "task", "task") else {
            return div();
        };
        let Some(response) = scene_row(scene, "response", "response") else {
            return div();
        };
        let Some(footer) = scene_footer(scene) else {
            return div();
        };
        div()
            .relative()
            .top(px(expanded_body_offset_y(metrics)))
            .opacity(metrics.expansion_opacity)
            .h(px((EXPANDED_HEIGHT - COMPACT_HEIGHT).max(0.0)))
            .ml(px(island_fillet(metrics)))
            .w(px(island_shell_width(metrics)))
            .px(px(BODY_PAD_X_PX))
            .pt(px(4.0))
            .pb(px(14.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(26.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(10.0))
                            .child(field_row("Task", task.value.clone(), 1))
                            .child(field_row("Response", response.value.clone(), 2))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(16.0))
                                    .min_w_0()
                                    .child(field_label("Action"))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .items_center()
                                            .gap(px(14.0))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_color(content_text_color(false))
                                                    .text_size(px(UI_TEXT_PX))
                                                    .line_height(px(UI_LINE_HEIGHT_PX))
                                                    .child(
                                                        scene_row(scene, "details_left", "action")
                                                            .map(|row| row.value)
                                                            .unwrap_or_else(|| "None".to_string()),
                                                    ),
                                            )
                                            .child(Self::chip(
                                                scene_row(scene, "details_left", "phase")
                                                    .map(|row| row.value)
                                                    .unwrap_or_else(|| "Idle".to_string()),
                                            ))
                                            .child(Self::chip(
                                                scene_row(scene, "details_left", "state")
                                                    .map(|row| row.value)
                                                    .unwrap_or_else(|| "Idle".to_string()),
                                            )),
                                    ),
                            )
                            .child(tools_field(scene)),
                    )
                    .child(
                        div()
                            .w(px(54.0))
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(5.0))
                            .child(task_gauge(
                                scene,
                                &self.snapshot.phase,
                                motion_elapsed_secs(
                                    self.started.elapsed().as_secs_f32(),
                                    self.reduced_motion,
                                ),
                            ))
                            .child(
                                div()
                                    .text_size(px(UI_META_PX))
                                    .line_height(px(UI_LINE_HEIGHT_PX))
                                    .text_color(rgb(0x9f9fa6))
                                    .child(gauge_caption(step_counter, &self.snapshot.phase)),
                            ),
                    ),
            )
            .child(
                div()
                    .mt_auto()
                    .pt(px(11.0))
                    .border_t_1()
                    .border_color(rule_color(0.08))
                    .flex()
                    .gap(px(40.0))
                    .child(footer_cell("Elapsed", footer.elapsed))
                    .child(footer_cell("Model", footer.model))
                    .child(footer_cell("Transport", footer.transport)),
            )
    }

    fn finish_drag(&mut self, window: &mut Window, cx: &mut App) {
        self.drag = None;
        if let Some(display) = window.display(cx) {
            let snapped = snap_island_bounds(window.bounds(), display.bounds());
            window.set_bounds(snapped);
        }
    }

    fn metrics(&self) -> HudMetrics {
        metrics_with_compact_width(
            self.response_progress,
            self.expansion_progress,
            self.compact_width_px,
        )
    }
}

#[cfg(test)]
fn activity_dot_alpha(index: usize, elapsed_secs: f32, active: bool, speed: f32) -> f32 {
    activity_dot_style(index, 6, elapsed_secs, active, speed).alpha
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct ActivityDotStyle {
    alpha: f32,
    lightness: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ActorStyle {
    x: f32,
    y: f32,
    opacity: f32,
}

fn actor_style(
    actor: &cua_core::IslandActor,
    metrics: HudMetrics,
    elapsed_secs: f32,
) -> ActorStyle {
    let base = ActorStyle {
        x: actor.x as f32,
        y: actor.y as f32,
        opacity: 0.92,
    };
    let Some(motion) = &actor.motion else {
        return base;
    };
    match motion {
        IslandMotion::None => base,
        IslandMotion::Fade { duration_ms } => {
            let progress = repeating_progress(elapsed_secs, *duration_ms);
            ActorStyle {
                opacity: 0.32 + (progress * std::f32::consts::PI).sin().max(0.0) * 0.6,
                ..base
            }
        }
        IslandMotion::Pulse { duration_ms } => {
            let progress = repeating_progress(elapsed_secs, *duration_ms);
            ActorStyle {
                opacity: 0.42 + (progress * std::f32::consts::TAU).sin().abs() * 0.5,
                ..base
            }
        }
        IslandMotion::SlideTo { x, y, duration_ms } => {
            let progress = smooth_progress(repeating_progress(elapsed_secs, *duration_ms));
            ActorStyle {
                x: lerp(actor.x as f32, *x as f32, progress),
                y: lerp(actor.y as f32, *y as f32, progress),
                ..base
            }
        }
        IslandMotion::WalkTo {
            region,
            item,
            duration_ms,
        } => {
            let (target_x, target_y) = actor_region_anchor(region, item, metrics)
                .unwrap_or((actor.x as f32, actor.y as f32));
            let progress = smooth_progress(repeating_progress(elapsed_secs, *duration_ms));
            ActorStyle {
                x: lerp(actor.x as f32, target_x, progress),
                y: lerp(actor.y as f32, target_y, progress),
                ..base
            }
        }
    }
}

fn actor_region_anchor(region: &str, item: &str, metrics: HudMetrics) -> Option<(f32, f32)> {
    let width = island_width(metrics);
    let height = island_height(metrics);
    let header_y = actor_anchor_y(12.0, height);
    let (x, y) = match (region, item) {
        ("left", "orb") | ("header_left", "orb") => Some((20.0, header_y)),
        ("left", "input") | ("header_left", "input") => Some((56.0, header_y)),
        ("center", "status") | ("header_center", "status") => Some((width * 0.5, header_y)),
        ("right", "transport") | ("header_right", "transport") => Some((width - 240.0, header_y)),
        ("right", "target") | ("header_right", "target") => Some((width - 170.0, header_y)),
        ("right", "activity") | ("header_right", "activity") => Some((width - 58.0, header_y)),
        ("task", _) => Some((width * 0.24, actor_anchor_y(92.0, height))),
        ("response", _) => Some((width * 0.50, actor_anchor_y(142.0, height))),
        ("details_left", _) => Some((width * 0.24, actor_anchor_y(194.0, height))),
        ("details_right", _) => Some((width * 0.66, actor_anchor_y(194.0, height))),
        ("footer", _) => Some((width * 0.50, actor_anchor_y(EXPANDED_HEIGHT - 42.0, height))),
        _ => None,
    }?;
    Some((x.clamp(0.0, width), y))
}

fn actor_anchor_y(y: f32, island_height: f32) -> f32 {
    y.clamp(0.0, (island_height - HEADER_ORB_PX).max(0.0))
}

fn background_paint(background: &IslandBackground, elapsed_secs: f32) -> Background {
    match background {
        IslandBackground::Solid { color, opacity } => {
            Background::from(scene_color(color, *opacity).unwrap_or_else(default_background_color))
        }
        IslandBackground::Transparent { .. } => Background::from(hsla(0.0, 0.0, 0.0, 0.0)),
        IslandBackground::LinearGradient {
            angle_degrees,
            opacity,
            stops,
        } => gradient_background(*angle_degrees, *opacity, gradient_edge_pair(stops)),
        IslandBackground::AnimatedGradient {
            angle_degrees,
            opacity,
            duration_ms,
            stops,
        } => {
            let pair = animated_gradient_pair(stops, elapsed_secs, *duration_ms);
            gradient_background(*angle_degrees, *opacity, pair)
        }
        IslandBackground::NeonSweep {
            base_color,
            sweep_color,
            opacity,
            duration_ms,
        } => {
            let progress = repeating_progress(elapsed_secs, *duration_ms);
            let angle = (90.0 + progress * 360.0) % 360.0;
            let base = scene_color(base_color, *opacity).unwrap_or_else(default_background_color);
            let sweep = scene_color(sweep_color, *opacity).unwrap_or_else(default_background_color);
            linear_gradient(
                angle,
                linear_color_stop(base, 0.18),
                linear_color_stop(sweep, 0.82),
            )
        }
    }
}

fn gradient_background(
    angle_degrees: u16,
    opacity: u8,
    pair: Option<(&IslandColorStop, &IslandColorStop)>,
) -> Background {
    let Some((from, to)) = pair else {
        return Background::from(default_background_color());
    };
    let from = scene_color(&from.color, opacity).unwrap_or_else(default_background_color);
    let to = scene_color(&to.color, opacity).unwrap_or_else(default_background_color);
    linear_gradient(
        angle_degrees as f32,
        linear_color_stop(from, 0.0),
        linear_color_stop(to, 1.0),
    )
}

fn gradient_edge_pair(stops: &[IslandColorStop]) -> Option<(&IslandColorStop, &IslandColorStop)> {
    Some((stops.first()?, stops.last()?))
}

fn animated_gradient_pair(
    stops: &[IslandColorStop],
    elapsed_secs: f32,
    duration_ms: u16,
) -> Option<(&IslandColorStop, &IslandColorStop)> {
    if stops.len() < 2 {
        return None;
    }
    let progress = repeating_progress(elapsed_secs, duration_ms);
    let head = ((progress * stops.len() as f32).floor() as usize) % stops.len();
    let tail = (head + 1) % stops.len();
    Some((&stops[head], &stops[tail]))
}

fn default_background_color() -> Hsla {
    hsla(0.0, 0.0, 0.0, 0.92)
}

fn scene_color(hex: &str, opacity: u8) -> Option<Hsla> {
    let value = parse_hex_color(hex)?;
    let mut color: Hsla = rgb(value).into();
    color.a = (opacity.min(100) as f32) / 100.0;
    Some(color)
}

fn parse_hex_color(hex: &str) -> Option<u32> {
    let digits = hex.strip_prefix('#')?;
    if digits.len() != 6 || !digits.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

fn background_contrast_scrim_alpha(background: &IslandBackground) -> f32 {
    match background {
        IslandBackground::Solid { color, opacity } => {
            if is_default_solid_background(color, *opacity) {
                0.0
            } else if *opacity < 75 || color_luminance(color).unwrap_or(0.0) > 0.18 {
                0.82
            } else {
                0.34
            }
        }
        IslandBackground::Transparent { opacity } => {
            if *opacity >= 90 {
                0.0
            } else {
                0.78
            }
        }
        IslandBackground::LinearGradient { opacity, stops, .. }
        | IslandBackground::AnimatedGradient { opacity, stops, .. } => {
            if *opacity < 80
                || stops
                    .iter()
                    .any(|stop| color_luminance(&stop.color).unwrap_or(0.0) > 0.18)
            {
                0.82
            } else {
                0.38
            }
        }
        IslandBackground::NeonSweep {
            base_color,
            sweep_color,
            opacity,
            ..
        } => {
            let base = color_luminance(base_color).unwrap_or(0.0);
            let sweep = color_luminance(sweep_color).unwrap_or(0.0);
            if *opacity < 80 || base.max(sweep) > 0.18 {
                0.74
            } else {
                0.34
            }
        }
    }
}

#[cfg(test)]
fn background_needs_foreground_lift(background: &IslandBackground) -> bool {
    background_contrast_scrim_alpha(background) > 0.0
}

fn is_default_solid_background(color: &str, opacity: u8) -> bool {
    opacity == 92 && color.eq_ignore_ascii_case("#000000")
}

fn color_luminance(hex: &str) -> Option<f32> {
    let value = parse_hex_color(hex)?;
    let r = ((value >> 16) & 0xff) as f32 / 255.0;
    let g = ((value >> 8) & 0xff) as f32 / 255.0;
    let b = (value & 0xff) as f32 / 255.0;
    Some(0.2126 * r + 0.7152 * g + 0.0722 * b)
}

fn repeating_progress(elapsed_secs: f32, duration_ms: u16) -> f32 {
    let duration = (duration_ms as f32 / 1_000.0).max(0.05);
    (elapsed_secs % duration) / duration
}

fn smooth_progress(progress: f32) -> f32 {
    let forward = if progress <= 0.5 {
        progress * 2.0
    } else {
        (1.0 - progress) * 2.0
    };
    forward * forward * (3.0 - 2.0 * forward)
}

fn lerp(start: f32, end: f32, t: f32) -> f32 {
    start + (end - start) * t
}

#[cfg(test)]
fn activity_dot_style(
    index: usize,
    dot_count: usize,
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

    let dot_count = dot_count.max(1);
    let head = ((elapsed_secs * steps_per_second).floor() as usize) % dot_count;
    let distance_behind = (head + dot_count - index) % dot_count;
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

fn center_text_for(scene: &IslandScene) -> String {
    scene_text(scene, "center", "status")
        .or_else(|| scene_text(scene, "header_center", "status"))
        .expect("IslandScene must include center status")
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct HeaderLayoutWidths {
    title: f32,
    center: f32,
}

fn header_layout_widths(
    metrics: HudMetrics,
    title: &str,
    transport: &str,
    target: &str,
) -> HeaderLayoutWidths {
    let title_width = header_title_width_px(title);
    let chrome_width = HEADER_LEAD_WIDTH_PX
        + title_width
        + 2.0
        + header_chips_width_px(transport, target)
        + HEADER_RING_PX;
    let gap_width = header_gap_width_px();
    let center_width =
        (island_shell_width(metrics) - (HEADER_PAD_X_PX * 2.0) - chrome_width - gap_width)
            .max(HEADER_CENTER_MIN_WIDTH_PX);

    HeaderLayoutWidths {
        title: title_width,
        center: center_width,
    }
}

fn compact_shell_width_target(scene: &IslandScene) -> f32 {
    let title = scene_text(scene, "left", "input")
        .or_else(|| scene_text(scene, "header_left", "input"))
        .unwrap_or_default();
    let center = scene_text(scene, "center", "status")
        .or_else(|| scene_text(scene, "header_center", "status"))
        .unwrap_or_default();
    let transport = scene_text(scene, "right", "transport")
        .or_else(|| scene_text(scene, "header_right", "transport"))
        .unwrap_or_default();
    let target = scene_text(scene, "right", "target")
        .or_else(|| scene_text(scene, "header_right", "target"))
        .unwrap_or_default();
    let chrome_width = HEADER_LEAD_WIDTH_PX
        + header_title_width_px(&title)
        + 2.0
        + header_chips_width_px(&transport, &target)
        + HEADER_RING_PX;
    let center_width =
        (estimated_center_text_width_px(&center) + 8.0).max(HEADER_CENTER_MIN_WIDTH_PX);
    let width = (HEADER_PAD_X_PX * 2.0) + chrome_width + header_gap_width_px() + center_width;

    width.clamp(COMPACT_WIDTH, EXPANDED_WIDTH)
}

fn header_title_width_px(title: &str) -> f32 {
    (estimated_header_text_width_px(title) + 2.0)
        .clamp(HEADER_TITLE_MIN_WIDTH_PX, HEADER_TITLE_MAX_WIDTH_PX)
}

fn header_gap_width_px() -> f32 {
    (HEADER_GAP_PX * 5.0) + HEADER_TITLE_DIVIDER_GAP_PX
}

fn header_chips_width_px(transport: &str, target: &str) -> f32 {
    chip_width_px(transport) + chip_width_px(target) + 7.0
}

fn chip_width_px(label: &str) -> f32 {
    (estimated_header_text_width_px(label) + 18.0).max(32.0)
}

fn estimated_header_text_width_px(text: &str) -> f32 {
    text.chars().count() as f32 * MARQUEE_CHAR_WIDTH_PX
}

fn center_text_slot(
    center: String,
    reply_visible: bool,
    viewport_width_px: f32,
    visible_secs: f32,
) -> impl IntoElement {
    let offset = marquee_offset_px(&center, viewport_width_px, visible_secs);
    div()
        .w(px(viewport_width_px))
        .h(px(COMPACT_ROW_ITEM_HEIGHT_PX))
        .flex_none()
        .overflow_hidden()
        .whitespace_nowrap()
        .flex()
        .items_center()
        .text_color(if reply_visible {
            rgb(0xf1f1f4)
        } else {
            rgb(0xb9b9c0)
        })
        .text_size(px(UI_TEXT_PX))
        .line_height(px(UI_LINE_HEIGHT_PX))
        .child(
            div()
                .flex_none()
                .whitespace_nowrap()
                .line_height(px(UI_LINE_HEIGHT_PX))
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

fn ambient_layer(scene: &IslandScene, metrics: HudMetrics, elapsed_secs: f32) -> impl IntoElement {
    let radius = island_radius(metrics);
    let fillet = island_fillet(metrics);
    let shell_width = island_shell_width(metrics);
    let patterns = scene.ambient.clone();
    canvas(
        move |_, _, _| (radius, fillet, shell_width, patterns.clone(), elapsed_secs),
        move |bounds, (radius, fillet, shell_width, patterns, elapsed_secs), window, _| {
            for pattern in patterns.iter().filter(|pattern| pattern.active) {
                paint_ambient_pattern(
                    window,
                    bounds,
                    radius,
                    fillet,
                    shell_width,
                    pattern,
                    elapsed_secs,
                );
            }
        },
    )
    .absolute()
    .left_0()
    .top_0()
    .size_full()
}

fn paint_ambient_pattern(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    radius: f32,
    fillet: f32,
    shell_width: f32,
    pattern: &IslandAmbientPattern,
    elapsed_secs: f32,
) {
    let Some(color) = ambient_color(pattern, elapsed_secs) else {
        return;
    };
    let background = match pattern.kind {
        IslandAmbientKind::SoftSweep => {
            let angle =
                90.0 + repeating_progress(elapsed_secs, ambient_duration_ms(pattern)) * 45.0;
            linear_gradient(
                angle,
                linear_color_stop(hsla(color.h, color.s, color.l, 0.0), 0.0),
                linear_color_stop(color, 1.0),
            )
        }
        IslandAmbientKind::BreathingGlow => Background::from(color),
    };
    let shell_bounds = Bounds {
        origin: point(bounds.origin.x + px(fillet), bounds.origin.y),
        size: size(px(shell_width), bounds.size.height),
    };

    paint_concave_fillet(window, bounds, fillet, shell_width, background, false);
    paint_concave_fillet(window, bounds, fillet, shell_width, background, true);
    window.paint_quad(fill(shell_bounds, background).corner_radii(Corners::all(px(radius))));
}

fn ambient_color(pattern: &IslandAmbientPattern, elapsed_secs: f32) -> Option<Hsla> {
    let mut color = scene_color(&pattern.color, pattern.opacity)?;
    let speed = pattern.speed.max(1) as f32;
    let progress = ((elapsed_secs * speed) % 1.0) * std::f32::consts::TAU;
    let pulse = match pattern.kind {
        IslandAmbientKind::SoftSweep => 0.72 + progress.sin().abs() * 0.18,
        IslandAmbientKind::BreathingGlow => 0.44 + ((progress.sin() + 1.0) * 0.5) * 0.32,
    };
    color.a *= pulse;
    Some(color)
}

fn ambient_duration_ms(pattern: &IslandAmbientPattern) -> u16 {
    let speed = pattern.speed.max(1) as u16;
    (6_000 / speed).clamp(250, 6_000)
}

fn shell_background_layer(
    metrics: HudMetrics,
    background: &IslandBackground,
    elapsed_secs: f32,
) -> impl IntoElement {
    let radius = island_radius(metrics);
    let fillet = island_fillet(metrics);
    let shell_width = island_shell_width(metrics);
    let paint = background_paint(background, elapsed_secs);
    let scrim = background_contrast_scrim_alpha(background);
    canvas(
        move |_, _, _| (radius, fillet, shell_width, paint, scrim),
        move |bounds, (radius, fillet, shell_width, paint, scrim), window, _| {
            let shell_bounds = Bounds {
                origin: point(bounds.origin.x + px(fillet), bounds.origin.y),
                size: size(px(shell_width), bounds.size.height),
            };
            paint_concave_fillet(window, bounds, fillet, shell_width, paint, false);
            paint_concave_fillet(window, bounds, fillet, shell_width, paint, true);
            window.paint_quad(fill(shell_bounds, paint).corner_radii(Corners::all(px(radius))));
            if scrim > 0.0 {
                let scrim_color = hsla(0.0, 0.0, 0.0, scrim);
                paint_concave_fillet(
                    window,
                    bounds,
                    fillet,
                    shell_width,
                    scrim_color.into(),
                    false,
                );
                paint_concave_fillet(
                    window,
                    bounds,
                    fillet,
                    shell_width,
                    scrim_color.into(),
                    true,
                );
                window.paint_quad(
                    fill(shell_bounds, scrim_color).corner_radii(Corners::all(px(radius))),
                );
            }
        },
    )
    .absolute()
    .left_0()
    .top_0()
    .size_full()
}

fn minimized_background_layer() -> impl IntoElement {
    canvas(
        move |_, _, _| (),
        move |bounds, _, window, _| {
            window.paint_quad(
                fill(bounds, default_background_color())
                    .corner_radii(Corners::all(px(MINIMIZED_RADIUS))),
            );
        },
    )
    .absolute()
    .left_0()
    .top_0()
    .size_full()
}

fn paint_concave_fillet(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    fillet: f32,
    shell_width: f32,
    background: Background,
    right: bool,
) {
    if fillet <= 0.0 {
        return;
    }
    let steps = 18;
    let x0 = bounds.origin.x
        + if right {
            px(fillet + shell_width)
        } else {
            px(0.0)
        };
    let y0 = bounds.origin.y;
    let mut builder = PathBuilder::fill();

    if right {
        builder.move_to(point(x0, y0));
        builder.line_to(point(x0 + px(fillet), y0));
        for step in 0..=steps {
            let progress = step as f32 / steps as f32;
            let theta = (-90.0 - (progress * 90.0)).to_radians();
            let center_x = x0 + px(fillet);
            let center_y = y0 + px(fillet);
            builder.line_to(point(
                center_x + px(theta.cos() * fillet),
                center_y + px(theta.sin() * fillet),
            ));
        }
    } else {
        builder.move_to(point(x0, y0));
        builder.line_to(point(x0 + px(fillet), y0));
        builder.line_to(point(x0 + px(fillet), y0 + px(fillet)));
        for step in 0..=steps {
            let progress = step as f32 / steps as f32;
            let theta = (0.0 - (progress * 90.0)).to_radians();
            let center_x = x0;
            let center_y = y0 + px(fillet);
            builder.line_to(point(
                center_x + px(theta.cos() * fillet),
                center_y + px(theta.sin() * fillet),
            ));
        }
    }

    builder.close();
    if let Ok(path) = builder.build() {
        window.paint_path(path, background);
    }
}

fn field_label(label: impl Into<String>) -> impl IntoElement {
    div()
        .w(px(BODY_LABEL_WIDTH_PX))
        .flex_none()
        .text_size(px(UI_TEXT_PX))
        .line_height(px(UI_LINE_HEIGHT_PX))
        .text_color(hsla(0.0, 0.0, 1.0, 0.36))
        .child(label.into().to_uppercase())
}

fn field_row(label: impl Into<String>, value: impl Into<String>, lines: usize) -> impl IntoElement {
    div()
        .flex()
        .items_baseline()
        .gap(px(16.0))
        .min_w_0()
        .child(field_label(label))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .max_h(px(if lines > 1 { 38.0 } else { 19.0 }))
                .overflow_hidden()
                .text_color(content_text_color(false))
                .text_size(px(UI_TEXT_PX))
                .line_height(px(UI_LINE_HEIGHT_PX))
                .child(value.into()),
        )
}

fn tools_field(scene: &IslandScene) -> impl IntoElement {
    let rows = scene_tool_rows(scene);
    div()
        .flex()
        .items_baseline()
        .gap(px(16.0))
        .min_w_0()
        .child(field_label("Tools"))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(compact_tool_row(&rows[0]))
                .child(compact_tool_row(&rows[1])),
        )
}

fn compact_tool_row(row: &SceneToolRow) -> impl IntoElement {
    div()
        .flex()
        .items_baseline()
        .gap(px(14.0))
        .min_w_0()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(content_text_color(false))
                .text_size(px(UI_TEXT_PX))
                .line_height(px(UI_LINE_HEIGHT_PX))
                .child(row.label.clone()),
        )
        .child(
            div()
                .flex_none()
                .text_color(rgb(0x8e8e93))
                .text_size(px(UI_META_PX))
                .line_height(px(UI_LINE_HEIGHT_PX))
                .child(tool_meta_text(row)),
        )
}

fn tool_meta_text(row: &SceneToolRow) -> String {
    format!("{} · {} · {}", row.tool, row.app, row.age)
}

fn footer_cell(label: impl Into<String>, value: impl Into<String>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(2.0))
        .min_w(px(118.0))
        .child(
            div()
                .text_size(px(UI_TEXT_PX))
                .line_height(px(UI_LINE_HEIGHT_PX))
                .text_color(hsla(0.0, 0.0, 1.0, 0.38))
                .child(label.into().to_uppercase()),
        )
        .child(
            div()
                .truncate()
                .text_size(px(UI_TEXT_PX))
                .line_height(px(UI_LINE_HEIGHT_PX))
                .text_color(hsla(0.0, 0.0, 1.0, 0.90))
                .child(value.into()),
        )
}

fn task_gauge(scene: &IslandScene, phase: &HudPhase, elapsed: f32) -> impl IntoElement {
    let active = scene_dot_chase(scene).is_some_and(|dot| dot.active);
    let speed = scene_dot_chase(scene)
        .map(|dot| dot.speed as f32)
        .unwrap_or(0.0);
    let count = scene_dot_chase(scene)
        .map(|dot| dot.count as usize)
        .unwrap_or(6);
    let step_counter = scene_step_counter(scene);
    let accent = phase_accent(phase);
    canvas(
        move |_, _, _| (active, speed, count, step_counter, accent, elapsed),
        move |bounds, (active, speed, count, step_counter, accent, elapsed), window, _| {
            paint_activity_ring(
                window,
                bounds,
                accent,
                active,
                speed,
                count,
                step_counter,
                elapsed,
            );
        },
    )
    .size(px(TASK_RING_PX))
}

fn gauge_caption(step_counter: Option<(usize, usize)>, phase: &HudPhase) -> String {
    if let Some((index, total)) = step_counter {
        format!("{index} / {total}")
    } else if matches!(phase, HudPhase::Reply | HudPhase::Error) {
        "Holding".to_string()
    } else {
        phase.label().to_string()
    }
}

fn phase_accent(phase: &HudPhase) -> Hsla {
    match phase {
        HudPhase::Listening => hsla(18.0 / 360.0, 1.0, 0.59, 1.0),
        HudPhase::Dispatching | HudPhase::Reply => hsla(148.0 / 360.0, 1.0, 0.45, 1.0),
        HudPhase::Error => hsla(3.0 / 360.0, 1.0, 0.61, 1.0),
        HudPhase::RecordingStopped
        | HudPhase::Accepted
        | HudPhase::Transcribing
        | HudPhase::Planning => hsla(276.0 / 360.0, 0.88, 0.65, 1.0),
        _ => hsla(0.0, 0.0, 0.56, 1.0),
    }
}

fn actor_fill_color(kind: &IslandActorKind, accent: Hsla) -> Hsla {
    let lightness = match kind {
        IslandActorKind::Sprite => accent.l,
        IslandActorKind::Particle => (accent.l + 0.16).min(0.82),
    };
    hsla(accent.h, accent.s, lightness, 0.92)
}

#[allow(clippy::too_many_arguments)]
fn paint_activity_ring(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    accent: Hsla,
    active: bool,
    speed: f32,
    count: usize,
    step_counter: Option<(usize, usize)>,
    elapsed: f32,
) {
    let size = bounds.size.width.min(bounds.size.height).to_f64() as f32;
    let center = bounds.center();
    let stroke = (size * 0.09).max(2.0);
    let radius = size / 2.0 - stroke / 2.0;
    paint_ring_arc(window, center, radius, stroke, 0.0, 360.0, rule_color(0.09));

    let sweep = activity_ring_sweep_deg(active, step_counter);
    if sweep <= 0.1 {
        return;
    }
    let revolutions_per_second = speed.max(0.0) / count.max(1) as f32;
    let start = if step_counter.is_some() {
        -90.0
    } else {
        -90.0 + elapsed * revolutions_per_second * 360.0
    };
    paint_ring_arc(window, center, radius, stroke, start, sweep, accent);
}

fn activity_ring_sweep_deg(active: bool, step_counter: Option<(usize, usize)>) -> f32 {
    if let Some((index, total)) = step_counter {
        360.0 * (index as f32 / total.max(1) as f32).clamp(0.0, 1.0)
    } else if active {
        ACTIVE_RING_SWEEP_DEG
    } else {
        0.0
    }
}

fn paint_ring_arc(
    window: &mut Window,
    center: Point<Pixels>,
    radius: f32,
    stroke: f32,
    start_deg: f32,
    sweep_deg: f32,
    color: Hsla,
) {
    let steps = ((sweep_deg.abs() / 4.5).ceil() as usize).clamp(8, 96);
    let outer = radius + stroke / 2.0;
    let inner = (radius - stroke / 2.0).max(0.0);
    let mut outer_points = Vec::with_capacity(steps + 1);
    let mut inner_points = Vec::with_capacity(steps + 1);
    for step in 0..=steps {
        let progress = step as f32 / steps as f32;
        let theta = (start_deg + sweep_deg * progress).to_radians();
        outer_points.push(point(
            center.x + px(theta.cos() * outer),
            center.y + px(theta.sin() * outer),
        ));
        inner_points.push(point(
            center.x + px(theta.cos() * inner),
            center.y + px(theta.sin() * inner),
        ));
    }
    let mut builder = PathBuilder::fill();
    builder.move_to(outer_points[0]);
    for point in outer_points.iter().skip(1) {
        builder.line_to(*point);
    }
    for point in inner_points.iter().rev() {
        builder.line_to(*point);
    }
    builder.close();
    if let Ok(path) = builder.build() {
        window.paint_path(path, Background::from(color));
    }
    let cap = stroke / 2.0;
    for endpoint in [outer_points[0], *outer_points.last().unwrap()] {
        let cap_bounds = Bounds {
            origin: point(endpoint.x - px(cap), endpoint.y - px(cap)),
            size: size(px(stroke), px(stroke)),
        };
        window.paint_quad(fill(cap_bounds, color).corner_radii(Corners::all(px(cap))));
    }
}

fn stoplight(color: u32) -> Div {
    div()
        .w(px(STOPLIGHT_SIZE_PX))
        .h(px(STOPLIGHT_SIZE_PX))
        .rounded_full()
        .bg(rgb(color))
        .opacity(0.9)
        .hover(|style| style.opacity(1.0))
}

fn stoplight_cluster_width_px() -> f32 {
    (STOPLIGHT_SIZE_PX * 3.0) + (STOPLIGHT_GAP_PX * 2.0)
}

fn stoplight_hover_bounds(window_bounds: Bounds<Pixels>, metrics: HudMetrics) -> Bounds<Pixels> {
    let padding = 4.0;
    Bounds {
        origin: point(
            window_bounds.origin.x + px(island_fillet(metrics) + HEADER_PAD_X_PX - padding),
            window_bounds.origin.y + px(STOPLIGHT_TOP_PX - padding),
        ),
        size: size(
            px(stoplight_cluster_width_px() + padding * 2.0),
            px(STOPLIGHT_SIZE_PX + padding * 2.0),
        ),
    }
}

fn content_text_color(high_contrast: bool) -> Rgba {
    if high_contrast {
        rgb(0xffffff)
    } else {
        rgb(0xd8d8de)
    }
}

fn rule_color(alpha: f32) -> Hsla {
    hsla(0.0, 0.0, 1.0, alpha)
}

fn island_shell_width(metrics: HudMetrics) -> f32 {
    metrics.width
}

fn metrics_with_compact_width(
    response_progress: f32,
    expansion_progress: f32,
    compact_width_px: f32,
) -> HudMetrics {
    let mut metrics = HudMetrics::with_expansion(response_progress, expansion_progress);
    metrics.width = cua_voice::hud::lerp(
        compact_width_px.clamp(COMPACT_WIDTH, EXPANDED_WIDTH),
        EXPANDED_WIDTH,
        metrics.expansion_opacity,
    );
    metrics
}

fn island_width(metrics: HudMetrics) -> f32 {
    island_shell_width(metrics) + (island_fillet(metrics) * 2.0)
}

fn island_window_width(metrics: HudMetrics) -> f32 {
    island_width(metrics)
}

fn island_height(metrics: HudMetrics) -> f32 {
    metrics.height
}

fn island_radius(metrics: HudMetrics) -> f32 {
    metrics.radius
}

fn island_fillet(metrics: HudMetrics) -> f32 {
    let width_progress =
        ((metrics.width - COMPACT_WIDTH) / (EXPANDED_WIDTH - COMPACT_WIDTH)).clamp(0.0, 1.0);
    cua_voice::hud::lerp(COMPACT_FILLET, EXPANDED_FILLET, width_progress)
}

fn minimized_content_visible(progress: f32) -> bool {
    progress >= 0.55
}

fn should_render_minimized_content(minimized: bool, progress: f32) -> bool {
    minimized || minimized_content_visible(progress)
}

const fn compact_content_axis_y() -> f32 {
    (COMPACT_HEIGHT / 2.0) + COMPACT_CONTENT_Y_OFFSET_PX
}

#[cfg(test)]
const fn compact_row_frame_axis_y() -> f32 {
    (COMPACT_HEIGHT / 2.0) + COMPACT_CONTENT_Y_OFFSET_PX
}

#[cfg(test)]
const fn compact_row_item_top_y() -> f32 {
    compact_row_frame_axis_y() - (COMPACT_ROW_ITEM_HEIGHT_PX / 2.0)
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

fn shell_motion_secs(reduced_motion: bool) -> f32 {
    if reduced_motion {
        REDUCED_SHELL_MOTION_SECS
    } else {
        SHELL_MOTION_SECS
    }
}

fn content_motion_secs(reduced_motion: bool) -> f32 {
    if reduced_motion {
        REDUCED_CONTENT_MOTION_SECS
    } else {
        CONTENT_MOTION_SECS
    }
}

fn motion_elapsed_secs(elapsed_secs: f32, reduced_motion: bool) -> f32 {
    if reduced_motion {
        0.0
    } else {
        elapsed_secs
    }
}

fn ring_ambient_active(
    active: bool,
    step_counter: Option<(usize, usize)>,
    reduced_motion: bool,
) -> bool {
    active && (!reduced_motion || step_counter.is_some())
}

fn expanded_body_offset_y(metrics: HudMetrics) -> f32 {
    -6.0 * (1.0 - metrics.expansion_opacity.clamp(0.0, 1.0))
}

fn advance_motion_progress(current: f32, target: f32, dt_secs: f32, duration_secs: f32) -> f32 {
    let current = current.clamp(0.0, 1.0);
    let target = target.clamp(0.0, 1.0);
    if current == target {
        return target;
    }
    let step = (dt_secs / duration_secs.max(0.001)).clamp(0.0, 1.0);
    if target > current {
        (current + step).min(target)
    } else {
        (current - step).max(target)
    }
}

fn advance_scalar_motion(current: f32, target: f32, dt_secs: f32, duration_secs: f32) -> f32 {
    if current == target {
        return target;
    }
    let full_travel_px = (EXPANDED_WIDTH - COMPACT_WIDTH).max(1.0);
    let step = full_travel_px * (dt_secs / duration_secs.max(0.001)).clamp(0.0, 1.0);
    if target > current {
        (current + step).min(target)
    } else {
        (current - step).max(target)
    }
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

#[derive(Clone, Copy)]
struct DotChaseScene {
    active: bool,
    count: u8,
    speed: u8,
}

#[derive(Clone)]
struct SceneRow {
    value: String,
}

#[derive(Clone)]
struct SceneToolRow {
    label: String,
    tool: String,
    app: String,
    age: String,
}

#[derive(Clone)]
struct SceneFooter {
    elapsed: String,
    model: String,
    transport: String,
}

fn scene_text(scene: &IslandScene, region: &str, id: &str) -> Option<String> {
    let item = scene_item(scene, region, id)?;
    match item {
        IslandItem::Label { text, .. }
        | IslandItem::Marquee { text, .. }
        | IslandItem::Chip { text, .. } => Some(text.clone()),
        _ => None,
    }
}

fn scene_item<'a>(scene: &'a IslandScene, region: &str, id: &str) -> Option<&'a IslandItem> {
    scene
        .regions
        .get(region)?
        .items
        .iter()
        .find(|item| island_item_id(item) == id)
}

fn island_item_id(item: &IslandItem) -> &str {
    match item {
        IslandItem::Label { id, .. }
        | IslandItem::Marquee { id, .. }
        | IslandItem::Chip { id, .. }
        | IslandItem::StepCounter { id, .. }
        | IslandItem::DotChase { id, .. }
        | IslandItem::Row { id, .. }
        | IslandItem::ToolRow { id, .. }
        | IslandItem::Divider { id }
        | IslandItem::Spacer { id, .. } => id,
    }
}

fn scene_dot_chase(scene: &IslandScene) -> Option<DotChaseScene> {
    for region in ["right", "header_right"] {
        if let Some(IslandItem::DotChase {
            active,
            count,
            speed,
            ..
        }) = scene_item(scene, region, "activity")
        {
            return Some(DotChaseScene {
                active: *active,
                count: *count,
                speed: *speed,
            });
        }
    }
    None
}

fn scene_step_counter(scene: &IslandScene) -> Option<(usize, usize)> {
    let IslandItem::StepCounter { index, total, .. } = scene_item(scene, "task", "step")? else {
        return None;
    };
    Some((*index as usize, *total as usize))
}

fn scene_row(scene: &IslandScene, region: &str, id: &str) -> Option<SceneRow> {
    let IslandItem::Row { value, .. } = scene_item(scene, region, id)? else {
        return None;
    };
    Some(SceneRow {
        value: value.clone(),
    })
}

fn scene_tool_rows(scene: &IslandScene) -> [SceneToolRow; 2] {
    let mut rows = scene
        .regions
        .get("details_right")
        .map(|region| {
            region
                .items
                .iter()
                .filter_map(|item| {
                    let IslandItem::ToolRow {
                        label,
                        tool,
                        app,
                        age,
                        ..
                    } = item
                    else {
                        return None;
                    };
                    Some(SceneToolRow {
                        label: label.clone(),
                        tool: tool.clone(),
                        app: app.clone(),
                        age: age.clone(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    while rows.len() < 2 {
        rows.push(SceneToolRow {
            label: String::new(),
            tool: String::new(),
            app: String::new(),
            age: String::new(),
        });
    }
    [rows.remove(0), rows.remove(0)]
}

fn scene_footer(scene: &IslandScene) -> Option<SceneFooter> {
    Some(SceneFooter {
        elapsed: scene_text(scene, "footer", "elapsed")?,
        model: scene_text(scene, "footer", "model")?,
        transport: scene_text(scene, "footer", "transport")?,
    })
}

fn should_reset_after_reply_collapse(reply_window_expired: bool, response_progress: f32) -> bool {
    reply_window_expired && response_progress == 0.0
}

fn scene_renders_expanded_body(scene: &IslandScene) -> bool {
    scene.layout == IslandLayout::Expanded
        && scene_row(scene, "task", "task").is_some()
        && scene_row(scene, "response", "response").is_some()
        && scene_footer(scene).is_some()
}

impl Render for VoiceHud {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let expansion_command = self.drain_events();
        let reply_window_expired =
            self.snapshot.expanded_until.is_some() && !self.snapshot.is_expanded();
        if self.snapshot.expire_transcript(Instant::now()) {
            cx.notify();
        }
        if let Some(expanded) = expansion_command {
            self.set_expanded_from_render(expanded, window, cx);
        }
        if should_reset_after_reply_collapse(reply_window_expired, self.response_progress) {
            self.snapshot.apply(VoiceUiEvent::Idle);
        }
        let preliminary_metrics = self.metrics();
        let preliminary_reply_visible = response_flash_visible(preliminary_metrics);
        let preliminary_display = HudDisplay::from_snapshot(&self.snapshot);
        let preliminary_scene = self.custom_scene.clone().unwrap_or_else(|| {
            island_scene_from_snapshot(
                &self.snapshot,
                &preliminary_display,
                self.expanded && !self.minimized,
                preliminary_reply_visible,
                &self.model_label,
                self.started.elapsed(),
            )
            .expect("voice HUD scene should be valid")
        });
        self.tick_animation(compact_shell_width_target(&preliminary_scene));
        let metrics = self.metrics();
        let stoplights_visible = point_inside_bounds(
            current_cursor_point(),
            stoplight_hover_bounds(window.bounds(), metrics),
        );
        if self.stoplights_visible != stoplights_visible {
            self.stoplights_visible = stoplights_visible;
            cx.notify();
        }
        self.snapshot.expire_programmed_step(Instant::now());
        window.request_animation_frame();
        let display = HudDisplay::from_snapshot(&self.snapshot);
        let reply_visible = response_flash_visible(metrics);
        let mut scene = self.custom_scene.clone().unwrap_or_else(|| {
            island_scene_from_snapshot(
                &self.snapshot,
                &display,
                self.expanded && !self.minimized,
                reply_visible,
                &self.model_label,
                self.started.elapsed(),
            )
            .expect("internal IslandScene mapping must validate")
        });
        if let Some(theme) = &self.custom_theme {
            scene.theme = Some(theme.clone());
        }
        if let Some(background) = &self.custom_background {
            scene.background = background.clone();
        }
        let center_text = center_text_for(&scene);
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
            .child(self.render_surface(&scene, metrics, center_text, cx))
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
    let reduced_motion = reduced_motion_enabled(args.reduced_motion);
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
        if let Err(error) = start_embedded_daemon_if_needed(&config.profile, runtime.clone()) {
            tx.send(VoiceUiEvent::Error(format!(
                "Daemon start failed: {error:#}"
            )))
            .ok();
        }
        start_control_shortcut_controller(config.clone(), runtime.clone(), tx.clone());
        start_island_double_tap_listener(tx.clone(), island_bounds.clone());
        start_agent_step_poll(config.profile.clone(), config.debug_trace, tx.clone());
        start_inbox_turn_poll(config.clone(), runtime.clone(), tx.clone());
    }
    Application::new().run(move |cx: &mut App| {
        if should_request_desktop_access {
            request_desktop_access_once_if_packaged_app(&desktop_access_profile);
        }
        let bounds = top_centered_bounds(cx);
        cx.open_window(hud_window_options(bounds), move |window, cx| {
            let hud = cx.new(|_| {
                VoiceHud::new(
                    rx,
                    ui_mode.clone(),
                    model_label.clone(),
                    island_bounds.clone(),
                    reduced_motion,
                )
            });
            let weak_hud = hud.downgrade();
            window
                .spawn(cx, async move |cx| loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(33))
                        .await;
                    if weak_hud
                        .update_in(cx, |hud, window, cx| {
                            if let Some(expanded) = hud.drain_events() {
                                hud.set_expanded_from_render(expanded, window, cx);
                            } else {
                                cx.notify();
                            }
                            window.refresh();
                        })
                        .is_err()
                    {
                        break;
                    }
                })
                .detach();
            hud
        })
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
            VoiceUiEvent::SceneSet(scene) => {
                serde_json::json!({"event": "scene_set", "scene": scene})
            }
            VoiceUiEvent::SceneReset => serde_json::json!({"event": "scene_reset"}),
            VoiceUiEvent::SceneTheme(theme) => {
                serde_json::json!({"event": "scene_theme", "theme": theme})
            }
            VoiceUiEvent::SceneBackground(background) => {
                serde_json::json!({"event": "scene_background", "background": background})
            }
            VoiceUiEvent::ToggleExpanded => serde_json::json!({"event": "toggle_expanded"}),
            VoiceUiEvent::SetExpanded(expanded) => {
                serde_json::json!({"event": "set_expanded", "expanded": expanded})
            }
            VoiceUiEvent::SetMinimized(minimized) => {
                serde_json::json!({"event": "set_minimized", "minimized": minimized})
            }
            VoiceUiEvent::Idle => serde_json::json!({"event": "idle"}),
        };
        println!("{value}");
    }
}

struct SingleInstance {
    path: PathBuf,
    running: Arc<AtomicBool>,
}

impl SingleInstance {
    fn acquire(profile: &str) -> anyhow::Result<Option<Self>> {
        let path = single_instance_socket_path(profile);
        match UnixListener::bind(&path) {
            Ok(listener) => Ok(Some(Self::from_listener(path, listener)?)),
            Err(error) if error.kind() == ErrorKind::AddrInUse => {
                if single_instance_socket_is_responsive(&path) {
                    return Ok(None);
                }
                std::fs::remove_file(&path).ok();
                let listener = UnixListener::bind(&path)?;
                Ok(Some(Self::from_listener(path, listener)?))
            }
            Err(error) => Err(error.into()),
        }
    }

    fn from_listener(path: PathBuf, listener: UnixListener) -> anyhow::Result<Self> {
        listener.set_nonblocking(true)?;
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = running.clone();
        thread::spawn(move || single_instance_ping_loop(listener, thread_running));
        Ok(Self { path, running })
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        std::fs::remove_file(&self.path).ok();
    }
}

const SINGLE_INSTANCE_PING: &[u8] = b"cua-voice ping\n";
const SINGLE_INSTANCE_ACK: &[u8] = b"cua-voice alive\n";

fn single_instance_socket_is_responsive(path: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(path) else {
        return false;
    };
    let timeout = Some(Duration::from_millis(120));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    if stream.write_all(SINGLE_INSTANCE_PING).is_err() {
        return false;
    }
    let mut buffer = [0_u8; 16];
    matches!(
        stream.read(&mut buffer),
        Ok(read) if &buffer[..read] == SINGLE_INSTANCE_ACK
    )
}

fn single_instance_ping_loop(listener: UnixListener, running: Arc<AtomicBool>) {
    while running.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let timeout = Some(Duration::from_millis(120));
                let _ = stream.set_read_timeout(timeout);
                let _ = stream.set_write_timeout(timeout);
                let mut buffer = [0_u8; 15];
                if matches!(
                    stream.read(&mut buffer),
                    Ok(read) if &buffer[..read] == SINGLE_INSTANCE_PING
                ) {
                    let _ = stream.write_all(SINGLE_INSTANCE_ACK);
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break,
        }
    }
}

fn start_agent_step_poll(profile: String, debug_trace: bool, tx: Sender<VoiceUiEvent>) {
    std::thread::Builder::new()
        .name("cua-hud-events".to_string())
        .spawn(move || {
            if debug_trace {
                eprintln!("cua HUD event poll starting for profile {profile}");
            }
            let Ok(runtime) = tokio::runtime::Runtime::new() else {
                tx.send(VoiceUiEvent::Error(
                    "HUD event poll failed: could not start async runtime".to_string(),
                ))
                .ok();
                return;
            };
            runtime.block_on(async move {
                let mut last_sequence = 0_u64;
                let mut last_start_attempt = Instant::now() - Duration::from_secs(5);
                loop {
                    let client = match CuaClient::new(profile.clone()).await {
                        Ok(client) => client,
                        Err(error) => {
                            if debug_trace {
                                eprintln!("cua HUD event poll client failed: {error:#}");
                            }
                            tx.send(VoiceUiEvent::Error(format!(
                                "HUD event poll failed: {error:#}"
                            )))
                            .ok();
                            tokio::time::sleep(Duration::from_millis(250)).await;
                            continue;
                        }
                    };
                    let Ok(mut session) = client.session().await else {
                        if debug_trace {
                            eprintln!("cua HUD event poll session connect failed");
                        }
                        if last_start_attempt.elapsed() >= Duration::from_secs(1) {
                            last_start_attempt = Instant::now();
                            let _ = spawn_profile_daemon(client.profile());
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    };
                    if last_sequence == 0 {
                        match session.events_snapshot().await {
                            Ok(events) => {
                                last_sequence = max_daemon_event_sequence(&events);
                                if debug_trace {
                                    eprintln!(
                                        "cua HUD event poll snapshot sequence {last_sequence}"
                                    );
                                }
                            }
                            Err(error) => {
                                if debug_trace {
                                    eprintln!("cua HUD event poll snapshot failed: {error:#}");
                                }
                                tx.send(VoiceUiEvent::Error(format!(
                                    "HUD event poll failed: {error:#}"
                                )))
                                .ok();
                                tokio::time::sleep(Duration::from_millis(250)).await;
                                continue;
                            }
                        }
                    }
                    while let Ok(events) = session.events_wait(last_sequence, 1_000).await {
                        for event in events {
                            if debug_trace {
                                eprintln!("cua HUD event poll received {event}");
                            }
                            if let Some(event) = agent_ui_event_from_daemon_event_advancing_cursor(
                                &event,
                                &mut last_sequence,
                            ) {
                                tx.send(event).ok();
                            }
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            });
        })
        .expect("start HUD event poll thread");
}

fn start_inbox_turn_poll(
    config: VoiceConfig,
    runtime: Arc<tokio::runtime::Runtime>,
    tx: Sender<VoiceUiEvent>,
) {
    runtime.spawn(async move {
        let Ok(client) = CuaClient::new(config.profile.clone()).await else {
            tx.send(VoiceUiEvent::Error("Invalid cua profile path".to_string()))
                .ok();
            return;
        };
        let mut last_sequence = 0_u64;
        let mut last_connect_error: Option<String> = None;
        loop {
            let mut session = match client.session().await {
                Ok(session) => {
                    last_connect_error = None;
                    session
                }
                Err(error) => {
                    let error = format!("{error:#}");
                    if last_connect_error.as_deref() != Some(error.as_str()) {
                        eprintln!(
                            "cua voice inbox poll could not connect to profile socket for {}: {error}",
                            config.profile
                        );
                        last_connect_error = Some(error);
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    continue;
                }
            };
            if last_sequence == 0 {
                match session.events_snapshot().await {
                    Ok(events) => {
                        last_sequence = max_daemon_event_sequence(&events);
                    }
                    Err(error) => {
                        eprintln!("cua voice inbox poll events snapshot failed: {error:#}");
                        break;
                    }
                }
            }
            loop {
                match session.events_wait(last_sequence, 1_000).await {
                    Ok(events) => {
                        for event in events {
                            if let Some(sequence) =
                                event.get("sequence").and_then(|value| value.as_u64())
                            {
                                last_sequence = last_sequence.max(sequence);
                            }
                            let Some(message) = inbound_message_from_daemon_event(&event) else {
                                continue;
                            };
                            let message_id = message.message_id.clone();
                            if let Err(error) = client.inbox_running(message_id.clone()).await {
                                let message = format!("mark inbox message running failed: {error:#}");
                                eprintln!("{message}");
                                let _ = tx.send(VoiceUiEvent::Error(message));
                            }
                            let prompt = inbound_prompt(&message);
                            let result =
                                run_text_turn_checked(config.clone(), prompt, tx.clone()).await;
                            match result {
                                Ok(()) => {
                                    if let Err(error) = client
                                        .inbox_done(message_id, Some("completed".to_string()))
                                        .await
                                    {
                                        let message =
                                            format!("mark inbox message done failed: {error:#}");
                                        eprintln!("{message}");
                                        let _ = tx.send(VoiceUiEvent::Error(message));
                                    }
                                }
                                Err(error) => {
                                    let error = error.to_string();
                                    if let Err(status_error) =
                                        client.inbox_failed(message_id, error.clone()).await
                                    {
                                        eprintln!(
                                            "mark inbox message failed-state update failed: {status_error:#}"
                                        );
                                    }
                                    let _ = tx.send(VoiceUiEvent::Error(error));
                                }
                            }
                        }
                    }
                    Err(error) => {
                        eprintln!("cua voice inbox poll events wait failed: {error:#}");
                        break;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
}

#[derive(Debug, Clone)]
struct InboundHudMessage {
    message_id: String,
    source: String,
    text: String,
    payload: serde_json::Value,
    delivery_method: String,
}

fn inbound_message_from_daemon_event(event: &serde_json::Value) -> Option<InboundHudMessage> {
    if event.get("kind").and_then(|value| value.as_str()) != Some("inbound_message") {
        return None;
    }
    let data = event.get("data")?;
    Some(InboundHudMessage {
        message_id: data.get("message_id")?.as_str()?.to_string(),
        source: data
            .get("source")
            .and_then(|value| value.as_str())
            .unwrap_or("inbox")
            .to_string(),
        text: data.get("text")?.as_str()?.to_string(),
        payload: data
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        delivery_method: data
            .get("delivery_method")
            .and_then(|value| value.as_str())
            .unwrap_or("inbox")
            .to_string(),
    })
}

fn inbound_prompt(message: &InboundHudMessage) -> String {
    let payload = if message.payload.is_null() {
        "{}".to_string()
    } else {
        serde_json::to_string(&message.payload).unwrap_or_else(|_| "{}".to_string())
    };
    format!(
        "Inbound cua message received at {} via {} from source {}.\n\
         Treat this exactly like a user command. Check current desktop context when useful, use tools when needed, and verify visible effects before finishing.\n\
         Message: {}\n\
         Payload JSON: {}",
        chrono::Utc::now().to_rfc3339(),
        message.delivery_method,
        message.source,
        message.text,
        payload
    )
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

fn reduced_motion_enabled(requested: bool) -> bool {
    resolve_reduced_motion(
        requested,
        cua_platform_macos::system_reduce_motion_enabled(),
    )
}

fn resolve_reduced_motion(requested: bool, system_enabled: bool) -> bool {
    requested || system_enabled
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
        return top_attached_fallback_bounds(window_size);
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

fn top_attached_fallback_bounds(window_size: gpui::Size<Pixels>) -> Bounds<Pixels> {
    Bounds {
        origin: point(px(0.0), px(TOP_MARGIN)),
        size: window_size,
    }
}

fn hud_window_options(bounds: Bounds<Pixels>) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: None,
        focus: false,
        kind: WindowKind::PopUp,
        is_resizable: false,
        is_minimizable: false,
        mouse_passthrough: false,
        window_background: WindowBackgroundAppearance::Transparent,
        ..Default::default()
    }
}

fn animated_island_bounds(
    current: Bounds<Pixels>,
    metrics: HudMetrics,
    minimized_progress: f32,
    display_bounds: Bounds<Pixels>,
) -> Bounds<Pixels> {
    let minimized_progress = cua_voice::hud::shell_ease(minimized_progress.clamp(0.0, 1.0));
    let display_left = display_bounds.origin.x.to_f64() as f32;
    let display_right =
        display_bounds.origin.x.to_f64() as f32 + display_bounds.size.width.to_f64() as f32;
    let width = cua_voice::hud::lerp(
        island_window_width(metrics),
        MINIMIZED_WIDTH,
        minimized_progress,
    );
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

fn dragged_island_bounds(
    start_bounds: Bounds<Pixels>,
    start_cursor: Point<Pixels>,
    cursor: Point<Pixels>,
    active: bool,
) -> Option<Bounds<Pixels>> {
    let dx = cursor.x - start_cursor.x;
    if !active && dx.abs() < px(DRAG_THRESHOLD_PX) {
        return None;
    }
    Some(Bounds {
        origin: point(start_bounds.origin.x + dx, start_bounds.origin.y),
        size: start_bounds.size,
    })
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
    profile: &str,
    runtime: Arc<tokio::runtime::Runtime>,
) -> anyhow::Result<()> {
    if profile_daemon_is_alive(profile) {
        return Ok(());
    }
    spawn_profile_daemon(profile)?;
    runtime.block_on(wait_until_ready(Duration::from_secs(3), || {
        let profile = profile.to_string();
        async move {
            if profile_daemon_is_alive(&profile) {
                Ok(())
            } else {
                anyhow::bail!("daemon socket is not accepting connections")
            }
        }
    }))
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
            if let Err(error) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "cua permission marker directory create failed for {}: {error}",
                    parent.display()
                );
            }
        }
        let result = current();
        if let Err(error) = std::fs::write(&path, format!("{:?}\n", result)) {
            eprintln!(
                "cua permission marker write failed for {}: {error}",
                path.display()
            );
        }
        return result;
    }
    if let Some(parent) = path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            eprintln!(
                "cua permission marker directory create failed for {}: {error}",
                parent.display()
            );
        }
    }
    let result = request();
    if let Err(error) = std::fs::write(&path, format!("{:?}\n", result)) {
        eprintln!(
            "cua permission marker write failed for {}: {error}",
            path.display()
        );
    }
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

    fn scene_center_text_for_test(snapshot: HudSnapshot) -> String {
        let display = HudDisplay::from_snapshot(&snapshot);
        let scene = island_scene_from_snapshot(
            &snapshot,
            &display,
            false,
            false,
            DEFAULT_PLANNER_MODEL,
            Duration::ZERO,
        )
        .unwrap();
        center_text_for(&scene)
    }

    #[test]
    fn island_background_color_parser_accepts_only_hex_rgb() {
        assert_eq!(parse_hex_color("#1e9bff"), Some(0x1e9bff));
        assert_eq!(parse_hex_color("#000000"), Some(0x000000));
        assert_eq!(parse_hex_color("1e9bff"), None);
        assert_eq!(parse_hex_color("#fff"), None);
        assert_eq!(parse_hex_color("#nothex"), None);
    }

    #[test]
    fn background_contrast_scrim_preserves_default_and_protects_light_backdrops() {
        let default_background = IslandBackground::Solid {
            color: "#000000".to_string(),
            opacity: 92,
        };
        assert_eq!(background_contrast_scrim_alpha(&default_background), 0.0);
        assert!(!background_needs_foreground_lift(&default_background));

        let light_background = IslandBackground::Solid {
            color: "#eef8ff".to_string(),
            opacity: 92,
        };
        assert!(background_contrast_scrim_alpha(&light_background) >= 0.80);
        assert!(background_needs_foreground_lift(&light_background));

        let translucent_background = IslandBackground::Transparent { opacity: 16 };
        assert!(background_contrast_scrim_alpha(&translucent_background) >= 0.75);
        assert!(background_needs_foreground_lift(&translucent_background));

        let gradient_background = IslandBackground::AnimatedGradient {
            angle_degrees: 92,
            opacity: 82,
            duration_ms: 7200,
            stops: vec![
                IslandColorStop {
                    offset: 0,
                    color: "#eef8ff".to_string(),
                },
                IslandColorStop {
                    offset: 1000,
                    color: "#f4eaff".to_string(),
                },
            ],
        };
        assert!(background_contrast_scrim_alpha(&gradient_background) >= 0.80);
        assert!(background_needs_foreground_lift(&gradient_background));
    }

    #[test]
    fn animated_background_pair_cycles_without_reset_branch() {
        let stops = vec![
            IslandColorStop {
                offset: 0,
                color: "#000000".to_string(),
            },
            IslandColorStop {
                offset: 500,
                color: "#1e9bff".to_string(),
            },
            IslandColorStop {
                offset: 1000,
                color: "#9b5cff".to_string(),
            },
        ];

        let first = animated_gradient_pair(&stops, 0.0, 900).unwrap();
        let middle = animated_gradient_pair(&stops, 0.31, 900).unwrap();
        let wrapped = animated_gradient_pair(&stops, 0.89, 900).unwrap();

        assert_eq!(first.0.color, "#000000");
        assert_eq!(first.1.color, "#1e9bff");
        assert_eq!(middle.0.color, "#1e9bff");
        assert_eq!(middle.1.color, "#9b5cff");
        assert_eq!(wrapped.0.color, "#9b5cff");
        assert_eq!(wrapped.1.color, "#000000");
    }

    #[test]
    fn ambient_patterns_are_bounded_and_never_part_of_the_default_scene() {
        let snapshot = HudSnapshot::default();
        let display = HudDisplay::from_snapshot(&snapshot);
        let scene = island_scene_from_snapshot(
            &snapshot,
            &display,
            false,
            false,
            DEFAULT_PLANNER_MODEL,
            Duration::ZERO,
        )
        .unwrap();
        assert!(scene.ambient.is_empty());

        let pattern = IslandAmbientPattern {
            id: "soft-sweep".to_string(),
            kind: IslandAmbientKind::SoftSweep,
            active: true,
            color: "#1e9bff".to_string(),
            opacity: 24,
            speed: 2,
        };
        let first = ambient_color(&pattern, 0.0).unwrap();
        let second = ambient_color(&pattern, 0.125).unwrap();

        assert_eq!(ambient_duration_ms(&pattern), 3_000);
        assert!(first.a > 0.0 && first.a <= 0.24);
        assert!(second.a > first.a);
    }

    fn step_snapshot(index: u16, total: u16, label: &str) -> HudSnapshot {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::AgentStep {
            label: label.to_string(),
            source: Some("remote".to_string()),
            task: Some("Test".to_string()),
            tool: Some("Unix socket".to_string()),
            step_index: Some(index),
            step_total: Some(total),
            ttl_ms: None,
        });
        snapshot
    }

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
    fn hud_motion_uses_reference_durations() {
        assert_eq!(SHELL_MOTION_SECS, 0.320);
        assert_eq!(CONTENT_MOTION_SECS, 0.210);
        assert_eq!(shell_motion_secs(false), SHELL_MOTION_SECS);
        assert_eq!(content_motion_secs(false), CONTENT_MOTION_SECS);
        assert_eq!(shell_motion_secs(true), 0.110);
        assert_eq!(content_motion_secs(true), 0.110);

        assert_eq!(
            advance_motion_progress(0.0, 1.0, SHELL_MOTION_SECS, SHELL_MOTION_SECS),
            1.0
        );
        assert_eq!(
            advance_motion_progress(1.0, 0.0, CONTENT_MOTION_SECS, CONTENT_MOTION_SECS),
            0.0
        );
        assert_eq!(
            advance_motion_progress(0.0, 1.0, SHELL_MOTION_SECS / 2.0, SHELL_MOTION_SECS),
            0.5
        );
    }

    #[test]
    fn reduced_motion_freezes_ambient_loops_but_keeps_step_progress() {
        assert_eq!(motion_elapsed_secs(14.0, false), 14.0);
        assert_eq!(motion_elapsed_secs(14.0, true), 0.0);
        assert!(ring_ambient_active(true, Some((1, 4)), true));
        assert!(!ring_ambient_active(true, None, true));
        assert!(ring_ambient_active(true, None, false));
    }

    #[test]
    fn reduced_motion_respects_explicit_or_system_request() {
        assert!(!resolve_reduced_motion(false, false));
        assert!(resolve_reduced_motion(true, false));
        assert!(resolve_reduced_motion(false, true));
        assert!(resolve_reduced_motion(true, true));
    }

    #[test]
    fn expanded_body_uses_reference_entry_offset() {
        let closed = expanded_body_offset_y(HudMetrics::with_expansion(0.0, 0.0));
        let settling = expanded_body_offset_y(HudMetrics::with_expansion(0.0, 0.5));
        let open = expanded_body_offset_y(HudMetrics::with_expansion(0.0, 1.0));

        assert_eq!(closed, -6.0);
        assert!(settling > closed);
        assert!(settling < open);
        assert_eq!(open, -0.0);
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
    fn island_window_uses_real_shell_without_side_fillet_gutters() {
        let compact = HudMetrics::with_expansion(0.0, 0.0);
        let expanded = HudMetrics::with_expansion(0.0, 1.0);

        assert_eq!(island_shell_width(compact), COMPACT_WIDTH);
        assert_eq!(island_fillet(compact), 0.0);
        assert_eq!(COMPACT_FILLET, 0.0);
        assert_eq!(island_width(compact), COMPACT_WIDTH);
        assert_eq!(island_shell_width(expanded), EXPANDED_WIDTH);
        assert_eq!(island_fillet(expanded), 0.0);
        assert_eq!(EXPANDED_FILLET, 0.0);
        assert_eq!(island_width(expanded), EXPANDED_WIDTH);
    }

    #[test]
    fn stoplights_center_on_compact_bar_axis() {
        assert_eq!(
            STOPLIGHT_TOP_PX + (STOPLIGHT_SIZE_PX / 2.0),
            compact_content_axis_y()
        );
    }

    #[test]
    fn compact_hud_controls_share_center_axis() {
        assert_eq!(COMPACT_ROW_ITEM_HEIGHT_PX % 2.0, 0.0);
        const { assert!(COMPACT_ROW_ITEM_HEIGHT_PX < COMPACT_HEIGHT) };
        const { assert!(UI_LINE_HEIGHT_PX <= COMPACT_ROW_ITEM_HEIGHT_PX) };
        assert_eq!(compact_content_axis_y(), COMPACT_HEIGHT / 2.0);
        assert_eq!(compact_row_frame_axis_y(), COMPACT_HEIGHT / 2.0);
        assert_eq!(
            compact_row_item_top_y() + (COMPACT_ROW_ITEM_HEIGHT_PX / 2.0),
            COMPACT_HEIGHT / 2.0
        );
        assert_eq!(UI_TEXT_PX, 12.0);
        assert_eq!(UI_META_PX, UI_TEXT_PX);
        assert_eq!(HEADER_ORB_PX, 18.0);
        assert_eq!(HEADER_RING_PX, 16.0);
        assert_eq!(TASK_RING_PX, 22.0);
        assert_eq!(HEADER_GAP_PX, 8.0);
        assert_eq!(HEADER_TITLE_DIVIDER_GAP_PX, 2.0);
        assert_eq!(STOPLIGHT_GAP_PX, 4.0);
        assert_eq!(HEADER_TITLE_MIN_WIDTH_PX, 0.0);
        assert_eq!(HEADER_TITLE_MAX_WIDTH_PX, 112.0);
        assert_eq!(HEADER_CENTER_MIN_WIDTH_PX, 260.0);
        assert_eq!(HEADER_LEAD_WIDTH_PX, 28.0);
        let stoplight_cluster_width = stoplight_cluster_width_px();
        let stoplight_title_gap = HEADER_LEAD_WIDTH_PX + HEADER_GAP_PX - stoplight_cluster_width;
        assert!(stoplight_title_gap >= 4.0);
        assert!(
            header_title_width_px("Voice control")
                >= estimated_header_text_width_px("Voice control")
        );
        const { assert!(HEADER_TITLE_DIVIDER_GAP_PX < HEADER_GAP_PX) };
    }

    #[test]
    fn compact_header_title_rail_tracks_each_mode_label_tightly() {
        let automation = header_layout_widths(
            HudMetrics::with_expansion(0.0, 0.0),
            "Automation",
            "HTTP",
            "Model",
        );
        let voice = header_layout_widths(
            HudMetrics::with_expansion(0.0, 0.0),
            "Voice control",
            "HTTP",
            "Model",
        );

        assert!(automation.title < voice.title);
        assert!(automation.center > voice.center);
        assert!(voice.title >= estimated_header_text_width_px("Voice control"));
        assert!(automation.title >= estimated_header_text_width_px("Automation"));
        assert!(automation.center >= HEADER_CENTER_MIN_WIDTH_PX);
        assert!(voice.center >= HEADER_CENTER_MIN_WIDTH_PX);
    }

    fn compact_width_test_scene(center: &str, transport: &str, target: &str) -> IslandScene {
        let mut regions = std::collections::BTreeMap::new();
        regions.insert(
            "left".to_string(),
            cua_core::IslandRegion {
                items: vec![IslandItem::Label {
                    id: "input".to_string(),
                    text: "Voice control".to_string(),
                }],
            },
        );
        regions.insert(
            "center".to_string(),
            cua_core::IslandRegion {
                items: vec![IslandItem::Marquee {
                    id: "status".to_string(),
                    text: center.to_string(),
                }],
            },
        );
        regions.insert(
            "right".to_string(),
            cua_core::IslandRegion {
                items: vec![
                    IslandItem::Chip {
                        id: "transport".to_string(),
                        text: transport.to_string(),
                    },
                    IslandItem::Chip {
                        id: "target".to_string(),
                        text: target.to_string(),
                    },
                ],
            },
        );

        IslandScene {
            schema_version: cua_core::ISLAND_SCHEMA_VERSION.to_string(),
            layout: IslandLayout::Compact,
            mode: UiMode::Headful,
            background: IslandBackground::Solid {
                color: "#000000".to_string(),
                opacity: 92,
            },
            regions,
            ambient: Vec::new(),
            actors: Vec::new(),
            theme: None,
        }
    }

    #[test]
    fn compact_shell_width_expands_to_fit_header_text_target() {
        let baseline = compact_width_test_scene("Ready", "cua", "idle");
        let long = compact_width_test_scene(
            "Watching the active browser tab until verification completes",
            "cloud computer backend",
            "Oracle VM fleet",
        );

        assert_eq!(compact_shell_width_target(&baseline), COMPACT_WIDTH);
        let long_target = compact_shell_width_target(&long);
        assert!(long_target > COMPACT_WIDTH);
        assert!(long_target <= EXPANDED_WIDTH);
    }

    #[test]
    fn compact_shell_width_retargets_smoothly_instead_of_snapping() {
        let mid_width = COMPACT_WIDTH + 80.0;
        let target_width = COMPACT_WIDTH + 160.0;
        let advanced = advance_scalar_motion(
            mid_width,
            target_width,
            SHELL_MOTION_SECS / 4.0,
            SHELL_MOTION_SECS,
        );
        let retreating = advance_scalar_motion(
            advanced,
            COMPACT_WIDTH,
            SHELL_MOTION_SECS / 4.0,
            SHELL_MOTION_SECS,
        );

        assert!(advanced > mid_width);
        assert!(advanced < target_width);
        assert!(retreating < advanced);
        assert!(retreating > COMPACT_WIDTH);
    }

    #[test]
    fn expanded_metrics_ignore_compact_text_fit_width() {
        let compact = metrics_with_compact_width(0.0, 0.0, COMPACT_WIDTH + 80.0);
        let expanded = metrics_with_compact_width(0.0, 1.0, COMPACT_WIDTH + 80.0);

        assert_eq!(compact.width, COMPACT_WIDTH + 80.0);
        assert_eq!(expanded.width, EXPANDED_WIDTH);
    }

    #[test]
    fn compact_header_gives_center_slot_more_space_after_title_divider_tightening() {
        let metrics = HudMetrics::with_expansion(0.0, 0.0);
        let title = "Voice control";
        let old_uniform_gap_width = HEADER_GAP_PX * 6.0;
        let old_center = (island_shell_width(metrics)
            - (HEADER_PAD_X_PX * 2.0)
            - HEADER_LEAD_WIDTH_PX
            - header_title_width_px(title)
            - 2.0
            - header_chips_width_px("Socket", "macOS")
            - HEADER_RING_PX
            - old_uniform_gap_width)
            .max(HEADER_CENTER_MIN_WIDTH_PX);
        let tightened = header_layout_widths(metrics, title, "Socket", "macOS");

        assert_eq!(header_gap_width_px(), old_uniform_gap_width - 6.0);
        assert_eq!(tightened.center, old_center + 6.0);
    }

    #[test]
    fn tool_meta_uses_reference_separator_rhythm() {
        let row = SceneToolRow {
            label: "Choosing action".to_string(),
            tool: "Router".to_string(),
            app: "Model".to_string(),
            age: "now".to_string(),
        };

        assert_eq!(tool_meta_text(&row), "Router · Model · now");
    }

    #[test]
    fn compact_scene_never_renders_expanded_body() {
        let snapshot = HudSnapshot::default();
        let display = HudDisplay::from_snapshot(&snapshot);
        let compact = island_scene_from_snapshot(
            &snapshot,
            &display,
            false,
            false,
            DEFAULT_PLANNER_MODEL,
            Duration::ZERO,
        )
        .unwrap();
        let expanded = island_scene_from_snapshot(
            &snapshot,
            &display,
            true,
            false,
            DEFAULT_PLANNER_MODEL,
            Duration::ZERO,
        )
        .unwrap();

        assert!(!scene_renders_expanded_body(&compact));
        assert!(scene_renders_expanded_body(&expanded));
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

        assert_eq!(snapped.origin.x, px(876.0));
        assert_eq!(snapped.origin.y, px(TOP_MARGIN));
        assert_eq!(snapped.size, dropped.size);
    }

    #[test]
    fn startup_fallback_bounds_stay_attached_to_top_edge() {
        let fallback = top_attached_fallback_bounds(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));

        assert_eq!(fallback.origin, point(px(0.0), px(TOP_MARGIN)));
        assert_eq!(fallback.size, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));
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
            origin: point(px(800.0), px(80.0)),
            size: size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
        };

        assert_eq!(snap_island_bounds(left_drop, display).origin.x, px(0.0));
        assert_eq!(snap_island_bounds(right_drop, display).origin.x, px(876.0));
    }

    #[test]
    fn drag_motion_preserves_top_attachment_axis() {
        let start_bounds = Bounds {
            origin: point(px(420.0), px(TOP_MARGIN)),
            size: size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
        };
        let dragged = dragged_island_bounds(
            start_bounds,
            point(px(800.0), px(12.0)),
            point(px(860.0), px(180.0)),
            false,
        )
        .expect("drag crosses the activation threshold");

        assert_eq!(dragged.origin.x, px(480.0));
        assert_eq!(dragged.origin.y, px(TOP_MARGIN));
        assert_eq!(dragged.size, start_bounds.size);
    }

    #[test]
    fn drag_motion_waits_for_reference_threshold() {
        let start_bounds = Bounds {
            origin: point(px(420.0), px(TOP_MARGIN)),
            size: size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
        };

        assert_eq!(
            dragged_island_bounds(
                start_bounds,
                point(px(800.0), px(12.0)),
                point(px(802.0), px(180.0)),
                false,
            ),
            None
        );
        assert!(dragged_island_bounds(
            start_bounds,
            point(px(800.0), px(12.0)),
            point(px(802.0), px(180.0)),
            true,
        )
        .is_some());
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
    fn stoplights_only_reveal_over_left_orb_well() {
        let window_bounds = Bounds {
            origin: point(px(100.0), px(20.0)),
            size: size(px(COMPACT_WIDTH), px(COMPACT_HEIGHT)),
        };
        let metrics = HudMetrics::with_expansion(0.0, 0.0);
        let hover = stoplight_hover_bounds(window_bounds, metrics);

        assert!(point_inside_bounds(
            point(
                px(100.0 + HEADER_PAD_X_PX + HEADER_ORB_PX / 2.0),
                px(20.0 + COMPACT_HEIGHT / 2.0)
            ),
            hover
        ));
        assert!(point_inside_bounds(
            point(
                px(100.0 + HEADER_PAD_X_PX + stoplight_cluster_width_px() - 1.0),
                px(20.0 + COMPACT_HEIGHT / 2.0)
            ),
            hover
        ));
        assert!(!point_inside_bounds(
            point(
                px(100.0 + COMPACT_WIDTH / 2.0),
                px(20.0 + COMPACT_HEIGHT / 2.0)
            ),
            hover
        ));
        assert!(!point_inside_bounds(
            point(
                px(100.0 + COMPACT_WIDTH - 20.0),
                px(20.0 + COMPACT_HEIGHT / 2.0)
            ),
            hover
        ));
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
            DEFAULT_PLANNER_MODEL.to_string(),
            island_bounds,
            false,
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
    fn programmed_scene_drives_hud_until_reset() {
        let (tx, rx) = channel();
        let island_bounds = Arc::new(Mutex::new(None));
        let mut hud = VoiceHud::new(
            rx,
            UiMode::Headful,
            DEFAULT_PLANNER_MODEL.to_string(),
            island_bounds,
            false,
        );
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::AgentStep {
            label: "render programmable scene".to_string(),
            source: Some("automation".to_string()),
            task: Some("UI protocol".to_string()),
            tool: Some("Unix socket".to_string()),
            step_index: Some(2),
            step_total: Some(3),
            ttl_ms: None,
        });
        let display = HudDisplay::from_snapshot(&snapshot);
        let scene = island_scene_from_snapshot(
            &snapshot,
            &display,
            true,
            false,
            DEFAULT_PLANNER_MODEL,
            Duration::ZERO,
        )
        .unwrap();

        tx.send(VoiceUiEvent::SceneSet(scene.clone())).unwrap();
        assert_eq!(hud.drain_events(), Some(true));
        assert_eq!(hud.custom_scene.as_ref(), Some(&scene));

        tx.send(VoiceUiEvent::SceneReset).unwrap();
        assert_eq!(hud.drain_events(), None);
        assert!(hud.custom_scene.is_none());
    }

    #[test]
    fn animated_island_bounds_preserves_center_and_top_attachment() {
        let display = Bounds {
            origin: point(px(0.0), px(0.0)),
            size: size(px(1512.0), px(982.0)),
        };
        let compact = Bounds {
            origin: point(px(416.0), px(0.0)),
            size: size(px(WINDOW_WIDTH), px(COMPACT_HEIGHT)),
        };
        let expanded_metrics = HudMetrics::with_expansion(0.0, 1.0);

        let expanded = animated_island_bounds(compact, expanded_metrics, 0.0, display);

        assert_eq!(expanded.origin.y, px(TOP_MARGIN));
        assert_eq!(expanded.size, size(px(EXPANDED_WIDTH), px(EXPANDED_HEIGHT)));
        assert_eq!(expanded.origin.x, px(320.0));
    }

    #[test]
    fn hud_window_starts_passive_until_expanded() {
        let bounds = Bounds {
            origin: point(px(416.0), px(0.0)),
            size: size(px(WINDOW_WIDTH), px(COMPACT_HEIGHT)),
        };
        let options = hud_window_options(bounds);

        assert!(!options.focus);
        assert_eq!(options.kind, WindowKind::PopUp);
        assert!(!options.mouse_passthrough);
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
        assert!(should_render_minimized_content(true, 0.0));
        assert!(should_render_minimized_content(true, 0.2));
        assert!(!should_render_minimized_content(false, 0.54));
        assert!(should_render_minimized_content(false, 0.55));
    }

    #[test]
    fn step_label_stays_compact_and_structured() {
        assert_eq!(
            scene_center_text_for_test(step_snapshot(2, 5, "checking target")),
            "Step 2/5   checking target"
        );
    }

    #[test]
    fn step_label_accepts_declarative_totals_beyond_voice_defaults() {
        assert_eq!(
            scene_center_text_for_test(step_snapshot(37, 120, "verifying the selected window")),
            "Step 37/120   verifying the selected window"
        );
    }

    #[test]
    fn idle_center_text_does_not_show_zero_step_counter() {
        let snapshot = HudSnapshot::default();

        assert_eq!(scene_center_text_for_test(snapshot), "Ready");
    }

    #[test]
    fn recording_center_text_is_plain_listening() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::Listening { ms: 1_250 });

        assert_eq!(scene_center_text_for_test(snapshot), "Listening");
    }

    #[test]
    fn accepted_center_text_is_plain_accepted() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::Accepted);

        assert_eq!(scene_center_text_for_test(snapshot), "Accepted");
    }

    #[test]
    fn recording_stopped_center_text_is_plain_stopped() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::RecordingStopped);

        assert_eq!(scene_center_text_for_test(snapshot), "Recording Stopped");
    }

    #[test]
    fn transcribing_center_text_is_plain_processing() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::Transcribing);

        assert_eq!(scene_center_text_for_test(snapshot), "Processing");
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
            DEFAULT_PLANNER_MODEL.to_string(),
            Arc::new(Mutex::new(None)),
            false,
        );

        assert_eq!(hud.snapshot.mode, UiMode::Headless);
        assert_eq!(hud.snapshot.input_label, "Automation");
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
            scene_center_text_for_test(snapshot),
            "Step 2/5   Opening Safari with cua"
        );
    }

    #[test]
    fn center_text_omits_counter_without_protocol_step_numbers() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::Planning {
            tool: "OpenRouter Vision".to_string(),
        });
        assert_eq!(
            scene_center_text_for_test(snapshot.clone()),
            "Choosing action"
        );

        snapshot.apply(VoiceUiEvent::AgentStep {
            label: "checking target".to_string(),
            source: Some("remote".to_string()),
            task: Some("Web browsing".to_string()),
            tool: Some("Unix socket".to_string()),
            step_index: None,
            step_total: None,
            ttl_ms: Some(2_000),
        });
        assert_eq!(scene_center_text_for_test(snapshot), "checking target");
    }

    #[test]
    fn marquee_stays_still_for_short_center_text() {
        assert_eq!(marquee_offset_px("Ready", 356.0, 10.0), 0.0);
    }

    #[test]
    fn marquee_waits_then_scrolls_long_center_text() {
        let text =
            "Step 7/120   validating a long custom agent step that needs to reveal itself slowly";
        let viewport_width = 356.0;

        assert_eq!(
            marquee_offset_px(text, viewport_width, MARQUEE_START_DELAY_SECS - 0.1),
            0.0
        );
        assert_eq!(
            marquee_offset_px(text, viewport_width, MARQUEE_START_DELAY_SECS),
            0.0
        );
        assert!(marquee_offset_px(text, viewport_width, MARQUEE_START_DELAY_SECS + 1.0) > 0.0);
    }

    #[test]
    fn marquee_holds_at_end_before_looping() {
        let text =
            "Step 9/42   long custom step label for reviewing every visible change carefully";
        let viewport_width = 356.0;
        let overflow = (estimated_center_text_width_px(text) - viewport_width).max(0.0);
        let scroll_duration = overflow / MARQUEE_SCROLL_SPEED_PX_PER_SEC;
        let end_hold_time = MARQUEE_START_DELAY_SECS + scroll_duration + 0.2;

        assert_eq!(
            marquee_offset_px(text, viewport_width, end_hold_time),
            overflow
        );
    }

    #[test]
    fn activity_dots_are_static_when_idle() {
        let snapshot = HudSnapshot::default();
        let display = HudDisplay::from_snapshot(&snapshot);
        let scene = island_scene_from_snapshot(
            &snapshot,
            &display,
            false,
            false,
            DEFAULT_PLANNER_MODEL,
            Duration::ZERO,
        )
        .unwrap();
        let activity = scene_dot_chase(&scene).unwrap();

        assert!(!activity.active);
        assert_eq!(
            activity_dot_alpha(0, 0.0, activity.active, f32::from(activity.speed)),
            0.24
        );
        assert_eq!(
            activity_dot_alpha(0, 10.0, activity.active, f32::from(activity.speed)),
            0.24
        );
        assert_eq!(
            activity_dot_alpha(5, 10.0, activity.active, f32::from(activity.speed)),
            0.24
        );
    }

    #[test]
    fn activity_dots_run_a_circular_trailing_chase_when_active() {
        let mut snapshot = HudSnapshot::default();
        snapshot.apply(VoiceUiEvent::Dispatching(
            "mouse click at 10,10".to_string(),
        ));
        let display = HudDisplay::from_snapshot(&snapshot);
        let scene = island_scene_from_snapshot(
            &snapshot,
            &display,
            false,
            false,
            DEFAULT_PLANNER_MODEL,
            Duration::ZERO,
        )
        .unwrap();
        let activity = scene_dot_chase(&scene).unwrap();
        let speed = f32::from(activity.speed);
        let count = activity.count as usize;
        let start = (0..count)
            .map(|index| activity_dot_alpha(index, 0.0, true, speed))
            .collect::<Vec<_>>();
        let later = (0..count)
            .map(|index| activity_dot_alpha(index, 1.0 / speed, true, speed))
            .collect::<Vec<_>>();
        let wrapped = (0..count)
            .map(|index| activity_dot_alpha(index, count as f32 / speed, true, speed))
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
    fn activity_ring_active_spinner_exposes_curved_partial_sweep() {
        let sweep = activity_ring_sweep_deg(true, None);

        assert_eq!(sweep, ACTIVE_RING_SWEEP_DEG);
        assert!(sweep >= 120.0);
        assert!(sweep < 180.0);
        assert_eq!(activity_ring_sweep_deg(false, None), 0.0);
    }

    #[test]
    fn activity_ring_step_counter_remains_progress_gauge() {
        assert_eq!(activity_ring_sweep_deg(true, Some((1, 4))), 90.0);
        assert_eq!(activity_ring_sweep_deg(false, Some((2, 4))), 180.0);
        assert_eq!(activity_ring_sweep_deg(true, Some((5, 4))), 360.0);
    }

    #[test]
    fn activity_dots_keep_one_neon_blue_family() {
        let head = activity_dot_style(0, 6, 0.0, true, 8.0);
        let immediate_trail = activity_dot_style(5, 6, 0.0, true, 8.0);
        let older_trail = activity_dot_style(4, 6, 0.0, true, 8.0);
        let dormant = activity_dot_style(3, 6, 0.0, true, 8.0);

        assert!(head.alpha > immediate_trail.alpha);
        assert!(immediate_trail.alpha > older_trail.alpha);
        assert!(older_trail.alpha > dormant.alpha);
        assert!(head.lightness < immediate_trail.lightness);
        assert!(immediate_trail.lightness < older_trail.lightness);
    }

    #[test]
    fn actor_motion_resolves_without_changing_layout_metrics() {
        let actor = cua_core::IslandActor {
            id: "pet".to_string(),
            kind: cua_core::IslandActorKind::Sprite,
            layer: cua_core::IslandLayer::Actor,
            anchor: cua_core::IslandAnchor::Canvas,
            x: 20,
            y: 10,
            width: 12,
            height: 12,
            motion: Some(cua_core::IslandMotion::WalkTo {
                region: "right".to_string(),
                item: "activity".to_string(),
                duration_ms: 1_000,
            }),
            interactive: false,
        };
        let metrics = HudMetrics::with_expansion(0.0, 0.0);

        let start = actor_style(&actor, metrics, 0.0);
        let middle = actor_style(&actor, metrics, 0.25);
        let end = actor_style(&actor, metrics, 0.5);

        assert_eq!(island_shell_width(metrics), COMPACT_WIDTH);
        assert_eq!(island_width(metrics), WINDOW_WIDTH);
        assert_eq!(start.x, 20.0);
        assert!(middle.x > start.x);
        assert!(end.x > middle.x);
        assert_eq!(start.y, 10.0);
    }

    #[test]
    fn actor_region_anchors_stay_inside_visible_island() {
        let metrics = HudMetrics::with_expansion(0.0, 1.0);
        let regions = [
            ("header_left", "orb"),
            ("header_left", "input"),
            ("header_center", "status"),
            ("header_right", "transport"),
            ("header_right", "target"),
            ("header_right", "activity"),
            ("task", "task"),
            ("response", "response"),
            ("details_left", "action"),
            ("details_right", "tool_0"),
            ("footer", "elapsed"),
        ];

        for (region, item) in regions {
            let (x, y) = actor_region_anchor(region, item, metrics).unwrap();
            assert!(
                (0.0..=island_width(metrics)).contains(&x),
                "{region}.{item} x={x}"
            );
            assert!(
                (0.0..=(island_height(metrics) - HEADER_ORB_PX)).contains(&y),
                "{region}.{item} y={y}"
            );
        }
    }

    #[test]
    fn actor_palette_inherits_the_phase_accent() {
        let accent = phase_accent(&HudPhase::Dispatching);
        let sprite = actor_fill_color(&IslandActorKind::Sprite, accent);
        let particle = actor_fill_color(&IslandActorKind::Particle, accent);

        assert_eq!(sprite.h, accent.h);
        assert_eq!(particle.h, accent.h);
        assert_eq!(sprite.s, accent.s);
        assert_eq!(particle.s, accent.s);
        assert!(particle.l > sprite.l);
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
    fn stale_single_instance_socket_is_replaced() {
        let profile = format!("test-{}", uuid::Uuid::new_v4());
        let path = single_instance_socket_path(&profile);
        let listener = UnixListener::bind(&path).unwrap();

        assert!(!single_instance_socket_is_responsive(&path));

        drop(listener);
        let instance = SingleInstance::acquire(&profile).unwrap();
        assert!(instance.is_some());
        assert!(single_instance_socket_is_responsive(&path));
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
