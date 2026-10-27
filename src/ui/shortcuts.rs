//! The keyboard shortcuts overlay: a card listing every key, drawn over whichever layout is
//! showing.

use std::sync::Arc;

use egui::{
    Align2, Color32, CursorIcon, FontId, Galley, Id, Key, Order, Painter, Pos2, Rect, Sense,
    Shadow, Stroke, StrokeKind, Vec2,
};

use super::font_light;
use super::panel_theme::PanelTheme;
use super::widgets::ease;
use super::with_alpha;

/// What triggers a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Key(Key),
    /// A key pressed with Shift held.
    Shift(Key),
    /// A click away from the controls.
    Click,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    Playback,
    Display,
    Lyrics,
    Export,
    App,
}

impl Group {
    const ALL: [Self; 5] = [
        Self::Playback,
        Self::Display,
        Self::Lyrics,
        Self::Export,
        Self::App,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Playback => "Playback",
            Self::Display => "Display",
            Self::Lyrics => "Lyrics",
            Self::Export => "Export",
            Self::App => "App",
        }
    }

    /// The card column the group sits in, chosen so the three columns come out about as tall.
    fn column(self) -> usize {
        match self {
            Self::Playback | Self::Lyrics => 0,
            Self::Display => 1,
            Self::Export | Self::App => 2,
        }
    }
}

pub struct Shortcut {
    pub inputs: &'static [Input],
    pub description: &'static str,
    pub group: Group,
}

const fn shortcut(inputs: &'static [Input], description: &'static str, group: Group) -> Shortcut {
    Shortcut {
        inputs,
        description,
        group,
    }
}

use Group::*;
use Input::{Click, Key as K, Shift};

/// Every shortcut, in the order the overlay lists them within each group.
pub const SHORTCUTS: &[Shortcut] = &[
    shortcut(&[K(Key::Space), Click], "Play / pause", Playback),
    shortcut(
        &[K(Key::ArrowLeft), K(Key::ArrowRight)],
        "Previous / next track",
        Playback,
    ),
    shortcut(&[K(Key::L)], "Cycle layouts", Display),
    shortcut(&[K(Key::F), K(Key::F11)], "Fullscreen", Display),
    shortcut(&[K(Key::T)], "Date and clock", Display),
    shortcut(&[K(Key::B)], "Moving backdrop", Display),
    shortcut(&[K(Key::A)], "Artist background", Display),
    shortcut(&[K(Key::K)], "Album colours", Display),
    shortcut(&[K(Key::S)], "Skip buttons (Spotify)", Display),
    shortcut(&[K(Key::C)], "Mouse cursor", Display),
    shortcut(&[K(Key::Y)], "Lyrics on / off", Lyrics),
    shortcut(&[Shift(Key::Y)], "Lyrics manager", Lyrics),
    shortcut(&[K(Key::J)], "Lyric alignment (Spotify)", Lyrics),
    shortcut(&[K(Key::O)], "Lyric flow (Spotify)", Lyrics),
    shortcut(&[K(Key::E)], "Clip export panel", Export),
    shortcut(&[K(Key::OpenBracket)], "Mark clip in-point", Export),
    shortcut(&[K(Key::CloseBracket)], "Mark clip out-point", Export),
    shortcut(
        &[K(Key::Questionmark), K(Key::H)],
        "Keyboard shortcuts",
        App,
    ),
    shortcut(&[K(Key::D)], "Debug window", App),
    shortcut(&[K(Key::Escape), K(Key::Q)], "Quit", App),
];

/// Text on the key caps of an input: one cap, or one per key of a combination.
fn caps(input: Input) -> Vec<&'static str> {
    match input {
        Shift(key) => vec!["Shift", label(K(key))],
        _ => vec![label(input)],
    }
}

/// Text on an input's key cap.
fn label(input: Input) -> &'static str {
    match input {
        Click => "Click",
        Shift(_) => "Shift",
        K(Key::Space) => "Space",
        K(Key::Escape) => "Esc",
        K(Key::ArrowLeft) => "←",
        K(Key::ArrowRight) => "→",
        K(Key::OpenBracket) => "[",
        K(Key::CloseBracket) => "]",
        K(Key::Questionmark) => "?",
        K(key) => key_name(key),
    }
}

