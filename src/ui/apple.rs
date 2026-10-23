//! Layout modelled on Apple Music's fullscreen player, over a blurred, darkened copy of the
//! album art. Wide windows put art and controls on the left and synced lyrics on the right;
//! tall and square windows stack the art above the lyrics.

use std::sync::Arc;
use std::time::Duration;

use egui::epaint::{RectShape, Shadow};
use egui::{Align2, Color32, FontId, Galley, Id, Painter, Pos2, Rect, Sense, Vec2};

use super::backdrop;
use super::lyrics_column::{self, LyricsColumn};
use super::scheme::Ink;
use super::widgets::{
    self, ease, icon_button, lerp, paint_icon_button, paint_seek_bar, seek_bar, Icon,
};
use super::{cover_rect, fit_text, fmt_time, font_bold, font_light, with_alpha, View, FULL_UV};
use crate::art::ArtTextures;
use crate::player::{Command, PlayState, SongInfo};

/// Height of the art-and-controls block as a multiple of the art size.
const BLOCK_HEIGHT: f32 = 1.56;
/// Seconds for the art column to slide between centred and left-aligned.
pub const COLUMN_SLIDE: f32 = 0.7;
/// Height of the lyrics column's active line, as a fraction from its top.
const LYRICS_ANCHOR: f32 = 0.36;
/// Width-to-height ratios at or beyond which the layout is landscape or portrait.
const LANDSCAPE_ASPECT: f32 = 1.2;
const PORTRAIT_ASPECT: f32 = 0.8;

const MUTED: f32 = 0.6;
/// Corner radius of the art as a fraction of its side.
const ART_RADIUS: f32 = 0.03;

/// Black wash over the full-brightness moving art: lighter on dark covers so the movement
/// shows, heavier on light ones so white text stays readable.
pub fn motion_darkening(brightness: f32) -> f32 {
    (0.3 + 0.32 * brightness).clamp(0.4, 0.6)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Landscape,
    Portrait,
    Square,
}

impl Orientation {
    pub fn of(size: Vec2) -> Self {
        let aspect = size.x / size.y.max(1.0);
        if aspect >= LANDSCAPE_ASPECT {
            Self::Landscape
        } else if aspect <= PORTRAIT_ASPECT {
            Self::Portrait
        } else {
            Self::Square
        }
    }
}

/// Title and "Artist — Album" lines.
pub struct Meta {
    title: Arc<Galley>,
    subtitle: Arc<Galley>,
    gap: f32,
}

impl Meta {
    /// Lines of font sizes `title` and `subtitle`, shrunk to fit `width`, in `ink` with the
    /// subtitle at opacity `muted`.
    pub fn new(
        painter: &Painter,
        song: &SongInfo,
        title: f32,
        subtitle: f32,
        width: f32,
        muted: f32,
        ink: &Ink,
    ) -> Self {
        let sub = match (song.artist.is_empty(), song.album.is_empty()) {
            (false, false) => format!("{} — {}", song.artist, song.album),
            (false, true) => song.artist.clone(),
            (true, _) => song.album.clone(),
        };
        Self {
            title: fit_text(painter, &song.title, font_bold, title, width, ink.text),
            subtitle: fit_text(
                painter,
                &sub,
                font_light,
                subtitle,
                width,
                with_alpha(ink.soft, muted),
            ),
            gap: title / 7.5,
        }
    }

    pub fn height(&self) -> f32 {
        self.title.size().y + self.gap + self.subtitle.size().y
    }

    /// Each line with its top-left corner, below `pos` and centred on it or starting at it.
    fn lines(&self, pos: Pos2, centered: bool) -> [(Pos2, &Arc<Galley>); 2] {
        let x = |g: &Galley| {
            if centered {
                pos.x - g.size().x / 2.0
            } else {
                pos.x
            }
        };
        let y = pos.y + self.title.size().y + self.gap;
        [
            (Pos2::new(x(&self.title), pos.y), &self.title),
            (Pos2::new(x(&self.subtitle), y), &self.subtitle),
        ]
    }

    /// Paints the lines below `pos`, centred on it or starting at it.
    pub fn paint(&self, painter: &Painter, pos: Pos2, centered: bool) {
        for (at, galley) in self.lines(pos, centered) {
            painter.galley(at, galley.clone(), Color32::WHITE);
        }
    }

