//! The command palette: one line that starts a session.
//!
//! Modelled on the editor palettes people already know — type, arrow, enter —
//! because the alternative (a dialog of dropdowns) is slower for the same task
//! and cannot be driven from the keyboard.

use dtr_complete::{Completion, Engine, Inline};
use egui::{Align2, FontId, Key, Modifiers, RichText, Sense, TextEdit, vec2};

use crate::ui::Look;

use crate::sources;

/// What accepting a line asks the app to do.
pub enum Action {
    Connect(String),
    Theme(String),
    Ssh(String),
    Shell(String),
    Replay(String),
    /// Open a file pane in the current tab, at this path when one was given.
    Files(Option<String>),
    Demo,
    Quit,
    /// Show the keyboard map: the palette's own answer for somebody who does not
    /// know the chord yet.
    Keys,
    /// The preferences card, for the same reason `Keys` is here: a person who
    /// does not know `ctrl+,` should not have to find a menu button either.
    Preferences,
    /// The same page the strip's menu opens.
    About,
    /// The debug overlay: fps, frame time and the vsync wait `present` hides.
    ///
    /// Named `overlay` and not `stats` because a command's first letter is its
    /// accelerator and `s` is `ssh`'s — `crates/dtr-complete` fails the build on
    /// a collision. The `--stats` flag is the other way in.
    Stats,
    /// Delayed input for a serial line: `lag` and not `delay`, because `d` is
    /// demo's accelerator, and not `slow` because `s` is ssh's.
    KeyDelay,
}

pub struct Palette {
    engine: Engine,
    pub open: bool,
    input: String,
    selected: usize,
    completions: Vec<Completion>,
    inline: Option<Inline>,
}

impl Default for Palette {
    fn default() -> Self {
        Self::new(false)
    }
}

impl Palette {
    /// An empty palette. Deliberately *not* reading the machine: this is
    /// constructed on every frame (the render path takes it out of the app to
    /// satisfy the borrow checker), and `engine()` walks all of sysfs through
    /// serialport. That walk was 38% of a profile of somebody scrolling.
    pub fn new(open: bool) -> Self {
        let mut palette = Self {
            engine: dtr_complete::Engine::new(),
            open,
            input: String::new(),
            selected: 0,
            completions: Vec::new(),
            inline: None,
        };
        if open {
            palette.refresh();
        }
        palette.recompute();
        palette
    }

    /// Re-read ports and hosts: the device you want is probably the one you
    /// just plugged in.
    fn refresh(&mut self) {
        let started = std::time::Instant::now();
        self.engine = sources::engine();
        tracing::debug!(
            elapsed_ms = started.elapsed().as_millis(),
            "completion sources refreshed"
        );
    }

    /// Which completion is highlighted. Read by the tests to check the keys,
    /// and test-only on purpose: production draws the highlight, it does not ask
    /// where it is.
    #[cfg(test)]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// What has been typed. Read by the tests, and by anything that wants to
    /// know what the palette is asking without looking at the screen.
    pub fn text(&self) -> &str {
        &self.input
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        if self.open {
            // Every open starts from nothing: a palette that remembers the last
            // command makes the next one a correction rather than a sentence.
            self.input.clear();
            self.selected = 0;
            self.completions.clear();
            self.inline = None;
            self.refresh();
            self.recompute();
        }
    }

    fn recompute(&mut self) {
        self.completions = self.engine.complete(&self.input);
        self.inline = self.engine.inline(&self.input);
        if self.selected >= self.completions.len() {
            self.selected = 0;
        }
    }

