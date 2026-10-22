//! Offline export of a song range as MP4 clips, rendered on a background thread.

pub mod ffmpeg;
pub mod render;
pub mod scene;

use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use log::{info, warn};
use mpd_display_rs::lyrics::LyricsState;

use crate::config::{Config, Layout};
use crate::player::SongInfo;
use ffmpeg::Encoder;
use render::Renderer;
pub use scene::Scene;

/// Frames rendered before the clip starts so animations settle; they are not written.
const PRE_ROLL: Duration = Duration::from_millis(1500);
/// Minimum gap between progress callbacks.
const NOTIFY_INTERVAL: Duration = Duration::from_millis(100);
/// Byte budget for the artist and title part of a file name.
const MAX_NAME_BYTES: usize = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Aspect {
    Landscape,
    Portrait,
    Square,
}

impl Aspect {
    pub const ALL: [Aspect; 3] = [Aspect::Landscape, Aspect::Portrait, Aspect::Square];

    /// Output size in pixels (width, height).
    pub fn size(self) -> (u32, u32) {
        match self {
            Aspect::Landscape => (1920, 1080),
            Aspect::Portrait => (1080, 1920),
            Aspect::Square => (1080, 1080),
        }
    }

    /// File name tag, e.g. `9x16`.
    pub fn label(self) -> &'static str {
        match self {
            Aspect::Landscape => "16x9",
            Aspect::Portrait => "9x16",
            Aspect::Square => "1x1",
        }
    }

    /// Human-readable ratio, e.g. `9:16`.
    pub fn display_name(self) -> &'static str {
        match self {
            Aspect::Landscape => "16:9",
            Aspect::Portrait => "9:16",
            Aspect::Square => "1:1",
        }
    }
}

/// Look of an exported clip: the dark Apple layout, the colourful Vibrant one, or the
/// Spotify layout over the artist background.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Style {
    #[default]
    Dark,
    Vibrant,
    Spotify,
}

impl Style {
    pub const ALL: [Style; 3] = [Style::Dark, Style::Vibrant, Style::Spotify];