    /// Paints a soft dark halo of opacity `alpha` and size `radius` behind the lines placed as
    /// by [`Meta::paint`].
    pub fn paint_shadow(
        &self,
        painter: &Painter,
        pos: Pos2,
        centered: bool,
        alpha: f32,
        radius: f32,
    ) {
        for (at, galley) in self.lines(pos, centered) {
            widgets::paint_text_shadow(painter, at, galley, alpha, radius);
        }
    }
}

/// Progress bar, times and transport buttons, stacked downwards from `top`.
struct Controls {
    center_x: f32,
    top: f32,
    bar_width: f32,
    /// Distance between the outermost buttons, roughly.
    button_span: f32,
    /// Size unit for the bar thickness, text and icons.
    unit: f32,
}

impl Controls {
    fn time_font(unit: f32) -> egui::FontId {
        font_light(unit * 0.04)
    }

    fn time_height(painter: &Painter, unit: f32) -> f32 {
        painter
            .layout_no_wrap("0:00".into(), Self::time_font(unit), Color32::WHITE)
            .size()
            .y
    }

    fn thickness(unit: f32) -> f32 {
        (unit * 0.02).max(5.0)
    }

    /// Height from the top of the bar to the bottom of the largest button.
    fn height(painter: &Painter, unit: f32) -> f32 {
        Self::thickness(unit) + unit * 0.175 + Self::time_height(painter, unit)
    }
}

/// Where each part of the layout goes for one frame.
struct Placement {
    art: Rect,
    meta: Meta,
    meta_pos: Pos2,
    meta_centered: bool,
    lyrics: Rect,
    /// Font size of the active lyric line.
    lyrics_base: f32,
    lyrics_anchor: f32,
    controls: Controls,
}

/// Draws the layout and returns the command a click asked for, if any.
pub fn draw(ui: &mut egui::Ui, view: &View) -> Option<Command> {
    let screen = ui.max_rect();
    let ctx = ui.ctx().clone();
    let id = Id::unique("apple-layout");
    let background = (!view.capture).then(|| ui.interact(screen, id, Sense::click()));
    let background_click = || {
        background
            .as_ref()
            .is_some_and(|r| r.clicked())
            .then_some(Command::TogglePause)
    };
    let painter = ui.painter_at(screen);
    let unit = screen.width().min(screen.height());
    let time = ctx.input(|i| i.time);
    let ink = view.ink_on_dark();

    let show_clock = view.show_clock && !view.capture;
    let clock_t = ease(ctx.animate_bool_with_time(id.with("clock"), show_clock, 0.4));
    let lyrics_t = ease(ctx.animate_value_with_time(
        id.with("lyrics"),
        lyrics_column::column_target(ui, id, view),
        COLUMN_SLIDE,
    ));

    let Some(song) = view.player.current.as_ref() else {
        paint_backdrop(&painter, screen, view, time, None);
        let text = if view.player.connected {
            "Nothing playing"
        } else {
            "Connecting to MPD…"
        };
        painter.text(
            screen.center(),
            Align2::CENTER_CENTER,
            text,
            font_bold(unit / 22.0),
            with_alpha(Color32::WHITE, 0.8),
        );
        if clock_t > 0.0 {
            paint_clock(&painter, screen, view, clock_t, &ink);
        }
        return background_click();
    };

    let place = match Orientation::of(screen.size()) {
        Orientation::Landscape => landscape(&painter, screen, song, clock_t, lyrics_t, &ink),
        Orientation::Portrait => portrait(&painter, screen, song, clock_t, lyrics_t, &ink),
        Orientation::Square => square(&painter, screen, song, clock_t, lyrics_t, &ink),
    };

    paint_backdrop(
        &painter,
        screen,
        view,
        time,
        Some((place.art.center(), place.art.width())),
    );
    if clock_t > 0.0 {
        paint_clock(&painter, screen, view, clock_t, &ink);
    }
    if lyrics_t > 0.01 {
        if !view.capture {
            // Clicks on the lyrics are swallowed rather than toggling playback.
            ui.interact(place.lyrics, id.with("lyrics-area"), Sense::click());
        }
        let column = LyricsColumn {
            rect: place.lyrics,
            base: place.lyrics_base,
            anchor: place.lyrics_anchor,
            alpha: lyrics_t,
            line_alpha: lyrics_column::line_alpha,
            shadow: 0.0,
            ink,
        };
        lyrics_column::paint(ui, id.with(&song.file), &column, view);
    }
    paint_art(&painter, place.art, view, ART_RADIUS);
    place
        .meta
        .paint(&painter, place.meta_pos, place.meta_centered);

    paint_controls(ui, &painter, id, view, song, &place.controls, &ink).or_else(background_click)
}

