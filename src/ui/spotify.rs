//! Layout modelled on the Spotify desktop client's fullscreen mode.

use std::sync::Arc;
use std::time::Duration;

use egui::epaint::{RectShape, Shadow};
use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align, Align2, Color32, FontId, Galley, Id, Painter, Pos2, Rect, Sense, Stroke, StrokeKind,
    Vec2,
};
use mpd_display_rs::lyrics::{Lyrics, LyricsState};

use super::apple::{self, square_uv};
use super::backdrop;
use super::widgets::{ease, lerp, paint_text_shadow};
use super::{cover_rect, fit_text, fmt_time, font_bold, font_light, with_alpha, View, FULL_UV};
use crate::art::ArtTextures;
use crate::player::{Command, SongInfo};

const TEXT_BACKGROUND: Color32 = Color32::from_rgb(156, 156, 156);
const TEXT_FOREGROUND: Color32 = Color32::from_rgb(255, 255, 255);
const TEXT_MIDGROUND: Color32 = Color32::from_rgb(195, 195, 195);
const BACKGROUND_IMAGE_TINT: Color32 = Color32::from_rgb(75, 75, 75);
const BACKGROUND: Color32 = Color32::from_rgb(50, 50, 50);
const ACCENT: Color32 = Color32::from_rgb(29, 185, 84);

/// Shown in place of an empty lyric line and before the first one.
const GAP_TEXT: &str = "• • •";
/// Seconds for the lyrics to scroll by one line.
const LYRICS_SCROLL: f64 = 0.6;
/// Opacity of the line above and below the active one.
const PREVIOUS_ALPHA: f32 = 0.45;
const NEXT_ALPHA: f32 = 0.55;
/// Size of the lines around the active one relative to it.
const SIDE_SCALE: f32 = 0.8;

/// Song progress from which the Up next card shows.
const UP_NEXT_AT: f32 = 0.9;
/// Seconds for the Up next card to slide in or out.
const UP_NEXT_SLIDE: f32 = 0.5;

/// Progress bar geometry, shared by drawing and hit-testing.
struct Bar {
    rect: Rect,
    hover_band: Rect,
}

impl Bar {
    fn new(screen: Rect) -> Self {
        let (w, h) = (screen.width(), screen.height());
        let width = w * 0.85;
        let height = h * 0.006;
        let rect = Rect::from_min_size(
            screen.min + Vec2::new((w - width) * 0.5, h * 0.9),
            Vec2::new(width, height),
        );
        let margin = h * 0.02;
        let hover_band = Rect::from_x_y_ranges(
            screen.x_range(),
            rect.center().y - margin..=rect.center().y + margin,
        );
        Self { rect, hover_band }
    }

    fn fraction_at(&self, pos: Pos2) -> Option<f32> {
        (self.hover_band.contains(pos) && self.rect.x_range().contains(pos.x))
            .then(|| (pos.x - self.rect.left()) / self.rect.width())
    }
}

/// Draws the layout and returns the command a click asked for, if any.
pub fn draw(ui: &mut egui::Ui, view: &View) -> Option<Command> {
    let screen = ui.max_rect();
    let painter = ui.painter_at(screen);
    let bar = Bar::new(screen);
    if view.capture {
        paint(ui, &painter, screen, &bar, false, view);
        return None;
    }
    let response = ui.interact(screen, Id::unique("spotify-layout"), Sense::click());
    let hover = response
        .hover_pos()
        .is_some_and(|p| bar.hover_band.contains(p));

    paint(ui, &painter, screen, &bar, hover, view);

    let pos = response
        .interact_pointer_pos()
        .filter(|_| response.clicked())?;
    let duration = view.player.current.as_ref().and_then(|s| s.duration);
    match (bar.fraction_at(pos), duration) {
        (Some(fraction), Some(d)) => Some(Command::SeekTo(d.mul_f32(fraction.clamp(0.0, 1.0)))),
        _ => Some(Command::TogglePause),
    }
}

