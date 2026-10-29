//! Dialog for choosing a clip range, look and formats to export, plus the range rules behind it.

use std::collections::HashMap;
use std::time::Duration;

use egui::epaint::{CornerRadius, Shadow};
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Color32, CursorIcon, Frame, Id, Margin, Modal, Painter, Pos2, Rect, Response,
    RichText, ScrollArea, Sense, Stroke, Ui, Vec2,
};
use encore::lyrics::{LyricLine, Lyrics, LyricsState};

use super::fonts::DisplayFont;
use super::panel_theme::PanelTheme;
use super::{font_bold, font_light, with_alpha};
use crate::art;
use crate::config::{Layout, LyricsAlign, LyricsFlow};
use crate::export::{Aspect, ExportRequest, Quality, Style};
use crate::player::SongInfo;

/// Longest clip that can be exported.
pub const MAX_CLIP: Duration = Duration::from_secs(180);
/// Clips longer than this get a warning.
pub const LONG_CLIP: Duration = Duration::from_secs(90);
/// Shortest range the range slider and the nudge buttons leave.
pub const MIN_CLIP: Duration = Duration::from_millis(500);
/// Lead-in before the first picked line.
const LEAD_IN: Duration = Duration::from_millis(500);
/// Step of the start and end nudge buttons, and with Shift held.
const NUDGE: Duration = Duration::from_millis(500);
const NUDGE_SHIFT: Duration = Duration::from_secs(5);
/// Clip length after the last line when nothing follows it and the song length is unknown.
const LAST_LINE_TAIL: Duration = Duration::from_secs(5);
/// Clip length when only one end of the range is known.
const DEFAULT_CLIP: Duration = Duration::from_secs(15);
pub const EXPORT_FPS: u32 = 30;

const PANEL_WIDTH: f32 = 920.0;
/// Panels narrower than this stack the two columns.
const TWO_COLUMNS: f32 = 760.0;
/// Share of the width taken by the clip column.
const CLIP_SHARE: f32 = 0.54;
const COLUMN_GAP: f32 = 16.0;
const CARD_GAP: f32 = 12.0;
const CARD_PAD: f32 = 16.0;
const LIST_HEIGHT: f32 = 290.0;
const TIME_COLUMN: f32 = 48.0;
/// Width of the labels in front of chip rows.
const ROW_LABEL: f32 = 64.0;
pub(super) const PILL_HEIGHT: f32 = 38.0;
/// Radius of the range slider's handles, and how close a press must be to grab one.
const HANDLE_RADIUS: f32 = 8.0;
const HANDLE_REACH: f32 = 14.0;
/// Opacity of the scrim over the layout.
const BACKDROP: f32 = 130.0 / 255.0;

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

/// What a drag on the range slider moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grab {
    Start,
    End,
    /// The whole range, keeping its length.
    Middle,
}

/// The part of the range slider under `x`, given the handles' positions `start_x` and `end_x`:
/// a handle within `reach` (the nearer one when both are), the range between them, or nothing.
pub fn grab_at(x: f32, start_x: f32, end_x: f32, reach: f32) -> Option<Grab> {
    let (to_start, to_end) = ((x - start_x).abs(), (x - end_x).abs());
    if to_start <= reach || to_end <= reach {
        // Handles on top of each other: the start takes presses left of it, the end the rest.
        let start = to_start < to_end || (to_start == to_end && x < start_x);
        Some(if start { Grab::Start } else { Grab::End })
    } else if x > start_x && x < end_x {
        Some(Grab::Middle)
    } else {
        None
    }
}

/// What a press at `x` grabs: as [`grab_at`], or the nearer handle on the bare track.
pub fn pick_grab(x: f32, start_x: f32, end_x: f32, reach: f32) -> Grab {
    grab_at(x, start_x, end_x, reach).unwrap_or(if (x - start_x).abs() <= (x - end_x).abs() {
        Grab::Start
    } else {
        Grab::End
    })
}

/// The range `from` with `grab` moved by `delta` seconds, within a song of length `song`. The
/// handles stay at least [`MIN_CLIP`] and at most [`MAX_CLIP`] apart; moving the middle keeps
/// the length and stops at the ends of the song.
pub fn drag_range(
    from: (Duration, Duration),
    grab: Grab,
    delta: f64,
    song: Duration,
) -> (Duration, Duration) {
    let (start, end) = (from.0.as_secs_f64(), from.1.as_secs_f64());
    let (min, max, song) = (
        MIN_CLIP.as_secs_f64(),
        MAX_CLIP.as_secs_f64(),
        song.as_secs_f64(),
    );
    // The lower bound wins when the bounds cross.
    let fit = |v: f64, lo: f64, hi: f64| v.min(hi).max(lo);
    let (start, end) = match grab {
        Grab::Start => (fit(start + delta, (end - max).max(0.0), end - min), end),
        Grab::End => (
            start,
            fit(end + delta, start + min, (start + max).min(song)),
        ),
        Grab::Middle => {
            let len = end - start;
            let start = fit(start + delta, 0.0, song - len);
            (start, start + len)
        }
    };
    let at = |secs: f64| Duration::from_secs_f64(secs.max(0.0));
    (at(start), at(end))
}

