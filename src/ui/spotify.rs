//! Layout modelled on the Spotify desktop client's fullscreen mode, with controls styled after
//! Quickshell's media widget: a rounded-square play/pause button level with the bottom of the
//! album art, and a full-width wavy seek bar, optionally between previous and next buttons.

use std::sync::Arc;
use std::time::Duration;

use egui::epaint::RectShape;
use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align, Align2, Color32, CursorIcon, FontId, Galley, Id, Painter, Pos2, Rect, Sense, Vec2,
};
use mpd_display_rs::lyrics::{Lyrics, LyricsState};

use super::apple::{self, square_uv};
use super::backdrop;
use super::icons::{paint_icon, Icon};
use super::scheme::{mix, Scheme};
use super::wavy::{self, BarColors, Wavy};
use super::widgets::{ease, lerp, paint_smeared_galley, seek_input};
use super::{cover_rect, fit_text, font_bold, with_alpha, View, FULL_UV};
use crate::art::ArtTextures;
use crate::config::{LyricsAlign, LyricsFlow};
use crate::player::{Command, PlayState, SongInfo};

const BACKGROUND_IMAGE_TINT: Color32 = Color32::from_rgb(75, 75, 75);
const BACKGROUND: Color32 = Color32::from_rgb(50, 50, 50);

/// Shown in place of an empty lyric line and before the first one.
const GAP_TEXT: &str = "• • •";
/// Seconds for the classic lyrics to scroll by one line.
const LYRICS_SCROLL: f64 = 0.6;
/// Seconds for the stacked lyrics to slide up by one line.
const STACK_SCROLL: f64 = 0.4;
/// Opacity of the active stacked line and of the three below it; the next step is gone.
const STACK_ALPHA: [f32; 5] = [1.0, 0.55, 0.35, 0.15, 0.0];
/// Vertical smear of moving stacked lines per line-per-second of scroll speed, relative to
/// the font size, and its ceiling.
const SMEAR: f32 = 0.08;
const MAX_SMEAR: f32 = 0.4;
/// Opacity of the line above and below the active one.
const PREVIOUS_ALPHA: f32 = 0.28;
const NEXT_ALPHA: f32 = 0.35;
/// Size of the lines around the active one relative to it, as in the Apple layout.
const SIDE_SCALE: f32 = 0.9;

/// Song progress from which the Up next card shows.
const UP_NEXT_AT: f32 = 0.9;
/// Height around which the lyrics are centred, relative to the screen height.
const LYRICS_CENTER: f32 = 0.3;
/// Corner radius of the album art relative to its size.
const ART_RADIUS: f32 = 0.035;

/// Seconds for the Up next card to slide in or out.
const UP_NEXT_SLIDE: f32 = 0.5;

/// Seconds for the seek bar's wave to flatten when paused or rise again.
const WAVE_FADE: f32 = 0.35;
/// Seconds for the play button to change colour between playing and paused.
const BUTTON_MORPH: f32 = 0.2;
/// Share of an icon's em box left of the skip icons' visible outline.
const ICON_SIDE_BEARING: f32 = 0.19;
/// Height of the seek bar's handle relative to its line width.
const HANDLE_HEIGHT: f32 = 3.6;

/// Positions of the bottom block, shared by drawing and hit-testing.
struct Block {
    art: Rect,
    /// Top-left corner of the title.
    title: Pos2,
    /// Font size of the title; the artist is half of it.
    title_size: f32,
    /// Icon size of the previous and next buttons, which are as tall as the controls row.
    icon: f32,
    /// Whether the previous and next buttons show.
    skip_buttons: bool,
    previous: Rect,
    next: Rect,
    /// The seek bar's row, between the previous and next buttons when they show and from
    /// margin to margin otherwise.
    bar: Rect,
    play: Rect,
    /// Thickness of the seek bar's wave and track.
    line: f32,
}

