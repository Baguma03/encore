//! Dialog for choosing a clip range and formats to export, plus the range rules behind it.

use std::collections::HashMap;
use std::time::Duration;

use egui::epaint::{CornerRadius, Shadow};
use egui::{
    Align, Align2, Color32, CursorIcon, Frame, Id, Margin, Modal, Pos2, Rect, Response, RichText,
    ScrollArea, Sense, Stroke, Ui, Vec2,
};
use mpd_display_rs::lyrics::{LyricLine, Lyrics, LyricsState};

use super::{font_bold, font_light, with_alpha};
use crate::art;
use crate::config::Layout;
use crate::export::{Aspect, ExportRequest, Style};
use crate::player::SongInfo;

/// Longest clip that can be exported.
pub const MAX_CLIP: Duration = Duration::from_secs(180);
/// Clips longer than this get a warning.
pub const LONG_CLIP: Duration = Duration::from_secs(90);
/// Lead-in before the first picked line.
const LEAD_IN: Duration = Duration::from_millis(500);
/// Step of the start and end nudge buttons.
const NUDGE: Duration = Duration::from_millis(500);
/// Clip length after the last line when nothing follows it and the song length is unknown.
const LAST_LINE_TAIL: Duration = Duration::from_secs(5);
/// Clip length when only one end of the range is known.
const DEFAULT_CLIP: Duration = Duration::from_secs(15);
pub const EXPORT_FPS: u32 = 30;

const PANEL_WIDTH: f32 = 540.0;
const LIST_HEIGHT: f32 = 300.0;
const TIME_COLUMN: f32 = 52.0;
const AMBER: Color32 = Color32::from_rgb(240, 190, 100);

/// Clip range for lyric lines `first..=last`: from just before the first line to the start of
/// the line after the last one, the song end, or a few seconds past the last line.
pub fn range_for_lines(
    lines: &[LyricLine],
    first: usize,
    last: usize,
    song_duration: Option<Duration>,
) -> (Duration, Duration) {
    if lines.is_empty() {
        return (Duration::ZERO, song_duration.unwrap_or(LAST_LINE_TAIL));
    }
    let (first, last) = (first.min(last), first.max(last).min(lines.len() - 1));
    let first = first.min(last);
    let start = lines[first].time.saturating_sub(LEAD_IN);
    let end = match (lines.get(last + 1), song_duration) {
        (Some(next), _) => next.time,
        (None, Some(d)) => d,
        (None, None) => lines[last].time + LAST_LINE_TAIL,
    };
    let end = song_duration.map_or(end, |d| end.min(d));
    (start, end)
}

/// In and out points set while listening.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Marks {
    pub start: Option<Duration>,
    pub end: Option<Duration>,
}

impl Marks {
    /// Clip range from the marks; a missing end is filled in with a default length.
    pub fn range(&self, song_duration: Option<Duration>) -> Option<(Duration, Duration)> {
        let clamp = |d: Duration| song_duration.map_or(d, |max| d.min(max));
        let (start, end) = match (self.start, self.end) {
            (Some(a), Some(b)) => (a.min(b), a.max(b)),
            (Some(a), None) => (a, a + DEFAULT_CLIP),
            (None, Some(b)) => (b.saturating_sub(DEFAULT_CLIP), b),
            (None, None) => return None,
        };
        Some((clamp(start), clamp(end)))
    }
}

/// Picked lyric lines: a click sets the anchor, the next click (or a shift-click) the other end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub other: usize,
    pub awaiting_end: bool,
}

impl Selection {
    pub fn bounds(&self) -> (usize, usize) {
        (self.anchor.min(self.other), self.anchor.max(self.other))
    }

    fn contains(&self, i: usize) -> bool {
        let (a, b) = self.bounds();
        (a..=b).contains(&i)
    }
}

