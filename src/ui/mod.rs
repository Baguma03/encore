pub mod apple;
pub mod export_panel;
pub mod spotify;
pub mod toasts;
pub mod widgets;

use std::sync::Arc;
use std::time::Duration;

use egui::{
    Color32, FontData, FontDefinitions, FontFamily, FontId, Galley, Painter, Pos2, Rect, Vec2,
};

use mpd_display_rs::lyrics::LyricsState;

use crate::art::ArtTextures;
use crate::player::PlayerState;

pub const FONT_LIGHT: &str = "circular-book";
pub const FONT_BOLD: &str = "bold";

/// Album art to draw, with the previous song's art kept for the cross-fade.
#[derive(Clone, Copy, Default)]
pub struct ArtView<'a> {
    pub current: Option<&'a ArtTextures>,
    pub previous: Option<&'a ArtTextures>,
    /// Opacity of `current` over `previous`; 1.0 once the cross-fade is over.
    pub fade: f32,
}

/// Everything a layout needs to draw one frame. Animations read egui's input time, so a frame
/// depends only on this and the `RawInput` it is drawn with.
pub struct View<'a> {
    pub player: &'a PlayerState,
    pub art: ArtView<'a>,
    pub watermark: Option<&'a egui::TextureHandle>,
    pub show_clock: bool,
    pub lyrics_enabled: bool,
    /// Lyrics of the current song; `NotFound` when disabled or nothing is playing.
    pub lyrics: LyricsState,
    /// Wall-clock time shown by the clock.
    pub now: chrono::DateTime<chrono::Local>,
    /// Off-screen rendering: no clock, no hover effects and no interaction.
    pub capture: bool,
}

/// Registers CircularStd Book as the default proportional font and Bold as the `"bold"` family.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        FONT_LIGHT.to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/font/CircularStd-Book.otf"
        ))),
    );
    fonts.font_data.insert(
        FONT_BOLD.to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../assets/font/CircularStd-Bold.otf"
        ))),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, FONT_LIGHT.to_owned());
    // Keep egui's fallbacks behind the bold face for glyphs CircularStd lacks.
    let mut bold = vec![FONT_BOLD.to_owned()];
    bold.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .into_iter()
            .flatten()
            .cloned(),
    );
    fonts
        .families
        .insert(FontFamily::Name(FONT_BOLD.into()), bold);
    ctx.set_fonts(fonts);
}

pub fn font_light(size: f32) -> FontId {
    FontId::proportional(size)
}

pub fn font_bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FONT_BOLD.into()))
}

/// Formats a duration as `m:ss`.
pub fn fmt_time(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Lays out `text` on one line, shrinking the font by 5% steps until it is narrower than `max_width`.
pub fn fit_text(
    painter: &Painter,
    text: &str,
    font: fn(f32) -> FontId,
    size: f32,
    max_width: f32,
    color: Color32,
) -> Arc<Galley> {
    let mut size = size;
    loop {
        let galley = painter.layout_no_wrap(text.to_owned(), font(size), color);
        if galley.size().x < max_width || size < 4.0 {
            return galley;
        }
        size *= 0.95;
    }
}

/// Rect of an image of `image_size` scaled by `scale` with its top-left corner at `top_left`.
pub fn scaled_rect(image_size: Vec2, scale: f32, top_left: Pos2) -> Rect {
    Rect::from_min_size(top_left, (image_size * scale).ceil())
}

/// Rect that makes an image of `image_size` cover `target` completely, centred and cropped by the clip.
pub fn cover_rect(image_size: Vec2, target: Rect) -> Rect {
    if image_size.x <= 0.0 || image_size.y <= 0.0 {
        return target;
    }
    let scale = (target.width() / image_size.x).max(target.height() / image_size.y);
    Rect::from_center_size(target.center(), image_size * scale)
}

/// Full-texture UV rect.
pub const FULL_UV: Rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));

/// Multiplies a tint by an opacity.
pub fn with_alpha(color: Color32, alpha: f32) -> Color32 {
    color.gamma_multiply(alpha.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_time() {
        assert_eq!(fmt_time(Duration::from_secs(0)), "0:00");
        assert_eq!(fmt_time(Duration::from_secs(65)), "1:05");
        assert_eq!(fmt_time(Duration::from_millis(599_999)), "9:59");
        assert_eq!(fmt_time(Duration::from_secs(3600)), "60:00");
    }

    #[test]
    fn cover_rect_fills_target() {
        let target = Rect::from_min_size(Pos2::ZERO, Vec2::new(1920.0, 1080.0));
        let wide = cover_rect(Vec2::new(3000.0, 1000.0), target);
        assert_eq!(wide.height(), 1080.0);
        assert!(wide.width() >= 1920.0);
        assert!((wide.center() - target.center()).length() < 1e-3);

        let tall = cover_rect(Vec2::new(500.0, 1000.0), target);
        assert_eq!(tall.width(), 1920.0);
        assert!(tall.height() >= 1080.0);
    }

    #[test]
    fn scaled_rect_rounds_up() {
        let r = scaled_rect(Vec2::new(10.0, 5.0), 1.55, Pos2::new(2.0, 3.0));
        assert_eq!(r.min, Pos2::new(2.0, 3.0));
        assert_eq!(r.size(), Vec2::new(16.0, 8.0));
    }
}
