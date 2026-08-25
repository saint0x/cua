use clap::Parser;
use cua_voice::activation::ControlDoubleTap;
use cua_voice::hud::{HudDisplay, HudRow, PANEL_RADIUS, TOP_MARGIN, WINDOW_HEIGHT, WINDOW_WIDTH};
use cua_voice::orb::paint_orb;
use cua_voice::ui_state::{HudSnapshot, VoiceUiEvent};
use cua_voice::{run_voice_turn, VoiceConfig};
use gpui::{
    canvas, div, hsla, point, prelude::*, px, rgb, size, App, Application, Bounds, BoxShadow,
    Context, IntoElement, ParentElement, Render, Styled, Window, WindowBackgroundAppearance,
    WindowBounds, WindowKind, WindowOptions,
};
use rdev::{listen, EventType, Key};
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
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                VoiceUiEvent::Armed if self.execute_turns && !self.busy => {
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

    fn row(dot: gpui::Hsla, row: &HudRow) -> impl IntoElement {
        div()
            .h(px(23.0))
            .flex()
            .items_center()
            .gap_2()
            .child(div().w(px(6.0)).h(px(6.0)).rounded_full().bg(dot))
            .child(
                div()
                    .flex_1()
                    .text_color(rgb(0xc6c6c9))
                    .text_sm()
                    .child(row.label),
            )
            .child(Self::chip(row.tool))
            .child(Self::chip(row.app))
            .child(
                div()
                    .w(px(35.0))
                    .overflow_hidden()
                    .text_color(rgb(0x67676d))
                    .text_xs()
                    .child(row.age),
            )
    }
}

impl Render for VoiceHud {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_events();
        if self.snapshot.expanded_until.is_some() && !self.snapshot.is_expanded() {
            self.snapshot.apply(VoiceUiEvent::Idle);
        }
        window.request_animation_frame();
        let display = HudDisplay::from_snapshot(&self.snapshot);
        div()
            .size_full()
            .bg(hsla(0.0, 0.0, 0.0, 0.0))
            .flex()
            .items_start()
            .justify_center()
            .child(
                div()
                    .w(px(WINDOW_WIDTH))
                    .h(px(WINDOW_HEIGHT))
                    .rounded(px(PANEL_RADIUS))
                    .overflow_hidden()
                    .bg(hsla(0.0, 0.0, 0.0, 0.95))
                    .shadow(vec![BoxShadow {
                        color: hsla(0.0, 0.0, 0.0, 0.60),
                        blur_radius: px(14.0),
                        spread_radius: px(0.0),
                        offset: point(px(0.0), px(6.0)),
                    }])
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(22.0))
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .w(px(94.0))
                                    .text_color(rgb(0xf3f0f3))
                                    .text_sm()
                                    .child("Help"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_color(hsla(32.0 / 360.0, 0.95, 0.65, 1.0))
                                            .text_xs()
                                            .child("✦"),
                                    )
                                    .child(div().text_color(rgb(0xd8d8dc)).text_xs().child("5h"))
                                    .child(
                                        div()
                                            .text_color(hsla(140.0 / 360.0, 0.86, 0.52, 1.0))
                                            .text_xs()
                                            .child("11%"),
                                    )
                                    .child(div().text_color(rgb(0x56565d)).text_xs().child("4h1m"))
                                    .child(div().text_color(rgb(0x56565d)).text_xs().child("|"))
                                    .child(div().text_color(rgb(0xd8d8dc)).text_xs().child("7d"))
                                    .child(
                                        div()
                                            .text_color(hsla(140.0 / 360.0, 0.86, 0.52, 1.0))
                                            .text_xs()
                                            .child("2%"),
                                    )
                                    .child(div().text_color(rgb(0x56565d)).text_xs().child("6h1m")),
                            ),
                    )
                    .child(
                        div()
                            .h(px(58.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .rounded(px(5.0))
                            .bg(hsla(0.0, 0.0, 1.0, 0.055))
                            .px_2()
                            .gap_2()
                            .child(div().w(px(14.0)).child(self.orb()))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .w_full()
                                    .child(
                                        div()
                                            .text_color(rgb(0xf6f7fb))
                                            .text_sm()
                                            .child(display.title),
                                    )
                                    .child(
                                        div()
                                            .text_color(rgb(0x828288))
                                            .text_sm()
                                            .child(format!("You: {}", display.prompt)),
                                    )
                                    .child(
                                        div()
                                            .text_color(hsla(140.0 / 360.0, 0.90, 0.55, 1.0))
                                            .text_sm()
                                            .child(display.result),
                                    ),
                            )
                            .child(Self::chip(display.phase))
                            .child(Self::chip(display.tool))
                            .child(
                                div()
                                    .w(px(28.0))
                                    .overflow_hidden()
                                    .text_color(rgb(0x67676d))
                                    .text_xs()
                                    .child("now"),
                            ),
                    )
                    .child(Self::row(
                        hsla(216.0 / 360.0, 0.96, 0.58, 1.0),
                        &display.rows[0],
                    ))
                    .child(Self::row(
                        hsla(142.0 / 360.0, 0.78, 0.50, 1.0),
                        &display.rows[1],
                    )),
            )
    }
}

fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    let demo = args.demo;
    let config = VoiceConfig {
        profile: args.profile,
        record_ms: args.record_ms,
        stt_model: args.stt_model,
        planner_model: args.planner_model,
    };
    let runtime = Arc::new(tokio::runtime::Runtime::new()?);
    let (tx, rx) = channel::<VoiceUiEvent>();
    start_double_control_listener(tx.clone());
    if demo {
        start_demo_cycle(tx.clone());
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
    std::thread::spawn(move || {
        let sequence = [
            VoiceUiEvent::Armed,
            VoiceUiEvent::Listening { ms: 1200 },
            VoiceUiEvent::Transcribing,
            VoiceUiEvent::Transcript("Click 640 360".to_string()),
            VoiceUiEvent::Planning,
            VoiceUiEvent::Dispatching("click 640 360".to_string()),
            VoiceUiEvent::Reply("Clicked the center target.".to_string()),
        ];
        for event in sequence {
            tx.send(event).ok();
            std::thread::sleep(Duration::from_millis(650));
        }
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