/// Step of a nudge button, larger with Shift held.
pub fn nudge_step(shift: bool) -> Duration {
    if shift {
        NUDGE_SHIFT
    } else {
        NUDGE
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
            Self::NoFormat => Some("Pick at least one size."),
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

/// The display's live settings, which the panel starts from.
pub struct LiveLook {
    pub style: Style,
    pub motion: bool,
    /// The artist background setting of each layout.
    pub artist_bg: HashMap<Layout, bool>,
    pub tint: bool,
    pub skip_buttons: bool,
    pub lyrics_align: LyricsAlign,
    pub lyrics_flow: LyricsFlow,
    /// The display font, which the clip's text is set in.
    pub display_font: DisplayFont,
}

/// A drag on the range slider: what it holds, the range and the pointer's song position when
/// it started.
#[derive(Clone, Copy, Debug)]
struct Drag {
    grab: Grab,
    from: (Duration, Duration),
    pointer: f64,
}

pub struct ExportPanel {
    song: SongInfo,
    next: Option<SongInfo>,
    lyrics: LyricsState,
    selection: Option<Selection>,
    start: Duration,
    end: Duration,
    /// Length the range slider spans: the song, or a stretch past the range when unknown.
    span: Duration,
    drag: Option<Drag>,
    with_audio: bool,
    audio_found: bool,
    style: Style,
    motion: bool,
    /// The live artist background setting of each layout, picked again with each style.
    artist_bg_live: HashMap<Layout, bool>,
    artist_bg: bool,
    has_artist_image: bool,
    tint: bool,
    skip_buttons: bool,
    lyrics_align: LyricsAlign,
    lyrics_flow: LyricsFlow,
    display_font: DisplayFont,
    scroll_to: Option<usize>,
    /// Colours of the frame being drawn.
    theme: PanelTheme,
}

impl ExportPanel {
    /// Opens on the marked range if there is one, else on the active lyric line, else on a
    /// stretch starting at `elapsed`, with the look of `live` and the artist background as it
    /// is for the style's layout.
    pub fn new(
        song: SongInfo,
        next: Option<SongInfo>,
        lyrics: LyricsState,
        elapsed: Duration,
        marks: Marks,
        audio_found: bool,
        live: LiveLook,
    ) -> Self {
        let mut panel = Self {
            has_artist_image: art::has_artist_image(&song.artist),
            artist_bg: live
                .artist_bg
                .get(&live.style.layout())
                .copied()
                .unwrap_or(false),
            artist_bg_live: live.artist_bg,
            song,
            next,
            lyrics: LyricsState::Loading,
            selection: None,
            start: Duration::ZERO,
            end: Duration::ZERO,
            span: Duration::ZERO,
            drag: None,
            with_audio: true,
            audio_found,
            style: live.style,
            motion: live.motion,
            tint: live.tint,
            skip_buttons: live.skip_buttons,
            lyrics_align: live.lyrics_align,
            lyrics_flow: live.lyrics_flow,
            display_font: live.display_font,
            scroll_to: None,
            theme: PanelTheme::PLAIN,
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
        panel.span = panel
            .song
            .duration
            .filter(|d| !d.is_zero())
            .unwrap_or(panel.end + MAX_CLIP);
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

    /// Sets the range from the slider or a nudge; the next line click starts a new selection.
    fn set_range(&mut self, range: (Duration, Duration)) {
        if range != (self.start, self.end) {
            (self.start, self.end) = range;
            self.selection = None;
        }
    }

    fn request(&self, formats: &[Aspect], quality: Quality) -> ExportRequest {
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
            tint: self.tint,
            skip_buttons: self.skip_buttons,
            lyrics_align: self.lyrics_align,
            lyrics_flow: self.lyrics_flow,
            display_font: self.display_font.clone(),
            quality,
        }
    }

    /// Rough size of all files the export writes at `quality`.
    fn estimate_bytes(&self, formats: &[Aspect], quality: Quality) -> u64 {
        let secs = self.end.saturating_sub(self.start).as_secs_f64();
        let audio = self.with_audio && self.audio_found;
        self.style
            .formats(formats)
            .iter()
            .map(|a| quality.estimate_bytes(a.size(), secs, audio))
            .sum()
    }

    /// Draws the dialog over everything else in the colours of `theme`; `formats` and `quality`
    /// are the session's choices.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        theme: &PanelTheme,
        formats: &mut Vec<Aspect>,
        quality: &mut Quality,
    ) -> PanelResult {
        self.theme = *theme;
        theme.scope(ctx, || self.show_modal(ctx, formats, quality))
    }

    fn show_modal(
        &mut self,
        ctx: &egui::Context,
        formats: &mut Vec<Aspect>,
        quality: &mut Quality,
    ) -> PanelResult {
        let t = self.theme;
        let frame = Frame::new()
            .fill(t.surface)
            .stroke(Stroke::new(1.0, t.border))
            .corner_radius(20)
            .inner_margin(Margin::same(24))
            .shadow(Shadow {
                offset: [0, 12],
                blur: 48,
                spread: 0,
                color: Color32::from_black_alpha(140),
            });
        let modal = Modal::new(Id::unique("export-panel"))
            .frame(frame)
            .backdrop_color(with_alpha(t.scrim, BACKDROP))
            .show(ctx, |ui| {
                let width = PANEL_WIDTH
                    .min(ctx.content_rect().width() - 80.0)
                    .max(300.0);
                ui.set_width(width);
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 0.0);
                self.contents(ui, formats, quality)
            });
        if modal.should_close() {
            return PanelResult::Close;
        }
        modal.inner
    }

    fn contents(
        &mut self,
        ui: &mut Ui,
        formats: &mut Vec<Aspect>,
        quality: &mut Quality,
    ) -> PanelResult {
        self.header(ui);
        ui.add_space(16.0);
        let width = ui.available_width();
        if width < TWO_COLUMNS {
            self.clip_card(ui);
            ui.add_space(CARD_GAP);
            return self.settings_column(ui, formats, quality, None);
        }

        let clip_width = ((width - COLUMN_GAP) * CLIP_SHARE).round();
        let settings_width = width - clip_width - COLUMN_GAP;
        let mut result = PanelResult::Open;
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = COLUMN_GAP;
            let clip = ui
                .vertical(|ui| {
                    ui.set_width(clip_width);
                    self.clip_card(ui);
                })
                .response
                .rect;
            ui.vertical(|ui| {
                ui.set_width(settings_width);
                result = self.settings_column(ui, formats, quality, Some(clip.bottom()));
            });
        });
        result
    }

    /// "Export clip", then the song's title and artist.
    fn header(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Export clip")
                    .font(font_bold(22.0))
                    .color(self.theme.heading),
            );
            let name: Vec<&str> = [&self.song.title, &self.song.artist]
                .into_iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            if !name.is_empty() {
                let text = format!("·  {}", name.join("  ·  "));
                ui.add(egui::Label::new(muted(&self.theme, &text, 16.0)).truncate());
            }
        });
    }

    /// The look and output cards and the buttons. With `bottom`, the output card stretches so
    /// the buttons end there.
    fn settings_column(
        &mut self,
        ui: &mut Ui,
        formats: &mut Vec<Aspect>,
        quality: &mut Quality,
        bottom: Option<f32>,
    ) -> PanelResult {
        let t = self.theme;
        let mut result = PanelResult::Open;
        self.look_card(ui, formats);
        ui.add_space(CARD_GAP);
        let verdict = Verdict::of(self.start, self.end, self.style.formats(formats).len());
        let output_bottom = bottom.map(|b| b - PILL_HEIGHT - CARD_GAP);
        self.output_card(ui, formats, quality, verdict, output_bottom);
        ui.add_space(CARD_GAP);
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if pill(ui, &t, "Export", true, verdict.can_export()).clicked() {
                result = PanelResult::Export(Box::new(self.request(formats, *quality)));
            }
            if pill(ui, &t, "Cancel", false, true).clicked() {
                result = PanelResult::Close;
            }
        });
        result
    }

    fn clip_card(&mut self, ui: &mut Ui) {
        let t = self.theme;
        card(ui, &t, None, |ui| {
            section_heading(ui, &t, "CLIP");
            self.line_list(ui);
            ui.add_space(4.0);
            self.range_slider(ui);
            self.range_rows(ui);
        });
    }

    fn look_card(&mut self, ui: &mut Ui, formats: &mut Vec<Aspect>) {
        let t = self.theme;
        card(ui, &t, None, |ui| {
            section_heading(ui, &t, "LOOK");
            ui.horizontal(|ui| {
                row_label(ui, &t, "Style");
                for style in Style::ALL {
                    if chip(ui, &t, style.display_name(), self.style == style, true).clicked() {
                        self.style = style;
                        self.artist_bg = self
                            .artist_bg_live
                            .get(&style.layout())
                            .copied()
                            .unwrap_or(false);
                    }
                }
            });
            ui.horizontal(|ui| {
                row_label(ui, &t, "Size");
                for aspect in Aspect::ALL {
                    let enabled = self.style.supports(aspect);
                    let on = if self.style == Style::EncoreClassic {
                        enabled
                    } else {
                        formats.contains(&aspect)
                    };
                    if chip(ui, &t, aspect.display_name(), on, enabled).clicked() {
                        if on {
                            formats.retain(|&a| a != aspect);
                        } else {
                            formats.push(aspect);
                        }
                    }
                }
            });
            ui.horizontal(|ui| {
                row_label(ui, &t, "Lyrics");
                let enabled = self.style == Style::EncoreClassic;
                for align in LyricsAlign::ALL {
                    let on = self.lyrics_align == align;
                    let chip = chip(ui, &t, align.display_name(), on, enabled);
                    let chip = if enabled {
                        chip
                    } else {
                        chip.on_hover_text("Encore Classic style only")
                    };
                    if chip.clicked() {
                        self.lyrics_align = align;
                    }
                }
            });
            ui.horizontal(|ui| {
                row_label(ui, &t, "Flow");
                let enabled = self.style == Style::EncoreClassic;
                for flow in LyricsFlow::ALL {
                    let on = self.lyrics_flow == flow;
                    let chip = chip(ui, &t, flow.display_name(), on, enabled);
                    let chip = if enabled {
                        chip
                    } else {
                        chip.on_hover_text("Encore Classic style only")
                    };
                    if chip.clicked() {
                        self.lyrics_flow = flow;
                    }
                }
            });
            let cell = cell_width(ui);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0 * CELL_GAP;
                if switch_cell(ui, &t, "Motion", self.motion, true, cell).clicked() {
                    self.motion = !self.motion;
                }
                let on = self.artist_bg && self.has_artist_image;
                if switch_cell(ui, &t, "Artist background", on, self.has_artist_image, cell)
                    .clicked()
                {
                    self.artist_bg = !self.artist_bg;
                }
            });
            ui.horizontal(|ui| {
                if switch_cell(ui, &t, "Album colours", self.tint, true, cell).clicked() {
                    self.tint = !self.tint;
                }
            });
        });
    }

    fn output_card(
        &mut self,
        ui: &mut Ui,
        formats: &[Aspect],
        quality: &mut Quality,
        verdict: Verdict,
        bottom: Option<f32>,
    ) {
        let t = self.theme;
        card(ui, &t, bottom, |ui| {
            section_heading(ui, &t, "OUTPUT");
            let cell = cell_width(ui);
            ui.horizontal(|ui| {
                if switch_cell(ui, &t, "Audio", self.with_audio, true, cell).clicked() {
                    self.with_audio = !self.with_audio;
                }
            });
            ui.horizontal(|ui| {
                row_label(ui, &t, "Quality");
                for q in Quality::ALL {
                    let name = match q {
                        Quality::High => "High",
                        Quality::Small => "Small",
                    };
                    if chip(ui, &t, name, *quality == q, true)
                        .on_hover_text(q.display_name())
                        .clicked()
                    {
                        *quality = q;
                    }
                }
                let bytes = self.estimate_bytes(formats, *quality);
                if bytes > 0 {
                    ui.add_space(4.0);
                    ui.label(muted(&t, &format!("~{}", fmt_size(bytes)), 14.0));
                }
            });
            let mut notes = Vec::new();
            if self.style == Style::EncoreClassic {
                notes.push("Encore Classic style is 16:9 only.");
            }
            if !self.has_artist_image {
                notes.push("No artist image for this artist.");
            }
            if self.with_audio && !self.audio_found {
                notes.push("No audio file found — the clip will be silent.");
            }
            if !notes.is_empty() || verdict.message().is_some() {
                ui.add_space(-2.0);
                ui.spacing_mut().item_spacing.y = 4.0;
            }
            for note in notes {
                ui.label(muted(&t, note, 13.0));
            }
            if let Some(msg) = verdict.message() {
                ui.label(
                    RichText::new(msg)
                        .font(font_light(13.0))
                        .color(with_alpha(t.warning, 0.9)),
                );
            }
        });
    }

    /// The song's lyric lines in a fixed-height box, the lines in the clip highlighted; a
    /// message in their place without synced lyrics.
    fn line_list(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let box_height = LIST_HEIGHT + 8.0;
        let Some(lines) = self.synced() else {
            let text = match &self.lyrics {
                LyricsState::Loading => "Searching lyrics…",
                LyricsState::Found(_) => {
                    "These lyrics aren't time-synced.\nDrag the handles below, or mark a range \
                     with [ and ] while listening."
                }
                LyricsState::NotFound => {
                    "No lyrics found.\nDrag the handles below, or mark a range with [ and ] \
                     while listening."
                }
            };
            let (rect, _) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), box_height), Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, 12, t.layer(5));
            let mut job = LayoutJob::simple(
                text.to_owned(),
                font_light(14.0),
                with_alpha(t.heading, 0.5),
                rect.width() - 64.0,
            );
            job.halign = Align::Center;
            let galley = painter.layout_job(job);
            painter.galley(
                rect.center() - Vec2::new(0.0, galley.size().y / 2.0),
                galley,
                t.heading,
            );
            return;
        };
        let lines = lines.to_vec();
        let (start, end) = (self.start, self.end);
        let in_clip = |t: Duration| t >= start && t < end;
        let first = lines.iter().position(|l| in_clip(l.time));
        let last = lines.iter().rposition(|l| in_clip(l.time));
        let clip = first.zip(last);
        let mut clicked = None;
        let scroll_to = self.scroll_to.take();
        Frame::new()
            .fill(t.layer(5))
            .corner_radius(12)
            .inner_margin(Margin::same(4))
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .max_height(LIST_HEIGHT)
                    .min_scrolled_height(LIST_HEIGHT)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        for (i, line) in lines.iter().enumerate() {
                            let response = line_row(ui, &t, i, line, clip);
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
            Some(s) if s.awaiting_end => "Now click the clip's last line, or the same line again.",
            _ => "Click the first and last lines, or drag the handles.",
        };
        ui.add_space(-2.0);
        ui.label(muted(&t, hint, 12.5));
    }

    /// A bar over the whole song with a handle at each end of the clip and a tick at every
    /// lyric line. Handles and the range between them drag freely.
    fn range_slider(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let span = self.span.as_secs_f64().max(1.0);
        let label_font = font_light(12.0);
        let labels = [super::fmt_time(Duration::ZERO), super::fmt_time(self.span)];
        let label_width = labels
            .iter()
            .map(|text| {
                ui.painter()
                    .layout_no_wrap(text.clone(), label_font.clone(), t.heading)
                    .size()
                    .x
            })
            .fold(0.0, f32::max);
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), 34.0),
            Sense::click_and_drag(),
        );
        let cy = rect.top() + 13.0;
        let inset = label_width + 12.0 + HANDLE_RADIUS;
        let bar = Rect::from_x_y_ranges(
            rect.left() + inset..=rect.right() - inset,
            cy - 2.0..=cy + 2.0,
        );
        let x_of = |d: Duration| bar.left() + bar.width() * (d.as_secs_f64() / span) as f32;
        let secs_at = |x: f32| f64::from(((x - bar.left()) / bar.width()).clamp(0.0, 1.0)) * span;

        if !response.is_pointer_button_down_on() {
            self.drag = None;
        } else if let Some(p) = response.interact_pointer_pos() {
            let (sx, ex) = (x_of(self.start), x_of(self.end));
            let drag = match self.drag {
                Some(drag) => drag,
                None => {
                    let grab = pick_grab(p.x, sx, ex, HANDLE_REACH);
                    // A press on the bare track brings the nearer handle to it.
                    let pointer = match (grab_at(p.x, sx, ex, HANDLE_REACH), grab) {
                        (Some(_), _) => secs_at(p.x),
                        (None, Grab::Start) => self.start.as_secs_f64(),
                        (None, _) => self.end.as_secs_f64(),
                    };
                    Drag {
                        grab,
                        from: (self.start, self.end),
                        pointer,
                    }
                }
            };
            self.drag = Some(drag);
            let delta = secs_at(p.x) - drag.pointer;
            self.set_range(drag_range(drag.from, drag.grab, delta, self.span));
        }

        let (sx, ex) = (x_of(self.start), x_of(self.end));
        let hovered = response
            .hover_pos()
            .and_then(|p| grab_at(p.x, sx, ex, HANDLE_REACH));
        let active = self.drag.map(|d| d.grab).or(hovered);
        if response.hovered() || self.drag.is_some() {
            ui.ctx().set_cursor_icon(match (self.drag, active) {
                (Some(_), Some(Grab::Middle)) => CursorIcon::Grabbing,
                (None, Some(Grab::Middle)) => CursorIcon::Grab,
                (_, Some(_)) => CursorIcon::ResizeHorizontal,
                (_, None) => CursorIcon::PointingHand,
            });
        }

        let painter = ui.painter();
        let label_color = with_alpha(t.heading, 0.45);
        let [first, last] = labels;
        painter.text(
            Pos2::new(rect.left(), cy),
            Align2::LEFT_CENTER,
            first,
            label_font.clone(),
            label_color,
        );
        painter.text(
            Pos2::new(rect.right(), cy),
            Align2::RIGHT_CENTER,
            last,
            label_font,
            label_color,
        );
        painter.rect_filled(bar, 2, t.layer(28));
        for line in self.synced().unwrap_or_default() {
            if line.time > self.span {
                break;
            }
            let x = x_of(line.time);
            let inside = line.time >= self.start && line.time < self.end;
            let color = if inside { t.wash(150) } else { t.layer(60) };
            painter.line_segment(
                [Pos2::new(x, cy + 8.0), Pos2::new(x, cy + 13.0)],
                Stroke::new(1.0, color),
            );
        }
        let middle = active == Some(Grab::Middle);
        let range = Rect::from_x_y_ranges(sx..=ex.max(sx), bar.y_range())
            .expand2(Vec2::new(0.0, if middle { 1.0 } else { 0.0 }));
        painter.rect_filled(
            range,
            3,
            with_alpha(t.accent, if middle { 1.0 } else { 0.85 }),
        );
        for (grab, x) in [(Grab::Start, sx), (Grab::End, ex)] {
            let hot = ui.ctx().animate_bool_with_time(
                response.id.with(grab_key(grab)),
                active == Some(grab),
                0.12,
            );
            let center = Pos2::new(x, cy);
            let radius = HANDLE_RADIUS + 1.5 * hot;
            painter.circle_filled(
                center + Vec2::new(0.0, 1.0),
                radius + 2.5,
                Color32::from_black_alpha(60),
            );
            painter.circle_filled(center, radius, t.accent);
            painter.circle_filled(center, 2.5, with_alpha(t.on_accent, 0.6 + 0.4 * hot));
        }
    }

    /// Start and end times with nudge buttons, and the clip length.
    fn range_rows(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let step = nudge_step(ui.input(|i| i.modifiers.shift)).as_secs_f64();
        for grab in [Grab::Start, Grab::End] {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (label, at) = match grab {
                    Grab::Start => ("Start", self.start),
                    _ => ("End", self.end),
                };
                row_label(ui, &t, label);
                let (rect, _) = ui.allocate_exact_size(Vec2::new(70.0, 30.0), Sense::hover());
                ui.painter().text(
                    rect.left_center(),
                    Align2::LEFT_CENTER,
                    fmt_precise(at),
                    font_bold(15.0),
                    t.heading,
                );
                for (text, sign) in [("−", -1.0), ("+", 1.0)] {
                    let hint = "0.5 s · Shift: 5 s";
                    if nudge(ui, &t, text).on_hover_text(hint).clicked() {
                        let range = (self.start, self.end);
                        self.set_range(drag_range(range, grab, sign * step, self.span));
                    }
                }
                if grab == Grab::End {
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let len = self.end.saturating_sub(self.start);
                        ui.label(value(&t, &fmt_precise(len)));
                        ui.label(muted(&t, "Length", 14.0));
                    });
                }
            });
        }
    }
}

