use clap::Parser;
use cua_voice::activation::ControlDoubleTap;
use cua_voice::ui_state::{HudPhase, HudSnapshot, VoiceUiEvent};
use cua_voice::{run_voice_turn, VoiceConfig};
use gpui::{
    canvas, div, fill, hsla, point, prelude::*, px, rgb, size, App, Application, Bounds, BoxShadow,
    Context, IntoElement, ParentElement, Render, Styled, Window, WindowBounds, WindowOptions,
};
use rdev::{listen, EventType, Key};
use std::net::SocketAddr;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, Parser)]
#[command(name = "cua-voice", version, about = "Rust voice HUD for CUA")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8765")]
    server_addr: SocketAddr,
    #[arg(long, default_value = "default")]
    profile: String,
    #[arg(long, default_value_t = 4500)]
    record_ms: u64,
    #[arg(long, default_value = "openai/whisper-1")]
    stt_model: String,
    #[arg(long, default_value = "openai/gpt-5.4-mini")]
    planner_model: String,
}

struct VoiceHud {
    rx: Receiver<VoiceUiEvent>,
    tx: Sender<VoiceUiEvent>,
    config: VoiceConfig,
    runtime: Arc<tokio::runtime::Runtime>,
    snapshot: HudSnapshot,
    busy: bool,
    started: Instant,
}

impl VoiceHud {
    fn new(
        rx: Receiver<VoiceUiEvent>,
        tx: Sender<VoiceUiEvent>,
        config: VoiceConfig,
        runtime: Arc<tokio::runtime::Runtime>,
    ) -> Self {
        Self {
            rx,
            tx,
            config,
            runtime,
            snapshot: HudSnapshot::default(),
            busy: false,
            started: Instant::now(),
        }
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                VoiceUiEvent::Armed if !self.busy => {
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
            move |bounds, _, _| (bounds, phase, elapsed),
            move |bounds, (layout, phase, elapsed), window, _| {
                let center = point(
                    layout.origin.x + layout.size.width / 2.0,
                    layout.origin.y + layout.size.height / 2.0,
                );
                let pulse = (elapsed * 4.4).sin() * 0.5 + 0.5;
                let base = match phase {
                    HudPhase::Listening => hsla(200.0 / 360.0, 0.96, 0.62, 1.0),
                    HudPhase::Transcribing | HudPhase::Planning => {
                        hsla(286.0 / 360.0, 0.92, 0.66, 1.0)
                    }
                    HudPhase::Dispatching => hsla(152.0 / 360.0, 0.78, 0.58, 1.0),
                    HudPhase::Error => hsla(352.0 / 360.0, 0.90, 0.62, 1.0),
                    _ => hsla(218.0 / 360.0, 0.88, 0.64, 1.0),
                };
                for i in 0..5 {
                    let scale = 1.0 - (i as f32 * 0.13);
                    let alpha = (0.18 + pulse * 0.16) / (i as f32 + 1.0);
                    let mut color = base;
                    color.a = alpha;
                    let radius = layout.size.width.min(layout.size.height) * scale / 2.0;
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(center.x - radius, center.y - radius),
                            size: size(radius * 2.0, radius * 2.0),
                        },
                        color,
                    ));
                }
                let mut core = rgb(0xdff7ff);
                core.a = 0.88;
                let radius = bounds.size.width.min(bounds.size.height) * (0.22 + pulse * 0.05);
                window.paint_quad(fill(
                    Bounds {
                        origin: point(center.x - radius, center.y - radius),
                        size: size(radius * 2.0, radius * 2.0),
                    },
                    core,
                ));
            },
        )
        .size(px(28.0))
    }
}

impl Render for VoiceHud {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_events();
        if self.snapshot.expanded_until.is_some() && !self.snapshot.is_expanded() {
            self.snapshot.apply(VoiceUiEvent::Idle);
        }
        window.request_animation_frame();
        let expanded = self.snapshot.is_expanded();
        let panel_width = if expanded { 360.0 } else { 520.0 };
        let panel_height = if expanded { 300.0 } else { 54.0 };
        let status_text = self
            .snapshot
            .response
            .clone()
            .or_else(|| self.snapshot.transcript.clone())
            .unwrap_or_else(|| self.snapshot.step.label.clone());
        div()
            .size_full()
            .bg(hsla(0.0, 0.0, 0.0, 0.0))
            .flex()
            .items_start()
            .justify_center()
            .pt(px(16.0))
            .child(
                div()
                    .w(px(panel_width))
                    .h(px(panel_height))
                    .rounded_lg()
                    .bg(hsla(220.0 / 360.0, 0.23, 0.03, 0.92))
                    .shadow(vec![BoxShadow {
                        color: hsla(0.0, 0.0, 0.0, 0.44),
                        blur_radius: px(24.0),
                        spread_radius: px(0.0),
                        offset: point(px(0.0), px(10.0)),
                    }])
                    .border_1()
                    .border_color(hsla(218.0 / 360.0, 0.14, 0.19, 0.75))
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .child(self.orb())
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
                                            .child(self.snapshot.task.clone()),
                                    )
                                    .child(div().text_color(rgb(0x9aa4b2)).text_xs().child(
                                        format!(
                                            "Step {}/{} · {} · {}",
                                            self.snapshot.step.index,
                                            self.snapshot.step.total,
                                            self.snapshot.phase.label(),
                                            self.snapshot.tool
                                        ),
                                    )),
                            )
                            .child(
                                div()
                                    .text_color(rgb(0xb8c0ce))
                                    .text_xs()
                                    .child(self.snapshot.step.label.clone()),
                            ),
                    )
                    .when(expanded, |panel| {
                        panel.child(
                            div()
                                .text_color(rgb(0xf7f8fb))
                                .text_sm()
                                .line_height(px(20.0))
                                .child(status_text),
                        )
                    }),
            )
    }
}

fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let args = Args::parse();
    let config = VoiceConfig {
        server_addr: args.server_addr,
        profile: args.profile,
        record_ms: args.record_ms,
        stt_model: args.stt_model,
        planner_model: args.planner_model,
    };
    let runtime = Arc::new(tokio::runtime::Runtime::new()?);
    let (tx, rx) = channel::<VoiceUiEvent>();
    start_double_control_listener(tx.clone());
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(620.0), px(360.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            {
                let runtime = runtime.clone();
                let tx = tx.clone();
                let config = config.clone();
                move |_, cx| cx.new(|_| VoiceHud::new(rx, tx, config, runtime))
            },
        )
        .unwrap();
        cx.activate(true);
    });
    Ok(())
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
