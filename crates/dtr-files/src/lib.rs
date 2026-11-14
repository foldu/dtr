//! The file browser: a directory listing, a selection, and the views over them.
//!
//! One browser, three frontends. The file pane in a terminal draws it in a tab,
//! the manager is the same browser in a window of its own, and the desktop's file
//! picker is the same browser again in a third window with a footer and an
//! answer — so nothing here may know which one it is in. That is what the crate
//! boundary buys: no winit, no wgpu, no `libghostty-vt`, no terminal theme. A
//! surface hands in an `egui::Ui`, a rectangle in points, and a
//! [`dtr_widgets::Skin`]; the browser paints and reports what a person asked for.
//!
//! The two things a caller must supply, because the browser cannot know them:
//!
//! - **The skin**, not a look. A terminal resolves its look from a theme file and
//!   the desktop's own palette; the picker resolves the same skin without a
//!   terminal. Both are `dtr_widgets::Skin` by the time a browser sees them.
//! - **A rectangle in points**, already converted from physical pixels. The
//!   terminal lays panes out in physical pixels and knows the scale; a window's
//!   client area is in points already.
//!
//! `docs/manager.md` is the design: the grid, the streamed listing, the sort, and
//! the order the work lands in.

mod browser;

pub use browser::{
    Browser, Entry, Key, PaneView, arrange, clamp_selection, display_join, display_path,
    first_visible, human_size, pane_ui, parent_of,
};
