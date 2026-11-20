//! The widget vocabulary the chrome is drawn with, in two styles.
//!
//! Before this crate every surface invented its own controls: the serial sheet
//! hand-painted twelve of them, the settings menu used egui's stock widgets, the
//! tab strip painted chips, and the palette, the explorer and the link menu each
//! had a fourth answer. The same control — a combo box — existed twice and looked
//! different in each place.
//!
//! The fix is one vocabulary asked for one *skin*:
//!
//! - [`Palette`] names the colours by role. The chrome used to write
//!   `mix(surface, foreground, 0.16)` at thirty call sites with fifteen different
//!   factors standing in for "raised", "border", "hover" and "scrim", and a style
//!   cannot change a rule that is a literal inside a button.
//! - [`Style`] is the shape: control height, radii, spacing, fonts, and the few
//!   layout decisions that make a dialog read as native or as dtr.
//! - [`Skin`] is the pair, and [`Skin::egui_style`] derives `egui::Style` from it
//!   so the widgets that are still egui's own (`TextEdit`, `ScrollArea`, popup
//!   frames) share the surface with the painted ones.
//!
//! Four presets, and one honest rule about them:
//!
//! - [`Preset::Native`] is the platform's own chrome. It exists so that a window
//!   with dtr's frame still has controls a Windows user recognises. On Windows it
//!   takes its colours from the system (light/dark and the user's accent); on a
//!   GNOME desktop it is Adwaita's. Where a machine has no answer it resolves to
//!   the terminal's own chrome — by decision, not by accident (`docs/ui.md`).
//! - [`Preset::Adwaita`], [`Preset::Windows`] and [`Preset::Egui`] are looks the
//!   window carries: a person can ask for GNOME's chrome on Windows, Windows' on
//!   Linux, or egui's plain greys on either. They are the three with a [`Scheme`]
//!   and an accent of their own ([`Choice`]).
//!
//! **The rule**: a preset is a request. Asking for a look this machine cannot
//! read (`native` with no portal, no registry) gets the closest one it can paint
//! and says so in `-v` — never a window that quietly ignored what it was asked
//! for.

mod format;
mod palette;
mod skin;
mod style;
mod widgets;

pub use format::human_size;
pub use palette::{AccentLadder, Palette, readable_on};
pub use skin::{
    ADWAITA_ACCENT, Choice, EGUI_ACCENT_DARK, EGUI_ACCENT_LIGHT, Origin, Skin, WINDOWS_ACCENT_DARK,
    WINDOWS_ACCENT_LIGHT,
};
pub use style::{Focus, Footer, Preset, Scheme, Section, Style, Title};
pub use widgets::{
    Button, Card, CardOutput, Conflict, ConflictAnswer, ConflictDecision, ContextMenu, Edit,
    Editable, Glyph, Icon, IconTile, Job, JobDirection, JobState, LABEL_ROWS, Mark, Queue,
    QueueRows, Row as MenuRow, RowSpec, Stripe, above_the_root, check, check_box, cross, cross_at,
    icon_tile, label_cell, menu_row, menu_row_chord, menu_row_icon, menu_separator, menu_tip,
    menu_width, plus, point_at, prime_area, row, section, segmented, stripe, tabs, tile_height,
};
