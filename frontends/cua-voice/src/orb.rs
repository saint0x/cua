use crate::ui_state::HudPhase;
use gpui::{hsla, point, px, Background, Bounds, Path, PathBuilder, Pixels, Window};
use std::f32::consts::TAU;

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct OrbPalette {
    pub glow: gpui::Hsla,
    pub core: gpui::Hsla,
    pub accent: gpui::Hsla,
    pub rim: gpui::Hsla,
}

impl OrbPalette {
    pub fn for_phase(phase: &HudPhase) -> Self {
        match phase {
            HudPhase::Listening => Self::new(190.0, 202.0, 322.0, 44.0),
            HudPhase::Transcribing | HudPhase::Planning => Self::new(286.0, 318.0, 198.0, 52.0),
            HudPhase::Dispatching => Self::new(150.0, 178.0, 214.0, 66.0),
            HudPhase::Error => Self::new(352.0, 18.0, 312.0, 44.0),
            _ => Self::new(252.0, 196.0, 318.0, 48.0),
        }
    }

    fn new(glow: f32, core: f32, accent: f32, rim: f32) -> Self {
        Self {
            glow: hsla(glow / 360.0, 0.95, 0.60, 0.22),
            core: hsla(core / 360.0, 0.96, 0.68, 0.54),
            accent: hsla(accent / 360.0, 0.90, 0.64, 0.48),
            rim: hsla(rim / 360.0, 0.94, 0.70, 0.42),
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

    for (index, layer) in ORB_LAYERS.iter().enumerate() {
        let path = blob_path(center, radius, *layer, elapsed + index as f32 * 0.21);
        window.paint_path(path, layer_color(palette, index, layer.alpha_scale));
    }

    let shine = Bounds {
        origin: point(center.x - px(radius * 0.18), center.y - px(radius * 0.30)),
        size: gpui::size(px(radius * 0.28), px(radius * 0.18)),
    };
    let mut highlight = hsla(0.0, 0.0, 1.0, 0.40);
    if matches!(phase, HudPhase::Listening | HudPhase::Dispatching) {
        highlight.a = 0.52;
    }
    window.paint_quad(gpui::fill(shine, highlight));
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
}