    /// Draw the palette over the grid, and report what the user chose.
    ///
    /// An overlay inside the same `Ui` rather than an `egui::Window`: a palette
    /// is a fixed thing at a fixed place, and a floating window would invite
    /// dragging it around a terminal that has no room for it.
    pub fn ui(&mut self, ui: &mut egui::Ui, look: &Look) -> Option<Action> {
        if !self.open {
            return None;
        }

        let screen = ui.max_rect();
        let width = 560.0f32.min(screen.width() - 32.0).max(280.0);
        let skin = look.skin();
        let palette = skin.palette;
        let style = &skin.style;
        let inset = 10.0;
        let top = screen.top() + 32.0;
        let left = screen.center().x - width / 2.0;

        // The card is painted *after* its body, into two shapes that were added
        // before the body took its space, so its height is the height of what it
        // holds. A fixed height was a card that a skin with tall rows outgrew:
        // the last completions — and the selected row with them — landed below
        // it, on the window behind. Swapping the shapes afterwards is the only
        // way to have the frame both behind the text and sized by it.
        let painter = ui.painter().clone();
        let backdrop = painter.add(egui::Shape::Noop);
        let border = painter.add(egui::Shape::Noop);

        // How much of the window the body may use. Everything is measured from
        // this rather than predicted: the field's height and the skin's row
        // spacing are egui's and the skin's, not numbers this file knows.
        let room = egui::Rect::from_min_size(
            egui::pos2(left + inset, top + inset),
            vec2(
                width - 2.0 * inset,
                (screen.height() - 64.0 - 2.0 * inset).max(style.row),
            ),
        );

        let mut action = None;
        let mut close = false;
        let body = ui
            .scope_builder(egui::UiBuilder::new().max_rect(room), |ui| {
                ui.set_width(ui.available_width());

                // Keys first: consuming them here keeps the text field from
                // also acting on them (arrow keys move the caret otherwise).
                // Arrows for the fingers that expect them, `C-n`/`C-p` for the
                // ones that have been navigating lists since Emacs. Both are
                // consumed here so the text field never sees them.
                let (down, up, enter, tab, escape) = ui.input_mut(|input| {
                    let down = input.consume_key(Modifiers::NONE, Key::ArrowDown)
                        | input.consume_key(Modifiers::CTRL, Key::N);
                    let up = input.consume_key(Modifiers::NONE, Key::ArrowUp)
                        | input.consume_key(Modifiers::CTRL, Key::P);
                    (
                        down,
                        up,
                        input.consume_key(Modifiers::NONE, Key::Enter),
                        input.consume_key(Modifiers::NONE, Key::Tab),
                        input.consume_key(Modifiers::NONE, Key::Escape),
                    )
                });
                close = escape;
                if escape {
                    // What was abandoned, so `-v` can show whether a query was
                    // cancelled or never typed.
                    tracing::debug!(query = self.text(), "palette closed");
                }
                if down && !self.completions.is_empty() {
                    self.selected = (self.selected + 1) % self.completions.len();
                }
                if up && !self.completions.is_empty() {
                    self.selected =
                        (self.selected + self.completions.len() - 1) % self.completions.len();
                }
                if enter {
                    action = self.accept_selected();
                }
                if tab {
                    match self.inline.clone() {
                        Some(inline) => {
                            self.input = inline.insert;
                            self.recompute();
                        }
                        None => action = self.accept_selected(),
                    }
                }

                let query_font =
                    FontId::new(style.value_font.size + 2.5, style.value_font.family.clone());
                // Centred in the field, not hung from its top: a single-line
                // `TextEdit` lays its galley out with the *parent's* alignment —
                // `LEFT_TOP` in a top-down `Ui` — and the field is as tall as the
                // skin's row, so the query sat at the top of a 34-point Adwaita
                // field instead of in the middle of it. `widgets/combo.rs` has the
                // same trap and the same answer.
                let response = TextEdit::singleline(&mut self.input)
                    .hint_text("connect, ssh, shell, replay, files, demo…")
                    .font(query_font.clone())
                    .desired_width(f32::INFINITY)
                    .align(Align2::LEFT_CENTER)
                    .show(ui);
                if response.response.changed() {
                    self.recompute();
                }
                if ui.memory(|memory| memory.focused().is_none()) {
                    response.response.request_focus();
                }

                // Ghost text: what Tab would add, painted where it would appear.
                if let Some(inline) = &self.inline {
                    let origin = response.galley_pos + vec2(response.galley.size().x, 0.0);
                    ui.painter().text(
                        origin,
                        Align2::LEFT_TOP,
                        &inline.suffix,
                        query_font,
                        palette.disabled,
                    );
                }

                ui.add_space(6.0);
                if self.completions.is_empty() {
                    ui.label(
                        RichText::new("nothing matches")
                            .font(style.value_font.clone())
                            .color(palette.dim),
                    );
                    return;
                }

                // The two hint lines at the bottom are measured *before* the
                // rows, not after them: the second one wraps at this width, so
                // what it costs is a fact about the fonts rather than a number,
                // and counting only the rows left a short window's hints under
                // its bottom edge — outside the card drawn around them.
                let hint_font =
                    FontId::new(style.label_font.size - 1.5, style.label_font.family.clone());
                let hints = [
                    (
                        "up/down or ctrl-n/ctrl-p choose   tab complete   enter run   esc close",
                        palette.dim,
                    ),
                    // The only place the keys are written down. Nothing else in
                    // the window says that tabs exist, which is how a terminal
                    // ends up with one tab and no idea there could be more.
                    (
                        "ctrl+shift t tab    ctrl+tab switch    ctrl+| split    \
                         ctrl+shift w close    ctrl+shift o block    ctrl+shift p palette    \
                         ctrl+, preferences    ctrl+? keys",
                        palette.disabled,
                    ),
                ];
                let galleys: Vec<(egui::Color32, std::sync::Arc<egui::Galley>)> = hints
                    .iter()
                    .map(|(text, colour)| {
                        let galley = ui.painter().layout(
                            (*text).to_owned(),
                            hint_font.clone(),
                            *colour,
                            room.width(),
                        );
                        (*colour, galley)
                    })
                    .collect();
                let spacing = ui.spacing().item_spacing.y;
                let hint_gap = 4.0;
                let hints_height = galleys
                    .iter()
                    .map(|(_, galley)| galley.size().y)
                    .sum::<f32>()
                    + spacing * (galleys.len() - 1) as f32;

                // Ten rows is what a list wants at most, and a list you can see at
                // once beats one you have to scroll — but the skin decides how tall
                // a row is, so how many fit is measured from the space the field
                // left, less what the hints below them need. A compact skin gets
                // ten; a tall one gets fewer rather than rows outside the card.
                let row = style.row;
                let fits = (((ui.available_height() - hint_gap - hints_height + spacing)
                    / (row + spacing))
                    .floor() as usize)
                    .max(1);
                let visible = 10.min(self.completions.len()).min(fits);
                let first = self.selected.saturating_sub(visible - 1);
                let row_height = style.row;
                for index in first..(first + visible).min(self.completions.len()) {
                    let completion = &self.completions[index];
                    let selected = index == self.selected;
                    let (rect, response) = ui.allocate_exact_size(
                        vec2(ui.available_width(), row_height),
                        Sense::click(),
                    );

                    // Background before text: painting the highlight after the
                    // labels hid the selected row's own text.
                    //
                    // The pair is the palette's `selection`/`on_selection`, not the
                    // accent on a pressed control: a theme's accent is a colour
                    // chosen to be *seen*, not one chosen to be read on, and the
                    // native skin's blue over its own grey band was neither. These
                    // two slots are the ones a skin fills in to be legible.
                    let painter = ui.painter();
                    if selected {
                        painter.rect_filled(
                            rect.expand2(vec2(4.0, 0.0)),
                            style.control_radius.saturating_sub(1),
                            palette.selection,
                        );
                    }
                    painter.text(
                        rect.left_center(),
                        Align2::LEFT_CENTER,
                        &completion.item.label,
                        style.value_font.clone(),
                        if selected {
                            palette.on_selection
                        } else {
                            palette.text
                        },
                    );
                    painter.text(
                        rect.right_center(),
                        Align2::RIGHT_CENTER,
                        &completion.item.detail,
                        FontId::new(style.label_font.size - 0.5, style.label_font.family.clone()),
                        if selected {
                            palette.on_selection.gamma_multiply(0.75)
                        } else {
                            palette.dim
                        },
                    );
                    if response.clicked() {
                        self.selected = index;
                        action = self.accept_selected();
                    }
                }

                // Painted through `ui.label` from the same strings and font the
                // rows were measured against, so the reservation and the picture
                // cannot disagree about a wrapped line — and the hints stay in the
                // accessibility tree, which painted galleys would not.
                ui.add_space(hint_gap);
                for (text, colour) in hints {
                    ui.label(RichText::new(text).font(hint_font.clone()).color(colour));
                }
            })
            .response
            .rect;

        // The card, now that its body has taken its space: the two placeholders
        // that were added first become the frame. The top and the left are known
        // — the body starts at the inset — and the bottom is where the content
        // ended, which is what makes a tall skin's sheet tall.
        //
        // A floor, not a cap: the card keeps the height it has always had, so a
        // query that filters to three rows does not make it jump under the eye,
        // while a skin with tall rows grows it instead of spilling out of it.
        let floor = (top + 300.0).min(screen.bottom() - 4.0);
        let frame = egui::Rect::from_min_max(
            egui::pos2(left, top),
            egui::pos2(left + width, (body.bottom() + inset).max(floor)),
        )
        .intersect(screen.shrink(4.0));
        painter.set(
            backdrop,
            egui::Shape::rect_filled(frame, style.card_radius, palette.card),
        );
        painter.set(
            border,
            egui::Shape::rect_stroke(
                frame,
                style.card_radius,
                egui::Stroke::new(style.border, palette.border),
                egui::StrokeKind::Inside,
            ),
        );

        if close {
            self.open = false;
        }
        action
    }

