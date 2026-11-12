//! The modal card: one centered sheet over a dimmed window.
//!
//! Four surfaces — about, the keyboard map, the settings menu and the serial
//! sheet — were each spelling the same shell: clamp a size to the screen, center
//! it, prime the area so the first frame is not in the corner, paint the scrim,
//! create the `Area`, install the skin, draw the frame, put a title and a cross
//! in the corner. Their bodies differ; the shell does not, and two copies of a
//! shell are two places a radius, a margin or the scrim can drift apart.
//!
//! A card owns the shell and the header convention:
//!
//! - an optional dim **kicker** above the title, which takes the cross with it —
//!   a sheet whose first line says what it is puts the way out beside that line;
//! - the **title**, with an optional inline **note** and an optional right-aligned
//!   **trailing** detail (the serial sheet's `115200 8N1`);
//! - the **cross**, named for accessibility.
//!
//! The footer belongs to the body. Settings wants two buttons, the keyboard map
//! a hint, the serial sheet both — and a card that guessed would have to grow a
//! slot for every screen's idea of a footer.

use egui::{Align, Context, FontId, Id, Layout, Rect, RichText, Stroke, Ui, Vec2};

use crate::{Skin, cross};

/// Put an `Area`'s remembered state where the card will actually be drawn.
///
/// An `Area` placed for the first time lays out at its default position and only
/// lands on `rect` on the next frame; a card that appears a frame late in the
/// corner is the flash this prevents. `size` matters too: it is what makes the
/// first frame's title row as wide as the card rather than as wide as the screen.
pub fn prime_area(ctx: &Context, id: Id, order: egui::Order, rect: Rect, interactable: bool) {
    let mut state = egui::AreaState::default();
    state.pivot_pos = Some(rect.min);
    state.size = Some(rect.size());
    state.interactable = interactable;
    ctx.memory_mut(|memory| {
        memory
            .areas_mut()
            .set_state(egui::LayerId::new(order, id), state);
    });
}

/// A centered card: the shell, the header, and a body supplied by the screen.
pub struct Card<'a> {
    id: Id,
    size: Vec2,
    min: Vec2,
    title: &'a str,
    kicker: Option<&'a str>,
    note: Option<&'a str>,
    trailing: Option<&'a str>,
    close: Option<&'a str>,
    /// The width the body may not shrink below, inside the card's margin.
    body_min: f32,
}

/// What the card did this frame: the cross, and whatever the body returned.
pub struct CardOutput<R> {
    pub close: bool,
    pub body: R,
}

impl<'a> Card<'a> {
    pub fn new(id: Id, title: &'a str) -> Self {
        Self {
            id,
            // A card with no size is a line; every caller sets one.
            size: Vec2::new(320.0, 200.0),
            min: Vec2::ZERO,
            title,
            kicker: None,
            note: None,
            trailing: None,
            close: None,
            body_min: 0.0,
        }
    }

