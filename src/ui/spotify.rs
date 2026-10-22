//! Layout modelled on the Spotify desktop client's fullscreen mode.

use egui::{Color32, Id, Painter, Pos2, Rect, Sense, Vec2};

use super::{
    cover_rect, fit_text, fmt_time, font_bold, font_light, scaled_rect, with_alpha, View, FULL_UV,
};
use crate::art::ArtTextures;
use crate::player::Command;

const TEXT_BACKGROUND: Color32 = Color32::from_rgb(156, 156, 156);
const TEXT_FOREGROUND: Color32 = Color32::from_rgb(255, 255, 255);
const TEXT_MIDGROUND: Color32 = Color32::from_rgb(195, 195, 195);
const BACKGROUND_IMAGE_TINT: Color32 = Color32::from_rgb(75, 75, 75);
const BACKGROUND: Color32 = Color32::from_rgb(50, 50, 50);
const ACCENT: Color32 = Color32::from_rgb(29, 185, 84);

/// Progress bar geometry, shared by drawing and hit-testing.
struct Bar {
    rect: Rect,
    hover_band: Rect,
}

impl Bar {
    fn new(screen: Rect) -> Self {
        let (w, h) = (screen.width(), screen.height());
        let width = w * 0.85;
        let height = h * 0.006;
        let rect = Rect::from_min_size(
            screen.min + Vec2::new((w - width) * 0.5, h * 0.9),
            Vec2::new(width, height),
        );
        let margin = h * 0.02;
        let hover_band = Rect::from_x_y_ranges(
            screen.x_range(),
            rect.center().y - margin..=rect.center().y + margin,
        );
        Self { rect, hover_band }
    }

    fn fraction_at(&self, pos: Pos2) -> Option<f32> {
        (self.hover_band.contains(pos) && self.rect.x_range().contains(pos.x))
            .then(|| (pos.x - self.rect.left()) / self.rect.width())
    }
}

/// Draws the layout and returns the command a click asked for, if any.
pub fn draw(ui: &mut egui::Ui, view: &View) -> Option<Command> {
    let screen = ui.max_rect();
    let painter = ui.painter_at(screen);
    let bar = Bar::new(screen);
    if view.capture {
        paint(&painter, screen, &bar, false, view);
        return None;
    }
    let response = ui.interact(screen, Id::unique("spotify-layout"), Sense::click());
    let hover = response
        .hover_pos()
        .is_some_and(|p| bar.hover_band.contains(p));

    paint(&painter, screen, &bar, hover, view);

    let pos = response
        .interact_pointer_pos()
        .filter(|_| response.clicked())?;
    let duration = view.player.current.as_ref().and_then(|s| s.duration);
    match (bar.fraction_at(pos), duration) {
        (Some(fraction), Some(d)) => Some(Command::SeekTo(d.mul_f32(fraction.clamp(0.0, 1.0)))),
        _ => Some(Command::TogglePause),
    }
}

