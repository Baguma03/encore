//! The lyrics manager: searches LRCLIB and KuGou, imports lyrics files, edits and saves a
//! song's manual lyrics, times them line by line, and adjusts the timing of the lyrics it
//! shows.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::epaint::Shadow;
use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat, TextWrapping};
use egui::text_edit::TextEditState;
use egui::{
    Align, Align2, Color32, CursorIcon, Event, FontId, Frame, Galley, Id, Key, LayerId, Margin,
    Modal, Modifiers, Order, Painter, Pos2, Rect, Response, ScrollArea, Sense, Stroke, StrokeKind,
    TextEdit, Ui, Vec2,
};
use encore::file_drop;
use encore::lyrics::lrc::{format_timestamp, parse_lrc};
use encore::lyrics::search::{
    Candidate, LyricsSearcher, SearchOutcome, SearchRequest, SearchState,
};
use encore::lyrics::sync_edit::{insert_stamp, tap_time, Clock, SyncDoc, SyncSession};
use encore::lyrics::{
    read_lyrics_file, Adjustments, Lyrics, LyricsInfo, LyricsProvider, LyricsState, Origin, SongKey,
};
use encore::portal::{FilePicker, PickResult};

use super::export_panel::{
    card, chip, hit, muted, paint_nudge, pill, section_heading, switch_cell, NUDGE_SIZE,
};
use super::panel_theme::PanelTheme;
use super::scheme::Ink;
use super::shortcuts::paint_key_cap;
use super::toasts::Toast;
use super::widgets::{ease, lerp, paint_icon, seek_bar, Icon};
use super::{fmt_time, font_bold, font_light, with_alpha};
use crate::player::{Command, SongInfo};

/// File types offered by the file dialog and accepted when dropped on the window.
const EXTENSIONS: [&str; 2] = ["lrc", "txt"];
const PATTERNS: [&str; 2] = ["*.lrc", "*.txt"];

const PANEL_WIDTH: f32 = 1040.0;
/// Width of the timing view, which moves to the side so the lyrics behind it stay visible.
const TIMING_WIDTH: f32 = 500.0;
/// Width of the sync view, which docks like the timing view.
const SYNC_WIDTH: f32 = 600.0;
/// Panels narrower than this stack the two columns.
const TWO_COLUMNS: f32 = 760.0;
/// Share of the width taken by the results column.
const RESULTS_SHARE: f32 = 0.47;
const COLUMN_GAP: f32 = 16.0;
const CARD_GAP: f32 = 12.0;
const CARD_PAD: f32 = 16.0;
/// Gap between the docked timing view and the window edge.
const DOCK_EDGE: f32 = 48.0;
const DOCK_TIME: f32 = 0.25;
/// Opacity of the scrim over the layout, and over it while the timing view is docked.
const BACKDROP: f32 = 130.0 / 255.0;
const TIMING_BACKDROP: f32 = 30.0 / 255.0;
/// Height of the column area: this share of the window, within the limits below.
const BODY_SHARE: f32 = 0.6;
const MIN_BODY: f32 = 240.0;
const MAX_BODY: f32 = 600.0;
/// Lyric lines previewed on a result card.
const PREVIEW_LINES: usize = 2;
const ENTRY_PAD: f32 = 14.0;
const ICON_BUTTON: f32 = 30.0;
const FIELD_HEIGHT: f32 = 46.0;

/// Offset nudges, in milliseconds, with their labels.
const OFFSET_STEPS: [(i64, &str); 4] = [
    (-500, "−0.5 s"),
    (-100, "−0.1 s"),
    (100, "+0.1 s"),
    (500, "+0.5 s"),
];
/// Largest offset the nudges reach either way.
const MAX_OFFSET_MS: i64 = 60_000;

/// How far ← and → seek in the sync view.
const SEEK_STEP: Duration = Duration::from_secs(3);
/// How far ← and → and the transport's buttons seek in the other views.
const TRANSPORT_STEP: Duration = Duration::from_secs(5);
/// Seeks stop this far before the end of the song: reaching the end finishes it, the player
/// moves on to the next song and the panel follows, clearing the editor.
const END_GUARD: Duration = Duration::from_secs(1);
/// A seek shows its target until the player reports a position this close to it, or for at
/// most `SEEK_HOLD`.
const SEEK_SETTLE: Duration = Duration::from_millis(400);
const SEEK_HOLD: Duration = Duration::from_secs(1);
const TRANSPORT_HEIGHT: f32 = 32.0;
/// Thickness of the transport's seek bar, which thickens on hover.
const TRANSPORT_TRACK: f32 = 4.0;
/// Clicking a line's time plays from this long before the line, to hear the lead-in.
const LEAD_IN: Duration = Duration::from_secs(1);
/// Step of a line's ± nudges, in milliseconds.
const LINE_NUDGE_MS: i64 = 100;
const SYNC_ROW: f32 = 38.0;
/// Width of a sync row's time column.
const TIME_COLUMN: f32 = 96.0;
/// Room above and below the sync view's list for its other parts.
const SYNC_CHROME: f32 = 404.0;
const MIN_SYNC_LIST: f32 = 160.0;
const MAX_SYNC_LIST: f32 = 640.0;
/// Shown in place of a line's time while it has none.
const NO_TIME: &str = "--:--.--";

const SAVED_TOAST: Duration = Duration::from_secs(4);
const NOTICE_TOAST: Duration = Duration::from_secs(3);

// ---------------------------------------------------------------------------------------------
// State

/// What the editor was filled from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// The lyrics the display shows.
    Current,
    /// A search result, by index into the latest outcome.
    Result(usize),
    /// The imported lyrics file.
    File,
    /// A search result of an earlier search.
    Kept,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub entry: Entry,
    /// Where the editor's text came from, shown above the editor.
    pub label: String,
}

/// A lyrics file picked in the dialog or dropped on the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    pub name: String,
    pub text: String,
}

/// Everything the panel shows that does not come from the lyrics provider, kept apart from
/// drawing so its rules can be tested.
pub struct Draft {
    song: SongInfo,
    key: SongKey,
    query: String,
    /// The running search started from this panel.
    token: Option<u64>,
    outcome: Option<Arc<SearchOutcome>>,
    file: Option<Imported>,
    selection: Option<Selection>,
    editor: String,
    /// The editor's text as selected, for telling edits apart and resetting them.
    original: String,
    timing: bool,
    /// The sync view's pass over the editor's text, while the view is open.
    sync: Option<SyncState>,
    confirm_delete: bool,
    error: Option<String>,
}

/// The sync view's state.
pub struct SyncState {
    pub session: SyncSession,
    /// Scroll the next line to stamp into view on the next frame.
    scroll: bool,
}

impl Draft {
    pub fn new(song: SongInfo) -> Self {
        Self {
            key: song.lyrics_key(),
            song,
            query: String::new(),
            token: None,
            outcome: None,
            file: None,
            selection: None,
            editor: String::new(),
            original: String::new(),
            timing: false,
            sync: None,
            confirm_delete: false,
            error: None,
        }
    }

    /// Switches to `song` when it is another song: clears the search, the import and the
    /// editor and returns true. The view (search or timing) stays.
    pub fn follow(&mut self, song: &SongInfo) -> bool {
        let key = song.lyrics_key();
        if key == self.key {
            return false;
        }
        *self = Self {
            timing: self.timing,
            ..Self::new(song.clone())
        };
        true
    }

    /// "Artist - Title" of the song, the search field's hint and what the fill button enters.
    pub fn song_query(&self) -> String {
        [self.song.artist.trim(), self.song.title.trim()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" - ")
    }

    /// The search for the field's text, or for the song's own title and artist when it is
    /// blank.
    pub fn request(&self) -> SearchRequest {
        let mut req = SearchRequest::from_key(&self.key);
        let query = self.query.trim();
        if !query.is_empty() {
            req.query = Some(query.to_owned());
        }
        req
    }

    /// Records a started search. A result selected from the previous outcome keeps its text.
    pub fn searching(&mut self, token: u64) {
        self.token = Some(token);
        self.outcome = None;
        if let Some(sel) = &mut self.selection {
            if matches!(sel.entry, Entry::Result(_)) {
                sel.entry = Entry::Kept;
            }
        }
    }

    pub fn is_searching(&self) -> bool {
        self.token.is_some()
    }

    /// Takes the outcome of this panel's search from the searcher's state.
    pub fn poll_search(&mut self, state: &SearchState) {
        match state {
            SearchState::Done(token, outcome) if self.token == Some(*token) => {
                self.outcome = Some(Arc::clone(outcome));
                self.token = None;
            }
            // Another panel's search, or a cancel: this one is no longer running.
            SearchState::Idle if self.token.is_some() => self.token = None,
            _ => {}
        }
    }

    /// Fills the editor from `entry`.
    pub fn select(&mut self, entry: Entry, label: String, text: &str) {
        self.selection = Some(Selection { entry, label });
        self.editor = text.to_owned();
        self.original = text.to_owned();
        self.confirm_delete = false;
        self.error = None;
    }

    pub fn select_result(&mut self, index: usize) {
        let Some(c) = self.outcome.as_ref().and_then(|o| o.candidates.get(index)) else {
            return;
        };
        let label = result_label(c);
        let text = c.text.clone();
        self.select(Entry::Result(index), label, &text);
    }

    pub fn import(&mut self, name: String, text: String) {
        let label = format!("File: {name}");
        self.select(Entry::File, label, &text);
        self.file = Some(Imported { name, text });
        self.timing = false;
        self.sync = None;
    }

    pub fn select_file(&mut self) {
        if let Some(file) = self.file.clone() {
            self.select(Entry::File, format!("File: {}", file.name), &file.text);
        }
    }

    /// Follows the shown lyrics' text into the editor while it shows them unedited, and
    /// selects them when nothing else is selected.
    pub fn refresh_current(&mut self, text: Option<&str>) {
        let showing_current = self
            .selection
            .as_ref()
            .is_some_and(|s| s.entry == Entry::Current);
        match text {
            Some(text) if self.selection.is_none() && self.editor.is_empty() => {
                self.select(Entry::Current, "Current lyrics".into(), text);
            }
            Some(text) if showing_current && !self.edited() && self.original != text => {
                self.editor = text.to_owned();
                self.original = text.to_owned();
            }
            None if showing_current && !self.edited() => {
                self.selection = None;
                self.editor.clear();
                self.original.clear();
            }
            _ => {}
        }
    }

    pub fn edited(&self) -> bool {
        self.editor != self.original
    }

    pub fn reset_editor(&mut self) {
        self.editor = self.original.clone();
    }

    /// Something other than the unchanged current lyrics is in the editor, and it holds at
    /// least one lyric line.
    pub fn can_save(&self) -> bool {
        let Some(sel) = &self.selection else {
            return false;
        };
        (sel.entry != Entry::Current || self.edited()) && !parse_lrc(&self.editor).is_empty()
    }

    fn is_selected(&self, entry: Entry) -> bool {
        self.selection.as_ref().is_some_and(|s| s.entry == entry)
    }

    /// The editor holds lyric lines to time.
    pub fn can_sync(&self) -> bool {
        !parse_lrc(&self.editor).is_empty()
    }

    /// Opens the sync view on the editor's text.
    pub fn start_sync(&mut self) {
        if self.can_sync() {
            self.sync = Some(SyncState {
                session: SyncSession::new(SyncDoc::parse(&self.editor)),
                scroll: true,
            });
            self.timing = false;
            self.confirm_delete = false;
        }
    }

    /// Closes the sync view, writing its lines back into the editor as LRC.
    pub fn apply_sync(&mut self) {
        if let Some(sync) = self.sync.take() {
            self.editor = sync.session.doc.to_text(self.song.duration);
        }
    }

    /// Closes the sync view and leaves the editor as it was.
    pub fn cancel_sync(&mut self) {
        self.sync = None;
    }

    pub fn is_syncing(&self) -> bool {
        self.sync.is_some()
    }
}

/// What a key does in the sync view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncKey {
    Stamp,
    Back,
    SeekBack,
    SeekForward,
    TogglePause,
    Gap,
    Cancel,
}

/// The sync view's keys. They act without Ctrl, Alt or the command key.
pub fn sync_key(key: Key) -> Option<SyncKey> {
    Some(match key {
        Key::Space | Key::Enter => SyncKey::Stamp,
        Key::Backspace => SyncKey::Back,
        Key::ArrowLeft => SyncKey::SeekBack,
        Key::ArrowRight => SyncKey::SeekForward,
        Key::P => SyncKey::TogglePause,
        Key::G => SyncKey::Gap,
        Key::Escape => SyncKey::Cancel,
        _ => return None,
    })
}

/// What a playback key does outside the sync view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackKey {
    SeekBack,
    SeekForward,
    TogglePause,
}

/// The playback keys of the editor and timing views: ←, → and P on their own while no text
/// field has the keyboard (`typing`), and with Alt at any time, which leaves the keys to the
/// text field's cursor and typing.
pub fn playback_key(key: Key, modifiers: Modifiers, typing: bool) -> Option<PlaybackKey> {
    if modifiers.ctrl || modifiers.command || modifiers.shift || (typing && !modifiers.alt) {
        return None;
    }
    Some(match key {
        Key::ArrowLeft => PlaybackKey::SeekBack,
        Key::ArrowRight => PlaybackKey::SeekForward,
        Key::P => PlaybackKey::TogglePause,
        _ => return None,
    })
}

/// `to` kept within the song: from its start to `END_GUARD` before its end.
pub fn clamp_seek(to: Duration, duration: Option<Duration>) -> Duration {
    match duration {
        Some(d) => to.min(d.saturating_sub(END_GUARD)),
        None => to,
    }
}

/// The position a `step` back or forward from `elapsed` seeks to, within the song. A step
/// forward never goes back, even within `END_GUARD` of the end.
pub fn seek_target(
    elapsed: Duration,
    step: Duration,
    forward: bool,
    duration: Option<Duration>,
) -> Duration {
    if forward {
        clamp_seek(elapsed + step, duration).max(elapsed)
    } else {
        elapsed.saturating_sub(step)
    }
}

/// The position to show while a seek to `to`, asked for `since` ago, is on its way to the
/// player, or None once the player reports `elapsed` near it or the seek is too old to wait for.
pub fn pending_position(
    elapsed: Duration,
    playing: bool,
    to: Duration,
    since: Duration,
) -> Option<Duration> {
    let expected = if playing { to + since } else { to };
    (since < SEEK_HOLD && elapsed.abs_diff(expected) >= SEEK_SETTLE).then_some(expected)
}

