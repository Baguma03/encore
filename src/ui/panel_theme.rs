//! Colours of the panels drawn over the layouts (the shortcuts overlay, the clip export panel):
//! a dark card subtly tinted towards the cover's hue, with the layout's pastel for headings and
//! the scheme's primary colour as the accent. With album colours off it is the neutral grey
//! look.

use egui::{Color32, Context, Stroke, Visuals};

use super::scheme::{ensure_contrast, from_hsl, mix, to_hsl, Scheme};
use super::with_alpha;

/// Opacity of the card's fill over the dimmed layout.
const SURFACE_ALPHA: u8 = 242;
/// HSL lightness of the tinted card, as dark as the neutral one.
const SURFACE_LIGHTNESS: f32 = 0.115;
/// Share of the primary colour's saturation the card keeps, and its ceiling: enough to
/// recognise the hue, never a coloured slab.
const SURFACE_SATURATION: f32 = 0.45;
const SURFACE_MAX_SATURATION: f32 = 0.3;

/// Weight of the heading colour in each text role, the rest being the card.
const TEXT_WEIGHT: f32 = 0.9;
const BODY_WEIGHT: f32 = 0.79;
const OUTLINE_WEIGHT: f32 = 0.74;
const MUTED_WEIGHT: f32 = 0.47;

/// WCAG contrast ratios each role keeps against the card.
const HEADING_CONTRAST: f32 = 9.0;
const TEXT_CONTRAST: f32 = 7.0;
const BODY_CONTRAST: f32 = 4.5;
/// Hints and separators: about the neutral grey's contrast.
const MUTED_CONTRAST: f32 = 3.5;
const OUTLINE_CONTRAST: f32 = 3.0;
/// The accent is a fill under text and a colour of its own, so it stays clearly visible.
const ACCENT_CONTRAST: f32 = 6.0;
const ON_ACCENT_CONTRAST: f32 = 4.5;

const AMBER: Color32 = Color32::from_rgb(240, 190, 100);

/// Colour roles shared by every panel. Opaque unless noted; translucent layers come from
/// [`PanelTheme::layer`] and [`PanelTheme::wash`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelTheme {
    /// Dims the layout behind a panel; each panel applies its own opacity.
    pub scrim: Color32,
    /// Card fill, slightly translucent.
    pub surface: Color32,
    /// Card outline, translucent.
    pub border: Color32,
    /// Light colour behind [`PanelTheme::layer`]: raised fills, hover states and tracks.
    pub glow: Color32,
    /// Titles, headings and values: the layout's pastel.
    pub heading: Color32,
    /// Key caps, icons and other text that is not a heading.
    pub text: Color32,
    /// Descriptions and labels.
    pub body: Color32,
    /// Hints, separators and other quiet text.
    pub muted: Color32,
    /// Key cap and input borders.
    pub outline: Color32,
    /// Primary buttons, selected chips, switches that are on, slider fills and handles.
    pub accent: Color32,
    /// Text and icons on `accent`.
    pub on_accent: Color32,
    /// Warnings, such as a range that cannot be exported.
    pub warning: Color32,
}

impl PanelTheme {
    /// The neutral grey look, for album colours turned off.
    pub const PLAIN: Self = Self {
        scrim: Color32::BLACK,
        surface: Color32::from_rgba_premultiplied(27, 27, 29, SURFACE_ALPHA),
        border: Color32::from_rgba_premultiplied(16, 16, 16, 16),
        glow: Color32::WHITE,
        heading: Color32::WHITE,
        text: Color32::from_gray(228),
        body: Color32::from_gray(200),
        muted: Color32::from_gray(120),
        outline: Color32::from_gray(190),
        accent: Color32::WHITE,
        on_accent: Color32::from_rgb(20, 20, 24),
        warning: AMBER,
    };

    /// The theme for the current, already cross-faded `scheme`, or the plain look when album
    /// colours are off. Called every frame, so panels follow the song-change cross-fade.
    pub fn new(scheme: &Scheme, tint: bool) -> Self {
        if tint {
            Self::from_scheme(scheme)
        } else {
            Self::PLAIN
        }
    }

    /// The tinted theme for `scheme`.
    pub fn from_scheme(scheme: &Scheme) -> Self {
        let [h, s, _] = to_hsl(scheme.primary);
        let card = from_hsl([
            h,
            (s * SURFACE_SATURATION).min(SURFACE_MAX_SATURATION),
            SURFACE_LIGHTNESS,
        ]);
        let heading = ensure_contrast(scheme.text, card, HEADING_CONTRAST);
        let role = |weight: f32, min: f32| ensure_contrast(mix(heading, card, weight), card, min);
        let accent = ensure_contrast(scheme.primary, card, ACCENT_CONTRAST);
        Self {
            scrim: mix(card, Color32::BLACK, 0.3),
            surface: with_alpha(card, f32::from(SURFACE_ALPHA) / 255.0),
            border: with_alpha(heading, 16.0 / 255.0),
            glow: heading,
            heading,
            text: role(TEXT_WEIGHT, TEXT_CONTRAST),
            body: role(BODY_WEIGHT, BODY_CONTRAST),
            muted: role(MUTED_WEIGHT, MUTED_CONTRAST),
            outline: role(OUTLINE_WEIGHT, OUTLINE_CONTRAST),
            accent,
            on_accent: ensure_contrast(scheme.on_primary, accent, ON_ACCENT_CONTRAST),
            warning: ensure_contrast(AMBER, card, BODY_CONTRAST),
        }
    }

    /// The card's fill without its translucency, which the foreground roles are measured on.
    pub fn card(&self) -> Color32 {
        self.surface.to_opaque()
    }

