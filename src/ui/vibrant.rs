//! Layout modelled on Apple Music's colourful lyrics view: a bright backdrop of enlarged,
//! blurred copies of the cover tinted by its palette, the art with a compact progress bar and
//! centred title beneath it, and synced lyrics beside it. Tall and square windows stack the art above the lyrics.

use std::f32::consts::TAU;

use egui::epaint::Mesh;
use egui::{Align2, Color32, FontId, Id, Painter, Pos2, Rect, Sense, Shape, Vec2};

use super::apple::{self, Meta, Orientation, COLUMN_SLIDE};
use super::backdrop;
use super::lyrics_column::{self, LyricsColumn};
use super::scheme::Ink;
use super::up_next::{self, UpNext};
use super::widgets::{ease, lerp};
use super::{font_bold, font_light, with_alpha, View};
use crate::art;
use crate::player::{Command, SongInfo};

/// Corner radius of the art as a fraction of its side.
/// Opacity of the dark halo behind text over the artist image.
const ARTIST_SHADOW: f32 = 0.5;
const ART_RADIUS: f32 = 0.015;
/// Opacity of the "Artist — Album" line.
const SUBTITLE_ALPHA: f32 = 0.78;
/// Opacity of the palette gradient laid over the moving art as an accent.
const PALETTE_ACCENT: f32 = 0.2;
/// Saturation boost of the palette in the backdrop.
const BACKDROP_SATURATION: f32 = 1.3;
/// Height of the lyrics column's active line, as a fraction from its top.
const LYRICS_ANCHOR: f32 = 0.4;
/// Left edge of the landscape lyrics column with lyrics showing, and the right margin beside
/// it, relative to the screen width.
const LYRICS_LEFT: f32 = 0.55;
const LYRICS_RIGHT_MARGIN: f32 = 0.07;

/// Backdrop colours when there is no art.
const FALLBACK_PALETTE: [Color32; 3] = [
    Color32::from_rgb(92, 64, 168),
    Color32::from_rgb(214, 74, 128),
    Color32::from_rgb(244, 150, 84),
];

/// A soft colour blob drifting on a Lissajous path. Positions and sizes are fractions of the
/// window; speeds are radians per second.
struct Blob {
    center: [f32; 2],
    amplitude: [f32; 2],
    speed: [f32; 2],
    phase: [f32; 2],
    /// Radius as a fraction of the window's longer side.
    radius: f32,
    opacity: f32,
}

const BLOBS: [Blob; 5] = [
    Blob {
        center: [0.22, 0.25],
        amplitude: [0.18, 0.16],
        speed: [0.29, 0.37],
        phase: [0.0, 1.3],
        radius: 0.62,
        opacity: 1.0,
    },
    Blob {
        center: [0.8, 0.3],
        amplitude: [0.16, 0.2],
        speed: [0.24, 0.20],
        phase: [2.1, 0.4],
        radius: 0.58,
        opacity: 1.0,
    },
    Blob {
        center: [0.7, 0.85],
        amplitude: [0.2, 0.14],
        speed: [0.18, 0.31],
        phase: [4.0, 2.6],
        radius: 0.6,
        opacity: 1.0,
    },
    Blob {
        center: [0.2, 0.8],
        amplitude: [0.15, 0.18],
        speed: [0.33, 0.22],
        phase: [1.0, 5.1],
        radius: 0.5,
        opacity: 0.95,
    },
    Blob {
        center: [0.5, 0.45],
        amplitude: [0.25, 0.2],
        speed: [0.15, 0.26],
        phase: [3.3, 0.9],
        radius: 0.38,
        opacity: 0.7,
    },
];

/// Times and progress bar spanning the width, then the title and "Artist — Album" centred
/// under them.
struct Info {
    meta: Meta,
    left: f32,
    top: f32,
    width: f32,
    /// Size unit for the bar thickness and text.
    unit: f32,
}