/// A playback position as `m:ss.xx`, the transport's elapsed time.
pub fn fmt_clock(position: Duration) -> String {
    let cs = (position.as_millis() + 5) / 10;
    format!("{}:{:02}.{:02}", cs / 6000, cs / 100 % 60, cs % 100)
}

/// A playback position as a stamp would read: `mm:ss.xx`.
pub fn fmt_position(position: Duration) -> String {
    let cs = (position.as_millis() + 5) / 10;
    format_timestamp(Duration::from_millis(cs as u64 * 10))
}

// ---------------------------------------------------------------------------------------------
// Labels

/// "Synced (Cache (LRCLIB))", "Plain (Manual)".
pub fn lyrics_label(synced: bool, origin: Origin) -> String {
    let kind = if synced { "Synced" } else { "Plain" };
    format!("{kind} ({origin})")
}

/// Heading of the current lyrics card.
pub fn current_label(state: &LyricsState, info: Option<&LyricsInfo>) -> String {
    match (state, info) {
        (LyricsState::Loading, _) => "Looking up lyrics…".into(),
        (LyricsState::NotFound, _) | (LyricsState::Found(_), None) => "No lyrics".into(),
        (LyricsState::Found(_), Some(info)) => lyrics_label(info.synced, info.origin),
    }
}

/// "LRCLIB · Synced · Title", the editor's source label for a search result.
pub fn result_label(c: &Candidate) -> String {
    let kind = if c.synced() { "Synced" } else { "Plain" };
    format!("{} · {kind} · {}", c.provider, c.title.trim())
}

/// A timing offset: "0.0 s", "+0.3 s", "−1.25 s".
pub fn fmt_offset(ms: i64) -> String {
    let sign = match ms.signum() {
        1 => "+",
        -1 => "−",
        _ => "",
    };
    let abs = ms.unsigned_abs();
    let secs = abs as f64 / 1000.0;
    let digits = if abs.is_multiple_of(100) {
        1
    } else if abs.is_multiple_of(10) {
        2
    } else {
        3
    };
    format!("{sign}{secs:.digits$} s")
}

/// A result's length with how far it is from the song's: "3:23", "3:25 · +2 s",
/// "3:21 · −2 s". None without the result's length.
pub fn fmt_length(result: Option<Duration>, song: Option<Duration>) -> Option<String> {
    let result = result?;
    let mut text = fmt_time(result);
    if let Some(song) = song {
        let diff = result.as_secs_f64() - song.as_secs_f64();
        let secs = diff.abs().round() as u64;
        if secs >= 1 {
            let sign = if diff > 0.0 { "+" } else { "−" };
            text.push_str(&format!(" · {sign}{secs} s"));
        }
    }
    Some(text)
}

/// How sure a result is the song.
pub fn match_label(c: &Candidate) -> &'static str {
    if c.is_sure_match() {
        "Sure match"
    } else if c.is_good_match() {
        "Good match"
    } else {
        "Weak match"
    }
}

/// Why stretching is unavailable for the shown lyrics, or None when it is available.
pub fn stretch_blocker(
    info: Option<&LyricsInfo>,
    duration: Option<Duration>,
) -> Option<&'static str> {
    let Some(info) = info else {
        return Some("No lyrics to stretch");
    };
    if info.can_stretch {
        None
    } else if !info.synced {
        Some("Only synced lyrics can be stretched")
    } else if info.length.is_none() {
        Some("The lyrics have no [length:] tag")
    } else if duration.is_none_or(|d| d.is_zero()) {
        Some("The song's length is unknown")
    } else {
        Some("The [length:] tag is too far from the song's length")
    }
}

/// The new offset after a nudge of `step` milliseconds, within ±[`MAX_OFFSET_MS`].
pub fn nudged(offset_ms: i64, step: i64) -> i64 {
    (offset_ms + step).clamp(-MAX_OFFSET_MS, MAX_OFFSET_MS)
}

/// "Synced · 42 lines", "Plain · 12 lines", or None when the text holds no lyric line.
pub fn editor_summary(text: &str) -> Option<String> {
    let lyrics = parse_lrc(text);
    let (kind, count) = match &lyrics {
        Lyrics::Synced(lines) => (
            "Synced",
            lines.iter().filter(|l| !l.text.is_empty()).count(),
        ),
        Lyrics::Plain(lines) => ("Plain", lines.iter().filter(|l| !l.is_empty()).count()),
    };
    (!lyrics.is_empty()).then(|| {
        let s = if count == 1 { "" } else { "s" };
        format!("{kind} · {count} line{s}")
    })
}

