//! Small notification cards stacked in the bottom-right corner.

use std::path::PathBuf;
use std::time::Duration;

use egui::{
    Align2, Color32, CursorIcon, Frame, Id, Margin, Order, Rect, RichText, Sense, Stroke, Vec2,
};

use super::{font_bold, font_light, with_alpha};

const WIDTH: f32 = 320.0;
const FADE: f64 = 0.25;
const AMBER: Color32 = Color32::from_rgb(240, 190, 100);

pub struct Toast {
    title: String,
    detail: Option<String>,
    warning: Option<String>,
    /// Folder opened when the toast is clicked.
    open: Option<PathBuf>,
    ttl: Duration,
    /// egui input time when shown; set on first draw.
    shown_at: Option<f64>,
}

impl Toast {
    pub fn new(title: impl Into<String>, ttl: Duration) -> Self {
        Self {
            title: title.into(),
            detail: None,
            warning: None,
            open: None,
            ttl,
            shown_at: None,
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn warning(mut self, warning: Option<String>) -> Self {
        self.warning = warning;
        self
    }

    pub fn opens(mut self, dir: PathBuf) -> Self {
        self.open = Some(dir);
        self
    }
}

/// The running export, shown as a toast with a progress bar and a cancel button.
pub struct Progress {
    pub title: String,
    /// 0 to 1.
    pub fraction: f32,
    pub queued: usize,
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, toast: Toast) {
        self.items.push(toast);
    }

    /// Pushes `toast` after removing those whose title starts with `prefix`, so repeated
    /// notices don't pile up.
    pub fn replace(&mut self, prefix: &str, toast: Toast) {
        self.items.retain(|t| !t.title.starts_with(prefix));
        self.items.push(toast);
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Draws the toasts and `progress`; returns true if the export's cancel button was clicked.
    pub fn show(&mut self, ctx: &egui::Context, progress: Option<&Progress>) -> bool {
        let now = ctx.input(|i| i.time);
        self.items
            .retain(|t| t.shown_at.is_none_or(|at| now - at < t.ttl.as_secs_f64()));
        if self.items.is_empty() && progress.is_none() {
            return false;
        }

        let mut cancel = false;
        let mut open = None;
        egui::Area::new(Id::unique("toasts"))
            .order(Order::Tooltip)
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-24.0, -24.0))
            .interactable(true)
            .show(ctx, |ui| {
                ui.set_width(WIDTH);
                ui.spacing_mut().item_spacing.y = 10.0;
                for (i, toast) in self.items.iter_mut().enumerate() {
                    let at = *toast.shown_at.get_or_insert(now);
                    let age = now - at;
                    let left = toast.ttl.as_secs_f64() - age;
                    let alpha = (age / FADE).min(left / FADE).clamp(0.0, 1.0) as f32;
                    let response = ui
                        .scope(|ui| {
                            ui.multiply_opacity(alpha);
                            card().show(ui, |ui| {
                                ui.set_width(WIDTH - 32.0);
                                ui.label(title(&toast.title));
                                if let Some(detail) = &toast.detail {
                                    ui.label(muted(detail));
                                }
                                if let Some(warning) = &toast.warning {
                                    ui.label(
                                        RichText::new(warning)
                                            .font(font_light(13.0))
                                            .color(with_alpha(AMBER, 0.85)),
                                    );
                                }
                            })
                        })
                        .inner
                        .response;
                    if let Some(dir) = &toast.open {
                        let click = ui
                            .interact(response.rect, Id::unique("toast").with(i), Sense::click())
                            .on_hover_cursor(CursorIcon::PointingHand);
                        if click.clicked() {
                            open = Some(dir.clone());
                        }
                    }
                }
                if let Some(p) = progress {
                    cancel = progress_card(ui, p);
                }
            });

        if let Some(dir) = open {
            open_folder(dir);
        }
        cancel
    }
}

fn card() -> Frame {
    Frame::new()
        .fill(Color32::from_rgba_unmultiplied(20, 20, 24, 225))
        .stroke(Stroke::new(1.0, Color32::from_white_alpha(20)))
        .corner_radius(12)
        .inner_margin(Margin::symmetric(16, 12))
}

fn title(text: &str) -> RichText {
    RichText::new(text)
        .font(font_bold(14.0))
        .color(Color32::WHITE)
}

fn muted(text: &str) -> RichText {
    RichText::new(text)
        .font(font_light(13.0))
        .color(with_alpha(Color32::WHITE, 0.55))
}

/// Progress card; returns true when its cancel button is clicked.
fn progress_card(ui: &mut egui::Ui, p: &Progress) -> bool {
    let mut cancel = false;
    card().show(ui, |ui| {
        ui.set_width(WIDTH - 32.0);
        let top = ui.max_rect().right_top();
        ui.label(title(&p.title));
        if p.queued > 0 {
            ui.label(muted(&format!("Queued: {} more", p.queued)));
        }
        ui.add_space(2.0);
        let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 3.0), Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(bar, 1.5, Color32::from_white_alpha(30));
        let mut done = bar;
        done.set_width(bar.width() * p.fraction.clamp(0.0, 1.0));
        painter.rect_filled(done, 1.5, with_alpha(Color32::WHITE, 0.9));

        let close = Rect::from_center_size(top + Vec2::new(-8.0, 9.0), Vec2::splat(20.0));
        let response = ui
            .interact(close, Id::unique("export-cancel"), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text("Cancel export");
        let alpha = if response.hovered() { 0.95 } else { 0.5 };
        ui.painter().text(
            close.center(),
            Align2::CENTER_CENTER,
            "×",
            font_light(18.0),
            with_alpha(Color32::WHITE, alpha),
        );
        cancel = response.clicked();
    });
    cancel
}

/// Opens `dir` in the file manager without waiting for it.
fn open_folder(dir: PathBuf) {
    let spawned = std::process::Command::new("xdg-open")
        .arg(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawned {
        // Reap the child in the background so it doesn't linger as a zombie.
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => log::warn!("failed to open {}: {e}", dir.to_string_lossy()),
    }
}