/// Letters in lower case, other keys by egui's name for them.
fn key_name(key: Key) -> &'static str {
    const LETTERS: &str = "abcdefghijklmnopqrstuvwxyz";
    let name = key.name();
    match name.as_bytes() {
        [c @ b'A'..=b'Z'] => {
            let i = usize::from(c - b'A');
            &LETTERS[i..=i]
        }
        _ => name,
    }
}

const FADE: f32 = 0.15;
const COLUMNS: usize = 3;
/// Opacity of the scrim over the layout.
const BACKDROP: f32 = 200.0 / 255.0;

// Sizes for a 1000 px short side; the card scales with the window.
const PAD: f32 = 36.0;
const CORNER: f32 = 18.0;
/// Card top to the header pill.
const PILL_TOP: f32 = 18.0;
const PILL_TEXT: f32 = 16.0;
const PILL_HEIGHT: f32 = 40.0;
/// Gap between the pill's outer track and the tab inside it.
const PILL_INSET: f32 = 4.0;
const PILL_PAD: f32 = 14.0;
const ICON: f32 = 17.0;
const ICON_GAP: f32 = 8.0;
const CLOSE: f32 = 30.0;
/// Pill bottom to the first section heading.
const HEADER_GAP: f32 = 22.0;
const HEADING: f32 = 25.0;
const HEADING_ABOVE: f32 = 16.0;
const HEADING_BELOW: f32 = 4.0;
const TEXT: f32 = 15.0;
/// CircularStd sits high in its line box; this lowers descriptions onto the key caps' centre.
const TEXT_NUDGE: f32 = 1.5;
const ROW: f32 = 28.0;
const CAP_TEXT: f32 = 13.0;
const CAP: f32 = 21.0;
const CAP_PAD: f32 = 5.0;
const CAP_CORNER: f32 = 4.0;
/// Space either side of the "/" between alternative keys.
const SEP_GAP: f32 = 4.0;
const CAPS_TO_TEXT: f32 = 24.0;
const COLUMN_GAP: f32 = 60.0;
const HINT: f32 = 12.0;
const HINT_GAP: f32 = 14.0;
/// Smallest scale before fitting, so small windows still get legible text when there is room.
const MIN_SCALE: f32 = 0.75;
/// Share of the window the card may cover before it shrinks to fit.
const MAX_SHARE: f32 = 0.92;

/// Draws the overlay, fading in and out as `open` changes. While shown it takes every click so
/// none reach the layout beneath; returns true when a click outside the card or on its close
/// button should close it. Colours come from `theme`.
pub fn draw(ctx: &egui::Context, open: bool, theme: &PanelTheme) -> bool {
    let id = Id::unique("shortcuts-overlay");
    let t = ease(ctx.animate_bool_with_time(id, open, FADE));
    if t <= 0.0 {
        return false;
    }
    let screen = ctx.content_rect();
    egui::Area::new(id)
        .order(Order::Foreground)
        .fixed_pos(screen.min)
        .constrain(false)
        .interactable(open)
        .show(ctx, |ui| {
            let backdrop = ui.allocate_rect(screen, Sense::click());
            let mut painter = ui.painter_at(screen);
            painter.multiply_opacity(t);
            let card = Card::fit(&painter, screen, theme);
            let rect = Rect::from_center_size(screen.center(), card.size);
            // The card swallows its own clicks; only the backdrop and the close button close it.
            ui.interact(rect, id.with("card"), Sense::click());
            let close = ui
                .interact(card.close_rect(rect), id.with("close"), Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand);
            let hover = ctx.animate_bool_with_time(id.with("close-hover"), close.hovered(), 0.1);
            card.paint(&painter, theme, screen, rect, hover);
            backdrop.clicked() || close.clicked()
        })
        .inner
}

/// What sits in a row's key column: key caps with a dim `/` between alternatives and a dim `+`
/// between the keys of a combination.
enum Part {
    Cap(Arc<Galley>),
    Separator(Arc<Galley>),
}

