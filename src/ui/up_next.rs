//! The Up next card shared by the layouts: for the end of a song, the queued song's cover with
//! its title and artist beside it and a countdown bar below slides into the top-right corner,
//! drawn straight over the backdrop.

use std::sync::Arc;

use egui::epaint::RectShape;
use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, FontId, Galley, Id, Painter, Pos2, Rect, Vec2};

use super::classic::square_uv;
use super::scheme::Ink;
use super::widgets::{ease, lerp, paint_text_shadow};
use super::{font_bold, with_alpha, View};
use crate::player::SongInfo;

/// Song progress from which the card shows.
const UP_NEXT_AT: f32 = 0.9;
/// Seconds for the card to slide in or out.
const UP_NEXT_SLIDE: f32 = 0.5;

/// Whether the Up next card is due: the last stretch of a song with another one queued.
pub fn up_next_due(progress: f32, has_next: bool) -> bool {
    has_next && progress >= UP_NEXT_AT
}

/// Share of the Up next stretch still to play, for the card's countdown bar.
pub fn countdown_fraction(progress: f32) -> f32 {
    ((1.0 - progress) / (1.0 - UP_NEXT_AT)).clamp(0.0, 1.0)
}

/// Horizontal offset of the card at slide amount `t` (0 hidden, 1 in place), pushing it
/// `distance` to the right when hidden.
pub fn slide_offset(t: f32, distance: f32) -> f32 {
    (1.0 - ease(t)) * distance
}

/// Colours of the card, from the layout's palette.
pub struct Colors {
    /// The title; at low opacity, the placeholder for a cover that hasn't loaded.
    pub title: Color32,
    pub artist: Color32,
    /// Unfilled part of the countdown bar.
    pub track: Color32,
    /// Part of the countdown bar still to play.
    pub fill: Color32,
    /// Opacity of a soft dark halo behind the text, for light backdrops; 0.0 for none.
    pub halo: f32,
}

impl Colors {
    /// Colours from a layout's `ink`, the artist at opacity `artist_alpha` like the layout's
    /// secondary text.
    pub fn from_ink(ink: &Ink, artist_alpha: f32, halo: f32) -> Self {
        Self {
            title: ink.text,
            artist: with_alpha(ink.soft, artist_alpha),
            track: ink.track,
            fill: ink.text,
            halo,
        }
    }
}

/// The card for one frame: its sizes, its place in the top-right corner of the screen and how
/// far it has slid in.
pub struct UpNext {
    /// Top-right corner of the card in place.
    corner: Pos2,
    width: f32,
    pad: f32,
    thumb: f32,
    /// Size unit: the window's shorter side.
    unit: f32,
    /// Slide amount: 0 hidden, 1 in place.
    t: f32,
    /// The song on the card, kept while it slides out after the song changes.
    song: Option<SongInfo>,
    countdown: f32,
}

impl UpNext {
    /// Places the card in `screen` and advances its slide towards whether `view` has it due.
    pub fn new(ui: &egui::Ui, screen: Rect, view: &View) -> Self {
        let (w, h) = (screen.width(), screen.height());
        let unit = w.min(h);
        let player = view.player;
        let progress = player.progress();
        let due = up_next_due(progress, player.next.is_some());
        let ctx = ui.ctx();
        let id = Id::unique("up-next");
        if due {
            ctx.data_mut(|d| d.insert_temp(id, player.next.clone()));
        }
        let t = ctx.animate_bool_with_time(id.with("shown"), due, UP_NEXT_SLIDE);
        let song = if t > 0.0 {
            ctx.data(|d| d.get_temp::<Option<SongInfo>>(id)).flatten()
        } else {
            None
        };
        Self {
            corner: Pos2::new(screen.right() - w / 30.0, screen.top() + h / 20.0),
            width: (unit * 0.36).min(w * 0.24),
            pad: unit * 0.02,
            thumb: unit * 0.075,
            unit,
            t: if song.is_some() { t } else { 0.0 },
            song,
            countdown: if due {
                countdown_fraction(progress)
            } else {
                0.0
            },
        }
    }

    fn bar_height(&self) -> f32 {
        (self.unit * 0.004).max(2.0)
    }