    /// A translucent layer of `glow` at `alpha` of 255: white at that alpha in the plain look.
    pub fn layer(&self, alpha: u8) -> Color32 {
        with_alpha(self.glow, f32::from(alpha) / 255.0)
    }

    /// A translucent layer of `accent` at `alpha` of 255, for secondary buttons and selections.
    pub fn wash(&self, alpha: u8) -> Color32 {
        with_alpha(self.accent, f32::from(alpha) / 255.0)
    }

    /// Sets egui's widget colours, text selection and window colours from the theme, for
    /// panels built from egui widgets.
    pub fn apply(&self, visuals: &mut Visuals) {
        let card = self.card();
        visuals.override_text_color = None;
        visuals.window_fill = card;
        visuals.panel_fill = card;
        visuals.window_stroke = Stroke::new(1.0, self.border);
        visuals.faint_bg_color = self.layer(7);
        visuals.extreme_bg_color = mix(card, Color32::BLACK, 0.6);
        visuals.text_edit_bg_color = Some(visuals.extreme_bg_color);
        visuals.code_bg_color = self.layer(16);
        visuals.hyperlink_color = self.accent;
        visuals.warn_fg_color = self.warning;
        visuals.error_fg_color = self.warning;
        visuals.selection.bg_fill = self.wash(90);
        visuals.selection.stroke = Stroke::new(1.0, self.heading);
        visuals.text_cursor.stroke = Stroke::new(2.0, self.accent);

        let w = &mut visuals.widgets;
        w.noninteractive.bg_fill = card;
        w.noninteractive.weak_bg_fill = card;
        w.noninteractive.bg_stroke = Stroke::new(1.0, self.border);
        w.noninteractive.fg_stroke = Stroke::new(1.0, self.body);
        w.inactive.bg_fill = self.layer(28);
        w.inactive.weak_bg_fill = self.layer(16);
        w.inactive.bg_stroke = Stroke::new(1.0, self.layer(30));
        w.inactive.fg_stroke = Stroke::new(1.0, self.text);
        w.hovered.bg_fill = self.wash(48);
        w.hovered.weak_bg_fill = self.wash(30);
        w.hovered.bg_stroke = Stroke::new(1.0, self.outline);
        w.hovered.fg_stroke = Stroke::new(1.5, self.heading);
        w.active.bg_fill = self.wash(70);
        w.active.weak_bg_fill = self.wash(50);
        w.active.bg_stroke = Stroke::new(1.0, self.accent);
        w.active.fg_stroke = Stroke::new(2.0, self.heading);
        w.open = w.hovered;
    }

    /// Runs `add` with the global style themed, then restores it: popups and tooltips take the
    /// global style, so this themes them along with everything `add` draws, while the rest of
    /// the frame keeps its own look.
    pub fn scope<R>(&self, ctx: &Context, add: impl FnOnce() -> R) -> R {
        let saved = ctx.global_style();
        ctx.global_style_mut(|style| self.apply(&mut style.visuals));
        let result = add();
        ctx.set_global_style(saved);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::super::scheme::contrast;
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
    fn plain_without_album_colours() {
        let scheme = Scheme::from_art(Color32::from_rgb(220, 40, 60));
        assert_eq!(PanelTheme::new(&scheme, false), PanelTheme::PLAIN);
        let plain = PanelTheme::PLAIN;
        assert_eq!(plain.layer(16), Color32::from_white_alpha(16));
        assert_eq!(plain.wash(26), Color32::from_white_alpha(26));
    }

    #[test]
    fn roles_stay_readable_for_any_cover() {
        for cover in COVERS {
            let t = PanelTheme::new(&Scheme::from_art(cover), true);
            let card = t.card();
            assert!(contrast(Color32::WHITE, card) >= 12.0, "{cover:?} {card:?}");
            for (role, min) in [
                (t.heading, HEADING_CONTRAST),
                (t.text, TEXT_CONTRAST),
                (t.body, BODY_CONTRAST),
                (t.muted, MUTED_CONTRAST),
                (t.outline, OUTLINE_CONTRAST),
                (t.accent, ACCENT_CONTRAST),
                (t.warning, BODY_CONTRAST),
            ] {
                assert!(contrast(role, card) >= min, "{cover:?} {role:?}");
            }
            assert!(
                contrast(t.on_accent, t.accent) >= ON_ACCENT_CONTRAST,
                "{cover:?}"
            );
        }
    }

    #[test]
    fn card_takes_a_hint_of_the_cover_hue() {
        let red = PanelTheme::new(&Scheme::from_art(Color32::from_rgb(220, 40, 60)), true);
        let blue = PanelTheme::new(&Scheme::from_art(Color32::from_rgb(30, 60, 200)), true);
        assert!(red.card().r() > red.card().b(), "{:?}", red.card());
        assert!(blue.card().b() > blue.card().r(), "{:?}", blue.card());
        assert!(red.accent.r() > red.accent.b(), "{:?}", red.accent);
    }

    #[test]
    fn follows_the_cross_fade() {
        let a = Scheme::from_art(Color32::RED);
        let b = Scheme::from_art(Color32::BLUE);
        assert_eq!(
            PanelTheme::new(&a.lerp(&b, 0.0), true),
            PanelTheme::new(&a, true)
        );
        assert_eq!(
            PanelTheme::new(&a.lerp(&b, 1.0), true),
            PanelTheme::new(&b, true)
        );
    }

    #[test]
    fn scope_restores_the_global_style() {
        let ctx = Context::default();
        let before = ctx.global_style();
        let theme = PanelTheme::new(&Scheme::from_art(Color32::RED), true);
        let inside = theme.scope(&ctx, || ctx.global_style().visuals.hyperlink_color);
        assert_eq!(inside, theme.accent);
        assert_eq!(*ctx.global_style(), *before);
    }
}
