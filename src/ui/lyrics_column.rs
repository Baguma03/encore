//! Scrolling, time-synced lyrics column shared by the layouts.

use std::sync::Arc;

use egui::{Align2, Color32, Id, Pos2, Rect, Vec2};
use encore::lyrics::{Lyrics, LyricsState};

use super::scheme::Ink;
use super::widgets::{
    ease, edge_fade, lerp, paint_blurred_galley, paint_text_shadow, smooth_value,
};
use super::{font_bold, font_light, with_alpha, View};

/// Seconds for a lyric line to grow into or out of the active style.
const LINE_EMPHASIS: f32 = 0.35;
/// Exponential rate of the lyrics scroll, per second.
const SCROLL_RATE: f32 = 7.0;
/// Size of inactive lyric lines relative to the active one.
const INACTIVE_SCALE: f32 = 0.9;

/// 1.0 when the lyrics column should be shown, 0.0 when the art should sit centred.
/// While lyrics are loading the previous choice is kept, so songs without lyrics don't bounce.
pub fn column_target(ui: &egui::Ui, id: Id, view: &View) -> f32 {
    let id = id.with("lyrics-shown");
    let shown = match &view.lyrics {
        _ if !view.lyrics_enabled || view.player.current.is_none() => false,
        LyricsState::Found(lyrics) => !lyrics.is_empty(),
        LyricsState::NotFound => false,
        LyricsState::Loading => ui.ctx().data(|d| d.get_temp(id)).unwrap_or(false),
    };
    ui.ctx().data_mut(|d| d.insert_temp(id, shown));
    if shown {
        1.0
    } else {
        0.0
    }
}

/// Opacity of a lyric line `distance` lines after the active one (negative for past lines),
/// before blending in the active emphasis.
pub fn line_alpha(distance: isize) -> f32 {
    match distance {
        0 => 0.55,
        1 => 0.5,
        2 => 0.36,
        3 => 0.26,
        d if d > 3 => (0.26 - 0.04 * (d - 3) as f32).max(0.12),
        -1 => 0.24,
        -2 => 0.17,
        _ => 0.12,
    }
}

/// Blur strength in `0.0..=1.0` for a line `distance` from the active one; the two lines on
/// either side stay sharp.
pub fn line_blur(distance: isize) -> f32 {
    let beyond = if distance < 0 {
        -distance - 2
    } else {
        distance - 2
    };
    (beyond.max(0) as f32 / 3.0).min(1.0)
}

/// Scroll offset that puts line `active` at the anchor, given each line's top `tops`.
/// Before the first line the offset leaves room above line 0 for the intro indicator.
pub fn scroll_target(tops: &[f32], active: Option<usize>, intro_height: f32) -> f32 {
    match active.and_then(|i| tops.get(i)) {
        Some(&top) => top,
        None => -intro_height,
    }
}

pub struct LyricsColumn {
    pub rect: Rect,
    /// Font size of the active line.
    pub base: f32,
    /// Height of the active line, as a fraction from the top of `rect`.
    pub anchor: f32,
    pub alpha: f32,
    /// Opacity of an inactive line by its distance from the active one, e.g. [`line_alpha`].
    pub line_alpha: fn(isize) -> f32,
    /// Opacity of a soft dark halo behind each line, for light backdrops; 0.0 for none.
    pub shadow: f32,
    /// The active line takes `ink.text`, the others `ink.soft`.
    pub ink: Ink,
}