    fn accept_selected(&mut self) -> Option<Action> {
        if self.completions.is_empty() {
            return None;
        }
        let line = self.completions[self.selected].item.insert.clone();
        self.open = false;
        action_for(&line)
    }
}

/// Turn a completed line into an action.
///
/// The argument is everything after the first word, not the second word: theme
/// names and replay paths contain spaces (most of the shipped theme names do).
/// Unknown lines do nothing — the palette is not a shell, and guessing would be
/// worse than ignoring.
fn action_for(line: &str) -> Option<Action> {
    let (command, rest) = match line.split_once(char::is_whitespace) {
        Some((command, rest)) => (command, rest.trim()),
        None => (line, ""),
    };
    match command {
        "connect" if !rest.is_empty() => Some(Action::Connect(rest.to_owned())),
        "theme" if !rest.is_empty() => Some(Action::Theme(rest.to_owned())),
        "ssh" if !rest.is_empty() => Some(Action::Ssh(rest.to_owned())),
        "exec" => Some(Action::Shell(rest.to_owned())),
        "replay" if !rest.is_empty() => Some(Action::Replay(rest.to_owned())),
        "files" => Some(Action::Files((!rest.is_empty()).then(|| rest.to_owned()))),
        "demo" => Some(Action::Demo),
        "quit" => Some(Action::Quit),
        "keys" => Some(Action::Keys),
        "preferences" => Some(Action::Preferences),
        "about" => Some(Action::About),
        // `stats` would share ssh's accelerator; see `Action::Stats`.
        "overlay" => Some(Action::Stats),
        // `delay` would share demo's accelerator, `slow` ssh's; see
        // `Action::KeyDelay`.
        "lag" => Some(Action::KeyDelay),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, action_for};

    #[test]
    fn connect_lines_become_connect_actions() {
        match action_for("connect /dev/ttyUSB0") {
            Some(Action::Connect(port)) => assert_eq!(port, "/dev/ttyUSB0"),
            _ => panic!("expected a connect action"),
        }
    }

    #[test]
    fn theme_lines_become_theme_actions() {
        match action_for("theme kanagawa wave") {
            // The whole argument, spaces included: most theme names have them.
            Some(Action::Theme(name)) => assert_eq!(name, "kanagawa wave"),
            _ => panic!("expected a theme action"),
        }
    }

    #[test]
    fn bare_exec_is_allowed() {
        assert!(matches!(action_for("exec"), Some(Action::Shell(s)) if s.is_empty()));
    }

    #[test]
    fn a_replay_path_with_a_space_survives() {
        match action_for("replay /tmp/my capture.bin") {
            Some(Action::Replay(path)) => assert_eq!(path, "/tmp/my capture.bin"),
            _ => panic!("expected a replay action"),
        }
    }

    #[test]
    fn files_opens_a_pane_and_takes_an_optional_path() {
        assert!(matches!(action_for("files"), Some(Action::Files(None))));
        match action_for("files /tmp/some dir") {
            Some(Action::Files(Some(path))) => assert_eq!(path, "/tmp/some dir"),
            _ => panic!("expected a files action with a path"),
        }
    }

    #[test]
    fn preferences_opens_the_card_from_the_palette() {
        // Named `preferences` and not `settings` because the first letter is the
        // accelerator and `s` is ssh's: the command is the card's own name.
        assert!(matches!(
            action_for("preferences"),
            Some(Action::Preferences)
        ));
    }

    #[test]
    fn overlay_toggles_the_debug_counter() {
        // `stats` would collide with ssh's accelerator; the command is named for
        // what it draws and the flag keeps the word the docs use.
        assert!(matches!(action_for("overlay"), Some(Action::Stats)));
    }

    #[test]
    fn lag_toggles_the_serial_input_delay() {
        // `delay` would share demo's accelerator and `slow` ssh's: a command's
        // first letter is its accelerator, and the completion engine refuses a
        // collision at build time.
        assert!(matches!(action_for("lag"), Some(Action::KeyDelay)));
    }

    #[test]
    fn unknown_lines_do_nothing() {
        assert!(action_for("nonsense").is_none());
        assert!(action_for("connect").is_none());
    }
}

