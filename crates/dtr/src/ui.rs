//! The chrome around the grid: the status strip, and the colours both it and
//! the renderer use.
//!
//! The grid itself is drawn by `render.rs` with wgpu; this module is what is
//! left of the M0 egui path — the parts that are genuinely small enough not to
//! need a renderer of their own.

use std::path::PathBuf;

use egui::{Color32, RichText, Ui};

/// How the grid is drawn: the font size, and an optional font file to load in
/// place of the embedded default.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// Installed family the grid should use, before the preferred list.
    pub family: Option<String>,
    pub size: f32,
    pub path: Option<PathBuf>,
    /// Families to try before dtr's own list when the grid face lacks a glyph,
    /// from `[font] fallback` in the config.
    pub fallback: Vec<String>,
    /// The terminal's own background, used behind the grid and as the base from
    /// which the chrome is mixed.
    pub background: [u8; 3],
    pub foreground: [u8; 3],
    /// Cursor/selection accent; the theme's cursor colour when it has one.
    pub accent: [u8; 3],
    /// Status bar and palette background: the terminal background lifted just
    /// enough to be a separate surface.
    pub surface: [u8; 3],
    /// Secondary text.
    pub dim: [u8; 3],
}

impl Default for Look {
    fn default() -> Self {
        Self {
            family: None,
            size: 14.0,
            path: None,
            fallback: Vec::new(),
            background: [0x0b, 0x0f, 0x0e],
            foreground: [0xdc, 0xdf, 0xdd],
            accent: [0xff, 0xb0, 0x00],
            surface: [0x14, 0x18, 0x16],
            dim: [0x6b, 0x77, 0x72],
        }
    }
}

impl Look {
    /// Build the chrome colours from a theme, if there is one.
    ///
    /// The chrome is not themed by any format in the wild, so it is derived:
    /// a surface slightly lighter than the background, secondary text between
    /// background and foreground, and the theme's cursor colour as the accent.
    pub fn themed(
        size: f32,
        family: Option<String>,
        path: Option<PathBuf>,
        theme: Option<&crate::theme::Theme>,
    ) -> Self {
        let mut look = Self {
            size,
            family,
            path,
            ..Self::default()
        };
        let Some(theme) = theme else { return look };

        if let Some(bg) = theme.background {
            look.background = bg;
        }
        if let Some(fg) = theme.foreground {
            look.foreground = fg;
        }
        if let Some(cursor) = theme.cursor {
            look.accent = cursor;
        } else if let Some(yellow) = theme.palette[3] {
            look.accent = yellow;
        }
        look.surface = mix(look.background, look.foreground, 0.10);
        look.dim = mix(look.background, look.foreground, 0.55);
        look
    }

    pub fn colour(rgb: [u8; 3]) -> Color32 {
        Color32::from_rgb(rgb[0], rgb[1], rgb[2])
    }
}

/// Linear blend of two colours; `t` of 0 is `a`, 1 is `b`.
fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let blend = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    [blend(a[0], b[0]), blend(a[1], b[1]), blend(a[2], b[2])]
}

