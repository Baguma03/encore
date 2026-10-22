//! Layout modelled on Apple Music's fullscreen player, over a blurred, darkened copy of the
//! album art. Wide windows put art and controls on the left and synced lyrics on the right;
//! tall and square windows stack the art above the lyrics.

use std::sync::Arc;

use egui::epaint::{RectShape, Shadow};
use egui::{Align2, Color32, Galley, Id, Painter, Pos2, Rect, Sense, Vec2};
use mpd_display_rs::lyrics::{Lyrics, LyricsState};

use super::widgets::{
    self, ease, edge_fade, icon_button, lerp, paint_blurred_galley, paint_icon_button,
    paint_seek_bar, seek_bar, smooth_value, Icon,
};
use super::{cover_rect, fit_text, fmt_time, font_bold, font_light, with_alpha, View, FULL_UV};
use crate::art::ArtTextures;
use crate::player::{Command, PlayState, SongInfo};

/// Height of the art-and-controls block as a multiple of the art size.
const BLOCK_HEIGHT: f32 = 1.56;
/// Seconds for the art column to slide between centred and left-aligned.
const COLUMN_SLIDE: f32 = 0.7;
/// Seconds for a lyric line to grow into or out of the active style.
const LINE_EMPHASIS: f32 = 0.35;
/// Exponential rate of the lyrics scroll, per second.
const SCROLL_RATE: f32 = 7.0;
/// Size of inactive lyric lines relative to the active one.
const INACTIVE_SCALE: f32 = 0.9;
/// Height of the lyrics column's active line, as a fraction from its top.
const LYRICS_ANCHOR: f32 = 0.36;
/// Width-to-height ratios at or beyond which the layout is landscape or portrait.
const LANDSCAPE_ASPECT: f32 = 1.2;
const PORTRAIT_ASPECT: f32 = 0.8;

const MUTED: f32 = 0.6;

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
struct Meta {
    title: Arc<Galley>,
    subtitle: Arc<Galley>,
    gap: f32,
}

impl Meta {
    fn new(painter: &Painter, song: &SongInfo, title: f32, subtitle: f32, width: f32) -> Self {
        let sub = match (song.artist.is_empty(), song.album.is_empty()) {
            (false, false) => format!("{} — {}", song.artist, song.album),
            (false, true) => song.artist.clone(),
            (true, _) => song.album.clone(),
        };
        Self {
            title: fit_text(
                painter,
                &song.title,
                font_bold,
                title,
                width,
                Color32::WHITE,
            ),
            subtitle: fit_text(
                painter,
                &sub,
                font_light,
                subtitle,
                width,
                with_alpha(Color32::WHITE, MUTED),
            ),
            gap: title / 7.5,
        }
    }

    fn height(&self) -> f32 {
        self.title.size().y + self.gap + self.subtitle.size().y
    }

    /// Paints the lines below `pos`, centred on it or starting at it.
    fn paint(&self, painter: &Painter, pos: Pos2, centered: bool) {
        let x = |g: &Galley| {
            if centered {
                pos.x - g.size().x / 2.0
            } else {
                pos.x
            }
        };
        painter.galley(
            Pos2::new(x(&self.title), pos.y),
            self.title.clone(),
            Color32::WHITE,
        );
        let y = pos.y + self.title.size().y + self.gap;
        painter.galley(
            Pos2::new(x(&self.subtitle), y),
            self.subtitle.clone(),
            Color32::WHITE,
        );
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
        (unit * 0.011).max(3.0)
    }

    /// Height from the top of the bar to the bottom of the largest button.
    fn height(painter: &Painter, unit: f32) -> f32 {
        Self::thickness(unit) + unit * 0.165 + Self::time_height(painter, unit)
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

    let show_clock = view.show_clock && !view.capture;
    let clock_t = ease(ctx.animate_bool_with_time(id.with("clock"), show_clock, 0.4));
    let lyrics_t = ease(ctx.animate_value_with_time(
        id.with("lyrics"),
        lyrics_column_target(ui, id, view),
        COLUMN_SLIDE,
    ));

    let Some(song) = view.player.current.as_ref() else {
        paint_backdrop(&painter, screen, view, None);
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
            paint_clock(&painter, screen, view, clock_t);
        }
        return background_click();
    };

