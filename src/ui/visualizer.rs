//! Audio wave drawn behind content, after Quickshell's `WaveVisualizer`: bar values smoothed
//! over their neighbours and filled as a soft wave rising from the bottom edge.

use egui::epaint::Mesh;
use egui::{Color32, Id, Painter, Pos2, Rect, Shape};

use super::{with_alpha, View};
use crate::player::PlayState;

/// Value at which the wave reaches the top of its area; cava's raw output range.
pub const MAX_VALUE: f32 = 1000.0;
/// Neighbours on each side averaged into each point.
const SMOOTHING: usize = 2;
/// Columns drawn between two neighbouring points.
const SUBDIVISIONS: usize = 4;
/// Seconds for the wave to settle flat after playback stops, or to rise again.
const LIVE_FADE: f32 = 0.5;

/// Moving average of each point with `window` neighbours on each side, clamped at the ends.
pub fn smooth(points: &[f32], window: usize) -> Vec<f32> {
    let n = points.len();
    (0..n)
        .map(|i| {
            let sum: f32 = (-(window as isize)..=window as isize)
                .map(|j| points[(i as isize + j).clamp(0, n as isize - 1) as usize])
                .sum();
            sum / (2 * window + 1) as f32
        })
        .collect()
}

/// How much of the wave shows: 1 while playing, easing to 0 while paused. 0 without bands.
pub fn level(ui: &egui::Ui, id: Id, view: &View) -> f32 {
    if view.visualizer.is_none_or(|b| b.len() < 2) {
        return 0.0;
    }
    let playing = view.player.state == PlayState::Playing;
    ui.ctx()
        .animate_bool_with_time(id.with("visualizer-live"), playing, LIVE_FADE)
}

/// Paints the wave of `points` filling `rect` from its bottom edge, scaled by `level`, in
/// `color` at opacity `alpha`. The bottom corners follow a rounding of `radius`; the top edge
/// fades out over `soft` points.
#[allow(clippy::too_many_arguments)]
pub fn paint(
    painter: &Painter,
    rect: Rect,
    radius: f32,
    points: &[f32],
    level: f32,
    color: Color32,
    alpha: f32,
    soft: f32,
) {
    let n = points.len();
    if n < 2 || level <= 0.0 || rect.width() <= 0.0 {
        return;
    }
    let smoothed = smooth(points, SMOOTHING);
    let h = rect.height();
    // Bottom edge, raised inside the rounded corners.
    let floor = |x: f32| {
        let dx = (x - rect.left()).min(rect.right() - x);
        if radius > 0.0 && dx < radius {
            let d = radius - dx.max(0.0);
            rect.bottom() - radius + (radius * radius - d * d).max(0.0).sqrt()
        } else {
            rect.bottom()
        }
    };
    let fill = with_alpha(color, alpha);
    let mut mesh = Mesh::default();
    let columns = (n - 1) * SUBDIVISIONS;
    for c in 0..=columns {
        let t = c as f32 / SUBDIVISIONS as f32;
        let i = (t.floor() as usize).min(n - 2);
        let value = smoothed[i] + (smoothed[i + 1] - smoothed[i]) * (t - i as f32);
        let x = rect.left() + rect.width() * c as f32 / columns as f32;
        let bottom = floor(x);
        let top = (rect.bottom() - (value / MAX_VALUE).clamp(0.0, 1.0) * h * level).min(bottom);
        mesh.colored_vertex(Pos2::new(x, bottom), fill);
        mesh.colored_vertex(Pos2::new(x, top), fill);
        mesh.colored_vertex(
            Pos2::new(x, (top - soft).max(rect.top())),
            Color32::TRANSPARENT,
        );
        if c > 0 {
            let (p, q) = (3 * (c as u32 - 1), 3 * c as u32);
            for k in 0..2 {
                mesh.add_triangle(p + k, q + k, q + k + 1);
                mesh.add_triangle(p + k, q + k + 1, p + k + 1);
            }
        }
    }
    painter.add(Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothing_averages_neighbours() {
        let s = smooth(&[0.0, 0.0, 10.0, 0.0, 0.0], 1);
        assert_eq!(s, [0.0, 10.0 / 3.0, 10.0 / 3.0, 10.0 / 3.0, 0.0]);
        // Ends repeat the edge value.
        let s = smooth(&[6.0, 0.0], 2);
        assert_eq!(s, [(3.0 * 6.0) / 5.0, (2.0 * 6.0) / 5.0]);
        assert!(smooth(&[], 2).is_empty());
    }
}