fn paint(painter: &Painter, screen: Rect, bar: &Bar, hover: bool, view: &View) {
    let (w, h) = (screen.width(), screen.height());
    let origin = screen.min;
    let player = view.player;
    let alpha = view.art.fade;

    painter.rect_filled(screen, 0.0, BACKGROUND);

    // Artist background
    let draw_bg = |art: &ArtTextures, a: f32| {
        if let Some(tex) = &art.artist_bg {
            painter.image(
                tex.id(),
                cover_rect(tex.size_vec2(), screen),
                FULL_UV,
                with_alpha(BACKGROUND_IMAGE_TINT, a),
            );
        }
    };
    if alpha < 1.0 {
        if let Some(prev) = view.art.previous {
            draw_bg(prev, 1.0 - alpha);
        }
    }
    if let Some(cur) = view.art.current {
        draw_bg(cur, alpha);
    }

    // Watermark
    let wm_pos = origin + Vec2::new(w / 20.0, h / 20.0);
    let wm_rect = match view.watermark {
        Some(tex) => {
            let size = tex.size_vec2();
            let rect = scaled_rect(size, (h / 9.0) / size.y.max(1.0), wm_pos);
            painter.image(tex.id(), rect, FULL_UV, Color32::WHITE);
            rect
        }
        None => Rect::from_min_size(wm_pos, Vec2::ZERO),
    };

    // Queue info
    let from_text = painter.layout_no_wrap(
        "PLAYING FROM MPD QUEUE".into(),
        font_light(h / 42.0),
        TEXT_BACKGROUND,
    );
    let text_x = wm_rect.right() + wm_rect.height() / 4.0;
    let from_y = wm_rect.top() + from_text.size().y;
    let queue_text = painter.layout_no_wrap(
        format!("{} tracks left", player.queue_len),
        font_bold(h / 31.0),
        TEXT_BACKGROUND,
    );
    let queue_y = from_y + queue_text.size().y;
    painter.galley(Pos2::new(text_x, from_y), from_text, TEXT_BACKGROUND);
    painter.galley(Pos2::new(text_x, queue_y), queue_text, TEXT_BACKGROUND);

    // Album art
    let art_size = w.min(h) / 3.0;
    let art_rect = Rect::from_min_size(
        origin + Vec2::new(w / 16.0, h / 6.0 * 5.0 - art_size),
        Vec2::splat(art_size),
    );
    if alpha < 1.0 {
        if let Some(prev) = view.art.previous {
            painter.image(
                prev.cover.id(),
                art_rect,
                FULL_UV,
                with_alpha(Color32::WHITE, 1.0 - alpha),
            );
        }
    }
    if let Some(cur) = view.art.current {
        painter.image(
            cur.cover.id(),
            art_rect,
            FULL_UV,
            with_alpha(Color32::WHITE, alpha),
        );
    }

    // Title and artist
    let (title, artist) = match &player.current {
        Some(song) => (song.title.as_str(), song.artist.as_str()),
        None => ("Nothing playing", ""),
    };
    let title_pos = Pos2::new(
        art_rect.left() + art_size * 1.1,
        art_rect.top() + art_size * 0.4,
    );
    let available = (screen.right() - title_pos.x) * 0.97;
    let title_size = w.min(h) / 9.0;
    let title_text = fit_text(
        painter,
        title,
        font_bold,
        title_size,
        available,
        TEXT_FOREGROUND,
    );
    let artist_pos = title_pos + Vec2::new(0.0, title_text.size().y + art_size * 0.05);
    painter.galley(title_pos, title_text, TEXT_FOREGROUND);
    let artist_text = fit_text(
        painter,
        artist,
        font_bold,
        title_size / 2.0,
        available,
        TEXT_MIDGROUND,
    );
    painter.galley(artist_pos, artist_text, TEXT_MIDGROUND);

    // Progress bar
    let bar_h = bar.rect.height();
    let radius = bar_h / 2.1;
    let progress = player.progress();
    painter.rect_filled(bar.rect, radius, TEXT_BACKGROUND.gamma_multiply(0.5));
    let filled = Rect::from_min_size(bar.rect.min, Vec2::new(bar.rect.width() * progress, bar_h));
    painter.rect_filled(filled, radius, if hover { ACCENT } else { TEXT_FOREGROUND });

    let duration = player
        .current
        .as_ref()
        .and_then(|s| s.duration)
        .unwrap_or_default();
    let elapsed_text = painter.layout_no_wrap(
        fmt_time(player.elapsed()),
        font_light(bar_h * 3.0),
        TEXT_FOREGROUND,
    );
    let text_y = bar.rect.top() - elapsed_text.size().y / 3.1;
    let elapsed_pos = Pos2::new(
        bar.rect.left() - elapsed_text.size().x - bar_h * 1.5,
        text_y,
    );
    painter.galley(elapsed_pos, elapsed_text, TEXT_FOREGROUND);
    let duration_text =
        painter.layout_no_wrap(fmt_time(duration), font_light(bar_h * 3.0), TEXT_FOREGROUND);
    painter.galley(
        Pos2::new(bar.rect.right() + bar_h * 1.5, text_y),
        duration_text,
        TEXT_FOREGROUND,
    );

    if hover {
        painter.circle_filled(
            Pos2::new(filled.right(), bar.rect.center().y),
            bar_h,
            TEXT_FOREGROUND,
        );
    }

    // Up next, shown for the last 10% of the song
    if progress >= 0.9 {
        let font_size = h / 30.0;
        let upnext_text = painter.layout_no_wrap(
            "Up next:".into(),
            font_bold(font_size * 1.1),
            TEXT_FOREGROUND,
        );
        let next_label = match &player.next {
            Some(song) if !song.artist.is_empty() => format!("{} - {}", song.artist, song.title),
            Some(song) => song.title.clone(),
            None => "Nothing".to_owned(),
        };
        let next_text = painter.layout_no_wrap(next_label, font_light(font_size), TEXT_FOREGROUND);
        let next_x = screen.right() - upnext_text.size().x.max(next_text.size().x) - w / 30.0;
        let next_y = origin.y + h / 20.0;
        let countdown_y = next_y + upnext_text.size().y + next_text.size().y + font_size / 2.0;
        let shrink = (screen.right() - next_x) * (progress * 10.0 - 9.0);
        let next_h = upnext_text.size().y;
        painter.galley(Pos2::new(next_x, next_y), upnext_text, TEXT_FOREGROUND);
        painter.galley(
            Pos2::new(next_x, next_y + next_h),
            next_text,
            TEXT_FOREGROUND,
        );
        let countdown = Rect::from_min_max(
            Pos2::new(next_x + shrink, countdown_y),
            Pos2::new(screen.right() + 5.0, countdown_y + bar_h),
        );
        painter.rect_filled(countdown, radius, TEXT_FOREGROUND);
    } else if view.show_clock && !view.capture {
        let clock = view.now.format("%H:%M").to_string();
        painter.text(
            Pos2::new(screen.right() - w / 30.0, origin.y + h / 20.0),
            egui::Align2::RIGHT_TOP,
            clock,
            font_bold(h / 22.0),
            TEXT_FOREGROUND,
        );
    }
}
