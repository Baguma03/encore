//! A small Material 3 dark scheme adapted to the album art, after Quickshell's
//! `AdaptedMaterialScheme`: theme colours mixed with the cover's average colour, then nudged
//! until text and controls stay readable.

use egui::Color32;

use super::widgets::lerp;
use super::with_alpha;

/// Base dark theme the art colour is mixed into.
const BACKGROUND: Color32 = Color32::from_rgb(0x14, 0x13, 0x13);
const ON_BACKGROUND: Color32 = Color32::from_rgb(0xe6, 0xe1, 0xe1);
const LAYER1: Color32 = Color32::from_rgb(0x1c, 0x1b, 0x1c);
const ON_SURFACE_VARIANT: Color32 = Color32::from_rgb(0xcb, 0xc5, 0xca);
const PRIMARY: Color32 = Color32::from_rgb(0xcb, 0xc4, 0xcb);
const ON_PRIMARY: Color32 = Color32::from_rgb(0x32, 0x2f, 0x34);
const PRIMARY_CONTAINER: Color32 = Color32::from_rgb(0x2d, 0x2a, 0x2f);
const SECONDARY_CONTAINER: Color32 = Color32::from_rgb(0x4d, 0x4b, 0x4d);
const ON_SECONDARY_CONTAINER: Color32 = Color32::from_rgb(0xec, 0xe6, 0xe9);

/// Darkest backdrop the layouts draw text on, after their washes.
const BACKDROP_REFERENCE: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);

/// WCAG contrast ratios each role keeps against what it is drawn on.
const TEXT_CONTRAST: f32 = 4.5;
const BACKDROP_TEXT_CONTRAST: f32 = 9.0;
const BACKDROP_SUBTEXT_CONTRAST: f32 = 5.0;
const CONTROL_CONTRAST: f32 = 3.0;
const TRACK_CONTRAST: f32 = 1.6;
/// Contrast of white text on `layer0`, which keeps the card dark.
const LAYER0_CONTRAST: f32 = 7.0;

/// HSL lightness of the main text tint for grey covers and for colourful ones; colourful
/// covers get a darker, more saturated pastel.
const TINT_LIGHTNESS: [f32; 2] = [0.95, 0.86];
/// Saturation of the main text tint relative to the art colour's, and its ceiling.
const TINT_SATURATION: f32 = 1.2;
const TINT_MAX_SATURATION: f32 = 0.8;
/// Share of white's contrast the text over the light Vibrant backdrop keeps.
const LIGHT_TEXT_KEEP: f32 = 0.8;
const LIGHT_SOFT_KEEP: f32 = 0.7;
/// Opacity of the unfilled part of tinted progress bars.
const TRACK_ALPHA: f32 = 0.2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scheme {
    pub primary: Color32,
    pub on_primary: Color32,
    pub secondary_container: Color32,
    pub on_secondary_container: Color32,
    /// Card background.
    pub layer0: Color32,
    pub layer1: Color32,
    /// Text on `layer0`.
    pub on_layer0: Color32,
    /// Secondary text on `layer0` and over the backdrops.
    pub subtext: Color32,
    /// Main text over the full-screen backdrops: a light pastel of the art's hue.
    pub text: Color32,
}

impl Default for Scheme {
    /// The scheme without art, built from the theme's own secondary container colour.
    fn default() -> Self {
        Self::from_color(SECONDARY_CONTAINER)
    }
}

impl Scheme {
    /// The scheme for a cover whose average colour is `average`, mixed with the theme like the
    /// widget's `artDominantColor`.
    pub fn from_art(average: Color32) -> Self {
        Self::from_color(mix(average, PRIMARY_CONTAINER, 0.8))
    }

