//! What a right-click on a link offers, and where a saved copy goes.
//!
//! Two overlays, both egui, both anchored in points: the menu at the pointer, the
//! dialog in the middle. Neither reaches into the terminal — the app draws them
//! and acts on the answer — which is why the file itself is fetched by the app
//! afterwards, through the same VFS a click uses.

use std::path::{Path, PathBuf};

use egui::{Align, Layout, Pos2, Rect, RichText, Stroke};

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

/// What the save dialog decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Confirm {
        location: dtr_vfs::Location,
        dest: PathBuf,
    },
    Cancel,
}

/// Where to put a copy, with the whole path editable.
///
/// Not a file browser: a terminal's user knows how to type a path, and a picker
/// is a second filesystem UI to keep working on three platforms. The default is
/// there so Enter is always a complete answer.
pub struct SaveDialog {
    open: bool,
    location: Option<dtr_vfs::Location>,
    /// What is being saved, in the form the toast uses: `jupyter:/var/log/x.png`.
    described: String,
    path: String,
    rect: Rect,
    /// The field takes focus on the first frame it exists: a dialog you have to
    /// click before you can type is a dialog that types into the shell behind it.
    focus: bool,
}

impl Default for SaveDialog {
    fn default() -> Self {
        Self {
            open: false,
            location: None,
            described: String::new(),
            path: String::new(),
            rect: Rect::NOTHING,
            focus: false,
        }
    }
}

impl SaveDialog {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
        self.location = None;
    }

    pub fn contains(&self, at: Pos2) -> bool {
        self.open && self.rect.contains(at)
    }

    pub fn show(&mut self, location: dtr_vfs::Location, described: String, default: String) {
        self.open = true;
        self.location = Some(location);
        self.described = described;
        self.path = default;
        self.focus = true;
    }

    /// The path the field holds, in the form the file will be written to.
    pub fn dest(&self) -> PathBuf {
        expand(self.path.trim())
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, look: &Look) -> Option<Outcome> {
        if !self.open {
            return None;
        }
        let escape = ui.input(|input| input.key_pressed(egui::Key::Escape));
        let enter = ui.input(|input| input.key_pressed(egui::Key::Enter));

        let screen = ui.max_rect();
        let width = 520.0f32.min(screen.width() - 32.0).max(240.0);
        let rect = Rect::from_center_size(screen.center(), egui::vec2(width, 128.0));
        let painter = ui.painter();
        painter.rect_filled(rect, 6.0, Look::colour(look.surface));
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0, Look::colour(look.dim).gamma_multiply(0.7)),
            egui::StrokeKind::Inside,
        );
        self.rect = rect;

        let mut path = std::mem::take(&mut self.path);
        let described = self.described.clone();
        let focus = std::mem::replace(&mut self.focus, false);
        let mut confirm = false;
        let mut cancel = false;
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(rect.shrink(12.0)),
            |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("Save a copy")
                        .monospace()
                        .color(Look::colour(look.foreground)),
                );
                ui.label(
                    RichText::new(&described)
                        .size(10.0)
                        .color(Look::colour(look.dim)),
                );
                ui.add_space(4.0);
                let field = ui.add(
                    egui::TextEdit::singleline(&mut path)
                        .desired_width(f32::INFINITY)
                        .hint_text("where to put it"),
                );
                if focus {
                    field.request_focus();
                }
                ui.add_space(6.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Save").clicked() {
                        confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            },
        );
        self.path = path;

        if escape || cancel {
            self.close();
            return Some(Outcome::Cancel);
        }
        if enter || confirm {
            let dest = self.dest();
            // Nothing typed is not a place to put a file; the dialog stays up.
            if dest.as_os_str().is_empty() || dest == Path::new("~") {
                return None;
            }
            let location = self.location.take();
            self.close();
            return location.map(|location| Outcome::Confirm { location, dest });
        }
        None
    }
}

