pub mod backdrop;
pub mod classic;
pub mod encore_classic;
pub mod export_panel;
pub mod icons;
pub mod lyrics_column;
pub mod lyrics_panel;
pub mod panel_theme;
pub mod scheme;
pub mod shortcuts;
pub mod toasts;
pub mod up_next;
pub mod vibrant;
pub mod wavy;
pub mod widgets;

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{
    Color32, FontData, FontDefinitions, FontFamily, FontId, Galley, Painter, Pos2, Rect, Vec2,
};

use encore::lyrics::LyricsState;

use crate::art::{ArtTextures, Thumb, ART_FADE};
use crate::config::{LyricsAlign, LyricsFlow};
use crate::player::PlayerState;
use scheme::{Ink, Scheme};

pub const FONT_LIGHT: &str = "circular-book";
pub const FONT_BOLD: &str = "bold";

/// Album art to draw, with the previous song's art kept for the cross-fade.
#[derive(Clone, Copy, Default)]
pub struct ArtView<'a> {
    pub current: Option<&'a ArtTextures>,
    pub previous: Option<&'a ArtTextures>,
    /// Opacity of `current` over `previous`; 1.0 once the cross-fade is over.
    pub fade: f32,
    /// The album colours come from the artist image wherever it is available, because it is
    /// the background; otherwise from the cover. See [`ArtTextures::colours`].
    pub artist_colours: bool,
    /// The colours shown before the colour source last switched, and how far the cross-fade
    /// from them has got; see [`ColourSource`].
    pub switch: Option<(Scheme, f32)>,
}

impl ArtView<'_> {
    /// The album colours of the current art, cross-fading from the previous song's and from
    /// the colours shown before the colour source switched.
    pub fn scheme(&self) -> Scheme {
        let of = |art: Option<&ArtTextures>| {
            art.map_or_else(Scheme::default, |a| a.colours(self.artist_colours))
        };
        let current = of(self.current);
        let songs = match self.previous.filter(|_| self.fade < 1.0) {
            Some(prev) => of(Some(prev)).lerp(&current, self.fade),
            None => current,
        };
        match self.switch {
            Some((from, t)) if t < 1.0 => from.lerp(&songs, t),
            _ => songs,
        }
    }
}

/// Where the album colours come from, the cover or the artist image, and the cross-fade
/// between the two after a switch: toggling the artist background or changing to a layout
/// with the other setting.
#[derive(Clone, Copy, Debug)]
pub struct ColourSource {
    artist: bool,
    /// The colours shown when the source last switched, and when it did.
    switched: Option<(Scheme, Instant)>,
}

impl ColourSource {
    pub fn new(artist: bool) -> Self {
        Self {
            artist,
            switched: None,
        }
    }

    /// Whether the colours come from the artist image when it is available.
    pub fn artist(&self) -> bool {
        self.artist
    }

    /// Takes the colours from the artist image when `artist` is set, else from the cover. On a
    /// switch the colours cross-fade from `shown`, those shown until `now`, over [`ART_FADE`].
    pub fn set(&mut self, artist: bool, shown: Scheme, now: Instant) {
        if artist != self.artist {
            self.artist = artist;
            self.switched = Some((shown, now));
        }
    }

    /// The colours shown before the last switch and the progress of the cross-fade from them,
    /// in `0.0..1.0`; `None` once it is over.
    pub fn switch(&self, now: Instant) -> Option<(Scheme, f32)> {
        let (from, at) = self.switched?;
        let t = now.saturating_duration_since(at).as_secs_f32() / ART_FADE.as_secs_f32();
        (t < 1.0).then_some((from, t))
    }
}

/// Everything a layout needs to draw one frame. Animations read egui's input time, so a frame
/// depends only on this and the `RawInput` it is drawn with.
pub struct View<'a> {
    pub player: &'a PlayerState,
    pub art: ArtView<'a>,
    /// Cover thumbnail of a queued song, possibly not the current `player.next` yet.
    pub next_cover: Option<&'a Thumb>,
    pub show_clock: bool,
    pub lyrics_enabled: bool,
    /// Lyrics of the current song; `NotFound` when disabled or nothing is playing.
    pub lyrics: LyricsState,
    /// Wall-clock time shown by the clock.
    pub now: chrono::DateTime<chrono::Local>,
    /// Off-screen rendering for exports: no clock, no hover effects and no interaction, and
    /// grain over a blurred-art backdrop against banding in the encoded video.
    pub capture: bool,
    /// Animated backdrop: the art drifts and turns behind the Classic and Vibrant layouts.
    pub motion: bool,
    /// Use the artist image as the background when the song's artist has one.
    pub artist_bg: bool,
    /// Text, icons and bars take their colours from the album art; plain white when off.
    pub tint: bool,
    /// Previous and next buttons at the ends of the Encore Classic layout's progress bar.
    pub skip_buttons: bool,
    /// Alignment of the Encore Classic layout's lyrics.
    pub lyrics_align: LyricsAlign,
    /// How the Encore Classic layout's synced lyrics move.
    pub lyrics_flow: LyricsFlow,
}