/// Selection after clicking line `index`, with `extend` when shift is held.
pub fn select(prev: Option<Selection>, index: usize, extend: bool) -> Selection {
    match prev {
        Some(s) if extend || s.awaiting_end => Selection {
            anchor: s.anchor,
            other: index,
            awaiting_end: false,
        },
        _ => Selection {
            anchor: index,
            other: index,
            awaiting_end: true,
        },
    }
}

/// Whether a clip can be exported, and what to tell the user about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Ready,
    /// Exportable, but long.
    Long,
    Empty,
    TooLong,
    NoFormat,
}

impl Verdict {
    pub fn of(start: Duration, end: Duration, formats: usize) -> Self {
        let len = end.saturating_sub(start);
        if end <= start {
            Self::Empty
        } else if len > MAX_CLIP {
            Self::TooLong
        } else if formats == 0 {
            Self::NoFormat
        } else if len > LONG_CLIP {
            Self::Long
        } else {
            Self::Ready
        }
    }

    pub fn can_export(self) -> bool {
        matches!(self, Self::Ready | Self::Long)
    }

    fn message(self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::Long => Some("Long clip — exporting takes a while."),
            Self::Empty => Some("The end must come after the start."),
            Self::TooLong => Some("Clips are limited to 3 minutes."),
            Self::NoFormat => Some("Pick at least one format."),
        }
    }
}

/// Formats a song position as `m:ss.t`.
pub fn fmt_precise(d: Duration) -> String {
    let tenths = (d.as_millis() + 50) / 100;
    let secs = tenths / 10;
    format!("{}:{:02}.{}", secs / 60, secs % 60, tenths % 10)
}

pub enum PanelResult {
    Open,
    Close,
    Export(Box<ExportRequest>),
}

pub struct ExportPanel {
    song: SongInfo,
    next: Option<SongInfo>,
    lyrics: LyricsState,
    selection: Option<Selection>,
    start: Duration,
    end: Duration,
    with_audio: bool,
    audio_found: bool,
    style: Style,
    motion: bool,
    /// The live artist background setting of each layout, picked again with each style.
    artist_bg_live: HashMap<Layout, bool>,
    artist_bg: bool,
    has_artist_image: bool,
    visualizer: bool,
    scroll_to: Option<usize>,
}

