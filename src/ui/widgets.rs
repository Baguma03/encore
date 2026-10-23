//! Reusable drawing pieces shared between layouts.

use std::f32::consts::TAU;
use std::sync::Arc;

use egui::epaint::{Mesh, PathStroke};
use egui::{
    Color32, CursorIcon, Galley, Id, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Vec2,
};

use super::scheme::Ink;

/// Cubic ease-in-out of `t` in `0.0..=1.0`.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Moves `current` towards `target` with exponential decay at `rate` per second.
pub fn approach(current: f32, target: f32, dt: f32, rate: f32) -> f32 {
    let step = 1.0 - (-rate * dt.max(0.0)).exp();
    let next = current + (target - current) * step;
    if (target - next).abs() < 0.05 {
        target
    } else {
        next
    }
}

/// Eased follower stored in egui memory under `id`; starts at `target` on first use.
/// Requests a repaint while it is still moving.
pub fn smooth_value(ctx: &egui::Context, id: Id, target: f32, rate: f32) -> f32 {
    let dt = ctx.input(|i| i.stable_dt).min(0.1);
    let value = ctx.data_mut(|d| {
        let slot = d.get_temp_mut_or_insert_with(id, || target);
        *slot = approach(*slot, target, dt, rate);
        *slot
    });
    if value != target {
        ctx.request_repaint();
    }
    value
}

/// Opacity multiplier that fades content out within `band` of the top and bottom of `top..bottom`.
pub fn edge_fade(y: f32, top: f32, bottom: f32, band: f32) -> f32 {
    if band <= 0.0 {
        return if (top..=bottom).contains(&y) {
            1.0
        } else {
            0.0
        };
    }
    let from_edge = (y - top).min(bottom - y);
    let t = (from_edge / band).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Paints a galley laid out in white, tinted by `color`, softened by overlapping offset copies
/// when `radius` is above half a point.
pub fn paint_blurred_galley(
    painter: &Painter,
    pos: Pos2,
    galley: &Arc<Galley>,
    color: Color32,
    radius: f32,
) {
    if radius < 0.5 {
        painter.galley_with_override_text_color(pos, galley.clone(), color);
        return;
    }
    const TAPS: usize = 8;
    painter.galley_with_override_text_color(pos, galley.clone(), color.gamma_multiply(0.3));
    for ring in [0.5, 1.0] {
        for k in 0..TAPS {
            let angle = TAU * (k as f32 + ring) / TAPS as f32;
            let offset = Vec2::angled(angle) * radius * ring;
            painter.galley_with_override_text_color(
                pos + offset,
                galley.clone(),
                color.gamma_multiply(0.11),
            );
        }
    }
}

/// Paints a galley laid out in white, tinted by `color`, smeared vertically over `length` as
/// if moving fast; sharp when `length` is below half a point.
pub fn paint_smeared_galley(
    painter: &Painter,
    pos: Pos2,
    galley: &Arc<Galley>,
    color: Color32,
    length: f32,
) {
    if length < 0.5 {
        painter.galley_with_override_text_color(pos, galley.clone(), color);
        return;
    }
    const TAPS: usize = 5;
    painter.galley_with_override_text_color(pos, galley.clone(), color.gamma_multiply(0.45));
    for k in 0..TAPS {
        let t = k as f32 / (TAPS - 1) as f32 - 0.5;
        painter.galley_with_override_text_color(
            pos + Vec2::new(0.0, t * length),
            galley.clone(),
            color.gamma_multiply(0.18),
        );
    }
}

/// Soft dark halo behind a galley at `pos`: black copies in two rings within `radius`, adding
/// up to roughly `alpha` under the glyphs. Does nothing when `alpha` is zero.
pub fn paint_text_shadow(
    painter: &Painter,
    pos: Pos2,
    galley: &Arc<Galley>,
    alpha: f32,
    radius: f32,
) {
    const TAPS: usize = 8;
    if alpha <= 0.0 {
        return;
    }
    let tap = Color32::BLACK.gamma_multiply((alpha / TAPS as f32).clamp(0.0, 1.0));
    for ring in [0.5, 1.0] {
        for k in 0..TAPS {
            let angle = TAU * (k as f32 + ring) / TAPS as f32;
            let offset = Vec2::angled(angle) * radius * ring;
            painter.galley_with_override_text_color(pos + offset, galley.clone(), tap);
        }
    }
}

/// Darkens the edges of `rect`, fading to nothing `inset` inside each edge.
pub fn paint_vignette(painter: &Painter, rect: Rect, inset: Vec2, color: Color32) {
    let inner = rect.shrink2(inset);
    let mut mesh = Mesh::default();
    for p in [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ] {
        mesh.colored_vertex(p, color);
    }
    for p in [
        inner.left_top(),
        inner.right_top(),
        inner.right_bottom(),
        inner.left_bottom(),
    ] {
        mesh.colored_vertex(p, Color32::TRANSPARENT);
    }
    for i in 0..4u32 {
        let j = (i + 1) % 4;
        mesh.add_triangle(i, j, 4 + i);
        mesh.add_triangle(j, 4 + j, 4 + i);
    }
    painter.add(Shape::mesh(mesh));
}

/// Soft radial gradient of `color` at the centre fading to transparent at `radius`.
pub fn paint_radial_glow(painter: &Painter, center: Pos2, radius: Vec2, color: Color32) {
    const SEGMENTS: u32 = 64;
    const RINGS: u32 = 8;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, color);
    for ring in 1..=RINGS {
        let r = ring as f32 / RINGS as f32;
        // Gaussian-like falloff, exactly zero at the rim.
        let strength = (-(r * r) * 3.0).exp() * (1.0 - r);
        let ring_color = color.gamma_multiply(strength);
        for seg in 0..SEGMENTS {
            let dir = Vec2::angled(TAU * seg as f32 / SEGMENTS as f32);
            mesh.colored_vertex(center + dir * radius * r, ring_color);
        }
    }
    let at = |ring: u32, seg: u32| 1 + (ring - 1) * SEGMENTS + seg % SEGMENTS;
    for seg in 0..SEGMENTS {
        mesh.add_triangle(0, at(1, seg), at(1, seg + 1));
        for ring in 1..RINGS {
            mesh.add_triangle(at(ring, seg), at(ring + 1, seg), at(ring + 1, seg + 1));
            mesh.add_triangle(at(ring, seg), at(ring + 1, seg + 1), at(ring, seg + 1));
        }
    }
    painter.add(Shape::mesh(mesh));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Previous,
    Next,
    Play,
    Pause,
    Shuffle,
    Repeat,
}