impl View<'_> {
    /// Colours adapted to the art, or the neutral scheme with album colours off.
    pub fn scheme(&self) -> Scheme {
        if self.tint {
            self.art.scheme()
        } else {
            Scheme::plain()
        }
    }

    /// Text and control colours over a dark backdrop.
    pub fn ink_on_dark(&self) -> Ink {
        if self.tint {
            Ink::on_dark(&self.art.scheme())
        } else {
            Ink::WHITE
        }
    }
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
    use crate::art::ArtImages;
    use egui::ColorImage;

    fn art(ctx: &egui::Context, file: &str, cover: Scheme, artist: Option<Scheme>) -> ArtTextures {
        let image = || ColorImage::new([1, 1], vec![Color32::GRAY]);
        crate::art::upload(
            ctx,
            file,
            ArtImages {
                cover: image(),
                backdrop: image(),
                motion: image(),
                artist_bg: artist.map(|_| image()),
                artist_brightness: 0.5,
                artist_scheme: artist,
                dominant: Color32::GRAY,
                palette: vec![Color32::GRAY],
                brightness: 0.5,
                scheme: cover,
            },
        )
    }

    fn red() -> Scheme {
        Scheme::from_art(Color32::from_rgb(220, 40, 60))
    }

    fn blue() -> Scheme {
        Scheme::from_art(Color32::from_rgb(30, 60, 200))
    }

    fn green() -> Scheme {
        Scheme::from_art(Color32::from_rgb(40, 200, 120))
    }

    #[test]
    fn art_view_takes_colours_from_the_background_image() {
        let ctx = egui::Context::default();
        let with_artist = art(&ctx, "a", red(), Some(blue()));
        let without = art(&ctx, "b", green(), None);
        let view = |current, artist_colours| ArtView {
            current: Some(current),
            fade: 1.0,
            artist_colours,
            ..Default::default()
        };
        assert_eq!(view(&with_artist, true).scheme(), blue());
        assert_eq!(view(&with_artist, false).scheme(), red());
        assert_eq!(view(&without, true).scheme(), green());
    }

    #[test]
    fn art_view_cross_fades_between_songs_and_sources() {
        let ctx = egui::Context::default();
        let prev = art(&ctx, "a", red(), Some(blue()));
        let cur = art(&ctx, "b", green(), None);
        // A song change fades from the previous song's artist colours to the new cover's.
        let songs = ArtView {
            current: Some(&cur),
            previous: Some(&prev),
            fade: 0.5,
            artist_colours: true,
            switch: None,
        };
        assert_eq!(songs.scheme(), blue().lerp(&green(), 0.5));
        // A source switch fades from what was shown to the new source's colours.
        let switched = ArtView {
            current: Some(&prev),
            fade: 1.0,
            artist_colours: true,
            switch: Some((red(), 0.25)),
            ..Default::default()
        };
        assert_eq!(switched.scheme(), red().lerp(&blue(), 0.25));
        let done = ArtView {
            switch: Some((red(), 1.0)),
            ..switched
        };
        assert_eq!(done.scheme(), blue());
    }

    #[test]
    fn colour_source_cross_fades_after_a_switch() {
        let start = Instant::now();
        let mut source = ColourSource::new(false);
        assert!(!source.artist());
        assert!(source.switch(start).is_none());
        // Setting the same source starts no fade.
        source.set(false, red(), start);
        assert!(source.switch(start).is_none());

        source.set(true, red(), start);
        assert!(source.artist());
        let (from, t) = source.switch(start + ART_FADE / 2).expect("fading");
        assert_eq!(from, red());
        assert!((t - 0.5).abs() < 1e-3, "{t}");
        assert!(source.switch(start + ART_FADE).is_none());

        // Switching back takes over from the colours shown at that moment.
        source.set(false, blue(), start + ART_FADE * 2);
        assert!(!source.artist());
        let (from, t) = source.switch(start + ART_FADE * 2).expect("fading");
        assert_eq!(from, blue());
        assert_eq!(t, 0.0);
    }

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
}