    let place = match Orientation::of(screen.size()) {
        Orientation::Landscape => landscape(&painter, screen, song, clock_t, lyrics_t),
        Orientation::Portrait => portrait(&painter, screen, song, clock_t, lyrics_t),
        Orientation::Square => square(&painter, screen, song, clock_t, lyrics_t),
    };

    paint_backdrop(
        &painter,
        screen,
        view,
        Some((place.art.center(), place.art.width())),
    );
    if clock_t > 0.0 {
        paint_clock(&painter, screen, view, clock_t);
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
        };
        paint_lyrics_column(ui, id.with(&song.file), &column, view);
    }
    paint_art(&painter, place.art, view);
    place
        .meta
        .paint(&painter, place.meta_pos, place.meta_centered);

    paint_controls(ui, &painter, id, view, song, &place.controls).or_else(background_click)
}

/// Art, title and controls in a column on the left, lyrics on the right. Without lyrics the
/// column slides to the centre.
fn landscape(
    painter: &Painter,
    screen: Rect,
    song: &SongInfo,
    clock_t: f32,
    lyrics_t: f32,
) -> Placement {
    let (w, h) = (screen.width(), screen.height());
    let content = Rect::from_min_max(
        Pos2::new(screen.left(), screen.top() + h * lerp(0.07, 0.22, clock_t)),
        Pos2::new(screen.right(), screen.bottom() - h * 0.07),
    );
    let s = lerp((h * 0.48).min(w * 0.4), (h * 0.44).min(w * 0.3), lyrics_t)
        .min(content.height() / BLOCK_HEIGHT);
    let column_x = lerp(screen.center().x, screen.left() + w * 0.27, lyrics_t);

    let meta = Meta::new(painter, song, s * 0.075, s * 0.055, s);
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
) -> Placement {
    let (w, h) = (screen.width(), screen.height());
    let u = w.min(h);
    let pad = w * 0.08;
    let top = screen.top() + lerp(h * 0.07, u * 0.24, clock_t);

    let control_unit = u * 0.5;
    let controls_top = screen.bottom() - h * 0.045 - Controls::height(painter, control_unit);

    let s = lerp((w * 0.78).min(h * 0.42), (w * 0.62).min(h * 0.36), lyrics_t);
    let meta = Meta::new(painter, song, u * 0.05, u * 0.036, s);
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
) -> Option<Command> {
    let u = c.unit;
    let mut command = None;
    let mut y = c.top;

    let duration = song.duration.unwrap_or_default();
    let thickness = Controls::thickness(u);
    let track = Rect::from_center_size(
        Pos2::new(c.center_x, y + thickness / 2.0),
        Vec2::new(c.bar_width, thickness),
    );
    let progress = view.player.progress();
    let elapsed = if view.capture {
        paint_seek_bar(painter, track, progress, 0.0);
        view.player.elapsed()
    } else {
        let bar = seek_bar(ui, id.with("seek"), track, progress, !duration.is_zero());
        if let Some(fraction) = bar.seek {
            command = Some(Command::SeekTo(duration.mul_f32(fraction)));
        }
        if bar.scrubbing {
            duration.mul_f32(bar.shown)
        } else {
            view.player.elapsed()
        }
    };

    let time_font = Controls::time_font(u);
    let time_color = with_alpha(Color32::WHITE, 0.5);
    y += thickness + u * 0.03;
    painter.text(
        Pos2::new(track.left(), y),
        Align2::LEFT_TOP,
        fmt_time(elapsed),
        time_font.clone(),
        time_color,
    );
    if !duration.is_zero() {
        let remaining = format!("-{}", fmt_time(duration.saturating_sub(elapsed)));
        painter.text(
            Pos2::new(track.right(), y),
            Align2::RIGHT_TOP,
            remaining,
            time_font,
            time_color,
        );
    }
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
            paint_icon_button(painter, icon, center, u * size, lit, 0.0, false);
        } else if icon_button(ui, id.with(cmd_key(cmd)), icon, center, u * size, lit).clicked() {
            command = Some(cmd);
        }
    }
    command
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