/// Art, title and controls in a column on the left, lyrics on the right. Without lyrics the
/// column slides to the centre.
fn landscape(
    painter: &Painter,
    screen: Rect,
    song: &SongInfo,
    clock_t: f32,
    lyrics_t: f32,
    ink: &Ink,
) -> Placement {
    let (w, h) = (screen.width(), screen.height());
    let content = Rect::from_min_max(
        Pos2::new(screen.left(), screen.top() + h * lerp(0.07, 0.22, clock_t)),
        Pos2::new(screen.right(), screen.bottom() - h * 0.07),
    );
    let s = lerp((h * 0.48).min(w * 0.4), (h * 0.44).min(w * 0.3), lyrics_t)
        .min(content.height() / BLOCK_HEIGHT);
    let column_x = lerp(screen.center().x, screen.left() + w * 0.27, lyrics_t);

    let meta = Meta::new(painter, song, s * 0.075, s * 0.055, s, MUTED, ink);
    let block_h = s * 1.33 + meta.height() + Controls::time_height(painter, s);
    let top = content.center().y - block_h / 2.0;
    let art = Rect::from_min_size(Pos2::new(column_x - s / 2.0, top), Vec2::splat(s));
    let meta_pos = Pos2::new(column_x, art.bottom() + s * 0.075);
    let lyrics = Rect::from_min_max(
        Pos2::new(
            screen.left() + w * lerp(0.58, 0.52, lyrics_t),
            content.top(),
        ),
        Pos2::new(screen.right() - w * 0.06, content.bottom()),
    );

    Placement {
        art,
        lyrics_base: (h / 22.0).min(lyrics.width() / 14.0),
        lyrics_anchor: LYRICS_ANCHOR,
        lyrics,
        controls: Controls {
            center_x: column_x,
            top: meta_pos.y + meta.height() + s * 0.065,
            bar_width: s,
            button_span: s,
            unit: s,
        },
        meta,
        meta_pos,
        meta_centered: true,
    }
}

/// Art centred near the top with the title under it, lyrics below and small controls at the
/// bottom. Without lyrics the art and title sit centred.
fn portrait(
    painter: &Painter,
    screen: Rect,
    song: &SongInfo,
    clock_t: f32,
    lyrics_t: f32,
    ink: &Ink,
) -> Placement {
    let (w, h) = (screen.width(), screen.height());
    let u = w.min(h);
    let pad = w * 0.08;
    let top = screen.top() + lerp(h * 0.07, u * 0.24, clock_t);

    let control_unit = u * 0.5;
    let controls_top = screen.bottom() - h * 0.045 - Controls::height(painter, control_unit);

    let s = lerp((w * 0.78).min(h * 0.42), (w * 0.62).min(h * 0.36), lyrics_t);
    let meta = Meta::new(painter, song, u * 0.05, u * 0.036, s, MUTED, ink);
    let meta_gap = u * 0.045;
    let block_h = s + meta_gap + meta.height();
    let centered = (top + controls_top - u * 0.04 - block_h) / 2.0;
    let art_top = lerp(centered.max(top), top, lyrics_t);
    let art = Rect::from_min_size(
        Pos2::new(screen.center().x - s / 2.0, art_top),
        Vec2::splat(s),
    );
    let meta_pos = Pos2::new(art.left(), art.bottom() + meta_gap);
    let lyrics = Rect::from_min_max(
        Pos2::new(screen.left() + pad, meta_pos.y + meta.height() + u * 0.06),
        Pos2::new(screen.right() - pad, controls_top - u * 0.05),
    );

    Placement {
        art,
        meta,
        meta_pos,
        meta_centered: false,
        lyrics,
        lyrics_base: u / 19.0,
        lyrics_anchor: 0.38,
        controls: Controls {
            center_x: screen.center().x,
            top: controls_top,
            bar_width: w - 2.0 * pad,
            button_span: (w - 2.0 * pad) * 0.8,
            unit: control_unit,
        },
    }
}