fn paint(ui: &egui::Ui, painter: &Painter, screen: Rect, bar: &Bar, hover: bool, view: &View) {
    let (w, h) = (screen.width(), screen.height());
    let origin = screen.min;
    let player = view.player;
    let alpha = view.art.fade;

    painter.rect_filled(screen, 0.0, BACKGROUND);

    // Artist image, or the blurred album art when there is none or it is turned off.
    if backdrop::uses_artist_image(view) {
        let pan = view.motion.then(|| ui.input(|i| i.time));
        backdrop::paint_artist_image(painter, screen, &view.art, BACKGROUND_IMAGE_TINT, pan);
    } else {
        paint_art_backdrop(ui, painter, screen, view);
    }

    // Album art
    let art_size = w.min(h) / 3.0;
    let art_rect = Rect::from_min_size(
        origin + Vec2::new(w / 16.0, h / 6.0 * 5.0 - art_size),
        Vec2::splat(art_size),
    );
    if alpha < 1.0 {
        if let Some(prev) = view.art.previous {
            painter.image(
                prev.cover.id(),
                art_rect,
                FULL_UV,
                with_alpha(Color32::WHITE, 1.0 - alpha),
            );
        }
    }
    if let Some(cur) = view.art.current {
        painter.image(
            cur.cover.id(),
            art_rect,
            FULL_UV,
            with_alpha(Color32::WHITE, alpha),
        );
    }

    // Title and artist
    let (title, artist) = match &player.current {
        Some(song) => (song.title.as_str(), song.artist.as_str()),
        None => ("Nothing playing", ""),
    };
    let title_pos = Pos2::new(
        art_rect.left() + art_size * 1.1,
        art_rect.top() + art_size * 0.4,
    );
    let available = (screen.right() - title_pos.x) * 0.97;
    let title_size = w.min(h) / 9.0;
    let title_text = fit_text(
        painter,
        title,
        font_bold,
        title_size,
        available,
        TEXT_FOREGROUND,
    );
    let artist_pos = title_pos + Vec2::new(0.0, title_text.size().y + art_size * 0.05);
    painter.galley(title_pos, title_text, TEXT_FOREGROUND);
    let artist_text = fit_text(
        painter,
        artist,
        font_bold,
        title_size / 2.0,
        available,
        TEXT_MIDGROUND,
    );
    painter.galley(artist_pos, artist_text, TEXT_MIDGROUND);

    // Progress bar
    let bar_h = bar.rect.height();
    let radius = bar_h / 2.1;
    let progress = player.progress();
    painter.rect_filled(bar.rect, radius, TEXT_BACKGROUND.gamma_multiply(0.5));
    let filled = Rect::from_min_size(bar.rect.min, Vec2::new(bar.rect.width() * progress, bar_h));
    painter.rect_filled(filled, radius, if hover { ACCENT } else { TEXT_FOREGROUND });

    let duration = player
        .current
        .as_ref()
        .and_then(|s| s.duration)
        .unwrap_or_default();
    let elapsed_text = painter.layout_no_wrap(
        fmt_time(player.elapsed()),
        font_light(bar_h * 3.0),
        TEXT_FOREGROUND,
    );
    let text_y = bar.rect.top() - elapsed_text.size().y / 3.1;
    let elapsed_pos = Pos2::new(
        bar.rect.left() - elapsed_text.size().x - bar_h * 1.5,
        text_y,
    );
    painter.galley(elapsed_pos, elapsed_text, TEXT_FOREGROUND);
    let duration_text =
        painter.layout_no_wrap(fmt_time(duration), font_light(bar_h * 3.0), TEXT_FOREGROUND);
    painter.galley(
        Pos2::new(bar.rect.right() + bar_h * 1.5, text_y),
        duration_text,
        TEXT_FOREGROUND,
    );

    if hover {
        painter.circle_filled(
            Pos2::new(filled.right(), bar.rect.center().y),
            bar_h,
            TEXT_FOREGROUND,
        );
    }

    // Up next card in the top-right corner, in the clock's place.
    let corner = Pos2::new(screen.right() - w / 30.0, origin.y + h / 20.0);
    let card = UpNextCard::new(screen);
    let card_t = paint_up_next(ui, painter, &card, corner, view);

    let clock_font = font_bold(h / 22.0);
    if view.show_clock && !view.capture && card_t < 1.0 {
        painter.text(
            corner,
            Align2::RIGHT_TOP,
            view.now.format("%H:%M").to_string(),
            clock_font.clone(),
            with_alpha(TEXT_FOREGROUND, 1.0 - card_t),
        );
    }

    // Lyrics between the clock and the album art, narrow enough to clear the card.
    let clock_h = painter
        .layout_no_wrap("0".into(), clock_font, Color32::WHITE)
        .size()
        .y;
    let top = corner.y + clock_h + h * 0.03;
    let bottom = art_rect.top() - h * 0.04;
    let side = card.width + (screen.right() - corner.x) + w.min(h) * 0.03;
    let width = (w * 0.6).min(w - 2.0 * side);
    if bottom > top && width > 0.0 {
        let rect = Rect::from_center_size(
            Pos2::new(screen.center().x, (top + bottom) / 2.0),
            Vec2::new(width, bottom - top),
        );
        paint_lyrics(ui, painter, rect, (h / 21.5).min(w / 24.0), view);
    }
}

