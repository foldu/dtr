//! The manager window's chrome, at the size a window is.
//!
//! These are `egui_kittest` harnesses, not a window: `dtr-files` paints from
//! data and reports answers, so the whole chrome can be laid out, clicked, typed
//! into and photographed without a compositor. The four snapshots are the states
//! worth seeing: a place selected, a search, a list view with a multi-selection,
//! and the burger menu open over a hovered row.
//!
//! A baseline is an input (`AGENTS.md`, snapshots): the skin is written out
//! rather than resolved from the desktop, the rows' pictures are uploaded by the
//! test rather than read from the machine's icon theme, and the path fixtures
//! are invented — a snapshot of `/home/ana` must not be a snapshot of whoever
//! ran the test.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use dtr_files::manager::{
    Drive, EntryCount, Host, LocationMode, ManagerAnswer, ManagerLayout, ManagerState, MenuChoice,
    SelectionSummary, Sidebar, SidebarChoice, SidebarEntry,
};
use dtr_files::{Layout, View};
use dtr_widgets::{Palette, Skin};
use egui::Rect;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

const WIDTH: f32 = 900.0;
const HEIGHT: f32 = 620.0;

fn skin() -> Skin {
    Skin::dtr(Palette::dtr(
        [0x0b, 0x0f, 0x0e],
        [0xdc, 0xdf, 0xdd],
        [0xff, 0xb0, 0x00],
    ))
}

fn window_rect() -> Rect {
    Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(WIDTH, HEIGHT))
}

/// The content state a window would hand the chrome: a project directory, a
/// grid view, one file selected.
fn state() -> ManagerState {
    let mut state = ManagerState::new("/home/ana/Projects/dtr", "/home/ana", View::default());
    state.entries = EntryCount {
        total: 24,
        shown: 24,
        reading: false,
    };
    state.selection = SelectionSummary {
        count: 1,
        bytes: Some(1536),
    };
    state.can_go_back = true;
    state.can_go_forward = false;
    state.can_go_up = true;
    state
}

/// The sidebar a window would build from `icons::Places`, `/proc/mounts` and
/// `~/.ssh/config` — written out, so the snapshot is the same on every machine.
fn sidebar() -> Sidebar {
    Sidebar {
        places: vec![
            SidebarEntry::path("home", "/home/ana", false).named("user-home"),
            SidebarEntry::path("Downloads", "/home/ana/Downloads", false).named("folder-download"),
            SidebarEntry::path("Projects", "/home/ana/Projects", true).named("folder"),
            SidebarEntry::path("/", "/", false).named("drive-harddisk"),
        ],
        drives: Sidebar::drives(
            &[Drive {
                label: "data".to_owned(),
                path: PathBuf::from("/mnt/data"),
            }],
            Path::new("/home/ana/Projects/dtr"),
        ),
        hosts: Sidebar::hosts(
            &[Host {
                alias: "jupyter".to_owned(),
                home: Some(PathBuf::from("/home/ana")),
            }],
            None,
        ),
    }
}

/// The pictures the caller resolved and uploaded. Held alive for the test's
/// length: a dropped `TextureHandle` frees the texture the chrome is painting.
struct Pictures {
    _handles: Vec<egui::TextureHandle>,
}

/// One 16-pixel folder, drawn into a `ColorImage` rather than read from the
/// machine — a baseline may not depend on which icon theme is installed.
fn folder(ctx: &egui::Context, name: &str, rgb: [u8; 3]) -> egui::TextureHandle {
    const SIDE: usize = 16;
    let colour = egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
    let mut pixels = vec![egui::Color32::TRANSPARENT; SIDE * SIDE];
    for y in 0..SIDE {
        for x in 0..SIDE {
            let tab = (3..6).contains(&y) && (2..8).contains(&x);
            let body = (6..14).contains(&y) && (2..14).contains(&x);
            if tab || body {
                pixels[y * SIDE + x] = colour;
            }
        }
    }
    ctx.load_texture(
        name,
        egui::ColorImage::new([SIDE, SIDE], pixels),
        egui::TextureOptions::LINEAR,
    )
}