    /// The card's area once it has slid in.
    pub fn rect(&self) -> Rect {
        let height = self.thumb + self.pad * 0.7 + self.bar_height();
        Rect::from_min_max(
            Pos2::new(self.corner.x - self.width, self.corner.y),
            Pos2::new(self.corner.x, self.corner.y + height),
        )
    }

    /// How far the card shows, eased: 0 hidden, 1 in place.
    pub fn shown(&self) -> f32 {
        ease(self.t)
    }

    /// A top edge for content under the card: `top` while it is hidden, moving to `gap`
    /// below the card as it shows.
    pub fn clear_below(&self, top: f32, gap: f32) -> f32 {
        lerp(top, (self.rect().bottom() + gap).max(top), self.shown())
    }

    /// Paints the card sliding in from the right in `colors`.
    pub fn paint(&self, painter: &Painter, view: &View, colors: &Colors) {
        let Some(song) = self.song.as_ref().filter(|_| self.t > 0.0) else {
            return;
        };
        let Self {
            corner,
            width,
            pad,
            thumb,
            unit,
            ..
        } = *self;
        let bar_h = self.bar_height();
        let offset = slide_offset(self.t, width + (painter.clip_rect().right() - corner.x));
        let left = corner.x - width + offset;

        let mut painter = painter.clone();
        painter.multiply_opacity(ease(self.t));

        let thumb_rect = Rect::from_min_size(Pos2::new(left, corner.y), Vec2::splat(thumb));
        let cover = view
            .next_cover
            .filter(|c| c.song_file == song.file)
            .map(|c| &c.texture)
            .or_else(|| {
                view.art
                    .current
                    .filter(|a| a.song_file == song.file)
                    .map(|a| &a.cover)
            });
        let thumb_radius = (thumb * 0.1) as u8;
        match cover {
            Some(tex) => {
                painter.add(
                    RectShape::filled(thumb_rect, thumb_radius, Color32::WHITE)
                        .with_texture(tex.id(), square_uv(tex.size_vec2())),
                );
            }
            None => {
                painter.rect_filled(thumb_rect, thumb_radius, with_alpha(colors.title, 0.12));
            }
        }

        let text_x = thumb_rect.right() + pad * 0.8;
        let text_w = left + width - text_x;
        let title = one_row(&painter, &song.title, font_bold(unit * 0.028), text_w);
        let artist = one_row(&painter, &song.artist, font_bold(unit * 0.021), text_w);
        let gap = unit * 0.005;
        let text_h = title.size().y + gap + artist.size().y;
        let text_top = thumb_rect.center().y - text_h / 2.0;
        let title_pos = Pos2::new(text_x, text_top);
        let artist_pos = Pos2::new(text_x, text_top + title.size().y + gap);
        let halo = unit * 0.004;
        paint_text_shadow(&painter, title_pos, &title, colors.halo, halo);
        paint_text_shadow(&painter, artist_pos, &artist, colors.halo, halo);
        painter.galley_with_override_text_color(title_pos, title, colors.title);
        painter.galley_with_override_text_color(artist_pos, artist, colors.artist);

        let track = Rect::from_min_size(
            Pos2::new(left, thumb_rect.bottom() + pad * 0.7),
            Vec2::new(width, bar_h),
        );
        let bar_radius = (bar_h / 2.0) as u8;
        painter.rect_filled(track, bar_radius, colors.track);
        let fill = Rect::from_min_size(track.min, Vec2::new(track.width() * self.countdown, bar_h));
        painter.rect_filled(fill, bar_radius, colors.fill);
    }
}

/// Lays out `text` on one row, ellipsized to `width`.
fn one_row(painter: &Painter, text: &str, font: FontId, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, Color32::WHITE);
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn up_next_timing() {
        assert!(!up_next_due(0.89, true));
        assert!(up_next_due(0.9, true));
        assert!(!up_next_due(0.95, false));
        assert_eq!(countdown_fraction(0.9), 1.0);
        assert!((countdown_fraction(0.95) - 0.5).abs() < 1e-4);
        assert_eq!(countdown_fraction(1.0), 0.0);
        assert_eq!(countdown_fraction(0.2), 1.0);
        assert_eq!(slide_offset(0.0, 100.0), 100.0);
        assert_eq!(slide_offset(1.0, 100.0), 0.0);
        assert!((slide_offset(0.5, 100.0) - 50.0).abs() < 1e-4);
    }
}