impl ExportPanel {
    /// Opens on the marked range if there is one, else on the active lyric line, else on a
    /// stretch starting at `elapsed`, with `style`, `motion` and `visualizer` picked and the
    /// artist background as `artist_bg` has it for the style's layout.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        song: SongInfo,
        next: Option<SongInfo>,
        lyrics: LyricsState,
        elapsed: Duration,
        marks: Marks,
        audio_found: bool,
        style: Style,
        motion: bool,
        artist_bg: HashMap<Layout, bool>,
        visualizer: bool,
    ) -> Self {
        let mut panel = Self {
            has_artist_image: art::has_artist_image(&song.artist),
            artist_bg: artist_bg.get(&style.layout()).copied().unwrap_or(false),
            artist_bg_live: artist_bg,
            song,
            next,
            lyrics: LyricsState::Loading,
            selection: None,
            start: Duration::ZERO,
            end: Duration::ZERO,
            with_audio: true,
            audio_found,
            style,
            motion,
            visualizer,
            scroll_to: None,
        };
        if let Some((start, end)) = marks.range(panel.song.duration) {
            panel.start = start;
            panel.end = end;
            panel.lyrics = lyrics;
            panel.scroll_to = panel
                .synced()
                .and_then(|lines| lines.partition_point(|l| l.time <= start).checked_sub(1));
        } else {
            let start = Duration::from_millis(elapsed.as_millis() as u64 / 500 * 500);
            panel.start = start;
            panel.end = panel
                .song
                .duration
                .map_or(start + DEFAULT_CLIP, |d| (start + DEFAULT_CLIP).min(d));
            panel.set_lyrics(lyrics, elapsed);
        }
        panel
    }

    pub fn song(&self) -> &SongInfo {
        &self.song
    }

    /// Takes lyrics that finished loading after the panel opened.
    pub fn refresh_lyrics(&mut self, lyrics: &LyricsState, elapsed: Duration) {
        if matches!(self.lyrics, LyricsState::Loading) && !matches!(lyrics, LyricsState::Loading) {
            self.set_lyrics(lyrics.clone(), elapsed);
        }
    }

    fn set_lyrics(&mut self, lyrics: LyricsState, elapsed: Duration) {
        self.lyrics = lyrics;
        if let LyricsState::Found(l) = &self.lyrics {
            if let Some(i) = l.current_line(elapsed) {
                self.pick(i, false);
                self.scroll_to = Some(i);
            }
        }
    }

    fn synced(&self) -> Option<&[LyricLine]> {
        match &self.lyrics {
            LyricsState::Found(l) => match l.as_ref() {
                Lyrics::Synced(lines) if !lines.is_empty() => Some(lines),
                _ => None,
            },
            _ => None,
        }
    }

    fn pick(&mut self, index: usize, extend: bool) {
        let sel = select(self.selection, index, extend);
        self.selection = Some(sel);
        if let Some(lines) = self.synced() {
            let (a, b) = sel.bounds();
            (self.start, self.end) = range_for_lines(lines, a, b, self.song.duration);
        }
    }

    fn request(&self, formats: &[Aspect]) -> ExportRequest {
        ExportRequest {
            song: self.song.clone(),
            next: self.next.clone(),
            lyrics: self.lyrics.clone(),
            start: self.start,
            end: self.end,
            formats: self.style.formats(formats),
            with_audio: self.with_audio,
            fps: EXPORT_FPS,
            style: self.style,
            motion: self.motion,
            artist_bg: self.artist_bg && self.has_artist_image,
            visualizer: self.visualizer && self.style == Style::Spotify,
        }
    }

    /// Draws the dialog over everything else; `formats` is the session's format choice.
    pub fn show(&mut self, ctx: &egui::Context, formats: &mut Vec<Aspect>) -> PanelResult {
        let frame = Frame::new()
            .fill(Color32::from_rgba_unmultiplied(20, 20, 24, 235))
            .stroke(Stroke::new(1.0, Color32::from_white_alpha(20)))
            .corner_radius(18)
            .inner_margin(Margin::same(24))
            .shadow(Shadow {
                offset: [0, 12],
                blur: 48,
                spread: 0,
                color: Color32::from_black_alpha(140),
            });
        let modal = Modal::new(Id::unique("export-panel"))
            .frame(frame)
            .backdrop_color(Color32::from_black_alpha(130))
            .show(ctx, |ui| {
                let width = PANEL_WIDTH
                    .min(ctx.content_rect().width() - 80.0)
                    .max(280.0);
                ui.set_width(width);
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                self.contents(ui, formats)
            });
        if modal.should_close() {
            return PanelResult::Close;
        }
        modal.inner
    }

    fn contents(&mut self, ui: &mut Ui, formats: &mut Vec<Aspect>) -> PanelResult {
        let mut result = PanelResult::Open;
        ui.label(
            RichText::new("Export clip")
                .font(font_bold(22.0))
                .color(Color32::WHITE),
        );
        let name = match (self.song.artist.is_empty(), self.song.title.is_empty()) {
            (false, false) => format!("{} — {}", self.song.artist, self.song.title),
            (true, _) => self.song.title.clone(),
            (false, true) => self.song.artist.clone(),
        };
        ui.add(egui::Label::new(muted(&name, 15.0)).truncate());
        ui.add_space(6.0);

        self.line_list(ui);
        ui.add_space(4.0);
        self.range_row(ui);
        ui.add_space(4.0);

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for aspect in Aspect::ALL {
                let enabled = self.style.supports(aspect);
                let on = if self.style == Style::Spotify {
                    enabled
                } else {
                    formats.contains(&aspect)
                };
                if chip(ui, aspect.display_name(), on, enabled).clicked() {
                    if on {
                        formats.retain(|&a| a != aspect);
                    } else {
                        formats.push(aspect);
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for style in Style::ALL {
                if chip(ui, style.display_name(), self.style == style, true).clicked() {
                    self.style = style;
                    self.artist_bg = self
                        .artist_bg_live
                        .get(&style.layout())
                        .copied()
                        .unwrap_or(false);
                }
            }
            ui.add_space(16.0);
            if switch(ui, &mut self.motion, true).clicked() {
                self.motion = !self.motion;
            }
            ui.label(
                RichText::new("Motion")
                    .font(font_light(15.0))
                    .color(Color32::WHITE),
            );
        });
        if self.style == Style::Spotify {
            ui.horizontal(|ui| {
                if switch(ui, &mut self.visualizer, true).clicked() {
                    self.visualizer = !self.visualizer;
                }
                ui.label(
                    RichText::new("Visualizer")
                        .font(font_light(15.0))
                        .color(Color32::WHITE),
                );
            });
        }
        ui.horizontal(|ui| {
            let mut on = self.artist_bg && self.has_artist_image;
            if switch(ui, &mut on, self.has_artist_image).clicked() {
                self.artist_bg = !self.artist_bg;
            }
            ui.label(
                RichText::new("Artist background")
                    .font(font_light(15.0))
                    .color(with_alpha(
                        Color32::WHITE,
                        if self.has_artist_image { 1.0 } else { 0.5 },
                    )),
            );
            if !self.has_artist_image {
                ui.label(muted("no artist image", 13.0));
            }
        });
        ui.horizontal(|ui| {
            if switch(ui, &mut self.with_audio, true).clicked() {
                self.with_audio = !self.with_audio;
            }
            ui.label(
                RichText::new("Include audio")
                    .font(font_light(15.0))
                    .color(Color32::WHITE),
            );
            if self.with_audio && !self.audio_found {
                ui.label(muted("no audio file found", 13.0));
            }
        });

        if self.style == Style::Spotify {
            ui.label(muted("Spotify style is 16:9 only", 13.0));
        }
        let verdict = Verdict::of(self.start, self.end, self.style.formats(formats).len());
        if let Some(msg) = verdict.message() {
            ui.label(
                RichText::new(msg)
                    .font(font_light(13.0))
                    .color(with_alpha(AMBER, 0.85)),
            );
        }
        ui.add_space(6.0);
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if pill(ui, "Export", true, verdict.can_export()).clicked() {
                result = PanelResult::Export(Box::new(self.request(formats)));
            }
            if pill(ui, "Cancel", false, true).clicked() {
                result = PanelResult::Close;
            }
        });
        result
    }

    fn line_list(&mut self, ui: &mut Ui) {
        let Some(lines) = self.synced() else {
            let text = match &self.lyrics {
                LyricsState::Loading => "Searching lyrics…",
                LyricsState::Found(_) => {
                    "Lyrics aren't time-synced — use [ and ] while listening to mark a range."
                }
                LyricsState::NotFound => "No lyrics — use [ and ] while listening to mark a range.",
            };
            ui.label(muted(text, 14.0));
            return;
        };
        let lines = lines.to_vec();
        let mut clicked = None;
        let scroll_to = self.scroll_to.take();
        Frame::new()
            .fill(Color32::from_white_alpha(6))
            .corner_radius(12)
            .inner_margin(Margin::same(6))
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .max_height(LIST_HEIGHT)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (i, line) in lines.iter().enumerate() {
                            let response = self.line_row(ui, i, line);
                            if scroll_to == Some(i) {
                                response.scroll_to_me(Some(Align::Center));
                            }
                            if response.clicked() {
                                clicked = Some(i);
                            }
                        }
                    });
            });
        if let Some(i) = clicked {
            let extend = ui.input(|i| i.modifiers.shift);
            self.pick(i, extend);
        }
        let hint = match self.selection {
            Some(s) if s.awaiting_end => "Click the last line of the clip, or the same line again.",
            _ => "Click a line to start the clip, then click its last line.",
        };
        ui.label(muted(hint, 13.0));
    }

    fn line_row(&self, ui: &mut Ui, i: usize, line: &LyricLine) -> Response {
        let width = ui.available_width();
        let pad = Vec2::new(10.0, 6.0);
        let selected = self.selection.filter(|s| s.contains(i));
        let text = match line.text.trim() {
            "" => "• • •",
            t => t,
        };
        let color = if selected.is_some() {
            Color32::WHITE
        } else {
            with_alpha(Color32::WHITE, 0.55)
        };
        let galley = ui.painter().layout(
            text.to_owned(),
            font_light(15.0),
            color,
            width - TIME_COLUMN - 2.0 * pad.x,
        );
        let height = galley.size().y + 2.0 * pad.y;
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::click());
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        let painter = ui.painter();
        if let Some(sel) = selected {
            let (a, b) = sel.bounds();
            let r = 10;
            let radius = CornerRadius {
                nw: if i == a { r } else { 0 },
                ne: if i == a { r } else { 0 },
                sw: if i == b { r } else { 0 },
                se: if i == b { r } else { 0 },
            };
            painter.rect_filled(rect, radius, Color32::from_white_alpha(26));
        } else if response.hovered() {
            painter.rect_filled(rect, 10, Color32::from_white_alpha(10));
        }
        painter.text(
            Pos2::new(rect.left() + pad.x, rect.top() + pad.y + 2.0),
            Align2::LEFT_TOP,
            super::fmt_time(line.time),
            font_light(12.0),
            with_alpha(Color32::WHITE, 0.4),
        );
        painter.galley(
            Pos2::new(rect.left() + pad.x + TIME_COLUMN, rect.top() + pad.y),
            galley,
            color,
        );
        response
    }

    fn range_row(&mut self, ui: &mut Ui) {
        let duration = self.song.duration;
        let clamp = |d: Duration| duration.map_or(d, |max| d.min(max));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(muted("Start", 14.0));
            ui.label(value(&fmt_precise(self.start)));
            if nudge(ui, "−").clicked() {
                self.start = self.start.saturating_sub(NUDGE);
            }
            if nudge(ui, "+").clicked() {
                self.start = clamp(self.start + NUDGE);
            }
            ui.add_space(14.0);
            ui.label(muted("End", 14.0));
            ui.label(value(&fmt_precise(self.end)));
            if nudge(ui, "−").clicked() {
                self.end = self.end.saturating_sub(NUDGE);
            }
            if nudge(ui, "+").clicked() {
                self.end = clamp(self.end + NUDGE);
            }
            ui.add_space(14.0);
            ui.label(muted("Length", 14.0));
            ui.label(value(&fmt_precise(self.end.saturating_sub(self.start))));
        });
    }
}

