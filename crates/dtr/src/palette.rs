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
}

pub struct Palette {
    engine: Engine,
    pub open: bool,
    input: String,
    selected: usize,
    completions: Vec<Completion>,
    inline: Option<Inline>,
}

impl Palette {
    pub fn new(open: bool) -> Self {
        let mut palette = Self {
            engine: sources::engine(),
            open,
            input: String::new(),
            selected: 0,
            completions: Vec::new(),
            inline: None,
        };
        palette.recompute();
        palette
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        if self.open {
            // Re-read ports and hosts: the device you want is probably the one
            // you just plugged in.
            self.engine = sources::engine();
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
        let rect = egui::Rect::from_min_size(
            egui::pos2(screen.center().x - width / 2.0, screen.top() + 32.0),
            vec2(width, 300.0),
        );

        let painter = ui.painter();
        painter.rect_filled(rect, 6.0, Look::colour(look.surface));
        painter.rect_stroke(
            rect,
            6.0,
            egui::Stroke::new(1.0, Look::colour(look.dim).gamma_multiply(0.5)),
            egui::StrokeKind::Inside,
        );

        let mut action = None;
        let mut close = false;
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(rect.shrink(10.0)),
            |ui| {
                ui.set_width(ui.available_width());

                // Keys first: consuming them here keeps the text field from
                // also acting on them (arrow keys move the caret otherwise).
                let (down, up, enter, tab, escape) = ui.input_mut(|input| {
                    (
                        input.consume_key(Modifiers::NONE, Key::ArrowDown),
                        input.consume_key(Modifiers::NONE, Key::ArrowUp),
                        input.consume_key(Modifiers::NONE, Key::Enter),
                        input.consume_key(Modifiers::NONE, Key::Tab),
                        input.consume_key(Modifiers::NONE, Key::Escape),
                    )
                });
                close = escape;
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

                let response = TextEdit::singleline(&mut self.input)
                    .hint_text("connect, ssh, shell, replay, files, demo…")
                    .font(FontId::monospace(15.0))
                    .desired_width(f32::INFINITY)
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
                        FontId::monospace(15.0),
                        Look::colour(look.dim).gamma_multiply(0.7),
                    );
                }

                ui.add_space(6.0);
                if self.completions.is_empty() {
                    ui.label(
                        RichText::new("nothing matches")
                            .size(12.0)
                            .color(Look::colour(look.dim)),
                    );
                    return;
                }

                // Ten rows is what fits without scrolling, and a list you can
                // see at once beats one you have to scroll.
                // Ten rows is what fits without scrolling, and a list you can
                // see at once beats one you have to scroll.
                let visible = 10.min(self.completions.len());
                let first = self.selected.saturating_sub(visible - 1);
                let row_height = 20.0;
                for index in first..(first + visible).min(self.completions.len()) {
                    let completion = &self.completions[index];
                    let selected = index == self.selected;
                    let (rect, response) = ui.allocate_exact_size(
                        vec2(ui.available_width(), row_height),
                        Sense::click(),
                    );

                    // Background before text: painting the highlight after the
                    // labels hid the selected row's own text.
                    let painter = ui.painter();
                    if selected {
                        painter.rect_filled(
                            rect.expand2(vec2(4.0, 0.0)),
                            3.0,
                            Look::colour(look.dim).gamma_multiply(0.25),
                        );
                    }
                    painter.text(
                        rect.left_center(),
                        Align2::LEFT_CENTER,
                        &completion.item.label,
                        FontId::monospace(13.0),
                        if selected {
                            Look::colour(look.accent)
                        } else {
                            Look::colour(look.foreground)
                        },
                    );
                    painter.text(
                        rect.right_center(),
                        Align2::RIGHT_CENTER,
                        &completion.item.detail,
                        FontId::proportional(11.0),
                        Look::colour(look.dim),
                    );
                    if response.clicked() {
                        self.selected = index;
                        action = self.accept_selected();
                    }
                }

                ui.add_space(4.0);
                ui.label(
                    RichText::new("↑↓ choose   tab complete   enter run   esc close")
                        .size(10.0)
                        .color(Look::colour(look.dim).gamma_multiply(0.7)),
                );
            },
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
        "shell" => Some(Action::Shell(rest.to_owned())),
        "replay" if !rest.is_empty() => Some(Action::Replay(rest.to_owned())),
        "files" => Some(Action::Files((!rest.is_empty()).then(|| rest.to_owned()))),
        "demo" => Some(Action::Demo),
        "quit" => Some(Action::Quit),
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
    fn bare_shell_is_allowed() {
        assert!(matches!(action_for("shell"), Some(Action::Shell(s)) if s.is_empty()));
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
    fn unknown_lines_do_nothing() {
        assert!(action_for("nonsense").is_none());
        assert!(action_for("connect").is_none());
    }
}
