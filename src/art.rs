use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use image::imageops::{self, FilterType};
use image::{DynamicImage, RgbaImage};
use log::{debug, warn};

use crate::config::Config;
use crate::player::SongInfo;

const WATERMARK_PATH: &str = "./assets/logo.png";
const BACKUP_ART_PATH: &str = "./assets/art_backup";
const ARTISTS_DIR: &str = "./artists";
const ARTIST_EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

const COVER_MAX: u32 = 1024;
const ARTIST_BG_MAX: u32 = 2560;
const BACKDROP_SMALL: u32 = 64;
const BACKDROP_LARGE: u32 = 256;
const BACKDROP_BLUR_SIGMA: f32 = 7.0;
const BACKDROP_BRIGHTNESS: f32 = 0.55;
const DOMINANT_SATURATION: f32 = 1.25;

/// Length of the cross-fade between the previous and current art.
pub const ART_FADE: Duration = Duration::from_millis(400);

pub struct ArtTextures {
    pub song_file: String,
    pub cover: TextureHandle,
    pub backdrop: TextureHandle,
    pub artist_bg: Option<TextureHandle>,
    pub dominant: Color32,
}

/// Decoded images for one song, ready to upload as textures.
#[derive(Clone)]
pub struct ArtImages {
    pub cover: ColorImage,
    /// Blurred, darkened small copy of the cover.
    pub backdrop: ColorImage,
    pub artist_bg: Option<ColorImage>,
    pub dominant: Color32,
}

struct Request {
    song_file: String,
    artist: String,
}

/// Loads album art and artist backgrounds on a worker thread; textures are created on the UI thread.
pub struct ArtLoader {
    tx: mpsc::Sender<Request>,
    rx: mpsc::Receiver<(String, ArtImages)>,
    requested: RefCell<Option<String>>,
    current: Option<ArtTextures>,
    previous: Option<ArtTextures>,
    changed_at: Option<Instant>,
    watermark_image: Option<ColorImage>,
    watermark: Option<TextureHandle>,
}

impl ArtLoader {
    pub fn new(cfg: &Config, on_ready: Box<dyn Fn() + Send + Sync>) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<Request>();
        let (res_tx, res_rx) = mpsc::channel();

        let worker = Worker::new(cfg);
        let spawned = thread::Builder::new()
            .name("art-loader".into())
            .spawn(move || {
                while let Ok(mut req) = req_rx.recv() {
                    // Only the most recent request matters after rapid skips.
                    while let Ok(newer) = req_rx.try_recv() {
                        req = newer;
                    }
                    let images = worker.load(&req.song_file, &req.artist);
                    debug!("art ready for {}", req.song_file);
                    if res_tx.send((req.song_file, images)).is_err() {
                        return;
                    }
                    on_ready();
                }
            });
        if let Err(e) = spawned {
            warn!("failed to start art loader thread: {e}");
        }

        Self {
            tx: req_tx,
            rx: res_rx,
            requested: RefCell::new(None),
            current: None,
            previous: None,
            changed_at: None,
            watermark_image: load_watermark(),
            watermark: None,
        }
    }

    /// Queues art loading for `song`, ignoring repeated requests for the same file.
    pub fn request(&self, song: &SongInfo) {
        let mut requested = self.requested.borrow_mut();
        if requested.as_deref() == Some(song.file.as_str()) {
            return;
        }
        *requested = Some(song.file.clone());
        let req = Request {
            song_file: song.file.clone(),
            artist: song.artist.clone(),
        };
        if self.tx.send(req).is_err() {
            warn!("art loader thread is gone");
        }
    }

    /// Uploads finished images as textures. Returns true if the current art changed.
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        if let Some(img) = self.watermark_image.take() {
            self.watermark = Some(ctx.load_texture("watermark", img, TextureOptions::LINEAR));
        }

        let mut changed = false;
        while let Ok((file, images)) = self.rx.try_recv() {
            if self.requested.borrow().as_deref() != Some(file.as_str()) {
                continue;
            }
            let textures = upload(ctx, &file, images);
            self.previous = self.current.replace(textures);
            self.changed_at = Some(Instant::now());
            changed = true;
        }
        changed
    }

    pub fn current(&self) -> Option<&ArtTextures> {
        self.current.as_ref()
    }

    /// The art shown before the current one, kept for cross-fading.
    pub fn previous(&self) -> Option<&ArtTextures> {
        self.previous.as_ref()
    }

    /// Opacity of the current art during the cross-fade from the previous one.
    pub fn fade(&self) -> f32 {
        match self.changed_at {
            Some(at) if self.previous.is_some() => {
                (at.elapsed().as_secs_f32() / ART_FADE.as_secs_f32()).min(1.0)
            }
            _ => 1.0,
        }
    }

    pub fn watermark(&self) -> Option<&TextureHandle> {
        self.watermark.as_ref()
    }
}

