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
pub fn ui(ui: &mut Ui, look: &Look) -> Option<Event> {
    draw(ui, look, env!("CARGO_PKG_VERSION"))
}

/// The card with the version given rather than compiled in, so the kittest
/// snapshot can pin one. A baseline that carries the release version fails on
/// the tag instead of on the branch: the bump happens after `just release`'s
/// gate has passed, and the job that would notice is the one that publishes, so
/// every release would need a regenerated picture committed after the fact.
/// The picture holds a fixed version; the version this binary reports is
/// asserted without one.
#[allow(deprecated)] // one fixed overlay: the id is a name, not a hash of state
fn draw(ui: &mut Ui, look: &Look, version: &str) -> Option<Event> {
    let skin = look.skin();
    let style = &skin.style;
    let palette = skin.palette;
    let note = format!("version {version}");
    let out = dtr_widgets::Card::new(egui::Id::new("dtr-about"), "dtr")
        .size(WIDTH, HEIGHT)
        .body_min(280.0)
        .note(&note)
        .close("close about")
        .show(ui, &skin, |ui, _inner| {
            ui.add_space(style.gap * 2.0);
            // Stacked rather than in a column: a term wide enough for
            // "a super serial terminal" leaves a body narrow enough to
            // elide, and an about page that truncates its own sentences
            // is worse than one that is four lines longer.
            for (term, body) in [
                (
                    "a super serial terminal",
                    "libghostty-vt, and a file browser for some reason",
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
    out.close.then_some(Event::Close)
}

#[cfg(test)]
mod tests {
    use egui_kittest::kittest::Queryable;

    /// The card names the binary that is running, and closes on its own cross.
    #[test]
    fn the_about_card_says_which_version_this_is() {
        // A fixed version, because the snapshot is compared byte-exactly on the
        // tag: `0.0.0` never moves, so a release cannot invalidate its own gate.
        let look = crate::ui::Look::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 620.0))
            .build_ui(move |ui| {
                let _ = super::draw(ui, &look, "0.0.0");
            });
        harness.run_steps(2);
        harness.get_by_label("version 0.0.0");
        harness.get_by_label("a super serial terminal");
        harness.snapshot("about");

        // And the real card says what the binary was built as.
        let real = crate::ui::Look::default();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 620.0))
            .build_ui(move |ui| {
                let _ = super::ui(ui, &real);
            });
        harness.run_steps(2);
        harness.get_by_label(&format!("version {}", env!("CARGO_PKG_VERSION")));
    }
}