impl Part {
    fn width(&self, s: f32) -> f32 {
        match self {
            Self::Cap(galley) => (galley.size().x + 2.0 * CAP_PAD * s).max(CAP * s),
            Self::Separator(galley) => galley.size().x + 2.0 * SEP_GAP * s,
        }
    }
}

enum Item {
    Heading(Arc<Galley>),
    Row { parts: Vec<Part>, text: Arc<Galley> },
}

struct Column {
    items: Vec<Item>,
    /// Width of the widest key column; descriptions line up after it.
    caps_width: f32,
    size: Vec2,
}

/// The laid-out card at scale `s`.
struct Card {
    s: f32,
    title: Arc<Galley>,
    hint: Arc<Galley>,
    columns: Vec<Column>,
    size: Vec2,
}

impl Card {
    /// Lays the card out for the window, shrinking it when it would not fit.
    fn fit(painter: &Painter, screen: Rect, theme: &PanelTheme) -> Self {
        let s = (screen.width().min(screen.height()) / 1000.0).max(MIN_SCALE);
        let card = Self::new(painter, theme, s);
        let max = screen.size() * MAX_SHARE;
        let fit = (max.x / card.size.x).min(max.y / card.size.y);
        if fit < 1.0 {
            Self::new(painter, theme, s * fit)
        } else {
            card
        }
    }

    fn new(painter: &Painter, theme: &PanelTheme, s: f32) -> Self {
        let title = painter.layout_no_wrap(
            "Keyboard shortcuts".into(),
            font_light(PILL_TEXT * s),
            theme.heading,
        );
        let hint = painter.layout_no_wrap(
            "Press ? or Esc to close".into(),
            font_light(HINT * s),
            theme.muted,
        );
        let columns: Vec<Column> = (0..COLUMNS).map(|i| column(painter, theme, s, i)).collect();
        let body = Vec2::new(
            columns.iter().map(|c| c.size.x).sum::<f32>() + COLUMN_GAP * s * (COLUMNS - 1) as f32,
            columns.iter().map(|c| c.size.y).fold(0.0, f32::max),
        );
        let pill_width = pill_width(&title, s);
        let width = (body.x + 2.0 * PAD * s).max(pill_width + 4.0 * (CLOSE + PAD) * s);
        let height = PILL_TOP * s
            + PILL_HEIGHT * s
            + HEADER_GAP * s
            + body.y
            + HINT_GAP * s
            + hint.size().y
            + PAD * s * 0.6;
        Self {
            s,
            title,
            hint,
            columns,
            size: Vec2::new(width, height),
        }
    }

    fn close_rect(&self, card: Rect) -> Rect {
        let s = self.s;
        let center = Pos2::new(
            card.right() - PILL_TOP * s - CLOSE * s / 2.0,
            card.top() + PILL_TOP * s + PILL_HEIGHT * s / 2.0,
        );
        Rect::from_center_size(center, Vec2::splat(CLOSE * s))
    }