/// `~/Pictures/x.png` is in the home directory, not in a directory called `~`.
///
/// A path field that shows `~/Downloads/x.png` and then writes a literal `~` is
/// the kind of thing that is discovered after the file is gone.
pub fn expand(text: &str) -> PathBuf {
    if text == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = text.strip_prefix("~/") {
        if let Some(mut home) = dirs::home_dir() {
            home.push(rest);
            return home;
        }
    }
    PathBuf::from(text)
}

#[cfg(test)]
mod tests {
    use super::{Choice, LinkMenu, Outcome, SaveDialog, expand};
    use dtr_vfs::{BackendId, Location};
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;
    use std::path::PathBuf;

    fn location() -> Location {
        Location {
            backend: BackendId::Ssh("jupyter".to_owned()),
            path: PathBuf::from("/srv/pictures/holiday.png"),
        }
    }

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

    /// The dialog starts at the default, and Enter answers with it — no typing
    /// needed for the common case.
    #[test]
    fn the_dialog_saves_where_it_says() {
        let look = crate::ui::Look::default();
        let dialog = std::rc::Rc::new(std::cell::RefCell::new(SaveDialog::default()));
        dialog.borrow_mut().show(
            location(),
            "jupyter:/srv/pictures/holiday.png".to_owned(),
            "/home/me/Downloads/holiday.png".to_owned(),
        );
        let sink = std::rc::Rc::clone(&dialog);
        let outcome = std::rc::Rc::new(std::cell::RefCell::new(None));
        let picked = std::rc::Rc::clone(&outcome);
        let mut harness = Harness::new_ui(move |ui| {
            if let Some(outcome) = sink.borrow_mut().ui(ui, &look) {
                *picked.borrow_mut() = Some(outcome);
            }
        });
        harness.run();

        harness.get_by_label_contains("Save a copy");
        harness.get_by_label_contains("jupyter:/srv/pictures/holiday.png");
        harness.key_press(egui::Key::Enter);
        harness.run();

        match outcome.borrow().clone() {
            Some(Outcome::Confirm { location, dest }) => {
                assert_eq!(location, super::tests::location());
                assert_eq!(dest, PathBuf::from("/home/me/Downloads/holiday.png"));
            }
            other => panic!("expected a save, got {other:?}"),
        }
    }

    #[test]
    fn escape_cancels_and_closes() {
        let look = crate::ui::Look::default();
        let dialog = std::rc::Rc::new(std::cell::RefCell::new(SaveDialog::default()));
        dialog
            .borrow_mut()
            .show(location(), "described".to_owned(), "/tmp/x".to_owned());
        let sink = std::rc::Rc::clone(&dialog);
        let outcome = std::rc::Rc::new(std::cell::RefCell::new(None));
        let picked = std::rc::Rc::clone(&outcome);
        let mut harness = Harness::new_ui(move |ui| {
            if let Some(outcome) = sink.borrow_mut().ui(ui, &look) {
                *picked.borrow_mut() = Some(outcome);
            }
        });
        harness.run();
        harness.key_press(egui::Key::Escape);
        harness.run();

        assert_eq!(*outcome.borrow(), Some(Outcome::Cancel));
        assert!(!dialog.borrow().is_open());
    }

    /// A path field showing `~` must write to the home directory, not to a
    /// literal tilde.
    #[test]
    fn a_tilde_is_a_home_directory() {
        let home = dirs::home_dir().expect("a home directory");
        assert_eq!(expand("~/Downloads/x.png"), home.join("Downloads/x.png"));
        assert_eq!(expand("~"), home);
        assert_eq!(expand("/tmp/x"), PathBuf::from("/tmp/x"));
        assert_eq!(expand("~/"), home.join(""));
        // A tilde that is not a leading component is just a character.
        assert_eq!(expand("/tmp/~/x"), PathBuf::from("/tmp/~/x"));
    }
}