impl Look {
    /// Register the configured font with egui, if there is one.
    ///
    /// Fonts are installed once, when the context is created: egui rebuilds its
    /// glyph atlas when the set changes, so doing this per frame would be a
    /// stutter on every repaint.
    ///
    /// Returns whether the platform's caption-glyph font was installed: the
    /// window buttons draw with it when it is there, and paint their shapes when
    /// it is not. Asking here, once, is what keeps a missing glyph from being a
    /// panic at draw time.
    pub fn install(&self, ctx: &egui::Context) -> bool {
        let mut definitions = egui::FontDefinitions::default();
        if let Some(path) = self.path.as_ref() {
            match std::fs::read(path) {
                Ok(bytes) => {
                    definitions.font_data.insert(
                        "dtr-mono".to_owned(),
                        std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                    );
                    definitions
                        .families
                        .entry(egui::FontFamily::Monospace)
                        .or_default()
                        .insert(0, "dtr-mono".to_owned());
                }
                Err(err) => tracing::warn!(
                    path = %path.display(),
                    %err,
                    "cannot read the configured font; keeping the default"
                ),
            }
        }
        // One CJK face for everything egui draws, appended so latin text keeps
        // its own face: egui's bundled families are latin-only and it panics on
        // a glyph none of them has, while the IME preedit and a palette query
        // are both places a person can type one.
        match crate::fonts::ui_fallback() {
            Some((name, bytes)) => {
                definitions.font_data.insert(
                    name.clone(),
                    std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                );
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    definitions
                        .families
                        .entry(family)
                        .or_default()
                        .push(name.clone());
                }
                tracing::debug!(family = %name, "ui fallback face");
            }
            None => tracing::debug!("no UI fallback face; CJK in the UI will panic"),
        }
        // The caption's own glyphs, when the platform ships them: on Windows the
        // window buttons are the OS's symbols, not a drawing of them.
        let mut caption_icons = false;
        match crate::fonts::caption_icons() {
            Some((name, bytes)) => {
                definitions.font_data.insert(
                    name.clone(),
                    std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                );
                definitions.families.insert(
                    egui::FontFamily::Name(name.clone().into()),
                    vec![name.clone()],
                );
                caption_icons = true;
                tracing::debug!(family = %name, "caption glyphs");
            }
            None => tracing::debug!("no caption-glyph font; the window buttons are painted"),
        }
        ctx.set_fonts(definitions);
        caption_icons
    }
}

/// Give an area a known rectangle before it is shown.
///
/// An area whose size egui has not seen yet spends its first frame in a
/// sizing pass, and a sizing pass paints nothing. That is invisible in a live
/// window (the second frame draws), but `--screenshot` renders exactly one
/// frame, so an unprimed area would be missing from every capture. Seeding the
/// state with the rectangle we already know skips that pass.
pub(crate) fn prime_area(
    ctx: &egui::Context,
    id: egui::Id,
    order: egui::Order,
    rect: egui::Rect,
    interactable: bool,
) {
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

/// Draw a full-screen overlay in a layer above the panes.
///
/// Panes are egui `Area`s in the background layer; the palette has to be above
/// them, or a file listing would paint over the command line it is feeding.
/// Install the image loaders once, before the first frame: the SVG loader is
/// what turns a Lucide file into a texture.
pub fn install_image_loaders(ctx: &egui::Context) {
    egui_extras::loaders::install_image_loaders(ctx);
}

pub fn overlay(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    let screen = ui.max_rect();
    let ctx = ui.ctx().clone();
    let id = egui::Id::unique(id);
    prime_area(&ctx, id, egui::Order::Foreground, screen, true);
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .constrain(false)
        .fade_in(false)
        .show(&ctx, |ui| {
            ui.set_min_size(screen.size());
            ui.set_max_size(screen.size());
            ui.set_clip_rect(screen);
            add(ui);
        });
}

/// Height of the status strip in logical points. The panes stop above it, so
/// this number is load-bearing: it is what keeps a split from running under
/// the chrome.
pub const STATUS_HEIGHT: f32 = 24.0;
/// The tab strip across the top, shown only when there is more than one tab.
/// Tabs existed before this and were invisible, which made `ctrl+tab` look
/// broken rather than unused.
pub const TAB_HEIGHT: f32 = 30.0;
/// The chip inside the strip. The strip is taller than this by design: a chip
/// that fills its panel makes any rounding in egui's layout paint over the first
/// terminal row.
pub const TAB_CHIP_HEIGHT: f32 = 20.0;
/// Breathing room between the last row of text and the status strip. Without
/// it the grid ends exactly on the strip's edge, and a prompt with a descender
/// looks like it is behind the bar rather than above it.
pub const GRID_GAP: f32 = 4.0;

/// Gap between split panes, in logical points.
pub const SPLIT_GAP: f32 = 3.0;

/// What the status bar shows. Totals come from the pane; rates are the UI's own
/// measurement, because it is the UI's clock that decides when to repaint.
pub struct Status<'a> {
    pub label: &'a str,
    /// `(index, count)` of the session tabs; `(1, 1)` is drawn without a tab
    /// marker, because one session is not a tab bar.
    pub tabs: (usize, usize),
    pub rx: u64,
    pub tx: u64,
    pub rx_rate: f64,
    pub tx_rate: f64,
    /// False while the transport is gone and something is trying to get it back.
    pub connected: bool,
    /// DTR and RTS levels, when the transport has modem lines at all.
    pub modem: Option<(bool, bool)>,
    /// Rows of scrollback below the viewport; 0 is the live screen.
    pub scrollback: u64,
    /// Whether byte counters mean anything for this transport. A shell in a pty
    /// is not a serial line: its byte totals are noise, and the strip is scarce
    /// space next to a filename or a path.
    pub counters: bool,
}