/// One lyric line with its time; lines `clip` (first, last) form one highlighted block.
fn line_row(
    ui: &mut Ui,
    t: &PanelTheme,
    i: usize,
    line: &LyricLine,
    clip: Option<(usize, usize)>,
) -> Response {
    let width = ui.available_width();
    let pad = Vec2::new(10.0, 6.0);
    let selected = clip.filter(|&(a, b)| (a..=b).contains(&i));
    let text = match line.text.trim() {
        "" => "• • •",
        t => t,
    };
    let color = if selected.is_some() {
        t.heading
    } else {
        with_alpha(t.heading, 0.55)
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
    if let Some((a, b)) = selected {
        let r = 10;
        let radius = CornerRadius {
            nw: if i == a { r } else { 0 },
            ne: if i == a { r } else { 0 },
            sw: if i == b { r } else { 0 },
            se: if i == b { r } else { 0 },
        };
        let fill = if response.hovered() { 34 } else { 26 };
        painter.rect_filled(rect, radius, t.wash(fill));
    } else if response.hovered() {
        painter.rect_filled(rect, 10, t.layer(10));
    }
    painter.text(
        Pos2::new(rect.left() + pad.x, rect.top() + pad.y + 2.0),
        Align2::LEFT_TOP,
        super::fmt_time(line.time),
        font_light(12.0),
        with_alpha(t.heading, 0.4),
    );
    painter.galley(
        Pos2::new(rect.left() + pad.x + TIME_COLUMN, rect.top() + pad.y),
        galley,
        color,
    );
    response
}

fn grab_key(grab: Grab) -> &'static str {
    match grab {
        Grab::Start => "start",
        Grab::End => "end",
        Grab::Middle => "middle",
    }
}