/// The first `n` lines with words, without timestamps.
fn preview_lines(lyrics: &Lyrics, n: usize) -> Vec<String> {
    let texts: Box<dyn Iterator<Item = &str>> = match lyrics {
        Lyrics::Synced(lines) => Box::new(lines.iter().map(|l| l.text.as_str())),
        Lyrics::Plain(lines) => Box::new(lines.iter().map(String::as_str)),
    };
    texts
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .take(n)
        .map(str::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Panel

/// What the app passes in each frame.
pub struct Env<'a> {
    pub provider: &'a LyricsProvider,
    /// Playback position, when the panel's song is the one playing.
    pub elapsed: Option<Duration>,
    /// Dock the timing view at the right edge instead of the left, away from the lyrics.
    pub dock_right: bool,
    /// The lyrics are shown on the display.
    pub lyrics_visible: bool,
    /// The song is playing rather than paused.
    pub playing: bool,
}

/// What the panel asks of the app after a frame.
#[derive(Default)]
pub struct Output {
    pub close: bool,
    pub toasts: Vec<Toast>,
    /// Playback commands from the transport, the playback keys and the sync view.
    pub commands: Vec<Command>,
}

/// Lyrics of the panel's song as the provider has them this frame.
struct Shown {
    state: LyricsState,
    info: Option<Arc<LyricsInfo>>,
    has_override: bool,
    adjustments: Adjustments,
}

pub struct LyricsPanel {
    draft: Draft,
    searcher: LyricsSearcher,
    picker: Option<FilePicker>,
    online: bool,
    repaint: Arc<dyn Fn() + Send + Sync>,
    /// Colours of the frame being drawn.
    theme: PanelTheme,
    /// The sync view's stamped lines at their song positions, for the layout behind the panel.
    preview: Option<Arc<Lyrics>>,
    /// The latest seek the panel asked for and when, until the player reports it.
    seek: Option<(Duration, Instant)>,
}

impl LyricsPanel {
    /// `online` offers the search; `repaint` is called from background threads when a search
    /// or file pick finishes.
    pub fn new(song: SongInfo, online: bool, repaint: Arc<dyn Fn() + Send + Sync>) -> Self {
        let on_ready = Arc::clone(&repaint);
        Self {
            draft: Draft::new(song),
            searcher: LyricsSearcher::new(Box::new(move || on_ready())),
            picker: None,
            online,
            repaint,
            theme: PanelTheme::PLAIN,
            preview: None,
            seek: None,
        }
    }

    pub fn song(&self) -> &SongInfo {
        &self.draft.song
    }

    /// The sync view is open; its keys belong to the panel.
    pub fn is_syncing(&self) -> bool {
        self.draft.is_syncing()
    }

    /// While the sync view has stamped lines: those lines as the display would show them once
    /// saved, to show in place of the song's lyrics. Not saved anywhere.
    pub fn preview(&self) -> Option<Arc<Lyrics>> {
        self.preview.clone()
    }

    /// Follows a song change: the panel stays open for the new song with its search cleared.
    pub fn follow(&mut self, song: &SongInfo) {
        if self.draft.follow(song) {
            self.searcher.cancel();
        }
    }

    /// Draws the panel in the colours of `theme`.
    pub fn show(&mut self, ctx: &egui::Context, theme: &PanelTheme, env: &Env<'_>) -> Output {
        self.theme = *theme;
        let mut out = Output::default();
        self.poll(ctx);
        let key = self.draft.key.clone();
        let shown = Shown {
            state: env.provider.get(&key),
            info: env.provider.info(&key),
            has_override: env.provider.has_override(&key),
            adjustments: env.provider.adjustments(&key),
        };
        self.draft
            .refresh_current(shown.info.as_ref().map(|i| &*i.text));
        // The keys and the views work from the position a seek asked for until the player
        // reports it, so steps chain and the readouts move at once.
        let live = Env {
            elapsed: self.position(env),
            ..*env
        };
        self.sync_keys(ctx, &live, &shown, &mut out);
        self.playback_keys(ctx, &live, &mut out);
        self.note_seeks(&out.commands);
        let live = Env {
            elapsed: self.position(env),
            ..*env
        };
        let sent = out.commands.len();
        theme.scope(ctx, || self.show_modal(ctx, &live, &shown, &mut out));
        self.note_seeks(&out.commands[sent..]);
        self.update_preview(&shown);
        out
    }

    /// The playback position to show: the latest seek's target while it is on its way to the
    /// player, else the player's.
    fn position(&mut self, env: &Env<'_>) -> Option<Duration> {
        let elapsed = env.elapsed?;
        let pending = self
            .seek
            .and_then(|(to, at)| pending_position(elapsed, env.playing, to, at.elapsed()));
        if pending.is_none() {
            self.seek = None;
        }
        Some(pending.unwrap_or(elapsed))
    }

    fn note_seeks(&mut self, commands: &[Command]) {
        let last = commands.iter().rev().find_map(|c| match c {
            Command::SeekTo(to) => Some(*to),
            _ => None,
        });
        if let Some(to) = last {
            self.seek = Some((to, Instant::now()));
        }
    }

    /// Takes the playback keys of the editor and timing views out of this frame's input, so
    /// neither the text fields nor the app see them, and acts on them. Alt+P also takes the
    /// character it types.
    fn playback_keys(&mut self, ctx: &egui::Context, env: &Env<'_>, out: &mut Output) {
        if self.draft.is_syncing() {
            return;
        }
        let typing = ctx.text_edit_focused();
        let keys: Vec<PlaybackKey> = ctx.input_mut(|i| {
            let mut keys = Vec::new();
            let mut alt_text = false;
            i.events.retain(|e| match e {
                Event::Key {
                    key,
                    pressed,
                    repeat,
                    modifiers,
                    ..
                } => {
                    let Some(action) = playback_key(*key, *modifiers, typing) else {
                        return true;
                    };
                    if *pressed {
                        alt_text = modifiers.alt;
                        if !*repeat {
                            keys.push(action);
                        }
                    }
                    false
                }
                Event::Text(_) if alt_text => {
                    alt_text = false;
                    false
                }
                _ => true,
            });
            keys
        });
        for key in keys {
            self.playback(key, env, TRANSPORT_STEP, out);
        }
    }

    /// Seeks `step` back or forward, or plays or pauses, while the panel's song is playing.
    fn playback(&self, key: PlaybackKey, env: &Env<'_>, step: Duration, out: &mut Output) {
        let Some(elapsed) = env.elapsed else {
            return;
        };
        out.commands.push(match key {
            PlaybackKey::SeekBack | PlaybackKey::SeekForward => Command::SeekTo(seek_target(
                elapsed,
                step,
                key == PlaybackKey::SeekForward,
                self.draft.song.duration,
            )),
            PlaybackKey::TogglePause => Command::TogglePause,
        });
    }

    /// The clock of the sync view's document for this song.
    fn sync_clock(&self, shown: &Shown) -> Option<Clock> {
        let sync = self.draft.sync.as_ref()?;
        Some(
            sync.session
                .doc
                .clock(shown.adjustments, self.draft.song.duration),
        )
    }

    fn update_preview(&mut self, shown: &Shown) {
        let lyrics = self.draft.sync.as_ref().and_then(|sync| {
            let clock = self.sync_clock(shown)?;
            (sync.session.doc.stamped() > 0).then(|| clock.lyrics(&sync.session.doc))
        });
        match lyrics {
            Some(l) if self.preview.as_deref() != Some(&l) => self.preview = Some(Arc::new(l)),
            Some(_) => {}
            None => self.preview = None,
        }
    }

    /// Takes the sync view's keys out of this frame's input, so neither egui's widgets nor the
    /// app see them, and acts on them. Nothing has the keyboard focus in the sync view.
    fn sync_keys(&mut self, ctx: &egui::Context, env: &Env<'_>, shown: &Shown, out: &mut Output) {
        if !self.draft.is_syncing() {
            return;
        }
        ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        let keys: Vec<SyncKey> = ctx.input_mut(|i| {
            let mut keys = Vec::new();
            i.events.retain(|e| match e {
                Event::Key {
                    key,
                    pressed,
                    repeat,
                    modifiers,
                    ..
                } if !(modifiers.ctrl || modifiers.alt || modifiers.command) => {
                    let Some(action) = sync_key(*key) else {
                        return true;
                    };
                    if *pressed && !*repeat {
                        keys.push(action);
                    }
                    false
                }
                _ => true,
            });
            keys
        });
        for key in keys {
            self.sync_action(key, env, shown, out);
        }
    }

    fn sync_action(&mut self, key: SyncKey, env: &Env<'_>, shown: &Shown, out: &mut Output) {
        let Some(clock) = self.sync_clock(shown) else {
            return;
        };
        let playback = match key {
            SyncKey::SeekBack => Some(PlaybackKey::SeekBack),
            SyncKey::SeekForward => Some(PlaybackKey::SeekForward),
            SyncKey::TogglePause => Some(PlaybackKey::TogglePause),
            _ => None,
        };
        if let Some(key) = playback {
            self.playback(key, env, SEEK_STEP, out);
            return;
        }
        let Some(sync) = self.draft.sync.as_mut() else {
            return;
        };
        let tap = env
            .elapsed
            .map(|e| clock.file_time(tap_time(e, env.playing)));
        match key {
            SyncKey::Stamp => {
                if let Some(at) = tap {
                    sync.session.stamp(at);
                    sync.scroll = true;
                }
            }
            SyncKey::Back => {
                sync.session.back();
                sync.scroll = true;
            }
            SyncKey::Gap => {
                if let Some(at) = tap {
                    sync.session.gap(at);
                    sync.scroll = true;
                }
            }
            SyncKey::SeekBack | SyncKey::SeekForward | SyncKey::TogglePause => {}
            SyncKey::Cancel => self.draft.cancel_sync(),
        }
    }

    /// Takes a finished search, a picked file and dropped files.
    fn poll(&mut self, ctx: &egui::Context) {
        self.draft.poll_search(&self.searcher.state());
        if let Some(result) = self.picker.as_mut().and_then(FilePicker::poll) {
            self.picker = None;
            match result {
                PickResult::Picked(path) => self.import_path(&path),
                PickResult::Cancelled => {}
                PickResult::Unavailable(msg) => {
                    self.draft.error = Some(format!(
                        "No file dialog available ({msg}). Drop a file on the window instead."
                    ));
                }
            }
        }
        if let Some(path) = file_drop::dropped_files(ctx, &EXTENSIONS).first() {
            self.import_path(path);
        }
    }

    fn import_path(&mut self, path: &Path) {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        match read_lyrics_file(path) {
            Ok(text) => self.draft.import(name, text),
            Err(e) => self.draft.error = Some(format!("Couldn't read {name}: {e}")),
        }
    }

    fn search(&mut self) {
        if self.online {
            let token = self.searcher.search(self.draft.request());
            self.draft.searching(token);
        }
    }

    fn pick_file(&mut self) {
        if self.picker.is_none() {
            let repaint = Arc::clone(&self.repaint);
            self.picker = Some(FilePicker::open(
                "Add lyrics file",
                &[("Lyrics", &PATTERNS)],
                move || repaint(),
            ));
        }
    }

    fn show_modal(&mut self, ctx: &egui::Context, env: &Env<'_>, shown: &Shown, out: &mut Output) {
        let t = self.theme;
        let id = Id::unique("lyrics-panel");
        let screen = ctx.content_rect();
        let syncing = self.draft.is_syncing();
        let docked = self.draft.timing || syncing;
        let max_width = if syncing {
            SYNC_WIDTH
        } else if docked {
            TIMING_WIDTH
        } else {
            PANEL_WIDTH
        };
        let width = max_width.min(screen.width() - 80.0).max(300.0);
        let dock = ease(ctx.animate_bool_with_time(id.with("dock"), docked, DOCK_TIME));
        let side = if env.dock_right { 1.0 } else { -1.0 };
        let shift = ((screen.width() - width - 48.0) / 2.0 - DOCK_EDGE).max(0.0) * dock * side;
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
        let area = Modal::default_area(id).anchor(Align2::CENTER_CENTER, Vec2::new(shift, 0.0));
        let modal = Modal::new(id)
            .area(area)
            .frame(frame)
            .backdrop_color(with_alpha(t.scrim, lerp(BACKDROP, TIMING_BACKDROP, dock)))
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing = Vec2::new(8.0, 0.0);
                if syncing {
                    let list = (screen.height() - SYNC_CHROME).clamp(MIN_SYNC_LIST, MAX_SYNC_LIST);
                    self.sync_view(ui, env, shown, list, out);
                } else if self.draft.timing {
                    self.timing_view(ui, env, shown, out);
                } else {
                    let body = (screen.height() * BODY_SHARE).clamp(MIN_BODY, MAX_BODY);
                    self.search_view(ui, env, shown, body, out);
                }
            });
        if file_drop::files_hovered(ctx, &EXTENSIONS) {
            drop_hint(ctx, &t, modal.response.rect);
        }
    }

    fn search_view(
        &mut self,
        ui: &mut Ui,
        env: &Env<'_>,
        shown: &Shown,
        body: f32,
        out: &mut Output,
    ) {
        self.header(ui, out);
        ui.add_space(16.0);
        self.search_row(ui);
        ui.add_space(CARD_GAP);
        let width = ui.available_width();
        if width < TWO_COLUMNS {
            self.current_card(ui, env, shown, out);
            ui.add_space(CARD_GAP);
            self.results_card(ui, body * 0.45);
            ui.add_space(CARD_GAP);
            self.editor_card(ui, env, shown, None, body * 0.45);
        } else {
            let results_width = ((width - COLUMN_GAP) * RESULTS_SHARE).round();
            let editor_width = width - results_width - COLUMN_GAP;
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = COLUMN_GAP;
                let left = ui
                    .vertical(|ui| {
                        ui.set_width(results_width);
                        let top = ui.cursor().top();
                        self.current_card(ui, env, shown, out);
                        ui.add_space(CARD_GAP);
                        let used = ui.cursor().top() - top;
                        self.results_card(ui, (body - used).max(120.0));
                    })
                    .response
                    .rect;
                ui.vertical(|ui| {
                    ui.set_width(editor_width);
                    self.editor_card(ui, env, shown, Some(left.bottom()), body);
                });
            });
        }
        ui.add_space(CARD_GAP);
        transport(ui, &self.theme, env, self.draft.song.duration, false, out);
        ui.add_space(CARD_GAP);
        key_hints(ui, &self.theme, PLAYBACK_HINTS);
        ui.add_space(CARD_GAP);
        self.footer(ui, env, out);
    }

    /// "Lyrics", the song's title and artist, and the close button.
    fn header(&mut self, ui: &mut Ui, out: &mut Output) {
        let t = self.theme;
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Lyrics")
                    .font(font_bold(22.0))
                    .color(t.heading),
            );
            let name: Vec<&str> = [&self.draft.song.title, &self.draft.song.artist]
                .into_iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            let close_width = ICON_BUTTON + 8.0;
            if !name.is_empty() {
                let text = format!("·  {}", name.join("  ·  "));
                ui.scope(|ui| {
                    ui.set_max_width(ui.available_width() - close_width);
                    ui.add(egui::Label::new(muted(&t, &text, 16.0)).truncate());
                });
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let (rect, response) = hit(ui, Vec2::splat(ICON_BUTTON), true);
                let response = response.on_hover_text("Close (Esc)");
                paint_icon_button(ui, &t, Glyph::Close, rect, &response, false);
                if response.clicked() {
                    out.close = true;
                }
            });
        });
    }

    /// The search field and the button that fills it from the song.
    fn search_row(&mut self, ui: &mut Ui) {
        let t = self.theme;
        let hint = self.draft.song_query();
        let mut search = false;
        ui.horizontal(|ui| {
            let field_width = ui.available_width() - ICON_BUTTON - 16.0;
            let id = Id::unique("lyrics-search-field");
            let focused = ui.memory(|m| m.has_focus(id));
            let stroke = if focused {
                Stroke::new(1.5, with_alpha(t.accent, 0.9))
            } else {
                Stroke::new(1.0, t.layer(48))
            };
            let output = TextEdit::singleline(&mut self.draft.query)
                .id(id)
                .hint_text(
                    egui::RichText::new(hint)
                        .font(font_light(16.0))
                        .color(with_alpha(t.heading, 0.4)),
                )
                .font(font_light(16.0))
                .text_color(t.heading)
                .desired_width(field_width)
                .min_size(Vec2::new(field_width, FIELD_HEIGHT))
                .align(Align2::LEFT_CENTER)
                .frame(
                    Frame::new()
                        .fill(t.layer(5))
                        .stroke(stroke)
                        .corner_radius(14)
                        .inner_margin(Margin::symmetric(16, 0)),
                )
                .show(ui);
            if output.response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                search = true;
            }
            ui.add_space(4.0);
            let (rect, response) = hit(ui, Vec2::new(ICON_BUTTON + 8.0, FIELD_HEIGHT), true);
            let response = response.on_hover_text("Use the song's artist and title");
            paint_icon_button(ui, &t, Glyph::Fill, rect, &response, false);
            if response.clicked() {
                self.draft.query = self.draft.song_query();
            }
        });
        if search {
            self.search();
        }
    }

    /// The lyrics the display shows, with copy, timing and delete buttons.
    fn current_card(&mut self, ui: &mut Ui, env: &Env<'_>, shown: &Shown, out: &mut Output) {
        let t = self.theme;
        let width = ui.available_width();
        let inner = width - 2.0 * ENTRY_PAD;
        let text = shown.info.as_ref().map(|i| Arc::clone(&i.text));
        let painter = ui.painter().clone();

        let mut title = current_label(&shown.state, shown.info.as_deref());
        if let Some(info) = &shown.info {
            let adj = info.adjustments;
            if adj.offset_ms != 0 {
                title.push_str(&format!("  ·  {}", fmt_offset(adj.offset_ms)));
            }
            if adj.stretch && info.can_stretch {
                title.push_str("  ·  stretched");
            }
        }
        let icons = if shown.has_override { 3.0 } else { 2.0 };
        let title_width = inner - 26.0 - icons * (ICON_BUTTON + 4.0);
        let title = one_line(&painter, &title, font_bold(16.0), t.heading, title_width);

        let lines: Vec<Arc<Galley>> = if self.draft.confirm_delete {
            vec![painter.layout(
                "Remove the saved lyrics and look the song up again?".into(),
                font_light(14.0),
                t.warning,
                inner,
            )]
        } else {
            let lines = match &shown.state {
                LyricsState::Found(l) => preview_lines(l, PREVIEW_LINES),
                LyricsState::Loading => vec!["Searching the song's lyrics sources…".into()],
                LyricsState::NotFound => {
                    vec!["Nothing found. Search below or add an LRC file.".into()]
                }
            };
            lines
                .iter()
                .map(|l| one_line(&painter, l, font_light(14.0), t.body, inner))
                .collect()
        };
        let lines_height: f32 = lines.iter().map(|g| g.size().y + 3.0).sum();
        let confirm_row = if self.draft.confirm_delete { 40.0 } else { 0.0 };
        let height = 2.0 * ENTRY_PAD + ICON_BUTTON + 8.0 + lines_height + confirm_row;

        let selectable = text.is_some();
        let (rect, response) = hit(ui, Vec2::new(width, height), selectable);
        let selected = self.draft.is_selected(Entry::Current);
        paint_entry(ui, &t, rect, &response, selected);

        let top = rect.top() + ENTRY_PAD;
        let left = rect.left() + ENTRY_PAD;
        let icon = Rect::from_min_size(
            Pos2::new(left, top + ICON_BUTTON / 2.0 - 10.0),
            Vec2::splat(20.0),
        );
        paint_glyph(&painter, Glyph::Lines, icon, t.text);
        painter.galley(
            Pos2::new(left + 28.0, top + (ICON_BUTTON - title.size().y) / 2.0),
            title,
            t.heading,
        );

        // Buttons from the right: copy, timing, delete.
        let mut x = rect.right() - ENTRY_PAD - ICON_BUTTON;
        let mut button = |ui: &mut Ui, glyph: Glyph, tip: &str, on: bool, enabled: bool| {
            let r = Rect::from_min_size(Pos2::new(x, top), Vec2::splat(ICON_BUTTON));
            x -= ICON_BUTTON + 4.0;
            let sense = if enabled {
                Sense::click()
            } else {
                Sense::hover()
            };
            let mut resp = ui.interact(r, Id::unique(("lyrics-current", glyph as u8)), sense);
            if enabled {
                resp = resp.on_hover_cursor(CursorIcon::PointingHand);
            }
            let resp = resp.on_hover_text(tip);
            let mut p = ui.painter().clone();
            if !enabled {
                p.multiply_opacity(0.35);
            }
            paint_icon_on(&p, &t, glyph, r, enabled && resp.hovered(), on);
            resp.clicked()
        };
        if button(ui, Glyph::Copy, "Copy the lyrics", false, selectable) {
            if let Some(text) = &text {
                ui.ctx().copy_text(text.to_string());
                out.toasts.push(
                    Toast::new("Lyrics copied", NOTICE_TOAST).detail("LRC text on the clipboard"),
                );
            }
        }
        if button(ui, Glyph::Timer, "Timing", self.draft.timing, true) {
            self.draft.timing = !self.draft.timing;
            self.draft.confirm_delete = false;
        }
        if shown.has_override
            && button(
                ui,
                Glyph::Trash,
                "Remove the saved lyrics",
                self.draft.confirm_delete,
                true,
            )
        {
            self.draft.confirm_delete = !self.draft.confirm_delete;
        }

        let mut y = top + ICON_BUTTON + 8.0;
        for galley in lines {
            let h = galley.size().y;
            painter.galley(Pos2::new(left, y), galley, t.body);
            y += h + 3.0;
        }
        if self.draft.confirm_delete {
            let row = Rect::from_min_size(Pos2::new(left, y + 6.0), Vec2::new(inner, 30.0));
            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(row)
                    .layout(egui::Layout::left_to_right(Align::Center)),
            );
            if chip(&mut child, &t, "Remove", true, true).clicked() {
                self.draft.confirm_delete = false;
                match env.provider.delete_override(&self.draft.key) {
                    Ok(()) => out.toasts.push(
                        Toast::new("Saved lyrics removed", NOTICE_TOAST)
                            .detail("Looking the song up again"),
                    ),
                    Err(e) => self.draft.error = Some(format!("Couldn't remove the lyrics: {e}")),
                }
            }
            if chip(&mut child, &t, "Keep", false, true).clicked() {
                self.draft.confirm_delete = false;
            }
        }
        if response.clicked() && !self.draft.confirm_delete {
            if let Some(text) = &text {
                self.draft
                    .select(Entry::Current, "Current lyrics".into(), text);
            }
        }
    }

    /// Search results and the imported file, or what the search is doing.
    fn results_card(&mut self, ui: &mut Ui, height: f32) {
        let t = self.theme;
        card(ui, &t, None, |ui| {
            ui.horizontal(|ui| {
                section_heading(ui, &t, "RESULTS");
                if let Some(outcome) = &self.draft.outcome {
                    let n = outcome.candidates.len();
                    if n > 0 {
                        ui.label(muted(&t, &n.to_string(), 12.5));
                    }
                }
            });
            let list_height = (height - 2.0 * CARD_PAD - 24.0).max(80.0);
            let searching = self.draft.is_searching();
            let outcome = self.draft.outcome.clone();
            let has_entries = self.draft.file.is_some()
                || outcome.as_ref().is_some_and(|o| !o.candidates.is_empty());
            if !has_entries {
                let (rect, _) = ui.allocate_exact_size(
                    Vec2::new(ui.available_width(), list_height),
                    Sense::hover(),
                );
                self.results_message(ui, rect, searching, outcome.as_deref());
                return;
            }
            let mut clicked = None;
            ScrollArea::vertical()
                .id_salt("lyrics-results")
                .max_height(list_height)
                .min_scrolled_height(list_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    if searching {
                        status_row(ui, &t, "Searching LRCLIB and KuGou…", true);
                    }
                    if let Some(outcome) = &outcome {
                        notes(ui, &t, outcome);
                    }
                    if let Some(file) = &self.draft.file {
                        let selected = self.draft.is_selected(Entry::File);
                        if file_card(ui, &t, file, selected).clicked() {
                            clicked = Some(Entry::File);
                        }
                    }
                    if let Some(outcome) = &outcome {
                        let song = self.draft.song.duration;
                        for (i, c) in outcome.candidates.iter().enumerate() {
                            let selected = self.draft.is_selected(Entry::Result(i));
                            if result_card(ui, &t, c, song, selected).clicked() {
                                clicked = Some(Entry::Result(i));
                            }
                        }
                    }
                });
            match clicked {
                Some(Entry::Result(i)) => self.draft.select_result(i),
                Some(Entry::File) => self.draft.select_file(),
                _ => {}
            }
        });
    }

    /// The message in an empty results list.
    fn results_message(
        &self,
        ui: &mut Ui,
        rect: Rect,
        searching: bool,
        outcome: Option<&SearchOutcome>,
    ) {
        let t = self.theme;
        let painter = ui.painter();
        painter.rect_filled(rect, 12, t.layer(5));
        let (title, detail) = if searching {
            let spinner =
                Rect::from_center_size(rect.center() - Vec2::new(0.0, 22.0), Vec2::splat(32.0));
            egui::Spinner::new()
                .size(32.0)
                .color(with_alpha(t.accent, 0.9))
                .paint_at(ui, spinner);
            (String::new(), "Searching LRCLIB and KuGou…".to_owned())
        } else if !self.online {
            (
                "Online lyrics are off".to_owned(),
                "Searching needs lyrics_online. Add an LRC file instead.".to_owned(),
            )
        } else {
            match outcome {
                None => (
                    "Find other lyrics".to_owned(),
                    "Search LRCLIB and KuGou, or add an LRC file.".to_owned(),
                ),
                Some(o) if o.instrumental => (
                    "Instrumental".to_owned(),
                    "LRCLIB lists this song as an instrumental.".to_owned(),
                ),
                Some(o) if !o.failures.is_empty() => (
                    "Search failed".to_owned(),
                    o.failures
                        .iter()
                        .map(|f| format!("{}: {}", f.provider, f.message))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Some(_) => (
                    "No lyrics found".to_owned(),
                    "Try another spelling, or add an LRC file.".to_owned(),
                ),
            }
        };
        let painter = ui.painter();
        let width = rect.width() - 64.0;
        let mut job = LayoutJob {
            wrap: TextWrapping::wrap_at_width(width),
            halign: Align::Center,
            ..Default::default()
        };
        if !title.is_empty() {
            job.append(
                &format!("{title}\n"),
                0.0,
                TextFormat::simple(font_bold(17.0), t.heading),
            );
        }
        let detail_color = match outcome {
            Some(o) if !searching && !o.failures.is_empty() && o.candidates.is_empty() => t.warning,
            _ => with_alpha(t.heading, 0.55),
        };
        job.append(
            &detail,
            0.0,
            TextFormat::simple(font_light(14.0), detail_color),
        );
        let galley = painter.layout_job(job);
        let y = if searching {
            rect.center().y + 6.0
        } else {
            rect.center().y - galley.size().y / 2.0
        };
        painter.galley(Pos2::new(rect.center().x, y), galley, t.heading);
    }

    /// The editable text that Save writes, with the playback position and the sync view's
    /// button.
    fn editor_card(
        &mut self,
        ui: &mut Ui,
        env: &Env<'_>,
        shown: &Shown,
        bottom: Option<f32>,
        body: f32,
    ) {
        let t = self.theme;
        let editor_id = Id::unique("lyrics-editor-text");
        card(ui, &t, bottom, |ui| {
            let top = ui.cursor().top();
            ui.horizontal(|ui| {
                section_heading(ui, &t, "EDITOR");
                let label = self.draft.selection.as_ref().map_or_else(
                    || "Pick a result or add a file".to_owned(),
                    |s| s.label.clone(),
                );
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let reset = chip(ui, &t, "Reset", false, self.draft.edited())
                        .on_hover_text("Discard your edits");
                    if reset.clicked() {
                        self.draft.reset_editor();
                    }
                    let sync = chip(ui, &t, "Sync", false, self.draft.can_sync())
                        .on_hover_text("Time the lines while the song plays");
                    if sync.clicked() {
                        self.draft.start_sync();
                    }
                    ui.add(egui::Label::new(muted(&t, &label, 13.0)).truncate());
                });
            });
            let status_height = 30.0;
            let end = bottom.map_or(top + body, |b| b - CARD_PAD - 1.0);
            let height = (end - ui.cursor().top() - status_height - 10.0).max(120.0);
            let width = ui.available_width();
            Frame::new()
                .fill(t.layer(5))
                .corner_radius(12)
                .inner_margin(Margin::same(4))
                .show(ui, |ui| {
                    ScrollArea::vertical()
                        .id_salt("lyrics-editor")
                        .max_height(height - 8.0)
                        .min_scrolled_height(height - 8.0)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            TextEdit::multiline(&mut self.draft.editor)
                                .id(editor_id)
                                .font(FontId::monospace(13.5))
                                .text_color(t.text)
                                .hint_text(
                                    egui::RichText::new(
                                        "[00:12.30] Paste or type LRC here, or plain lines",
                                    )
                                    .color(with_alpha(t.heading, 0.35)),
                                )
                                .desired_width(width - 8.0)
                                .min_size(Vec2::new(width - 8.0, height - 8.0))
                                .frame(Frame::new().inner_margin(Margin::symmetric(10, 8)))
                                .show(ui);
                        });
                });
            ui.horizontal(|ui| {
                ui.set_height(status_height);
                match editor_summary(&self.draft.editor) {
                    Some(summary) => ui.label(muted(&t, &summary, 12.5)),
                    None if self.draft.editor.trim().is_empty() => {
                        ui.label(muted(&t, "Empty", 12.5))
                    }
                    None => ui.label(
                        egui::RichText::new("No lyric lines")
                            .font(font_light(12.5))
                            .color(t.warning),
                    ),
                };
                if self.draft.edited() {
                    ui.label(muted(&t, "·  Edited", 12.5));
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let insert = chip(ui, &t, "Insert time", false, env.elapsed.is_some())
                        .on_hover_text("Put the playback position at the cursor as [mm:ss.xx]");
                    if let (true, Some(elapsed)) = (insert.clicked(), env.elapsed) {
                        self.insert_time(ui.ctx(), editor_id, shown, elapsed);
                    }
                    position_readout(ui, &t, env.elapsed, 14.0);
                });
            });
        });
    }

    /// Puts the timestamp for `elapsed` into the editor at its cursor and gives the editor
    /// the keyboard again, with the cursor after the timestamp.
    fn insert_time(&mut self, ctx: &egui::Context, id: Id, shown: &Shown, elapsed: Duration) {
        let state = TextEditState::load(ctx, id);
        let cursor = state
            .as_ref()
            .and_then(|s| s.cursor.char_range())
            .map(|r| r.as_sorted_char_range().start.0);
        let clock =
            SyncDoc::parse(&self.draft.editor).clock(shown.adjustments, self.draft.song.duration);
        let (text, index) = insert_stamp(&self.draft.editor, cursor, clock.file_time(elapsed));
        self.draft.editor = text;
        let mut state = state.unwrap_or_default();
        state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(index))));
        state.store(ctx, id);
        ctx.memory_mut(|m| m.request_focus(id));
    }

    /// Add LRC file on the left; Search, Cancel and Save on the right.
    fn footer(&mut self, ui: &mut Ui, env: &Env<'_>, out: &mut Output) {
        let t = self.theme;
        ui.horizontal(|ui| {
            ui.set_height(egui::Vec2::splat(super::export_panel::PILL_HEIGHT).y);
            let pending = self.picker.is_some();
            let label = if pending {
                "Choosing a file…"
            } else {
                "Add LRC file"
            };
            if add_file_button(ui, &t, label, !pending)
                .on_hover_text("Pick a .lrc or .txt file, or drop one on the window")
                .clicked()
            {
                self.pick_file();
            }
            let mut save = false;
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if pill(ui, &t, "Save", true, self.draft.can_save())
                    .on_hover_text("Show these lyrics for the song from now on")
                    .clicked()
                {
                    save = true;
                }
                if pill(ui, &t, "Cancel", false, true).clicked() {
                    out.close = true;
                }
                let search = pill(ui, &t, "Search", false, self.online);
                let search = if self.online {
                    search
                } else {
                    search.on_hover_text("Online lyrics are off (lyrics_online)")
                };
                if search.clicked() {
                    self.search();
                }
                if let Some(error) = &self.draft.error {
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(error)
                                .font(font_light(13.5))
                                .color(t.warning),
                        )
                        .truncate(),
                    );
                }
            });
            if save {
                self.save(env, out);
            }
        });
    }

    fn save(&mut self, env: &Env<'_>, out: &mut Output) {
        match env
            .provider
            .save_override(&self.draft.key, &self.draft.editor)
        {
            Ok(()) => {
                let detail = editor_summary(&self.draft.editor).unwrap_or_default();
                out.toasts
                    .push(Toast::new("Lyrics saved", SAVED_TOAST).detail(detail));
                out.close = true;
            }
            Err(e) => self.draft.error = Some(format!("Couldn't save: {e}")),
        }
    }

    // -----------------------------------------------------------------------------------------
    // Sync view

    fn sync_view(
        &mut self,
        ui: &mut Ui,
        env: &Env<'_>,
        shown: &Shown,
        list_height: f32,
        out: &mut Output,
    ) {
        let t = self.theme;
        let Some(clock) = self.sync_clock(shown) else {
            return;
        };
        self.header(ui, out);
        ui.add_space(16.0);
        self.sync_controls(ui, env, clock);
        ui.add_space(CARD_GAP);
        self.sync_list(ui, env, clock, list_height, out);
        ui.add_space(CARD_GAP);
        transport(ui, &t, env, self.draft.song.duration, true, out);
        ui.add_space(CARD_GAP);
        key_hints(ui, &t, SYNC_HINTS);
        ui.add_space(CARD_GAP);
        let Some(sync) = &self.draft.sync else {
            return;
        };
        let doc = &sync.session.doc;
        let early = doc.out_of_order().iter().filter(|&&e| e).count();
        let open = doc.unstamped();
        let note = if let Some(error) = &self.draft.error {
            Some((error.clone(), t.warning))
        } else if early > 0 {
            let s = if early == 1 { " is" } else { "s are" };
            Some((
                format!("{early} line{s} earlier than the line before"),
                t.warning,
            ))
        } else if open > 0 && doc.stamped() > 0 {
            let s = if open == 1 { "" } else { "s" };
            Some((
                format!("{open} line{s} without a time stay unsynced"),
                with_alpha(t.heading, 0.55),
            ))
        } else {
            None
        };
        let mut apply = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            ui.set_height(super::export_panel::PILL_HEIGHT);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                apply = pill(ui, &t, "Apply", true, true)
                    .on_hover_text("Write the times into the editor; Save keeps them")
                    .clicked();
                cancel = pill(ui, &t, "Cancel", false, true)
                    .on_hover_text("Back to the editor without the times (Esc)")
                    .clicked();
                if let Some((note, color)) = note {
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(note)
                                .font(font_light(13.5))
                                .color(color),
                        )
                        .truncate(),
                    );
                }
            });
        });
        if apply {
            self.draft.apply_sync();
        } else if cancel {
            self.draft.cancel_sync();
        }
    }

    /// The playback position, how many lines have times, and the gap and clear buttons.
    fn sync_controls(&mut self, ui: &mut Ui, env: &Env<'_>, clock: Clock) {
        let t = self.theme;
        let Some(sync) = self.draft.sync.as_mut() else {
            return;
        };
        card(ui, &t, None, |ui| {
            ui.horizontal(|ui| {
                section_heading(ui, &t, "SYNC");
                let doc = &sync.session.doc;
                let total = doc.lines.iter().filter(|l| !l.is_gap()).count();
                let done = doc
                    .lines
                    .iter()
                    .filter(|l| !l.is_gap() && l.time.is_some())
                    .count();
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.label(muted(&t, &format!("{done} of {total} lines timed"), 12.5));
                });
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                position_readout(ui, &t, env.elapsed, 34.0);
                ui.add_space(6.0);
                let state = match (env.elapsed, env.playing) {
                    (None, _) => "Not playing",
                    (Some(_), true) => "Playing",
                    (Some(_), false) => "Paused",
                };
                ui.label(muted(&t, state, 14.0));
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let playing = env.elapsed.is_some();
                    let clear = chip(ui, &t, "Clear", false, sync.session.doc.stamped() > 0)
                        .on_hover_text("Remove every line's time");
                    if clear.clicked() {
                        sync.session.doc.clear();
                        sync.session.next = sync.session.doc.first_open();
                        sync.scroll = true;
                    }
                    let gap = chip(ui, &t, "Gap", false, playing)
                        .on_hover_text("Mark an instrumental break before the next line now (G)");
                    if let (true, Some(e)) = (gap.clicked(), env.elapsed) {
                        sync.session.gap(clock.file_time(e));
                        sync.scroll = true;
                    }
                });
            });
        });
    }

    /// Every line with its time; the next line to stamp is outlined and the line showing at
    /// the playback position is in the accent colour.
    fn sync_list(
        &mut self,
        ui: &mut Ui,
        env: &Env<'_>,
        clock: Clock,
        height: f32,
        out: &mut Output,
    ) {
        let t = self.theme;
        let duration = self.draft.song.duration;
        let Some(sync) = self.draft.sync.as_mut() else {
            return;
        };
        let early = sync.session.doc.out_of_order();
        let active = env.elapsed.and_then(|e| clock.active(&sync.session.doc, e));
        card(ui, &t, None, |ui| {
            ScrollArea::vertical()
                .id_salt("lyrics-sync")
                .max_height(height)
                .min_scrolled_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let width = ui.available_width();
                    let mut action = None;
                    let count = sync.session.doc.lines.len();
                    for (i, (line, &early)) in sync.session.doc.lines.iter().zip(&early).enumerate()
                    {
                        let row = SyncRow {
                            index: i,
                            time: line.time,
                            text: &line.text,
                            next: sync.session.next == Some(i),
                            active: active == Some(i),
                            early,
                        };
                        let (rect, row_action) =
                            sync_row(ui, &t, &row, width, env.elapsed.is_some());
                        let last = i + 1 == count && sync.session.next.is_none();
                        if sync.scroll && (row.next || last) {
                            ui.scroll_to_rect(rect, Some(Align::Center));
                        }
                        if row_action.is_some() {
                            action = row_action.map(|a| (i, a));
                        }
                    }
                    sync.scroll = false;
                    let Some((i, action)) = action else {
                        return;
                    };
                    match action {
                        RowAction::Select => sync.session.select(i),
                        RowAction::Seek => {
                            if let Some(time) = sync.session.doc.lines[i].time {
                                let at = clock.song_time(time).saturating_sub(LEAD_IN);
                                out.commands.push(Command::SeekTo(clamp_seek(at, duration)));
                            }
                        }
                        RowAction::Nudge(ms) => sync.session.doc.nudge(i, ms),
                        RowAction::StampNow => {
                            if let Some(e) = env.elapsed {
                                sync.session.doc.set(i, Some(clock.file_time(e)));
                            }
                        }
                    }
                });
        });
    }

    // -----------------------------------------------------------------------------------------
    // Timing view

    fn timing_view(&mut self, ui: &mut Ui, env: &Env<'_>, shown: &Shown, out: &mut Output) {
        let t = self.theme;
        self.header(ui, out);
        ui.add_space(16.0);
        self.current_card(ui, env, shown, out);
        ui.add_space(CARD_GAP);
        let synced = shown.info.as_ref().is_some_and(|i| i.synced);
        let adj = shown.adjustments;
        let mut next = adj;
        card(ui, &t, None, |ui| {
            section_heading(ui, &t, "TIMING");
            live_lines(ui, &t, &shown.state, env.elapsed);
            if !env.lyrics_visible {
                ui.label(muted(
                    &t,
                    "Lyrics are hidden on the display; press y after closing.",
                    12.5,
                ));
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for &(step, label) in &OFFSET_STEPS[..2] {
                    if chip(ui, &t, label, false, synced).clicked() {
                        next.offset_ms = nudged(adj.offset_ms, step);
                    }
                }
                let (rect, _) = ui.allocate_exact_size(Vec2::new(86.0, 30.0), Sense::hover());
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    fmt_offset(adj.offset_ms),
                    font_bold(17.0),
                    if synced {
                        t.heading
                    } else {
                        with_alpha(t.heading, 0.4)
                    },
                );
                for &(step, label) in &OFFSET_STEPS[2..] {
                    if chip(ui, &t, label, false, synced).clicked() {
                        next.offset_ms = nudged(adj.offset_ms, step);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    let reset = chip(ui, &t, "Reset", false, adj.offset_ms != 0)
                        .on_hover_text("Back to the file's own timing");
                    if reset.clicked() {
                        next.offset_ms = 0;
                    }
                });
            });
            ui.label(muted(
                &t,
                if synced {
                    "Positive values show every line later, negative earlier."
                } else {
                    "Timing applies to synced lyrics."
                },
                12.5,
            ));
            ui.add_space(2.0);
            let blocker = stretch_blocker(shown.info.as_deref(), self.draft.song.duration);
            let width = ui.available_width();
            let stretch = switch_cell(
                ui,
                &t,
                "Stretch to song length",
                adj.stretch,
                blocker.is_none(),
                width,
            );
            let stretch = match blocker {
                Some(why) => stretch.on_hover_text(why),
                None => stretch.on_hover_text(
                    "Scales line times by the song's length over the lyrics' [length:] tag, for \
                     sped-up or slowed versions",
                ),
            };
            if stretch.clicked() {
                next.stretch = !adj.stretch;
            }
            ui.label(muted(
                &t,
                "Changes show at once and are kept for this song.",
                12.5,
            ));
        });
        if next != adj {
            if let Err(e) = env.provider.set_adjustments(&self.draft.key, next) {
                self.draft.error = Some(format!("Couldn't save the timing: {e}"));
            }
        }
        ui.add_space(CARD_GAP);
        transport(ui, &t, env, self.draft.song.duration, false, out);
        ui.add_space(CARD_GAP);
        key_hints(ui, &t, PLAYBACK_HINTS);
        ui.add_space(CARD_GAP);
        ui.horizontal(|ui| {
            ui.set_height(super::export_panel::PILL_HEIGHT);
            if let Some(error) = &self.draft.error {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(error)
                            .font(font_light(13.5))
                            .color(t.warning),
                    )
                    .truncate(),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if pill(ui, &t, "Done", true, true).clicked() {
                    out.close = true;
                }
                if pill(ui, &t, "Back", false, true).clicked() {
                    self.draft.timing = false;
                }
            });
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Pieces