#[cfg(test)]
mod kittest_tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    /// Type a command, press Enter, get the action — the loop the terminal
    /// actually uses, driven through egui instead of through a window.
    #[test]
    fn typing_a_command_and_pressing_enter_returns_its_action() {
        let mut palette = Palette::new(false);
        palette.toggle();
        let look = Look::default();
        let action = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = action.clone();
        let mut harness = Harness::new_ui(move |ui| {
            if let Some(action) = palette.ui(ui, &look) {
                *sink.borrow_mut() = Some(action);
            }
        });
        harness.run();

        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text("exec");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();

        let taken = action.borrow_mut().take();
        assert!(
            matches!(taken, Some(Action::Shell(_))),
            "expected a shell action, got something else"
        );
    }

    /// The card is reachable by name from the palette, so somebody who does not
    /// know `ctrl+,` (or where the burger is) can still find it: the same loop as
    /// the shell command above, through the field and the list.
    #[test]
    fn the_palette_opens_the_preferences_card_by_name() {
        let mut palette = Palette::new(false);
        palette.toggle();
        let look = Look::default();
        let action = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = action.clone();
        let mut harness = Harness::new_ui(move |ui| {
            if let Some(action) = palette.ui(ui, &look) {
                *sink.borrow_mut() = Some(action);
            }
        });
        harness.run();

        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text("preferences");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();

        assert!(
            matches!(action.borrow_mut().take(), Some(Action::Preferences)),
            "typing the command should open the card"
        );
    }

    /// Reopening starts a fresh sentence: a palette that remembers the last
    /// command makes the next one a correction.
    #[test]
    fn reopening_clears_what_was_typed() {
        let palette = std::rc::Rc::new(std::cell::RefCell::new(Palette::new(false)));
        palette.borrow_mut().toggle();
        let look = Look::default();
        let shared = std::rc::Rc::clone(&palette);
        let mut harness = Harness::new_ui(move |ui| {
            shared.borrow_mut().ui(ui, &look);
        });
        harness.run();
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text("exec");
        harness.run();
        assert_eq!(palette.borrow().text(), "exec", "typing reached the field");

        palette.borrow_mut().toggle();
        palette.borrow_mut().toggle();
        harness.run();
        assert_eq!(palette.borrow().text(), "", "the field is empty on reopen");
    }

    /// `C-n` and `C-p` walk the list, and so do the arrows.
    #[test]
    fn control_n_and_p_move_through_the_completions() {
        let palette = std::rc::Rc::new(std::cell::RefCell::new(Palette::new(false)));
        palette.borrow_mut().toggle();
        let look = Look::default();
        let shared = std::rc::Rc::clone(&palette);
        let mut harness = Harness::new_ui(move |ui| {
            shared.borrow_mut().ui(ui, &look);
        });
        harness.run();

        // An empty query lists every command, so there is always something to
        // move through whatever the machine has plugged in. `key_press` carries
        // no modifiers, so the chords go in as raw events.
        harness.input_mut().events.push(egui::Event::Key {
            key: egui::Key::N,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::CTRL,
        });
        harness.run();
        assert_eq!(palette.borrow().selected(), 1, "C-n moves down one");

        harness.input_mut().events.push(egui::Event::Key {
            key: egui::Key::P,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::CTRL,
        });
        harness.run();
        assert_eq!(palette.borrow().selected(), 0, "C-p comes back");
    }

    /// The window installs the resolved skin on the context (`Gpu::render`), so
    /// every stock widget a screen borrows is drawn at the skin's row: without
    /// this the palette's `TextEdit` is an 18-point field rather than Adwaita's
    /// 34, and the harness cannot show what a query does inside one.
    fn install_skin(look: &Look, ui: &mut egui::Ui) {
        look.skin().apply(ui.ctx());
    }

    /// The palette as the eye sees it: a query, the completions it filtered to,
    /// and the key hints underneath.
    #[test]
    fn the_palette_renders() {
        let mut palette = Palette::new(false);
        palette.toggle();
        let look = Look::default();
        let mut harness = Harness::new_ui(|ui| {
            install_skin(&look, ui);
            palette.ui(ui, &look);
        });
        harness.run();
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text("s");
        harness.run();
        harness.snapshot("palette");
    }

    /// The same palette under a look whose rows are tall: the card is as tall as
    /// its list, so the last completion — and the selected row — are on it
    /// rather than on the window behind it. A fixed height fitted the compact
    /// skin and nothing else, which is what `native` is on Linux.
    #[test]
    fn the_palette_fits_a_tall_skin() {
        let mut palette = Palette::new(false);
        palette.toggle();
        let look = Look {
            choice: dtr_widgets::Choice::new(dtr_widgets::Preset::Adwaita),
            ..Look::default()
        };
        let mut harness = Harness::new_ui(|ui| {
            install_skin(&look, ui);
            palette.ui(ui, &look);
        });
        harness.run();
        harness.snapshot("palette_adwaita");
    }

    /// A window too short for ten rows shows fewer of them, and the card still
    /// ends inside the window: the row count is measured from what the field
    /// left, so the clamp is the same arithmetic as the tall skin in reverse.
    #[test]
    fn the_palette_fits_a_short_window() {
        let mut palette = Palette::new(false);
        palette.toggle();
        let look = Look {
            choice: dtr_widgets::Choice::new(dtr_widgets::Preset::Windows),
            ..Look::default()
        };
        let mut harness = Harness::builder()
            .with_size(egui::vec2(560.0, 300.0))
            .build_ui(|ui| {
                install_skin(&look, ui);
                palette.ui(ui, &look);
            });
        harness.run();
        harness.snapshot("palette_short");
    }
}