    /// The scheme adapted to `color`.
    pub fn from_color(color: Color32) -> Self {
        let dark = to_hsl(color)[2] < 0.5;
        let layer0 = mix(BACKGROUND, color, if dark { 0.6 } else { 0.5 });
        let layer0 = ensure_contrast(layer0, Color32::WHITE, LAYER0_CONTRAST);
        let on_layer0 = ensure_contrast(mix(ON_BACKGROUND, color, 0.5), layer0, TEXT_CONTRAST);
        let primary = ensure_contrast(
            mix(adapt_to_accent(PRIMARY, color), color, 0.5),
            layer0,
            CONTROL_CONTRAST,
        );
        let on_primary = ensure_contrast(
            mix(adapt_to_accent(ON_PRIMARY, color), color, 0.5),
            primary,
            CONTROL_CONTRAST,
        );
        let subtext = mix(ON_SURFACE_VARIANT, color, 0.5);
        let subtext = ensure_contrast(
            ensure_contrast(subtext, layer0, TEXT_CONTRAST),
            BACKDROP_REFERENCE,
            BACKDROP_SUBTEXT_CONTRAST,
        );
        let text = ensure_contrast(pastel(color), BACKDROP_REFERENCE, BACKDROP_TEXT_CONTRAST);
        Self {
            primary,
            on_primary,
            secondary_container: ensure_contrast(
                mix(SECONDARY_CONTAINER, color, 0.15),
                layer0,
                TRACK_CONTRAST,
            ),
            on_secondary_container: ensure_contrast(
                mix(ON_SECONDARY_CONTAINER, color, 0.5),
                layer0,
                TEXT_CONTRAST,
            ),
            layer0,
            layer1: mix(LAYER1, color, 0.5),
            on_layer0,
            subtext,
            text,
        }
    }

    /// The neutral grey scheme with white text, for album colours turned off.
    pub fn plain() -> Self {
        Self {
            text: Color32::WHITE,
            ..Self::from_color(Color32::from_gray(0x4c))
        }
    }

    /// Blend from `self` to `other` by `t`, for cross-fading between songs.
    pub fn lerp(&self, other: &Self, t: f32) -> Self {
        let f = |a: Color32, b: Color32| mix(b, a, t.clamp(0.0, 1.0));
        Self {
            primary: f(self.primary, other.primary),
            on_primary: f(self.on_primary, other.on_primary),
            secondary_container: f(self.secondary_container, other.secondary_container),
            on_secondary_container: f(self.on_secondary_container, other.on_secondary_container),
            layer0: f(self.layer0, other.layer0),
            layer1: f(self.layer1, other.layer1),
            on_layer0: f(self.on_layer0, other.on_layer0),
            subtext: f(self.subtext, other.subtext),
            text: f(self.text, other.text),
        }
    }
}

/// A light pastel of `color`'s hue: near white for grey colours, a clearly visible tint for
/// colourful ones.
fn pastel(color: Color32) -> Color32 {
    let [h, s, _] = to_hsl(color);
    let s = (s * TINT_SATURATION).min(TINT_MAX_SATURATION);
    let l = lerp(TINT_LIGHTNESS[0], TINT_LIGHTNESS[1], (s / 0.6).min(1.0));
    from_hsl([h, s, l])
}

/// Colours of text, icons and progress bars in the Classic and Vibrant layouts: plain white, or
/// light tints of the album colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ink {
    /// Title, active lyric line, icons and the filled part of progress bars.
    pub text: Color32,
    /// Secondary text and inactive lyric lines, which the layouts draw at reduced opacity.
    pub soft: Color32,
    /// Unfilled part of progress bars, translucent.
    pub track: Color32,
}

impl Ink {
    pub const WHITE: Self = Self {
        text: Color32::WHITE,
        soft: Color32::WHITE,
        track: Color32::from_rgba_premultiplied(46, 46, 46, 46),
    };

    /// Tints of `scheme` for a dark backdrop.
    pub fn on_dark(scheme: &Scheme) -> Self {
        Self {
            text: scheme.text,
            soft: mix(scheme.text, scheme.subtext, 0.6),
            track: with_alpha(scheme.text, TRACK_ALPHA),
        }
    }