fn muted(text: &str, size: f32) -> RichText {
    RichText::new(text)
        .font(font_light(size))
        .color(with_alpha(Color32::WHITE, 0.55))
}

fn value(text: &str) -> RichText {
    RichText::new(text)
        .font(font_bold(15.0))
        .color(Color32::WHITE)
}

/// Clickable rect of `size` with a pointing-hand cursor, or hover-only when disabled.
fn hit(ui: &mut Ui, size: Vec2, enabled: bool) -> (Rect, Response) {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    let response = if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    };
    (rect, response)
}

/// Small square ± button.
fn nudge(ui: &mut Ui, text: &str) -> Response {
    let (rect, response) = hit(ui, Vec2::splat(26.0), true);
    let fill = if response.hovered() { 36 } else { 20 };
    let painter = ui.painter();
    painter.rect_filled(rect, 8, Color32::from_white_alpha(fill));
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        font_bold(16.0),
        Color32::WHITE,
    );
    response
}

/// Toggleable chip for a format or style, at least 64 points wide; dimmed and inert when not
/// `enabled`.
fn chip(ui: &mut Ui, text: &str, on: bool, enabled: bool) -> Response {
    let text_width = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font_bold(14.0), Color32::WHITE)
        .size()
        .x;
    let (rect, response) = hit(ui, Vec2::new((text_width + 24.0).max(64.0), 32.0), enabled);
    let mut painter = ui.painter().clone();
    if !enabled {
        painter.multiply_opacity(0.35);
    }
    if on {
        painter.rect_filled(rect, 16, with_alpha(Color32::WHITE, 0.92));
    } else {
        let fill = if enabled && response.hovered() {
            22
        } else {
            10
        };
        painter.rect_filled(rect, 16, Color32::from_white_alpha(fill));
        painter.rect_stroke(
            rect,
            16,
            Stroke::new(1.0, Color32::from_white_alpha(30)),
            egui::StrokeKind::Inside,
        );
    }
    let color = if on {
        Color32::from_rgb(20, 20, 24)
    } else {
        with_alpha(Color32::WHITE, 0.8)
    };
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        font_bold(14.0),
        color,
    );
    response
}

