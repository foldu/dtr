//! A pager for the last command block.
//!
//! `capture-block` reads the block the OSC 133 prompt marks delimit out of the
//! terminal and parks it in the pane; this is what shows it. It is deliberately
//! not `less`: the text is already in memory, so the pager only has to wrap it,
//! scroll it and let the mouse select it. A read-only `Label` inside a
//! `ScrollArea` does all three, which is less code than a line-oriented pager and
//! cannot disagree with egui's font about where a line breaks.
//!
//! The keys are the ones a terminal's pager has taught everyone: `j`/`k` and the
//! arrows, `space`/`b` for a page, `g`/`G` for the ends, `q`/esc to leave and `y`
//! to take the block away with you.

use egui::{Align, Layout, RichText, Stroke};

use crate::ui::Look;

/// What a keystroke asked the app to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// The user is done.
    Close,
    /// Put the block on the clipboard.
    Copy(String),
}

pub struct Pager {
    open: bool,
    /// What the block was: the command line that produced it, in practice.
    title: String,
    text: String,
    /// Where the view starts, in points. `ScrollArea` keeps the real value while
    /// the wheel and the scrollbar move it, and hands it back after each frame.
    scroll: f32,
    /// One page, in points. Only the last frame's rectangle knows it, and the
    /// key that asks for a page arrives before the frame that would measure it.
    page: f32,
}

impl Default for Pager {
    fn default() -> Self {
        Self {
            open: false,
            title: String::new(),
            text: String::new(),
            scroll: 0.0,
            page: 400.0,
        }
    }
}

