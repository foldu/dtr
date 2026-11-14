//! The right-click menu, in the two states that matter: open, and open in a
//! light theme.
//!
//! It is a primitive rather than a pane's private drawing because the picture is
//! the contract — a card fill, a hairline, a rounded corner and the row that is
//! already the value — and the pane that opens it must not be the only place it
//! is drawn.

use std::cell::RefCell;
use std::rc::Rc;

use dtr_widgets::{ContextMenu, MenuRow, Palette, Skin};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

fn dtr_skin() -> Skin {
    Skin::dtr(Palette::dtr(
        [0x0b, 0x0f, 0x0e],
        [0xdc, 0xdf, 0xdd],
        [0xff, 0xb0, 0x00],
    ))
}

/// An open menu over a card, with one plain row and one that names its chord —
/// the two shapes a right-press produces.
fn picture(skin: Skin) -> Harness<'static> {
    let menu = Rc::new(RefCell::new(ContextMenu::default()));
    menu.borrow_mut().open_at(egui::pos2(16.0, 16.0));
    let mut harness = Harness::builder()
        .with_size(vec2(260.0, 160.0))
        .build_ui(move |ui| {
            skin.apply(ui.ctx());
            ui.painter()
                .rect_filled(ui.max_rect(), 0.0, skin.palette.card);
            let mut menu = menu.borrow_mut();
            menu.ui(
                ui.ctx(),
                &skin,
                egui::Id::unique("context-menu"),
                &[
                    MenuRow::new("open"),
                    MenuRow::new("copy path").chord("ctrl-alt-c"),
                ],
            );
        });
    harness.run();
    harness
}

#[test]
fn the_menu_renders_in_dtr_chrome() {
    let mut harness = picture(dtr_skin());
    harness.snapshot("context_menu_dtr");
}

#[test]
fn the_menu_renders_on_a_light_card() {
    let mut harness = picture(Skin::windows_light());
    harness.snapshot("context_menu_windows_light");
}

/// A row is the answer, and it is an index into the rows the caller drew — not a
/// label, because the caller is the one that knows what a row *means*.
#[test]
fn a_row_is_the_answer() {
    let skin = dtr_skin();
    let menu = Rc::new(RefCell::new(ContextMenu::default()));
    menu.borrow_mut().open_at(egui::pos2(8.0, 8.0));
    let answer = Rc::new(RefCell::new(None));
    let picked = Rc::clone(&answer);
    let mut harness = Harness::builder()
        .with_size(vec2(260.0, 160.0))
        .build_ui(move |ui| {
            skin.apply(ui.ctx());
            let mut menu = menu.borrow_mut();
            if let Some(index) = menu.ui(
                ui,
                &skin,
                egui::Id::unique("context-menu"),
                &[MenuRow::new("open"), MenuRow::new("copy path")],
            ) {
                *picked.borrow_mut() = Some(index);
            }
        });
    harness.run();
    harness.get_by_label("copy path").click();
    harness.run();
    assert_eq!(*answer.borrow(), Some(1), "the second row is the answer");
}

/// Escape is the menu's own: it closes, and it is not a row.
#[test]
fn escape_closes_without_choosing_a_row() {
    let skin = dtr_skin();
    let menu = Rc::new(RefCell::new(ContextMenu::default()));
    menu.borrow_mut().open_at(egui::pos2(8.0, 8.0));
    let answer = Rc::new(RefCell::new(None));
    let picked = Rc::clone(&answer);
    let state = Rc::clone(&menu);
    let mut harness = Harness::builder()
        .with_size(vec2(260.0, 160.0))
        .build_ui(move |ui| {
            skin.apply(ui.ctx());
            let mut menu = menu.borrow_mut();
            if let Some(index) = menu.ui(
                ui,
                &skin,
                egui::Id::unique("context-menu"),
                &[MenuRow::new("open")],
            ) {
                *picked.borrow_mut() = Some(index);
            }
        });
    harness.run();
    assert!(state.borrow().is_open());
    harness.key_press(egui::Key::Escape);
    harness.run();
    assert!(!state.borrow().is_open(), "Escape must close the menu");
    assert_eq!(*answer.borrow(), None, "Escape is not a row");
}

/// The drawn rectangle is what a press is tested against, so it has to be where
/// the body is: a `contains` that answered from the press point instead would
/// dismiss the menu whenever a row was clicked far from where it opened.
#[test]
fn the_drawn_rect_is_the_target() {
    let skin = dtr_skin();
    let menu = Rc::new(RefCell::new(ContextMenu::default()));
    menu.borrow_mut().open_at(egui::pos2(40.0, 40.0));
    let state = Rc::clone(&menu);
    let mut harness = Harness::builder()
        .with_size(vec2(300.0, 200.0))
        .build_ui(move |ui| {
            skin.apply(ui.ctx());
            menu.borrow_mut().ui(
                ui,
                &skin,
                egui::Id::unique("context-menu"),
                &[MenuRow::new("open"), MenuRow::new("copy path")],
            );
        });
    harness.run();
    let menu = state.borrow();
    // The pointer opened it at (40, 40), and the body starts there.
    assert!(menu.contains(egui::pos2(50.0, 50.0)));
    assert!(
        !menu.contains(egui::pos2(5.0, 5.0)),
        "a press above and left of the body is outside it"
    );
}

/// A closed menu draws nothing, and its rectangle from a previous opening is not
/// a target: otherwise a press that (re)opened it would be measured against the
/// old body and could land "inside" a menu that is no longer there.
#[test]
fn a_closed_menu_has_no_target() {
    let menu = Rc::new(RefCell::new(ContextMenu::default()));
    assert!(!menu.borrow().is_open());
    let state = Rc::clone(&menu);
    let mut harness = Harness::builder()
        .with_size(vec2(120.0, 80.0))
        .build_ui(move |ui| {
            menu_ui(&mut state.borrow_mut(), ui);
        });
    harness.run();
    assert!(!menu.borrow().contains(egui::pos2(0.0, 0.0)));
    menu.borrow_mut().open_at(egui::pos2(4.0, 4.0));
    assert!(menu.borrow().is_open());
    menu.borrow_mut().close();
    assert!(!menu.borrow().contains(egui::pos2(4.0, 4.0)));
}

fn menu_ui(menu: &mut ContextMenu, ui: &mut egui::Ui) {
    menu.ui(
        ui,
        &dtr_skin(),
        egui::Id::unique("context-menu"),
        &[MenuRow::new("open")],
    );
}
