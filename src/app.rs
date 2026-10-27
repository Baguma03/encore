use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use egui::{Event, Key, Modifiers, ViewportCommand};
use encore::lyrics::{Lyrics, LyricsConfig, LyricsProvider, LyricsState};

use crate::art::ArtLoader;
use crate::config::{Config, Layout, LyricsAlign, LyricsFlow, Source};
use crate::export::{self, Aspect, ExportJob, ExportRequest, ExportStatus, Quality, Style};
use crate::player::{Command, PlayState, Player, PlayerState, SongInfo};
use crate::ui::export_panel::{ExportPanel, LiveLook, Marks, PanelResult};
use crate::ui::lyrics_panel::{self, LyricsPanel};
use crate::ui::panel_theme::PanelTheme;
use crate::ui::toasts::{Progress, Toast, Toasts};
use crate::ui::{self, ArtView, View};

const PLAYING_FRAME: Duration = Duration::from_millis(33);
const IDLE_TICK: Duration = Duration::from_secs(1);
const LYRICS_DIR: &str = "./lyrics";

const MARK_TOAST: Duration = Duration::from_secs(2);
const SAVED_TOAST: Duration = Duration::from_secs(8);
const FAILED_TOAST: Duration = Duration::from_secs(10);
const CANCELLED_TOAST: Duration = Duration::from_secs(3);

/// What a key press does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Fullscreen,
    Cursor,
    Debug,
    TogglePause,
    Quit,
    Layout,
    Lyrics,
    Clock,
    Motion,
    ArtistBackground,
    Tint,
    SkipButtons,
    LyricsAlign,
    LyricsFlow,
    Previous,
    Next,
    OpenExport,
    CloseExport,
    MarkIn,
    MarkOut,
    Shortcuts,
    CloseShortcuts,
    OpenLyricsPanel,
    CloseLyricsPanel,
}

/// A key press: the key and whether Shift was held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Press {
    key: Key,
    shift: bool,
}

#[cfg(test)]
impl Press {
    fn key(key: Key) -> Self {
        Self { key, shift: false }
    }

    fn shift(key: Key) -> Self {
        Self { key, shift: true }
    }
}

/// What is open over the layout when a key is pressed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Open {
    export: bool,
    lyrics: bool,
    shortcuts: bool,
    /// The lyrics manager's sync view is open; it takes its own keys, Escape included.
    syncing: bool,
    /// A text field has the keyboard.
    typing: bool,
}

/// Maps a key press to its action. Shift matters only for `y`: Shift+Y opens the lyrics
/// manager, `y` alone toggles the lyrics. While a text field has the keyboard no key acts, so
/// typing never triggers a shortcut; Escape then only leaves the field. While the lyrics
/// manager or the export panel is open only closing it and fullscreen work, so keys meant for
/// the panel never reach playback; the lyrics manager takes ←, → and P (and Alt with them)
/// for its own seeking and play/pause, and in its sync view Escape is left to the view, which
/// goes back to the editor. While the shortcuts overlay is open Escape closes it instead
/// of quitting.
fn key_action(press: Press, open: Open) -> Option<Action> {
    let Press { key, shift } = press;
    if open.typing {
        return None;
    }
    if open.lyrics {
        return match key {
            Key::Escape if !open.syncing => Some(Action::CloseLyricsPanel),
            Key::Y if shift => Some(Action::CloseLyricsPanel),
            Key::F11 => Some(Action::Fullscreen),
            _ => None,
        };
    }
    if open.export {
        return match key {
            Key::E | Key::Escape => Some(Action::CloseExport),
            Key::F11 => Some(Action::Fullscreen),
            _ => None,
        };
    }
    Some(match key {
        Key::Escape if open.shortcuts => Action::CloseShortcuts,
        Key::F | Key::F11 => Action::Fullscreen,
        Key::C => Action::Cursor,
        Key::D => Action::Debug,
        Key::Space => Action::TogglePause,
        Key::Escape | Key::Q => Action::Quit,
        Key::L => Action::Layout,
        Key::Y if shift => Action::OpenLyricsPanel,
        Key::Y => Action::Lyrics,
        Key::T => Action::Clock,
        Key::B => Action::Motion,
        Key::A => Action::ArtistBackground,
        Key::K => Action::Tint,
        Key::S => Action::SkipButtons,
        Key::J => Action::LyricsAlign,
        Key::O => Action::LyricsFlow,
        Key::ArrowLeft => Action::Previous,
        Key::ArrowRight => Action::Next,
        Key::E => Action::OpenExport,
        Key::OpenBracket => Action::MarkIn,
        Key::CloseBracket => Action::MarkOut,
        Key::Questionmark | Key::H => Action::Shortcuts,
        _ => return None,
    })
}