    /// Lighter tints of `scheme` for a light, colourful backdrop of mean relative luminance
    /// `backdrop`, keeping most of the contrast white text would have.
    pub fn on_light(scheme: &Scheme, backdrop: f32) -> Self {
        let text = keep_white_contrast(scheme.text, backdrop, LIGHT_TEXT_KEEP);
        Self {
            text,
            soft: keep_white_contrast(scheme.text, backdrop, LIGHT_SOFT_KEEP),
            track: with_alpha(text, TRACK_ALPHA),
        }
    }
}

/// `fg` moved towards white until its contrast against a backdrop of relative luminance
/// `backdrop` is at least `share` of white's.
pub fn keep_white_contrast(fg: Color32, backdrop: f32, share: f32) -> Color32 {
    let ratio = |c: Color32| (luminance(c) + 0.05) / (backdrop + 0.05);
    let min = share * ratio(Color32::WHITE);
    (0..=20)
        .map(|step| mix(Color32::WHITE, fg, step as f32 / 20.0))
        .find(|c| ratio(*c) >= min)
        .unwrap_or(Color32::WHITE)
}

/// `weight` of `a` plus the rest of `b`, channel by channel, like `ColorUtils.mix`.
pub fn mix(a: Color32, b: Color32, weight: f32) -> Color32 {
    let f = |x: u8, y: u8| lerp(y as f32, x as f32, weight).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

/// Hue (degrees), saturation and lightness of an opaque colour.
pub fn to_hsl(c: Color32) -> [f32; 3] {
    let [r, g, b] = [c.r(), c.g(), c.b()].map(|v| v as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d <= 0.0 {
        return [0.0, 0.0, l];
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    [h, s.clamp(0.0, 1.0), l]
}

pub fn from_hsl([h, s, l]: [f32; 3]) -> Color32 {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match (h.rem_euclid(360.0) / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let ch = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgb(ch(r), ch(g), ch(b))
}

/// `base` with the hue and saturation of `accent`, like `ColorUtils.adaptToAccent`.
fn adapt_to_accent(base: Color32, accent: Color32) -> Color32 {
    let [h, s, _] = to_hsl(accent);
    from_hsl([h, s, to_hsl(base)[2]])
}

/// WCAG relative luminance of an opaque sRGB colour.
pub fn luminance(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
}

/// WCAG contrast ratio between two opaque colours, from 1 to 21.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `fg` moved towards white or black, whichever stands out more from `bg`, just far enough to
/// reach a contrast of `min`.
pub fn ensure_contrast(fg: Color32, bg: Color32, min: f32) -> Color32 {
    if contrast(fg, bg) >= min {
        return fg;
    }
    let target = if contrast(Color32::WHITE, bg) >= contrast(Color32::BLACK, bg) {
        Color32::WHITE
    } else {
        Color32::BLACK
    };
    (1..=20)
        .map(|step| mix(target, fg, step as f32 / 20.0))
        .find(|c| contrast(*c, bg) >= min)
        .unwrap_or(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COVERS: [Color32; 8] = [
        Color32::from_rgb(0, 0, 0),
        Color32::from_rgb(255, 255, 255),
        Color32::from_rgb(220, 40, 60),
        Color32::from_rgb(230, 120, 150),
        Color32::from_rgb(30, 60, 200),
        Color32::from_rgb(250, 220, 40),
        Color32::from_rgb(120, 110, 100),
        Color32::from_rgb(40, 200, 120),
    ];

    #[test]
    fn hsl_round_trip() {
        for c in COVERS {
            let back = from_hsl(to_hsl(c));
            let close = |a: u8, b: u8| a.abs_diff(b) <= 1;
            assert!(
                close(back.r(), c.r()) && close(back.g(), c.g()) && close(back.b(), c.b()),
                "{c:?} -> {back:?}"
            );
        }
    }

    #[test]
    fn contrast_is_raised_on_dark_and_light_backgrounds() {
        let grey = Color32::from_gray(128);
        let dark = Color32::from_gray(20);
        let light = Color32::from_gray(235);
        let on_dark = ensure_contrast(grey, dark, 7.0);
        assert!(contrast(on_dark, dark) >= 7.0);
        assert!(luminance(on_dark) > luminance(grey));
        let on_light = ensure_contrast(grey, light, 7.0);
        assert!(contrast(on_light, light) >= 7.0);
        assert!(luminance(on_light) < luminance(grey));
        // Already readable colours are kept.
        assert_eq!(ensure_contrast(Color32::WHITE, dark, 4.5), Color32::WHITE);
        assert!((contrast(Color32::WHITE, Color32::BLACK) - 21.0).abs() < 0.01);
    }

    #[test]
    fn schemes_stay_readable_for_any_cover() {
        for cover in COVERS {
            let s = Scheme::from_art(cover);
            assert!(
                contrast(s.on_layer0, s.layer0) >= TEXT_CONTRAST,
                "{cover:?}"
            );
            assert!(contrast(s.subtext, s.layer0) >= TEXT_CONTRAST, "{cover:?}");
            assert!(
                contrast(Color32::WHITE, s.layer0) >= LAYER0_CONTRAST,
                "{cover:?}"
            );
            assert!(
                contrast(s.on_primary, s.primary) >= CONTROL_CONTRAST,
                "{cover:?}"
            );
            assert!(
                contrast(s.primary, s.layer0) >= CONTROL_CONTRAST,
                "{cover:?}"
            );
            let backdrop = BACKDROP_REFERENCE;
            assert!(
                contrast(s.text, backdrop) >= BACKDROP_TEXT_CONTRAST,
                "{cover:?}"
            );
            assert!(
                contrast(s.subtext, backdrop) >= BACKDROP_SUBTEXT_CONTRAST,
                "{cover:?}"
            );
            // Text over the backdrops is near white.
            assert!(luminance(s.text) > 0.6, "{cover:?} {:?}", s.text);
        }
    }

    #[test]
    fn text_takes_a_hint_of_the_art_hue() {
        let red = Scheme::from_art(Color32::from_rgb(220, 40, 60)).text;
        let blue = Scheme::from_art(Color32::from_rgb(30, 60, 200)).text;
        assert!(red.r() > red.b(), "{red:?}");
        assert!(blue.b() > blue.r(), "{blue:?}");
    }

    #[test]
    fn text_is_a_visible_pastel() {
        let pink = Scheme::from_art(Color32::from_rgb(200, 60, 110)).text;
        let [_, s, l] = to_hsl(pink);
        assert!(s > 0.3 && l > 0.8, "{pink:?}");
        assert!(pink.r() > pink.g() + 25, "{pink:?}");
        let grey = Scheme::from_art(Color32::from_gray(120)).text;
        assert!(luminance(grey) > 0.85, "{grey:?}");
        assert_eq!(Scheme::plain().text, Color32::WHITE);
    }

    #[test]
    fn light_backdrop_ink_keeps_contrast() {
        for cover in COVERS {
            let scheme = Scheme::from_art(cover);
            for backdrop in [0.05, 0.2, 0.4, 0.7] {
                let ink = Ink::on_light(&scheme, backdrop);
                let ratio = |c: Color32| (luminance(c) + 0.05) / (backdrop + 0.05);
                let white = ratio(Color32::WHITE);
                assert!(
                    ratio(ink.text) >= LIGHT_TEXT_KEEP * white - 1e-4,
                    "{cover:?}"
                );
                assert!(
                    ratio(ink.soft) >= LIGHT_SOFT_KEEP * white - 1e-4,
                    "{cover:?}"
                );
                assert!(luminance(ink.text) >= luminance(ink.soft), "{cover:?}");
            }
            let dark = Ink::on_dark(&scheme);
            assert!(contrast(dark.text, BACKDROP_REFERENCE) >= BACKDROP_TEXT_CONTRAST);
        }
        assert_eq!(
            keep_white_contrast(Color32::BLACK, 1.0, 1.0),
            Color32::WHITE
        );
    }

    #[test]
    fn lerp_endpoints() {
        let a = Scheme::from_art(Color32::RED);
        let b = Scheme::from_art(Color32::BLUE);
        assert_eq!(a.lerp(&b, 0.0), a);
        assert_eq!(a.lerp(&b, 1.0), b);
    }
}
