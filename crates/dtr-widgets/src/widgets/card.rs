//! The modal card: one centered sheet over a dimmed window.
//!
//! Four surfaces — about, the keyboard map, the preferences card and the serial
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
//! **The body scrolls**, because a card is clamped to the window and a form is
//! not: a preferences card on a short window, a keyboard map beside a small
//! terminal or a serial sheet with a long detail line would otherwise draw past
//! the sheet's own edge. The scroll area is the card's, so no screen has to
//! remember it, and its bar is solid — the floating bar egui defaults to paints
//! over the body's right edge, which is exactly where a card's values and key
//! names end.
//!
//! A **tab row** belongs to the card too, between the header and the body. It is
//! the same argument as the footer's: the way from one panel to another must not
//! scroll away, so it is drawn outside the scroll area, and a card whose panels
//! are different heights keeps one height so nothing moves under the pointer.
//!
//! The **footer** is a row the card reserves *below* the scroll area and draws
//! outside it. Settings wants two buttons, the keyboard map a hint, the serial
//! sheet both — and a footer is how one leaves a sheet, so a button that
//! scrolls out of reach is a button that is not there. It is one row: a card
//! that asked for two would have a body that is not the card.

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
    /// The answers a card is asking for, drawn under the scrolling body.
    ///
    /// Boxed because the card is a builder: a generic footer would make every
    /// builder method generic over it, and a `Card<'a, F>` is a worse thing to
    /// read than one allocation per frame.
    footer: Option<Box<dyn FnOnce(&mut Ui) + 'a>>,
    /// A row of tabs between the header and the scrolling body.
    ///
    /// Above the scroll area rather than in it, for the reason the footer is
    /// below it: the way from one panel to another must not scroll out of reach,
    /// and a tab row that scrolled would be a card whose second panel needs the
    /// first panel's scrolling to find.
    tabs: Option<Box<dyn FnOnce(&mut Ui) + 'a>>,
    /// The width the body may not shrink below, inside the card's margin.
    body_min: f32,
    /// Which layer the card is in.
    order: egui::Order,
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
            footer: None,
            tabs: None,
            body_min: 0.0,
            order: egui::Order::Foreground,
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

    /// A row of answers, drawn under the scrolling body and outside it.
    ///
    /// The card reserves the skin's row plus its gap for it, so a footer whose
    /// own controls are row-high fits where the card put it. Whatever it reports
    /// is the screen's to collect — a closure cannot hand a value back through
    /// the card without a second type parameter, so a footer writes into the
    /// same cell its body does.
    pub fn footer(mut self, footer: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.footer = Some(Box::new(footer));
        self
    }

    /// A row of tabs between the header and the body.
    ///
    /// The card draws it and gets out of the way: the row it costs is the row its
    /// closure asks for, and the scrolling body starts below it. A card whose
    /// panels differ in height keeps one height — see [`Self::size`] — so the way
    /// between them does not move under the pointer.
    pub fn tabs(mut self, tabs: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.tabs = Some(Box::new(tabs));
        self
    }

    /// The narrowest the sheet's content may be, inside the card's margin. The
    /// keyboard map and the serial sheet are forms, not prose: below this they
    /// would be unreadable rather than merely small. The body is handed this
    /// width less the scroll bar, when a bar is showing.
    pub fn body_min(mut self, width: f32) -> Self {
        self.body_min = width;
        self
    }

    /// The layer the card is drawn in.
    ///
    /// [`egui::Order::Foreground`] by default, which is what a modal wants: it
    /// is above everything the frame drew before it. A card that belongs to a
    /// *pane* rather than to the window — a notice over one terminal while the
    /// rest of the window is still being used — asks for [`egui::Order::Middle`]
    /// instead, so that it sits above the panes and below the cards that must
    /// never be covered by it. Layer order within one `Order` is the order the
    /// areas were first created in, which is not something a card can know.
    pub fn order(mut self, order: egui::Order) -> Self {
        self.order = order;
        self
    }

    /// Draw the card and call `body` with the inner Ui and its content width.
    ///
    /// The width is passed rather than asked of the body because the card owns
    /// the clamp and the scroll bar: a body that recomputed `size - margin`
    /// would disagree with the card on a window narrower than the requested
    /// size, and one that did not subtract the bar would draw under it.
    pub fn show<R>(
        mut self,
        ui: &mut Ui,
        skin: &Skin,
        body: impl FnOnce(&mut Ui, f32) -> R,
    ) -> CardOutput<R> {
        let screen = ui.max_rect();
        let width = self.size.x.min(screen.width() - 32.0).max(self.min.x);
        let height = self.size.y.min(screen.height() - 32.0).max(self.min.y);
        let rect = Rect::from_center_size(screen.center(), Vec2::new(width, height));
        prime_area(ui.ctx(), self.id, self.order, rect, true);

        // The terminal is not the subject while the card is up. Enough scrim to
        // read the card against, not so much that the window looks disabled.
        ui.painter().rect_filled(screen, 0.0, skin.palette.scrim);

        let footer = self.footer.take();
        let tabs = self.tabs.take();
        let margin = skin.style.card_margin;
        let mut close = false;
        let mut result = None;
        egui::Area::new(self.id)
            .order(self.order)
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
                        let inner = (width - margin.sum().x).max(self.body_min);
                        ui.set_width(inner);
                        close = self.header(ui, skin);
                        if let Some(tabs) = tabs {
                            ui.add_space(skin.style.gap);
                            tabs(ui);
                        }

                        // The room down to the window's edge — the margin the
                        // card was clamped by, the sheet's own margin and the
                        // band the footer was promised taken out. The requested
                        // size is where the card is *centered*, not how tall it
                        // may be: a card that needs a little more than it asked
                        // for grows, and only a window too small for it makes
                        // the body scroll.
                        let band = if footer.is_some() {
                            skin.style.row + skin.style.gap
                        } else {
                            0.0
                        };
                        let bottom = screen.max.y - 16.0 - margin.bottomf();
                        let room = (bottom - ui.cursor().top() - band).max(1.0);
                        // A floating bar paints over the body's right edge, which
                        // is where a value or a key name ends; a solid one takes
                        // that space out of `available_width` instead.
                        ui.spacing_mut().scroll.floating = false;
                        egui::ScrollArea::vertical()
                            .id_salt(self.id.with("body"))
                            .max_height(room)
                            .auto_shrink([false, true])
                            .scroll_bar_visibility(
                                egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                            )
                            .show(ui, |ui| {
                                let inner = ui.available_width();
                                result = Some(body(ui, inner));
                            });
                        if let Some(footer) = footer {
                            ui.add_space(skin.style.gap);
                            footer(ui);
                        }
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

    /// A body longer than the sheet scrolls inside it, and the footer stays in
    /// the card: a card is clamped to the window, so the one thing a long body
    /// must not do is draw past the card's own edge — and the one thing a footer
    /// must not do is leave with it.
    #[test]
    fn a_long_body_scrolls_and_the_footer_stays_in_the_card() {
        let skin = skin();
        let pressed = std::rc::Rc::new(std::cell::Cell::new(false));
        let sink = pressed.clone();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(360.0, 300.0))
            .build_ui(move |ui| {
                let footer_skin = skin.clone();
                let sink = sink.clone();
                let _ = Card::new(egui::Id::unique("test-scrolling-card"), "long")
                    .size(420.0, 560.0)
                    .min(300.0, 200.0)
                    .close("close test card")
                    .footer(move |ui| {
                        if crate::Button::new("done").primary().show(ui, &footer_skin) {
                            sink.set(true);
                        }
                    })
                    .show(ui, &skin, |ui, _inner| {
                        for line in 0..40 {
                            ui.label(format!("line {line}"));
                        }
                    });
            });
        harness.run();
        harness.snapshot("card_scrolled");

        // The card is the window less 32 points either way, centered: 328 × 268
        // at the corner's 16. The footer's row must be inside that.
        let footer = harness.get_by_label("done").rect();
        assert!(
            footer.max.y <= 284.5,
            "the footer left the card: {footer:?}"
        );
        assert!(footer.min.y >= 16.0, "the footer left the card: {footer:?}");

        // And it is a footer, not a picture of one: it still answers.
        harness.get_by_label("done").click();
        harness.run();
        assert!(pressed.get(), "the footer must be pressable");
    }

    /// The tab row is the card's, not the body's: a long body scrolls under it
    /// and the way to the other panel stays where it was.
    #[test]
    fn a_tab_row_stays_above_a_long_body() {
        let skin = skin();
        let picked = std::rc::Rc::new(std::cell::Cell::new(false));
        let sink = picked.clone();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(360.0, 300.0))
            .build_ui(move |ui| {
                let tab_skin = skin.clone();
                let sink = sink.clone();
                let _ = Card::new(egui::Id::unique("test-tabbed-card"), "tabbed")
                    .size(420.0, 560.0)
                    .min(300.0, 200.0)
                    .tabs(move |ui| {
                        if crate::tabs(
                            ui,
                            &tab_skin,
                            "test",
                            false,
                            &[(false, "one"), (true, "two")],
                        )
                        .is_some()
                        {
                            sink.set(true);
                        }
                    })
                    .show(ui, &skin, |ui, _inner| {
                        for line in 0..40 {
                            ui.label(format!("line {line}"));
                        }
                    });
            });
        harness.run();
        // The row is inside the card's top edge and above the first body line.
        let tab = harness.get_by_label("test two").rect();
        assert!(tab.min.y < harness.get_by_label("line 0").rect().min.y);
        harness.get_by_label("test two").click();
        harness.run();
        assert!(picked.get(), "a tab row in a card must answer");
    }
}
