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
//! Two presets, and one honest rule about them:
//!
//! - [`Preset::Dtr`] is the terminal's own chrome — monospace labels, a small
//!   accent, tight rows.
//! - [`Preset::Native`] is the platform's own chrome. It exists so that a window
//!   with dtr's frame still has controls a Windows user recognises. On Windows it
//!   takes its colours from the system (light/dark and the user's accent); on
//!   Linux there is no portable system palette, so `native` resolves to the dtr
//!   style — by decision, not by accident (`docs/ui.md`).

mod palette;
mod skin;
mod style;
mod widgets;

pub use palette::Palette;
pub use skin::Skin;
pub use style::{Focus, Footer, Preset, Section, Style, Title};
pub use widgets::{Button, Edit, Editable, check, label_cell, menu_row, row, section, segmented};