/// Blurred album art, moving when motion is on, darkened to the artist image's level.
fn paint_art_backdrop(ui: &egui::Ui, painter: &Painter, screen: Rect, view: &View) {
    let brightness = view.art.current.map_or(0.5, |a| a.brightness);
    if view.motion {
        let time = ui.input(|i| i.time);
        backdrop::paint_motion(painter, screen, &view.art, time, 1.0);
        let wash = (apple::motion_darkening(brightness) + 0.15).min(0.75);
        painter.rect_filled(screen, 0.0, with_alpha(Color32::BLACK, wash));
    } else {
        let draw = |art: &ArtTextures, a: f32| {
            let tex = &art.backdrop;
            let rect = cover_rect(tex.size_vec2(), screen);
            painter.image(tex.id(), rect, FULL_UV, with_alpha(Color32::WHITE, a));
        };
        if let Some(prev) = view.art.previous.filter(|_| view.art.fade < 1.0) {
            draw(prev, 1.0);
        }
        if let Some(cur) = view.art.current {
            draw(cur, view.art.fade);
        }
        let wash = lerp(0.2, 0.45, brightness);
        painter.rect_filled(screen, 0.0, with_alpha(Color32::BLACK, wash));
    }
}

/// Lyric entries for the three-line view and the index of the active one. Synced lyrics start
/// with a gap marker shown before the first line; plain lyrics advance with `progress`.
pub fn lyric_entries(lyrics: &Lyrics, elapsed: Duration, progress: f32) -> (Vec<String>, usize) {
    match lyrics {
        Lyrics::Synced(lines) => {
            let entries = std::iter::once(GAP_TEXT.to_owned())
                .chain(lines.iter().map(|l| match l.text.trim() {
                    "" => GAP_TEXT.to_owned(),
                    t => t.to_owned(),
                }))
                .collect();
            let active = lyrics.current_line(elapsed).map_or(0, |i| i + 1);
            (entries, active)
        }
        Lyrics::Plain(lines) => {
            let entries: Vec<String> = lines
                .iter()
                .map(|l| l.trim())
                .filter(|l| !l.is_empty())
                .map(str::to_owned)
                .collect();
            let last = entries.len().saturating_sub(1);
            let active = ((progress.clamp(0.0, 1.0) * entries.len() as f32) as usize).min(last);
            (entries, active)
        }
    }
}

/// Opacity of an entry `distance` lines below the scroll position (negative above it): full
/// on the active line, dimmed on its neighbours and gone two lines away.
pub fn trio_alpha(distance: f32) -> f32 {
    let side = if distance < 0.0 {
        PREVIOUS_ALPHA
    } else {
        NEXT_ALPHA
    };
    let d = distance.abs();
    if d <= 1.0 {
        lerp(1.0, side, d)
    } else {
        lerp(side, 0.0, (d - 1.0).min(1.0))
    }
}

/// Size of an entry `distance` lines from the scroll position, relative to the active line.
pub fn trio_scale(distance: f32) -> f32 {
    lerp(1.0, SIDE_SCALE, distance.abs().min(1.0))
}

/// An eased scroll from one entry to another, started at `since` seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scroll {
    pub from: f32,
    pub to: f32,
    pub since: f64,
}