    pub fn display_name(self) -> &'static str {
        match self {
            Style::Dark => "Dark",
            Style::Vibrant => "Vibrant",
            Style::Spotify => "Spotify",
        }
    }

    /// The style matching the live layout.
    pub fn for_layout(layout: Layout) -> Self {
        match layout {
            Layout::Apple => Style::Dark,
            Layout::Vibrant => Style::Vibrant,
            Layout::Spotify => Style::Spotify,
        }
    }

    /// The live layout this style renders.
    pub fn layout(self) -> Layout {
        match self {
            Style::Dark => Layout::Apple,
            Style::Vibrant => Layout::Vibrant,
            Style::Spotify => Layout::Spotify,
        }
    }

    /// Whether this style can render `aspect`; the Spotify layout is landscape only.
    pub fn supports(self, aspect: Aspect) -> bool {
        self != Style::Spotify || aspect == Aspect::Landscape
    }

    /// The formats to render for the `chosen` ones, in [`Aspect::ALL`] order. The Spotify
    /// style always renders 16:9 alone.
    pub fn formats(self, chosen: &[Aspect]) -> Vec<Aspect> {
        if self == Style::Spotify {
            return vec![Aspect::Landscape];
        }
        Aspect::ALL
            .into_iter()
            .filter(|a| chosen.contains(a))
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct ExportRequest {
    pub song: SongInfo,
    /// Song queued after `song`, for the Spotify style's Up next card.
    pub next: Option<SongInfo>,
    pub lyrics: LyricsState,
    /// Clip range as song positions.
    pub start: Duration,
    pub end: Duration,
    /// Each format is rendered to its own file, in this order.
    pub formats: Vec<Aspect>,
    pub with_audio: bool,
    pub fps: u32,
    pub style: Style,
    /// Animated backdrop.
    pub motion: bool,
    /// Artist image as the background, when the artist has one.
    pub artist_bg: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExportStatus {
    Queued,
    Preparing,
    Rendering {
        aspect: Aspect,
        frame: u32,
        total: u32,
    },
    /// Written files, plus a warning such as missing audio.
    Done(Vec<PathBuf>, Option<String>),
    Failed(String),
    Cancelled,
}

/// Everything a scene needs to draw one frame.
pub struct FrameInputs<'a> {
    /// Song position shown in this frame.
    pub elapsed: Duration,
    pub song: &'a SongInfo,
    pub lyrics: &'a LyricsState,
}

struct Shared {
    status: Mutex<ExportStatus>,
    cancel: AtomicBool,
}

impl Shared {
    fn status(&self) -> MutexGuard<'_, ExportStatus> {
        self.status.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A running or finished export. Dropping it cancels the export and waits for the thread.
pub struct ExportJob {
    shared: Arc<Shared>,
    formats: Vec<Aspect>,
    handle: Option<JoinHandle<()>>,
}

impl ExportJob {
    pub fn status(&self) -> ExportStatus {
        self.shared.status().clone()
    }

    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        matches!(
            *self.shared.status(),
            ExportStatus::Done(..) | ExportStatus::Failed(_) | ExportStatus::Cancelled
        )
    }

    pub fn formats(&self) -> &[Aspect] {
        &self.formats
    }

    /// Overall progress from 0 to 1 across all formats.
    pub fn progress(&self) -> f32 {
        match *self.shared.status() {
            ExportStatus::Queued | ExportStatus::Preparing => 0.0,
            ExportStatus::Rendering {
                aspect,
                frame,
                total,
            } => {
                let index = self.formats.iter().position(|&a| a == aspect).unwrap_or(0);
                let within = if total == 0 {
                    1.0
                } else {
                    frame as f32 / total as f32
                };
                ((index as f32 + within) / self.formats.len().max(1) as f32).min(1.0)
            }
            ExportStatus::Done(..) | ExportStatus::Failed(_) | ExportStatus::Cancelled => 1.0,
        }
    }
}

impl Drop for ExportJob {
    fn drop(&mut self) {
        self.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Starts exporting `req` with the default scene; `on_progress` is called on status changes
/// and at most about ten times a second while rendering.
pub fn spawn(
    cfg: &Config,
    req: ExportRequest,
    on_progress: Box<dyn Fn() + Send + Sync>,
) -> ExportJob {
    let scene = scene::default_scene(cfg, &req);
    spawn_with_scene(cfg, req, scene, on_progress)
}

/// Like [`spawn`], drawing frames with `scene`.
pub fn spawn_with_scene(
    cfg: &Config,
    mut req: ExportRequest,
    scene: Box<dyn Scene>,
    on_progress: Box<dyn Fn() + Send + Sync>,
) -> ExportJob {
    let mut formats = Vec::new();
    for aspect in req.formats.drain(..) {
        if !formats.contains(&aspect) {
            formats.push(aspect);
        }
    }
    req.formats = formats.clone();

    let shared = Arc::new(Shared {
        status: Mutex::new(ExportStatus::Queued),
        cancel: AtomicBool::new(false),
    });
    let worker = Worker {
        shared: Arc::clone(&shared),
        music_dir: cfg.music_dir.clone(),
        export_dir: cfg.export_dir.clone(),
        on_progress,
        last_notify: None,
    };
    let handle = thread::Builder::new()
        .name("export".into())
        .spawn(move || worker.run(req, scene));
    let handle = match handle {
        Ok(h) => Some(h),
        Err(e) => {
            *shared.status() = ExportStatus::Failed(format!("failed to start export thread: {e}"));
            None
        }
    };
    ExportJob {
        shared,
        formats,
        handle,
    }
}

/// `<Artist> - <Title> (1m05-1m35) 9x16.mp4` in `dir`, with ` (2)`, ` (3)`… added if taken.
pub fn output_path(
    dir: &Path,
    song: &SongInfo,
    start: Duration,
    end: Duration,
    aspect: Aspect,
) -> PathBuf {
    let title = match song.title.trim() {
        "" => Path::new(&song.file)
            .file_stem()
            .map(|s| s.to_string_lossy().trim().to_owned())
            .unwrap_or_default(),
        t => t.to_owned(),
    };
    let artist = song.artist.trim();
    let name = match (artist.is_empty(), title.is_empty()) {
        (true, true) => "clip".to_owned(),
        (true, false) => title,
        (false, true) => artist.to_owned(),
        (false, false) => format!("{artist} - {title}"),
    };
    let stem = format!(
        "{} ({}-{}) {}",
        sanitize(&name),
        clip_time(start),
        clip_time(end),
        aspect.label()
    );
    let mut path = dir.join(format!("{stem}.mp4"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem} ({n}).mp4"));
        n += 1;
    }
    path
}

/// `1m05` for 65 seconds.
fn clip_time(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}m{:02}", s / 60, s % 60)
}

/// Replaces characters unsafe in file names, collapses whitespace, hides no dotfiles and caps
/// the length at a char boundary.
fn sanitize(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| {
            if c.is_control() || r#"/\:*?"<>|"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut out = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.starts_with('.') {
        out.replace_range(..1, "_");
    }
    if out.len() > MAX_NAME_BYTES {
        let mut cut = MAX_NAME_BYTES;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.truncate(out.trim_end().len());
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Hidden in-progress name next to `out`, renamed to `out` once ffmpeg succeeds.
fn part_path(out: &Path) -> PathBuf {
    let name = out
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    out.with_file_name(format!(".{name}.part.mp4"))
}

enum Stop {
    Cancelled,
    Failed(String),
}

impl From<String> for Stop {
    fn from(e: String) -> Self {
        Stop::Failed(e)
    }
}

struct Worker {
    shared: Arc<Shared>,
    music_dir: Option<PathBuf>,
    export_dir: PathBuf,
    on_progress: Box<dyn Fn() + Send + Sync>,
    last_notify: Option<Instant>,
}

impl Worker {
    fn run(mut self, req: ExportRequest, mut scene: Box<dyn Scene>) {
        let started = Instant::now();
        let mut written = Vec::new();
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            self.export(&req, scene.as_mut(), &mut written)
        }))
        .unwrap_or_else(|_| Err(Stop::Failed("export crashed".to_owned())));
        let status = match result {
            Ok(warning) => {
                info!(
                    "exported {} file(s) in {:.1}s",
                    written.len(),
                    started.elapsed().as_secs_f32()
                );
                ExportStatus::Done(written, warning)
            }
            Err(Stop::Cancelled) => ExportStatus::Cancelled,
            Err(Stop::Failed(e)) => {
                warn!("export failed: {e}");
                ExportStatus::Failed(e)
            }
        };
        self.set_status(status, true);
    }

    fn cancelled(&self) -> bool {
        self.shared.cancel.load(Ordering::Relaxed)
    }

    fn set_status(&mut self, status: ExportStatus, force: bool) {
        *self.shared.status() = status;
        let due = self
            .last_notify
            .is_none_or(|at| at.elapsed() >= NOTIFY_INTERVAL);
        if force || due {
            self.last_notify = Some(Instant::now());
            (self.on_progress)();
        }
    }

    /// Renders every format; returns a warning for the finished status.
    fn export(
        &mut self,
        req: &ExportRequest,
        scene: &mut dyn Scene,
        written: &mut Vec<PathBuf>,
    ) -> Result<Option<String>, Stop> {
        self.set_status(ExportStatus::Preparing, true);
        if req.end <= req.start {
            return Err(Stop::Failed("the clip range is empty".to_owned()));
        }
        if req.formats.is_empty() {
            return Err(Stop::Failed("no format selected".to_owned()));
        }
        let fps = req.fps.max(1);
        std::fs::create_dir_all(&self.export_dir).map_err(|e| {
            format!(
                "failed to create {}: {e}",
                self.export_dir.to_string_lossy()
            )
        })?;

        let (audio, warning) = self.audio_source(req);
        let (w, h) = req.formats[0].size();
        let mut renderer = Renderer::new([w, h])?;
        info!("export rendering on {}", renderer.adapter_name());
        for (index, &aspect) in req.formats.iter().enumerate() {
            if self.cancelled() {
                return Err(Stop::Cancelled);
            }
            if index > 0 {
                let (w, h) = aspect.size();
                renderer.reset([w, h])?;
            }
            scene.prepare(renderer.ctx());

            let out = output_path(&self.export_dir, &req.song, req.start, req.end, aspect);
            let part = part_path(&out);
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                self.render_one(
                    req,
                    scene,
                    &mut renderer,
                    aspect,
                    fps,
                    &part,
                    audio.as_deref(),
                )
            }))
            .unwrap_or_else(|_| Err(Stop::Failed("rendering crashed".to_owned())));
            match result {
                Ok(()) => {
                    std::fs::rename(&part, &out).map_err(|e| {
                        let _ = std::fs::remove_file(&part);
                        format!("failed to save {}: {e}", out.to_string_lossy())
                    })?;
                    info!("exported {}", out.to_string_lossy());
                    written.push(out);
                }
                Err(e) => {
                    let _ = std::fs::remove_file(&part);
                    return Err(e);
                }
            }
        }
        Ok(warning)
    }

    /// Path of the song's audio file when audio is wanted, or a warning explaining why there is none.
    fn audio_source(&self, req: &ExportRequest) -> (Option<PathBuf>, Option<String>) {
        if !req.with_audio {
            return (None, None);
        }
        let Some(dir) = &self.music_dir else {
            return (
                None,
                Some("no audio: MPD music directory unknown".to_owned()),
            );
        };
        let path = dir.join(&req.song.file);
        if path.is_file() {
            (Some(path), None)
        } else {
            (
                None,
                Some(format!("no audio: {} not found", path.to_string_lossy())),
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_one(
        &mut self,
        req: &ExportRequest,
        scene: &mut dyn Scene,
        renderer: &mut Renderer,
        aspect: Aspect,
        fps: u32,
        part: &Path,
        audio: Option<&Path>,
    ) -> Result<(), Stop> {
        let clip = req.end - req.start;
        let total = (clip.as_secs_f64() * f64::from(fps)).round().max(1.0) as u32;
        let pre_roll = (PRE_ROLL.as_secs_f64() * f64::from(fps)).round() as u32;
        let args = ffmpeg::build_args(
            part,
            aspect.size(),
            fps,
            audio.map(|path| (path, req.start, clip)),
        );
        let mut encoder = Encoder::spawn(&args)?;
        let dt = 1.0 / fps as f32;
        let started = Instant::now();

        self.set_status(
            ExportStatus::Rendering {
                aspect,
                frame: 0,
                total,
            },
            true,
        );
        for i in 0..pre_roll + total {
            if self.cancelled() {
                encoder.kill();
                return Err(Stop::Cancelled);
            }
            let offset = (f64::from(i) - f64::from(pre_roll)) / f64::from(fps);
            let elapsed = Duration::from_secs_f64((req.start.as_secs_f64() + offset).max(0.0));
            let inputs = FrameInputs {
                elapsed,
                song: &req.song,
                lyrics: &req.lyrics,
            };
            let time = f64::from(i) / f64::from(fps);
            let pixels = match renderer.render_frame(time, dt, |ui| scene.draw(ui, &inputs)) {
                Ok(p) => p,
                Err(e) => {
                    encoder.kill();
                    return Err(Stop::Failed(e));
                }
            };
            if i < pre_roll {
                continue;
            }
            encoder.write_frame(pixels)?;
            let frame = i - pre_roll + 1;
            self.set_status(
                ExportStatus::Rendering {
                    aspect,
                    frame,
                    total,
                },
                false,
            );
        }
        encoder.finish()?;
        let secs = started.elapsed().as_secs_f64();
        info!(
            "{} rendered {} frames in {secs:.1}s ({:.1} fps)",
            aspect.display_name(),
            pre_roll + total,
            f64::from(pre_roll + total) / secs.max(1e-6)
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(artist: &str, title: &str, file: &str) -> SongInfo {
        SongInfo {
            id: 1,
            file: file.to_owned(),
            title: title.to_owned(),
            artist: artist.to_owned(),
            album: String::new(),
            duration: Some(Duration::from_secs(200)),
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mpd-display-export-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn spotify_style_is_landscape_only() {
        let chosen = [Aspect::Square, Aspect::Portrait];
        assert_eq!(
            Style::Dark.formats(&chosen),
            [Aspect::Portrait, Aspect::Square]
        );
        assert_eq!(Style::Spotify.formats(&chosen), [Aspect::Landscape]);
        assert_eq!(Style::Spotify.formats(&[]), [Aspect::Landscape]);
        assert!(Style::Spotify.supports(Aspect::Landscape));
        assert!(!Style::Spotify.supports(Aspect::Portrait));
        assert!(Aspect::ALL.iter().all(|&a| Style::Vibrant.supports(a)));
        assert_eq!(Style::for_layout(Layout::Spotify), Style::Spotify);
        assert_eq!(Style::for_layout(Layout::Apple), Style::Dark);
    }

    #[test]
    fn aspect_sizes_and_labels() {
        assert_eq!(Aspect::Landscape.size(), (1920, 1080));
        assert_eq!(Aspect::Portrait.size(), (1080, 1920));
        assert_eq!(Aspect::Square.size(), (1080, 1080));
        let labels: Vec<_> = Aspect::ALL.iter().map(|a| a.label()).collect();
        assert_eq!(labels, ["16x9", "9x16", "1x1"]);
        let names: Vec<_> = Aspect::ALL.iter().map(|a| a.display_name()).collect();
        assert_eq!(names, ["16:9", "9:16", "1:1"]);
    }

    #[test]
    fn output_path_naming() {
        let dir = Path::new("/nonexistent-export-dir");
        let p = output_path(
            dir,
            &song("NF", "Let You Down", "x.flac"),
            Duration::from_millis(65_900),
            Duration::from_secs(95),
            Aspect::Portrait,
        );
        assert_eq!(p, dir.join("NF - Let You Down (1m05-1m35) 9x16.mp4"));

        let p = output_path(
            dir,
            &song("AC/DC", "What?  Is: <this>\t|now|", "x.flac"),
            Duration::ZERO,
            Duration::from_secs(600),
            Aspect::Square,
        );
        assert_eq!(
            p,
            dir.join("AC_DC - What_ Is_ _this___now_ (0m00-10m00) 1x1.mp4")
        );

        let p = output_path(
            dir,
            &song("", "", "a/.hidden.mp3"),
            Duration::ZERO,
            Duration::from_secs(5),
            Aspect::Landscape,
        );
        assert_eq!(p, dir.join("_hidden (0m00-0m05) 16x9.mp4"));

        let p = output_path(
            dir,
            &song("", "", ""),
            Duration::ZERO,
            Duration::from_secs(5),
            Aspect::Landscape,
        );
        assert_eq!(p, dir.join("clip (0m00-0m05) 16x9.mp4"));

        let long = output_path(
            dir,
            &song("", &"é".repeat(200), "f"),
            Duration::ZERO,
            Duration::from_secs(5),
            Aspect::Landscape,
        );
        let name = long.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.len() <= MAX_NAME_BYTES + 40, "{name}");
        assert!(name.ends_with(" (0m00-0m05) 16x9.mp4"));
    }

    #[test]
    fn output_path_collisions() {
        let dir = temp_dir("collide");
        let s = song("A", "B", "f");
        let at = |n| {
            output_path(
                &dir,
                &s,
                Duration::ZERO,
                Duration::from_secs(n),
                Aspect::Portrait,
            )
        };
        let first = at(5);
        assert_eq!(first, dir.join("A - B (0m00-0m05) 9x16.mp4"));
        std::fs::write(&first, b"").expect("write");
        assert_eq!(at(5), dir.join("A - B (0m00-0m05) 9x16 (2).mp4"));
        std::fs::write(at(5), b"").expect("write");
        assert_eq!(at(5), dir.join("A - B (0m00-0m05) 9x16 (3).mp4"));
        assert_eq!(
            part_path(&first),
            dir.join(".A - B (0m00-0m05) 9x16.part.mp4")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Draws a red block, a block moving with the song position, and text.
    struct TestScene;

    impl Scene for TestScene {
        fn prepare(&mut self, _ctx: &egui::Context) {}

        fn draw(&mut self, ui: &mut egui::Ui, frame: &FrameInputs<'_>) {
            let rect = ui.max_rect();
            let painter = ui.painter();
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(20, 40, 90));
            let red = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() / 2.0, 200.0));
            painter.rect_filled(red, 0.0, egui::Color32::from_rgb(255, 0, 0));
            let x = (frame.elapsed.as_secs_f32() * 100.0) % rect.width();
            let moving = egui::Rect::from_min_size(egui::pos2(x, 400.0), egui::vec2(80.0, 80.0));
            painter.rect_filled(moving, 8.0, egui::Color32::from_rgb(0, 200, 120));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                format!("{} {:.2}", frame.song.title, frame.elapsed.as_secs_f32()),
                crate::ui::font_bold(48.0),
                egui::Color32::WHITE,
            );
        }
    }

    fn run(cmd: &mut std::process::Command) -> String {
        let out = cmd.output().expect("run command");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    #[ignore = "needs a GPU"]
    fn render_pixels() {
        let mut r = Renderer::new([64, 32]).expect("renderer");
        let px = r
            .render_frame(0.0, 1.0 / 30.0, |ui| {
                let rect = ui.max_rect();
                let (left, right) = rect.split_left_right_at_fraction(0.5);
                ui.painter()
                    .rect_filled(left, 0.0, egui::Color32::from_rgb(255, 0, 0));
                ui.painter()
                    .rect_filled(right, 0.0, egui::Color32::from_rgb(0, 128, 255));
            })
            .expect("frame")
            .to_vec();
        assert_eq!(px.len(), 64 * 32 * 4);
        let at = |x: usize, y: usize| &px[(y * 64 + x) * 4..][..4];
        let close = |a: &[u8], b: [u8; 4]| a.iter().zip(b).all(|(&a, b)| a.abs_diff(b) <= 1);
        assert!(close(at(10, 10), [255, 0, 0, 255]), "{:?}", at(10, 10));
        assert!(close(at(50, 20), [0, 128, 255, 255]), "{:?}", at(50, 20));
    }

    /// Renders 2s at 540x960 with audio through ffmpeg and checks the result with ffprobe.
    #[test]
    #[ignore = "needs a GPU and ffmpeg"]
    fn export_smoke() {
        let dir = temp_dir("smoke");
        let audio = dir.join("a.flac");
        run(std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "sine=frequency=440:duration=5"])
            .arg(&audio));

        let s = song("Artist", "Smoke", "a.flac");
        let lyrics = LyricsState::NotFound;
        let (w, h, fps) = (540u32, 960u32, 30u32);
        let start = Duration::from_millis(1500);
        let clip = Duration::from_secs(2);
        let out = dir.join("out.mp4");
        let args = ffmpeg::build_args(&out, (w, h), fps, Some((&audio, start, clip)));
        let mut encoder = Encoder::spawn(&args).expect("ffmpeg");
        let mut renderer = Renderer::new([w, h]).expect("renderer");
        let mut scene = TestScene;

        let frames = 2 * fps;
        let began = Instant::now();
        for i in 0..frames {
            let elapsed = start + Duration::from_secs_f64(f64::from(i) / f64::from(fps));
            let inputs = FrameInputs {
                elapsed,
                song: &s,
                lyrics: &lyrics,
            };
            let px = renderer
                .render_frame(f64::from(i) / 30.0, 1.0 / 30.0, |ui| {
                    scene.draw(ui, &inputs)
                })
                .expect("frame");
            encoder.write_frame(px).expect("write");
        }
        let render_secs = began.elapsed().as_secs_f64();
        encoder.finish().expect("encode");
        println!(
            "{frames} frames at {w}x{h} on {}: {:.1} fps incl. encoding",
            renderer.adapter_name(),
            f64::from(frames) / render_secs
        );

        let probe = run(std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_name,width,height,pix_fmt",
            ])
            .args(["-show_entries", "format=duration", "-of", "json"])
            .arg(&out));
        let json: serde_json::Value = serde_json::from_str(&probe).expect("ffprobe json");
        let streams = json["streams"].as_array().expect("streams");
        let video = streams
            .iter()
            .find(|s| s["codec_name"] == "h264")
            .expect("h264 stream");
        assert_eq!(video["width"], 540);
        assert_eq!(video["height"], 960);
        assert_eq!(video["pix_fmt"], "yuv420p");
        assert!(streams.iter().any(|s| s["codec_name"] == "aac"), "{probe}");
        let duration: f64 = json["format"]["duration"]
            .as_str()
            .and_then(|d| d.parse().ok())
            .expect("duration");
        assert!((duration - 2.0).abs() < 0.15, "duration {duration}");

        if let Some(frame_png) = std::env::var_os("EXPORT_SMOKE_FRAME") {
            run(std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", "1", "-i"])
                .arg(&out)
                .args(["-frames:v", "1"])
                .arg(frame_png));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Exports 6 seconds of a library song with a fixture LRC, then checks the files with
    /// ffprobe and saves frames every 0.5 s from 1 s to 4 s next to them.
    /// Set `MPD_DISPLAY_TEST_SONG` to an MPD-relative file and `MPD_DISPLAY_EXPORT_DIR` to the
    /// output folder. Optional: `MPD_DISPLAY_TEST_STYLE` (`dark`, `vibrant` or `spotify`),
    /// `MPD_DISPLAY_TEST_MOTION=off`, `MPD_DISPLAY_TEST_START` (clip start in seconds, default
    /// 60), `MPD_DISPLAY_TEST_FORMATS` (comma-separated labels such as `16x9,1x1`) and
    /// `MPD_DISPLAY_TEST_NEXT` (the queued song for the Up next card) and
    /// `MPD_DISPLAY_TEST_ARTIST_BG=on|off` (default on for the Spotify style only). MPD is only queried for
    /// tags and art.
    #[test]
    #[ignore = "needs a GPU, ffmpeg, MPD and a song from the library"]
    fn export_of_library_song() {
        let Ok(file) = std::env::var("MPD_DISPLAY_TEST_SONG") else {
            eprintln!("skipped: MPD_DISPLAY_TEST_SONG is not set");
            return;
        };
        let cfg = Config::from_env();
        let lookup = |file: &str| {
            let tags = run(std::process::Command::new("mpc")
                .args([
                    "--host",
                    &cfg.mpc_host(),
                    "--port",
                    &cfg.mpd_port.to_string(),
                ])
                .args([
                    "-f",
                    "[%artist%]::[%title%]::[%album%]::[%time%]",
                    "ls",
                    file,
                ]));
            let mut tags = tags.lines().next().unwrap_or_default().split("::");
            let mut tag = || tags.next().unwrap_or_default().to_owned();
            let (artist, title, album) = (tag(), tag(), tag());
            let duration = tag()
                .split_once(':')
                .and_then(|(m, s)| Some(m.parse::<u64>().ok()? * 60 + s.parse::<u64>().ok()?))
                .unwrap_or(200);
            SongInfo {
                id: 1,
                file: file.to_owned(),
                artist,
                title,
                album,
                duration: Some(Duration::from_secs(duration)),
            }
        };
        let song = lookup(&file);
        let next = std::env::var("MPD_DISPLAY_TEST_NEXT")
            .ok()
            .filter(|f| !f.is_empty())
            .map(|f| lookup(&f));
        let start: u64 = std::env::var("MPD_DISPLAY_TEST_START")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);

        let lrc: String = [
            (-1.0, "This is the line before the clip"),
            (0.5, "First line inside the clip"),
            (
                2.0,
                "A second line that is long enough to wrap onto another row in a narrow column",
            ),
            (3.5, "Third"),
            (5.0, ""),
            (6.5, "Fifth line after an instrumental gap"),
            (8.0, "After the clip"),
        ]
        .iter()
        .map(|&(at, text)| {
            let ms = ((start as f64 + at) * 1000.0) as u64;
            format!(
                "[{:02}:{:02}.{:02}]{text}\n",
                ms / 60_000,
                ms / 1000 % 60,
                ms % 1000 / 10
            )
        })
        .collect();
        let lyrics = LyricsState::Found(Arc::new(mpd_display_rs::lyrics::lrc::parse_lrc(&lrc)));
        let style = match std::env::var("MPD_DISPLAY_TEST_STYLE").as_deref() {
            Ok("vibrant") => Style::Vibrant,
            Ok("spotify") => Style::Spotify,
            _ => Style::Dark,
        };
        let formats: Vec<Aspect> = match std::env::var("MPD_DISPLAY_TEST_FORMATS") {
            Ok(list) => Aspect::ALL
                .into_iter()
                .filter(|a| list.split(',').any(|l| l.trim() == a.label()))
                .collect(),
            Err(_) => Aspect::ALL.to_vec(),
        };
        let formats = style.formats(&formats);
        let req = ExportRequest {
            song,
            next,
            lyrics,
            start: Duration::from_secs(start),
            end: Duration::from_secs(start + 6),
            formats: formats.clone(),
            with_audio: true,
            fps: 30,
            style,
            motion: std::env::var("MPD_DISPLAY_TEST_MOTION").as_deref() != Ok("off"),
            artist_bg: match std::env::var("MPD_DISPLAY_TEST_ARTIST_BG").as_deref() {
                Ok(v) => v != "off",
                Err(_) => style == Style::Spotify,
            },
        };
        let began = Instant::now();
        let job = spawn(&cfg, req, Box::new(|| {}));
        while !job.is_finished() {
            thread::sleep(Duration::from_millis(50));
        }
        println!(
            "exported {} x 6s in {:.1}s",
            formats.len(),
            began.elapsed().as_secs_f64()
        );
        let ExportStatus::Done(paths, warning) = job.status() else {
            panic!("unexpected status {:?}", job.status());
        };
        assert_eq!(warning, None);
        assert_eq!(paths.len(), formats.len());

        for (path, &aspect) in paths.iter().zip(&formats) {
            let probe = run(std::process::Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "stream=codec_name,width,height",
                ])
                .args(["-show_entries", "format=duration", "-of", "json"])
                .arg(path));
            let json: serde_json::Value = serde_json::from_str(&probe).expect("ffprobe json");
            let streams = json["streams"].as_array().expect("streams");
            let video = streams
                .iter()
                .find(|s| s["codec_name"] == "h264")
                .expect("h264 stream");
            let (w, h) = aspect.size();
            assert_eq!(
                (video["width"].as_u64(), video["height"].as_u64()),
                (Some(u64::from(w)), Some(u64::from(h)))
            );
            assert!(streams.iter().any(|s| s["codec_name"] == "aac"), "{probe}");
            let duration: f64 = json["format"]["duration"]
                .as_str()
                .and_then(|d| d.parse().ok())
                .expect("duration");
            assert!((duration - 6.0).abs() < 0.2, "duration {duration}");

            for half in 2..=8 {
                let at = f64::from(half) / 2.0;
                let png = cfg
                    .export_dir
                    .join(format!("frame-{}-{at:.1}s.png", aspect.label()));
                run(std::process::Command::new("ffmpeg")
                    .args([
                        "-hide_banner",
                        "-loglevel",
                        "error",
                        "-y",
                        "-ss",
                        &at.to_string(),
                        "-i",
                    ])
                    .arg(path)
                    .args(["-frames:v", "1"])
                    .arg(&png));
                println!("{}", png.to_string_lossy());
            }
        }
    }

    /// Runs the export thread end to end: pre-roll, progress, renaming and the missing-audio warning.
    #[test]
    #[ignore = "needs a GPU and ffmpeg"]
    fn export_job_completes() {
        let dir = temp_dir("job");
        let cfg = Config::from_lookup(|key| match key {
            "MPD_MUSIC_DIR" => Some(dir.to_string_lossy().into_owned()),
            "MPD_DISPLAY_EXPORT_DIR" => Some(dir.join("out").to_string_lossy().into_owned()),
            _ => None,
        });
        let req = ExportRequest {
            song: song("Artist", "Job", "missing.flac"),
            next: None,
            lyrics: LyricsState::NotFound,
            start: Duration::from_secs(10),
            end: Duration::from_millis(10_500),
            formats: vec![Aspect::Square, Aspect::Square],
            with_audio: true,
            fps: 30,
            style: Style::Dark,
            motion: true,
            artist_bg: false,
        };
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let job = spawn_with_scene(
            &cfg,
            req,
            Box::new(TestScene),
            Box::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
            }),
        );
        assert_eq!(job.formats(), [Aspect::Square]);
        let began = Instant::now();
        while !job.is_finished() {
            thread::sleep(Duration::from_millis(20));
        }
        // 45 pre-roll + 15 clip frames at 1080x1080.
        println!(
            "export job: {:.1} fps incl. encoding",
            60.0 / began.elapsed().as_secs_f64()
        );
        let ExportStatus::Done(paths, warning) = job.status() else {
            panic!("unexpected status {:?}", job.status());
        };
        assert_eq!(paths, [dir.join("out/Artist - Job (0m10-0m10) 1x1.mp4")]);
        assert!(paths[0].is_file());
        assert!(warning.is_some_and(|w| w.starts_with("no audio")));
        assert_eq!(job.progress(), 1.0);
        assert!(calls.load(Ordering::Relaxed) >= 3);
        let leftovers = std::fs::read_dir(dir.join("out")).expect("out dir").count();
        assert_eq!(leftovers, 1);
        drop(job);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