    /// The size the card asks for. It is clamped to the window less 32 points
    /// either way, and then raised to [`Self::min`].
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.size = Vec2::new(width, height);
        self
    }

    /// The smallest the card may get on a window too small for [`Self::size`].
    pub fn min(mut self, width: f32, height: f32) -> Self {
        self.min = Vec2::new(width, height);
        self
    }

    /// A dim caption above the title, e.g. `serial connection`.
    pub fn kicker(mut self, text: &'a str) -> Self {
        self.kicker = Some(text);
        self
    }

    /// A dim note immediately right of the title, e.g. `version 0.1.0`.
    pub fn note(mut self, text: &'a str) -> Self {
        self.note = Some(text);
        self
    }

    /// A dim detail at the card's right edge on the title line.
    ///
    /// It shares the row with the title, so a card with both a kicker and a
    /// trailing detail puts the cross on the kicker's row and the detail on the
    /// title's — the way the serial sheet has always read.
    pub fn trailing(mut self, text: &'a str) -> Self {
        self.trailing = Some(text);
        self
    }

    /// The cross's accessible name. Without it the card has no way out but the
    /// keyboard, so every surface that means to be escapable passes one.
    pub fn close(mut self, label: &'a str) -> Self {
        self.close = Some(label);
        self
    }

    /// The narrowest the body may be, inside the card's margin. The keyboard map
    /// and the serial sheet are forms, not prose: below this they would be
    /// unreadable rather than merely small.
    pub fn body_min(mut self, width: f32) -> Self {
        self.body_min = width;
        self
    }

    /// Draw the card and call `body` with the inner Ui and its content width.
    ///
    /// The width is passed rather than asked of the body because the card owns
    /// the clamp: a body that recomputed `size - margin` would disagree with the
    /// card on a window narrower than the requested size.
    pub fn show<R>(
        self,
        ui: &mut Ui,
        skin: &Skin,
        body: impl FnOnce(&mut Ui, f32) -> R,
    ) -> CardOutput<R> {
        let screen = ui.max_rect();
        let width = self.size.x.min(screen.width() - 32.0).max(self.min.x);
        let height = self.size.y.min(screen.height() - 32.0).max(self.min.y);
        let rect = Rect::from_center_size(screen.center(), Vec2::new(width, height));
        prime_area(ui.ctx(), self.id, egui::Order::Foreground, rect, true);

        // The terminal is not the subject while the card is up. Enough scrim to
        // read the card against, not so much that the window looks disabled.
        ui.painter().rect_filled(screen, 0.0, skin.palette.scrim);

        let mut close = false;
        let mut result = None;
        egui::Area::new(self.id)
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .constrain(false)
            // A fade would make the card invisible on the first frame, and the
            // first frame is the only one a capture has.
            .fade_in(false)
            .show(ui.ctx(), |ui| {
                // The stock widgets inside a body (a text field, a combo box,
                // the scroll area's bar) read the context style, not the one the
                // card was given.
                skin.apply(ui.ctx());
                ui.set_min_size(rect.size());
                egui::Frame::new()
                    .fill(skin.palette.card)
                    .stroke(Stroke::new(skin.style.border, skin.palette.border))
                    .corner_radius(skin.style.card_radius)
                    .inner_margin(skin.style.card_margin)
                    .show(ui, |ui| {
                        let inner = (width - skin.style.card_margin.sum().x).max(self.body_min);
                        ui.set_width(inner);
                        close = self.header(ui, skin);
                        result = Some(body(ui, inner));
                    });
            });
        CardOutput {
            close,
            body: result.expect("the card's area is shown every frame"),
        }
    }

    /// The header: the kicker row carries the cross when there is one, then the
    /// title row carries the title and its details.
    fn header(&self, ui: &mut Ui, skin: &Skin) -> bool {
        let palette = skin.palette;
        let style = &skin.style;
        let mut close = false;
        let small = |text: &str| {
            RichText::new(text)
                .font(FontId::new(
                    style.label_font.size - 1.0,
                    style.label_font.family.clone(),
                ))
                .color(palette.dim)
        };
        let cross_here = |ui: &mut Ui, close: &mut bool| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(label) = self.close
                    && cross(ui, skin, label, 24.0).clicked()
                {
                    *close = true;
                }
            });
        };

        if let Some(kicker) = self.kicker {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(kicker)
                        .font(style.label_font.clone())
                        .color(palette.dim),
                );
                cross_here(ui, &mut close);
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(self.title)
                        .font(style.title_font.clone())
                        .color(palette.text),
                );
                if let Some(trailing) = self.trailing {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(small(trailing));
                    });
                }
            });
        } else {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(self.title)
                        .font(style.title_font.clone())
                        .color(palette.text),
                );
                if let Some(note) = self.note {
                    ui.label(
                        RichText::new(note)
                            .font(style.label_font.clone())
                            .color(palette.dim),
                    );
                }
                cross_here(ui, &mut close);
            });
        }
        close
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Palette, Skin};
    use egui_kittest::kittest::Queryable;

    fn skin() -> Skin {
        Skin::dtr(Palette::dtr(
            [0x0b, 0x0f, 0x0e],
            [0xdc, 0xdf, 0xdd],
            [0xff, 0xb0, 0x00],
        ))
    }

    /// The header carries every slot at once: kicker, title, trailing detail and
    /// the cross on the kicker's row. The body is the caller's, and the card
    /// reports the cross without knowing what closing means.
    #[test]
    fn the_header_carries_the_kicker_the_title_the_detail_and_the_cross() {
        let skin = skin();
        let closed = std::rc::Rc::new(std::cell::Cell::new(false));
        let sink = closed.clone();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(640.0, 480.0))
            .build_ui(move |ui| {
                let out = Card::new(egui::Id::unique("test-card"), "ttyUSB0")
                    .size(420.0, 280.0)
                    .min(300.0, 200.0)
                    .kicker("serial connection")
                    .trailing("115200 8N1")
                    .close("close test card")
                    .show(ui, &skin, |ui, _inner| {
                        ui.label("body");
                    });
                if out.close {
                    sink.set(true);
                }
            });
        harness.run();
        harness.get_by_label("serial connection");
        harness.get_by_label("ttyUSB0");
        harness.get_by_label("115200 8N1");

        harness.get_by_label("close test card").click();
        harness.run();
        assert!(closed.get(), "the cross must be reported");
    }
}
