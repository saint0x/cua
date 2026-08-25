use crate::ui_state::HudPhase;
use gpui::{hsla, point, px, Background, Bounds, Path, PathBuilder, Pixels, Window};
use std::f32::consts::TAU;

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct OrbPalette {
    pub base: gpui::Hsla,
    pub glow: gpui::Hsla,
    pub core: gpui::Hsla,
    pub accent: gpui::Hsla,
    pub rim: gpui::Hsla,
    pub shell_mid: gpui::Hsla,
    pub shell_edge: gpui::Hsla,
    pub spec: gpui::Hsla,
}

impl OrbPalette {
    pub fn for_phase(phase: &HudPhase) -> Self {
        let mut palette = Self::voice_wave();
        match phase {
            HudPhase::Listening => {
                palette.glow.a = 0.30;
                palette.core.a = 0.70;
                palette.accent.a = 0.58;
            }
            HudPhase::Transcribing | HudPhase::Planning => {
                palette.glow.h = 286.0 / 360.0;
                palette.shell_mid.a = 0.30;
                palette.spec.a = 0.72;
            }
            HudPhase::Dispatching => {
                palette.accent.h = 202.0 / 360.0;
                palette.rim.h = 158.0 / 360.0;
                palette.shell_edge.h = 68.0 / 360.0;
            }
            HudPhase::Error => {
                palette.core.h = 18.0 / 360.0;
                palette.accent.h = 352.0 / 360.0;
                palette.rim.h = 312.0 / 360.0;
                palette.shell_edge.h = 44.0 / 360.0;
            }
            _ => {}
        }
        palette
    }

    fn voice_wave() -> Self {
        Self {
            base: hsla(272.0 / 360.0, 0.65, 0.03, 0.24),
            glow: hsla(301.0 / 360.0, 0.63, 0.49, 0.26),
            core: hsla(301.0 / 360.0, 0.63, 0.49, 0.64),
            accent: hsla(352.0 / 360.0, 1.00, 0.68, 0.56),
            rim: hsla(254.0 / 360.0, 1.00, 0.66, 0.52),
            shell_mid: hsla(286.0 / 360.0, 1.00, 0.77, 0.26),
            shell_edge: hsla(350.0 / 360.0, 1.00, 0.74, 0.24),
            spec: hsla(327.0 / 360.0, 1.00, 0.93, 0.66),
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

    let base = OrbLayer {
        radius: 0.62,
        amplitude: 0.018 + energy * 0.012,
        phase_offset: 0.0,
        alpha_scale: 0.42,
    };
    window.paint_path(
        blob_path(center, radius, base, elapsed * 0.40),
        Background::from(palette.base),
    );

    let separation = 0.18 + energy * 0.08;
    for (index, offset) in [-1.0, -0.34, 0.34, 1.0].iter().enumerate() {
        let path = voice_wave_ribbon_path(center, radius, elapsed, *offset * separation, 44);
        let mut color = match index {
            0 => palette.core,
            1 => palette.rim,
            2 => palette.accent,
            _ => palette.core,
        };
        color.a *= 0.58 + energy * 0.40;
        window.paint_path(path, Background::from(color));
    }
    let mut hot_line = palette.spec;
    hot_line.a *= 0.36 + energy * 0.28;
    window.paint_path(
        voice_wave_ribbon_path(center, radius, elapsed, 0.0, 48),
        Background::from(hot_line),
    );

    let shell = OrbLayer {
        radius: 0.72,
        amplitude: 0.020 + energy * 0.024,
        phase_offset: 2.6,
        alpha_scale: 0.36,
    };
    let mut shell_color = palette.shell_mid;
    shell_color.a *= 0.80 + energy * 0.28;
    window.paint_path(
        blob_path(center, radius, shell, elapsed * 0.62),
        Background::from(shell_color),
    );
    let rim = OrbLayer {
        radius: 0.78,
        amplitude: 0.012 + energy * 0.012,
        phase_offset: 3.5,
        alpha_scale: 0.28,
    };
    let mut rim_color = palette.shell_edge;
    rim_color.a *= 0.75 + energy * 0.30;
    window.paint_path(
        blob_path(center, radius, rim, elapsed * 0.50),
        Background::from(rim_color),
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

pub fn voice_wave_ribbon_points(
    center_x: f32,
    center_y: f32,
    radius: f32,
    elapsed: f32,
    separation: f32,
    samples: usize,
) -> Vec<(f32, f32)> {
    let mut top = Vec::with_capacity(samples);
    let mut bottom = Vec::with_capacity(samples);
    for i in 0..samples {
        let t = i as f32 / (samples - 1).max(1) as f32;
        let x = -0.84 + t * 1.68;
        let edge_falloff = (1.0 - (x * 0.94).powi(2)).max(0.0);
        let envelope = edge_falloff * edge_falloff;
        let drift = elapsed * 2.28;
        let low = 0.5 + 0.5 * (elapsed * 0.37).cos();
        let mid = 0.5 + 0.5 * (elapsed * 0.51 + 1.2).sin();
        let high = 0.5 + 0.5 * (elapsed * 0.73 + 2.1).cos();
        let amplitude = 0.23 + low * 0.018 + mid * 0.025 + high * 0.018;
        let y = envelope * amplitude * (x * 1.10 + drift + separation * 7.0).sin();
        let width = (0.030 + mid * 0.006) * (0.35 + envelope * 0.65);
        top.push((
            center_x + x * radius * 0.76,
            center_y + (y + separation - width) * radius,
        ));
        bottom.push((
            center_x + x * radius * 0.76,
            center_y + (y + separation + width) * radius,
        ));
    }
    bottom.reverse();
    top.extend(bottom);
    top
}

fn voice_wave_ribbon_path(
    center: gpui::Point<Pixels>,
    radius: f32,
    elapsed: f32,
    separation: f32,
    samples: usize,
) -> Path<Pixels> {
    let points = voice_wave_ribbon_points(
        center.x.to_f64() as f32,
        center.y.to_f64() as f32,
        radius,
        elapsed,
        separation,
        samples,
    );
    let mut builder = PathBuilder::fill();
    builder.move_to(point(px(points[0].0), px(points[0].1)));
    for (x, y) in points.iter().skip(1) {
        builder.line_to(point(px(*x), px(*y)));
    }
    builder.close();
    builder.build().expect("voice wave path must be valid")
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
    fn voice_wave_ribbon_stays_inside_glass_shell() {
        let points = voice_wave_ribbon_points(10.0, 10.0, 8.0, 0.7, 0.18, 32);

        assert_eq!(points.len(), 64);
        for (x, y) in points {
            let distance = ((x - 10.0).powi(2) + (y - 10.0).powi(2)).sqrt();
            assert!(distance <= 8.0 * 0.88);
        }
    }

    #[test]
    fn voice_wave_palette_uses_reference_color_family() {
        let palette = OrbPalette::for_phase(&HudPhase::Listening);

        assert!((palette.core.h - 301.0 / 360.0).abs() < 0.002);
        assert!((palette.accent.h - 352.0 / 360.0).abs() < 0.002);
        assert!((palette.rim.h - 254.0 / 360.0).abs() < 0.002);
        assert!(palette.spec.l > 0.90);
    }
}
