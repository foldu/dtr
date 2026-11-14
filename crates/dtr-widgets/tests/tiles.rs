//! The grid's cell, in every state a listing can put one in.
//!
//! A tile is painted, not a widget, so there is no accessibility node to click
//! and no hover to fake: the states are arguments. That makes this picture the
//! only place the four of them are seen side by side — an idle folder, one under
//! the pointer, a selected file and a dimmed hidden entry — and the two rows of
//! elided name underneath.
//!
//! Both looks are rendered on Linux, for the reason the sheet's gallery is: the
//! Windows skin is a palette and a token table, not a call into Windows, so a
//! tile whose label colour only works on a dark background shows up here.

use dtr_widgets::{Glyph, Icon, IconTile, Palette, Skin, icon_tile, tile_height};
use egui::{Pos2, Rect, vec2};

fn dtr_skin() -> Skin {
    Skin::dtr(Palette::dtr(
        [0x0b, 0x0f, 0x0e],
        [0xdc, 0xdf, 0xdd],
        [0xff, 0xb0, 0x00],
    ))
}

/// A cell's state, as the pane would hand it over.
struct Cell {
    glyph: Glyph,
    label: &'static str,
    selected: bool,
    hovered: bool,
    dim: bool,
}

const CELLS: &[Cell] = &[
    Cell {
        glyph: Glyph::Folder,
        label: "documents",
        selected: false,
        hovered: false,
        dim: false,
    },
    Cell {
        glyph: Glyph::Folder,
        label: "pictures",
        selected: false,
        hovered: true,
        dim: false,
    },
    Cell {
        glyph: Glyph::File,
        label: "notes.txt",
        selected: true,
        hovered: false,
        dim: false,
    },
    Cell {
        glyph: Glyph::Parent,
        label: "..",
        selected: false,
        hovered: false,
        dim: false,
    },
    // A name that has no chance of fitting: the ellipsis is the point of the
    // picture, and a name that fits proves nothing about the second row.
    Cell {
        glyph: Glyph::File,
        label: "a-report-with-a-name-nobody-would-type.txt",
        selected: false,
        hovered: false,
        dim: false,
    },
    Cell {
        glyph: Glyph::File,
        label: ".bashrc",
        selected: false,
        hovered: false,
        dim: true,
    },
];

fn grid(ui: &mut egui::Ui, skin: &Skin) {
    skin.apply(ui.ctx());
    let palette = skin.palette;
    ui.painter().rect_filled(ui.max_rect(), 0.0, palette.card);

    // A row's height is what one laid-out line measures; `Fonts::row_height`
    // needs `&mut Fonts`, which a `Ui` does not hand out.
    let line = ui
        .painter()
        .layout_no_wrap(
            "M".to_owned(),
            skin.style.value_font.clone(),
            skin.palette.text,
        )
        .size()
        .y;
    let height = tile_height(skin.style.tile_icon, line);
    let cell = vec2(112.0, height);
    let origin = ui.max_rect().min + vec2(12.0, 12.0);
    for (index, state) in CELLS.iter().enumerate() {
        let column = index % 3;
        let row = index / 3;
        let at = origin + vec2(column as f32 * (cell.x + 6.0), row as f32 * (cell.y + 10.0));
        icon_tile(
            ui,
            skin,
            Rect::from_min_size(Pos2::new(at.x, at.y), cell),
            &IconTile {
                label: state.label,
                icon: Icon::Glyph(state.glyph),
                selected: state.selected,
                hovered: state.hovered,
                dim: state.dim,
            },
        );
    }
}

fn picture(skin: Skin) -> egui_kittest::Harness<'static> {
    let mut harness = egui_kittest::Harness::builder()
        .with_size(vec2(380.0, 340.0))
        .build_ui(move |ui| grid(ui, &skin));
    harness.run();
    harness
}

/// dtr's own chrome: the folder takes the accent, the page takes the control
/// fill, and a selected name is the accent rather than an inverted pill's ink —
/// the same rule the list view's rows follow.
#[test]
fn the_cells_render_in_dtr_chrome() {
    let mut harness = picture(dtr_skin());
    harness.snapshot("tiles_dtr");
}

/// Windows in light mode, where a page painted in `control` is a white sheet on
/// a white card and only its hairline says it is there.
#[test]
fn the_cells_render_in_windows_light() {
    let mut harness = picture(Skin::windows_light());
    harness.snapshot("tiles_windows_light");
}