/// iOS-style on/off switch; the caller flips `on` when it is clicked. Dimmed and inert when
/// not `enabled`.
fn switch(ui: &mut Ui, on: &mut bool, enabled: bool) -> Response {
    let (rect, response) = hit(ui, Vec2::new(40.0, 24.0), enabled);
    let t = ui.ctx().animate_bool_with_time(response.id, *on, 0.15);
    let mut painter = ui.painter().clone();
    if !enabled {
        painter.multiply_opacity(0.35);
    }
    let off = Color32::from_white_alpha(40);
    let lit = with_alpha(Color32::WHITE, 0.9);
    painter.rect_filled(rect, 12, off.lerp_to_gamma(lit, t));
    let x = rect.left() + 12.0 + t * (rect.width() - 24.0);
    let knob = if t > 0.5 {
        Color32::from_rgb(20, 20, 24)
    } else {
        Color32::WHITE
    };
    painter.circle_filled(Pos2::new(x, rect.center().y), 9.0, knob);
    response
}

/// Rounded button: `accent` is white with dark text, otherwise a translucent ghost.
fn pill(ui: &mut Ui, text: &str, accent: bool, enabled: bool) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font_bold(15.0), Color32::WHITE);
    let size = Vec2::new(galley.size().x + 40.0, 38.0);
    let (rect, response) = hit(ui, size, enabled);
    let hovered = enabled && response.hovered();
    let (fill, color) = if accent {
        (
            with_alpha(Color32::WHITE, if hovered { 1.0 } else { 0.9 }),
            Color32::from_rgb(20, 20, 24),
        )
    } else {
        (
            Color32::from_white_alpha(if hovered { 30 } else { 16 }),
            Color32::WHITE,
        )
    };
    let alpha = if enabled { 1.0 } else { 0.35 };
    let painter = ui.painter();
    painter.rect_filled(rect, 19, with_alpha(fill, alpha));
    painter.galley_with_override_text_color(
        rect.center() - galley.size() / 2.0,
        galley,
        with_alpha(color, alpha),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn lines(spec: &[(u64, &str)]) -> Vec<LyricLine> {
        spec.iter()
            .map(|&(t, text)| LyricLine {
                time: ms(t),
                text: text.to_owned(),
            })
            .collect()
    }

    #[test]
    fn range_spans_picked_lines() {
        let l = lines(&[(10_000, "a"), (14_000, "b"), (20_000, "c"), (25_000, "d")]);
        let dur = Some(ms(60_000));
        assert_eq!(range_for_lines(&l, 0, 0, dur), (ms(9_500), ms(14_000)));
        assert_eq!(range_for_lines(&l, 1, 2, dur), (ms(13_500), ms(25_000)));
        // Reversed picks give the same range.
        assert_eq!(range_for_lines(&l, 2, 1, dur), (ms(13_500), ms(25_000)));
        // The last line runs to the end of the song.
        assert_eq!(range_for_lines(&l, 3, 3, dur), (ms(24_500), ms(60_000)));
    }

    #[test]
    fn range_ends_at_gap_lines() {
        let l = lines(&[(10_000, "a"), (12_000, ""), (30_000, "b")]);
        assert_eq!(
            range_for_lines(&l, 0, 0, Some(ms(60_000))),
            (ms(9_500), ms(12_000))
        );
    }

    #[test]
    fn range_without_duration() {
        let l = lines(&[(200, "a"), (4_000, "b")]);
        assert_eq!(range_for_lines(&l, 0, 0, None), (ms(0), ms(4_000)));
        assert_eq!(range_for_lines(&l, 1, 1, None), (ms(3_500), ms(9_000)));
    }

    #[test]
    fn range_clamps_to_song() {
        let l = lines(&[(10_000, "a"), (70_000, "b")]);
        assert_eq!(
            range_for_lines(&l, 0, 0, Some(ms(50_000))),
            (ms(9_500), ms(50_000))
        );
        assert_eq!(
            range_for_lines(&l, 0, 9, Some(ms(80_000))),
            (ms(9_500), ms(80_000))
        );
        assert_eq!(range_for_lines(&[], 0, 0, None), (ms(0), LAST_LINE_TAIL));
    }

    #[test]
    fn marks_range() {
        let m = |start, end| Marks { start, end };
        assert_eq!(m(None, None).range(None), None);
        assert_eq!(
            m(Some(ms(5_000)), Some(ms(9_000))).range(None),
            Some((ms(5_000), ms(9_000)))
        );
        assert_eq!(
            m(Some(ms(9_000)), Some(ms(5_000))).range(None),
            Some((ms(5_000), ms(9_000)))
        );
        assert_eq!(
            m(Some(ms(50_000)), None).range(Some(ms(60_000))),
            Some((ms(50_000), ms(60_000)))
        );
        assert_eq!(
            m(None, Some(ms(10_000))).range(None),
            Some((ms(0), ms(10_000)))
        );
    }

    #[test]
    fn selection_clicks() {
        let first = select(None, 4, false);
        assert_eq!(first.bounds(), (4, 4));
        assert!(first.awaiting_end);
        let second = select(Some(first), 2, false);
        assert_eq!(second.bounds(), (2, 4));
        assert!(!second.awaiting_end);
        // A third click starts over; shift-click moves the end instead.
        let third = select(Some(second), 7, false);
        assert_eq!(third.bounds(), (7, 7));
        let shifted = select(Some(second), 9, true);
        assert_eq!(shifted.bounds(), (4, 9));
        assert!(!shifted.awaiting_end);
    }

    #[test]
    fn verdicts() {
        let s = |n| Duration::from_secs(n);
        assert_eq!(Verdict::of(s(10), s(40), 1), Verdict::Ready);
        assert_eq!(Verdict::of(s(10), s(110), 1), Verdict::Long);
        assert_eq!(Verdict::of(s(0), s(180), 2), Verdict::Long);
        assert_eq!(Verdict::of(s(0), s(181), 2), Verdict::TooLong);
        assert_eq!(Verdict::of(s(10), s(10), 1), Verdict::Empty);
        assert_eq!(Verdict::of(s(10), s(5), 1), Verdict::Empty);
        assert_eq!(Verdict::of(s(10), s(20), 0), Verdict::NoFormat);
        assert!(Verdict::Long.can_export());
        assert!(!Verdict::NoFormat.can_export());
    }

    #[test]
    fn precise_times() {
        assert_eq!(fmt_precise(ms(64_500)), "1:04.5");
        assert_eq!(fmt_precise(ms(95_000)), "1:35.0");
        assert_eq!(fmt_precise(ms(59_960)), "1:00.0");
        assert_eq!(fmt_precise(ms(0)), "0:00.0");
    }
}