impl Block {
    fn new(screen: Rect, skip_buttons: bool) -> Self {
        let (w, h) = (screen.width(), screen.height());
        let unit = w.min(h);
        let art_size = unit / 3.0 * 1.12;
        let (left, right) = (screen.left() + w / 16.0, screen.right() - w / 16.0);
        let art = Rect::from_min_size(
            Pos2::new(left, screen.top() + h / 6.0 * 5.0 - art_size),
            Vec2::splat(art_size),
        );
        let title = Pos2::new(art.right() + art_size * 0.1, art.top() + art_size * 0.4);

        let play_size = unit * 0.105;
        let play = Rect::from_min_max(
            Pos2::new(right - play_size, art.bottom() - play_size),
            Pos2::new(right, art.bottom()),
        );

        // The controls row runs along the bottom between the side margins, the icons' visible
        // outlines reaching the margins.
        let icon = unit * 0.055;
        let row_y = screen.top() + h * 0.903;
        let row = Rect::from_x_y_ranges(left..=right, row_y - icon / 2.0..=row_y + icon / 2.0);
        let previous = Rect::from_min_size(
            Pos2::new(left - icon * ICON_SIDE_BEARING, row.top()),
            Vec2::splat(icon),
        );
        let next = Rect::from_min_size(
            Pos2::new(right + icon * ICON_SIDE_BEARING - icon, row.top()),
            Vec2::splat(icon),
        );
        let gap = unit * 0.008;
        let bar = if skip_buttons {
            Rect::from_x_y_ranges(
                previous.right() + gap..=(next.left() - gap).max(previous.right() + gap),
                row.y_range(),
            )
        } else {
            row
        };
        Self {
            art,
            title,
            title_size: unit / 9.0 * 0.9,
            icon,
            skip_buttons,
            previous,
            next,
            bar,
            play,
            line: (unit * 0.0065).max(2.0),
        }
    }

    fn wavy(&self, wave: f32, time: f64, pressed: bool) -> Wavy {
        Wavy {
            line: self.line,
            handle_height: HANDLE_HEIGHT,
            wave,
            time,
            pressed,
        }
    }
}

/// Draws the layout and returns the command a click asked for, if any. Clicks away from the
/// controls toggle play/pause.
pub fn draw(ui: &mut egui::Ui, view: &View) -> Option<Command> {
    let screen = ui.max_rect();
    let painter = ui.painter_at(screen);
    let block = Block::new(screen, view.skip_buttons);
    let background =
        (!view.capture).then(|| ui.interact(screen, Id::unique("spotify-layout"), Sense::click()));
    let command = paint(ui, &painter, screen, &block, view);
    command.or_else(|| {
        background
            .filter(|r| r.clicked())
            .map(|_| Command::TogglePause)
    })
}