/// Formats a byte count as `12 MB`, or `0.8 MB` below 10 MB.
pub fn fmt_size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb < 10.0 {
        format!("{mb:.1} MB")
    } else {
        format!("{mb:.0} MB")
    }
}

pub(super) fn muted(t: &PanelTheme, text: &str, size: f32) -> RichText {
    RichText::new(text)
        .font(font_light(size))
        .color(with_alpha(t.heading, 0.55))
}

fn value(t: &PanelTheme, text: &str) -> RichText {
    RichText::new(text).font(font_bold(15.0)).color(t.heading)
}

/// A section card: a softly lighter rounded box. With `bottom`, it reaches down at least that far.
pub(super) fn card<R>(
    ui: &mut Ui,
    t: &PanelTheme,
    bottom: Option<f32>,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    Frame::new()
        .fill(t.layer(7))
        .stroke(Stroke::new(1.0, t.layer(12)))
        .corner_radius(14)
        .inner_margin(Margin::same(CARD_PAD as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(bottom) = bottom {
                ui.set_min_height(bottom - CARD_PAD - 1.0 - ui.max_rect().top());
            }
            ui.spacing_mut().item_spacing.y = 10.0;
            add(ui)
        })
        .inner
}

/// Small, spaced-out uppercase heading of a card.
pub(super) fn section_heading(ui: &mut Ui, t: &PanelTheme, text: &str) {
    let job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: font_bold(11.5),
            extra_letter_spacing: 1.5,
            color: with_alpha(t.heading, 0.45),
            ..Default::default()
        },
    );
    ui.add(egui::Label::new(job).selectable(false));
}

