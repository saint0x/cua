use crate::ui_state::HudPhase;
use gpui::{fill, hsla, point, px, Bounds, Corners, Pixels, Window};
use std::f32::consts::TAU;

const SPHERE_FRACTION: f32 = 0.78;
const CAMERA_TILT: f32 = 0.30;
const BAND_TILT: f32 = 0.55;
const EDGE_THINNING: f32 = 0.25;
const EDGE_LIGHTENING: f32 = 0.18;
const INK_NEAR: f32 = 0.08;
const INK_FAR: f32 = 0.52;
const GHOST_INK: f32 = 0.78;
const ALPHA_FAR: f32 = 0.40;
const ALPHA_NEAR: f32 = 1.00;
const GOLDEN_ANGLE: f32 = std::f32::consts::PI * (3.0 - 2.236_068);

const WAVES: [OrbWave; 2] = [
    OrbWave {
        amplitude: 0.16,
        spatial: 3.0,
        temporal: -1.7,
        lane_phase: 0.22,
    },
    OrbWave {
        amplitude: 0.07,
        spatial: 5.0,
        temporal: 1.1,
        lane_phase: 0.0,
    },
];

#[derive(Debug, Copy, Clone, PartialEq)]
struct OrbWave {
    amplitude: f32,
    spatial: f32,
    temporal: f32,
    lane_phase: f32,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct DottedOrbTuning {
    speed: f32,
    lanes: usize,
    samples_per_lane: usize,
    lane_spacing: f32,
    radius_base: f32,
    radius_depth: f32,
    ghost_dots: usize,
    ghost_radius: f32,
    ghost_alpha_far: f32,
    ghost_alpha_near: f32,
}

const TINY: DottedOrbTuning = DottedOrbTuning {
    speed: 3.4,
    lanes: 6,
    samples_per_lane: 18,
    lane_spacing: 0.17,
    radius_base: 2.42,
    radius_depth: 3.74,
    ghost_dots: 10,
    ghost_radius: 1.9,
    ghost_alpha_far: 0.16,
    ghost_alpha_near: 0.42,
};

const SMALL: DottedOrbTuning = DottedOrbTuning {
    speed: 3.0,
    lanes: 9,
    samples_per_lane: 30,
    lane_spacing: 0.115,
    radius_base: 1.87,
    radius_depth: 2.89,
    ghost_dots: 20,
    ghost_radius: 1.4,
    ghost_alpha_far: 0.14,
    ghost_alpha_near: 0.38,
};

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct DottedOrbDot {
    pub x: f32,
    pub y: f32,
    pub depth: f32,
    pub radius: f32,
    pub ink: f32,
    pub alpha: f32,
}

pub fn paint_orb(window: &mut Window, bounds: Bounds<Pixels>, phase: &HudPhase, elapsed: f32) {
    let size = bounds.size.width.min(bounds.size.height).to_f64() as f32;
    let tuning = tuning_for_size(size);
    let dots = dotted_orb_dots(
        size,
        elapsed * tuning.speed * phase_wave_rate(phase),
        tuning,
    );
    let hue = phase_accent_hue(phase);
    let cast = phase_accent_cast(phase);

    for dot in dots {
        let level = 1.0 - dot.ink.clamp(0.0, 1.0);
        let accent_mix = cast * level;
        let color = hsla(hue, accent_mix, level, dot.alpha);
        let radius = dot.radius.max(0.35);
        let dot_bounds = Bounds {
            origin: point(
                bounds.origin.x + px(dot.x - radius),
                bounds.origin.y + px(dot.y - radius),
            ),
            size: gpui::size(px(radius * 2.0), px(radius * 2.0)),
        };
        window.paint_quad(fill(dot_bounds, color).corner_radii(Corners::all(px(radius))));
    }
}

pub fn dotted_orb_dots(size: f32, elapsed: f32, tuning: DottedOrbTuning) -> Vec<DottedOrbDot> {
    let sphere_radius = (size / 2.0) * SPHERE_FRACTION;
    let radius_scale = (size / 300.0).powf(0.60);
    let mut dots = Vec::with_capacity(tuning.ghost_dots + tuning.lanes * tuning.samples_per_lane);

    build_ghost_sphere(&mut dots, size, sphere_radius, radius_scale, tuning);
    build_band(
        &mut dots,
        size,
        sphere_radius,
        radius_scale,
        elapsed,
        tuning,
    );
    dots.sort_by(|a, b| a.depth.total_cmp(&b.depth));
    dots
}

fn build_ghost_sphere(
    out: &mut Vec<DottedOrbDot>,
    size: f32,
    sphere_radius: f32,
    radius_scale: f32,
    tuning: DottedOrbTuning,
) {
    for i in 0..tuning.ghost_dots {
        let y = 1.0 - (2.0 * (i as f32 + 0.5)) / tuning.ghost_dots as f32;
        let ring = (1.0 - y * y).max(0.0).sqrt();
        let theta = i as f32 * GOLDEN_ANGLE;
        push_dot(
            out,
            size,
            sphere_radius,
            ring * theta.cos(),
            y,
            ring * theta.sin(),
            tuning.ghost_radius * radius_scale,
            GHOST_INK,
            tuning.ghost_alpha_far,
            tuning.ghost_alpha_near,
        );
    }
}

fn build_band(
    out: &mut Vec<DottedOrbDot>,
    size: f32,
    sphere_radius: f32,
    radius_scale: f32,
    elapsed: f32,
    tuning: DottedOrbTuning,
) {
    let mid = (tuning.lanes - 1) as f32 / 2.0;
    let cos_tilt = BAND_TILT.cos();
    let sin_tilt = BAND_TILT.sin();

    for lane in 0..tuning.lanes {
        let lane_offset = (lane as f32 - mid) * tuning.lane_spacing;
        let edge = (lane as f32 - mid).abs() / mid.max(1.0);
        for sample in 0..tuning.samples_per_lane {
            let theta = sample as f32 / tuning.samples_per_lane as f32 * TAU;
            let mut wave = 0.0;
            for source in WAVES {
                wave += source.amplitude
                    * (source.spatial * theta
                        + source.temporal * elapsed
                        + lane as f32 * source.lane_phase)
                        .sin();
            }

            let offset = lane_offset + wave;
            let x = theta.cos();
            let y = cos_tilt * theta.sin() - sin_tilt * offset;
            let z = sin_tilt * theta.sin() + cos_tilt * offset;
            let length = x.hypot(y).hypot(z).max(f32::EPSILON);
            let ux = x / length;
            let uy = y / length;
            let uz = z / length;

            let near = projected_near(uy, uz);
            let radius = (tuning.radius_base + tuning.radius_depth * near)
                * (1.0 - EDGE_THINNING * edge)
                * radius_scale;
            let ink = INK_FAR + (INK_NEAR - INK_FAR) * near + EDGE_LIGHTENING * edge;
            let alpha = ALPHA_FAR + (ALPHA_NEAR - ALPHA_FAR) * near;
            push_dot(
                out,
                size,
                sphere_radius,
                ux,
                uy,
                uz,
                radius,
                ink,
                alpha,
                alpha,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_dot(
    out: &mut Vec<DottedOrbDot>,
    size: f32,
    sphere_radius: f32,
    ux: f32,
    uy: f32,
    uz: f32,
    radius: f32,
    ink: f32,
    alpha_far: f32,
    alpha_near: f32,
) {
    let center = size / 2.0;
    let x = ux * sphere_radius;
    let y = uy * sphere_radius;
    let z = uz * sphere_radius;
    let cos_tilt = CAMERA_TILT.cos();
    let sin_tilt = CAMERA_TILT.sin();
    let screen_y = y * cos_tilt - z * sin_tilt;
    let depth = y * sin_tilt + z * cos_tilt;
    let near = (depth / sphere_radius + 1.0) / 2.0;
    let alpha = alpha_far + (alpha_near - alpha_far) * near;

    if alpha >= 0.02 {
        out.push(DottedOrbDot {
            x: center + x,
            y: center - screen_y,
            depth,
            radius: radius.max(0.3),
            ink,
            alpha,
        });
    }
}

fn projected_near(uy: f32, uz: f32) -> f32 {
    let depth = uy * CAMERA_TILT.sin() + uz * CAMERA_TILT.cos();
    (depth + 1.0) / 2.0
}

pub fn tuning_for_size(size: f32) -> DottedOrbTuning {
    if size < 24.0 {
        TINY
    } else {
        SMALL
    }
}

fn phase_wave_rate(phase: &HudPhase) -> f32 {
    match phase {
        HudPhase::Idle => 0.30,
        HudPhase::Listening => 1.60,
        HudPhase::Dispatching | HudPhase::Reply => 1.25,
        HudPhase::Error => 1.85,
        _ => 1.05,
    }
}

fn phase_accent_cast(phase: &HudPhase) -> f32 {
    match phase {
        HudPhase::Idle => 0.05,
        HudPhase::Listening => 0.32,
        HudPhase::Dispatching | HudPhase::Reply => 0.28,
        HudPhase::Error => 0.36,
        _ => 0.26,
    }
}

fn phase_accent_hue(phase: &HudPhase) -> f32 {
    match phase {
        HudPhase::Listening => 18.0 / 360.0,
        HudPhase::Dispatching | HudPhase::Reply => 148.0 / 360.0,
        HudPhase::Error => 3.0 / 360.0,
        HudPhase::RecordingStopped
        | HudPhase::Accepted
        | HudPhase::Transcribing
        | HudPhase::Planning => 276.0 / 360.0,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_orb_uses_authored_small_and_tiny_presets() {
        assert_eq!(tuning_for_size(18.0).lanes, 6);
        assert_eq!(tuning_for_size(30.0).lanes, 9);
    }

    #[test]
    fn dotted_orb_geometry_is_depth_sorted_and_bounded() {
        let size = 30.0;
        let dots = dotted_orb_dots(size, 1.7, tuning_for_size(size));

        assert!(!dots.is_empty());
        for pair in dots.windows(2) {
            assert!(pair[0].depth <= pair[1].depth);
        }
        for dot in dots {
            assert!(dot.x.is_finite() && dot.y.is_finite());
            assert!(dot.radius >= 0.3);
            assert!(dot.alpha >= 0.02);
            assert!(dot.x >= -3.0 && dot.x <= size + 3.0);
            assert!(dot.y >= -3.0 && dot.y <= size + 3.0);
        }
    }

    #[test]
    fn phase_rates_keep_idle_slow_and_listening_alert() {
        assert!(phase_wave_rate(&HudPhase::Idle) < phase_wave_rate(&HudPhase::Planning));
        assert!(phase_wave_rate(&HudPhase::Listening) > phase_wave_rate(&HudPhase::Planning));
    }
}