fn paint(
    ui: &egui::Ui,
    painter: &Painter,
    screen: Rect,
    block: &Block,
    view: &View,
) -> Option<Command> {
    let (w, h) = (screen.width(), screen.height());
    let origin = screen.min;
    let player = view.player;
    let scheme = view.scheme();
    let id = Id::unique("spotify-controls");

    painter.rect_filled(screen, 0.0, BACKGROUND);

    // Artist image, or the blurred album art when there is none or it is turned off.
    if backdrop::uses_artist_image(view) {
        let pan = view.motion.then(|| ui.input(|i| i.time));
        backdrop::paint_artist_image(painter, screen, &view.art, BACKGROUND_IMAGE_TINT, pan);
    } else {
        paint_art_backdrop(ui, painter, screen, view);
    }
    backdrop::paint_export_grain(painter, screen, view);

    // Album art
    let art_rect = block.art;
    apple::paint_art(painter, art_rect, view, ART_RADIUS);

    // Title and artist beside the art, clear of the play button.
    let (title, artist) = match &player.current {
        Some(song) => (song.title.as_str(), song.artist.as_str()),
        None => ("Nothing playing", ""),
    };
    let gap = w.min(h) * 0.03;
    let full = block.play.right() - block.title.x;
    let beside_button = block.play.left() - gap - block.title.x;
    let fit = |text, size, width, color| fit_text(painter, text, font_bold, size, width, color);
    let mut title_text = fit(title, block.title_size, full, scheme.text);
    if block.title.y + title_text.size().y > block.play.top() - gap {
        title_text = fit(title, block.title_size, beside_button, scheme.text);
    }
    let artist_pos = block.title + Vec2::new(0.0, title_text.size().y + art_rect.height() * 0.05);
    painter.galley(block.title, title_text, scheme.text);
    let artist_text = fit(
        artist,
        block.title_size / 2.0,
        beside_button,
        scheme.subtext,
    );
    painter.galley(artist_pos, artist_text, scheme.subtext);

    let command = paint_controls(ui, painter, block, view, &scheme, id);

    // Up next card in the top-right corner, in the clock's place.
    let corner = Pos2::new(screen.right() - w / 30.0, origin.y + h / 20.0);
    let card = UpNextCard::new(screen);
    let card_t = paint_up_next(ui, painter, &card, corner, view, &scheme);

    let clock_font = font_bold(h / 22.0);
    if view.show_clock && !view.capture && card_t < 1.0 {
        painter.text(
            corner,
            Align2::RIGHT_TOP,
            view.now.format("%H:%M").to_string(),
            clock_font.clone(),
            with_alpha(scheme.text, 1.0 - card_t),
        );
    }

    // Lyrics in the upper part of the screen, aligned as the view asks. Where they reach under
    // the top-right corner they stay below the clock, and move down below the Up next block
    // while it shows.
    let clock_h = painter
        .layout_no_wrap("0".into(), clock_font, Color32::WHITE)
        .size()
        .y;
    let x_range = lyrics_x_range(screen, art_rect, view.lyrics_align);
    let under_corner = x_range.max > corner.x - card.width - w * 0.02;
    let top = if under_corner {
        corner.y + clock_h + h * 0.06
    } else {
        origin.y + h * 0.06
    };
    let bottom = art_rect.top() - h * 0.03;
    let card_t = if under_corner { card_t } else { 0.0 };
    let below_card = (corner.y + card.height() + h * 0.06 + bottom) / 2.0;
    let center =
        lerp(origin.y + h * LYRICS_CENTER, below_card, ease(card_t)).clamp(top, bottom.max(top));
    if bottom > top && x_range.span() > 0.0 {
        // The Apple layout's lyric size at the same window size.
        let base = (h / 22.0).min(x_range.span() / 14.0);
        let card_bottom = corner.y + card.height() + h * 0.04;
        let area = Rect::from_x_y_ranges(
            x_range,
            lerp(top, card_bottom, ease(card_t)).min(bottom)..=bottom,
        );
        paint_lyrics(ui, painter, area, center, base, view, scheme.text);
    }
    command
}