/// Muted label of fixed width in front of a row of controls.
fn row_label(ui: &mut Ui, t: &PanelTheme, text: &str) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ROW_LABEL, 30.0), Sense::hover());
    ui.painter().text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        text,
        font_light(14.0),
        with_alpha(t.heading, 0.55),
    );
}

/// Half the gap between two switch cells.
const CELL_GAP: f32 = 12.0;

/// Width of a switch cell when two share a row of `ui`.
fn cell_width(ui: &Ui) -> f32 {
    (ui.available_width() - 2.0 * CELL_GAP) / 2.0
}

/// Clickable rect of `size` with a pointing-hand cursor, or hover-only when disabled.
pub(super) fn hit(ui: &mut Ui, size: Vec2, enabled: bool) -> (Rect, Response) {
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

/// Side of a [`nudge`] button.
pub(super) const NUDGE_SIZE: f32 = 26.0;

/// Small square ± button.
fn nudge(ui: &mut Ui, t: &PanelTheme, text: &str) -> Response {
    let (rect, response) = hit(ui, Vec2::splat(NUDGE_SIZE), true);
    paint_nudge(ui.painter(), t, rect, text, response.hovered());
    response
}

/// Paints a ± button over `rect`.
pub(super) fn paint_nudge(
    painter: &Painter,
    t: &PanelTheme,
    rect: Rect,
    text: &str,
    hovered: bool,
) {
    let fill = if hovered { 36 } else { 20 };
    painter.rect_filled(rect, 8, t.wash(fill));
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        font_bold(16.0),
        t.heading,
    );
}