/// Uploads `images` as textures named after `song_file`.
pub fn upload(ctx: &egui::Context, song_file: &str, images: ArtImages) -> ArtTextures {
    let load = |kind: &str, img| {
        ctx.load_texture(format!("{kind}:{song_file}"), img, TextureOptions::LINEAR)
    };
    ArtTextures {
        cover: load("cover", images.cover),
        backdrop: load("backdrop", images.backdrop),
        artist_bg: images.artist_bg.map(|img| load("artist", img)),
        dominant: images.dominant,
        song_file: song_file.to_owned(),
    }
}

/// Loads the art for `song` synchronously, the same way the background loader does.
pub fn load_art_images(cfg: &Config, song: &SongInfo) -> ArtImages {
    Worker::new(cfg).load(&song.file, &song.artist)
}

/// Decodes the watermark logo, if present.
pub fn load_watermark() -> Option<ColorImage> {
    load_file(Path::new(WATERMARK_PATH)).map(|img| to_color_image(&img.to_rgba8()))
}

struct Worker {
    host: String,
    port: String,
    backup: Arc<DynamicImage>,
}

impl Worker {
    fn new(cfg: &Config) -> Self {
        let backup = load_file(Path::new(BACKUP_ART_PATH)).unwrap_or_else(|| {
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(1, 1, [80, 80, 80, 255].into()))
        });
        Self {
            host: cfg.mpc_host(),
            port: cfg.mpd_port.to_string(),
            backup: Arc::new(backup),
        }
    }

    fn load(&self, song_file: &str, artist: &str) -> ArtImages {
        let cover = ["readpicture", "albumart"]
            .into_iter()
            .find_map(|cmd| self.mpc_image(cmd, song_file))
            .unwrap_or_else(|| (*self.backup).clone());
        let cover = if cover.width().max(cover.height()) > COVER_MAX {
            cover.resize(COVER_MAX, COVER_MAX, FilterType::Triangle)
        } else {
            cover
        };
        let small = cover.thumbnail(BACKDROP_SMALL, BACKDROP_SMALL).to_rgba8();

        ArtImages {
            backdrop: to_color_image(&backdrop(&small)),
            dominant: dominant_color(&small),
            cover: to_color_image(&cover.to_rgba8()),
            artist_bg: artist_background(artist).map(|img| {
                let img = if img.width().max(img.height()) > ARTIST_BG_MAX {
                    img.resize(ARTIST_BG_MAX, ARTIST_BG_MAX, FilterType::Triangle)
                } else {
                    img
                };
                to_color_image(&img.to_rgba8())
            }),
        }
    }

    fn mpc_image(&self, cmd: &str, file: &str) -> Option<DynamicImage> {
        let output = Command::new("mpc")
            .args(["--host", &self.host, "--port", &self.port, cmd, file])
            .output();
        match output {
            Ok(out) if out.status.success() && !out.stdout.is_empty() => {
                match image::load_from_memory(&out.stdout) {
                    Ok(img) => Some(img),
                    Err(e) => {
                        debug!("mpc {cmd} returned undecodable data for {file}: {e}");
                        None
                    }
                }
            }
            Ok(_) => {
                debug!("mpc {cmd} found no art for {file}");
                None
            }
            Err(e) => {
                warn!("failed to run mpc {cmd}: {e}");
                None
            }
        }
    }
}