/// Build the harness and give every sidebar row a picture, through the harness's
/// own context so the textures live exactly as long as the frames do.
fn harness(
    state: ManagerState,
    sidebar: Sidebar,
) -> (Harness<'static>, Rc<RefCell<ManagerAnswer>>, Pictures) {
    let answer = Rc::new(RefCell::new(ManagerAnswer::default()));
    let sink = Rc::clone(&answer);
    let sidebar = Rc::new(RefCell::new(sidebar));
    let rows = Rc::clone(&sidebar);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(WIDTH, HEIGHT))
        .build_ui(move |ui| {
            let skin = skin();
            let layout = ManagerLayout::new(window_rect(), &skin);
            let got = dtr_files::manager::manager_ui(ui, &layout, &skin, &state, &rows.borrow());
            sink.borrow_mut().merge(&got);
        });
    harness.run();

    let mut pictures = Vec::new();
    {
        let mut borrowed = sidebar.borrow_mut();
        // Deref once, then take the three fields apart: a `RefMut`'s field
        // projections are not disjoint, and the rows are edited in place.
        let Sidebar {
            places,
            drives,
            hosts,
        } = &mut *borrowed;
        for (section, entries) in [(0, places), (1, drives), (2, hosts)] {
            for (index, entry) in entries.iter_mut().enumerate() {
                let rgb = match (section, index) {
                    (0, 0) => [0x9a, 0xc8, 0xf2],
                    (0, _) => [0xf2, 0xc8, 0x6b],
                    (1, _) => [0x8f, 0xb8, 0x9a],
                    _ => [0xc8, 0x9a, 0xf2],
                };
                let picture = folder(&harness.ctx, &format!("row {section}-{index}"), rgb);
                entry.icon = Some(picture.id());
                pictures.push(picture);
            }
        }
    }
    harness.run();
    (harness, answer, Pictures { _handles: pictures })
}

/// The window at 900 × 620, a place selected in the sidebar.
#[test]
fn the_manager_window_draws_its_chrome() {
    let (mut harness, answer, _pictures) = harness(state(), sidebar());
    // The toolbar, the sidebar and the status bar are all addressable: a painted
    // control with no accessibility node is invisible to a screen reader and
    // unaddressable from a test.
    for label in ["back", "forward", "up", "menu"] {
        assert!(harness.query_by_label(label).is_some(), "no {label} button");
    }
    for label in ["open ~", "open Projects", "open dtr"] {
        assert!(harness.query_by_label(label).is_some(), "no {label} crumb");
    }
    for label in ["home", "Downloads", "Projects", "data", "jupyter"] {
        assert!(
            harness.query_by_label(label).is_some(),
            "no {label} sidebar row"
        );
    }
    harness.snapshot("manager_window");
    // Nothing was pressed, so nothing is asked for.
    assert!(answer.borrow().is_empty(), "{:?}", answer.borrow());
}

/// A search with text in the field and a filtered count in the status bar.
#[test]
fn a_search_says_how_many_it_left() {
    let mut state = state();
    state.location = PathBuf::from("/var/log");
    state.search = "log".to_owned();
    state.entries = EntryCount {
        total: 128,
        shown: 3,
        reading: false,
    };
    state.selection = SelectionSummary::default();
    // The fixture marks Projects as the current place; `/var/log` is not one of
    // the sidebar's rows, so none of them is.
    let mut sidebar = sidebar();
    for row in sidebar
        .places
        .iter_mut()
        .chain(sidebar.drives.iter_mut())
        .chain(sidebar.hosts.iter_mut())
    {
        row.selected = false;
    }
    let (mut harness, _, _pictures) = harness(state, sidebar);
    // The field carries what was typed, which is also the check that it is a
    // real text field and not a word painted into a row.
    let field = harness.get_by_role(egui::accesskit::Role::TextInput);
    assert_eq!(field.value().as_deref(), Some("log"));
    harness.snapshot("manager_search");
}

/// The list view, and a multi-selection's count and total size.
#[test]
fn a_list_view_and_a_multi_selection_read_as_a_count() {
    let mut state = state();
    state.view.layout = Layout::List;
    state.selection = SelectionSummary {
        count: 3,
        bytes: Some(4_509_696),
    };
    let (mut harness, _, _pictures) = harness(state, sidebar());
    assert!(
        harness.query_by_label("view list").is_some(),
        "no list toggle"
    );
    harness.snapshot("manager_list_selection");
}

/// The burger menu open, with a row hovered — the menu's hover fill is a state
/// of its own, and this is where it is seen.
#[test]
fn the_burger_menu_opens_over_a_hovered_row() {
    let (mut harness, answer, _pictures) = harness(state(), sidebar());
    harness.get_by_label("menu").click();
    harness.run();
    assert!(
        harness.query_by_label("show hidden").is_some(),
        "the menu did not open"
    );
    harness.get_by_label("show hidden").hover();
    harness.run();
    harness.snapshot("manager_menu");
    // The hovered row reports only when pressed; the pointer did not press it.
    assert_eq!(answer.borrow().menu, None);
}