impl Pager {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
        // The text is the memory: a block of a few hundred kilobytes held for the
        // rest of the process is a leak nobody would ever look for.
        self.text = String::new();
        self.title = String::new();
        self.scroll = 0.0;
    }

    /// Show a captured block. The title is its first line — the command, for a
    /// block a prompt mark delimited — so the header says what you are looking at
    /// without a second question.
    pub fn show(&mut self, text: String, fallback: &str) {
        self.title = text
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| line.trim_end().to_owned())
            .unwrap_or_else(|| fallback.to_owned());
        self.text = text;
        self.open = true;
        self.scroll = 0.0;
    }

    /// Handle one key. `None` when the key is not the pager's, so the caller can
    /// swallow it rather than typing it into a shell nobody is looking at.
    pub fn handle(
        &mut self,
        key: &winit::keyboard::Key,
        mods: &winit::keyboard::ModifiersState,
    ) -> Option<Action> {
        use winit::keyboard::{Key, NamedKey};

        let line = 18.0;
        match key {
            Key::Named(NamedKey::Escape) => Some(Action::Close),
            Key::Named(NamedKey::ArrowDown) => {
                self.scroll_by(line);
                None
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.scroll_by(-line);
                None
            }
            Key::Named(NamedKey::PageDown) => {
                self.scroll_by(self.page);
                None
            }
            Key::Named(NamedKey::PageUp) => {
                self.scroll_by(-self.page);
                None
            }
            Key::Named(NamedKey::Home) => {
                self.scroll = 0.0;
                None
            }
            Key::Named(NamedKey::End) => {
                // Past the end is clamped by the scroll area, which is the only
                // place that knows how tall the text turned out.
                self.scroll = f32::MAX;
                None
            }
            // A chord is not a pager key: `ctrl+g` is a binding, not "go to the
            // top", and swallowing it here would hide the binding.
            Key::Character(_) if mods.control_key() || mods.alt_key() => None,
            Key::Character(text) => {
                for ch in text.chars() {
                    match ch {
                        'q' => return Some(Action::Close),
                        'j' => self.scroll_by(line),
                        'k' => self.scroll_by(-line),
                        ' ' | 'd' => self.scroll_by(self.page),
                        'b' | 'u' => self.scroll_by(-self.page),
                        'g' => self.scroll = 0.0,
                        'G' => self.scroll = f32::MAX,
                        'y' => return Some(Action::Copy(self.text.clone())),
                        _ => {}
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Move the view. The top is known here; the bottom is not — only the frame
    /// that lays the text out knows how tall it is — so overshooting downwards is
    /// left for the scroll area to clamp and undershooting is not allowed at all.
    fn scroll_by(&mut self, delta: f32) {
        self.scroll = (self.scroll + delta).max(0.0);
    }

    /// Draw the pager, if it is open.
    ///
    /// The keys are handled before the frame — `App::on_key` has them — so drawing
    /// only has to lay the text out and hand back where the scroll area ended up.
    pub fn ui(&mut self, ui: &mut egui::Ui, look: &Look) {
        if !self.open {
            return;
        }
        let screen = ui.max_rect();
        let size = egui::vec2(
            (screen.width() * 0.86).min(1100.0).max(280.0),
            (screen.height() * 0.82).min(900.0).max(160.0),
        );
        let rect = egui::Rect::from_center_size(screen.center(), size);
        let painter = ui.painter();
        painter.rect_filled(rect, 6.0, Look::colour(look.surface));
        painter.rect_stroke(
            rect,
            6.0,
            Stroke::new(1.0, Look::colour(look.dim).gamma_multiply(0.5)),
            egui::StrokeKind::Inside,
        );

        ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink(10.0)), |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(
                        RichText::new(&self.title)
                            .monospace()
                            .color(Look::colour(look.foreground)),
                    )
                    .truncate(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new("esc closes   y copies")
                            .small()
                            .color(Look::colour(look.dim)),
                    );
                });
            });
            ui.separator();

            let height = (ui.available_height() - 4.0).max(0.0);
            self.page = height;
            let output = egui::ScrollArea::vertical()
                .id_salt(egui::Id::unique("dtr-pager-text"))
                .auto_shrink([false, false])
                .max_height(height)
                .vertical_scroll_offset(self.scroll)
                .show(ui, |ui| {
                    // Selectable, so the mouse can take a paragraph out of the
                    // block without opening an editor for it.
                    ui.add(
                        egui::Label::new(
                            RichText::new(&self.text)
                                .monospace()
                                .color(Look::colour(look.foreground)),
                        )
                        .selectable(true)
                        .wrap(),
                    );
                });
            self.scroll = output.state.clamped_offset().y;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Pager};
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    fn opened() -> Pager {
        let mut pager = Pager::default();
        pager.show("ls -la\ntotal 4\n".to_owned(), "block");
        pager
    }

    fn press(pager: &mut Pager, key: &Key) -> Option<Action> {
        pager.handle(key, &ModifiersState::empty())
    }

    #[test]
    fn the_title_is_the_command_that_made_the_block() {
        let pager = opened();
        assert_eq!(pager.title, "ls -la");
        assert!(pager.is_open());
    }

    #[test]
    fn a_block_with_no_text_still_names_itself() {
        let mut pager = Pager::default();
        pager.show(String::new(), "the block");
        assert_eq!(pager.title, "the block");
    }

    #[test]
    fn leaving_lets_go_of_the_text() {
        let mut pager = opened();
        assert_eq!(
            press(&mut pager, &Key::Named(NamedKey::Escape)),
            Some(Action::Close)
        );
        pager.close();
        assert!(!pager.is_open());
        assert!(pager.text.is_empty(), "the block is not kept for ever");
    }

    /// `q` and esc both leave, and `y` hands the whole block back to the caller —
    /// the pager has no clipboard of its own.
    #[test]
    fn the_keys_are_the_ones_a_pager_taught_everyone() {
        let mut pager = opened();
        assert_eq!(
            press(&mut pager, &Key::Character("q".into())),
            Some(Action::Close)
        );
        let mut pager = opened();
        assert_eq!(
            press(&mut pager, &Key::Character("y".into())),
            Some(Action::Copy("ls -la\ntotal 4\n".to_owned()))
        );
    }

    /// Scrolling is in points and clamped by the scroll area, so a key may ask for
    /// more than exists — it must not be able to ask for *less* than the top.
    #[test]
    fn the_view_moves_and_stops_at_the_top() {
        let mut pager = opened();
        pager.page = 100.0;
        press(&mut pager, &Key::Named(NamedKey::PageDown));
        assert_eq!(pager.scroll, 100.0);
        press(&mut pager, &Key::Character("j".into()));
        assert_eq!(pager.scroll, 118.0);
        press(&mut pager, &Key::Character("g".into()));
        assert_eq!(pager.scroll, 0.0);
        press(&mut pager, &Key::Named(NamedKey::PageUp));
        assert_eq!(pager.scroll, 0.0, "past the top is not a position");
    }

    /// A key that is not the pager's is still the pager's to swallow: typing into
    /// a shell nobody can see is worse than doing nothing.
    #[test]
    fn other_keys_do_nothing_and_say_so() {
        let mut pager = opened();
        assert_eq!(press(&mut pager, &Key::Character("x".into())), None);
        assert_eq!(pager.scroll, 0.0);
    }

    /// A chord belongs to the keymap, not to the pager.
    #[test]
    fn a_chord_is_not_a_pager_key() {
        let mut pager = opened();
        let ctrl = ModifiersState::CONTROL;
        assert_eq!(pager.handle(&Key::Character("j".into()), &ctrl), None);
        assert_eq!(pager.scroll, 0.0);
    }
}

#[cfg(test)]
mod render_tests {
    use super::Pager;
    use crate::ui::Look;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    /// The pager is laid out by egui, so the harness proves what a screenshot
    /// would: the whole block reached the label — one node carries all of it, so
    /// the last line is in the layout too — and the way out is written down.
    #[test]
    fn the_block_and_its_keys_are_on_screen() {
        let look = Look::default();
        let mut pager = Pager::default();
        let body: String = (0..40).map(|line| format!("line {line}\n")).collect();
        let block = format!("cargo test\n{body}");
        pager.show(block.clone(), "the block");

        let pager = std::rc::Rc::new(std::cell::RefCell::new(pager));
        let sink = std::rc::Rc::clone(&pager);
        let mut harness = Harness::new_ui(move |ui| sink.borrow_mut().ui(ui, &look));
        harness.run();

        // The body is one node with the whole text in it, so this is also the
        // assertion that no line was dropped.
        harness.get_by_label(&block);
        harness.get_by_label_contains("esc closes");
    }
}
