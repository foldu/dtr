//! About dtr: what this is, and which version of it is running.
//!
//! A terminal has no documents to identify, so "about" is a small card rather than
//! a dialog with a title bar: the name, the version, the one sentence that tells a
//! person what they are looking at, and the two facts that matter when something
//! is wrong — the VT core it draws with and where it came from. Closed the same way
//! as every other card here: the cross, or Escape.

use egui::Ui;

use crate::ui::Look;

const WIDTH: f32 = 480.0;
const HEIGHT: f32 = 260.0;

/// What the card asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Close,
}

/// Draw the card. It reads nothing but the skin: every fact on it is a compile-
/// time constant, so it cannot disagree with the binary that is running.
#[allow(deprecated)]
pub fn ui(ui: &mut Ui, look: &Look) -> Option<Event> {
    let skin = look.skin();
    let palette = skin.palette;
    let style = &skin.style;
    let screen = ui.max_rect();
    let width = WIDTH.min(screen.width() - 32.0).max(280.0);
    let height = HEIGHT.min(screen.height() - 32.0).max(200.0);
    let rect = egui::Rect::from_center_size(screen.center(), egui::vec2(width, height));
    let id = egui::Id::new("dtr-about");
    crate::ui::prime_area(ui.ctx(), id, egui::Order::Foreground, rect, true);
    ui.painter().rect_filled(screen, 0.0, palette.scrim);

    let mut event = None;
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .constrain(false)
        .fade_in(false)
        .show(ui.ctx(), |ui| {
            skin.apply(ui.ctx());
            ui.set_min_size(rect.size());
            egui::Frame::new()
                .fill(palette.card)
                .stroke(egui::Stroke::new(style.border, palette.border))
                .corner_radius(style.card_radius)
                .inner_margin(style.card_margin)
                .show(ui, |ui| {
                    let inner = (width - style.card_margin.sum().x).max(280.0);
                    ui.set_width(inner);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("dtr")
                                .font(style.title_font.clone())
                                .color(palette.text),
                        );
                        ui.label(
                            egui::RichText::new(concat!("version ", env!("CARGO_PKG_VERSION")))
                                .font(egui::FontId::new(
                                    style.label_font.size,
                                    style.label_font.family.clone(),
                                ))
                                .color(palette.dim),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if dtr_widgets::cross(ui, &skin, "close about", 24.0).clicked() {
                                event = Some(Event::Close);
                            }
                        });
                    });
                    ui.add_space(style.gap * 2.0);
                    // Stacked rather than in a column: a term wide enough for
                    // "a serial-first terminal" leaves a body narrow enough to
                    // elide, and an about page that truncates its own sentences
                    // is worse than one that is four lines longer.
                    for (term, body) in [
                        (
                            "a serial-first terminal",
                            "libghostty-vt for the terminal itself",
                        ),
                        ("drawn with", "egui and wgpu, one thread per pane"),
                        ("config", "TOML, in ~/.config/dtr/config.toml"),
                    ] {
                        ui.label(
                            egui::RichText::new(term)
                                .font(egui::FontId::new(
                                    style.label_font.size - 1.0,
                                    style.label_font.family.clone(),
                                ))
                                .color(palette.dim),
                        );
                        ui.label(
                            egui::RichText::new(body)
                                .font(egui::FontId::new(
                                    style.label_font.size,
                                    style.label_font.family.clone(),
                                ))
                                .color(palette.text),
                        );
                        ui.add_space(style.gap * 0.5);
                    }
                    ui.add_space(style.gap);
                    ui.label(
                        egui::RichText::new("esc closes   ·   ctrl+? lists the keys")
                            .font(egui::FontId::new(
                                style.label_font.size - 1.0,
                                style.label_font.family.clone(),
                            ))
                            .color(palette.dim),
                    );
                });
        });
    event
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;

    /// The card names the binary that is running, and closes on its own cross.
    #[test]
    fn the_about_card_says_which_version_this_is() {
        let look = crate::ui::Look::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 620.0))
            .build_ui(move |ui| {
                let _ = super::ui(ui, &look);
            });
        harness.run_steps(2);
        harness.get_by_label(&format!("version {}", env!("CARGO_PKG_VERSION")));
        harness.get_by_label("a serial-first terminal");
        harness.snapshot("about");
    }
}