impl Scroll {
    /// Scroll position at time `now`.
    pub fn at(self, now: f64) -> f32 {
        let t = ((now - self.since) / LYRICS_SCROLL) as f32;
        lerp(self.from, self.to, ease(t))
    }

    /// Retargets the scroll to `to` at `now`, continuing from where it is. Jumps of more than
    /// a couple of lines, as after a seek, only animate the last line.
    pub fn retarget(self, to: f32, now: f64) -> Self {
        if to == self.to {
            return self;
        }
        let here = self.at(now);
        let from = if (to - here).abs() > 2.0 {
            to - (to - here).signum()
        } else {
            here
        };
        Self {
            from,
            to,
            since: now,
        }
    }
}

/// Lays out `text` centred on x = 0, wrapped at `width`.
fn centered(painter: &Painter, text: &str, font: FontId, width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple(text.to_owned(), font, Color32::WHITE, width);
    job.halign = Align::Center;
    painter.layout_job(job)
}

/// Three centred lyric lines: the previous one dimmed above the active one and the next one
/// below, scrolling up as the song moves on. `base` is the active line's font size.
fn paint_lyrics(ui: &egui::Ui, painter: &Painter, rect: Rect, base: f32, view: &View) {
    let (Some(song), LyricsState::Found(lyrics)) = (&view.player.current, &view.lyrics) else {
        return;
    };
    if !view.lyrics_enabled {
        return;
    }
    let (entries, active) = lyric_entries(lyrics, view.player.elapsed(), view.player.progress());
    if entries.is_empty() {
        return;
    }
    let synced = matches!(lyrics.as_ref(), Lyrics::Synced(_));

    let ctx = ui.ctx();
    let now = ctx.input(|i| i.time);
    let id = Id::unique("spotify-lyrics").with(&song.file);
    let target = active as f32;
    let scroll = ctx.data(|d| d.get_temp::<Scroll>(id)).map_or(
        Scroll {
            from: target,
            to: target,
            since: now,
        },
        |s| s.retarget(target, now),
    );
    ctx.data_mut(|d| d.insert_temp(id, scroll));
    let position = scroll.at(now);
    if position != target {
        ctx.request_repaint();
    }

    // Entries around the position, stacked from the top with their sizes for this frame.
    let first = (position.floor() as isize - 2).max(0) as usize;
    let last = (position.ceil() as usize + 2).min(entries.len() - 1);
    let gap = base * 0.95;
    let mut y = 0.0;
    let mut laid_out = Vec::with_capacity(last + 1 - first);
    for (i, text) in entries.iter().enumerate().take(last + 1).skip(first) {
        let distance = i as f32 - position;
        let galley = centered(
            painter,
            text,
            font_bold(base * trio_scale(distance)),
            rect.width(),
        );
        let height = galley.size().y;
        laid_out.push((distance, y, galley));
        y += height + gap;
    }
    let center_of = |i: usize| {
        laid_out
            .get(i.saturating_sub(first))
            .map(|(_, top, g)| top + g.size().y / 2.0)
    };
    let below = position.floor().max(0.0) as usize;
    let frac = position - below as f32;
    let here = match (center_of(below), center_of(below + 1)) {
        (Some(a), Some(b)) => lerp(a, b, frac),
        (Some(a), None) => a,
        _ => 0.0,
    };

    let anchor = rect.center().y;
    for (i, (distance, top, galley)) in laid_out.into_iter().enumerate() {
        let mut a = trio_alpha(distance);
        // The marker before the first line only shows while it is the active entry.
        if synced && first + i == 0 && distance < 0.0 {
            a *= (1.0 + distance).max(0.0);
        }
        if a <= 0.0 {
            continue;
        }
        let pos = Pos2::new(rect.center().x, anchor + top - here);
        paint_text_shadow(painter, pos, &galley, a * 0.6, base * 0.12);
        painter.galley_with_override_text_color(pos, galley, with_alpha(Color32::WHITE, a));
    }
}

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

/// Sizes of the Up next card.
struct UpNextCard {
    width: f32,
    pad: f32,
    thumb: f32,
    /// Size unit: the window's shorter side.
    unit: f32,
}