/// A short-lived note in the corner: "copied 1.2 kB".
///
/// Deliberately small and out of the way. A toast is the answer to "did that
/// work?", asked at the moment the answer is still useful; anything larger
/// gets in the way of the thing you just copied from.
#[allow(deprecated)] // one fixed overlay: the id is a name, not a hash of state
pub fn toast(ui: &mut Ui, look: &Look, text: &str, alpha: f32) {
    let corner = ui.ctx().content_rect().right_bottom();
    let id = egui::Id::new("dtr-toast");
    let rect = egui::Rect::from_min_size(
        egui::pos2(corner.x - 260.0, corner.y - 64.0),
        egui::vec2(240.0, 26.0),
    );
    // A fresh Area spends its first frame measuring and paints nothing, which
    // makes a one-frame capture of it blank. Prime the state with the rect.
    prime_area(ui.ctx(), id, egui::Order::Foreground, rect, false);
    egui::Area::new(id)
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(corner.x - 260.0, corner.y - 64.0))
        .interactable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                // Mixed between the theme's surface and its text, so the note
                // reads as chrome rather than as terminal output — and with
                // enough contrast to be read at a glance, which is the only
                // way a toast is ever read.
                .fill(Color32::from_rgba_unmultiplied(
                    look.foreground[0],
                    look.foreground[1],
                    look.foreground[2],
                    (36.0 * alpha) as u8,
                ))
                .stroke(egui::Stroke::new(
                    1.0,
                    Color32::from_rgba_unmultiplied(
                        look.accent[0],
                        look.accent[1],
                        look.accent[2],
                        (200.0 * alpha) as u8,
                    ),
                ))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.label(RichText::new(text).monospace().size(13.0).strong().color(
                        Color32::from_rgba_unmultiplied(
                            look.foreground[0],
                            look.foreground[1],
                            look.foreground[2],
                            (255.0 * alpha) as u8,
                        ),
                    ));
                });
        });
}

/// The tab strip: one chip per tab, the active one lit, each clickable.
///
/// Drawing and hit-testing are the same rectangle on purpose — a click that
/// misses the chip the eye sees is the classic way to make tabs feel broken.
/// What a click in the tab strip asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabHit {
    Select(usize),
    Close(usize),
    /// Open a new tab: `None` is the local shell, `Some(destination)` is a
    /// serial port or an ssh host. The destination travels *typed*: the app
    /// used to open every non-`None` answer as `ssh`, which ran
    /// `ssh /dev/ttyUSB0` when the entry picked was a serial port.
    New(Option<crate::sources::Destination>),
}

/// What the strip drew, and how much of the row it took.
///
/// The rectangle is not decoration. With a client-side frame the rest of the
/// strip is the window's drag region, and "the rest" has to start exactly where
/// the chips stop — otherwise a press on a chip moves the window, which is the
/// overlapping-hit-targets bug in a new place.
#[derive(Clone, Debug)]
pub struct TabStrip {
    pub hit: Option<TabHit>,
    pub used: egui::Rect,
}