/// The previous, active and next lyric lines at the playback position, with their times.
fn live_lines(ui: &mut Ui, t: &PanelTheme, state: &LyricsState, elapsed: Option<Duration>) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 112.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 12, t.layer(5));
    let lines = match state {
        LyricsState::Found(l) => match l.as_ref() {
            Lyrics::Synced(lines) if !lines.is_empty() => Some((l, lines)),
            _ => None,
        },
        _ => None,
    };
    let message = match (lines, elapsed) {
        (None, _) => Some("No synced lyrics to time"),
        (Some(_), None) => Some("The song isn't playing"),
        _ => None,
    };
    if let Some(message) = message {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            message,
            font_light(14.0),
            with_alpha(t.heading, 0.5),
        );
        return;
    }
    let (Some((lyrics, lines)), Some(elapsed)) = (lines, elapsed) else {
        return;
    };
    let current = lyrics.current_line(elapsed);
    let text_left = rect.left() + 64.0;
    let text_width = rect.right() - 14.0 - text_left;
    let rows: [(Option<usize>, bool); 3] = [
        (current.and_then(|i| i.checked_sub(1)), false),
        (current, true),
        (current.map_or(Some(0), |i| Some(i + 1)), false),
    ];
    for (row, (index, active)) in rows.into_iter().enumerate() {
        let y = rect.top() + 22.0 + row as f32 * 34.0;
        let Some(line) = index.and_then(|i| lines.get(i)) else {
            if active {
                painter.text(
                    Pos2::new(text_left, y),
                    Align2::LEFT_CENTER,
                    "• • •",
                    font_bold(16.0),
                    with_alpha(t.heading, 0.6),
                );
            }
            continue;
        };
        let text = match line.text.trim() {
            "" => "• • •",
            text => text,
        };
        let (font, color) = if active {
            (font_bold(17.0), t.heading)
        } else {
            (font_light(14.0), with_alpha(t.heading, 0.45))
        };
        painter.text(
            Pos2::new(rect.left() + 14.0, y),
            Align2::LEFT_CENTER,
            fmt_time(line.time),
            font_light(12.5),
            if active {
                t.accent
            } else {
                with_alpha(t.heading, 0.35)
            },
        );
        let galley = one_line(painter, text, font, color, text_width);
        painter.galley(
            Pos2::new(text_left, y - galley.size().y / 2.0),
            galley,
            color,
        );
    }
}