/// Art at the top left with the title beside it, lyrics below and compact controls at the
/// bottom. Without lyrics the art and title sit centred vertically.
fn square(
    painter: &Painter,
    screen: Rect,
    song: &SongInfo,
    clock_t: f32,
    lyrics_t: f32,
    ink: &Ink,
) -> Placement {
    let (w, h) = (screen.width(), screen.height());
    let u = w.min(h);
    let pad = w * 0.08;
    let top = screen.top() + lerp(u * 0.07, u * 0.23, clock_t);

    let control_unit = u * 0.4;
    let controls_top = screen.bottom() - u * 0.04 - Controls::height(painter, control_unit);

    let s = lerp(u * 0.4, u * 0.28, lyrics_t);
    let meta_gap = u * 0.045;
    let meta = Meta::new(
        painter,
        song,
        u * 0.05,
        u * 0.034,
        w - 2.0 * pad - s - meta_gap,
        MUTED,
        ink,
    );
    let centered = (top + controls_top - u * 0.04 - s) / 2.0;
    let art_top = lerp(centered.max(top), top, lyrics_t);
    let art = Rect::from_min_size(Pos2::new(screen.left() + pad, art_top), Vec2::splat(s));
    let meta_pos = Pos2::new(art.right() + meta_gap, art.center().y - meta.height() / 2.0);
    let lyrics = Rect::from_min_max(
        Pos2::new(screen.left() + pad, art.bottom() + u * 0.05),
        Pos2::new(screen.right() - pad, controls_top - u * 0.04),
    );

    Placement {
        art,
        meta,
        meta_pos,
        meta_centered: false,
        lyrics,
        lyrics_base: u / 20.0,
        lyrics_anchor: LYRICS_ANCHOR,
        controls: Controls {
            center_x: screen.center().x,
            top: controls_top,
            bar_width: w - 2.0 * pad,
            button_span: (w - 2.0 * pad) * 0.6,
            unit: control_unit,
        },
    }
}

/// Paints the progress bar, times and transport buttons; returns the command a click asked for.
/// In capture mode they are decoration only.
fn paint_controls(
    ui: &egui::Ui,
    painter: &Painter,
    id: Id,
    view: &View,
    song: &SongInfo,
    c: &Controls,
    ink: &Ink,
) -> Option<Command> {
    let u = c.unit;
    let mut y = c.top;

    let thickness = Controls::thickness(u);
    let track = Rect::from_center_size(
        Pos2::new(c.center_x, y + thickness / 2.0),
        Vec2::new(c.bar_width, thickness),
    );
    let (elapsed, seek) = seek_track(ui, painter, id, view, song, track, ink);
    let mut command = seek;

    y += thickness + u * 0.04;
    paint_times(
        painter,
        track,
        y,
        elapsed,
        song.duration.unwrap_or_default(),
        Controls::time_font(u),
        with_alpha(ink.soft, 0.5),
    );
    y += Controls::time_height(painter, u) + u * 0.09;

    let playing = view.player.state == PlayState::Playing;
    let buttons = [
        (
            Icon::Shuffle,
            -0.46,
            0.05,
            view.player.random,
            Command::ToggleRandom,
        ),
        (Icon::Previous, -0.25, 0.08, true, Command::Previous),
        (
            if playing { Icon::Pause } else { Icon::Play },
            0.0,
            0.09,
            true,
            Command::TogglePause,
        ),
        (Icon::Next, 0.25, 0.08, true, Command::Next),
        (
            Icon::Repeat,
            0.46,
            0.05,
            view.player.repeat,
            Command::ToggleRepeat,
        ),
    ];
    for (icon, dx, size, lit, cmd) in buttons {
        let center = Pos2::new(c.center_x + c.button_span * dx, y);
        if view.capture {
            paint_icon_button(painter, icon, center, u * size, lit, ink.text, 0.0, false);
        } else if icon_button(
            ui,
            id.with(cmd_key(cmd)),
            icon,
            center,
            u * size,
            lit,
            ink.text,
        )
        .clicked()
        {
            command = Some(cmd);
        }
    }
    command
}