    fn paint(
        &self,
        painter: &Painter,
        theme: &PanelTheme,
        screen: Rect,
        rect: Rect,
        close_hover: f32,
    ) {
        let s = self.s;
        painter.rect_filled(screen, 0.0, with_alpha(theme.scrim, BACKDROP));

        let corner = CORNER * s;
        let shadow = Shadow {
            offset: [0, (10.0 * s) as i8],
            blur: (40.0 * s).min(255.0) as u8,
            spread: 0,
            color: Color32::from_black_alpha(120),
        };
        painter.add(shadow.as_shape(rect, corner));
        painter.rect(
            rect,
            corner,
            theme.surface,
            Stroke::new(1.0, theme.border),
            StrokeKind::Inside,
        );

        // Header: a tab track with one selected tab, as in a tabbed cheatsheet.
        let pill = Rect::from_center_size(
            Pos2::new(
                rect.center().x,
                rect.top() + (PILL_TOP + PILL_HEIGHT / 2.0) * s,
            ),
            Vec2::new(pill_width(&self.title, s), PILL_HEIGHT * s),
        );
        painter.rect_filled(pill, pill.height() / 2.0, theme.layer(7));
        let tab = pill.shrink(PILL_INSET * s);
        painter.rect_filled(tab, tab.height() / 2.0, theme.wash(16));
        let icon = Rect::from_min_size(
            Pos2::new(tab.left() + PILL_PAD * s, tab.center().y - ICON * s / 2.0),
            Vec2::splat(ICON * s),
        );
        keyboard_icon(painter, icon, theme.text);
        painter.galley(
            Pos2::new(
                icon.right() + ICON_GAP * s,
                tab.center().y - self.title.size().y / 2.0,
            ),
            self.title.clone(),
            theme.heading,
        );

        let close = self.close_rect(rect);
        if close_hover > 0.0 {
            painter.circle_filled(
                close.center(),
                close.width() / 2.0,
                theme.layer((22.0 * close_hover) as u8),
            );
        }
        let arm = close.width() * 0.17;
        let stroke = Stroke::new(
            (1.6 * s).max(1.0),
            theme.body.lerp_to_gamma(theme.heading, close_hover),
        );
        let c = close.center();
        painter.line_segment([c + Vec2::splat(-arm), c + Vec2::splat(arm)], stroke);
        painter.line_segment([c + Vec2::new(-arm, arm), c + Vec2::new(arm, -arm)], stroke);

        let top = pill.bottom() + HEADER_GAP * s;
        let mut x = rect.left() + PAD * s;
        let body_width = self.columns.iter().map(|c| c.size.x).sum::<f32>()
            + COLUMN_GAP * s * (COLUMNS - 1) as f32;
        x += ((rect.width() - 2.0 * PAD * s) - body_width).max(0.0) / 2.0;
        for column in &self.columns {
            paint_column(painter, theme, column, Pos2::new(x, top), s);
            x += column.size.x + COLUMN_GAP * s;
        }

        painter.galley(
            Pos2::new(
                rect.right() - PAD * s - self.hint.size().x,
                rect.bottom() - PAD * s * 0.6 - self.hint.size().y,
            ),
            self.hint.clone(),
            theme.muted,
        );
    }
}

fn pill_width(title: &Galley, s: f32) -> f32 {
    2.0 * (PILL_INSET + PILL_PAD) * s + ICON * s + ICON_GAP * s + title.size().x
}

fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Monospace for key names; the arrows come from the proportional fallbacks, whose arrows are
/// larger than the monospace face's.
fn cap_font(input: Input, s: f32) -> FontId {
    match input {
        K(Key::ArrowLeft | Key::ArrowRight) => font_light(CAP_TEXT * s),
        _ => mono(CAP_TEXT * s),
    }
}

fn column(painter: &Painter, theme: &PanelTheme, s: f32, index: usize) -> Column {
    let mut items = Vec::new();
    for group in Group::ALL.into_iter().filter(|g| g.column() == index) {
        items.push(Item::Heading(painter.layout_no_wrap(
            group.title().into(),
            font_light(HEADING * s),
            theme.accent,
        )));
        for shortcut in SHORTCUTS.iter().filter(|sc| sc.group == group) {
            let mut parts = Vec::new();
            for (i, &input) in shortcut.inputs.iter().enumerate() {
                if i > 0 {
                    parts.push(Part::Separator(painter.layout_no_wrap(
                        "/".into(),
                        mono(CAP_TEXT * s),
                        theme.muted,
                    )));
                }
                for (j, cap) in caps(input).into_iter().enumerate() {
                    if j > 0 {
                        parts.push(Part::Separator(painter.layout_no_wrap(
                            "+".into(),
                            mono(CAP_TEXT * s),
                            theme.muted,
                        )));
                    }
                    parts.push(Part::Cap(painter.layout_no_wrap(
                        cap.into(),
                        cap_font(input, s),
                        theme.text,
                    )));
                }
            }
            let text = painter.layout_no_wrap(
                shortcut.description.into(),
                font_light(TEXT * s),
                theme.body,
            );
            items.push(Item::Row { parts, text });
        }
    }

    let (mut caps_width, mut text_width, mut height) = (0.0_f32, 0.0_f32, 0.0);
    for (i, item) in items.iter().enumerate() {
        match item {
            Item::Heading(galley) => {
                if i > 0 {
                    height += HEADING_ABOVE * s;
                }
                height += galley.size().y + HEADING_BELOW * s;
            }
            Item::Row { parts, text } => {
                caps_width = caps_width.max(parts.iter().map(|p| p.width(s)).sum());
                text_width = text_width.max(text.size().x);
                height += ROW * s;
            }
        }
    }
    Column {
        items,
        caps_width,
        size: Vec2::new(caps_width + CAPS_TO_TEXT * s + text_width, height),
    }
}