/// Play/pause, a step back and forward, and a seek bar between the playback position and the
/// song's length, for the panel's song while it is playing. The steps are the sync view's
/// (`sync`) or the other views' and seek like their ← and → keys.
fn transport(
    ui: &mut Ui,
    t: &PanelTheme,
    env: &Env<'_>,
    duration: Option<Duration>,
    sync: bool,
    out: &mut Output,
) {
    let live = env.elapsed.is_some();
    let step = if sync { SEEK_STEP } else { TRANSPORT_STEP };
    let keys = |key: &str| {
        if sync {
            format!("({key})")
        } else {
            format!("({key}, or Alt+{key} in a text field)")
        }
    };
    ui.horizontal(|ui| {
        ui.set_height(TRANSPORT_HEIGHT);
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, response) = hit(ui, Vec2::splat(TRANSPORT_HEIGHT), live);
        paint_play_button(ui, t, rect, &response, env.playing, live);
        let label = if env.playing { "Pause" } else { "Play" };
        if response
            .on_hover_text(format!("{label} {}", keys("P")))
            .clicked()
        {
            out.commands.push(Command::TogglePause);
        }
        let secs = step.as_secs();
        for (forward, text, key) in [(false, "−", "←"), (true, "+", "→")] {
            let what = if forward { "Forward" } else { "Back" };
            let response = chip(ui, t, &format!("{text}{secs} s"), false, live)
                .on_hover_text(format!("{what} {secs} seconds {}", keys(key)));
            if let (true, Some(e)) = (response.clicked(), env.elapsed) {
                out.commands
                    .push(Command::SeekTo(seek_target(e, step, forward, duration)));
            }
        }
        ui.add_space(8.0);

        // The times' places are taken first and painted after the bar, which decides the
        // position they show while it is dragged.
        let font = FontId::monospace(13.5);
        let painter = ui.painter();
        let measure = |text: &str| {
            painter
                .layout_no_wrap(text.to_owned(), font.clone(), t.heading)
                .size()
                .x
        };
        let total = duration.map_or_else(|| "-:--".to_owned(), fmt_time);
        let elapsed_width = measure("00:00.00");
        let total_width = measure(&total);
        let (elapsed_rect, _) =
            ui.allocate_exact_size(Vec2::new(elapsed_width, TRANSPORT_HEIGHT), Sense::hover());
        ui.add_space(8.0);
        let bar_width = (ui.available_width() - total_width - 8.0 - 2.0 * 6.0).max(40.0);
        let (bar_rect, _) =
            ui.allocate_exact_size(Vec2::new(bar_width, TRANSPORT_HEIGHT), Sense::hover());
        ui.add_space(8.0);
        let (total_rect, _) =
            ui.allocate_exact_size(Vec2::new(total_width, TRANSPORT_HEIGHT), Sense::hover());

        let length = duration.filter(|d| !d.is_zero());
        let progress = match (env.elapsed, length) {
            (Some(e), Some(d)) => (e.as_secs_f32() / d.as_secs_f32()).clamp(0.0, 1.0),
            _ => 0.0,
        };
        let track = Rect::from_center_size(
            bar_rect.center(),
            Vec2::new(bar_rect.width(), TRANSPORT_TRACK),
        );
        let ink = Ink {
            text: t.accent,
            soft: t.body,
            track: t.layer(30),
        };
        let bar = seek_bar(
            ui,
            Id::unique("lyrics-transport-seek"),
            track,
            progress,
            live && length.is_some(),
            &ink,
        );
        let target = |fraction: f32| length.map(|d| clamp_seek(d.mul_f32(fraction), duration));
        if let Some(to) = bar.seek.and_then(target) {
            out.commands.push(Command::SeekTo(to));
        }
        let (shown, color) = if bar.scrubbing {
            (target(bar.shown), t.accent)
        } else {
            (env.elapsed, t.heading)
        };
        let painter = ui.painter();
        let (text, color) = match shown {
            Some(e) => (fmt_clock(e), color),
            None => ("-:--.--".to_owned(), with_alpha(t.heading, 0.35)),
        };
        painter.text(
            elapsed_rect.right_center(),
            Align2::RIGHT_CENTER,
            text,
            font.clone(),
            color,
        );
        painter.text(
            total_rect.left_center(),
            Align2::LEFT_CENTER,
            total,
            font,
            with_alpha(t.heading, 0.55),
        );
    });
}

/// The transport's play/pause button: an accent disc with the icon of what it does.
fn paint_play_button(
    ui: &Ui,
    t: &PanelTheme,
    rect: Rect,
    response: &Response,
    playing: bool,
    enabled: bool,
) {
    let mut painter = ui.painter().clone();
    if !enabled {
        painter.multiply_opacity(0.35);
    }
    let hovered = enabled && response.hovered();
    let pressed = enabled && response.is_pointer_button_down_on();
    let r = rect.width() / 2.0 * if pressed { 0.92 } else { 1.0 };
    painter.circle_filled(
        rect.center(),
        r,
        with_alpha(t.accent, if hovered { 1.0 } else { 0.9 }),
    );
    let icon = if playing { Icon::Pause } else { Icon::Play };
    // The play triangle sits a little right of centre to look centred.
    let nudge = if playing { 0.0 } else { 1.0 };
    paint_icon(
        &painter,
        icon,
        rect.center() + Vec2::new(nudge, 0.0),
        r * 0.85,
        t.on_accent,
    );
}

/// The playback position as `mm:ss.xx` in monospace, or blanks when the song isn't playing.
fn position_readout(ui: &mut Ui, t: &PanelTheme, elapsed: Option<Duration>, size: f32) {
    let (text, color) = match elapsed {
        Some(e) => (fmt_position(e), t.heading),
        None => (NO_TIME.to_owned(), with_alpha(t.heading, 0.35)),
    };
    ui.label(
        egui::RichText::new(text)
            .font(FontId::monospace(size))
            .color(color),
    );
}

/// What a sync row shows.
struct SyncRow<'a> {
    index: usize,
    time: Option<Duration>,
    text: &'a str,
    /// The next line to stamp.
    next: bool,
    /// Showing at the playback position.
    active: bool,
    /// Earlier than the line before.
    early: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowAction {
    Select,
    /// Play from just before the line.
    Seek,
    Nudge(i64),
    StampNow,
}

