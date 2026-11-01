//! What a right-click on a link offers.
//!
//! One overlay, in egui, anchored at the pointer: it does not reach into the
//! terminal. The app draws it, acts on the answer, and the file itself is fetched
//! afterwards through the same VFS a click uses — including "Save as", whose
//! destination comes from the desktop's own dialog rather than from a widget here.

use egui::{Pos2, Rect, RichText, Stroke};

use crate::ui::Look;

/// What the menu was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Open it the way a left click would.
    Open,
    /// Ask where to put a copy.
    SaveAs,
    /// Put the path on the clipboard.
    CopyPath,
}

/// The menu a right-click on a link opens.
pub struct LinkMenu {
    open: bool,
    at: Pos2,
    title: String,
    subtitle: String,
    /// Where it was last drawn, so a press outside it can dismiss it. Points,
    /// like everything egui knows.
    rect: Rect,
}

impl Default for LinkMenu {
    fn default() -> Self {
        Self {
            open: false,
            at: Pos2::ZERO,
            title: String::new(),
            subtitle: String::new(),
            rect: Rect::NOTHING,
        }
    }
}

impl LinkMenu {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Is `at` (points) inside the menu as it was last drawn? The press that
    /// dismisses a menu has to be told apart from the press that uses one.
    pub fn contains(&self, at: Pos2) -> bool {
        self.open && self.rect.contains(at)
    }

    /// Open at `at`, about the file called `title` living at `subtitle`.
    pub fn open_at(&mut self, at: Pos2, title: String, subtitle: String) {
        self.open = true;
        self.at = at;
        self.title = title;
        self.subtitle = subtitle;
    }

    /// Draw it. Returns what the user chose, if anything.
    pub fn ui(&mut self, ui: &mut egui::Ui, look: &Look) -> Option<Choice> {
        if !self.open {
            return None;
        }
        // Escape is the menu's own business: it is the only thing that knows it
        // is open, and a modal dismissed elsewhere drifts out of sync with itself.
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.open = false;
            return None;
        }
        let title = self.title.clone();
        let subtitle = self.subtitle.clone();
        let mut choice = None;
        // Constrained, so a right-click near an edge does not put the last item
        // off the screen — which is exactly where "Save as" would be.
        let response = egui::Area::new(egui::Id::unique("dtr-link-menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(self.at)
            .constrain(true)
            .fade_in(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(Look::colour(look.surface))
                    .stroke(Stroke::new(
                        1.0,
                        Look::colour(look.dim).gamma_multiply(0.7),
                    ))
                    .corner_radius(4.0)
                    .inner_margin(6)
                    .show(ui, |ui| {
                        ui.set_min_width(190.0);
                        ui.label(
                            RichText::new(&title)
                                .monospace()
                                .color(Look::colour(look.foreground)),
                        );
                        ui.label(
                            RichText::new(&subtitle)
                                .size(10.0)
                                .color(Look::colour(look.dim)),
                        );
                        ui.separator();
                        for (label, wanted) in [
                            ("Open", Choice::Open),
                            // Three dots, not an ellipsis character: egui's own
                            // fonts are not guaranteed to have every glyph dtr
                            // puts in a label.
                            ("Save as...", Choice::SaveAs),
                            ("Copy path", Choice::CopyPath),
                        ] {
                            let button = egui::Button::new(label).min_size(egui::vec2(180.0, 0.0));
                            if ui.add(button).clicked() {
                                choice = Some(wanted);
                            }
                        }
                    });
            });
        self.rect = response.response.rect;
        choice
    }
}

#[cfg(test)]
mod tests {
    use super::{Choice, LinkMenu};
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;

    fn menu() -> LinkMenu {
        let mut menu = LinkMenu::default();
        menu.open_at(
            egui::pos2(40.0, 40.0),
            "holiday.png".to_owned(),
            "jupyter:/srv/pictures/holiday.png".to_owned(),
        );
        menu
    }

    /// The menu the right-click shows: what it is about, and the three things it
    /// can do.
    #[test]
    fn the_menu_offers_open_save_and_copy() {
        let look = crate::ui::Look::default();
        let menu = std::rc::Rc::new(std::cell::RefCell::new(menu()));
        let sink = std::rc::Rc::clone(&menu);
        let chosen = std::rc::Rc::new(std::cell::RefCell::new(None));
        let picked = std::rc::Rc::clone(&chosen);
        let mut harness = Harness::new_ui(move |ui| {
            if let Some(choice) = sink.borrow_mut().ui(ui, &look) {
                *picked.borrow_mut() = Some(choice);
            }
        });
        harness.run();

        // A label paints a text run inside itself, so every string here is two
        // nodes; what matters is that it is on screen at all.
        for text in ["holiday.png", "jupyter:/srv/pictures/holiday.png"] {
            assert!(
                harness.query_all_by_label_contains(text).count() > 0,
                "{text} is missing from the menu"
            );
        }
        harness.get_by_label("Save as...").click();
        harness.run();
        assert_eq!(*chosen.borrow(), Some(Choice::SaveAs));
    }
}