pub fn tab_bar(
    ui: &mut Ui,
    labels: &[String],
    active: usize,
    destinations: &[crate::sources::Destination],
    look: &Look,
) -> TabStrip {
    // Idempotent, and here rather than only at startup so every caller — the
    // window, a capture, a kittest harness — gets a working loader.
    install_image_loaders(ui.ctx());
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (index, label) in labels.iter().enumerate() {
            let is_active = index == active;
            let text = format!("{} {}", index + 1, label);
            let fg = if is_active {
                Look::colour(look.background)
            } else {
                Look::colour(look.foreground)
            };
            let galley = ui
                .painter()
                .layout_no_wrap(text, egui::FontId::monospace(12.0), fg);
            // Two hit targets in one chip: the label switches, the cross closes.
            // A keyboard-focusable widget here would be activated by Enter and
            // by Space — pressing Enter in the shell must not touch the strip,
            // so both are pointer-only.
            //
            // The two targets have to be **disjoint rects**, not two sensors over
            // the same chip. Overlapping click targets make egui's topmost-widget
            // rule decide who gets the press, and the deciding factor is the
            // allocation order — which reads as a button that works only some of
            // the time. The chip itself only reserves space and senses hover.
            let close_size = TAB_CHIP_HEIGHT - 4.0;
            let size = egui::vec2(galley.size().x + 18.0 + close_size, TAB_CHIP_HEIGHT);
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());

            let close_rect = egui::Rect::from_center_size(
                egui::pos2(rect.right() - 8.0 - close_size / 2.0, rect.center().y),
                egui::vec2(close_size, close_size),
            );
            let label_rect =
                egui::Rect::from_min_max(rect.min, egui::pos2(close_rect.left() - 4.0, rect.max.y));
            let response = ui.interact(
                label_rect,
                egui::Id::unique(("dtr-tab", index)),
                egui::Sense::click(),
            );
            let close = ui.interact(
                close_rect,
                egui::Id::unique(("dtr-tab-close", index)),
                egui::Sense::click(),
            );

            // Hover is the strip's own affordance: a chip that looks the same
            // under the pointer as away from it reads as decoration.
            let hovered = response.hovered() || close.hovered();
            let fill = match (is_active, hovered) {
                (true, true) => Look::colour(look.accent).gamma_multiply(1.15),
                (true, false) => Look::colour(look.accent),
                (false, true) => Look::colour(look.surface).gamma_multiply(1.6),
                (false, false) => Look::colour(look.surface),
            };
            let painter = ui.painter();
            painter.rect_filled(rect, 4.0, fill);
            painter.galley(
                egui::pos2(rect.left() + 7.0, rect.center().y - galley.size().y / 2.0),
                galley,
                fg,
            );
            // Lucide's `x`, fetched by the lucide-icons skill and embedded, so
            // the one-file build still has it. Tinted rather than recoloured:
            // the SVG keeps `currentColor` and stays byte-identical upstream.
            // `paint_at`, not `ui.put`: an image added as a widget is another
            // click target sitting on top of the cross, and it wins the press
            // half the time. Painting cannot sense anything.
            egui::Image::new(egui::include_image!("../assets/icons/x.svg"))
                .fit_to_exact_size(close_rect.shrink(2.0).size())
                .tint(fg)
                .paint_at(ui, close_rect.shrink(2.0));

            // Painted by hand, so the accessibility nodes are declared by hand
            // too — otherwise a screen reader (and the test harness) sees a
            // strip with no tabs in it at all.
            let name = format!("{} {}", index + 1, &labels[index]);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
            let close_name = format!("close tab {}", index + 1);
            close.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &close_name));

            // `Sense::click()` is not enough to be pointer-only: egui activates
            // any focused click-sensing widget on Enter, so a chip that once had
            // focus turned the next Enter in the shell into a tab switch — the
            // log said `active tab why="chip clicked"` with no mouse involved.
            // A keyboard "click" happens with the pointer elsewhere, so hover is
            // the test that separates them; the focus is surrendered as well so
            // the chip cannot hold it in the first place.
            for stolen in [&close, &response] {
                if stolen.has_focus() {
                    stolen.surrender_focus();
                }
            }
            let pointer = ui.input(|i| i.pointer.any_click());
            if close.clicked() && pointer {
                clicked = Some(TabHit::Close(index));
            } else if response.clicked() && pointer {
                clicked = Some(TabHit::Select(index));
            }
        }

        // The new-tab button. Its menu is where a terminal's destinations live:
        // the local shell first, then the serial ports and the ssh hosts, each
        // group behind a separator. Pointer only, like the chips, so Enter in
        // the shell never opens a menu.
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(TAB_CHIP_HEIGHT + 4.0, TAB_CHIP_HEIGHT),
            egui::Sense::click(),
        );
        let fill = if response.hovered() {
            Look::colour(look.surface).gamma_multiply(1.6)
        } else {
            Look::colour(look.surface)
        };
        ui.painter().rect_filled(rect, 4.0, fill);
        // Painted, for the same reason as the cross: a widget here would sit on
        // top of the button and eat its clicks.
        egui::Image::new(egui::include_image!("../assets/icons/plus.svg"))
            .fit_to_exact_size(rect.shrink(4.0).size())
            .tint(Look::colour(look.foreground))
            .paint_at(ui, rect.shrink(4.0));
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "new tab"));

        // Same trap as the chips: a focused click-sensing widget is activated by
        // Enter, and the popup opens on a click. The fake click is cleared so
        // only the pointer opens the menu, and the focus is given back.
        if response.has_focus() {
            response.surrender_focus();
        }
        let mut pointer_only = response;
        pointer_only
            .flags
            .remove(egui::response::Flags::FAKE_PRIMARY_CLICKED);
        let menu = egui::Popup::menu(&pointer_only).show(|ui| {
            ui.set_min_width(220.0);
            if ui.button("local shell").clicked() {
                clicked = Some(TabHit::New(None));
                ui.close();
            }
            // The groups are separated, not just prefixed. `serial …` and
            // `ssh …` are different verbs that lead to different panes, and a
            // run of `/dev/tty…` rows with an `ssh` row in the middle is one
            // where the pointer lands on the wrong half of the menu. A group
            // that is empty takes its separator with it, so a machine with no
            // serial ports looks like it always did.
            for serial in [true, false] {
                let group: Vec<&crate::sources::Destination> = destinations
                    .iter()
                    .filter(|destination| destination.is_serial() == serial)
                    .collect();
                if group.is_empty() {
                    continue;
                }
                ui.separator();
                for destination in group {
                    if ui.button(destination.label()).clicked() {
                        clicked = Some(TabHit::New(Some(destination.clone())));
                        ui.close();
                    }
                }
            }
            if destinations.is_empty() {
                ui.label(
                    egui::RichText::new("no serial ports and no ssh hosts")
                        .size(11.0)
                        .color(Look::colour(look.dim)),
                );
            }
        });
        let _ = menu;
    });
    // What the horizontal actually took, so a drag region can start after it.
    // Taken from the ui rather than computed from the chips: the `+` is part of
    // the strip too, and a drag that started on it would be a drag on a menu.
    let used = ui.min_rect();
    TabStrip { hit: clicked, used }
}