/// First artist of a multi-artist tag, lowercased to match `artists/` file names.
pub fn first_artist(artist: &str) -> String {
    [", ", "/", " & ", "; "]
        .iter()
        .fold(artist, |name, sep| name.split(sep).next().unwrap_or(name))
        .trim()
        .to_lowercase()
}

fn artist_background(artist: &str) -> Option<DynamicImage> {
    let name = first_artist(artist);
    if name.is_empty() {
        return None;
    }
    ARTIST_EXTENSIONS
        .iter()
        .map(|ext| PathBuf::from(ARTISTS_DIR).join(format!("{name}.{ext}")))
        .filter(|path| path.is_file())
        .find_map(|path| load_file(&path))
}

fn load_file(path: &Path) -> Option<DynamicImage> {
    // Content sniffing lets extensionless files like the backup art decode.
    let bytes = std::fs::read(path)
        .map_err(|e| warn!("failed to read {}: {e}", path.display()))
        .ok()?;
    image::load_from_memory(&bytes)
        .map_err(|e| warn!("failed to decode {}: {e}", path.display()))
        .ok()
}

fn backdrop(small: &RgbaImage) -> RgbaImage {
    let blurred = imageops::blur(small, BACKDROP_BLUR_SIGMA);
    let scale = BACKDROP_LARGE as f32 / small.width().max(small.height()).max(1) as f32;
    let (w, h) = (
        ((small.width() as f32 * scale).round() as u32).max(1),
        ((small.height() as f32 * scale).round() as u32).max(1),
    );
    let mut large = imageops::resize(&blurred, w, h, FilterType::Triangle);
    for px in large.pixels_mut() {
        for c in &mut px.0[..3] {
            *c = (*c as f32 * BACKDROP_BRIGHTNESS) as u8;
        }
    }
    large
}

fn dominant_color(small: &RgbaImage) -> Color32 {
    let (mut sum, mut weight) = ([0.0f32; 3], 0.0f32);
    for px in small.pixels() {
        let a = px.0[3] as f32 / 255.0;
        for (s, c) in sum.iter_mut().zip(px.0) {
            *s += c as f32 * a;
        }
        weight += a;
    }
    if weight <= 0.0 {
        return Color32::from_gray(50);
    }
    let avg = sum.map(|s| s / weight);
    let gray = (avg[0] + avg[1] + avg[2]) / 3.0;
    let [r, g, b] = avg.map(|c| (gray + (c - gray) * DOMINANT_SATURATION).clamp(0.0, 255.0) as u8);
    Color32::from_rgb(r, g, b)
}

fn to_color_image(img: &RgbaImage) -> ColorImage {
    ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_artist_splits_and_lowercases() {
        assert_eq!(first_artist("Quality Control, Lil Baby"), "quality control");
        assert_eq!(first_artist("OutKast/Sleepy Brown"), "outkast");
        assert_eq!(first_artist("Mariah Carey & Boyz II Men"), "mariah carey");
        assert_eq!(first_artist("NF; Hopsin"), "nf");
        assert_eq!(first_artist(""), "");
    }

    #[test]
    fn dominant_of_solid_image() {
        let img = RgbaImage::from_pixel(4, 4, [100, 100, 100, 255].into());
        assert_eq!(dominant_color(&img), Color32::from_rgb(100, 100, 100));
    }

    #[test]
    fn backdrop_is_darkened_and_upscaled() {
        let img = RgbaImage::from_pixel(64, 32, [200, 200, 200, 255].into());
        let out = backdrop(&img);
        assert_eq!((out.width(), out.height()), (256, 128));
        assert!(out.pixels().all(|p| p.0[0] <= 111));
    }
}