/// The previous and next buttons either side of the wavy seek bar, and the play/pause
/// button. Returns the command a click asked for; decoration only when capturing.
fn paint_controls(
    ui: &egui::Ui,
    painter: &Painter,
    block: &Block,
    view: &View,
    scheme: &Scheme,
    id: Id,
) -> Option<Command> {
    let player = view.player;
    let playing = player.state == PlayState::Playing;
    let ctx = ui.ctx();
    let duration = player
        .current
        .as_ref()
        .and_then(|s| s.duration)
        .unwrap_or_default();
    let mut command = None;

    // Seek bar
    let wave = ease(ctx.animate_bool_with_time(id.with("wave"), playing, WAVE_FADE));
    let mut style = block.wavy(wave, ui.input(|i| i.time), false);
    let range = style.range(block.bar);
    let progress = player.progress();
    let shown = if view.capture {
        progress
    } else {
        let pad = Vec2::new(0.0, block.icon * 0.2);
        let bar = seek_input(
            ui,
            id.with("seek"),
            range,
            pad,
            progress,
            !duration.is_zero(),
        );
        style.pressed = bar.scrubbing;
        if let Some(fraction) = bar.seek {
            command = Some(Command::SeekTo(duration.mul_f32(fraction)));
        }
        bar.shown
    };
    let colors = BarColors {
        highlight: scheme.primary,
        track: scheme.secondary_container,
        handle: scheme.primary,
        dot: scheme.on_secondary_container,
    };
    wavy::paint(painter, block.bar, shown, &style, &colors);

    // Previous and next
    let skip_buttons = [
        (
            Icon::Previous,
            block.previous,
            Command::Previous,
            "previous",
        ),
        (Icon::Next, block.next, Command::Next, "next"),
    ];
    for (icon, rect, cmd, key) in skip_buttons.into_iter().filter(|_| block.skip_buttons) {
        let hover = hover_button(ui, view, rect, id.with(key), cmd, &mut command);
        if hover > 0.0 {
            painter.circle_filled(
                rect.center(),
                rect.width() * 0.6,
                with_alpha(scheme.secondary_container, hover * 0.8),
            );
        }
        paint_icon(
            painter,
            icon,
            rect.center(),
            block.icon,
            scheme.on_secondary_container,
        );
    }

    // Play/pause: the primary colour while playing, the secondary container while paused.
    let morph = ctx.animate_bool_with_time(id.with("play-morph"), playing, BUTTON_MORPH);
    let hover = hover_button(
        ui,
        view,
        block.play,
        id.with("play"),
        Command::TogglePause,
        &mut command,
    );
    let fill = scheme
        .secondary_container
        .lerp_to_gamma(scheme.primary, morph)
        .lerp_to_gamma(Color32::WHITE, 0.1 * hover);
    let icon_color = scheme
        .on_secondary_container
        .lerp_to_gamma(scheme.on_primary, morph);
    painter.rect_filled(block.play, block.play.width() * 0.36, fill);
    let icon = if playing { Icon::Pause } else { Icon::Play };
    paint_icon(
        painter,
        icon,
        block.play.center(),
        block.play.width() * 0.5,
        icon_color,
    );
    command
}

