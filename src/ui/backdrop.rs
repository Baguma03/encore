//! Animated backdrop shared by the Apple and Vibrant layouts: enlarged, blurred copies of the
//! album art that drift, rotate and blend into one another, like Apple Music's moving artwork.

use egui::emath::Rot2;
use egui::epaint::{Mesh, Vertex};
use egui::{Color32, Painter, Pos2, Rect, Shape, TextureHandle, Vec2};

use super::{cover_rect, with_alpha, ArtView, View, FULL_UV};
use crate::art::ArtTextures;

/// One copy of the art. Its centre drifts on a Lissajous path and its size breathes;
/// positions and drift are fractions of the window, speeds are radians per second.
struct Layer {
    center: [f32; 2],
    amplitude: [f32; 2],
    speed: [f32; 2],
    phase: [f32; 2],
    /// Side as a multiple of the window's longer side. Large enough that the edges never show
    /// on any window shape, given the drift and the breathing.
    scale: f32,
    /// Side of one copy of the art as a multiple of the window's longer side; the square is
    /// filled with mirrored copies around it.
    art: f32,
    /// Rotation speed; the direction alternates between layers.
    spin: f32,
    angle: f32,
    /// Relative size change of the breathing, its speed and phase.
    breathe: f32,
    breathe_speed: f32,
    breathe_phase: f32,
    opacity: f32,
    /// Adds its light to the layers below instead of covering them.
    additive: bool,
}

const LAYERS: [Layer; 5] = [
    Layer {
        center: [0.5, 0.5],
        amplitude: [0.22, 0.18],
        speed: [0.42, 0.33],
        phase: [0.0, 1.2],
        scale: 3.4,
        art: 1.2,
        spin: 0.35,
        angle: 0.0,
        breathe: 0.12,
        breathe_speed: 0.55,
        breathe_phase: 0.0,
        opacity: 1.0,
        additive: false,
    },
    Layer {
        center: [0.5, 0.5],
        amplitude: [0.3, 0.26],
        speed: [0.61, 0.47],
        phase: [2.0, 0.3],
        scale: 3.6,
        art: 0.9,
        spin: -0.5,
        angle: 2.1,
        breathe: 0.14,
        breathe_speed: 0.71,
        breathe_phase: 1.7,
        opacity: 0.8,
        additive: false,
    },
    Layer {
        center: [0.5, 0.5],
        amplitude: [0.28, 0.3],
        speed: [0.52, 0.66],
        phase: [4.2, 3.1],
        scale: 3.6,
        art: 0.75,
        spin: 0.6,
        angle: 4.0,
        breathe: 0.15,
        breathe_speed: 0.48,
        breathe_phase: 3.4,
        opacity: 0.7,
        additive: false,
    },
    Layer {
        center: [0.5, 0.5],
        amplitude: [0.32, 0.22],
        speed: [0.36, 0.57],
        phase: [1.1, 5.0],
        scale: 3.6,
        art: 1.0,
        spin: -0.42,
        angle: 5.3,
        breathe: 0.1,
        breathe_speed: 0.63,
        breathe_phase: 5.0,
        opacity: 0.6,
        additive: false,
    },
    Layer {
        center: [0.5, 0.5],
        amplitude: [0.3, 0.3],
        speed: [0.47, 0.39],
        phase: [3.3, 2.2],
        scale: 3.6,
        art: 0.8,
        spin: 0.45,
        angle: 1.0,
        breathe: 0.15,
        breathe_speed: 0.8,
        breathe_phase: 0.9,
        opacity: 0.22,
        additive: true,
    },
];

/// Paints the moving art over `rect` at egui input time `time` and opacity `strength`,
/// cross-fading from the previous song's art. Paints nothing without art, so callers fill a
/// base first.
pub fn paint_motion(painter: &Painter, rect: Rect, art: &ArtView, time: f64, strength: f32) {
    // Wrap the clock so f32 phases keep their precision over long sessions.
    let t = (time % 100_000.0) as f32;
    if let Some(prev) = art.previous.filter(|_| art.fade < 1.0) {
        paint_layers(painter, rect, prev, t, strength);
    }
    if let Some(cur) = art.current {
        paint_layers(painter, rect, cur, t, art.fade * strength);
    }
}

/// Whether the layouts should draw the current song's artist image: the setting is on and
/// the image exists.
pub fn uses_artist_image(view: &View) -> bool {
    view.artist_bg && view.art.current.is_some_and(|a| a.artist_bg.is_some())
}

/// Paints the artist image covering `rect`, tinted by `tint`, cross-fading from the previous
/// song's image. With `time`, the image slowly pans and zooms.
pub fn paint_artist_image(
    painter: &Painter,
    rect: Rect,
    art: &ArtView,
    tint: Color32,
    time: Option<f64>,
) {
    let draw = |art: &ArtTextures, alpha: f32| {
        let Some(tex) = &art.artist_bg else {
            return;
        };
        let cover = cover_rect(tex.size_vec2(), rect);
        let cover = match time {
            Some(time) => {
                let t = (time % 100_000.0) as f32;
                let zoom = 1.08 + 0.05 * (t * 0.07).sin();
                let pan = Vec2::new((t * 0.043).sin(), (t * 0.031 + 1.0).cos()) * 0.025;
                Rect::from_center_size(cover.center() + pan * rect.size(), cover.size() * zoom)
            }
            None => cover,
        };
        painter.image(tex.id(), cover, FULL_UV, with_alpha(tint, alpha));
    };
    if let Some(prev) = art.previous.filter(|_| art.fade < 1.0) {
        draw(prev, 1.0);
    }
    if let Some(cur) = art.current {
        draw(cur, art.fade);
    }
}

fn paint_layers(painter: &Painter, rect: Rect, art: &ArtTextures, t: f32, alpha: f32) {
    let texture = &art.motion;
    // Light covers are bright enough without the additive highlight.
    let highlight = (1.0 - art.brightness).clamp(0.0, 1.0);
    let size = rect.size();
    let long = size.x.max(size.y);
    for layer in &LAYERS {
        let at = |k: usize| {
            layer.center[k] + layer.amplitude[k] * (layer.speed[k] * t + layer.phase[k]).sin()
        };
        let center = rect.min + Vec2::new(at(0) * size.x, at(1) * size.y);
        let opacity = layer.opacity * if layer.additive { highlight } else { 1.0 };
        let tint = with_alpha(Color32::WHITE, opacity * alpha);
        // Zero alpha in premultiplied colour blends additively.
        let tint = if layer.additive {
            Color32::from_rgba_premultiplied(tint.r(), tint.g(), tint.b(), 0)
        } else {
            tint
        };
        let breath = 1.0 + layer.breathe * (layer.breathe_speed * t + layer.breathe_phase).sin();
        let uv = Rect::from_center_size(Pos2::new(0.5, 0.5), Vec2::splat(layer.scale / layer.art));
        paint_rotated(
            painter,
            texture,
            uv,
            center,
            layer.scale * long * breath,
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
                let smallest = layer.scale * (1.0 - layer.breathe) * long;
                assert!(smallest / 2.0 > corner, "{w}x{h}");
            }
        }
    }
}