pub fn status_bar(ui: &mut Ui, status: &Status, look: &Look) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        if status.tabs.1 > 1 {
            ui.label(
                RichText::new(format!("{}/{}", status.tabs.0, status.tabs.1))
                    .monospace()
                    .size(11.0)
                    .color(Look::colour(look.accent)),
            );
        }
        ui.label(
            RichText::new(status.label)
                .monospace()
                .size(12.0)
                .color(Look::colour(look.foreground).gamma_multiply(0.8)),
        );
        if let Some((dtr, rts)) = status.modem {
            for (label, level) in [("DTR", dtr), ("RTS", rts)] {
                ui.label(
                    RichText::new(format!("{label}{}", if level { "●" } else { "○" }))
                        .monospace()
                        .size(11.0)
                        .color(if level {
                            Look::colour(look.accent)
                        } else {
                            Look::colour(look.dim)
                        }),
                );
            }
        }
        if status.scrollback > 0 {
            ui.label(
                RichText::new(format!("{} lines up", status.scrollback))
                    .monospace()
                    .size(11.0)
                    .color(Look::colour(look.accent)),
            );
        }
        if !status.connected {
            ui.label(
                RichText::new("reconnecting…")
                    .monospace()
                    .size(12.0)
                    .color(Look::colour(look.accent)),
            );
        }
        if !status.counters {
            return;
        }
        ui.label(
            RichText::new(format!(
                "RX {} ({}/s)   TX {} ({}/s)",
                human_bytes(status.rx as f64),
                human_bytes(status.rx_rate),
                human_bytes(status.tx as f64),
                human_bytes(status.tx_rate),
            ))
            .monospace()
            .size(12.0)
            .color(Look::colour(look.dim)),
        );
    });
}