/// A clickable area that sets `command` to `cmd` when clicked. Returns its eased hover amount;
/// 0 when capturing.
fn hover_button(
    ui: &egui::Ui,
    view: &View,
    rect: Rect,
    id: Id,
    cmd: Command,
    command: &mut Option<Command>,
) -> f32 {
    if view.capture {
        return 0.0;
    }
    let response = ui
        .interact(rect, id, Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    if response.clicked() {
        *command = Some(cmd);
    }
    ui.ctx()
        .animate_bool_with_time(id.with("hover"), response.hovered(), 0.15)
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
    /// Scroll position at time `now`, for a scroll lasting `secs`.
    pub fn at(self, now: f64, secs: f64) -> f32 {
        let t = ((now - self.since) / secs) as f32;
        lerp(self.from, self.to, ease(t))
    }

    /// Scroll speed at time `now` in lines per second, for a scroll lasting `secs`.
    pub fn speed(self, now: f64, secs: f64) -> f32 {
        const DT: f64 = 0.01;
        (self.at(now, secs) - self.at(now - DT, secs)) / DT as f32
    }

    /// Retargets a scroll lasting `secs` to `to` at `now`, continuing from where it is. Jumps
    /// of more than a couple of lines, as after a seek, only animate the last line.
    pub fn retarget(self, to: f32, now: f64, secs: f64) -> Self {
        if to == self.to {
            return self;
        }
        let here = self.at(now, secs);
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

/// Horizontal extent of the lyrics, from the side margin to the middle of the screen so lines
/// never cross it: the left half from the art's left edge, the right half up to the play
/// button's right edge, or as wide centred over the space right of the art.
fn lyrics_x_range(screen: Rect, art: Rect, align: LyricsAlign) -> egui::Rangef {
    let margin = screen.width() / 16.0;
    let (left, right, middle) = (
        screen.left() + margin,
        screen.right() - margin,
        screen.center().x,
    );
    let width = (middle - left).max(0.0);
    match align {
        LyricsAlign::Left => egui::Rangef::new(left, middle),
        LyricsAlign::Right => egui::Rangef::new(middle, right),
        LyricsAlign::Center => {
            let center = (art.right() + right) / 2.0;
            egui::Rangef::new(center - width / 2.0, center + width / 2.0)
        }
    }
}

/// Text alignment within the lyrics' extent.
fn lyrics_halign(align: LyricsAlign) -> Align {
    match align {
        LyricsAlign::Left => Align::Min,
        LyricsAlign::Center => Align::Center,
        LyricsAlign::Right => Align::Max,
    }
}

/// Lays out `text` aligned by `halign` on x = 0, wrapped at `width`.
fn aligned(painter: &Painter, text: &str, font: FontId, width: f32, halign: Align) -> Arc<Galley> {
    let mut job = LayoutJob::simple(text.to_owned(), font, Color32::WHITE, width);
    job.halign = halign;
    painter.layout_job(job)
}

/// Opacity of a stacked entry `distance` lines below the scroll position (negative above it):
/// full on the active line, fading over the three below it, and gone once it has moved a line
/// up.
pub fn stack_alpha(distance: f32) -> f32 {
    if distance <= -1.0 {
        0.0
    } else if distance < 0.0 {
        (1.0 + distance).powi(2)
    } else {
        let i = distance.floor() as usize;
        match (STACK_ALPHA.get(i), STACK_ALPHA.get(i + 1)) {
            (Some(&a), Some(&b)) => lerp(a, b, distance.fract()),
            _ => 0.0,
        }
    }
}

/// The eased scroll stored under `id`, retargeted to `target` at `now`; requests a repaint
/// while it moves.
fn scroll_to(ctx: &egui::Context, id: Id, target: f32, now: f64, secs: f64) -> Scroll {
    let scroll = ctx.data(|d| d.get_temp::<Scroll>(id)).map_or(
        Scroll {
            from: target,
            to: target,
            since: now,
        },
        |s| s.retarget(target, now, secs),
    );
    ctx.data_mut(|d| d.insert_temp(id, scroll));
    if scroll.at(now, secs) != target {
        ctx.request_repaint();
    }
    scroll
}

/// X position of galleys aligned by `halign` in `rect`.
fn aligned_x(rect: Rect, halign: Align) -> f32 {
    match halign {
        Align::Min => rect.left(),
        Align::Center => rect.center().x,
        Align::Max => rect.right(),
    }
}

/// The lyrics in `color` within `area`, aligned as the view asks: three lines around height
/// `center`, or synced lyrics in the stacked flow hanging from the top. `base` is the active
/// line's font size.
fn paint_lyrics(
    ui: &egui::Ui,
    painter: &Painter,
    area: Rect,
    center: f32,
    base: f32,
    view: &View,
    color: Color32,
) {
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
    let id = Id::unique("spotify-lyrics").with(&song.file);
    let halign = lyrics_halign(view.lyrics_align);
    let lines = Lines {
        entries: &entries,
        active,
        base,
        halign,
        color,
    };
    if synced && view.lyrics_flow == LyricsFlow::Stacked {
        paint_stacked(ui, painter, area, &lines, id.with("stacked"));
    } else {
        paint_classic(ui, painter, area, center, &lines, synced, id);
    }
}

/// Lyric entries to draw and how.
struct Lines<'a> {
    entries: &'a [String],
    active: usize,
    /// Font size of the active line.
    base: f32,
    halign: Align,
    color: Color32,
}

/// Three lines around height `center`: the previous one dimmed above the active one and the
/// next one below, scrolling up as the song moves on. Wrapped lines that would leave `rect`
/// move the three back inside it. `synced` lyrics start with a gap marker; the scroll is kept
/// under `id`.
fn paint_classic(
    ui: &egui::Ui,
    painter: &Painter,
    rect: Rect,
    center: f32,
    lines: &Lines,
    synced: bool,
    id: Id,
) {
    let Lines {
        entries,
        active,
        base,
        halign,
        color,
    } = *lines;
    let now = ui.ctx().input(|i| i.time);
    let position =
        scroll_to(ui.ctx(), id, active as f32, now, LYRICS_SCROLL).at(now, LYRICS_SCROLL);
    let x = aligned_x(rect, halign);

    // Entries around the position, stacked from the top with their sizes for this frame.
    let first = (position.floor() as isize - 2).max(0) as usize;
    let last = (position.ceil() as usize + 2).min(entries.len() - 1);
    let gap = base * 0.75;
    let mut y = 0.0;
    let mut laid_out = Vec::with_capacity(last + 1 - first);
    for (i, text) in entries.iter().enumerate().take(last + 1).skip(first) {
        let distance = i as f32 - position;
        let galley = aligned(
            painter,
            text,
            font_bold(base * trio_scale(distance)),
            rect.width(),
            halign,
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

    // Keep the visible lines inside the area; when they do not fit, the bottom wins and lines
    // above the area fade out.
    let visible = laid_out
        .iter()
        .filter(|(distance, ..)| trio_alpha(*distance) > 0.05)
        .map(|(_, top, g)| (center + top - here, center + top - here + g.size().y));
    let (ext_top, ext_bottom) = visible.fold((f32::MAX, f32::MIN), |(t, b), (top, bottom)| {
        (t.min(top), b.max(bottom))
    });
    let shift = if ext_top <= ext_bottom {
        (rect.top() - ext_top)
            .max(0.0)
            .min(rect.bottom() - ext_bottom)
    } else {
        0.0
    };
    let anchor = center + shift;
    for (i, (distance, top, galley)) in laid_out.into_iter().enumerate() {
        let mut a = trio_alpha(distance);
        // The marker before the first line only shows while it is the active entry.
        if synced && first + i == 0 && distance < 0.0 {
            a *= (1.0 + distance).max(0.0);
        }
        if a <= 0.0 {
            continue;
        }
        let pos = Pos2::new(x, anchor + top - here);
        let above = ((pos.y + galley.size().y - rect.top()) / galley.size().y).clamp(0.0, 1.0);
        let a = a * above * above;
        if a > 0.0 {
            painter.galley_with_override_text_color(pos, galley, with_alpha(color, a));
        }
    }
}

/// Synced lyrics hanging from the top of `rect`: the active line brightest, the three after
/// it fading out below. On a new line the stack slides up, the outgoing line fading as
/// it leaves and the lines smearing while they move.
fn paint_stacked(ui: &egui::Ui, painter: &Painter, rect: Rect, lines: &Lines, id: Id) {
    let Lines {
        entries,
        active,
        base,
        halign,
        color,
    } = *lines;
    let now = ui.ctx().input(|i| i.time);
    let scroll = scroll_to(ui.ctx(), id, active as f32, now, STACK_SCROLL);
    let position = scroll.at(now, STACK_SCROLL);
    let smear = (scroll.speed(now, STACK_SCROLL).abs() * SMEAR).min(MAX_SMEAR) * base;
    let x = aligned_x(rect, halign);

    let first = (position.floor() as isize - 1).max(0) as usize;
    let last = (position.floor().max(0.0) as usize + STACK_ALPHA.len()).min(entries.len() - 1);
    let gap = base * 0.5;
    let mut y = 0.0;
    let mut laid_out = Vec::with_capacity(last + 1 - first);
    for (i, text) in entries.iter().enumerate().take(last + 1).skip(first) {
        let distance = i as f32 - position;
        let size = base * trio_scale(distance);
        let galley = aligned(painter, text, font_bold(size), rect.width(), halign);
        let height = galley.size().y;
        laid_out.push((distance, y, galley));
        y += height + gap;
    }
    let top_of = |i: usize| laid_out.get(i.checked_sub(first)?).map(|(_, top, _)| *top);
    let below = position.floor().max(0.0) as usize;
    let here = match (top_of(below), top_of(below + 1)) {
        (Some(a), Some(b)) => lerp(a, b, position - below as f32),
        (Some(a), None) => a,
        _ => 0.0,
    };

    // Lines leave upwards past the top and fade out towards the bottom.
    let clip = Rect::from_x_y_ranges(
        painter.clip_rect().x_range(),
        rect.top() - base * 3.0..=rect.bottom(),
    );
    let painter = painter.with_clip_rect(clip.intersect(painter.clip_rect()));
    let fade_band = base * 1.5;
    for (distance, top, galley) in laid_out {
        let y = rect.top() + top - here;
        let center = y + galley.size().y / 2.0;
        let fade = ((rect.bottom() - center) / fade_band).clamp(0.0, 1.0);
        let a = stack_alpha(distance) * fade * fade * (3.0 - 2.0 * fade);
        if a > 0.0 {
            let pos = Pos2::new(x, y);
            paint_smeared_galley(&painter, pos, &galley, with_alpha(color, a), smear);
        }
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

/// Sizes of the Up next block: the queued song's cover with its title and artist beside it,
/// and a countdown bar below, drawn straight over the backdrop.
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
            thumb: unit * 0.075,
            unit,
        }
    }

    fn bar_height(&self) -> f32 {
        (self.unit * 0.004).max(2.0)
    }

    fn height(&self) -> f32 {
        self.thumb + self.pad * 0.7 + self.bar_height()
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

/// The queued song, sliding in from the right with its top-right corner at
/// `corner` for the end of the song and out once it changes. Returns how far it is shown.
fn paint_up_next(
    ui: &egui::Ui,
    painter: &Painter,
    card: &UpNextCard,
    corner: Pos2,
    view: &View,
    scheme: &Scheme,
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
    let bar_h = card.bar_height();
    let offset = slide_offset(t, width + (painter.clip_rect().right() - corner.x));
    let left = corner.x - width + offset;

    let mut painter = painter.clone();
    painter.multiply_opacity(ease(t));

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
            painter.rect_filled(thumb_rect, thumb_radius, with_alpha(scheme.text, 0.12));
        }
    }

    let text_x = thumb_rect.right() + pad * 0.8;
    let text_w = left + width - text_x;
    let title = one_row(&painter, &song.title, font_bold(unit * 0.028), text_w);
    let artist = one_row(&painter, &song.artist, font_bold(unit * 0.021), text_w);
    let gap = unit * 0.005;
    let text_h = title.size().y + gap + artist.size().y;
    let text_top = thumb_rect.center().y - text_h / 2.0;
    let artist_top = text_top + title.size().y + gap;
    painter.galley_with_override_text_color(Pos2::new(text_x, text_top), title, scheme.text);
    painter.galley_with_override_text_color(Pos2::new(text_x, artist_top), artist, scheme.subtext);

    let track = Rect::from_min_size(
        Pos2::new(left, thumb_rect.bottom() + pad * 0.7),
        Vec2::new(width, bar_h),
    );
    let bar_radius = (bar_h / 2.0) as u8;
    painter.rect_filled(track, bar_radius, with_alpha(scheme.text, 0.2));
    let fill = Rect::from_min_size(track.min, Vec2::new(track.width() * countdown, bar_h));
    // A light shade of the art's primary colour.
    let fill_color = mix(scheme.primary, scheme.text, 0.5);
    painter.rect_filled(fill, bar_radius, fill_color);
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
        .retarget(3.0, 10.0, LYRICS_SCROLL);
        let at = |t| s.at(t, LYRICS_SCROLL);
        assert_eq!(at(10.0), 2.0);
        assert!((at(10.0 + LYRICS_SCROLL / 2.0) - 2.5).abs() < 1e-4);
        assert_eq!(at(10.0 + LYRICS_SCROLL), 3.0);
        assert_eq!(at(100.0), 3.0);
        // Moving fastest midway, still once settled.
        assert!(s.speed(10.0 + LYRICS_SCROLL / 2.0, LYRICS_SCROLL) > 1.0);
        assert_eq!(s.speed(100.0, LYRICS_SCROLL), 0.0);
        // Retargeting midway continues from the current position.
        let mid = s.retarget(4.0, 10.0 + LYRICS_SCROLL / 2.0, LYRICS_SCROLL);
        assert!((mid.from - 2.5).abs() < 1e-4);
        // Long jumps only animate the last line.
        let jump = s.retarget(20.0, 50.0, LYRICS_SCROLL);
        assert_eq!(jump.from, 19.0);
        let back = s.retarget(0.0, 50.0, LYRICS_SCROLL);
        assert_eq!(back.from, 1.0);
        assert_eq!(s.retarget(3.0, 50.0, LYRICS_SCROLL), s);
    }

    #[test]
    fn stacked_lines_fade_downwards_and_leave_upwards() {
        assert_eq!(stack_alpha(0.0), 1.0);
        assert!((stack_alpha(1.0) - 0.55).abs() < 1e-6);
        assert!((stack_alpha(2.0) - 0.35).abs() < 1e-6);
        assert!((stack_alpha(3.0) - 0.15).abs() < 1e-6);
        assert_eq!(stack_alpha(4.0), 0.0);
        assert_eq!(stack_alpha(9.0), 0.0);
        // The outgoing line is gone by the time it has moved up a line.
        assert!(stack_alpha(-0.5) > 0.0 && stack_alpha(-0.5) < 0.5);
        assert_eq!(stack_alpha(-1.0), 0.0);
        assert!(stack_alpha(1.5) < stack_alpha(1.0) && stack_alpha(1.5) > stack_alpha(2.0));
    }

    #[test]
    fn controls_line_up_with_the_art() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1920.0, 1080.0));
        let b = Block::new(screen, true);
        // A little larger than a third of the shorter side.
        assert!(b.art.width() > 1080.0 / 3.0 * 1.1 && b.art.width() < 1080.0 / 3.0 * 1.15);
        assert_eq!(b.play.bottom(), b.art.bottom());
        assert!(b.title.x > b.art.right() && b.title.y > b.art.top());
        // The controls row runs below the art across the screen, its ends at the margins.
        assert!(b.bar.top() > b.art.bottom());
        assert!(b.previous.right() < b.bar.left());
        assert!(b.bar.right() < b.next.left());
        assert!(b.next.right() > b.play.right());
        assert!(b.previous.left() < b.art.left());
        assert!(b.bar.width() > 1920.0 * 0.8);

        // Without the skip buttons the bar runs from margin to margin.
        let full = Block::new(screen, false);
        assert_eq!(full.bar.left(), 1920.0 / 16.0);
        assert_eq!(full.bar.right(), 1920.0 - 1920.0 / 16.0);
        assert_eq!(full.bar.left(), full.art.left());
        assert_eq!(full.bar.right(), full.play.right());
        assert_eq!(full.bar.y_range(), b.bar.y_range());
    }

    #[test]
    fn lyrics_align_to_the_art_and_the_play_button() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(1920.0, 1080.0));
        let b = Block::new(screen, true);
        let left = lyrics_x_range(screen, b.art, LyricsAlign::Left);
        assert_eq!(left.min, b.art.left());
        assert_eq!(left.max, 960.0);
        let right = lyrics_x_range(screen, b.art, LyricsAlign::Right);
        assert_eq!(right.max, b.play.right());
        assert_eq!(right.min, 960.0);
        let center = lyrics_x_range(screen, b.art, LyricsAlign::Center);
        assert!((center.span() - right.span()).abs() < 1e-3);
        assert!(center.min > b.art.right() && center.max < b.play.right());
        assert!((center.center() - (b.art.right() + b.play.right()) / 2.0).abs() < 1e-3);
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