/// 1.0 when the lyrics column should be shown, 0.0 when the art should sit centred.
/// While lyrics are loading the previous choice is kept, so songs without lyrics don't bounce.
fn lyrics_column_target(ui: &egui::Ui, id: Id, view: &View) -> f32 {
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

/// Scales the colour channels of an opaque colour.
fn shade(color: Color32, k: f32) -> Color32 {
    let f = |c: u8| (c as f32 * k).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(color.r()), f(color.g()), f(color.b()))
}

/// Darkened, blurred art filling the window, a glow behind the art and a vignette.
fn paint_backdrop(painter: &Painter, screen: Rect, view: &View, glow: Option<(Pos2, f32)>) {
    let alpha = view.art.fade;
    let current = view.art.current;
    let dominant = current.map_or(Color32::from_rgb(40, 40, 48), |a| a.dominant);
    painter.rect_filled(screen, 0.0, shade(dominant, 0.3));

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

    if let Some((center, size)) = glow {
        // Zero alpha in premultiplied colour blends additively.
        let lit = shade(dominant, 0.55);
        let additive = Color32::from_rgba_premultiplied(lit.r(), lit.g(), lit.b(), 0);
        widgets::paint_radial_glow(painter, center, Vec2::splat(size * 1.15), additive);
    }
    let inset = Vec2::new(screen.width() * 0.22, screen.height() * 0.3);
    widgets::paint_vignette(painter, screen, inset, Color32::from_black_alpha(150));
}

/// Centre-cropped UV rect that shows a square from an image of `size`.
fn square_uv(size: Vec2) -> Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return FULL_UV;
    }
    let side = size.x.min(size.y);
    let half = Vec2::new(side / size.x, side / size.y) / 2.0;
    Rect::from_center_size(Pos2::new(0.5, 0.5), half * 2.0)
}

/// Rounded album art with a soft shadow, cross-fading from the previous song's art.
fn paint_art(painter: &Painter, rect: Rect, view: &View) {
    let s = rect.width();
    let radius = (s * 0.03).min(255.0) as u8;
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

/// Date and time centred at the top of the window, sized from its shorter side.
fn paint_clock(painter: &Painter, screen: Rect, view: &View, alpha: f32) {
    let h = screen.width().min(screen.height());
    let now = view.now;
    let date_pos = Pos2::new(screen.center().x, screen.top() + h * 0.045);
    let date = painter.text(
        date_pos,
        Align2::CENTER_TOP,
        now.format("%a %-d %b").to_string(),
        font_light(h * 0.028),
        with_alpha(Color32::WHITE, 0.6 * alpha),
    );
    painter.text(
        Pos2::new(date_pos.x, date.bottom() - h * 0.005),
        Align2::CENTER_TOP,
        now.format("%H:%M").to_string(),
        font_bold(h / 10.0),
        with_alpha(Color32::WHITE, 0.85 * alpha),
    );
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

struct LyricsColumn {
    rect: Rect,
    /// Font size of the active line.
    base: f32,
    /// Height of the active line, as a fraction from the top of `rect`.
    anchor: f32,
    alpha: f32,
}

fn paint_lyrics_column(ui: &egui::Ui, id: Id, column: &LyricsColumn, view: &View) {
    let LyricsColumn {
        rect,
        base,
        alpha,
        anchor: anchor_frac,
    } = *column;
    let painter = ui.painter_at(rect.expand2(Vec2::new(rect.width() * 0.05, 0.0)));
    let anchor = rect.top() + rect.height() * anchor_frac;

    let lyrics = match &view.lyrics {
        LyricsState::Found(lyrics) => Arc::clone(lyrics),
        LyricsState::Loading => {
            let color = with_alpha(Color32::WHITE, 0.45 * alpha);
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
                    with_alpha(Color32::WHITE, a),
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
                paint_blurred_galley(
                    &painter,
                    Pos2::new(rect.left(), top),
                    &galley,
                    with_alpha(Color32::WHITE, a),
                    blur,
                );
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
                    painter.galley_with_override_text_color(
                        Pos2::new(rect.left(), top),
                        galley,
                        with_alpha(Color32::WHITE, a),
                    );
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