/// Toggleable chip for a format or style, at least 64 points wide; dimmed and inert when not
/// `enabled`.
pub(super) fn chip(ui: &mut Ui, t: &PanelTheme, text: &str, on: bool, enabled: bool) -> Response {
    let text_width = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font_bold(14.0), t.heading)
        .size()
        .x;
    let (rect, response) = hit(ui, Vec2::new((text_width + 24.0).max(64.0), 30.0), enabled);
    let mut painter = ui.painter().clone();
    if !enabled {
        painter.multiply_opacity(0.35);
    }
    if on {
        painter.rect_filled(rect, 15, with_alpha(t.accent, 0.92));
    } else {
        let fill = if enabled && response.hovered() {
            22
        } else {
            10
        };
        painter.rect_filled(rect, 15, t.layer(fill));
        painter.rect_stroke(
            rect,
            15,
            Stroke::new(1.0, t.layer(30)),
            egui::StrokeKind::Inside,
        );
    }
    let color = if on {
        t.on_accent
    } else {
        with_alpha(t.heading, 0.8)
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

/// A label with an iOS-style on/off switch at the right of a cell `width` wide; the whole cell
/// is clickable, and the caller flips the setting when it is clicked. Dimmed and inert when not
/// `enabled`.
pub(super) fn switch_cell(
    ui: &mut Ui,
    t: &PanelTheme,
    label: &str,
    on: bool,
    enabled: bool,
    width: f32,
) -> Response {
    let (rect, response) = hit(ui, Vec2::new(width, 28.0), enabled);
    let on_t = ui.ctx().animate_bool_with_time(response.id, on, 0.15);
    let mut painter = ui.painter().clone();
    if !enabled {
        painter.multiply_opacity(0.4);
    }
    let hover = enabled && response.hovered();
    painter.text(
        rect.left_center(),
        Align2::LEFT_CENTER,
        label,
        font_light(15.0),
        with_alpha(t.heading, if hover { 1.0 } else { 0.9 }),
    );
    let track = Rect::from_min_size(
        Pos2::new(rect.right() - 40.0, rect.center().y - 12.0),
        Vec2::new(40.0, 24.0),
    );
    let off = t.layer(if hover { 56 } else { 40 });
    let lit = with_alpha(t.accent, 0.9);
    painter.rect_filled(track, 12, off.lerp_to_gamma(lit, on_t));
    let x = track.left() + 12.0 + on_t * (track.width() - 24.0);
    let knob = if on_t > 0.5 { t.on_accent } else { t.heading };
    painter.circle_filled(Pos2::new(x, track.center().y), 9.0, knob);
    response
}

/// Rounded button: `accent` is filled with the theme's accent, otherwise a translucent ghost.
pub(super) fn pill(
    ui: &mut Ui,
    t: &PanelTheme,
    text: &str,
    accent: bool,
    enabled: bool,
) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font_bold(15.0), t.heading);
    let size = Vec2::new(galley.size().x + 40.0, PILL_HEIGHT);
    let (rect, response) = hit(ui, size, enabled);
    let hovered = enabled && response.hovered();
    let (fill, color) = if accent {
        (
            with_alpha(t.accent, if hovered { 1.0 } else { 0.9 }),
            t.on_accent,
        )
    } else {
        (t.wash(if hovered { 30 } else { 16 }), t.heading)
    };
    let alpha = if enabled { 1.0 } else { 0.35 };
    let painter = ui.painter();
    painter.rect_filled(rect, PILL_HEIGHT / 2.0, with_alpha(fill, alpha));
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

    fn secs(n: f64) -> Duration {
        Duration::from_secs_f64(n)
    }

    #[test]
    fn dragging_a_handle_moves_one_end() {
        let song = secs(200.0);
        let range = (secs(10.0), secs(20.0));
        assert_eq!(
            drag_range(range, Grab::Start, -4.0, song),
            (secs(6.0), secs(20.0))
        );
        assert_eq!(
            drag_range(range, Grab::End, 7.5, song),
            (secs(10.0), secs(27.5))
        );
        // No snapping: any position is kept.
        assert_eq!(
            drag_range(range, Grab::Start, 0.25, song),
            (secs(10.25), secs(20.0))
        );
        // The ends of the song stop the handles.
        assert_eq!(
            drag_range(range, Grab::Start, -50.0, song),
            (Duration::ZERO, secs(20.0))
        );
        let late = (secs(150.0), secs(190.0));
        assert_eq!(drag_range(late, Grab::End, 30.0, song), (secs(150.0), song));
    }

    #[test]
    fn handles_never_cross() {
        let song = secs(200.0);
        let range = (secs(10.0), secs(20.0));
        assert_eq!(
            drag_range(range, Grab::Start, 30.0, song),
            (secs(20.0) - MIN_CLIP, secs(20.0))
        );
        assert_eq!(
            drag_range(range, Grab::End, -30.0, song),
            (secs(10.0), secs(10.0) + MIN_CLIP)
        );
    }

    #[test]
    fn handles_keep_the_three_minute_cap() {
        let song = secs(400.0);
        let range = (secs(100.0), secs(200.0));
        assert_eq!(
            drag_range(range, Grab::End, 150.0, song),
            (secs(100.0), secs(100.0) + MAX_CLIP)
        );
        assert_eq!(
            drag_range(range, Grab::Start, -150.0, song),
            (secs(200.0) - MAX_CLIP, secs(200.0))
        );
    }

    #[test]
    fn dragging_the_middle_keeps_the_length() {
        let song = secs(200.0);
        let range = (secs(10.0), secs(20.0));
        assert_eq!(
            drag_range(range, Grab::Middle, 12.5, song),
            (secs(22.5), secs(32.5))
        );
        assert_eq!(
            drag_range(range, Grab::Middle, -30.0, song),
            (Duration::ZERO, secs(10.0))
        );
        assert_eq!(
            drag_range(range, Grab::Middle, 500.0, song),
            (secs(190.0), song)
        );
    }

    #[test]
    fn nudges_step_further_with_shift() {
        assert_eq!(nudge_step(false), ms(500));
        assert_eq!(nudge_step(true), Duration::from_secs(5));
        let range = (secs(10.0), secs(20.0));
        let step = nudge_step(true).as_secs_f64();
        assert_eq!(
            drag_range(range, Grab::End, step, secs(200.0)),
            (secs(10.0), secs(25.0))
        );
        assert_eq!(
            drag_range(range, Grab::Start, -step, secs(200.0)),
            (secs(5.0), secs(20.0))
        );
    }

    #[test]
    fn presses_grab_handles_or_the_range() {
        let (s, e, reach) = (100.0, 300.0, 14.0);
        assert_eq!(grab_at(105.0, s, e, reach), Some(Grab::Start));
        assert_eq!(grab_at(290.0, s, e, reach), Some(Grab::End));
        assert_eq!(grab_at(200.0, s, e, reach), Some(Grab::Middle));
        assert_eq!(grab_at(50.0, s, e, reach), None);
        assert_eq!(grab_at(400.0, s, e, reach), None);
        // Bare track: the nearer handle.
        assert_eq!(pick_grab(50.0, s, e, reach), Grab::Start);
        assert_eq!(pick_grab(400.0, s, e, reach), Grab::End);
        // Handles on top of each other split by side.
        assert_eq!(grab_at(95.0, 100.0, 100.0, reach), Some(Grab::Start));
        assert_eq!(grab_at(105.0, 100.0, 100.0, reach), Some(Grab::End));
        assert_eq!(grab_at(100.5, 100.0, 102.0, reach), Some(Grab::Start));
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
        assert_eq!(Verdict::of(s(10), s(10) + MIN_CLIP, 1), Verdict::Ready);
        assert!(Verdict::Long.can_export());
        assert!(!Verdict::NoFormat.can_export());
    }

    #[test]
    fn sizes() {
        assert_eq!(fmt_size(840_000), "0.8 MB");
        assert_eq!(fmt_size(9_960_000), "10.0 MB");
        assert_eq!(fmt_size(15_600_000), "16 MB");
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

    use encore::lyrics::lrc::parse_lrc;

    use super::*;
    use crate::export::render::Renderer;
    use crate::player::{Origin, PlayState, PlayerState};
    use crate::ui::toasts::{Progress, Toast, Toasts};
    use crate::ui::{classic, shortcuts, ArtView, View};

    /// Renders the panel and toasts over the Classic layout to the PNG named by
    /// `ENCORE_EXPORT_PANEL_SNAPSHOT`, or `export-panel.png` in the temp folder.
    /// `ENCORE_EXPORT_PANEL_STYLE=encore-classic` picks the Encore Classic style instead of Vibrant, and
    /// `ENCORE_EXPORT_PANEL_LYRICS=none` shows a song without lyrics.
    /// `ENCORE_EXPORT_PANEL_SONG=<file>` loads the art of that MPD file, which the layout and
    /// the panel take their colours from; `ENCORE_EXPORT_PANEL_TINT=off` turns album colours
    /// off. `ENCORE_EXPORT_PANEL_SHORTCUTS=on` draws the shortcuts overlay instead of the panel.
    /// `ENCORE_EXPORT_PANEL_DISPLAY_FONT` sets the layout in another display font, given as a
    /// display font list entry.
    #[test]
    #[ignore = "needs a GPU"]
    fn panel_snapshot() {
        let out = std::env::var_os("ENCORE_EXPORT_PANEL_SNAPSHOT")
            .map_or_else(|| std::env::temp_dir().join("export-panel.png"), Into::into);
        let song = SongInfo {
            id: 1,
            file: "x.flac".into(),
            title: "All The Stars".into(),
            artist: "Kendrick Lamar & SZA".into(),
            album: "Black Panther".into(),
            duration: Some(Duration::from_secs(232)),
            origin: Origin::Mpd,
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
        let lyrics = match std::env::var("ENCORE_EXPORT_PANEL_LYRICS").as_deref() {
            Ok("none") => LyricsState::NotFound,
            _ => LyricsState::Found(Arc::new(parse_lrc(&lrc))),
        };
        let mut panel = ExportPanel::new(
            song.clone(),
            None,
            lyrics.clone(),
            Duration::from_secs(30),
            Marks::default(),
            false,
            LiveLook {
                style: match std::env::var("ENCORE_EXPORT_PANEL_STYLE").as_deref() {
                    Ok("encore-classic") => Style::EncoreClassic,
                    _ => Style::Vibrant,
                },
                motion: true,
                artist_bg: HashMap::new(),
                tint: true,
                skip_buttons: true,
                lyrics_align: LyricsAlign::Right,
                lyrics_flow: LyricsFlow::Stacked,
                display_font: DisplayFont::default(),
            },
        );
        if panel.synced().is_some() {
            panel.selection = None;
            panel.pick(4, false);
            panel.pick(8, false);
            panel.scroll_to = Some(6);
        } else {
            panel.set_range((Duration::from_secs(95), Duration::from_millis(112_500)));
        }
        let env_on = |name: &str, default: bool| {
            std::env::var(name).map_or(default, |v| !matches!(v.as_str(), "off" | "0" | ""))
        };
        let tint = env_on("ENCORE_EXPORT_PANEL_TINT", true);
        let shortcuts = env_on("ENCORE_EXPORT_PANEL_SHORTCUTS", false);
        let art_song = std::env::var("ENCORE_EXPORT_PANEL_SONG")
            .ok()
            .map(|file| SongInfo {
                file,
                ..song.clone()
            });
        let mut formats = vec![Aspect::Portrait, Aspect::Square];
        let mut quality = Quality::default();
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
        if let Ok(entry) = std::env::var("ENCORE_EXPORT_PANEL_DISPLAY_FONT") {
            let spec = crate::ui::fonts::FontSpec::parse(&entry, None);
            let font = crate::ui::fonts::resolve(&spec).expect("display font");
            crate::ui::fonts::install(r.ctx(), &font);
        }
        let art = art_song.map(|s| {
            let images = art::load_art_images(&crate::config::Config::from_env(), &s);
            art::upload(r.ctx(), &s.file, images)
        });
        let art = ArtView {
            current: art.as_ref(),
            previous: None,
            fade: 1.0,
            artist_colours: false,
            switch: None,
        };
        let theme = PanelTheme::new(&art.scheme(), tint);
        let mut pixels = Vec::new();
        for i in 0..15 {
            let px = r
                .render_frame(f64::from(i) / 30.0, 1.0 / 30.0, |ui| {
                    let view = View {
                        player: &player,
                        art,
                        next_cover: None,
                        artist_bg: false,
                        show_clock: true,
                        lyrics_enabled: true,
                        lyrics: lyrics.clone(),
                        now: chrono::Local::now(),
                        capture: false,
                        motion: false,
                        tint,
                        skip_buttons: true,
                        lyrics_align: LyricsAlign::Right,
                        lyrics_flow: LyricsFlow::Stacked,
                    };
                    classic::draw(ui, &view);
                    let ctx = ui.ctx().clone();
                    if shortcuts {
                        shortcuts::draw(&ctx, true, &theme);
                    } else {
                        panel.show(&ctx, &theme, &mut formats, &mut quality);
                        toasts.show(&ctx, Some(&progress));
                    }
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