/// Back, forward and up are buttons, and a disabled one is a picture: the
/// window owns the history, so it says what is possible.
#[test]
fn the_toolbar_reports_back_and_up_but_not_a_disabled_forward() {
    let (mut harness, answer, _pictures) = harness(state(), sidebar());
    harness.get_by_label("back").click();
    harness.run();
    assert!(answer.borrow().back);
    assert!(
        !answer.borrow().forward,
        "the disabled forward button reported"
    );
    harness.get_by_label("up").click();
    harness.run();
    assert!(answer.borrow().up);
}

/// A breadcrumb segment is a target for the directory it names.
#[test]
fn a_crumb_opens_the_directory_it_names() {
    let (mut harness, answer, _pictures) = harness(state(), sidebar());
    harness.get_by_label("open Projects").click();
    harness.run();
    assert_eq!(
        answer.borrow().navigate,
        Some(PathBuf::from("/home/ana/Projects"))
    );
}

/// A sidebar row reports what choosing it means, not where the pixels were.
#[test]
fn a_sidebar_row_reports_its_choice() {
    let (mut harness, answer, _pictures) = harness(state(), sidebar());
    harness.get_by_label("jupyter").click();
    harness.run();
    assert_eq!(
        answer.borrow().sidebar,
        Some(SidebarChoice::Host {
            alias: "jupyter".to_owned(),
            home: Some(PathBuf::from("/home/ana")),
        })
    );
}

/// The grid/list control sets the view and says which one it chose.
#[test]
fn the_view_toggle_reports_the_other_layout() {
    let (mut harness, answer, _pictures) = harness(state(), sidebar());
    harness.get_by_label("view list").click();
    harness.run();
    assert_eq!(answer.borrow().layout, Some(Layout::List));
}

/// Typing in the field reports the whole text, not a delta: the window owns the
/// search string and writes what it is told.
#[test]
fn typing_in_the_search_field_reports_the_text() {
    let (mut harness, answer, _pictures) = harness(state(), sidebar());
    {
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .focus();
    }
    harness.run();
    harness
        .get_by_role(egui::accesskit::Role::TextInput)
        .type_text("log");
    harness.run();
    assert_eq!(answer.borrow().search.as_deref(), Some("log"));
}

/// A menu row reports its verb, and closes the menu.
#[test]
fn a_menu_row_reports_its_choice() {
    let (mut harness, answer, _pictures) = harness(state(), sidebar());
    harness.get_by_label("menu").click();
    harness.run();
    harness.get_by_label("edit location").click();
    harness.run();
    assert_eq!(answer.borrow().menu, Some(MenuChoice::EditLocation));
    assert_eq!(
        answer.borrow().location_mode,
        Some(LocationMode::Edit),
        "the mode travels with the row, so a caller need not match on it"
    );
}

/// The location field reports a typed path.
#[test]
fn the_location_field_reports_a_typed_path() {
    let mut state = state();
    state.location_mode = LocationMode::Edit;
    let path = "/home/ana/Projects/dtr";
    let (mut harness, answer, _pictures) = harness(state, sidebar());
    // Two text fields exist in this state — the location and the search — so a
    // query by role finds both; the field is the one holding the path. Its value
    // is also the check that it is a real text field and not a word painted in.
    let values: Vec<Option<String>> = harness
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .map(|node| node.value())
        .collect();
    assert!(
        values.iter().any(|value| value.as_deref() == Some(path)),
        "the field does not show the path: {values:?}"
    );
    // Focus the field that holds the path, then type into it. kittest's
    // accesskit focus action needs the node, so the query is repeated rather
    // than held across a frame.
    for node in harness.query_all_by_role(egui::accesskit::Role::TextInput) {
        if node.value().as_deref() == Some(path) {
            node.focus();
        }
    }
    harness.run();
    for node in harness.query_all_by_role(egui::accesskit::Role::TextInput) {
        if node.value().as_deref() == Some(path) {
            node.type_text("tmp");
        }
    }
    harness.run();
    let typed = answer.borrow().typed_location.clone().unwrap_or_default();
    assert!(
        typed.contains("tmp"),
        "typing did not reach the caller: {typed:?}"
    );
}

/// The geometry the window and the painter share: the content is where the
/// window will put the browser, and a point maps to exactly one band.
#[test]
fn the_layout_splits_the_window_for_both_consumers() {
    let skin = skin();
    let layout = ManagerLayout::new(window_rect(), &skin);
    assert_eq!(layout.client, window_rect());
    assert_eq!(layout.content.left(), layout.sidebar.right());
    assert_eq!(layout.content.right(), WIDTH);
    assert_eq!(layout.toolbar.bottom(), layout.content.top());
    assert_eq!(layout.content.bottom(), layout.status.top());
    let slots = layout.status_slots(&skin.style);
    assert!(
        slots.transfer.right() <= layout.status.right(),
        "the queue's slot escapes the status bar"
    );
}