/// One line of the sync list: its time, which seeks there; its text, which selects it as the
/// next line to stamp; and, on the hovered and the next line, ± nudges and a stamp button.
/// `live` is whether the song is playing in the panel, which seeking and stamping need.
fn sync_row(
    ui: &mut Ui,
    t: &PanelTheme,
    row: &SyncRow<'_>,
    width: f32,
    live: bool,
) -> (Rect, Option<RowAction>) {
    let (rect, response) = hit(ui, Vec2::new(width, SYNC_ROW), true);
    let response = response.on_hover_text("Stamp this line next");
    let painter = ui.painter().clone();
    let hovered = ui.rect_contains_pointer(rect);
    let mut action = response.clicked().then_some(RowAction::Select);

    if row.next {
        painter.rect(
            rect,
            10,
            t.wash(22),
            Stroke::new(1.5, with_alpha(t.accent, 0.9)),
            StrokeKind::Inside,
        );
    } else if hovered {
        painter.rect_filled(rect, 10, t.layer(10));
    }
    if row.active {
        let bar = Rect::from_center_size(
            Pos2::new(rect.left() + 5.0, rect.center().y),
            Vec2::new(3.0, SYNC_ROW - 16.0),
        );
        painter.rect_filled(bar, 1.5, t.accent);
    }

    // Time: seeks to just before the line.
    let time_rect = Rect::from_min_size(
        Pos2::new(rect.left() + 12.0, rect.top() + 5.0),
        Vec2::new(TIME_COLUMN - 12.0, SYNC_ROW - 10.0),
    );
    let seekable = live && row.time.is_some();
    let time_sense = if seekable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let mut time_resp = ui.interact(
        time_rect,
        Id::unique(("lyrics-sync-time", row.index)),
        time_sense,
    );
    if seekable {
        time_resp = time_resp
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Play from 1 s before this line");
        if time_resp.hovered() {
            painter.rect_filled(time_rect, 8, t.layer(18));
        }
        if time_resp.clicked() {
            action = Some(RowAction::Seek);
        }
    }
    if row.early {
        time_resp.on_hover_text("Earlier than the line before");
    }
    let time_color = if row.early {
        t.warning
    } else if row.active {
        t.accent
    } else if row.time.is_some() {
        t.text
    } else {
        with_alpha(t.heading, 0.35)
    };
    let time_text = row.time.map_or_else(|| NO_TIME.to_owned(), fmt_position);
    painter.text(
        time_rect.center(),
        Align2::CENTER_CENTER,
        time_text,
        FontId::monospace(14.0),
        time_color,
    );

    // Buttons from the right: stamp now, +, −.
    let controls = hovered || row.next;
    let mut right = rect.right() - 8.0;
    if controls {
        let mut button = |id: &str, enabled: bool| {
            let r = Rect::from_min_size(
                Pos2::new(right - NUDGE_SIZE, rect.center().y - NUDGE_SIZE / 2.0),
                Vec2::splat(NUDGE_SIZE),
            );
            right -= NUDGE_SIZE + 4.0;
            let sense = if enabled {
                Sense::click()
            } else {
                Sense::hover()
            };
            let mut resp = ui.interact(r, Id::unique((id, row.index)), sense);
            if enabled {
                resp = resp.on_hover_cursor(CursorIcon::PointingHand);
            }
            (r, resp)
        };
        let (r, stamp) = button("sync-stamp", live);
        let stamp = stamp.on_hover_text("Set to the playback position");
        let mut p = painter.clone();
        if !live {
            p.multiply_opacity(0.35);
        }
        p.rect_filled(r, 8, t.wash(if live && stamp.hovered() { 36 } else { 20 }));
        paint_glyph(
            &p,
            Glyph::Timer,
            Rect::from_center_size(r.center(), Vec2::splat(17.0)),
            t.heading,
        );
        if stamp.clicked() {
            action = Some(RowAction::StampNow);
        }
        let timed = row.time.is_some();
        for (text, ms) in [("+", LINE_NUDGE_MS), ("−", -LINE_NUDGE_MS)] {
            let (r, resp) = button(if ms > 0 { "sync-later" } else { "sync-earlier" }, timed);
            let resp = resp.on_hover_text(if ms > 0 {
                "0.1 s later"
            } else {
                "0.1 s earlier"
            });
            let mut p = painter.clone();
            if !timed {
                p.multiply_opacity(0.35);
            }
            paint_nudge(&p, t, r, text, timed && resp.hovered());
            if resp.clicked() {
                action = Some(RowAction::Nudge(ms));
            }
        }
        right -= 6.0;
    }

    // Text.
    let gap = row.text.trim().is_empty();
    let text = if gap { "• • •" } else { row.text };
    let (font, color) = if row.active {
        (font_bold(15.5), t.accent)
    } else if gap {
        (font_bold(14.0), with_alpha(t.heading, 0.45))
    } else if row.time.is_some() || row.next {
        (font_light(15.0), t.heading)
    } else {
        (font_light(15.0), with_alpha(t.heading, 0.6))
    };
    let left = rect.left() + TIME_COLUMN + 10.0;
    let galley = one_line(&painter, text, font, color, right - left);
    painter.galley(
        Pos2::new(left, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    (rect, action)
}

/// Key caps and what they do, for [`key_hints`].
type Hints = [(&'static [&'static str], &'static str)];

/// The sync view's keys.
const SYNC_HINTS: &Hints = &[
    (&["Space"], "stamp"),
    (&["Bksp"], "back"),
    (&["←", "→"], "3 s"),
    (&["P"], "play/pause"),
    (&["G"], "gap"),
    (&["Esc"], "cancel"),
];

/// The playback keys of the editor and timing views.
const PLAYBACK_HINTS: &Hints = &[
    (&["←", "→"], "5 s"),
    (&["P"], "play/pause"),
    (&["Alt"], "with them in a text field"),
];

/// A centred row of outlined key caps with what they do.
fn key_hints(ui: &mut Ui, t: &PanelTheme, hints: &Hints) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
    let painter = ui.painter();
    // Laid out once to centre the row.
    let measure = |painter: &Painter| {
        let mut x = 0.0;
        for (i, (caps, label)) in hints.iter().enumerate() {
            if i > 0 {
                x += 18.0;
            }
            for cap in caps.iter() {
                x += paint_key_cap(painter, t, cap, Pos2::new(-10_000.0, -10_000.0), 1.0) + 4.0;
            }
            x += 2.0
                + painter
                    .layout_no_wrap((*label).to_owned(), font_light(13.0), t.body)
                    .size()
                    .x;
        }
        x
    };
    let total = measure(painter);
    let mut x = rect.center().x - total / 2.0;
    let mid = rect.center().y;
    for (i, (caps, label)) in hints.iter().enumerate() {
        if i > 0 {
            x += 18.0;
        }
        for cap in caps.iter() {
            x += paint_key_cap(painter, t, cap, Pos2::new(x, mid), 1.0) + 4.0;
        }
        x += 2.0;
        let galley = painter.layout_no_wrap((*label).to_owned(), font_light(13.0), t.body);
        let w = galley.size().x;
        painter.galley(
            Pos2::new(x, mid - galley.size().y / 2.0 + 1.0),
            galley,
            t.body,
        );
        x += w;
    }
}

/// A one-row status message with an optional spinner.
fn status_row(ui: &mut Ui, t: &PanelTheme, text: &str, spinner: bool) {
    ui.horizontal(|ui| {
        if spinner {
            ui.add(egui::Spinner::new().size(16.0).color(t.accent));
        }
        ui.label(muted(t, text, 13.5));
    });
}

/// Provider failures and the instrumental flag above a list of results.
fn notes(ui: &mut Ui, t: &PanelTheme, outcome: &SearchOutcome) {
    if outcome.instrumental {
        ui.label(muted(t, "LRCLIB lists this song as an instrumental.", 13.0));
    }
    for failure in &outcome.failures {
        ui.add(
            egui::Label::new(
                egui::RichText::new(format!("{} failed: {}", failure.provider, failure.message))
                    .font(font_light(13.0))
                    .color(t.warning),
            )
            .truncate(),
        );
    }
}

/// A search result: provider, kind, match quality, title and artist, length and the first
/// lyric lines.
fn result_card(
    ui: &mut Ui,
    t: &PanelTheme,
    c: &Candidate,
    song: Option<Duration>,
    selected: bool,
) -> Response {
    let kind = if c.synced() { "Synced" } else { "Plain" };
    let name: Vec<&str> = [c.title.trim(), c.artist.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    let quality = match_label(c);
    let quality_color = if c.is_sure_match() {
        t.accent
    } else if c.is_good_match() {
        t.body
    } else {
        t.warning
    };
    let length = fmt_length(c.duration, song);
    let length_color = if c.duration_matches() {
        with_alpha(t.heading, 0.55)
    } else {
        t.warning
    };
    entry_card(
        ui,
        t,
        selected,
        EntryText {
            badge: c.provider.name(),
            kind,
            tag: Some((quality, quality_color)),
            name: name.join("  ·  "),
            side: length.map(|l| (l, length_color)),
            lines: preview_lines(&c.lyrics, PREVIEW_LINES),
        },
    )
}

/// The imported file as a result.
fn file_card(ui: &mut Ui, t: &PanelTheme, file: &Imported, selected: bool) -> Response {
    let lyrics = parse_lrc(&file.text);
    let kind = match &lyrics {
        Lyrics::Synced(_) => "Synced",
        Lyrics::Plain(_) if lyrics.is_empty() => "Empty",
        Lyrics::Plain(_) => "Plain",
    };
    entry_card(
        ui,
        t,
        selected,
        EntryText {
            badge: "FILE",
            kind,
            tag: None,
            name: file.name.clone(),
            side: None,
            lines: preview_lines(&lyrics, PREVIEW_LINES),
        },
    )
}

struct EntryText {
    badge: &'static str,
    kind: &'static str,
    tag: Option<(&'static str, Color32)>,
    name: String,
    side: Option<(String, Color32)>,
    lines: Vec<String>,
}

fn entry_card(ui: &mut Ui, t: &PanelTheme, selected: bool, e: EntryText) -> Response {
    let width = ui.available_width();
    let inner = width - 2.0 * ENTRY_PAD;
    let painter = ui.painter().clone();
    let badge = painter.layout_no_wrap(e.badge.into(), font_bold(11.0), t.on_accent);
    let kind = painter.layout_no_wrap(e.kind.into(), font_light(13.0), t.body);
    let tag = e.tag.map(|(text, color)| {
        (
            painter.layout_no_wrap(text.into(), font_light(13.0), color),
            color,
        )
    });
    let side = e
        .side
        .map(|(text, color)| (painter.layout_no_wrap(text, font_light(13.0), color), color));
    let side_width = side.as_ref().map_or(0.0, |(g, _)| g.size().x + 12.0);
    let name = one_line(
        &painter,
        &e.name,
        font_bold(15.0),
        t.heading,
        inner - side_width,
    );
    let lines: Vec<Arc<Galley>> = e
        .lines
        .iter()
        .map(|l| {
            one_line(
                &painter,
                l,
                font_light(13.5),
                with_alpha(t.heading, 0.55),
                inner,
            )
        })
        .collect();
    let row1 = 20.0;
    let row2 = name.size().y;
    let lines_height: f32 = lines.iter().map(|g| g.size().y + 2.0).sum();
    let height = 2.0 * ENTRY_PAD + row1 + 6.0 + row2 + 6.0 + lines_height;
    let (rect, response) = hit(ui, Vec2::new(width, height), true);
    paint_entry(ui, t, rect, &response, selected);

    let left = rect.left() + ENTRY_PAD;
    let mut y = rect.top() + ENTRY_PAD;
    let badge_rect =
        Rect::from_min_size(Pos2::new(left, y), Vec2::new(badge.size().x + 14.0, row1));
    painter.rect_filled(badge_rect, row1 / 2.0, with_alpha(t.accent, 0.85));
    painter.galley(badge_rect.center() - badge.size() / 2.0, badge, t.on_accent);
    painter.galley(
        Pos2::new(badge_rect.right() + 10.0, y + (row1 - kind.size().y) / 2.0),
        kind,
        t.body,
    );
    if let Some((tag, color)) = tag {
        painter.galley(
            Pos2::new(
                rect.right() - ENTRY_PAD - tag.size().x,
                y + (row1 - tag.size().y) / 2.0,
            ),
            tag,
            color,
        );
    }
    y += row1 + 6.0;
    painter.galley(Pos2::new(left, y), name, t.heading);
    if let Some((side, color)) = side {
        painter.galley(
            Pos2::new(
                rect.right() - ENTRY_PAD - side.size().x,
                y + (row2 - side.size().y) / 2.0,
            ),
            side,
            color,
        );
    }
    y += row2 + 6.0;
    for galley in lines {
        let h = galley.size().y;
        painter.galley(Pos2::new(left, y), galley, t.heading);
        y += h + 2.0;
    }
    response
}

/// Background of a selectable card: a soft layer, lighter on hover, outlined in the accent
/// when selected.
fn paint_entry(ui: &Ui, t: &PanelTheme, rect: Rect, response: &Response, selected: bool) {
    let painter = ui.painter();
    let hovered = response.hovered() && response.sense.senses_click();
    let fill = if selected {
        t.wash(if hovered { 30 } else { 22 })
    } else {
        t.layer(if hovered { 14 } else { 7 })
    };
    let stroke = if selected {
        Stroke::new(1.5, with_alpha(t.accent, 0.9))
    } else {
        Stroke::new(1.0, t.layer(12))
    };
    painter.rect(rect, 14, fill, stroke, StrokeKind::Inside);
}

/// Text on one line, cut with an ellipsis at `width`.
fn one_line(
    painter: &Painter,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping::truncate_at_width(width.max(10.0));
    painter.layout_job(job)
}

/// "Add LRC file" with a plus in a circle, as a borderless button.
fn add_file_button(ui: &mut Ui, t: &PanelTheme, text: &str, enabled: bool) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font_bold(15.0), t.heading);
    let size = Vec2::new(galley.size().x + 52.0, super::export_panel::PILL_HEIGHT);
    let (rect, response) = hit(ui, size, enabled);
    let mut painter = ui.painter().clone();
    if !enabled {
        painter.multiply_opacity(0.5);
    }
    if enabled && response.hovered() {
        painter.rect_filled(rect, rect.height() / 2.0, t.layer(14));
    }
    let icon = Rect::from_center_size(
        Pos2::new(rect.left() + 22.0, rect.center().y),
        Vec2::splat(22.0),
    );
    paint_glyph(&painter, Glyph::Plus, icon, t.text);
    painter.galley(
        Pos2::new(icon.right() + 10.0, rect.center().y - galley.size().y / 2.0),
        galley,
        t.heading,
    );
    response
}

/// Darkens the panel and asks for the file while lyrics files are dragged over the window.
fn drop_hint(ctx: &egui::Context, t: &PanelTheme, rect: Rect) {
    let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::unique("lyrics-drop")));
    painter.rect(
        rect,
        20,
        with_alpha(t.card(), 0.94),
        Stroke::new(2.0, with_alpha(t.accent, 0.9)),
        StrokeKind::Inside,
    );
    let icon = Rect::from_center_size(rect.center() - Vec2::new(0.0, 30.0), Vec2::splat(40.0));
    paint_glyph(&painter, Glyph::Plus, icon, t.accent);
    painter.text(
        rect.center() + Vec2::new(0.0, 16.0),
        Align2::CENTER_TOP,
        "Drop to add lyrics",
        font_bold(22.0),
        t.heading,
    );
    painter.text(
        rect.center() + Vec2::new(0.0, 48.0),
        Align2::CENTER_TOP,
        ".lrc or .txt",
        font_light(14.0),
        t.muted,
    );
}