impl Info {
    fn new(painter: &Painter, song: &SongInfo, width: f32, unit: f32, ink: &Ink) -> Self {
        Self {
            meta: Meta::new(
                painter,
                song,
                unit * 0.052,
                unit * 0.045,
                width,
                SUBTITLE_ALPHA,
                ink,
            ),
            left: 0.0,
            top: 0.0,
            width,
            unit,
        }
    }

    fn time_font(&self) -> FontId {
        font_light(self.unit * 0.034)
    }

    fn time_height(&self, painter: &Painter) -> f32 {
        painter
            .layout_no_wrap("0:00".into(), self.time_font(), Color32::WHITE)
            .size()
            .y
    }

    fn thickness(&self) -> f32 {
        (self.unit * 0.017).max(4.5)
    }

    fn height(&self, painter: &Painter) -> f32 {
        self.time_height(painter)
            + self.unit * 0.026
            + self.thickness()
            + self.unit * 0.055
            + self.meta.height()
    }

    fn at(mut self, left: f32, top: f32) -> Self {
        self.left = left;
        self.top = top;
        self
    }
}

/// Where each part of the layout goes for one frame.
struct Placement {
    art: Rect,
    info: Info,
    lyrics: Rect,
    /// Font size of the active lyric line.
    lyrics_base: f32,
    lyrics_anchor: f32,
}

/// Draws the layout and returns the command a click asked for, if any.
pub fn draw(ui: &mut egui::Ui, view: &View) -> Option<Command> {
    let screen = ui.max_rect();
    let ctx = ui.ctx().clone();
    let id = Id::unique("vibrant-layout");
    let background = (!view.capture).then(|| ui.interact(screen, id, Sense::click()));
    let background_click = || {
        background
            .as_ref()
            .is_some_and(|r| r.clicked())
            .then_some(Command::TogglePause)
    };
    let painter = ui.painter_at(screen);
    let unit = screen.width().min(screen.height());
    // A still backdrop is the moving one frozen at time zero.
    let time = if view.motion {
        ctx.input(|i| i.time)
    } else {
        0.0
    };

    paint_backdrop(&painter, screen, view, time);
    backdrop::paint_export_grain(&painter, screen, view);
    let ink = ink(view);

    let show_clock = view.show_clock && !view.capture;
    let clock_t = ease(ctx.animate_bool_with_time(id.with("clock"), show_clock, 0.4));
    let lyrics_t = ease(ctx.animate_value_with_time(
        id.with("lyrics"),
        lyrics_column::column_target(ui, id, view),
        COLUMN_SLIDE,
    ));
    if clock_t > 0.0 {
        apple::paint_clock(&painter, screen, view, clock_t, &ink);
    }

    let Some(song) = view.player.current.as_ref() else {
        let text = if view.player.connected {
            "Nothing playing"
        } else {
            view.player.waiting_text()
        };
        painter.text(
            screen.center(),
            Align2::CENTER_CENTER,
            text,
            font_bold(unit / 22.0),
            with_alpha(Color32::WHITE, 0.85),
        );
        return background_click();
    };

    // Stacked arrangements move down below the Up next card while it shows.
    let up_next = UpNext::new(ui, screen, view);
    let clear = up_next.clear_below(screen.top(), unit * 0.04);
    let place = match Orientation::of(screen.size()) {
        Orientation::Landscape => landscape(&painter, screen, song, clock_t, lyrics_t, &ink),
        Orientation::Portrait => portrait(&painter, screen, song, clock_t, clear, lyrics_t, &ink),
        Orientation::Square => square(&painter, screen, song, clock_t, clear, lyrics_t, &ink),
    };

    if lyrics_t > 0.01 {
        if !view.capture {
            // Clicks on the lyrics are swallowed rather than toggling playback.
            ui.interact(place.lyrics, id.with("lyrics-area"), Sense::click());
        }
        let column = LyricsColumn {
            rect: place.lyrics,
            clear_top: up_next.clear_below(place.lyrics.top(), screen.height() * 0.04),
            base: place.lyrics_base,
            anchor: place.lyrics_anchor,
            alpha: lyrics_t,
            line_alpha,
            shadow: text_shadow(view),
            ink,
        };
        lyrics_column::paint(ui, id.with(&song.file), &column, view);
    }
    up_next.paint(
        &painter,
        view,
        &up_next::Colors::from_ink(&ink, SUBTITLE_ALPHA, text_shadow(view)),
    );
    apple::paint_art(&painter, place.art, view, ART_RADIUS);
    paint_info(ui, &painter, id, view, song, &place.info, &ink).or_else(background_click)
}

