use crate::ui_state::HudPhase;
use gpui::{hsla, point, px, Background, Bounds, Path, PathBuilder, Pixels, Window};
use std::f32::consts::TAU;

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct OrbPalette {
    pub glow: gpui::Hsla,
    pub core: gpui::Hsla,
    pub accent: gpui::Hsla,
    pub rim: gpui::Hsla,
    pub shell: gpui::Hsla,
    pub spec: gpui::Hsla,
}

impl OrbPalette {
    pub fn for_phase(phase: &HudPhase) -> Self {
        match phase {
            HudPhase::Listening => Self::new(196.0, 306.0, 348.0, 252.0),
            HudPhase::Transcribing | HudPhase::Planning => Self::new(286.0, 318.0, 196.0, 252.0),
            HudPhase::Dispatching => Self::new(158.0, 202.0, 252.0, 68.0),
            HudPhase::Error => Self::new(352.0, 18.0, 312.0, 44.0),
            _ => Self::new(252.0, 306.0, 348.0, 196.0),
        }
    }

    fn new(glow: f32, core: f32, accent: f32, rim: f32) -> Self {
        Self {
            glow: hsla(glow / 360.0, 0.95, 0.60, 0.24),
            core: hsla(core / 360.0, 0.94, 0.60, 0.58),
            accent: hsla(accent / 360.0, 0.94, 0.62, 0.50),
            rim: hsla(rim / 360.0, 0.94, 0.70, 0.46),
            shell: hsla(rim / 360.0, 0.78, 0.78, 0.28),
            spec: hsla(0.0, 0.0, 1.0, 0.62),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct OrbLayer {
    pub radius: f32,
    pub amplitude: f32,
    pub phase_offset: f32,
    pub alpha_scale: f32,
}

pub const ORB_LAYERS: [OrbLayer; 5] = [
    OrbLayer {
        radius: 0.52,
        amplitude: 0.060,
        phase_offset: 0.00,
        alpha_scale: 0.42,
    },
    OrbLayer {
        radius: 0.43,
        amplitude: 0.085,
        phase_offset: 1.35,
        alpha_scale: 0.50,
    },
    OrbLayer {
        radius: 0.34,
        amplitude: 0.110,
        phase_offset: 2.40,
        alpha_scale: 0.56,
    },
    OrbLayer {
        radius: 0.24,
        amplitude: 0.070,
        phase_offset: 3.15,
        alpha_scale: 0.72,
    },
    OrbLayer {
        radius: 0.13,
        amplitude: 0.030,
        phase_offset: 0.85,
        alpha_scale: 0.82,
    },
];

pub fn paint_orb(window: &mut Window, bounds: Bounds<Pixels>, phase: &HudPhase, elapsed: f32) {
    let palette = OrbPalette::for_phase(phase);
    let center = bounds.center();
    let radius = bounds.size.width.min(bounds.size.height).to_f64() as f32 * 0.5;
    let energy = phase_energy(phase);

    let glow = OrbLayer {
        radius: 0.98,
        amplitude: 0.035 + energy * 0.018,
        phase_offset: 0.4,
        alpha_scale: 0.22,
    };
    window.paint_path(
        blob_path(center, radius, glow, elapsed * 0.72),
        Background::from(palette.glow),
    );

    for (index, layer) in ORB_LAYERS.iter().enumerate() {
        let mut layer = *layer;
        layer.amplitude *= 1.0 + energy * 0.55;
        let path = blob_path(
            center,
            radius,
            layer,
            elapsed * (1.0 + energy) + index as f32 * 0.21,
        );
        window.paint_path(path, layer_color(palette, index, layer.alpha_scale));
    }

    for (index, offset) in [-0.11, 0.0, 0.12].iter().enumerate() {
        let path = membrane_path(center, radius, elapsed, *offset, 32);
        let alpha = [0.22, 0.34, 0.20][index] * (0.85 + energy * 0.35);
        let mut color = if index == 1 {
            palette.accent
        } else {
            palette.core
        };
        color.a = alpha;
        window.paint_path(path, Background::from(color));
    }

    let shell = OrbLayer {
        radius: 0.72,
        amplitude: 0.030 + energy * 0.020,
        phase_offset: 2.6,
        alpha_scale: 0.36,
    };
    let mut shell_color = palette.shell;
    shell_color.a *= 0.8 + energy * 0.25;
    window.paint_path(
        blob_path(center, radius, shell, elapsed * 0.62),
        Background::from(shell_color),
    );

    let shine = Bounds {
        origin: point(center.x - px(radius * 0.28), center.y - px(radius * 0.38)),
        size: gpui::size(px(radius * 0.32), px(radius * 0.16)),
    };
    window.paint_quad(gpui::fill(shine, palette.spec));
}

pub fn blob_points(
    center_x: f32,
    center_y: f32,
    radius: f32,
    layer: OrbLayer,
    elapsed: f32,
    samples: usize,
) -> Vec<(f32, f32)> {
    (0..samples)
        .map(|i| {
            let t = i as f32 / samples as f32;
            let theta = t * TAU;
            let wobble = (theta * 3.0 + elapsed * 2.0 + layer.phase_offset).sin() * layer.amplitude
                + (theta * 5.0 - elapsed * 1.35).cos() * layer.amplitude * 0.45;
            let r = radius * layer.radius * (1.0 + wobble);
            (center_x + theta.cos() * r, center_y + theta.sin() * r)
        })
        .collect()
}

pub fn membrane_points(
    center_x: f32,
    center_y: f32,
    radius: f32,
    elapsed: f32,
    y_offset: f32,
    samples: usize,
) -> Vec<(f32, f32)> {
    let width = 0.060 + y_offset.abs() * 0.08;
    let mut top = Vec::with_capacity(samples);
    let mut bottom = Vec::with_capacity(samples);
    for i in 0..samples {
        let t = i as f32 / (samples - 1).max(1) as f32;
        let x = -0.88 + t * 1.76;
        let rim = (1.0 - x * x).max(0.0).powf(0.72);
        let drift = elapsed * 0.82;
        let wave =
            rim * (0.20 * (x * 1.48 + drift).sin() + 0.055 * (x * 3.2 - drift * 0.43 + 1.1).sin());
        let y = y_offset + wave;
        let local_width = width * (0.38 + rim * 0.62);
        top.push((
            center_x + x * radius * 0.76,
            center_y + (y - local_width) * radius,
        ));
        bottom.push((
            center_x + x * radius * 0.76,
            center_y + (y + local_width) * radius,
        ));
    }
    bottom.reverse();
    top.extend(bottom);
    top
}

fn membrane_path(
    center: gpui::Point<Pixels>,
    radius: f32,
    elapsed: f32,
    y_offset: f32,
    samples: usize,
) -> Path<Pixels> {
    let points = membrane_points(
        center.x.to_f64() as f32,
        center.y.to_f64() as f32,
        radius,
        elapsed,
        y_offset,
        samples,
    );
    let mut builder = PathBuilder::fill();
    builder.move_to(point(px(points[0].0), px(points[0].1)));
    for (x, y) in points.iter().skip(1) {
        builder.line_to(point(px(*x), px(*y)));
    }
    builder.close();
    builder.build().expect("orb membrane path must be valid")
}

fn phase_energy(phase: &HudPhase) -> f32 {
    match phase {
        HudPhase::Listening => 0.55,
        HudPhase::Transcribing | HudPhase::Planning => 0.38,
        HudPhase::Dispatching => 0.46,
        HudPhase::Error => 0.22,
        _ => 0.16,
    }
}

fn blob_path(
    center: gpui::Point<Pixels>,
    radius: f32,
    layer: OrbLayer,
    elapsed: f32,
) -> Path<Pixels> {
    let points = blob_points(
        center.x.to_f64() as f32,
        center.y.to_f64() as f32,
        radius,
        layer,
        elapsed,
        42,
    );
    let mut builder = PathBuilder::fill();
    builder.move_to(point(px(points[0].0), px(points[0].1)));
    for (x, y) in points.iter().skip(1) {
        builder.line_to(point(px(*x), px(*y)));
    }
    builder.close();
    builder.build().expect("orb path must be valid")
}

fn layer_color(palette: OrbPalette, index: usize, alpha_scale: f32) -> Background {
    let mut color = match index {
        0 => palette.glow,
        1 => palette.accent,
        2 => palette.core,
        3 => palette.rim,
        _ => hsla(0.0, 0.0, 1.0, 0.75),
    };
    color.a *= alpha_scale;
    color.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_geometry_stays_inside_expected_radius() {
        let layer = ORB_LAYERS[1];
        let points = blob_points(20.0, 20.0, 16.0, layer, 0.25, 48);

        assert_eq!(points.len(), 48);
        for (x, y) in points {
            let distance = ((x - 20.0).powi(2) + (y - 20.0).powi(2)).sqrt();
            assert!(distance < 16.0 * (layer.radius + layer.amplitude * 1.6));
        }
    }

    #[test]
    fn phase_palettes_are_distinct_for_errors() {
        assert_ne!(
            OrbPalette::for_phase(&HudPhase::Error),
            OrbPalette::for_phase(&HudPhase::Idle)
        );
    }

    #[test]
    fn voice_membrane_stays_inside_glass_shell() {
        let points = membrane_points(10.0, 10.0, 8.0, 0.7, 0.0, 32);

        assert_eq!(points.len(), 64);
        for (x, y) in points {
            let distance = ((x - 10.0).powi(2) + (y - 10.0).powi(2)).sqrt();
            assert!(distance <= 8.0 * 0.94);
        }
    }
}