/// Paints `icon` centred on `center`, fitting a square of side `size`.
pub fn paint_icon(painter: &Painter, icon: Icon, center: Pos2, size: f32, color: Color32) {
    let half = size / 2.0;
    let p = |x: f32, y: f32| center + Vec2::new(x, y) * half;
    let soft = Stroke::new(size * 0.06, color);
    let tri = |pts: [Pos2; 3]| Shape::convex_polygon(pts.to_vec(), color, soft);
    match icon {
        Icon::Play => {
            painter.add(tri([p(-0.55, -0.8), p(0.85, 0.0), p(-0.55, 0.8)]));
        }
        Icon::Pause => {
            let bar = Vec2::new(0.36, 1.6) * half;
            let r = (bar.x * 0.3).min(255.0) as u8;
            painter.rect_filled(Rect::from_center_size(p(-0.35, 0.0), bar), r, color);
            painter.rect_filled(Rect::from_center_size(p(0.35, 0.0), bar), r, color);
        }
        Icon::Next | Icon::Previous => {
            let s = if icon == Icon::Next { 1.0 } else { -1.0 };
            painter.add(tri([p(-0.95 * s, -0.6), p(0.0, 0.0), p(-0.95 * s, 0.6)]));
            painter.add(tri([p(0.0, -0.6), p(0.95 * s, 0.0), p(0.0, 0.6)]));
        }
        Icon::Shuffle => {
            let stroke = PathStroke::new(size * 0.09, color);
            let y = 0.5;
            painter.line(
                vec![p(-1.0, y), p(-0.4, y), p(0.3, -y), p(0.75, -y)],
                stroke.clone(),
            );
            painter.line(
                vec![p(-1.0, -y), p(-0.4, -y), p(0.3, y), p(0.75, y)],
                stroke,
            );
            for y in [-y, y] {
                painter.add(tri([p(0.65, y - 0.28), p(1.0, y), p(0.65, y + 0.28)]));
            }
        }
        Icon::Repeat => {
            let stroke = PathStroke::new(size * 0.09, color);
            painter.line(
                vec![p(-0.85, 0.15), p(-0.85, -0.45), p(0.6, -0.45)],
                stroke.clone(),
            );
            painter.line(vec![p(0.85, -0.15), p(0.85, 0.45), p(-0.6, 0.45)], stroke);
            painter.add(tri([p(0.55, -0.75), p(0.95, -0.45), p(0.55, -0.15)]));
            painter.add(tri([p(-0.55, 0.15), p(-0.95, 0.45), p(-0.55, 0.75)]));
        }
    }
}