/// Art with its info in a column on the left, lyrics on the right. Without lyrics the column
/// slides to the centre.
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
        Pos2::new(screen.left(), screen.top() + h * lerp(0.08, 0.22, clock_t)),
        Pos2::new(screen.right(), screen.bottom() - h * 0.08),
    );
    let s = lerp((h * 0.5).min(w * 0.4), (h * 0.44).min(w * 0.32), lyrics_t)
        .min(content.height() / 1.32);
    let column_x = lerp(screen.center().x, screen.left() + w * 0.27, lyrics_t);

    let info = Info::new(painter, song, s, s, ink);
    let gap = s * 0.06;
    let block_h = s + gap + info.height(painter);
    let top = content.center().y - block_h / 2.0;
    let art = Rect::from_min_size(Pos2::new(column_x - s / 2.0, top), Vec2::splat(s));
    let lyrics = Rect::from_min_max(
        Pos2::new(
            screen.left() + w * lerp(0.6, LYRICS_LEFT, lyrics_t),
            content.top(),
        ),
        Pos2::new(screen.right() - w * LYRICS_RIGHT_MARGIN, content.bottom()),
    );

    Placement {
        art,
        info: info.at(art.left(), art.bottom() + gap),
        lyrics_base: (h / 22.0).min(lyrics.width() / 14.0),
        lyrics_anchor: LYRICS_ANCHOR,
        lyrics,
    }
}

/// Width of the landscape lyrics column with lyrics showing, for a screen `width` wide.
pub fn lyrics_width(width: f32) -> f32 {
    width * (1.0 - LYRICS_LEFT - LYRICS_RIGHT_MARGIN)
}

/// Art centred near the top with its info beneath, lyrics below. Without lyrics the art and
/// info sit centred. Nothing goes above `clear`.
fn portrait(
    painter: &Painter,
    screen: Rect,
    song: &SongInfo,
    clock_t: f32,
    clear: f32,
    lyrics_t: f32,
    ink: &Ink,
) -> Placement {
    let (w, h) = (screen.width(), screen.height());
    let u = w.min(h);
    let pad = w * 0.08;
    let top = (screen.top() + lerp(h * 0.07, u * 0.24, clock_t)).max(clear);
    let bottom = screen.bottom() - h * 0.04;

    let s = lerp((w * 0.8).min(h * 0.42), (w * 0.62).min(h * 0.34), lyrics_t);
    let info = Info::new(painter, song, s, u * 0.85, ink);
    let gap = u * 0.04;
    let block_h = s + gap + info.height(painter);
    let centered = (top + bottom - block_h) / 2.0;
    let art_top = lerp(centered.max(top), top, lyrics_t);
    let art = Rect::from_min_size(
        Pos2::new(screen.center().x - s / 2.0, art_top),
        Vec2::splat(s),
    );
    let info_top = art.bottom() + gap;
    let lyrics = Rect::from_min_max(
        Pos2::new(
            screen.left() + pad,
            info_top + info.height(painter) + u * 0.06,
        ),
        Pos2::new(screen.right() - pad, bottom),
    );

    Placement {
        art,
        info: info.at(art.left(), info_top),
        lyrics,
        lyrics_base: u / 19.0,
        lyrics_anchor: 0.3,
    }
}

