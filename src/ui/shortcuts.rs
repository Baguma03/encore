//! The keyboard shortcuts overlay: a card listing every key, drawn over whichever layout is
//! showing.

use std::sync::Arc;

use egui::{
    Align2, Color32, Galley, Id, Key, Order, Painter, Pos2, Rect, Sense, Shadow, Stroke,
    StrokeKind, Vec2,
};

use super::widgets::ease;
use super::{font_bold, font_light, with_alpha};

/// What triggers a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Key(Key),
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
            Self::Playback => "PLAYBACK",
            Self::Display => "DISPLAY",
            Self::Lyrics => "LYRICS",
            Self::Export => "EXPORT",
            Self::App => "APP",
        }
    }

    /// The card column the group sits in.
    fn column(self) -> usize {
        match self {
            Self::Playback | Self::Display => 0,
            Self::Lyrics | Self::Export | Self::App => 1,
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
use Input::{Click, Key as K};

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

/// Text on an input's key cap.
fn label(input: Input) -> &'static str {
    match input {
        Click => "Click",
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
const CARD_FILL: Color32 = Color32::from_rgba_premultiplied(18, 18, 22, 235);

// Sizes for a 1000 px short side; the card scales with the window.
const PAD: f32 = 44.0;
const CORNER: f32 = 24.0;
const TITLE: f32 = 34.0;
const SUBTITLE: f32 = 17.0;
const TITLE_GAP: f32 = 8.0;
const BODY_GAP: f32 = 22.0;
const HEADING: f32 = 14.0;
const HEADING_ABOVE: f32 = 20.0;
const HEADING_BELOW: f32 = 6.0;
const TEXT: f32 = 20.0;
const ROW: f32 = 40.0;
const CAP_TEXT: f32 = 16.0;
const CAP: f32 = 30.0;
const CAP_PAD: f32 = 10.0;
const CAP_GAP: f32 = 6.0;
const CAPS_TO_TEXT: f32 = 16.0;
const COLUMN_GAP: f32 = 56.0;
/// Share of the window the card may cover before it shrinks to fit.
const MAX_SHARE: f32 = 0.92;

/// Draws the overlay, fading in and out as `open` changes. While shown it takes every click so
/// none reach the layout beneath; returns true when it was clicked.
pub fn draw(ctx: &egui::Context, open: bool) -> bool {
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
            let response = ui.allocate_rect(screen, Sense::click());
            let mut painter = ui.painter_at(screen);
            painter.multiply_opacity(t);
            paint(&painter, screen);
            response.clicked()
        })
        .inner
}

enum Item {
    Heading(Arc<Galley>),
    Row {
        caps: Vec<Arc<Galley>>,
        text: Arc<Galley>,
    },
}

struct Column {
    items: Vec<Item>,
    /// Width of the widest set of key caps; descriptions line up after it.
    caps_width: f32,
    size: Vec2,
}

/// The laid-out card at scale `s`.
struct Card {
    s: f32,
    title: Arc<Galley>,
    subtitle: Arc<Galley>,
    columns: [Column; 2],
    size: Vec2,
}

impl Card {
    fn new(painter: &Painter, s: f32) -> Self {
        let title = painter.layout_no_wrap(
            "Keyboard shortcuts".into(),
            font_bold(TITLE * s),
            Color32::WHITE,
        );
        let subtitle = painter.layout_no_wrap(
            "Press ? or Esc to close".into(),
            font_light(SUBTITLE * s),
            with_alpha(Color32::WHITE, 0.5),
        );
        let columns = [0, 1].map(|i| column(painter, s, i));
        let body = Vec2::new(
            columns[0].size.x + COLUMN_GAP * s + columns[1].size.x,
            columns[0].size.y.max(columns[1].size.y),
        );
        let header = title.size().y + TITLE_GAP * s + subtitle.size().y + BODY_GAP * s;
        let size = Vec2::new(
            body.x.max(title.size().x).max(subtitle.size().x),
            header + body.y,
        ) + Vec2::splat(2.0 * PAD * s);
        Self {
            s,
            title,
            subtitle,
            columns,
            size,
        }
    }
}

fn column(painter: &Painter, s: f32, index: usize) -> Column {
    let mut items = Vec::new();
    for group in Group::ALL.into_iter().filter(|g| g.column() == index) {
        items.push(Item::Heading(painter.layout_no_wrap(
            group.title().into(),
            font_bold(HEADING * s),
            with_alpha(Color32::WHITE, 0.45),
        )));
        for shortcut in SHORTCUTS.iter().filter(|sc| sc.group == group) {
            let caps = shortcut
                .inputs
                .iter()
                .map(|&input| {
                    painter.layout_no_wrap(
                        label(input).into(),
                        font_bold(CAP_TEXT * s),
                        Color32::WHITE,
                    )
                })
                .collect();
            let text = painter.layout_no_wrap(
                shortcut.description.into(),
                font_light(TEXT * s),
                with_alpha(Color32::WHITE, 0.88),
            );
            items.push(Item::Row { caps, text });
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
            Item::Row { caps, text } => {
                caps_width = caps_width.max(caps_span(caps, s));
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

fn cap_width(galley: &Galley, s: f32) -> f32 {
    (galley.size().x + 2.0 * CAP_PAD * s).max(CAP * s)
}

fn caps_span(caps: &[Arc<Galley>], s: f32) -> f32 {
    let widths: f32 = caps.iter().map(|g| cap_width(g, s)).sum();
    widths + CAP_GAP * s * caps.len().saturating_sub(1) as f32
}

fn paint(painter: &Painter, screen: Rect) {
    painter.rect_filled(screen, 0.0, Color32::from_black_alpha(110));

    let s = screen.width().min(screen.height()) / 1000.0;
    let mut card = Card::new(painter, s);
    let max = screen.size() * MAX_SHARE;
    let fit = (max.x / card.size.x).min(max.y / card.size.y);
    if fit < 1.0 {
        card = Card::new(painter, s * fit);
    }
    let s = card.s;

    let rect = Rect::from_center_size(screen.center(), card.size);
    let corner = CORNER * s;
    let shadow = Shadow {
        offset: [0, (12.0 * s) as i8],
        blur: (48.0 * s).min(255.0) as u8,
        spread: 0,
        color: Color32::from_black_alpha(140),
    };
    painter.add(shadow.as_shape(rect, corner));
    painter.rect(
        rect,
        corner,
        CARD_FILL,
        Stroke::new(1.0, Color32::from_white_alpha(20)),
        StrokeKind::Inside,
    );

    let inner = rect.shrink(PAD * s);
    let mut y = inner.top();
    painter.galley(
        Pos2::new(inner.left(), y),
        card.title.clone(),
        Color32::WHITE,
    );
    y += card.title.size().y + TITLE_GAP * s;
    painter.galley(
        Pos2::new(inner.left(), y),
        card.subtitle.clone(),
        Color32::WHITE,
    );
    y += card.subtitle.size().y + BODY_GAP * s;

    let mut x = inner.left();
    for column in &card.columns {
        paint_column(painter, column, Pos2::new(x, y), s);
        x += column.size.x + COLUMN_GAP * s;
    }
}

fn paint_column(painter: &Painter, column: &Column, top_left: Pos2, s: f32) {
    let cap_fill = Color32::from_white_alpha(22);
    let cap_stroke = Stroke::new(1.0, Color32::from_white_alpha(45));
    let mut y = top_left.y;
    for (i, item) in column.items.iter().enumerate() {
        match item {
            Item::Heading(galley) => {
                if i > 0 {
                    y += HEADING_ABOVE * s;
                }
                painter.galley(Pos2::new(top_left.x, y), galley.clone(), Color32::WHITE);
                y += galley.size().y + HEADING_BELOW * s;
            }
            Item::Row { caps, text } => {
                let mid = y + ROW * s / 2.0;
                let mut x = top_left.x;
                for cap in caps {
                    let width = cap_width(cap, s);
                    let key = Rect::from_min_size(
                        Pos2::new(x, mid - CAP * s / 2.0),
                        Vec2::new(width, CAP * s),
                    );
                    painter.rect(key, 7.0 * s, cap_fill, cap_stroke, StrokeKind::Inside);
                    painter.galley(
                        Align2::CENTER_CENTER
                            .anchor_size(key.center(), cap.size())
                            .min,
                        cap.clone(),
                        Color32::WHITE,
                    );
                    x += width + CAP_GAP * s;
                }
                let text_x = top_left.x + column.caps_width + CAPS_TO_TEXT * s;
                painter.galley(
                    Pos2::new(text_x, mid - text.size().y / 2.0),
                    text.clone(),
                    Color32::WHITE,
                );
                y += ROW * s;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_key_is_listed_once() {
        let mut keys: Vec<Key> = SHORTCUTS
            .iter()
            .flat_map(|sc| sc.inputs)
            .filter_map(|&input| match input {
                K(key) => Some(key),
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
    }
}