/// Clickable vector icon in `color` with a soft hover halo; `lit` dims the icon when false.
pub fn icon_button(
    ui: &egui::Ui,
    id: Id,
    icon: Icon,
    center: Pos2,
    size: f32,
    lit: bool,
    color: Color32,
) -> Response {
    let hit = Rect::from_center_size(center, Vec2::splat(size * 2.0));
    let response = ui
        .interact(hit, id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    let hover = ui
        .ctx()
        .animate_bool_with_time(id.with("hover"), response.hovered(), 0.15);
    let pressed = response.is_pointer_button_down_on();
    paint_icon_button(ui.painter(), icon, center, size, lit, color, hover, pressed);
    response
}

/// Paints an [`icon_button`] with hover amount `hover` in `0.0..=1.0`.
#[allow(clippy::too_many_arguments)]
pub fn paint_icon_button(
    painter: &Painter,
    icon: Icon,
    center: Pos2,
    size: f32,
    lit: bool,
    color: Color32,
    hover: f32,
    pressed: bool,
) {
    let scale = if pressed { 0.9 } else { 1.0 };
    if hover > 0.0 {
        painter.circle_filled(
            center,
            size * 0.95,
            Color32::from_white_alpha((22.0 * hover) as u8),
        );
    }
    let alpha = if lit {
        lerp(0.88, 1.0, hover)
    } else {
        lerp(0.35, 0.6, hover)
    };
    paint_icon(
        painter,
        icon,
        center,
        size * scale,
        color.gamma_multiply(alpha),
    );
}

pub struct SeekBarOutput {
    /// Fraction to show: the pointer position while scrubbing, otherwise the playback progress.
    pub shown: f32,
    pub scrubbing: bool,
    /// Fraction to seek to, set when a click or drag ends.
    pub seek: Option<f32>,
    /// Hover amount in `0.0..=1.0`, eased.
    pub hover: f32,
}

/// Fraction of `track` at horizontal position `x`, clamped to `0.0..=1.0`.
pub fn fraction_at(track: Rect, x: f32) -> f32 {
    if track.width() <= 0.0 {
        return 0.0;
    }
    ((x - track.left()) / track.width()).clamp(0.0, 1.0)
}

/// Thin rounded progress bar in `ink` that thickens on hover and seeks on click or drag.
pub fn seek_bar(
    ui: &egui::Ui,
    id: Id,
    track: Rect,
    progress: f32,
    enabled: bool,
    ink: &Ink,
) -> SeekBarOutput {
    let pad = Vec2::new(track.height() * 1.5, track.height() * 2.5 + 6.0);
    let bar = seek_input(ui, id, track, pad, progress, enabled);
    paint_seek_bar(ui.painter(), track, bar.shown, bar.hover, ink);
    bar
}

/// Click and drag handling for a seek bar on `track`, sensitive within `pad` around it.
pub fn seek_input(
    ui: &egui::Ui,
    id: Id,
    track: Rect,
    pad: Vec2,
    progress: f32,
    enabled: bool,
) -> SeekBarOutput {
    let sense = if enabled {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let response = ui.interact(track.expand2(pad), id, sense);
    let response = if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    };
    let active = enabled && (response.hovered() || response.dragged());
    let hover = ui
        .ctx()
        .animate_bool_with_time(id.with("hover"), active, 0.15);

    let pointer = response
        .interact_pointer_pos()
        .or_else(|| ui.ctx().pointer_latest_pos());
    let scrubbing = enabled && (response.dragged() || response.is_pointer_button_down_on());
    let released = enabled && (response.clicked() || response.drag_stopped());
    let at_pointer = pointer.map(|p| fraction_at(track, p.x));
    let shown = if scrubbing {
        at_pointer.unwrap_or(progress)
    } else {
        progress
    };

    SeekBarOutput {
        shown,
        scrubbing,
        seek: if released { at_pointer } else { None },
        hover,
    }
}

/// Paints a [`seek_bar`] filled to `shown` with hover amount `hover` in `0.0..=1.0`.
pub fn paint_seek_bar(painter: &Painter, track: Rect, shown: f32, hover: f32, ink: &Ink) {
    let thickness = track.height() * lerp(1.0, 1.5, hover);
    let bar = Rect::from_center_size(track.center(), Vec2::new(track.width(), thickness));
    let radius = (thickness / 2.0).min(255.0) as u8;
    painter.rect_filled(bar, radius, ink.track);
    let filled = Rect::from_min_size(
        bar.min,
        Vec2::new(bar.width() * shown.clamp(0.0, 1.0), thickness),
    );
    painter.rect_filled(
        filled,
        radius,
        ink.text.gamma_multiply(lerp(0.78, 1.0, hover)),
    );
    if hover > 0.0 {
        painter.circle_filled(
            Pos2::new(filled.right(), bar.center().y),
            thickness * 1.3 * hover,
            ink.text,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_endpoints_and_midpoint() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert!((ease(0.5) - 0.5).abs() < 1e-6);
        assert_eq!(ease(-1.0), 0.0);
        assert_eq!(ease(2.0), 1.0);
    }

    #[test]
    fn approach_converges_and_snaps() {
        let mut v = 0.0;
        for _ in 0..200 {
            v = approach(v, 100.0, 1.0 / 60.0, 8.0);
        }
        assert_eq!(v, 100.0);
        let half = approach(0.0, 100.0, 0.1, 8.0);
        assert!(half > 0.0 && half < 100.0);
        assert_eq!(approach(5.0, 5.0, 0.016, 8.0), 5.0);
    }

    #[test]
    fn edge_fade_profile() {
        assert_eq!(edge_fade(50.0, 0.0, 100.0, 20.0), 1.0);
        assert_eq!(edge_fade(0.0, 0.0, 100.0, 20.0), 0.0);
        assert_eq!(edge_fade(100.0, 0.0, 100.0, 20.0), 0.0);
        assert_eq!(edge_fade(-5.0, 0.0, 100.0, 20.0), 0.0);
        let mid = edge_fade(10.0, 0.0, 100.0, 20.0);
        assert!((mid - 0.5).abs() < 1e-6);
        assert_eq!(edge_fade(10.0, 0.0, 100.0, 0.0), 1.0);
    }

    #[test]
    fn fraction_clamps() {
        let track = Rect::from_min_size(Pos2::new(100.0, 0.0), Vec2::new(200.0, 4.0));
        assert_eq!(fraction_at(track, 50.0), 0.0);
        assert_eq!(fraction_at(track, 200.0), 0.5);
        assert_eq!(fraction_at(track, 400.0), 1.0);
        assert_eq!(
            fraction_at(Rect::from_min_size(Pos2::ZERO, Vec2::ZERO), 1.0),
            0.0
        );
    }
}
