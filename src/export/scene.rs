//! What an export draws on each frame.

use egui::ColorImage;

use super::Style;
use super::{ExportRequest, FrameInputs};
use crate::art::{self, ArtImages, ArtTextures, Thumb};
use crate::config::{Config, LyricsAlign, LyricsFlow};
use crate::player::{PlayState, PlayerState, SongInfo};
use crate::ui::{apple, spotify, vibrant, ArtView, View};

/// Draws export frames. Lives on the export thread.
pub trait Scene: Send {
    /// Called with a fresh egui context before the first frame of each format;
    /// textures used by `draw` must be uploaded into this context.
    fn prepare(&mut self, ctx: &egui::Context);

    /// Draws one frame filling `ui.max_rect()`.
    fn draw(&mut self, ui: &mut egui::Ui, frame: &FrameInputs<'_>);
}

/// The scene used by [`super::spawn`]: the layout of the request's style as a
/// non-interactive capture.
pub fn default_scene(cfg: &Config, req: &ExportRequest) -> Box<dyn Scene> {
    Box::new(LayoutScene::new(cfg.clone(), req))
}

/// The layout of a [`Style`] without clock, hover effects or interaction. Art is loaded on
/// the export thread on first use and uploaded into each format's context.
pub struct LayoutScene {
    cfg: Config,
    song: SongInfo,
    next: Option<SongInfo>,
    style: Style,
    motion: bool,
    artist_bg: bool,
    tint: bool,
    skip_buttons: bool,
    lyrics_align: LyricsAlign,
    lyrics_flow: LyricsFlow,
    images: Option<ArtImages>,
    art: Option<ArtTextures>,
    next_image: Option<ColorImage>,
    next_cover: Option<Thumb>,
}

impl LayoutScene {
    /// The scene for the song, style and look of `req`.
    pub fn new(cfg: Config, req: &ExportRequest) -> Self {
        Self {
            cfg,
            song: req.song.clone(),
            next: req.next.clone(),
            style: req.style,
            motion: req.motion,
            artist_bg: req.artist_bg,
            tint: req.tint,
            skip_buttons: req.skip_buttons,
            lyrics_align: req.lyrics_align,
            lyrics_flow: req.lyrics_flow,
            images: None,
            art: None,
            next_image: None,
            next_cover: None,
        }
    }
}

impl Scene for LayoutScene {
    fn prepare(&mut self, ctx: &egui::Context) {
        let images = self
            .images
            .get_or_insert_with(|| art::load_art_images(&self.cfg, &self.song));
        self.art = Some(art::upload(ctx, &self.song.file, images.clone()));
        if let Some(next) = &self.next {
            let image = self
                .next_image
                .get_or_insert_with(|| art::load_thumb_image(&self.cfg, next));
            self.next_cover = Some(art::upload_thumb(ctx, &next.file, image.clone()));
        }
    }

    fn draw(&mut self, ui: &mut egui::Ui, frame: &FrameInputs<'_>) {
        let mut player =
            PlayerState::snapshot(frame.song.clone(), frame.elapsed, PlayState::Playing);
        player.next = self.next.clone();
        let view = View {
            player: &player,
            art: ArtView {
                current: self.art.as_ref(),
                previous: None,
                fade: 1.0,
            },
            next_cover: self.next_cover.as_ref(),
            show_clock: false,
            lyrics_enabled: true,
            lyrics: frame.lyrics.clone(),
            now: chrono::Local::now(),
            capture: true,
            motion: self.motion,
            artist_bg: self.artist_bg,
            tint: self.tint,
            skip_buttons: self.skip_buttons,
            lyrics_align: self.lyrics_align,
            lyrics_flow: self.lyrics_flow,
        };
        match self.style {
            Style::Dark => apple::draw(ui, &view),
            Style::Vibrant => vibrant::draw(ui, &view),
            Style::Spotify => spotify::draw(ui, &view),
        };
    }
}