fn human_bytes(value: f64) -> String {
    const K: f64 = 1024.0;
    if !value.is_finite() || value < 0.0 {
        return "—".to_owned();
    }
    if value < K {
        format!("{value:.0} B")
    } else if value < K * K {
        format!("{:.1} kB", value / K)
    } else {
        format!("{:.1} MB", value / (K * K))
    }
}

#[cfg(test)]
mod kittest_tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    /// The new-tab button offers the destinations a terminal has, each kind
    /// behind its own separator: the local shell, the serial ports, the hosts.
    ///
    /// The grouping is also the *type*, and that is the point. The menu used to
    /// decide "serial" by whether the destination began with `/`, and the app
    /// then opened every entry as `ssh` — so picking a port ran
    /// `ssh /dev/ttyUSB0`, and a `Host` someone spelled `prod/db` would have
    /// been run as a port. Both are checked here.
    #[test]
    fn the_new_tab_menu_offers_the_local_shell_and_each_kind_of_destination() {
        use crate::sources::Destination;

        let look = Look::default();
        let clicked = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = clicked.clone();
        let labels = vec!["demo".to_owned()];
        let destinations = vec![
            Destination::Serial {
                path: "/dev/ttyUSB0".to_owned(),
                detail: "FTDI".to_owned(),
            },
            Destination::Ssh {
                alias: "web".to_owned(),
                detail: "alice@10.0.0.2".to_owned(),
            },
            Destination::Ssh {
                alias: "prod/db".to_owned(),
                detail: String::new(),
            },
        ];
        let mut harness = Harness::new_ui(move |ui| {
            if let Some(hit) = tab_bar(ui, &labels, 0, &destinations, &look).hit {
                *sink.borrow_mut() = Some(hit);
            }
        });
        harness.run();

        harness.get_by_label("new tab").click();
        harness.run();
        harness.get_by_label("local shell").click();
        harness.run();
        assert_eq!(
            clicked.borrow().clone(),
            Some(TabHit::New(None)),
            "the first entry is the local shell"
        );

        *clicked.borrow_mut() = None;
        harness.get_by_label("new tab").click();
        harness.run();
        harness.get_by_label("serial /dev/ttyUSB0  FTDI").click();
        harness.run();
        assert_eq!(
            clicked.borrow().clone(),
            Some(TabHit::New(Some(Destination::Serial {
                path: "/dev/ttyUSB0".to_owned(),
                detail: "FTDI".to_owned(),
            }))),
            "a serial port opens that port"
        );

        *clicked.borrow_mut() = None;
        harness.get_by_label("new tab").click();
        harness.run();
        harness.get_by_label("ssh web  alice@10.0.0.2").click();
        harness.run();
        assert_eq!(
            clicked.borrow().clone(),
            Some(TabHit::New(Some(Destination::Ssh {
                alias: "web".to_owned(),
                detail: "alice@10.0.0.2".to_owned(),
            }))),
            "a host opens that host"
        );

        *clicked.borrow_mut() = None;
        harness.get_by_label("new tab").click();
        harness.run();
        harness.get_by_label("ssh prod/db").click();
        harness.run();
        assert!(
            matches!(
                clicked.borrow().clone(),
                Some(TabHit::New(Some(Destination::Ssh { .. })))
            ),
            "an alias with a slash in it is still an ssh host, not a port"
        );
    }

    /// The menu with both groups in it: local shell, the ports, the hosts.
    ///
    /// A separator is a painted line — no accessibility node, nothing the
    /// queries above can see — so a picture is the only proof the grouping is
    /// drawn at all rather than merely decided. `UPDATE_SNAPSHOTS=true cargo
    /// test` writes the baseline; the plain run compares against it.
    #[test]
    fn the_new_tab_menu_separates_its_groups() {
        use crate::sources::Destination;
        let look = Look::default();
        let labels = vec!["demo".to_owned()];
        let destinations = vec![
            Destination::Serial {
                path: "/dev/ttyUSB0".to_owned(),
                detail: "FTDI".to_owned(),
            },
            Destination::Ssh {
                alias: "web".to_owned(),
                detail: "alice@10.0.0.2".to_owned(),
            },
            Destination::Ssh {
                alias: "prod/db".to_owned(),
                detail: String::new(),
            },
        ];
        let mut harness = Harness::new_ui(move |ui| {
            tab_bar(ui, &labels, 0, &destinations, &look);
        });
        harness.run();

        harness.get_by_label("new tab").click();
        harness.run();
        harness.snapshot("new_tab_menu");
    }

    /// Enter belongs to the terminal, even after a chip was clicked.
    ///
    /// egui activates any focused click-sensing widget on Enter, and a chip that
    /// took focus turned the next Enter in the shell into a tab switch. The log
    /// said `active tab why="chip clicked"` with no mouse anywhere near it.
    #[test]
    fn enter_after_a_click_does_not_press_a_chip_again() {
        let look = Look::default();
        let labels = vec!["demo".to_owned(), "shell".to_owned()];
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = std::rc::Rc::clone(&seen);
        let mut harness = Harness::new_ui(move |ui| {
            if let Some(hit) = tab_bar(ui, &labels, 1, &[], &look).hit {
                sink.borrow_mut().push(hit);
            }
        });

        harness.get_by_label("1 demo").click();
        harness.run();
        assert_eq!(seen.borrow().len(), 1, "the click itself switches");
        seen.borrow_mut().clear();

        // kittest's click does not leave the chip focused, but egui keeps focus
        // from a real click indefinitely, so put it there by hand.
        harness.ctx.memory_mut(|memory| {
            memory.request_focus(egui::Id::unique(("dtr-tab", 0usize)));
        });
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert!(
            seen.borrow().is_empty(),
            "Enter must reach the terminal, not the focused chip: {:?}",
            seen.borrow()
        );
    }

    /// Hover is the strip's only feedback, and it is painted by hand: a
    /// regression in the fill is invisible to every other test, because the
    /// pointer is never over the strip in them.
    #[test]
    fn a_chip_lights_up_under_the_pointer() {
        let look = Look::default();
        let labels = vec!["demo".to_owned(), "shell".to_owned()];
        let mut harness = Harness::new_ui(move |ui| {
            tab_bar(ui, &labels, 1, &[], &look);
        });
        harness.run();

        // Away from the strip entirely: the state every other snapshot has.
        harness.get_by_label("1 demo").hover();
        harness.run();
        harness.snapshot("tab_bar_hover");
    }

    /// The strip's whole contract, without a window: a chip switches, its cross
    /// closes, and a key that belongs to the terminal touches neither.
    ///
    /// The second half is a regression test. The chips were `egui::Button`s once,
    /// and an egui button takes keyboard focus, so Enter — pressed a hundred
    /// times a minute in a terminal — activated whichever chip had it.
    #[test]
    fn a_chip_is_clicked_by_the_pointer_and_ignores_enter() {
        let labels = vec!["demo".to_owned(), "shell".to_owned()];
        let look = Look::default();
        // The closure runs on every step, so the answer is shared state rather
        // than a captured mutable borrow.
        let clicked = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = clicked.clone();
        let mut harness = Harness::new_ui(move |ui| {
            // Icons are SVG: the loader has to exist before the first frame.
            // (Installed per harness, not per process — each has its own ctx.)
            // Accumulate: a click is reported on the frame it happens and the
            // next frame starts from `None`.
            if let Some(index) = tab_bar(ui, &labels, 0, &[], &look).hit {
                *sink.borrow_mut() = Some(index);
            }
        });
        install_image_loaders(&harness.ctx);
        harness.run();
        assert_eq!(
            *clicked.borrow(),
            None,
            "drawing alone must not switch tabs"
        );

        harness.get_by_label("2 shell").click();
        harness.run();
        assert_eq!(
            *clicked.borrow(),
            Some(TabHit::Select(1)),
            "the pointer switches tabs"
        );

        // The cross on the chip closes it, and asks to close the tab it belongs
        // to rather than the one in front.
        *clicked.borrow_mut() = None;
        harness.get_by_label("close tab 2").click();
        harness.run();
        assert_eq!(
            *clicked.borrow(),
            Some(TabHit::Close(1)),
            "the cross closes the tab it is on"
        );

        // Enter is the shell's key. It must not reach the chips.
        *clicked.borrow_mut() = None;
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert_eq!(
            *clicked.borrow(),
            None,
            "Enter must not activate a tab chip"
        );
    }

    /// A visual record of the strip: two chips, the second one active.
    ///
    /// `UPDATE_SNAPSHOTS=true cargo test` writes the baseline; the default run
    /// compares against it.
    #[test]
    fn the_strip_renders() {
        let labels = vec!["demo".to_owned(), "shell".to_owned()];
        let look = Look::default();
        let mut harness = Harness::new_ui(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            tab_bar(ui, &labels, 1, &[], &look);
        });
        harness.run();
        harness.snapshot("tab_bar");
    }

    /// Every embedded image has a decoder behind it.
    ///
    /// `include_image!` with no matching egui_extras feature does not fail to
    /// compile; it fails at *paint* time, and egui's whole report is the ⚠ it
    /// draws in the image's place — which sat in the corner of the title bar,
    /// at 16 points, looking like the logo. The tab icons are SVG, so installing
    /// only the SVG loader was enough for them and for nothing else.
    ///
    /// The decode itself happens on a worker thread, so this polls: the first
    /// answer is `Pending` for any image that has a loader at all, and only the
    /// absence of one is an immediate error.
    #[test]
    fn every_embedded_image_decodes() {
        let ctx = egui::Context::default();
        install_image_loaders(&ctx);
        let size = egui::load::SizeHint::Size {
            width: 16,
            height: 16,
            maintain_aspect_ratio: true,
        };
        for source in [
            egui::include_image!("../../../assets/logo/dtr-icon-32.png"),
            egui::include_image!("../assets/icons/minus.svg"),
        ] {
            let uri = source.uri().expect("embedded, so it has a uri").to_owned();
            if let Err(err) = source.load(&ctx, egui::TextureOptions::LINEAR, size) {
                panic!("{uri} has no loader: {err}");
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                match ctx.try_load_texture(&uri, egui::TextureOptions::LINEAR, size) {
                    Ok(egui::load::TexturePoll::Ready { .. }) => break,
                    Ok(egui::load::TexturePoll::Pending { .. }) => {
                        assert!(std::time::Instant::now() < deadline, "{uri} never decoded");
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(err) => panic!("{uri} did not decode: {err}"),
                }
            }
        }
    }
}