/// The key a press stands for. Shift+/ reports as `?` on most systems; this catches the
/// platforms that report the unshifted key instead.
fn pressed_key(key: Key, modifiers: Modifiers) -> Key {
    if key == Key::Slash && modifiers.shift {
        Key::Questionmark
    } else {
        key
    }
}

pub struct App {
    config: Config,
    player: Player,
    art: ArtLoader,
    lyrics: LyricsProvider,
    layout: Layout,
    fullscreen: bool,
    cursor_visible: bool,
    show_debug: bool,
    lyrics_enabled: bool,
    show_clock: bool,
    motion: bool,
    /// Whether each layout uses the artist image as its background, kept for the session.
    artist_bg: HashMap<Layout, bool>,
    tint: bool,
    skip_buttons: bool,
    lyrics_align: LyricsAlign,
    lyrics_flow: LyricsFlow,
    export_panel: Option<ExportPanel>,
    /// Formats ticked in the export panel, kept for the session.
    export_formats: Vec<Aspect>,
    export_quality: Quality,
    /// In and out points for the song in `marks_song`.
    marks: Marks,
    marks_song: Option<String>,
    export_job: Option<ExportJob>,
    export_queue: VecDeque<ExportRequest>,
    toasts: Toasts,
    show_shortcuts: bool,
    lyrics_panel: Option<LyricsPanel>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, config: Config) -> Self {
        ui::install_fonts(&cc.egui_ctx);

        let ctx = cc.egui_ctx.clone();
        let player = Player::spawn(&config, Box::new(move || ctx.request_repaint()));
        let ctx = cc.egui_ctx.clone();
        let art = ArtLoader::new(&config, Box::new(move || ctx.request_repaint()));
        let ctx = cc.egui_ctx.clone();
        let lyrics = LyricsProvider::new(
            LyricsConfig {
                music_dir: config.music_dir.clone(),
                local_dir: LYRICS_DIR.into(),
                cache_dir: LyricsConfig::default_cache_dir(),
                online: config.lyrics_online,
            },
            Box::new(move || ctx.request_repaint()),
        );

        let toasts = Toasts::default();

        Self {
            layout: config.layout,
            lyrics_enabled: config.lyrics_enabled,
            motion: config.motion,
            artist_bg: [Layout::Spotify, Layout::Apple, Layout::Vibrant]
                .into_iter()
                .map(|l| (l, config.artist_bg(l)))
                .collect(),
            tint: config.tint,
            skip_buttons: config.skip_buttons,
            lyrics_align: config.lyrics_align,
            lyrics_flow: config.lyrics_flow,
            show_clock: config.clock,
            config,
            player,
            art,
            lyrics,
            fullscreen: true,
            cursor_visible: true,
            show_debug: false,
            export_panel: None,
            export_formats: vec![Aspect::Portrait],
            export_quality: Quality::default(),
            marks: Marks::default(),
            marks_song: None,
            export_job: None,
            export_queue: VecDeque::new(),
            toasts,
            show_shortcuts: false,
            lyrics_panel: None,
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context, state: &PlayerState) {
        let presses: Vec<Press> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } => Some(Press {
                        key: pressed_key(*key, *modifiers),
                        shift: modifiers.shift,
                    }),
                    _ => None,
                })
                .collect()
        });
        for press in presses {
            let lyrics = self.lyrics_panel.is_some();
            let open = Open {
                export: self.export_panel.is_some(),
                lyrics,
                shortcuts: self.show_shortcuts,
                syncing: self
                    .lyrics_panel
                    .as_ref()
                    .is_some_and(LyricsPanel::is_syncing),
                typing: lyrics && ctx.text_edit_focused(),
            };
            if let Some(action) = key_action(press, open) {
                self.run_action(ctx, action, state);
            }
        }
    }

    fn run_action(&mut self, ctx: &egui::Context, action: Action, state: &PlayerState) {
        match action {
            Action::Fullscreen => {
                self.fullscreen = !self.fullscreen;
                ctx.send_viewport_cmd(ViewportCommand::Fullscreen(self.fullscreen));
            }
            Action::Cursor => {
                self.cursor_visible = !self.cursor_visible;
                ctx.send_viewport_cmd(ViewportCommand::CursorVisible(self.cursor_visible));
            }
            Action::Debug => self.show_debug = !self.show_debug,
            Action::TogglePause => self.player.send(Command::TogglePause),
            Action::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
            Action::Layout => self.layout = self.layout.next(),
            Action::Lyrics => self.lyrics_enabled = !self.lyrics_enabled,
            Action::Clock => self.show_clock = !self.show_clock,
            Action::Motion => self.motion = !self.motion,
            Action::ArtistBackground => {
                let on = self.artist_bg_for(self.layout);
                self.artist_bg.insert(self.layout, !on);
            }
            Action::Tint => self.tint = !self.tint,
            Action::SkipButtons => self.skip_buttons = !self.skip_buttons,
            Action::LyricsAlign => self.lyrics_align = self.lyrics_align.next(),
            Action::LyricsFlow => self.lyrics_flow = self.lyrics_flow.next(),
            Action::Previous => self.player.send(Command::Previous),
            Action::Next => self.player.send(Command::Next),
            Action::OpenExport => {
                if let Some(song) = &state.current {
                    self.show_shortcuts = false;
                    self.open_export_panel(song.clone(), state.next.clone(), state.elapsed());
                }
            }
            Action::CloseExport => self.export_panel = None,
            Action::MarkIn | Action::MarkOut if state.current.is_some() => {
                let at = state.elapsed();
                let label = if action == Action::MarkIn {
                    self.marks.start = Some(at);
                    "In"
                } else {
                    self.marks.end = Some(at);
                    "Out"
                };
                let toast = Toast::new(format!("{label} {}", ui::fmt_time(at)), MARK_TOAST)
                    .detail("Press E to export");
                self.toasts.replace(label, toast);
            }
            Action::MarkIn | Action::MarkOut => {}
            Action::Shortcuts => self.show_shortcuts = !self.show_shortcuts,
            Action::CloseShortcuts => self.show_shortcuts = false,
            Action::OpenLyricsPanel => {
                if let Some(song) = &state.current {
                    self.show_shortcuts = false;
                    self.export_panel = None;
                    let ctx = ctx.clone();
                    self.lyrics_panel = Some(LyricsPanel::new(
                        song.clone(),
                        self.config.lyrics_online,
                        Arc::new(move || ctx.request_repaint()),
                    ));
                }
            }
            Action::CloseLyricsPanel => self.lyrics_panel = None,
        }
    }

    fn artist_bg_for(&self, layout: Layout) -> bool {
        self.artist_bg.get(&layout).copied().unwrap_or(false)
    }

    /// The song's lyrics, looked up even when the lyrics column is turned off.
    fn song_lyrics(&self, song: &SongInfo) -> LyricsState {
        let key = song.lyrics_key();
        self.lyrics.request(&key);
        self.lyrics.get(&key)
    }

    fn open_export_panel(&mut self, song: SongInfo, next: Option<SongInfo>, elapsed: Duration) {
        let lyrics = self.song_lyrics(&song);
        let audio_found = song
            .local_path(self.config.music_dir.as_deref())
            .is_some_and(|path| path.is_file());
        self.export_panel = Some(ExportPanel::new(
            song,
            next,
            lyrics,
            elapsed,
            self.marks,
            audio_found,
            LiveLook {
                style: Style::for_layout(self.layout),
                motion: self.motion,
                artist_bg: self.artist_bg.clone(),
                tint: self.tint,
                skip_buttons: self.skip_buttons,
                lyrics_align: self.lyrics_align,
                lyrics_flow: self.lyrics_flow,
            },
        ));
    }

    fn show_export_panel(&mut self, ctx: &egui::Context, theme: &PanelTheme, elapsed: Duration) {
        let Some(mut panel) = self.export_panel.take() else {
            return;
        };
        let lyrics = self.song_lyrics(panel.song());
        panel.refresh_lyrics(&lyrics, elapsed);
        match panel.show(
            ctx,
            theme,
            &mut self.export_formats,
            &mut self.export_quality,
        ) {
            PanelResult::Open => self.export_panel = Some(panel),
            PanelResult::Close => {}
            PanelResult::Export(req) => self.export_queue.push_back(*req),
        }
    }

    fn show_lyrics_panel(&mut self, ctx: &egui::Context, theme: &PanelTheme, state: &PlayerState) {
        let Some(mut panel) = self.lyrics_panel.take() else {
            return;
        };
        if let Some(song) = &state.current {
            panel.follow(song);
        }
        let key = panel.song().lyrics_key();
        self.lyrics.request(&key);
        let playing = state
            .current
            .as_ref()
            .is_some_and(|s| s.lyrics_key() == key);
        // The timing view docks away from the lyrics: right when the Spotify layout's lyrics
        // sit on the left, else left.
        let dock_right = self.layout == Layout::Spotify && self.lyrics_align == LyricsAlign::Left;
        let env = lyrics_panel::Env {
            provider: &self.lyrics,
            elapsed: playing.then(|| state.elapsed()),
            dock_right,
            lyrics_visible: self.lyrics_enabled,
            playing: state.state == PlayState::Playing,
        };
        let out = panel.show(ctx, theme, &env);
        for toast in out.toasts {
            self.toasts.push(toast);
        }
        for cmd in out.commands {
            self.player.send(cmd);
        }
        if !out.close {
            self.lyrics_panel = Some(panel);
        }
    }

    /// Clears the marks when the song changes.
    fn track_marks(&mut self, state: &PlayerState) {
        let file = state.current.as_ref().map(|s| &s.file);
        if file != self.marks_song.as_ref() {
            self.marks = Marks::default();
            self.marks_song = file.cloned();
        }
    }

    /// Reports a finished export and starts the next queued one.
    fn poll_exports(&mut self, ctx: &egui::Context) {
        if let Some(job) = self.export_job.take_if(|job| job.is_finished()) {
            self.toasts.push(self.finished_toast(job.status()));
            // Joining waits for the GPU device to be torn down; keep that off the UI thread.
            std::thread::spawn(move || drop(job));
        }
        if self.export_job.is_none() {
            if let Some(req) = self.export_queue.pop_front() {
                let ctx = ctx.clone();
                self.export_job = Some(export::spawn(
                    &self.config,
                    req,
                    Box::new(move || ctx.request_repaint()),
                ));
            }
        }
    }

    fn finished_toast(&self, status: ExportStatus) -> Toast {
        match status {
            ExportStatus::Done(paths, warning) => {
                let name = paths
                    .first()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let detail = match paths.len() {
                    0 | 1 => "Click to open the folder".to_owned(),
                    n => format!("and {} more · click to open the folder", n - 1),
                };
                Toast::new(format!("Saved: {name}"), SAVED_TOAST)
                    .detail(detail)
                    .warning(warning)
                    .opens(self.config.export_dir.clone())
            }
            ExportStatus::Failed(msg) => Toast::new(format!("Export failed: {msg}"), FAILED_TOAST),
            _ => Toast::new("Export cancelled", CANCELLED_TOAST),
        }
    }

    fn export_progress(&self) -> Option<Progress> {
        let job = self.export_job.as_ref()?;
        let fraction = job.progress();
        let title = match job.status() {
            ExportStatus::Rendering { aspect, .. } => {
                let formats = job.formats();
                let step = match formats.iter().position(|&a| a == aspect) {
                    Some(i) if formats.len() > 1 => format!(" ({}/{})", i + 1, formats.len()),
                    _ => String::new(),
                };
                format!(
                    "Exporting {}{step} · {:.0}%",
                    aspect.display_name(),
                    fraction * 100.0
                )
            }
            _ => "Preparing export…".to_owned(),
        };
        Some(Progress {
            title,
            fraction,
            queued: self.export_queue.len(),
        })
    }

    /// Queues lookups for the current and next song and returns the current song's lyrics,
    /// or the lyrics manager's preview of them while its sync view has stamped lines.
    fn update_lyrics(&self, state: &PlayerState) -> LyricsState {
        let Some(song) = state.current.as_ref().filter(|_| self.lyrics_enabled) else {
            return LyricsState::NotFound;
        };
        let key = song.lyrics_key();
        self.lyrics.request(&key);
        if let Some(next) = &state.next {
            self.lyrics.prefetch(&next.lyrics_key());
        }
        let preview = self
            .lyrics_panel
            .as_ref()
            .filter(|p| p.song().lyrics_key() == key)
            .and_then(LyricsPanel::preview);
        match preview {
            Some(lyrics) => LyricsState::Found(lyrics),
            None => self.lyrics.get(&key),
        }
    }

    fn art_view(&self) -> ArtView<'_> {
        ArtView {
            current: self.art.current(),
            previous: self.art.previous(),
            fade: self.art.fade(),
        }
    }

    /// Colours of the panels, following the album colours and their cross-fade.
    fn panel_theme(&self) -> PanelTheme {
        PanelTheme::new(&self.art_view().scheme(), self.tint)
    }

    fn view<'a>(&'a self, state: &'a PlayerState, lyrics: LyricsState) -> View<'a> {
        View {
            player: state,
            art: self.art_view(),
            next_cover: self.art.next_cover(),
            show_clock: self.show_clock,
            lyrics_enabled: self.lyrics_enabled,
            lyrics,
            now: chrono::Local::now(),
            capture: false,
            motion: self.motion,
            artist_bg: self.artist_bg_for(self.layout),
            tint: self.tint,
            skip_buttons: self.skip_buttons,
            lyrics_align: self.lyrics_align,
            lyrics_flow: self.lyrics_flow,
        }
    }

    fn draw_layout(&self, ui: &mut egui::Ui, state: &PlayerState, lyrics: LyricsState) {
        let view = self.view(state, lyrics);
        let cmd = match self.layout {
            Layout::Spotify => ui::spotify::draw(ui, &view),
            Layout::Apple => ui::apple::draw(ui, &view),
            Layout::Vibrant => ui::vibrant::draw(ui, &view),
        };
        // The panels' backdrops and the shortcuts overlay block clicks; this guards against any
        // that slip through.
        let covered =
            self.export_panel.is_some() || self.lyrics_panel.is_some() || self.show_shortcuts;
        if let Some(cmd) = cmd.filter(|_| !covered) {
            self.player.send(cmd);
        }
    }

    fn debug_window(&mut self, ctx: &egui::Context, state: &PlayerState, lyrics: &LyricsState) {
        let size = ctx.content_rect().size();
        let fps = ctx.input(|i| {
            if i.stable_dt > 0.0 {
                1.0 / i.stable_dt
            } else {
                0.0
            }
        });
        let mut open = self.show_debug;
        egui::Window::new("Debug").open(&mut open).show(ctx, |ui| {
            ui.heading("Window");
            ui.label(format!("Size: {:.0} x {:.0}", size.x, size.y));
            ui.label(format!("Fullscreen: {}", self.fullscreen));
            ui.label(format!("Frame rate: {fps:.0} fps"));
            ui.label(format!("Layout: {:?}", self.layout));
            let found = match lyrics {
                _ if !self.lyrics_enabled => "off".to_owned(),
                LyricsState::Loading => "searching".to_owned(),
                LyricsState::NotFound => "not found".to_owned(),
                LyricsState::Found(l) => match l.as_ref() {
                    Lyrics::Synced(lines) => format!("{} synced lines", lines.len()),
                    Lyrics::Plain(lines) => format!("{} plain lines", lines.len()),
                },
            };
            ui.label(format!("Lyrics: {found}"));

            ui.heading("Player");
            ui.label(format!(
                "Following: {}",
                state.source.as_deref().unwrap_or("nothing")
            ));
            ui.label(format!(
                "Source: {:?}{}",
                self.config.source,
                match (&self.config.mpris_player, self.config.source) {
                    (Some(pin), Source::Auto | Source::Mpris) => format!(", MPRIS player {pin:?}"),
                    _ => String::new(),
                }
            ));
            if self.config.source != Source::Mpris {
                ui.label(format!(
                    "MPD: {}:{}",
                    self.config.mpd_host, self.config.mpd_port
                ));
            }
            if let Some(dir) = &self.config.music_dir {
                ui.label(format!("Music dir: {}", dir.display()));
            }
            ui.label(format!(
                "Online lyrics: {}",
                if self.config.lyrics_online {
                    "on"
                } else {
                    "off"
                }
            ));
            ui.label(format!(
                "State: {:?}, queue: {}",
                state.state, state.queue_len
            ));
            if let Some(song) = &state.current {
                ui.label(format!("Album: {}", song.album));
                ui.label(format!("File: {}", song.file));
            }
            if let Some(art) = self.art.current() {
                ui.label(format!("Art: {}", art.song_file));
            }
            let (mut random, mut repeat) = (state.random, state.repeat);
            if ui.checkbox(&mut random, "Random").changed() {
                self.player.send(Command::ToggleRandom);
            }
            if ui.checkbox(&mut repeat, "Repeat").changed() {
                self.player.send(Command::ToggleRepeat);
            }
        });
        self.show_debug &= open;
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        if let Some(fullscreen) = ctx.input(|i| i.viewport().fullscreen) {
            self.fullscreen = fullscreen;
        }
        let state = self.player.state();
        self.track_marks(&state);
        self.handle_keys(&ctx, &state);
        if let Some(song) = &state.current {
            self.art.request(song);
        }
        if let Some(next) = &state.next {
            self.art.request_next(next);
        }
        self.art.poll(&ctx);

        let lyrics = self.update_lyrics(&state);

        egui::CentralPanel::no_frame().show(ui, |ui| self.draw_layout(ui, &state, lyrics.clone()));
        // Clicking the overlay closes it.
        let theme = self.panel_theme();
        if ui::shortcuts::draw(&ctx, self.show_shortcuts, &theme) {
            self.show_shortcuts = false;
        }

        if self.show_debug {
            self.debug_window(&ctx, &state, &lyrics);
        }

        self.show_export_panel(&ctx, &theme, state.elapsed());
        self.show_lyrics_panel(&ctx, &theme, &state);
        self.poll_exports(&ctx);
        let progress = self.export_progress();
        if self.toasts.show(&ctx, progress.as_ref()) {
            if let Some(job) = &self.export_job {
                job.cancel();
            }
        }

        let fading = self.art.fade() < 1.0;
        let overlays = self.export_panel.is_some()
            || self.lyrics_panel.is_some()
            || self.show_shortcuts
            || progress.is_some()
            || !self.toasts.is_empty();
        // The moving backdrop keeps flowing while paused.
        let moving = self.motion && self.layout != Layout::Spotify;
        if state.state == PlayState::Playing || fading || overlays || moving {
            ctx.request_repaint_after(PLAYING_FRAME);
        } else {
            ctx.request_repaint_after(IDLE_TICK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::shortcuts::Input;

    const NONE: Open = Open {
        export: false,
        lyrics: false,
        shortcuts: false,
        syncing: false,
        typing: false,
    };
    const EXPORT: Open = Open {
        export: true,
        ..NONE
    };
    const SHORTCUTS: Open = Open {
        shortcuts: true,
        ..NONE
    };
    const LYRICS: Open = Open {
        lyrics: true,
        ..NONE
    };

    fn act(key: Key, open: Open) -> Option<Action> {
        key_action(Press::key(key), open)
    }

    #[test]
    fn panel_blocks_app_keys() {
        for key in [
            Key::Space,
            Key::ArrowLeft,
            Key::ArrowRight,
            Key::Q,
            Key::L,
            Key::Y,
            Key::T,
            Key::B,
            Key::A,
            Key::K,
            Key::S,
            Key::J,
            Key::O,
            Key::D,
            Key::C,
            Key::F,
            Key::OpenBracket,
            Key::CloseBracket,
        ] {
            assert_eq!(act(key, EXPORT), None, "{key:?}");
            assert_eq!(act(key, LYRICS), None, "{key:?}");
            assert!(act(key, NONE).is_some(), "{key:?}");
        }
    }

    #[test]
    fn escape_and_e_depend_on_panel() {
        assert_eq!(act(Key::Escape, NONE), Some(Action::Quit));
        assert_eq!(act(Key::Escape, EXPORT), Some(Action::CloseExport));
        assert_eq!(act(Key::E, NONE), Some(Action::OpenExport));
        assert_eq!(act(Key::E, EXPORT), Some(Action::CloseExport));
        assert_eq!(act(Key::F11, EXPORT), Some(Action::Fullscreen));
        assert_eq!(act(Key::A, NONE), Some(Action::ArtistBackground));
        assert_eq!(act(Key::K, NONE), Some(Action::Tint));
        assert_eq!(act(Key::S, NONE), Some(Action::SkipButtons));
        assert_eq!(act(Key::K, EXPORT), None);
        assert_eq!(act(Key::S, EXPORT), None);
        assert_eq!(act(Key::J, NONE), Some(Action::LyricsAlign));
        assert_eq!(act(Key::J, EXPORT), None);
        assert_eq!(act(Key::O, NONE), Some(Action::LyricsFlow));
        assert_eq!(act(Key::O, EXPORT), None);
        assert_eq!(act(Key::Z, NONE), None);
    }

    #[test]
    fn shift_y_opens_the_lyrics_manager_and_y_toggles_lyrics() {
        let shift_y = Press::shift(Key::Y);
        assert_eq!(act(Key::Y, NONE), Some(Action::Lyrics));
        assert_eq!(key_action(shift_y, NONE), Some(Action::OpenLyricsPanel));
        assert_eq!(
            key_action(shift_y, SHORTCUTS),
            Some(Action::OpenLyricsPanel)
        );
        assert_eq!(key_action(shift_y, EXPORT), None);
        assert_eq!(key_action(shift_y, LYRICS), Some(Action::CloseLyricsPanel));
        assert_eq!(act(Key::Y, LYRICS), None);
        // Shift only matters for y.
        assert_eq!(key_action(Press::shift(Key::L), NONE), Some(Action::Layout));
    }

    #[test]
    fn lyrics_manager_keys() {
        assert_eq!(act(Key::Escape, LYRICS), Some(Action::CloseLyricsPanel));
        assert_eq!(act(Key::F11, LYRICS), Some(Action::Fullscreen));
        assert_eq!(act(Key::E, LYRICS), None);
        assert_eq!(act(Key::Questionmark, LYRICS), None);
        // Typing in a text field triggers nothing, not even Escape or Shift+Y.
        let typing = Open {
            typing: true,
            ..LYRICS
        };
        for key in [Key::Escape, Key::Space, Key::Q, Key::E, Key::F11, Key::Y] {
            assert_eq!(act(key, typing), None, "{key:?}");
        }
        assert_eq!(key_action(Press::shift(Key::Y), typing), None);
    }

    #[test]
    fn lyrics_manager_takes_its_playback_keys() {
        use egui::Modifiers;
        use lyrics_panel::{playback_key, PlaybackKey};
        let typing = Open {
            typing: true,
            ..LYRICS
        };
        // Neither the app's previous / next track nor its play / pause act while the panel is
        // open; the panel seeks and plays or pauses instead.
        for key in [Key::ArrowLeft, Key::ArrowRight, Key::P] {
            assert_eq!(act(key, LYRICS), None, "{key:?}");
            assert_eq!(act(key, typing), None, "{key:?}");
            assert!(
                playback_key(key, Modifiers::NONE, false).is_some(),
                "{key:?}"
            );
            assert!(playback_key(key, Modifiers::ALT, true).is_some(), "{key:?}");
            assert_eq!(playback_key(key, Modifiers::NONE, true), None, "{key:?}");
        }
        assert_eq!(
            playback_key(Key::P, Modifiers::NONE, false),
            Some(PlaybackKey::TogglePause)
        );
        assert_eq!(act(Key::Escape, LYRICS), Some(Action::CloseLyricsPanel));
        assert_eq!(act(Key::F11, LYRICS), Some(Action::Fullscreen));
        assert_eq!(
            key_action(Press::shift(Key::Y), LYRICS),
            Some(Action::CloseLyricsPanel)
        );
    }

    #[test]
    fn sync_view_keeps_its_keys() {
        let syncing = Open {
            syncing: true,
            ..LYRICS
        };
        // The view's own keys, Escape included, reach no app action.
        for key in [
            Key::Space,
            Key::Enter,
            Key::Backspace,
            Key::ArrowLeft,
            Key::ArrowRight,
            Key::P,
            Key::G,
            Key::Escape,
        ] {
            assert_eq!(act(key, syncing), None, "{key:?}");
            assert!(lyrics_panel::sync_key(key).is_some(), "{key:?}");
        }
        assert_eq!(act(Key::F11, syncing), Some(Action::Fullscreen));
        assert_eq!(
            key_action(Press::shift(Key::Y), syncing),
            Some(Action::CloseLyricsPanel)
        );
        assert_eq!(lyrics_panel::sync_key(Key::Q), None);
    }

    #[test]
    fn shortcuts_overlay_keys() {
        for key in [Key::Questionmark, Key::H] {
            assert_eq!(act(key, NONE), Some(Action::Shortcuts));
            assert_eq!(act(key, SHORTCUTS), Some(Action::Shortcuts));
            assert_eq!(act(key, EXPORT), None);
        }
        assert_eq!(act(Key::Escape, SHORTCUTS), Some(Action::CloseShortcuts));
        assert_eq!(act(Key::Q, SHORTCUTS), Some(Action::Quit));
        let both = Open {
            export: true,
            ..SHORTCUTS
        };
        assert_eq!(act(Key::Escape, both), Some(Action::CloseExport));
        assert_eq!(act(Key::L, SHORTCUTS), Some(Action::Layout));
    }

    #[test]
    fn shift_slash_is_a_question_mark() {
        let shift = Modifiers::SHIFT;
        assert_eq!(pressed_key(Key::Slash, shift), Key::Questionmark);
        assert_eq!(pressed_key(Key::Slash, Modifiers::NONE), Key::Slash);
        assert_eq!(pressed_key(Key::Questionmark, shift), Key::Questionmark);
        assert_eq!(pressed_key(Key::H, shift), Key::H);
    }

    #[test]
    fn shortcuts_overlay_lists_every_key() {
        let inputs: Vec<Input> = ui::shortcuts::SHORTCUTS
            .iter()
            .flat_map(|sc| sc.inputs)
            .copied()
            .collect();
        let states = [NONE, SHORTCUTS, EXPORT, LYRICS];
        for &key in Key::ALL {
            let mapped = states.iter().any(|&open| act(key, open).is_some());
            assert_eq!(mapped, inputs.contains(&Input::Key(key)), "{key:?}");
            // A Shift combination is listed when it does something other than the key alone.
            let shifted = states
                .iter()
                .any(|&open| key_action(Press::shift(key), open) != act(key, open));
            assert_eq!(
                shifted,
                inputs.contains(&Input::Shift(key)),
                "Shift+{key:?}"
            );
        }
    }
}