#[cfg(test)]
mod snapshot {
    use std::sync::Arc;

    use mpd_display_rs::lyrics::lrc::parse_lrc;

    use super::*;
    use crate::export::render::Renderer;
    use crate::player::{PlayState, PlayerState};
    use crate::ui::toasts::{Progress, Toast, Toasts};
    use crate::ui::{apple, ArtView, View};

    /// Renders the panel and toasts over the Apple layout to the PNG named by
    /// `EXPORT_PANEL_SNAPSHOT`, or `export-panel.png` in the temp folder.
    /// `EXPORT_PANEL_STYLE=spotify` picks the Spotify style instead of Vibrant.
    #[test]
    #[ignore = "needs a GPU"]
    fn panel_snapshot() {
        let out = std::env::var_os("EXPORT_PANEL_SNAPSHOT")
            .map_or_else(|| std::env::temp_dir().join("export-panel.png"), Into::into);
        let song = SongInfo {
            id: 1,
            file: "x.flac".into(),
            title: "All The Stars".into(),
            artist: "Kendrick Lamar & SZA".into(),
            album: "Black Panther".into(),
            duration: Some(Duration::from_secs(232)),
        };
        let lrc: String = (0..14)
            .map(|i| {
                let text = match i % 4 {
                    0 => "Love, let's talk about love",
                    1 => "Is it anything and everything you hoped for?",
                    2 => "",
                    _ => "Or do the feeling haunt you?",
                };
                format!("[00:{:02}.00]{text}\n", 20 + i * 3)
            })
            .collect();
        let lyrics = LyricsState::Found(Arc::new(parse_lrc(&lrc)));
        let mut panel = ExportPanel::new(
            song.clone(),
            None,
            lyrics.clone(),
            Duration::from_secs(30),
            Marks::default(),
            false,
            match std::env::var("EXPORT_PANEL_STYLE").as_deref() {
                Ok("spotify") => Style::Spotify,
                _ => Style::Vibrant,
            },
            true,
            HashMap::new(),
            true,
        );
        panel.pick(6, false);
        let mut formats = vec![Aspect::Portrait, Aspect::Square];
        let mut toasts = Toasts::default();
        toasts.push(
            Toast::new(
                "Saved: Kendrick Lamar & SZA - All The Stars (0m26-0m41) 9x16.mp4",
                Duration::from_secs(8),
            )
            .detail("Click to open the folder")
            .warning(Some("no audio: file not found".into())),
        );
        let progress = Progress {
            title: "Exporting 9:16 (1/2) · 42%".into(),
            fraction: 0.21,
            queued: 2,
        };
        let player = PlayerState::snapshot(song, Duration::from_secs(30), PlayState::Playing);

        let (w, h) = (1920u32, 1080u32);
        let mut r = Renderer::new([w, h]).expect("renderer");
        let mut pixels = Vec::new();
        for i in 0..15 {
            let px = r
                .render_frame(f64::from(i) / 30.0, 1.0 / 30.0, |ui| {
                    let view = View {
                        player: &player,
                        art: ArtView::default(),
                        next_cover: None,
                        artist_bg: false,
                        show_clock: true,
                        lyrics_enabled: true,
                        lyrics: lyrics.clone(),
                        now: chrono::Local::now(),
                        capture: false,
                        motion: false,
                        visualizer: None,
                    };
                    apple::draw(ui, &view);
                    let ctx = ui.ctx().clone();
                    panel.show(&ctx, &mut formats);
                    toasts.show(&ctx, Some(&progress));
                })
                .expect("frame");
            pixels = px.to_vec();
        }
        image::RgbaImage::from_raw(w, h, pixels)
            .expect("image")
            .save(&out)
            .expect("save");
    }
}
