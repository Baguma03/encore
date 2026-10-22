//! Animated backdrop shared by the Apple and Vibrant layouts: enlarged, blurred copies of the
//! album art that drift, rotate and blend into one another, like Apple Music's moving artwork.

use egui::emath::Rot2;
use egui::epaint::{Mesh, Vertex};
use egui::{Color32, Painter, Pos2, Rect, Shape, TextureHandle, Vec2};

use super::{with_alpha, ArtView};

/// One copy of the art. Its centre drifts on a Lissajous path; positions and drift are
/// fractions of the window, speeds are radians per second.
struct Layer {
    center: [f32; 2],
    amplitude: [f32; 2],
    speed: [f32; 2],
    phase: [f32; 2],
    /// Side as a multiple of the window's longer side. Large enough that the edges never show
    /// on any window shape, given the drift.
    scale: f32,
    /// Side of one copy of the art as a multiple of the window's longer side; the square is
    /// filled with mirrored copies around it.
    art: f32,
    /// Rotation speed in radians per second; the sign alternates between layers.
    spin: f32,
    angle: f32,
    opacity: f32,
}

const LAYERS: [Layer; 4] = [
    Layer {
        center: [0.5, 0.5],
        amplitude: [0.08, 0.06],
        speed: [0.34, 0.27],
        phase: [0.0, 1.2],
        scale: 2.6,
        art: 1.3,
        spin: 0.12,
        angle: 0.0,
        opacity: 1.0,
    },
    Layer {
        center: [0.35, 0.4],
        amplitude: [0.12, 0.1],
        speed: [0.43, 0.53],
        phase: [2.0, 0.3],
        scale: 2.3,
        art: 1.0,
        spin: -0.17,
        angle: 2.1,
        opacity: 0.55,
    },
    Layer {
        center: [0.65, 0.6],
        amplitude: [0.1, 0.12],
        speed: [0.50, 0.37],
        phase: [4.2, 3.1],
        scale: 2.3,
        art: 0.85,
        spin: 0.2,
        angle: 4.0,
        opacity: 0.5,
    },
    Layer {
        center: [0.5, 0.35],
        amplitude: [0.12, 0.08],
        speed: [0.30, 0.46],
        phase: [1.1, 5.0],
        scale: 2.1,
        art: 1.15,
        spin: -0.14,
        angle: 5.3,
        opacity: 0.35,
    },
];

/// Paints the moving art over `rect` at egui input time `time` and opacity `strength`,
/// cross-fading from the previous song's art. Paints nothing without art, so callers fill a
/// base first.
pub fn paint_motion(painter: &Painter, rect: Rect, art: &ArtView, time: f64, strength: f32) {
    // Wrap the clock so f32 phases keep their precision over long sessions.
    let t = (time % 100_000.0) as f32;
    if let Some(prev) = art.previous.filter(|_| art.fade < 1.0) {
        paint_layers(painter, rect, &prev.motion, t, strength);
    }
    if let Some(cur) = art.current {
        paint_layers(painter, rect, &cur.motion, t, art.fade * strength);
    }
}

fn paint_layers(painter: &Painter, rect: Rect, texture: &TextureHandle, t: f32, alpha: f32) {
    let size = rect.size();
    let long = size.x.max(size.y);
    for layer in &LAYERS {
        let at = |k: usize| {
            layer.center[k] + layer.amplitude[k] * (layer.speed[k] * t + layer.phase[k]).sin()
        };
        let center = rect.min + Vec2::new(at(0) * size.x, at(1) * size.y);
        let tint = with_alpha(Color32::WHITE, layer.opacity * alpha);
        let uv = Rect::from_center_size(Pos2::new(0.5, 0.5), Vec2::splat(layer.scale / layer.art));
        paint_rotated(
            painter,
            texture,
            uv,
            center,
            layer.scale * long,
            layer.angle + layer.spin * t,
            tint,
        );
    }
}

/// Paints the `uv` part of `texture` as a square of side `side` centred on `center`, turned by
/// `angle` radians.
fn paint_rotated(
    painter: &Painter,
    texture: &TextureHandle,
    uv: Rect,
    center: Pos2,
    side: f32,
    angle: f32,
    tint: Color32,
) {
    let rot = Rot2::from_angle(angle);
    let half = side / 2.0;
    let mut mesh = Mesh::with_texture(texture.id());
    for (corner, uv) in [
        (Vec2::new(-half, -half), uv.left_top()),
        (Vec2::new(half, -half), uv.right_top()),
        (Vec2::new(half, half), uv.right_bottom()),
        (Vec2::new(-half, half), uv.left_bottom()),
    ] {
        mesh.vertices.push(Vertex {
            pos: center + rot * corner,
            uv,
            color: tint,
        });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each layer's square must cover the window at any drift: its inscribed circle has to
    /// reach the farthest window corner from the most displaced centre.
    #[test]
    fn layers_always_cover_the_window() {
        for (w, h) in [(1920.0f32, 1080.0f32), (1080.0, 1920.0), (1080.0, 1080.0)] {
            let long = w.max(h);
            for layer in &LAYERS {
                let reach = |k: usize, len: f32| {
                    let lo = (layer.center[k] - layer.amplitude[k]) * len;
                    let hi = (layer.center[k] + layer.amplitude[k]) * len;
                    hi.max(len - lo)
                };
                let corner = Vec2::new(reach(0, w), reach(1, h)).length();
                assert!(layer.scale * long / 2.0 > corner, "{w}x{h}");
            }
        }
    }
}
