//! Material 3 wavy progress bar, after Quickshell's `StyledSlider` in its wavy configuration:
//! a sine wave over the elapsed part, a vertical bar handle with a gap on each side, the flat
//! remaining track and a stop dot at its end.

use std::f32::consts::TAU;

use egui::epaint::PathStroke;
use egui::{Color32, Painter, Pos2, Rect, Vec2};

/// Wavelength relative to the line width, so long bars carry more waves rather than wider ones.
const WAVELENGTH: f32 = 10.0;
/// Wave amplitude as a fraction of the line width.
const AMPLITUDE: f32 = 0.5;
/// Radians the wave moves per second.
const PHASE_SPEED: f32 = 2.5;
/// Handle width and the gap on each side of it, relative to the line width.
const HANDLE_WIDTH: f32 = 0.75;
const HANDLE_PRESSED_WIDTH: f32 = 0.375;
const HANDLE_GAP: f32 = 1.0;
const STOP_DOT: f32 = 0.75;
/// Horizontal step of the wave polyline, in points.
const STEP: f32 = 1.5;

#[derive(Clone, Copy, Debug)]
pub struct BarColors {
    pub highlight: Color32,
    pub track: Color32,
    pub handle: Color32,
    pub dot: Color32,
}

#[derive(Clone, Copy, Debug)]
pub struct Wavy {
    /// Thickness of the wave and the track.
    pub line: f32,
    /// Handle height relative to the line width.
    pub handle_height: f32,
    /// Wave height from 0 (flat) to 1, eased towards 0 while paused.
    pub wave: f32,
    /// Seconds of animation time; moves the wave.
    pub time: f64,
    pub pressed: bool,
}

impl Wavy {
    /// Space kept at each end of `width` for the handle; the playable range lies inside it.
    pub fn inset(&self) -> f32 {
        self.line * HANDLE_GAP
    }

    /// The part of `rect` the progress maps onto.
    pub fn range(&self, rect: Rect) -> Rect {
        Rect::from_x_y_ranges(
            rect.left() + self.inset()..=rect.right() - self.inset(),
            rect.y_range(),
        )
    }
}

/// Vertical offset of a wave of `wavelength` at `x` points from the bar's left end.
pub fn wave_offset(x: f32, wavelength: f32, amplitude: f32, phase: f32) -> f32 {
    if wavelength <= 0.0 {
        return 0.0;
    }
    amplitude * (TAU * x / wavelength + phase).sin()
}

/// Paints the bar across `rect`, centred on its middle, filled to `fraction`.
pub fn paint(painter: &Painter, rect: Rect, fraction: f32, wavy: &Wavy, colors: &BarColors) {
    let lw = wavy.line;
    let cy = rect.center().y;
    let range = wavy.range(rect);
    let hx = range.left() + fraction.clamp(0.0, 1.0) * range.width();
    let handle_w = lw
        * if wavy.pressed {
            HANDLE_PRESSED_WIDTH
        } else {
            HANDLE_WIDTH
        };
    let gap = lw * HANDLE_GAP;

    // Elapsed part: the wave, with round caps.
    let wave_end = hx - gap - handle_w / 2.0;
    if wave_end - rect.left() >= lw {
        let amplitude = lw * AMPLITUDE * wavy.wave.clamp(0.0, 1.0);
        let phase = ((wavy.time * f64::from(PHASE_SPEED)) % (TAU as f64 * 1000.0)) as f32;
        let (x0, x1) = (rect.left() + lw / 2.0, wave_end - lw / 2.0);
        let at = |x: f32| {
            Pos2::new(
                x,
                cy + wave_offset(x - rect.left(), lw * WAVELENGTH, amplitude, phase),
            )
        };
        let steps = ((x1 - x0) / STEP).ceil().max(1.0) as usize;
        let points: Vec<Pos2> = (0..=steps)
            .map(|i| at(x0 + (x1 - x0) * i as f32 / steps as f32))
            .collect();
        let (first, last) = (points[0], points[points.len() - 1]);
        painter.line(points, PathStroke::new(lw, colors.highlight));
        painter.circle_filled(first, lw / 2.0, colors.highlight);
        painter.circle_filled(last, lw / 2.0, colors.highlight);
    } else if wave_end > rect.left() {
        let dot = Rect::from_min_max(
            Pos2::new(rect.left(), cy - lw / 2.0),
            Pos2::new(wave_end, cy + lw / 2.0),
        );
        painter.rect_filled(dot, lw / 2.0, colors.highlight);
    }

    // Remaining part: the flat track.
    let track_start = hx + handle_w / 2.0 + gap;
    if rect.right() - track_start > 0.0 {
        let track = Rect::from_min_max(
            Pos2::new(track_start, cy - lw / 2.0),
            Pos2::new(rect.right(), cy + lw / 2.0),
        );
        painter.rect_filled(track, lw / 2.0, colors.track);
        let dot = lw * STOP_DOT;
        if track.width() > dot + gap {
            painter.circle_filled(
                Pos2::new(range.right() - dot / 2.0, cy),
                dot / 2.0,
                colors.dot,
            );
        }
    }

    let handle = Rect::from_center_size(
        Pos2::new(hx, cy),
        Vec2::new(handle_w, lw * wavy.handle_height),
    );
    painter.rect_filled(handle, handle_w / 2.0, colors.handle);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wave_follows_its_wavelength() {
        let w = 100.0;
        assert!(wave_offset(0.0, w, 2.0, 0.0).abs() < 1e-5);
        // A quarter wavelength in is a crest.
        assert!((wave_offset(w / 4.0, w, 2.0, 0.0) - 2.0).abs() < 1e-4);
        assert!((wave_offset(w, w, 2.0, 0.0)).abs() < 1e-3);
        assert_eq!(wave_offset(10.0, w, 0.0, 1.0), 0.0);
        assert_eq!(wave_offset(10.0, 0.0, 2.0, 1.0), 0.0);
    }

    #[test]
    fn progress_range_leaves_room_for_the_handle() {
        let wavy = Wavy {
            line: 4.0,
            handle_height: 6.0,
            wave: 1.0,
            time: 0.0,
            pressed: false,
        };
        let rect = Rect::from_min_size(Pos2::new(10.0, 0.0), Vec2::new(100.0, 24.0));
        let range = wavy.range(rect);
        assert_eq!(range.left(), 14.0);
        assert_eq!(range.right(), 106.0);
    }
}