/// Paints the seek bar on `track` in `ink`, interactive unless capturing. Returns the position
/// to show, which follows the pointer while scrubbing, and the seek a click or drag asked for.
pub fn seek_track(
    ui: &egui::Ui,
    painter: &Painter,
    id: Id,
    view: &View,
    song: &SongInfo,
    track: Rect,
    ink: &Ink,
) -> (Duration, Option<Command>) {
    let duration = song.duration.unwrap_or_default();
    let progress = view.player.progress();
    if view.capture {
        paint_seek_bar(painter, track, progress, 0.0, ink);
        return (view.player.elapsed(), None);
    }
    let bar = seek_bar(
        ui,
        id.with("seek"),
        track,
        progress,
        !duration.is_zero(),
        ink,
    );
    let command = bar
        .seek
        .map(|fraction| Command::SeekTo(duration.mul_f32(fraction)));
    let elapsed = if bar.scrubbing {
        duration.mul_f32(bar.shown)
    } else {
        view.player.elapsed()
    };
    (elapsed, command)
}

/// Paints the elapsed time at the left edge of `track` and the remaining time at its right
/// edge, with their tops at `y`. The remaining time is left out when `duration` is unknown.
pub fn paint_times(
    painter: &Painter,
    track: Rect,
    y: f32,
    elapsed: Duration,
    duration: Duration,
    font: FontId,
    color: Color32,
) {
    painter.text(
        Pos2::new(track.left(), y),
        Align2::LEFT_TOP,
        fmt_time(elapsed),
        font.clone(),
        color,
    );
    if !duration.is_zero() {
        let remaining = format!("-{}", fmt_time(duration.saturating_sub(elapsed)));
        painter.text(
            Pos2::new(track.right(), y),
            Align2::RIGHT_TOP,
            remaining,
            font,
            color,
        );
    }
}

fn cmd_key(cmd: Command) -> &'static str {
    match cmd {
        Command::ToggleRandom => "shuffle",
        Command::Previous => "previous",
        Command::TogglePause => "play",
        Command::Next => "next",
        Command::ToggleRepeat => "repeat",
        Command::SeekTo(_) => "seek",
    }
}

/// Scales the colour channels of an opaque colour.
fn shade(color: Color32, k: f32) -> Color32 {
    let f = |c: u8| (c as f32 * k).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(color.r()), f(color.g()), f(color.b()))
}

/// Darkened, blurred art filling the window, a glow behind the art and a vignette. With motion
/// on, the art drifts and turns at egui input time `time`.
fn paint_backdrop(
    painter: &Painter,
    screen: Rect,
    view: &View,
    time: f64,
    glow: Option<(Pos2, f32)>,
) {
    let dominant = view
        .art
        .current
        .map_or(Color32::from_rgb(40, 40, 48), |a| a.dominant);
    painter.rect_filled(screen, 0.0, shade(dominant, 0.3));

    if backdrop::uses_artist_image(view) {
        let pan = view.motion.then_some(time);
        backdrop::paint_artist_image(painter, screen, &view.art, Color32::WHITE, pan);
        let brightness = view.art.current.map_or(0.5, |a| a.artist_brightness);
        let wash = lerp(0.45, 0.68, brightness);
        painter.rect_filled(screen, 0.0, with_alpha(Color32::BLACK, wash));
    } else if view.motion {
        backdrop::paint_motion(painter, screen, &view.art, time, 1.0);
        let brightness = view.art.current.map_or(0.5, |a| a.brightness);
        let wash = motion_darkening(brightness);
        painter.rect_filled(screen, 0.0, with_alpha(Color32::BLACK, wash));
    } else {
        paint_still_art(painter, screen, view);
    }

    if let Some((center, size)) = glow {
        // Zero alpha in premultiplied colour blends additively.
        let lit = shade(dominant, 0.55);
        let additive = Color32::from_rgba_premultiplied(lit.r(), lit.g(), lit.b(), 0);
        widgets::paint_radial_glow(painter, center, Vec2::splat(size * 1.15), additive);
    }
    let inset = Vec2::new(screen.width() * 0.22, screen.height() * 0.3);
    widgets::paint_vignette(painter, screen, inset, Color32::from_black_alpha(150));
    backdrop::paint_export_grain(painter, screen, view);
}