// ---------------------------------------------------------------------------------------------
// Icons

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Glyph {
    Close,
    /// An arrow pointing down-left onto a line: fill the field from the song.
    Fill,
    Copy,
    Timer,
    Trash,
    /// A page of text lines.
    Lines,
    /// A plus in a circle.
    Plus,
}

/// An icon button over `rect`: a soft circle on hover, an accent wash when `on`.
fn paint_icon_button(
    ui: &Ui,
    t: &PanelTheme,
    glyph: Glyph,
    rect: Rect,
    response: &Response,
    on: bool,
) {
    paint_icon_on(ui.painter(), t, glyph, rect, response.hovered(), on);
}

fn paint_icon_on(
    painter: &Painter,
    t: &PanelTheme,
    glyph: Glyph,
    rect: Rect,
    hovered: bool,
    on: bool,
) {
    let r = rect.width().min(rect.height()) / 2.0;
    if on {
        painter.circle_filled(rect.center(), r, t.wash(if hovered { 60 } else { 44 }));
    } else if hovered {
        painter.circle_filled(rect.center(), r, t.layer(22));
    }
    let color = if on || hovered { t.heading } else { t.text };
    let icon = Rect::from_center_size(rect.center(), Vec2::splat(r * 1.15));
    paint_glyph(painter, glyph, icon, color);
}