impl UpNextCard {
    fn new(screen: Rect) -> Self {
        let unit = screen.width().min(screen.height());
        Self {
            width: (unit * 0.36).min(screen.width() * 0.24),
            pad: unit * 0.02,
            thumb: unit * 0.07,
            unit,
        }
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

/// Glass card with the queued song, sliding in from the right with its top-right corner at
/// `corner` for the end of the song and out once it changes. Returns how far it is shown.
fn paint_up_next(
    ui: &egui::Ui,
    painter: &Painter,
    card: &UpNextCard,
    corner: Pos2,
    view: &View,
) -> f32 {
    let player = view.player;
    let progress = player.progress();
    let due = up_next_due(progress, player.next.is_some());
    let ctx = ui.ctx();
    let id = Id::unique("spotify-up-next");
    // The song stays on the card while it slides out after the song changes.
    if due {
        ctx.data_mut(|d| d.insert_temp(id, player.next.clone()));
    }
    let t = ctx.animate_bool_with_time(id.with("shown"), due, UP_NEXT_SLIDE);
    if t <= 0.0 {
        return 0.0;
    }
    let Some(song) = ctx.data(|d| d.get_temp::<Option<SongInfo>>(id)).flatten() else {
        return 0.0;
    };
    let countdown = if due {
        countdown_fraction(progress)
    } else {
        0.0
    };

    let UpNextCard {
        width,
        pad,
        thumb,
        unit,
    } = *card;
    let label = painter.layout_job(LayoutJob::single_section(
        "UP NEXT".to_owned(),
        TextFormat {
            font_id: font_bold(unit * 0.016),
            extra_letter_spacing: unit * 0.0025,
            color: Color32::WHITE,
            ..Default::default()
        },
    ));
    let bar_h = (unit * 0.004).max(2.0);
    let height = pad + label.size().y + pad * 0.6 + thumb + pad * 0.9 + bar_h + pad * 0.8;
    let offset = slide_offset(t, width + (painter.clip_rect().right() - corner.x));
    let rect = Rect::from_min_size(
        Pos2::new(corner.x - width + offset, corner.y),
        Vec2::new(width, height),
    );

    let mut painter = painter.clone();
    painter.multiply_opacity(ease(t));
    let radius = (unit * 0.013).min(255.0) as u8;
    let shadow = Shadow {
        offset: [0, (unit * 0.008) as i8],
        blur: (unit * 0.03) as u8,
        spread: 0,
        color: Color32::from_black_alpha(90),
    };
    painter.add(shadow.as_shape(rect, radius));
    painter.rect(
        rect,
        radius,
        Color32::from_black_alpha(89),
        Stroke::new(1.0, Color32::from_white_alpha(20)),
        StrokeKind::Inside,
    );

    let mut y = rect.top() + pad;
    let label_h = label.size().y;
    painter.galley_with_override_text_color(
        Pos2::new(rect.left() + pad, y),
        label,
        with_alpha(Color32::WHITE, 0.55),
    );
    y += label_h + pad * 0.6;

    let thumb_rect = Rect::from_min_size(Pos2::new(rect.left() + pad, y), Vec2::splat(thumb));
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
    let thumb_radius = (thumb * 0.08) as u8;
    match cover {
        Some(tex) => {
            painter.add(
                RectShape::filled(thumb_rect, thumb_radius, Color32::WHITE)
                    .with_texture(tex.id(), square_uv(tex.size_vec2())),
            );
        }
        None => {
            painter.rect_filled(thumb_rect, thumb_radius, Color32::from_white_alpha(18));
        }
    }

    let text_x = thumb_rect.right() + pad * 0.7;
    let text_w = rect.right() - pad - text_x;
    let title = one_row(&painter, &song.title, font_bold(unit * 0.025), text_w);
    let artist = one_row(&painter, &song.artist, font_light(unit * 0.02), text_w);
    let gap = unit * 0.004;
    let text_h = title.size().y + gap + artist.size().y;
    let text_top = thumb_rect.center().y - text_h / 2.0;
    let artist_top = text_top + title.size().y + gap;
    painter.galley_with_override_text_color(Pos2::new(text_x, text_top), title, Color32::WHITE);
    painter.galley_with_override_text_color(
        Pos2::new(text_x, artist_top),
        artist,
        with_alpha(Color32::WHITE, 0.65),
    );
    y = thumb_rect.bottom() + pad * 0.9;

    let track = Rect::from_min_size(
        Pos2::new(rect.left() + pad, y),
        Vec2::new(width - 2.0 * pad, bar_h),
    );
    let bar_radius = (bar_h / 2.0) as u8;
    painter.rect_filled(track, bar_radius, Color32::from_white_alpha(38));
    let fill = Rect::from_min_size(track.min, Vec2::new(track.width() * countdown, bar_h));
    painter.rect_filled(fill, bar_radius, with_alpha(Color32::WHITE, 0.85));
    t
}

#[cfg(test)]
mod tests {
    use super::*;
    use mpd_display_rs::lyrics::LyricLine;

    fn synced(spec: &[(u64, &str)]) -> Lyrics {
        Lyrics::Synced(
            spec.iter()
                .map(|&(secs, text)| LyricLine {
                    time: Duration::from_secs(secs),
                    text: text.to_owned(),
                })
                .collect(),
        )
    }

    #[test]
    fn synced_entries_start_with_a_gap_marker() {
        let lyrics = synced(&[(10, "One"), (20, "  "), (30, "Three")]);
        let (entries, active) = lyric_entries(&lyrics, Duration::from_secs(5), 0.0);
        assert_eq!(entries, [GAP_TEXT, "One", GAP_TEXT, "Three"]);
        assert_eq!(active, 0);
        let at = |s| lyric_entries(&lyrics, Duration::from_secs(s), 0.0).1;
        assert_eq!(at(10), 1);
        assert_eq!(at(25), 2);
        assert_eq!(at(99), 3);
    }

    #[test]
    fn plain_entries_follow_progress() {
        let lyrics = Lyrics::Plain(vec!["a".into(), "".into(), "b".into(), "c".into()]);
        let at = |p| lyric_entries(&lyrics, Duration::ZERO, p);
        assert_eq!(at(0.0).0, ["a", "b", "c"]);
        assert_eq!(at(0.0).1, 0);
        assert_eq!(at(0.5).1, 1);
        assert_eq!(at(1.0).1, 2);
        assert_eq!(
            lyric_entries(&Lyrics::Plain(vec![]), Duration::ZERO, 0.5).1,
            0
        );
    }

    #[test]
    fn three_lines_are_visible() {
        assert_eq!(trio_alpha(0.0), 1.0);
        assert!((trio_alpha(-1.0) - PREVIOUS_ALPHA).abs() < 1e-6);
        assert!((trio_alpha(1.0) - NEXT_ALPHA).abs() < 1e-6);
        assert!(trio_alpha(1.0) > trio_alpha(-1.0));
        assert_eq!(trio_alpha(2.0), 0.0);
        assert_eq!(trio_alpha(-2.5), 0.0);
        assert!(trio_alpha(1.5) > 0.0 && trio_alpha(1.5) < NEXT_ALPHA);
        assert_eq!(trio_scale(0.0), 1.0);
        assert_eq!(trio_scale(-3.0), SIDE_SCALE);
        assert!(trio_scale(0.5) > SIDE_SCALE && trio_scale(0.5) < 1.0);
    }

    #[test]
    fn scroll_eases_to_the_target() {
        let s = Scroll {
            from: 2.0,
            to: 2.0,
            since: 0.0,
        }
        .retarget(3.0, 10.0);
        assert_eq!(s.at(10.0), 2.0);
        assert!((s.at(10.0 + LYRICS_SCROLL / 2.0) - 2.5).abs() < 1e-4);
        assert_eq!(s.at(10.0 + LYRICS_SCROLL), 3.0);
        assert_eq!(s.at(100.0), 3.0);
        // Retargeting midway continues from the current position.
        let mid = s.retarget(4.0, 10.0 + LYRICS_SCROLL / 2.0);
        assert!((mid.from - 2.5).abs() < 1e-4);
        // Long jumps only animate the last line.
        let jump = s.retarget(20.0, 50.0);
        assert_eq!(jump.from, 19.0);
        let back = s.retarget(0.0, 50.0);
        assert_eq!(back.from, 1.0);
        assert_eq!(s.retarget(3.0, 50.0), s);
    }

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