/// Art at the top left with its info beside it and lyrics below. Without lyrics the art and
/// info sit centred vertically. Nothing goes above `clear`.
fn square(
    painter: &Painter,
    screen: Rect,
    song: &SongInfo,
    clock_t: f32,
    clear: f32,
    lyrics_t: f32,
    ink: &Ink,
) -> Placement {
    let (w, h) = (screen.width(), screen.height());
    let u = w.min(h);
    let pad = w * 0.08;
    let top = (screen.top() + lerp(u * 0.07, u * 0.23, clock_t)).max(clear);
    let bottom = screen.bottom() - u * 0.05;

    let s = lerp(u * 0.42, u * 0.3, lyrics_t);
    let gap = u * 0.045;
    let info = Info::new(painter, song, w - 2.0 * pad - s - gap, u * 0.75, ink);
    let centered = (top + bottom - s) / 2.0;
    let art_top = lerp(centered.max(top), top, lyrics_t);
    let art = Rect::from_min_size(Pos2::new(screen.left() + pad, art_top), Vec2::splat(s));
    let info_top = art.center().y - info.height(painter) / 2.0;
    let lyrics = Rect::from_min_max(
        Pos2::new(screen.left() + pad, art.bottom() + u * 0.05),
        Pos2::new(screen.right() - pad, bottom),
    );

    Placement {
        art,
        info: info.at(art.right() + gap, info_top),
        lyrics,
        lyrics_base: u / 20.0,
        lyrics_anchor: 0.36,
    }
}

/// Paints the times, progress bar and title; returns the seek a click asked for.
fn paint_info(
    ui: &egui::Ui,
    painter: &Painter,
    id: Id,
    view: &View,
    song: &SongInfo,
    info: &Info,
    ink: &Ink,
) -> Option<Command> {
    let mut y = info.top;
    let times_top = y;
    y += info.time_height(painter) + info.unit * 0.026;
    let thickness = info.thickness();
    let track = Rect::from_min_size(Pos2::new(info.left, y), Vec2::new(info.width, thickness));
    let (elapsed, command) = apple::seek_track(ui, painter, id, view, song, track, ink);
    apple::paint_times(
        painter,
        track,
        times_top,
        elapsed,
        song.duration.unwrap_or_default(),
        info.time_font(),
        with_alpha(ink.soft, 0.7),
    );
    y += thickness + info.unit * 0.055;
    let pos = Pos2::new(info.left + info.width / 2.0, y);
    let shadow = text_shadow(view);
    info.meta
        .paint_shadow(painter, pos, true, shadow, info.unit * 0.012);
    info.meta.paint(painter, pos, true);
    command
}

/// Opacity of an inactive lyric line `distance` lines after the active one (negative for past
/// lines). Nearby lines stay fairly bright against the colourful backdrop.
fn line_alpha(distance: isize) -> f32 {
    match distance {
        0 | 1 => 0.68,
        2 => 0.62,
        3 => 0.52,
        d if d > 3 => (0.52 - 0.08 * (d - 3) as f32).max(0.25),
        -1 => 0.58,
        -2 => 0.48,
        _ => 0.36,
    }
}

/// Relative luminance of a colour in `0.0..=1.0`, from its gamma-encoded channels.
fn luminance(c: Color32) -> f32 {
    (0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32) / 255.0
}

