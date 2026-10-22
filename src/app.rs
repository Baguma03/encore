use std::collections::VecDeque;
use std::time::Duration;

use egui::{Event, Key, ViewportCommand};
use mpd_display_rs::lyrics::{Lyrics, LyricsConfig, LyricsProvider, LyricsState};

use crate::art::ArtLoader;
use crate::config::{Config, Layout};
use crate::export::{self, Aspect, ExportJob, ExportRequest, ExportStatus, Style};
use crate::player::{Command, PlayState, Player, PlayerState, SongInfo};
use crate::ui::export_panel::{ExportPanel, Marks, PanelResult};
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
    Previous,
    Next,
    OpenExport,
    CloseExport,
    MarkIn,
    MarkOut,
}

/// Maps a key to its action. While the export panel is open only closing it and fullscreen
/// work, so keys meant for the panel never reach playback.
fn key_action(key: Key, panel_open: bool) -> Option<Action> {
    if panel_open {
        return match key {
            Key::E | Key::Escape => Some(Action::CloseExport),
            Key::F11 => Some(Action::Fullscreen),
            _ => None,
        };
    }
    Some(match key {
        Key::F | Key::F11 => Action::Fullscreen,
        Key::C => Action::Cursor,
        Key::D => Action::Debug,
        Key::Space => Action::TogglePause,
        Key::Escape | Key::Q => Action::Quit,
        Key::L => Action::Layout,
        Key::Y => Action::Lyrics,
        Key::T => Action::Clock,
        Key::B => Action::Motion,
        Key::ArrowLeft => Action::Previous,
        Key::ArrowRight => Action::Next,
        Key::E => Action::OpenExport,
        Key::OpenBracket => Action::MarkIn,
        Key::CloseBracket => Action::MarkOut,
        _ => return None,
    })
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
    export_panel: Option<ExportPanel>,
    /// Formats ticked in the export panel, kept for the session.
    export_formats: Vec<Aspect>,
    /// In and out points for the song in `marks_song`.
    marks: Marks,
    marks_song: Option<String>,
    export_job: Option<ExportJob>,
    export_queue: VecDeque<ExportRequest>,
    toasts: Toasts,
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

        Self {
            layout: config.layout,
            lyrics_enabled: config.lyrics_enabled,
            motion: config.motion,
            config,
            player,
            art,
            lyrics,
            fullscreen: true,
            cursor_visible: true,
            show_debug: false,
            show_clock: true,
            export_panel: None,
            export_formats: vec![Aspect::Portrait],
            marks: Marks::default(),
            marks_song: None,
            export_job: None,
            export_queue: VecDeque::new(),
            toasts: Toasts::default(),
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context, state: &PlayerState) {
        let keys: Vec<Key> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        ..
                    } => Some(*key),
                    _ => None,
                })
                .collect()
        });
        for key in keys {
            if let Some(action) = key_action(key, self.export_panel.is_some()) {
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
            Action::Previous => self.player.send(Command::Previous),
            Action::Next => self.player.send(Command::Next),
            Action::OpenExport => {
                if let Some(song) = &state.current {
                    self.open_export_panel(song.clone(), state.elapsed());
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
        }
    }

    /// The song's lyrics, looked up even when the lyrics column is turned off.
    fn song_lyrics(&self, song: &SongInfo) -> LyricsState {
        let key = song.lyrics_key();
        self.lyrics.request(&key);
        self.lyrics.get(&key)
    }

    fn open_export_panel(&mut self, song: SongInfo, elapsed: Duration) {
        let lyrics = self.song_lyrics(&song);
        let audio_found = self
            .config
            .music_dir
            .as_ref()
            .is_some_and(|dir| dir.join(&song.file).is_file());
        self.export_panel = Some(ExportPanel::new(
            song,
            lyrics,
            elapsed,
            self.marks,
            audio_found,
            Style::for_layout(self.layout),
            self.motion,
        ));
    }

    fn show_export_panel(&mut self, ctx: &egui::Context, elapsed: Duration) {
        let Some(mut panel) = self.export_panel.take() else {
            return;
        };
        let lyrics = self.song_lyrics(panel.song());
        panel.refresh_lyrics(&lyrics, elapsed);
        match panel.show(ctx, &mut self.export_formats) {
            PanelResult::Open => self.export_panel = Some(panel),
            PanelResult::Close => {}
            PanelResult::Export(req) => self.export_queue.push_back(req),
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

    /// Queues lookups for the current and next song and returns the current song's lyrics.
    fn update_lyrics(&self, state: &PlayerState) -> LyricsState {
        let Some(song) = state.current.as_ref().filter(|_| self.lyrics_enabled) else {
            return LyricsState::NotFound;
        };
        let key = song.lyrics_key();
        self.lyrics.request(&key);
        if let Some(next) = &state.next {
            self.lyrics.prefetch(&next.lyrics_key());
        }
        self.lyrics.get(&key)
    }

    fn view<'a>(&'a self, state: &'a PlayerState, lyrics: LyricsState) -> View<'a> {
        View {
            player: state,
            art: ArtView {
                current: self.art.current(),
                previous: self.art.previous(),
                fade: self.art.fade(),
            },
            watermark: self.art.watermark(),
            show_clock: self.show_clock,
            lyrics_enabled: self.lyrics_enabled,
            lyrics,
            now: chrono::Local::now(),
            capture: false,
            motion: self.motion,
        }
    }

    fn draw_layout(&self, ui: &mut egui::Ui, state: &PlayerState, lyrics: LyricsState) {
        let view = self.view(state, lyrics);
        let cmd = match self.layout {
            Layout::Spotify => ui::spotify::draw(ui, &view),
            Layout::Apple => ui::apple::draw(ui, &view),
            Layout::Vibrant => ui::vibrant::draw(ui, &view),
        };
        // The export panel's backdrop blocks clicks; this guards against any that slip through.
        if let Some(cmd) = cmd.filter(|_| self.export_panel.is_none()) {
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

            ui.heading("MPD");
            ui.label(format!(
                "{}:{} ({})",
                self.config.mpd_host,
                self.config.mpd_port,
                if state.connected {
                    "connected"
                } else {
                    "disconnected"
                },
            ));
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
        self.art.poll(&ctx);

        let lyrics = self.update_lyrics(&state);

        egui::CentralPanel::no_frame().show(ui, |ui| self.draw_layout(ui, &state, lyrics.clone()));

        if self.show_debug {
            self.debug_window(&ctx, &state, &lyrics);
        }

        self.show_export_panel(&ctx, state.elapsed());
        self.poll_exports(&ctx);
        let progress = self.export_progress();
        if self.toasts.show(&ctx, progress.as_ref()) {
            if let Some(job) = &self.export_job {
                job.cancel();
            }
        }

        let fading = self.art.fade() < 1.0;
        let overlays = self.export_panel.is_some() || progress.is_some() || !self.toasts.is_empty();
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
            Key::D,
            Key::C,
            Key::F,
            Key::OpenBracket,
            Key::CloseBracket,
        ] {
            assert_eq!(key_action(key, true), None, "{key:?}");
            assert!(key_action(key, false).is_some(), "{key:?}");
        }
    }

    #[test]
    fn escape_and_e_depend_on_panel() {
        assert_eq!(key_action(Key::Escape, false), Some(Action::Quit));
        assert_eq!(key_action(Key::Escape, true), Some(Action::CloseExport));
        assert_eq!(key_action(Key::E, false), Some(Action::OpenExport));
        assert_eq!(key_action(Key::E, true), Some(Action::CloseExport));
        assert_eq!(key_action(Key::F11, true), Some(Action::Fullscreen));
        assert_eq!(key_action(Key::A, false), None);
    }
}