fn paint_column(painter: &Painter, theme: &PanelTheme, column: &Column, top_left: Pos2, s: f32) {
    let stroke = Stroke::new(s.max(1.0), theme.outline);
    let mut y = top_left.y;
    for (i, item) in column.items.iter().enumerate() {
        match item {
            Item::Heading(galley) => {
                if i > 0 {
                    y += HEADING_ABOVE * s;
                }
                painter.galley(Pos2::new(top_left.x, y), galley.clone(), theme.accent);
                y += galley.size().y + HEADING_BELOW * s;
            }
            Item::Row { parts, text } => {
                let mid = y + ROW * s / 2.0;
                let mut x = top_left.x;
                for part in parts {
                    let width = part.width(s);
                    let slot = Rect::from_min_size(
                        Pos2::new(x, mid - CAP * s / 2.0),
                        Vec2::new(width, CAP * s),
                    );
                    let galley = match part {
                        Part::Cap(galley) => {
                            painter.rect_stroke(slot, CAP_CORNER * s, stroke, StrokeKind::Inside);
                            galley
                        }
                        Part::Separator(galley) => galley,
                    };
                    painter.galley(
                        Align2::CENTER_CENTER
                            .anchor_size(slot.center(), galley.size())
                            .min,
                        galley.clone(),
                        theme.text,
                    );
                    x += width;
                }
                let text_x = top_left.x + column.caps_width + CAPS_TO_TEXT * s;
                painter.galley(
                    Pos2::new(text_x, mid - text.size().y / 2.0 + TEXT_NUDGE * s),
                    text.clone(),
                    theme.body,
                );
                y += ROW * s;
            }
        }
    }
}

/// An outlined keyboard: a rounded body with two rows of keys and a space bar, filling `rect`.
fn keyboard_icon(painter: &Painter, rect: Rect, color: Color32) {
    let u = rect.width() / 16.0;
    let body = Rect::from_center_size(rect.center(), Vec2::new(15.0 * u, 11.0 * u));
    painter.rect_stroke(
        body,
        2.0 * u,
        Stroke::new(1.4 * u, color),
        StrokeKind::Inside,
    );
    let key = Vec2::splat(1.6 * u);
    for (row, y) in [-2.6, 0.0].into_iter().enumerate() {
        let count = 4 - row;
        let step = 2.9 * u;
        let start = -step * (count - 1) as f32 / 2.0;
        for i in 0..count {
            let center = body.center() + Vec2::new(start + step * i as f32, y * u);
            painter.rect_filled(Rect::from_center_size(center, key), 0.3 * u, color);
        }
    }
    let bar = Rect::from_center_size(
        body.center() + Vec2::new(0.0, 2.7 * u),
        Vec2::new(6.4 * u, 1.5 * u),
    );
    painter.rect_filled(bar, 0.6 * u, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_key_is_listed_once() {
        let mut keys: Vec<(Key, bool)> = SHORTCUTS
            .iter()
            .flat_map(|sc| sc.inputs)
            .filter_map(|&input| match input {
                K(key) => Some((key, false)),
                Shift(key) => Some((key, true)),
                Click => None,
            })
            .collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), total);
    }

    #[test]
    fn every_group_has_shortcuts() {
        for group in Group::ALL {
            assert!(SHORTCUTS.iter().any(|sc| sc.group == group), "{group:?}");
        }
    }

    #[test]
    fn key_cap_labels() {
        assert_eq!(label(K(Key::L)), "l");
        assert_eq!(label(K(Key::F11)), "F11");
        assert_eq!(label(K(Key::Questionmark)), "?");
        assert_eq!(label(K(Key::Escape)), "Esc");
        assert_eq!(label(Click), "Click");
        assert_eq!(caps(Shift(Key::Y)), ["Shift", "y"]);
        assert_eq!(caps(K(Key::Y)), ["y"]);
    }
}