/// The pre-darkened blurred art stretched over the window, cross-fading between songs.
fn paint_still_art(painter: &Painter, screen: Rect, view: &View) {
    let alpha = view.art.fade;
    let current = view.art.current;
    let draw = |art: &ArtTextures, a: f32| {
        let tex = &art.backdrop;
        painter.image(
            tex.id(),
            cover_rect(tex.size_vec2(), screen),
            FULL_UV,
            with_alpha(Color32::WHITE, a),
        );
    };
    if alpha < 1.0 {
        if let Some(prev) = view.art.previous {
            draw(prev, 1.0);
        }
    }
    if let Some(cur) = current {
        draw(cur, alpha);
    }
    painter.rect_filled(screen, 0.0, Color32::from_black_alpha(70));
}

/// Centre-cropped UV rect that shows a square from an image of `size`.
pub fn square_uv(size: Vec2) -> Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return FULL_UV;
    }
    let side = size.x.min(size.y);
    let half = Vec2::new(side / size.x, side / size.y) / 2.0;
    Rect::from_center_size(Pos2::new(0.5, 0.5), half * 2.0)
}

/// Rounded album art with a soft shadow, cross-fading from the previous song's art. Corners
/// are rounded by `radius` times the side.
pub fn paint_art(painter: &Painter, rect: Rect, view: &View, radius: f32) {
    let s = rect.width();
    let radius = (s * radius).min(255.0) as u8;
    let shadow = Shadow {
        offset: [0, (s * 0.04).min(127.0) as i8],
        blur: (s * 0.16).min(255.0) as u8,
        spread: 0,
        color: Color32::from_black_alpha(130),
    };
    painter.add(shadow.as_shape(rect, radius));

    let draw = |art: &ArtTextures, a: f32| {
        let tex = &art.cover;
        let shape = RectShape::filled(rect, radius, with_alpha(Color32::WHITE, a))
            .with_texture(tex.id(), square_uv(tex.size_vec2()));
        painter.add(shape);
    };
    let alpha = view.art.fade;
    match (view.art.current, view.art.previous) {
        (Some(cur), prev) => {
            if let Some(prev) = prev.filter(|_| alpha < 1.0) {
                draw(prev, 1.0);
            }
            draw(cur, alpha);
        }
        (None, _) => {
            painter.rect_filled(rect, radius, Color32::from_white_alpha(14));
        }
    }
}

/// Date and time centred at the top of the window in `ink`, sized from its shorter side.
pub fn paint_clock(painter: &Painter, screen: Rect, view: &View, alpha: f32, ink: &Ink) {
    let h = screen.width().min(screen.height());
    let now = view.now;
    let date_pos = Pos2::new(screen.center().x, screen.top() + h * 0.045);
    let date = painter.text(
        date_pos,
        Align2::CENTER_TOP,
        now.format("%a %-d %b").to_string(),
        font_light(h * 0.028),
        with_alpha(ink.soft, 0.6 * alpha),
    );
    painter.text(
        Pos2::new(date_pos.x, date.bottom() - h * 0.005),
        Align2::CENTER_TOP,
        now.format("%H:%M").to_string(),
        font_bold(h / 10.0),
        with_alpha(ink.text, 0.85 * alpha),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_from_size() {
        let of = |w, h| Orientation::of(Vec2::new(w, h));
        assert_eq!(of(1920.0, 1080.0), Orientation::Landscape);
        assert_eq!(of(1080.0, 1920.0), Orientation::Portrait);
        assert_eq!(of(1080.0, 1080.0), Orientation::Square);
        assert_eq!(of(1200.0, 1000.0), Orientation::Landscape);
        assert_eq!(of(800.0, 1000.0), Orientation::Portrait);
        assert_eq!(of(1100.0, 1000.0), Orientation::Square);
        assert_eq!(of(10.0, 0.0), Orientation::Landscape);
    }

    #[test]
    fn square_uv_crops_centre() {
        assert_eq!(square_uv(Vec2::new(100.0, 100.0)), FULL_UV);
        let wide = square_uv(Vec2::new(200.0, 100.0));
        assert!((wide.left() - 0.25).abs() < 1e-6 && (wide.right() - 0.75).abs() < 1e-6);
        assert_eq!(wide.y_range(), FULL_UV.y_range());
        let tall = square_uv(Vec2::new(100.0, 400.0));
        assert!((tall.top() - 0.375).abs() < 1e-6 && (tall.bottom() - 0.625).abs() < 1e-6);
        assert_eq!(square_uv(Vec2::ZERO), FULL_UV);
    }
}
