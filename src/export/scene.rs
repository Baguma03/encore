//! What an export draws on each frame.

use super::{ExportRequest, FrameInputs};
use crate::art::{self, ArtImages, ArtTextures};
use crate::config::Config;
use crate::player::{PlayState, PlayerState, SongInfo};
use crate::ui::{apple, ArtView, View};

/// Draws export frames. Lives on the export thread.
pub trait Scene: Send {
    /// Called with a fresh egui context before the first frame of each format;
    /// textures used by `draw` must be uploaded into this context.
    fn prepare(&mut self, ctx: &egui::Context);

    /// Draws one frame filling `ui.max_rect()`.
    fn draw(&mut self, ui: &mut egui::Ui, frame: &FrameInputs<'_>);
}

/// The scene used by [`super::spawn`]: the Apple layout as a non-interactive capture.
pub fn default_scene(cfg: &Config, req: &ExportRequest) -> Box<dyn Scene> {
    Box::new(AppleScene::new(cfg.clone(), req.song.clone()))
}

/// The Apple layout without clock, hover effects or interaction. Art is loaded on the export
/// thread on first use and uploaded into each format's context.
pub struct AppleScene {
    cfg: Config,
    song: SongInfo,
    images: Option<ArtImages>,
    art: Option<ArtTextures>,
}

impl AppleScene {
    pub fn new(cfg: Config, song: SongInfo) -> Self {
        Self {
            cfg,
            song,
            images: None,
            art: None,
        }
    }
}

impl Scene for AppleScene {
    fn prepare(&mut self, ctx: &egui::Context) {
        let images = self
            .images
            .get_or_insert_with(|| art::load_art_images(&self.cfg, &self.song));
        self.art = Some(art::upload(ctx, &self.song.file, images.clone()));
    }

    fn draw(&mut self, ui: &mut egui::Ui, frame: &FrameInputs<'_>) {
        let player = PlayerState::snapshot(frame.song.clone(), frame.elapsed, PlayState::Playing);
        let view = View {
            player: &player,
            art: ArtView {
                current: self.art.as_ref(),
                previous: None,
                fade: 1.0,
            },
            watermark: None,
            show_clock: false,
            lyrics_enabled: true,
            lyrics: frame.lyrics.clone(),
            now: chrono::Local::now(),
            capture: true,
        };
        apple::draw(ui, &view);
    }
}