/// Paints an outlined icon filling `rect`.
fn paint_glyph(painter: &Painter, glyph: Glyph, rect: Rect, color: Color32) {
    let u = rect.width() / 20.0;
    let p = |x: f32, y: f32| rect.min + Vec2::new(x, y) * u;
    let stroke = Stroke::new((1.6 * u).max(1.0), color);
    match glyph {
        Glyph::Close => {
            painter.line_segment([p(5.5, 5.5), p(14.5, 14.5)], stroke);
            painter.line_segment([p(5.5, 14.5), p(14.5, 5.5)], stroke);
        }
        Glyph::Fill => {
            painter.line_segment([p(4.0, 17.0), p(16.0, 17.0)], stroke);
            painter.line_segment([p(5.5, 13.5), p(14.5, 4.5)], stroke);
            painter.line(vec![p(5.5, 7.5), p(5.5, 13.5), p(11.5, 13.5)], stroke);
        }
        Glyph::Copy => {
            let back = Rect::from_min_max(p(3.0, 3.0), p(12.5, 12.5));
            let front = Rect::from_min_max(p(7.5, 7.5), p(17.0, 17.0));
            painter.rect_stroke(back, 2.5 * u, stroke, StrokeKind::Middle);
            painter.rect_filled(front, 2.5 * u, Color32::TRANSPARENT);
            painter.rect_stroke(front, 2.5 * u, stroke, StrokeKind::Middle);
        }
        Glyph::Timer => {
            let c = p(10.0, 11.5);
            painter.circle_stroke(c, 6.5 * u, stroke);
            painter.line_segment([c, p(10.0, 7.5)], stroke);
            painter.line_segment([p(8.0, 2.5), p(12.0, 2.5)], stroke);
            painter.line_segment([p(10.0, 2.5), p(10.0, 5.0)], stroke);
        }
        Glyph::Trash => {
            painter.line_segment([p(3.5, 5.5), p(16.5, 5.5)], stroke);
            painter.line(
                vec![p(8.0, 5.5), p(8.0, 3.0), p(12.0, 3.0), p(12.0, 5.5)],
                stroke,
            );
            painter.line(
                vec![p(5.0, 5.5), p(6.0, 17.0), p(14.0, 17.0), p(15.0, 5.5)],
                stroke,
            );
            painter.line_segment([p(8.5, 8.5), p(8.5, 14.0)], stroke);
            painter.line_segment([p(11.5, 8.5), p(11.5, 14.0)], stroke);
        }
        Glyph::Lines => {
            let page = Rect::from_min_max(p(3.5, 2.5), p(16.5, 17.5));
            painter.rect_stroke(page, 3.0 * u, stroke, StrokeKind::Middle);
            for y in [7.0, 10.0, 13.0] {
                painter.line_segment([p(6.5, y), p(13.5, y)], stroke);
            }
        }
        Glyph::Plus => {
            painter.circle_stroke(p(10.0, 10.0), 8.0 * u, stroke);
            painter.line_segment([p(10.0, 6.0), p(10.0, 14.0)], stroke);
            painter.line_segment([p(6.0, 10.0), p(14.0, 10.0)], stroke);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::Origin as PlayerOrigin;

    fn song(title: &str) -> SongInfo {
        SongInfo {
            id: 1,
            file: format!("{title}.flac"),
            title: title.into(),
            artist: "Kendrick Lamar".into(),
            album: "Mr. Morale".into(),
            duration: Some(Duration::from_secs(203)),
            origin: PlayerOrigin::Mpd,
        }
    }

    fn candidate(text: &str, score: f32) -> Candidate {
        Candidate {
            provider: encore::lyrics::Provider::Lrclib,
            title: "Rich Spirit".into(),
            artist: "Kendrick Lamar".into(),
            album: String::new(),
            duration: Some(Duration::from_secs(205)),
            text: text.into(),
            lyrics: Arc::new(parse_lrc(text)),
            score,
            duration_diff: Some(Duration::from_secs(2)),
        }
    }

    fn outcome(candidates: Vec<Candidate>) -> Arc<SearchOutcome> {
        Arc::new(SearchOutcome {
            candidates,
            ..Default::default()
        })
    }

    #[test]
    fn requests_follow_the_field() {
        let mut d = Draft::new(song("Rich Spirit"));
        assert_eq!(d.song_query(), "Kendrick Lamar - Rich Spirit");
        let req = d.request();
        assert_eq!(req.query, None);
        assert_eq!(req.title, "Rich Spirit");
        assert_eq!(req.duration, Some(Duration::from_secs(203)));
        d.query = "  rich spirit  ".into();
        assert_eq!(d.request().query.as_deref(), Some("rich spirit"));
        d.query = "   ".into();
        assert_eq!(d.request().query, None);
    }

    #[test]
    fn search_outcomes_are_taken_by_token() {
        let mut d = Draft::new(song("A"));
        d.searching(4);
        assert!(d.is_searching());
        d.poll_search(&SearchState::Searching(4));
        assert!(d.is_searching());
        d.poll_search(&SearchState::Done(3, outcome(vec![])));
        assert!(d.is_searching() && d.outcome.is_none());
        d.poll_search(&SearchState::Done(
            4,
            outcome(vec![candidate("[00:01.00]a", 1.0)]),
        ));
        assert!(!d.is_searching());
        assert_eq!(d.outcome.as_ref().unwrap().candidates.len(), 1);
        d.searching(5);
        d.poll_search(&SearchState::Idle);
        assert!(!d.is_searching());
    }

    #[test]
    fn selecting_fills_the_editor_and_tracks_edits() {
        let mut d = Draft::new(song("A"));
        d.refresh_current(Some("[00:01.00]current"));
        assert_eq!(d.selection.as_ref().unwrap().entry, Entry::Current);
        assert_eq!(d.editor, "[00:01.00]current");
        // The unchanged current lyrics are not worth saving; an edit is.
        assert!(!d.can_save());
        d.editor.push_str("\n[00:02.00]more");
        assert!(d.edited() && d.can_save());
        d.reset_editor();
        assert!(!d.edited());

        d.searching(1);
        d.poll_search(&SearchState::Done(
            1,
            outcome(vec![candidate("[00:03.00]found", 0.95)]),
        ));
        d.select_result(0);
        assert_eq!(d.selection.as_ref().unwrap().entry, Entry::Result(0));
        assert_eq!(d.editor, "[00:03.00]found");
        assert!(d.can_save());
        // The current lyrics changing does not touch another selection.
        d.refresh_current(Some("[00:01.00]other"));
        assert_eq!(d.editor, "[00:03.00]found");
        // A new search keeps the selected result's text.
        d.searching(2);
        assert_eq!(d.selection.as_ref().unwrap().entry, Entry::Kept);
        assert!(d.can_save());
        d.editor = "[ar:x]\n".into();
        assert!(!d.can_save());
    }

    #[test]
    fn current_lyrics_follow_while_unedited() {
        let mut d = Draft::new(song("A"));
        d.refresh_current(Some("one"));
        d.refresh_current(Some("two"));
        assert_eq!(d.editor, "two");
        d.refresh_current(None);
        assert!(d.selection.is_none() && d.editor.is_empty());
        d.refresh_current(Some("three"));
        assert_eq!(d.editor, "three");
        d.editor = "mine".into();
        d.refresh_current(Some("four"));
        assert_eq!(d.editor, "mine");
    }

    #[test]
    fn imports_become_the_selection() {
        let mut d = Draft::new(song("A"));
        d.timing = true;
        d.import("a.lrc".into(), "[00:01.00]x".into());
        assert_eq!(d.selection.as_ref().unwrap().label, "File: a.lrc");
        assert!(d.can_save() && !d.timing);
        d.refresh_current(Some("current"));
        assert_eq!(d.editor, "[00:01.00]x");
    }

    #[test]
    fn song_changes_clear_the_panel() {
        let mut d = Draft::new(song("A"));
        d.query = "q".into();
        d.import("a.lrc".into(), "x".into());
        d.timing = true;
        assert!(!d.follow(&song("A")));
        assert_eq!(d.query, "q");
        assert!(d.follow(&song("B")));
        assert!(d.query.is_empty() && d.file.is_none() && d.selection.is_none());
        assert!(d.editor.is_empty() && d.timing);
        assert_eq!(d.song.title, "B");
    }

    #[test]
    fn labels() {
        use encore::lyrics::Provider;
        assert_eq!(
            lyrics_label(true, Origin::Cache(Provider::Lrclib)),
            "Synced (Cache (LRCLIB))"
        );
        assert_eq!(lyrics_label(false, Origin::Manual), "Plain (Manual)");
        assert_eq!(current_label(&LyricsState::NotFound, None), "No lyrics");
        assert_eq!(
            result_label(&candidate("[00:01.00]a", 1.0)),
            "LRCLIB · Synced · Rich Spirit"
        );
        assert_eq!(match_label(&candidate("a", 0.95)), "Sure match");
        assert_eq!(match_label(&candidate("a", 0.75)), "Good match");
        assert_eq!(match_label(&candidate("a", 0.2)), "Weak match");
    }

    #[test]
    fn offsets_and_lengths() {
        assert_eq!(fmt_offset(0), "0.0 s");
        assert_eq!(fmt_offset(300), "+0.3 s");
        assert_eq!(fmt_offset(-1250), "−1.25 s");
        assert_eq!(fmt_offset(-5), "−0.005 s");
        assert_eq!(nudged(59_900, 500), MAX_OFFSET_MS);
        assert_eq!(nudged(-100, -100), -200);

        let s = |n| Some(Duration::from_secs(n));
        assert_eq!(fmt_length(s(203), s(203)).as_deref(), Some("3:23"));
        assert_eq!(fmt_length(s(205), s(203)).as_deref(), Some("3:25 · +2 s"));
        assert_eq!(fmt_length(s(201), s(203)).as_deref(), Some("3:21 · −2 s"));
        assert_eq!(fmt_length(s(201), None).as_deref(), Some("3:21"));
        assert_eq!(fmt_length(None, s(1)), None);
    }

    #[test]
    fn editor_summaries() {
        assert_eq!(
            editor_summary("[00:01.00]a\n[00:02.00]\n[00:03.00]b").as_deref(),
            Some("Synced · 2 lines")
        );
        assert_eq!(editor_summary("one").as_deref(), Some("Plain · 1 line"));
        assert_eq!(editor_summary("[ar:x]\n  "), None);
    }

    #[test]
    fn sync_view_applies_or_leaves_the_editor() {
        let mut d = Draft::new(song("A"));
        d.import("a.txt".into(), "One\nTwo".into());
        assert!(d.can_sync());
        d.timing = true;
        d.start_sync();
        assert!(d.is_syncing() && !d.timing);
        let sync = d.sync.as_mut().unwrap();
        sync.session.stamp(Duration::from_millis(1500));
        // Cancel leaves the editor as it was.
        d.cancel_sync();
        assert!(!d.is_syncing());
        assert_eq!(d.editor, "One\nTwo");
        d.start_sync();
        let sync = d.sync.as_mut().unwrap();
        sync.session.stamp(Duration::from_millis(1500));
        sync.session.stamp(Duration::from_millis(3000));
        d.apply_sync();
        assert!(!d.is_syncing());
        assert_eq!(
            d.editor,
            "[length:03:23.00]\n[00:01.50]One\n[00:03.00]Two\n"
        );
        assert!(d.can_save());
        // Nothing to time in an empty editor; a new song closes the view.
        d.editor.clear();
        assert!(!d.can_sync());
        d.start_sync();
        assert!(!d.is_syncing());
        d.editor = "x".into();
        d.start_sync();
        assert!(d.follow(&song("B")));
        assert!(!d.is_syncing());
    }

    #[test]
    fn sync_keys_and_seeks() {
        assert_eq!(sync_key(Key::Space), Some(SyncKey::Stamp));
        assert_eq!(sync_key(Key::Enter), Some(SyncKey::Stamp));
        assert_eq!(sync_key(Key::Backspace), Some(SyncKey::Back));
        assert_eq!(sync_key(Key::P), Some(SyncKey::TogglePause));
        assert_eq!(sync_key(Key::G), Some(SyncKey::Gap));
        assert_eq!(sync_key(Key::Escape), Some(SyncKey::Cancel));
        assert_eq!(sync_key(Key::Y), None);
        let s = Duration::from_secs;
        let step = SEEK_STEP;
        assert_eq!(seek_target(s(10), step, false, None), s(7));
        assert_eq!(seek_target(s(1), step, false, None), s(0));
        assert_eq!(seek_target(s(10), step, true, Some(s(200))), s(13));
        assert_eq!(seek_target(s(10), TRANSPORT_STEP, true, None), s(15));
        assert_eq!(seek_target(s(10), TRANSPORT_STEP, false, None), s(5));
        assert_eq!(fmt_position(Duration::from_millis(62_345)), "01:02.35");
        assert_eq!(fmt_position(Duration::from_millis(62_344)), "01:02.34");
    }

    #[test]
    fn seeks_stay_within_the_song() {
        let s = Duration::from_secs;
        let ms = Duration::from_millis;
        // Forward stops short of the end, which would finish the song.
        assert_eq!(
            seek_target(s(197), TRANSPORT_STEP, true, Some(s(200))),
            s(199)
        );
        // Within the guard a step forward stays put rather than going back.
        assert_eq!(
            seek_target(ms(199_500), TRANSPORT_STEP, true, Some(s(200))),
            ms(199_500)
        );
        assert_eq!(seek_target(s(2), TRANSPORT_STEP, false, Some(s(200))), s(0));
        assert_eq!(clamp_seek(s(200), Some(s(200))), s(199));
        assert_eq!(clamp_seek(s(50), Some(s(200))), s(50));
        assert_eq!(clamp_seek(s(500), None), s(500));
        assert_eq!(clamp_seek(s(5), Some(ms(500))), s(0));
    }

    #[test]
    fn seeks_show_their_target_until_the_player_reports_it() {
        let ms = Duration::from_millis;
        // Paused: the target until the player is there.
        assert_eq!(
            pending_position(ms(10_000), false, ms(30_000), ms(50)),
            Some(ms(30_000))
        );
        assert_eq!(
            pending_position(ms(30_000), false, ms(30_000), ms(50)),
            None
        );
        // Playing: the target moves on with time.
        assert_eq!(
            pending_position(ms(10_000), true, ms(30_000), ms(200)),
            Some(ms(30_200))
        );
        assert_eq!(
            pending_position(ms(30_150), true, ms(30_000), ms(200)),
            None
        );
        // A seek the player never reports is let go.
        assert_eq!(
            pending_position(ms(10_000), false, ms(30_000), ms(1_500)),
            None
        );
        assert_eq!(fmt_clock(ms(62_345)), "1:02.35");
        assert_eq!(fmt_clock(ms(5_004)), "0:05.00");
        assert_eq!(fmt_clock(ms(600_000)), "10:00.00");
    }

    #[test]
    fn playback_keys_by_view() {
        use PlaybackKey::*;
        let none = Modifiers::NONE;
        let alt = Modifiers::ALT;
        for typing in [false, true] {
            // Alt works whether or not a text field has the keyboard.
            assert_eq!(playback_key(Key::ArrowLeft, alt, typing), Some(SeekBack));
            assert_eq!(
                playback_key(Key::ArrowRight, alt, typing),
                Some(SeekForward)
            );
            assert_eq!(playback_key(Key::P, alt, typing), Some(TogglePause));
            // Other combinations are left alone: Shift selects, Ctrl moves by word.
            for m in [Modifiers::SHIFT, Modifiers::CTRL, Modifiers::COMMAND] {
                assert_eq!(playback_key(Key::ArrowLeft, m, typing), None, "{m:?}");
                assert_eq!(playback_key(Key::P, m, typing), None, "{m:?}");
            }
            assert_eq!(playback_key(Key::Y, alt, typing), None);
            assert_eq!(playback_key(Key::Space, none, typing), None);
        }
        // Without Alt they act only while no text field has the keyboard.
        assert_eq!(playback_key(Key::ArrowLeft, none, false), Some(SeekBack));
        assert_eq!(
            playback_key(Key::ArrowRight, none, false),
            Some(SeekForward)
        );
        assert_eq!(playback_key(Key::P, none, false), Some(TogglePause));
        for key in [Key::ArrowLeft, Key::ArrowRight, Key::P] {
            assert_eq!(playback_key(key, none, true), None, "{key:?}");
        }
        // The sync view has its own keys and steps.
        assert_eq!(sync_key(Key::ArrowLeft), Some(SyncKey::SeekBack));
        assert_eq!(sync_key(Key::ArrowRight), Some(SyncKey::SeekForward));
        assert_eq!(sync_key(Key::P), Some(SyncKey::TogglePause));
        assert!(SEEK_STEP < TRANSPORT_STEP);
    }

    #[test]
    fn stretch_needs_a_length_tag() {
        let info = |synced, length: Option<u64>, can_stretch| LyricsInfo {
            origin: Origin::Manual,
            synced,
            text: Arc::from(""),
            adjustments: Adjustments::default(),
            length: length.map(Duration::from_secs),
            can_stretch,
        };
        let d = Some(Duration::from_secs(200));
        assert_eq!(stretch_blocker(Some(&info(true, Some(200), true)), d), None);
        assert!(stretch_blocker(None, d).is_some());
        assert_eq!(
            stretch_blocker(Some(&info(false, None, false)), d),
            Some("Only synced lyrics can be stretched")
        );
        assert_eq!(
            stretch_blocker(Some(&info(true, None, false)), d),
            Some("The lyrics have no [length:] tag")
        );
        assert_eq!(
            stretch_blocker(Some(&info(true, Some(200), false)), None),
            Some("The song's length is unknown")
        );
    }
}

#[cfg(test)]
mod snapshot {
    use encore::lyrics::search::{self, ProviderError};
    use encore::lyrics::{sources, LyricsConfig, Provider};

    use super::*;
    use crate::art;
    use crate::config::{LyricsAlign, LyricsFlow};
    use crate::export::render::Renderer;
    use crate::player::{Origin as PlayerOrigin, PlayState, PlayerState};
    use crate::ui::toasts::Toasts;
    use crate::ui::{classic, ArtView, View};

    const LRC: &str = "[ar:Kendrick Lamar]\n[al:Mr. Morale & The Big Steppers]\n[ti:Rich Spirit]\n\
        [length:03:23.20]\n[00:06.49]Takin' my baby to school, then I pray for her\n\
        [00:08.68]'Cause you bitches ain't never been cool, writin' testament\n\
        [00:11.27]Paintin' pictures, put me in the Louvre, that's a definite\n\
        [00:13.65]Universal shift, I'm in a groove and celebrity do not mean integrity\n\
        [00:17.60]I'm a good man, shakin' hand, firm grip rule\n\
        [00:21.10]Now I'm on a rich spirit\n[00:24.30]Rich spirit\n\
        [00:27.90]Can't nobody tell me nothin'\n[00:31.40]Rich spirit, rich spirit\n\
        [00:35.00]I'm on a rich spirit\n";

    fn candidate(
        provider: Provider,
        title: &str,
        artist: &str,
        secs: u64,
        synced: bool,
        score: f32,
    ) -> Candidate {
        let text = if synced {
            LRC.to_owned()
        } else {
            LRC.lines()
                .filter_map(|l| l.split_once(']').map(|(_, t)| t))
                .filter(|t| !t.contains(':') || t.contains(' '))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let duration = Duration::from_secs(secs);
        Candidate {
            provider,
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            duration: Some(duration),
            lyrics: Arc::new(parse_lrc(&text)),
            text,
            score,
            duration_diff: Some(duration.abs_diff(Duration::from_secs(203))),
        }
    }

    /// Renders the lyrics manager over the Classic layout to the PNG named by
    /// `ENCORE_LYRICS_PANEL_SNAPSHOT`, or `lyrics-panel.png` in the temp folder.
    /// `ENCORE_LYRICS_PANEL_STATE` picks what it shows: `results` (default), `searching`,
    /// `empty`, `offline`, `failed`, `file`, `timing`, `drop`, `live` for a real search, or the
    /// sync view: `sync` (plain lyrics, nothing stamped), `sync-half` (half the lines stamped)
    /// or `sync-warn` (a line earlier than the one before).
    /// `ENCORE_LYRICS_PANEL_SONG=<file>` loads the art of that MPD file, and
    /// `ENCORE_LYRICS_PANEL_TITLE` / `_ARTIST` / `_SECS` describe the song.
    #[test]
    #[ignore = "needs a GPU"]
    fn panel_snapshot() {
        let env = |name: &str| std::env::var(format!("ENCORE_LYRICS_PANEL_{name}")).ok();
        let out = env("SNAPSHOT")
            .map_or_else(|| std::env::temp_dir().join("lyrics-panel.png"), Into::into);
        let state = env("STATE").unwrap_or_else(|| "results".into());
        let song = SongInfo {
            id: 1,
            file: env("SONG").unwrap_or_else(|| "x.flac".into()),
            title: env("TITLE").unwrap_or_else(|| "Rich Spirit".into()),
            artist: env("ARTIST").unwrap_or_else(|| "Kendrick Lamar".into()),
            album: "Mr. Morale & The Big Steppers".into(),
            duration: Some(Duration::from_secs(
                env("SECS").and_then(|s| s.parse().ok()).unwrap_or(203),
            )),
            origin: PlayerOrigin::Mpd,
        };
        let key = song.lyrics_key();

        let dir = std::env::temp_dir().join(format!("encore-lyrics-panel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = dir.join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(
            cache.join(format!("{}.lrc", sources::cache_key(&key))),
            format!("[source:lrclib]\n{LRC}"),
        )
        .unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let tx = std::sync::Mutex::new(tx);
        let provider = LyricsProvider::new(
            LyricsConfig {
                music_dir: None,
                local_dir: dir.join("local"),
                manual_dir: dir.join("local/manual"),
                cache_dir: Some(cache),
                online: false,
            },
            Box::new(move || {
                let _ = tx.lock().unwrap().send(());
            }),
        );
        provider.request(&key);
        rx.recv_timeout(Duration::from_secs(5)).unwrap();

        let mut panel = LyricsPanel::new(song.clone(), state != "offline", Arc::new(|| {}));
        let d = &mut panel.draft;
        match state.as_str() {
            "searching" => {
                // A real search, still waiting on the network while the frames render.
                d.query = d.song_query();
                let token = panel.searcher.search(d.request());
                panel.draft.searching(token);
            }
            "empty" | "offline" => {}
            "failed" => {
                d.outcome = Some(Arc::new(SearchOutcome {
                    failures: vec![
                        ProviderError {
                            provider: Provider::Lrclib,
                            message: "timeout: global".into(),
                        },
                        ProviderError {
                            provider: Provider::Kugou,
                            message: "io: Connection refused".into(),
                        },
                    ],
                    ..Default::default()
                }));
            }
            "timing" => {
                provider
                    .set_adjustments(
                        &key,
                        Adjustments {
                            offset_ms: 300,
                            stretch: false,
                        },
                    )
                    .unwrap();
                d.timing = true;
            }
            "sync" | "sync-half" | "sync-warn" => {
                let plain: Vec<&str> = LRC
                    .lines()
                    .filter_map(|l| l.split_once(']').map(|(_, t)| t))
                    .filter(|t| !t.is_empty() && (!t.contains(':') || t.contains(' ')))
                    .collect();
                let mut text = plain[..5].join("\n");
                text.push_str("\n\n");
                text.push_str(&plain[5..].join("\n"));
                d.import("Rich Spirit.txt".into(), text);
                d.start_sync();
                if state != "sync" {
                    let session = &mut d.sync.as_mut().unwrap().session;
                    for ms in [6490, 8680, 11_270, 13_650, 17_600, 21_100] {
                        session.stamp(Duration::from_millis(ms));
                    }
                    if state == "sync-warn" {
                        session.doc.set(2, Some(Duration::from_millis(5000)));
                    }
                }
            }
            "live" => {
                let agent = sources::new_agent();
                d.outcome = Some(Arc::new(search::search(&agent, &d.request())));
            }
            _ => {
                d.outcome = Some(Arc::new(SearchOutcome {
                    candidates: vec![
                        candidate(
                            Provider::Lrclib,
                            "Rich Spirit",
                            "Kendrick Lamar",
                            203,
                            true,
                            1.0,
                        ),
                        candidate(
                            Provider::Kugou,
                            "Rich Spirit",
                            "Kendrick Lamar",
                            205,
                            true,
                            0.97,
                        ),
                        candidate(
                            Provider::Lrclib,
                            "Rich Spirit",
                            "Kendrick Lamar",
                            203,
                            false,
                            1.0,
                        ),
                        candidate(
                            Provider::Kugou,
                            "Rich Spirit (Sped Up)",
                            "Kendrick Lamar",
                            162,
                            true,
                            0.82,
                        ),
                        candidate(Provider::Kugou, "Spirit", "Beyoncé", 271, true, 0.4),
                    ],
                    failures: vec![],
                    instrumental: false,
                }));
                if state == "file" || state == "drop" {
                    d.import("Kendrick Lamar - Rich Spirit.lrc".into(), LRC.into());
                } else {
                    d.select_result(1);
                    d.editor.push_str("[00:38.50]Edited line\n");
                }
            }
        }

        let elapsed = Duration::from_secs(22);
        let player = PlayerState::snapshot(song.clone(), elapsed, PlayState::Playing);
        let (w, h) = (1920u32, 1080u32);
        let mut r = Renderer::new([w, h]).expect("renderer");
        let art = env("SONG").map(|_| {
            let images = art::load_art_images(&crate::config::Config::from_env(), &song);
            art::upload(r.ctx(), &song.file, images)
        });
        let art = ArtView {
            current: art.as_ref(),
            previous: None,
            fade: 1.0,
            artist_colours: false,
            switch: None,
        };
        let theme = PanelTheme::new(&art.scheme(), true);
        let mut toasts = Toasts::default();
        let mut pixels = Vec::new();
        for i in 0..20 {
            let px = r
                .render_frame(f64::from(i) / 30.0, 1.0 / 30.0, |ui| {
                    let view = View {
                        player: &player,
                        art,
                        next_cover: None,
                        artist_bg: false,
                        show_clock: false,
                        lyrics_enabled: true,
                        lyrics: panel
                            .preview()
                            .map_or_else(|| provider.get(&key), LyricsState::Found),
                        now: chrono::Local::now(),
                        capture: false,
                        motion: false,
                        tint: true,
                        skip_buttons: true,
                        lyrics_align: LyricsAlign::Right,
                        lyrics_flow: LyricsFlow::Stacked,
                    };
                    classic::draw(ui, &view);
                    let ctx = ui.ctx().clone();
                    let out = panel.show(
                        &ctx,
                        &theme,
                        &Env {
                            provider: &provider,
                            elapsed: Some(elapsed),
                            dock_right: false,
                            lyrics_visible: true,
                            playing: true,
                        },
                    );
                    for toast in out.toasts {
                        toasts.push(toast);
                    }
                    if state == "drop" {
                        let rect = ctx
                            .memory(|m| m.area_rect(Id::unique("lyrics-panel")))
                            .unwrap_or(Rect::NOTHING);
                        drop_hint(&ctx, &theme, rect);
                    }
                    toasts.show(&ctx, None);
                })
                .expect("frame");
            pixels = px.to_vec();
        }
        image::RgbaImage::from_raw(w, h, pixels)
            .expect("image")
            .save(&out)
            .expect("save");
        let _ = std::fs::remove_dir_all(dir);
    }
}