/// Paints the lyrics of `view`, left-aligned in `column.rect`, scrolled to the active line.
pub fn paint(ui: &egui::Ui, id: Id, column: &LyricsColumn, view: &View) {
    let LyricsColumn {
        rect,
        base,
        alpha,
        anchor: anchor_frac,
        line_alpha,
        shadow,
        ink,
    } = *column;
    let halo = base * 0.14;
    let painter = ui.painter_at(rect.expand2(Vec2::new(rect.width() * 0.05, 0.0)));
    let anchor = rect.top() + rect.height() * anchor_frac;

    let lyrics = match &view.lyrics {
        LyricsState::Found(lyrics) => Arc::clone(lyrics),
        LyricsState::Loading => {
            let color = with_alpha(ink.soft, 0.45 * alpha);
            painter.text(
                Pos2::new(rect.left(), anchor),
                Align2::LEFT_TOP,
                "Searching lyrics…",
                font_light(base * 0.6),
                color,
            );
            return;
        }
        LyricsState::NotFound => return,
    };
    let fade_band = rect.height() * 0.16;
    let fade =
        |top: f32, height: f32| edge_fade(top + height / 2.0, rect.top(), rect.bottom(), fade_band);

    match lyrics.as_ref() {
        Lyrics::Synced(lines) => {
            let ctx = ui.ctx();
            let active = lyrics.current_line(view.player.elapsed());
            let gap = base * 0.6;
            let mut tops = Vec::with_capacity(lines.len());
            let mut laid_out = Vec::with_capacity(lines.len());
            let mut y = 0.0;
            for (i, line) in lines.iter().enumerate() {
                let target = if active == Some(i) { 1.0 } else { 0.0 };
                let emphasis = ease(ctx.animate_value_with_time(id.with(i), target, LINE_EMPHASIS));
                let scale = lerp(INACTIVE_SCALE, 1.0, emphasis);
                let text = match line.text.trim() {
                    "" => "• • •".to_owned(),
                    t => t.to_owned(),
                };
                let galley = painter.layout(
                    text,
                    font_bold(base * scale),
                    Color32::WHITE,
                    rect.width() * scale,
                );
                tops.push(y);
                y += galley.size().y + gap;
                laid_out.push((galley, emphasis));
            }

            let intro = painter.layout_no_wrap("• • •".into(), font_bold(base), Color32::WHITE);
            let intro_h = intro.size().y + gap;
            let offset = smooth_value(
                ctx,
                id.with("scroll"),
                scroll_target(&tops, active, intro_h),
                SCROLL_RATE,
            );

            let intro_t =
                ctx.animate_bool_with_time(id.with("intro"), active.is_none(), LINE_EMPHASIS);
            if intro_t > 0.0 {
                let top = anchor - intro_h - offset;
                let pulse = 0.75 + 0.25 * (ctx.input(|i| i.time) as f32 * 2.5).sin();
                let a = alpha * intro_t * pulse * fade(top, intro.size().y);
                painter.galley_with_override_text_color(
                    Pos2::new(rect.left(), top),
                    intro,
                    with_alpha(ink.text, a),
                );
            }

            let pivot = active.map_or(-1, |i| i as isize);
            for (i, ((galley, emphasis), top)) in laid_out.into_iter().zip(tops).enumerate() {
                let top = anchor + top - offset;
                let height = galley.size().y;
                if top + height < rect.top() || top > rect.bottom() {
                    continue;
                }
                let distance = i as isize - pivot;
                let a = lerp(line_alpha(distance), 1.0, emphasis) * fade(top, height) * alpha;
                let blur = line_blur(distance) * (1.0 - emphasis) * base * 0.09;
                let pos = Pos2::new(rect.left(), top);
                paint_text_shadow(&painter, pos, &galley, shadow * a, halo);
                let color = ink.soft.lerp_to_gamma(ink.text, emphasis);
                paint_blurred_galley(&painter, pos, &galley, with_alpha(color, a), blur);
            }
        }
        Lyrics::Plain(lines) => {
            let gap = base * 0.5;
            let font = font_bold(base * 0.8);
            let galleys: Vec<_> = lines
                .iter()
                .map(|l| {
                    painter.layout(
                        l.trim().to_owned(),
                        font.clone(),
                        Color32::WHITE,
                        rect.width(),
                    )
                })
                .collect();
            let total: f32 = galleys.iter().map(|g| g.size().y + gap).sum();
            let target =
                view.player.progress() * (total - rect.height() * (1.0 - anchor_frac)).max(0.0);
            let offset = smooth_value(ui.ctx(), id.with("scroll"), target, SCROLL_RATE / 2.0);
            let mut top = anchor - offset;
            for galley in galleys {
                let height = galley.size().y;
                if top + height >= rect.top() && top <= rect.bottom() {
                    let a = 0.6 * fade(top, height) * alpha;
                    let pos = Pos2::new(rect.left(), top);
                    paint_text_shadow(&painter, pos, &galley, shadow * a, halo);
                    painter.galley_with_override_text_color(pos, galley, with_alpha(ink.soft, a));
                }
                top += height + gap;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upcoming_lines_are_brighter_than_past_ones() {
        assert!(line_alpha(1) > line_alpha(-1));
        assert!(line_alpha(2) > line_alpha(-2));
        assert!(line_alpha(1) > line_alpha(2));
        assert!(line_alpha(2) > line_alpha(3));
        assert!(line_alpha(3) >= line_alpha(10));
        assert!(line_alpha(100) > 0.0);
        assert!(line_alpha(-1) > line_alpha(-5));
    }

    #[test]
    fn only_distant_lines_blur() {
        for d in -2..=2 {
            assert_eq!(line_blur(d), 0.0, "distance {d}");
        }
        assert!(line_blur(3) > 0.0);
        assert!(line_blur(-3) > 0.0);
        assert!(line_blur(4) > line_blur(3));
        assert_eq!(line_blur(50), 1.0);
        assert_eq!(line_blur(-50), 1.0);
    }

    #[test]
    fn scroll_target_follows_active_line() {
        let tops = [0.0, 40.0, 100.0];
        assert_eq!(scroll_target(&tops, Some(0), 30.0), 0.0);
        assert_eq!(scroll_target(&tops, Some(2), 30.0), 100.0);
        assert_eq!(scroll_target(&tops, None, 30.0), -30.0);
        assert_eq!(scroll_target(&tops, Some(9), 30.0), -30.0);
    }
}