/// Smoothstep of `x` between `lo` and `hi`.
fn smooth(x: f32, lo: f32, hi: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Mean luminance of the art behind the layout, following the cross-fade between songs.
fn backdrop_brightness(view: &View) -> f32 {
    let of = |art: Option<&art::ArtTextures>| art.map_or(0.4, |a| a.brightness);
    match view.art.previous.filter(|_| view.art.fade < 1.0) {
        Some(prev) => lerp(prev.brightness, of(view.art.current), view.art.fade),
        None => of(view.art.current),
    }
}

/// Opacity of the black wash over a backdrop of `brightness`: almost none for dark and
/// mid-tone art, a little for very light art, which stays light.
pub fn darkening(brightness: f32) -> f32 {
    lerp(0.04, 0.2, smooth(brightness, 0.5, 0.9))
}

/// Relative luminance of the backdrop behind the text after its wash, roughly.
fn backdrop_luminance(view: &View) -> f32 {
    let gamma = if backdrop::uses_artist_image(view) {
        let brightness = view.art.current.map_or(0.5, |a| a.artist_brightness);
        brightness * (1.0 - artist_wash(brightness))
    } else {
        let brightness = backdrop_brightness(view);
        brightness * (1.0 - darkening(brightness))
    };
    gamma.clamp(0.0, 1.0).powf(2.2)
}

/// Text and control colours: light tints of the album colours that keep most of white's
/// contrast against the backdrop, or plain white with album colours off.
fn ink(view: &View) -> Ink {
    if view.tint {
        Ink::on_light(&view.art.scheme(), backdrop_luminance(view))
    } else {
        Ink::WHITE
    }
}

/// Opacity of the black wash over an artist image of `brightness`.
fn artist_wash(brightness: f32) -> f32 {
    lerp(0.2, 0.5, brightness)
}

/// Opacity of the dark halo behind text, stronger on lighter backdrops and on photos.
fn text_shadow(view: &View) -> f32 {
    if backdrop::uses_artist_image(view) {
        return ARTIST_SHADOW;
    }
    lerp(0.18, 0.45, smooth(backdrop_brightness(view), 0.3, 0.8))
}

/// Mixes `a` towards `b` by `t`, channel by channel.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| lerp(x as f32, y as f32, t).round() as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// Raises the saturation of a colour that isn't grey.
fn saturate(c: Color32) -> Color32 {
    let [h, s, v] = art::to_hsv([c.r(), c.g(), c.b()].map(f32::from));
    if s < 0.18 {
        return c;
    }
    art::from_hsv([h, (s * BACKDROP_SATURATION).min(1.0), v])
}

/// The palette made more saturated, with at least three colours: a palette of one or two is
/// padded with lighter and darker shades.
fn expand(palette: &[Color32]) -> Vec<Color32> {
    let mut colors: Vec<_> = if palette.is_empty() {
        FALLBACK_PALETTE.to_vec()
    } else {
        palette.iter().copied().map(saturate).collect()
    };
    let first = colors[0];
    if colors.len() < 3 {
        colors.push(mix(first, Color32::WHITE, 0.3));
    }
    if colors.len() < 3 {
        colors.push(mix(first, Color32::BLACK, 0.35));
    }
    colors
}

/// Enlarged, blurred copies of the art, moving at `time`, with the palette gradient faintly
/// over them as an accent, cross-fading from the previous song, and a light wash for text
/// contrast. Without art the palette gradient fills the window.
fn paint_backdrop(painter: &Painter, screen: Rect, view: &View, time: f64) {
    let art = &view.art;
    let current = art.current.map_or(&FALLBACK_PALETTE[..], |a| &a.palette);
    let previous = art.previous.filter(|_| art.fade < 1.0);
    let palettes = |alpha: f32| {
        if let Some(prev) = previous {
            paint_gradient(painter, screen, &prev.palette, time, alpha);
        }
        let fade = if previous.is_some() { art.fade } else { 1.0 };
        paint_gradient(painter, screen, current, time, alpha * fade);
    };
    if backdrop::uses_artist_image(view) {
        let pan = view.motion.then_some(time);
        backdrop::paint_artist_image(painter, screen, art, Color32::WHITE, pan);
        // Lighter than the Apple layout's wash; the text halo does the rest.
        let brightness = art.current.map_or(0.5, |a| a.artist_brightness);
        let wash = artist_wash(brightness);
        painter.rect_filled(screen, 0.0, with_alpha(Color32::BLACK, wash));
        return;
    }
    if art.current.is_none() {
        palettes(1.0);
    } else {
        backdrop::paint_motion(painter, screen, art, time, 1.0);
        palettes(PALETTE_ACCENT);
    }
    let wash = darkening(backdrop_brightness(view));
    painter.rect_filled(screen, 0.0, with_alpha(Color32::BLACK, wash));
}

/// A large-scale blend between palette colours with soft blobs of them drifting over it.
fn paint_gradient(painter: &Painter, screen: Rect, palette: &[Color32], time: f64, alpha: f32) {
    let colors = expand(palette);
    let n = colors.len();
    let corners = [colors[0], colors[1], colors[2 % n], colors[0]];
    paint_bilinear(painter, screen, corners, alpha);

    let size = screen.size();
    let long = size.x.max(size.y);
    // Wrap the clock so f32 phases keep their precision over long sessions.
    let t = (time % 100_000.0) as f32;
    for (i, blob) in BLOBS.iter().enumerate() {
        let color = colors[(i + 1) % n];
        let at = |k: usize| {
            blob.center[k] + blob.amplitude[k] * (blob.speed[k] * t + blob.phase[k]).sin()
        };
        let center = screen.min + Vec2::new(at(0) * size.x, at(1) * size.y);
        paint_soft_blob(
            painter,
            center,
            blob.radius * long,
            color,
            blob.opacity * alpha,
        );
    }

    // A small, bright highlight drifting over the rest.
    let brightest = colors
        .iter()
        .copied()
        .max_by(|a, b| luminance(*a).total_cmp(&luminance(*b)))
        .unwrap_or(Color32::WHITE);
    let center = screen.min
        + Vec2::new(
            (0.45 + 0.2 * (0.22 * t + 0.7).sin()) * size.x,
            (0.3 + 0.18 * (0.29 * t + 2.0).sin()) * size.y,
        );
    let highlight = mix(brightest, Color32::WHITE, 0.1);
    paint_soft_blob(painter, center, 0.3 * long, highlight, 0.6 * alpha);
}

/// Fills `rect` with a bilinear blend of `corners` (top left, top right, bottom left, bottom
/// right) at opacity `alpha`.
fn paint_bilinear(painter: &Painter, rect: Rect, corners: [Color32; 4], alpha: f32) {
    const STEPS: u32 = 8;
    let mut mesh = Mesh::default();
    for row in 0..=STEPS {
        let v = row as f32 / STEPS as f32;
        let left = mix(corners[0], corners[2], v);
        let right = mix(corners[1], corners[3], v);
        for col in 0..=STEPS {
            let u = col as f32 / STEPS as f32;
            let pos = rect.min + rect.size() * Vec2::new(u, v);
            mesh.colored_vertex(pos, with_alpha(mix(left, right, u), alpha));
        }
    }
    let at = |row: u32, col: u32| row * (STEPS + 1) + col;
    for row in 0..STEPS {
        for col in 0..STEPS {
            mesh.add_triangle(at(row, col), at(row, col + 1), at(row + 1, col + 1));
            mesh.add_triangle(at(row, col), at(row + 1, col + 1), at(row + 1, col));
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// Radial blob of `color` at opacity `alpha` in the centre, falling smoothly to nothing at
/// `radius`.
fn paint_soft_blob(painter: &Painter, center: Pos2, radius: f32, color: Color32, alpha: f32) {
    const SEGMENTS: u32 = 72;
    const RINGS: u32 = 24;
    let mut mesh = Mesh::default();
    mesh.colored_vertex(center, with_alpha(color, alpha));
    for ring in 1..=RINGS {
        let r = ring as f32 / RINGS as f32;
        let falloff = (1.0 - r * r).powi(2);
        let ring_color = with_alpha(color, alpha * falloff);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lighter_art_gets_a_little_more_wash() {
        assert!(darkening(0.9) > darkening(0.3));
        assert!(darkening(1.0) <= 0.2);
        assert!(darkening(0.0) > 0.0);
    }

    #[test]
    fn expand_pads_short_palettes() {
        assert_eq!(expand(&[]).len(), FALLBACK_PALETTE.len());
        let one = expand(&[Color32::from_rgb(200, 40, 40)]);
        assert_eq!(one.len(), 3);
        assert!(luminance(one[1]) > luminance(one[0]));
        assert!(luminance(one[2]) < luminance(one[0]));
        let five = [Color32::RED; 5];
        assert_eq!(expand(&five).len(), 5);
    }

    #[test]
    fn nearby_lines_are_brighter() {
        assert!(line_alpha(1) >= line_alpha(2));
        assert!(line_alpha(2) > line_alpha(5));
        assert!(line_alpha(-1) > line_alpha(-3));
        assert!(line_alpha(100) > 0.0);
    }
}
