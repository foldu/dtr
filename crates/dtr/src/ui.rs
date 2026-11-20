//! The chrome around the grid: the status strip, and the colours both it and
//! the renderer use.
//!
//! The grid itself is drawn by `render.rs` with wgpu; this module is what is
//! left of the M0 egui path — the parts that are genuinely small enough not to
//! need a renderer of their own.

use std::path::PathBuf;

use dtr_widgets::Preset;
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
    /// The glyph a **block** cursor sits on is drawn in this, over `accent`.
    /// The theme's `cursor-text`, and its background when it has none — which
    /// is Ghostty's default, and the only value that reads on every cursor
    /// colour.
    pub cursor_text: [u8; 3],
    /// Status bar and palette background: the terminal background lifted just
    /// enough to be a separate surface.
    pub surface: [u8; 3],
    /// Secondary text.
    pub dim: [u8; 3],
    /// Which chrome to draw: the look, its scheme and its accent.
    pub choice: dtr_widgets::Choice,
}

impl Default for Look {
    fn default() -> Self {
        Self {
            family: None,
            size: 14.0,
            path: None,
            fallback: Vec::new(),
            background: crate::theme::DEFAULT_BACKGROUND,
            foreground: [0xdc, 0xdf, 0xdd],
            accent: [0xff, 0xb0, 0x00],
            cursor_text: crate::theme::DEFAULT_BACKGROUND,
            surface: [0x14, 0x18, 0x16],
            dim: [0x6b, 0x77, 0x72],
            choice: dtr_widgets::Choice::default(),
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
        choice: dtr_widgets::Choice,
    ) -> Self {
        let mut look = Self {
            size,
            family,
            path,
            choice,
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
        // The glyph under a block cursor: the theme's `cursor-text`, and its
        // background when it has none. Read here, after `background`, because
        // the default is *that* colour and not the built-in one.
        look.cursor_text = theme.cursor_text.unwrap_or(look.background);
        look.surface = mix(look.background, look.foreground, 0.10);
        look.dim = mix(look.background, look.foreground, 0.55);
        look
    }

    /// The look a config file describes.
    ///
    /// One place, because "which keys make the window" has three readers — the
    /// window at startup, the preferences card's preview and its save — and two of
    /// them disagreeing is a save that lands somewhere the sheet never showed.
    pub fn from_config(config: &dtr_config::Config, theme: Option<&crate::theme::Theme>) -> Self {
        let mut look = Self::themed(
            config.font.size,
            config.font.family.clone(),
            config.font.path.clone(),
            theme,
            Self::choice_of(config),
        );
        look.fallback = config.font.fallback.clone();
        look
    }

    /// The chrome's skin, derived from these colours on demand.
    ///
    /// Derived rather than stored because the colours are the theme's: a stored
    /// skin would have to be rebuilt when the theme changes, and the one time
    /// somebody forgets is the frame the window looks like the old theme. A
    /// palette is a few dozen mixes, which is nothing next to a frame.
    ///
    /// `native` resolves to the platform's palette where this crate can read one
    /// and to `dtr` everywhere else — see `crate::system`, which reads it once and
    /// then keeps it current when the desktop changes it.
    pub fn skin(&self) -> dtr_widgets::Skin {
        self.choice.skin(
            dtr_widgets::Palette::dtr(self.background, self.foreground, self.accent),
            crate::system::native_skin(),
        )
    }

    /// The choice a config file asks for.
    ///
    /// The config crate has no window library, so its `[ui]` enums are its own and
    /// this is where they become the crate that paints. One place, because a
    /// second mapping is a second set of answers to "what does `windows` mean".
    pub fn choice_of(config: &dtr_config::Config) -> dtr_widgets::Choice {
        use dtr_config::{Scheme, Widgets};
        let preset = match config.ui.widgets {
            Widgets::Native => Preset::Native,
            Widgets::Adwaita => Preset::Adwaita,
            Widgets::Windows => Preset::Windows,
            Widgets::Egui => Preset::Egui,
        };
        dtr_widgets::Choice {
            preset,
            scheme: match config.ui.scheme {
                Scheme::System => dtr_widgets::Scheme::System,
                Scheme::Light => dtr_widgets::Scheme::Light,
                Scheme::Dark => dtr_widgets::Scheme::Dark,
            },
            accent: config
                .ui
                .accent
                .map(|[r, g, b]| egui::Color32::from_rgb(r, g, b)),
        }
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
pub(crate) use dtr_widgets::prime_area;

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
/// The smallest a chip gets, in points. The strip is taller than this by design:
/// a chip that fills its panel makes any rounding in egui's layout paint over the
/// first terminal row. How tall it actually is in a given strip is
/// [`tab_chip_height`].
pub const TAB_CHIP_HEIGHT: f32 = 20.0;

/// How tall a tab chip is in a strip `title_height` points tall.
///
/// The chip keeps the strip's air proportional: six points above and below it,
/// which is what a 30-point tab row had and what Windows' 31-pixel caption band
/// gives — and no taller than Adwaita's own tab, `tabbar tab { min-height: 26px }`,
/// because a pill that filled a 46-point title bar would read as a button. So the
/// Windows chip does not change and the GNOME one grows with the bar.
pub fn tab_chip_height(title_height: f32) -> f32 {
    (title_height - 12.0).clamp(TAB_CHIP_HEIGHT, 26.0)
}
/// How much room a chip's *text* may take before it is elided, in points.
///
/// A title is whatever the session called itself: a working directory, a serial
/// device path, an ssh alias with a command after it. Unbounded, one tab on the
/// path you happen to be in pushes every other tab — and the `+` — off the
/// strip, so the strip stops being a list and becomes one tab with its tail
/// cut off by the window edge. The full name stays in the tooltip and in the
/// accessibility node; only the painted line is shortened.
pub const TAB_LABEL_MAX_WIDTH: f32 = 180.0;
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
    /// DTR and RTS levels, when the transport has modem lines at all.
    pub modem: Option<(bool, bool)>,
    /// Rows of scrollback below the viewport; 0 is the live screen.
    pub scrollback: u64,
    /// Whether byte counters mean anything for this transport. A shell in a pty
    /// is not a serial line: its byte totals are noise, and the strip is scarce
    /// space next to a filename or a path.
    pub counters: bool,
    /// The period delayed input is spacing serial keystrokes by, when the mode is
    /// on and the focused pane is a serial line; `None` draws no chip.
    pub key_delay: Option<std::time::Duration>,
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
            let skin = look.skin();
            let palette = skin.palette;
            let style = &skin.style;
            egui::Frame::new()
                // A raised card rather than a wash over the grid: a note that
                // let the terminal show through was unreadable over exactly the
                // output it was reporting on.
                .fill(faded(palette.card, alpha))
                .stroke(egui::Stroke::new(
                    style.border,
                    faded(palette.border_strong, alpha),
                ))
                .corner_radius(style.chip_radius)
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(text)
                            .font(egui::FontId::new(
                                style.value_font.size + 1.0,
                                style.value_font.family.clone(),
                            ))
                            .color(faded(palette.text, alpha)),
                    );
                });
        });
}

/// A palette colour at the toast's current opacity.
fn faded(colour: Color32, alpha: f32) -> Color32 {
    let [r, g, b, _] = colour.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, (255.0 * alpha.clamp(0.0, 1.0)) as u8)
}

/// One pane whose connection is not there, for the frame to draw.
///
/// The rectangle is physical pixels, like a file pane's: the grid is laid out in
/// those, and egui is not.
pub struct Waiting {
    pub id: crate::layout::LeafId,
    pub rect: crate::layout::Rect,
    pub retry: crate::pane::Retry,
    /// Whether the notice has been folded away to its bar. The app owns this,
    /// because it outlives a frame and a press is not the only thing that sets
    /// it — a wheel notch does too.
    pub folded: bool,
}

/// What a press on the notice asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnectAsk {
    /// Try now, rather than at the end of the wait.
    Now,
    /// Give up. Closing the tab is how a person has always given up on a
    /// connection that will not come back.
    Close,
    /// Fold the notice away to its bar, so the screen underneath can be read,
    /// selected and scrolled. A press anywhere on it, or a wheel notch.
    Fold,
    /// Unfold it again from the bar.
    Unfold,
}

/// Draw the notice over a pane whose connection is not there.
///
/// Two shapes, one state. **Covered**, the notice *is* the pane: a terminal whose
/// transport is gone has nothing to show, so its whole rectangle is repainted in
/// the terminal's own background and the state is written in the middle of it —
/// a card floating in a dimmed screen would claim something else, that the
/// terminal is still there with something on top of it. **Folded**, it is a bar
/// across the top of the pane: one line of chrome, and the screen underneath is
/// the terminal's again.
///
/// The two exist because those are the two things a person wants, in that order.
/// A drop is loud enough to be noticed, and then it has to get out of the way:
/// the last screen before a drop is usually the thing one wants to read, select
/// and copy — the stack trace, the command that was running — and a notice that
/// covered it until the host came back would make an outage into a data loss.
/// So anything that means "show me the screen" — a press anywhere on the notice,
/// or a wheel notch over it — folds it, and the bar folds back open when pressed.
///
/// Both shapes are drawn in the middle layer: above the grid and a file pane next
/// door, below every modal. Covered, the pointer and the wheel belong to the
/// notice, because nothing under it can be aimed at; folded, only the bar does,
/// and everything else in the pane is the terminal's again.
///
/// Returns what was pressed, if any.
pub fn reconnect(ui: &mut Ui, view: &Waiting, look: &Look) -> Option<ReconnectAsk> {
    let pixels = ui.ctx().pixels_per_point();
    let rect = egui::Rect::from_min_size(
        egui::pos2(view.rect.x / pixels, view.rect.y / pixels),
        egui::vec2(view.rect.width / pixels, view.rect.height / pixels),
    );
    let (title, line) = headings(&view.retry);
    if view.folded {
        folded(ui, view, rect, look, title, &line)
    } else {
        // A pane with no room for a block and its buttons keeps its blank grid
        // rather than a notice with its own way out cut off — and then there is
        // nothing to fold, so the bar is what it gets instead.
        if rect.width() < 160.0 || rect.height() < 120.0 {
            return folded(ui, view, rect, look, title, &line);
        }
        covered(ui, view, rect, look, title, &line)
    }
}

/// The whole pane: the terminal's surface, with the state in the middle of it.
fn covered(
    ui: &mut Ui,
    view: &Waiting,
    rect: egui::Rect,
    look: &Look,
    title: &str,
    line: &str,
) -> Option<ReconnectAsk> {
    let retry = &view.retry;
    let skin = look.skin();
    let palette = skin.palette;
    let style = skin.style.clone();
    let label = egui::FontId::new(style.label_font.size - 1.0, style.label_font.family.clone());
    let wrap = (rect.width() - 2.0 * NOTICE_MARGIN).max(120.0);
    // A short pane keeps the rows that matter — what it is, what it is doing, the
    // way out and the way back — and drops the detail.
    let roomy = rect.height() >= 170.0;
    let roomiest = rect.height() >= 210.0;
    // The block is measured, not guessed: the rows use three fonts and two
    // buttons, and a block sized by eye is a block whose centring is off by the
    // width of a word. `layout` is the wrapping the labels will do and
    // `Button::measured_width` is the width the button will take, so these are
    // the numbers the drawing will produce.
    let spacing = ui.spacing().item_spacing;
    let mut block = Block::default();
    {
        let painter = ui.painter();
        let head = painter.layout(
            title.to_owned(),
            style.title_font.clone(),
            palette.text,
            wrap,
        );
        let host = painter.layout(retry.what.clone(), label.clone(), palette.dim, wrap);
        block.row_of(
            head.size().x + spacing.x + host.size().x,
            head.size().y.max(host.size().y),
        );
        block.row(&painter.layout(
            line.to_owned(),
            style.value_font.clone(),
            palette.text,
            wrap,
        ));
        if roomy && let Some(dir) = &retry.dir {
            block.row(&painter.layout(
                format!("reopening in {dir}"),
                label.clone(),
                palette.dim,
                wrap,
            ));
        }
        if roomiest && let Some(why) = &retry.why {
            block.row(&painter.layout(why.clone(), label.clone(), palette.dim, wrap));
        }
        block.row(&painter.layout(FOLD_HINT.to_owned(), label.clone(), palette.dim, wrap));
        // The way out and the way back, as wide as the widget says they are.
        let mut buttons = dtr_widgets::Button::measured_width(ui, &skin, "close tab");
        if matches!(retry.when, crate::pane::When::At(_)) {
            buttons += spacing.x + dtr_widgets::Button::measured_width(ui, &skin, "retry now");
        }
        block.row_of(buttons, style.row);
    }
    block.gaps(spacing.y);
    let (width, height) = (block.width.min(wrap).max(120.0), block.height);

    let id = egui::Id::unique(("dtr-reconnect", view.id));
    prime_area(ui.ctx(), id, egui::Order::Middle, rect, true);
    let mut ask = None;
    egui::Area::new(id)
        .order(egui::Order::Middle)
        .fixed_pos(rect.min)
        .interactable(true)
        .constrain(false)
        .fade_in(false)
        .show(ui.ctx(), |ui| {
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            ui.set_clip_rect(rect);
            // The terminal's own background, not the skin's window: the grid is
            // cleared with this colour, so the pane reads as empty rather than as
            // a panel that happens to be the same size.
            let plate = ui.max_rect();
            let surface =
                Color32::from_rgb(look.background[0], look.background[1], look.background[2]);
            ui.painter().rect_filled(plate, 0.0, surface);
            // The whole plate is the way to the screen behind it. It is allocated
            // before the buttons, so a press on one of those is the button's: the
            // hit test goes to the widget drawn last.
            // No tooltip here: the hint is already a line of the block, and a
            // tooltip would be a second copy of it in the accessibility tree.
            let behind = ui.allocate_rect(plate, egui::Sense::click());
            // The notice's own words are not text to select: a drag on them is a
            // drag on the notice, which is what folds it. Only the terminal's text
            // is selectable, and it is the thing this is uncovering.
            ui.style_mut().interaction.selectable_labels = false;
            // The block is placed by its centre rather than by a leading spacer:
            // it was measured, so its own rectangle can be centred in the pane and
            // the rows fill it exactly.
            let block = egui::Rect::from_center_size(
                plate.center(),
                egui::vec2(
                    width,
                    height.min((plate.height() - 2.0 * NOTICE_MARGIN).max(style.row)),
                ),
            );
            // The block is exactly as wide as its widest row, so it is centred
            // as a whole and every row inside it starts on the same edge: a tight
            // column of state, not a line of type floating in the middle.
            let mut ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(block)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            ui.spacing_mut().item_spacing = spacing;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(title)
                        .font(style.title_font.clone())
                        .color(palette.text),
                );
                ui.label(
                    RichText::new(&retry.what)
                        .font(label.clone())
                        .color(palette.dim),
                );
            });
            ui.label(
                RichText::new(line)
                    .font(style.value_font.clone())
                    .color(palette.text),
            );
            if roomy && let Some(dir) = &retry.dir {
                ui.label(
                    RichText::new(format!("reopening in {dir}"))
                        .font(label.clone())
                        .color(palette.dim),
                );
            }
            if roomiest && let Some(why) = &retry.why {
                ui.label(RichText::new(why).font(label.clone()).color(palette.dim));
            }
            ui.horizontal(|ui| {
                if matches!(retry.when, crate::pane::When::At(_))
                    && dtr_widgets::Button::new("retry now").show(ui, &skin)
                {
                    ask = Some(ReconnectAsk::Now);
                }
                if dtr_widgets::Button::new("close tab").show(ui, &skin) {
                    ask = Some(ReconnectAsk::Close);
                }
            });
            ui.label(
                RichText::new(FOLD_HINT)
                    .font(label.clone())
                    .color(palette.dim),
            );
            if behind.clicked() && ask.is_none() {
                ask = Some(ReconnectAsk::Fold);
            }
        });
    ask
}

/// The bar: one row of chrome across the top of the pane, and the terminal is
/// the terminal again underneath it.
fn folded(
    ui: &mut Ui,
    view: &Waiting,
    rect: egui::Rect,
    look: &Look,
    title: &str,
    line: &str,
) -> Option<ReconnectAsk> {
    let retry = &view.retry;
    let skin = look.skin();
    let palette = skin.palette;
    let style = skin.style.clone();
    let label = egui::FontId::new(style.label_font.size - 1.0, style.label_font.family.clone());
    if rect.width() < 120.0 {
        return None;
    }
    // One row of the skin's height, which is a button's: the bar has to hold two
    // of them without the row growing under the pointer.
    let bar = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), style.row));
    let id = egui::Id::unique(("dtr-reconnect", view.id));
    prime_area(ui.ctx(), id, egui::Order::Middle, bar, true);
    let mut ask = None;
    egui::Area::new(id)
        .order(egui::Order::Middle)
        .fixed_pos(bar.min)
        .interactable(true)
        .constrain(false)
        .fade_in(false)
        .show(ui.ctx(), |ui| {
            ui.set_min_size(bar.size());
            ui.set_max_size(bar.size());
            ui.set_clip_rect(bar);
            let plate = ui.max_rect();
            ui.painter().rect_filled(plate, 0.0, palette.card);
            // A hairline under it, so the bar reads as chrome over the grid
            // rather than as the terminal's first row.
            ui.painter().hline(
                plate.x_range(),
                plate.bottom() - 0.5,
                egui::Stroke::new(1.0, palette.border),
            );
            let mut behind = ui.allocate_rect(plate, egui::Sense::click());
            // Same as the cover: the bar is a button, not a line of text.
            ui.style_mut().interaction.selectable_labels = false;
            // What a one-line bar has no room for — the directory the session
            // will come back in, and what the last attempt said — is one hover
            // away rather than gone.
            let detail = detail(view);
            if !detail.is_empty() {
                behind = behind.on_hover_text(detail);
            }
            let mut ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(plate.shrink2(egui::vec2(10.0, 0.0)))
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            // Right to left: the buttons keep their width and the text gets what
            // is left, which is the order that lets a narrow pane elide a word
            // instead of pushing the way out of the pane.
            if dtr_widgets::Button::new("close tab").show(&mut ui, &skin) {
                ask = Some(ReconnectAsk::Close);
            }
            if matches!(retry.when, crate::pane::When::At(_))
                && dtr_widgets::Button::new("retry now").show(&mut ui, &skin)
            {
                ask = Some(ReconnectAsk::Now);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(title)
                        .font(style.value_font.clone())
                        .color(palette.accent),
                );
                if plate.width() >= 260.0 {
                    ui.label(
                        RichText::new(&retry.what)
                            .font(label.clone())
                            .color(palette.dim),
                    );
                }
                if plate.width() >= 380.0 {
                    ui.label(RichText::new("·").font(label.clone()).color(palette.dim));
                    ui.add(
                        egui::Label::new(
                            RichText::new(line).font(label.clone()).color(palette.dim),
                        )
                        .truncate(),
                    );
                }
            });
            if behind.clicked() && ask.is_none() {
                ask = Some(ReconnectAsk::Unfold);
            }
        });
    ask
}

/// What the bar cannot say: where the session will come back, and what the last
/// attempt said.
fn detail(view: &Waiting) -> String {
    let retry = &view.retry;
    let mut lines: Vec<String> = Vec::new();
    if let Some(dir) = &retry.dir {
        lines.push(format!("reopening in {dir}"));
    }
    if let Some(why) = &retry.why {
        lines.push(why.clone());
    }
    if view.folded {
        lines.push(FOLD_HINT.to_owned());
    }
    lines.join("\n")
}

/// The notice's block while it is being measured: the widest row and the height
/// of all of them, taken from the galleys the labels will lay out.
///
/// A block sized by eye is a block whose centring is off by the width of a word,
/// and the widest row is what its rectangle has to be.
#[derive(Default)]
struct Block {
    width: f32,
    height: f32,
    rows: usize,
}

impl Block {
    /// One row, measured by its galley.
    fn row(&mut self, galley: &egui::Galley) {
        self.row_of(galley.size().x, galley.size().y);
    }

    /// One row, measured by hand — a row that is a button, not a line of type.
    fn row_of(&mut self, width: f32, height: f32) {
        self.width = self.width.max(width);
        self.height += height;
        self.rows += 1;
    }

    /// The gaps between the rows, which the layout will insert.
    fn gaps(&mut self, gap: f32) {
        self.height += gap * self.rows.saturating_sub(1) as f32;
    }
}

/// The air the notice keeps from the pane's edges, in points. One number,
/// because both the wrap width and the block's clamp are computed from it.
const NOTICE_MARGIN: f32 = 28.0;

/// What the cover says about getting it out of the way. In the block, so a person
/// finds the gesture rather than guessing at it.
const FOLD_HINT: &str = "press or scroll to see the screen";

/// What the notice calls itself, and what it says it is doing.
///
/// A first connection is not a reconnection: the pane quotes the host in both
/// cases, and the word in front of it is the whole difference between "this is how
/// it starts" and "this started and then broke".
fn headings(retry: &crate::pane::Retry) -> (&'static str, String) {
    let title = if retry.attempt == 0 && retry.when != crate::pane::When::Watching {
        "connecting"
    } else {
        "reconnecting"
    };
    (title, retry_line(retry))
}

/// What the notice says is happening, in one line.
///
/// The countdown is the point of the notice: "waiting" and "waiting, and the
/// next attempt is in four seconds" are different things to a person deciding
/// whether to wait or to close the tab.
fn retry_line(retry: &crate::pane::Retry) -> String {
    use crate::pane::When;
    match retry.when {
        // A device is watched rather than dialled, so there is no deadline to
        // count: the port either turns up or it does not.
        When::Watching => "watching for the device".to_owned(),
        When::Dialling if retry.attempt == 0 => "waiting for the host".to_owned(),
        When::Dialling => format!("attempt {} · connecting", retry.attempt),
        When::At(at) => {
            let left = at.saturating_duration_since(std::time::Instant::now());
            let wait = if left.is_zero() {
                "now".to_owned()
            } else {
                format!("in {} s", left.as_secs() + 1)
            };
            format!("attempt {} · next attempt {wait}", retry.attempt)
        }
    }
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
    /// The strip's own menu: the commands that belong to the window rather than
    /// to a tab.
    Menu(Menu),
    /// Open a new tab: `None` is a shell the spawner picks for itself,
    /// `Some(destination)` is one of the shells, ports and hosts the menu
    /// offers. The destination travels *typed*: the app used to open every
    /// non-`None` answer as `ssh`, which ran `ssh /dev/ttyUSB0` when the entry
    /// picked was a serial port.
    New(Option<crate::sources::Destination>),
}

/// The window's own commands, the ones a menu can hold without a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Preferences,
    Keymap,
    About,
}

/// The strip's menu button: a burger at the right end, beside the window
/// buttons.
///
/// It sits there rather than at the left because the left is where tabs start;
/// putting it before them would move every tab whenever the menu was drawn. The
/// same place on both platforms — Windows puts an app menu next to its caption
/// buttons, and Linux has no caption buttons in a client frame but the same
/// right-hand end reads the same way.
pub fn app_menu(
    ui: &mut Ui,
    rect: egui::Rect,
    skin: &dtr_widgets::Skin,
    keymap: &crate::keys::Keymap,
) -> Option<Menu> {
    let palette = skin.palette;
    let response = ui.interact(rect, egui::Id::unique("dtr-menu"), egui::Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter().rect_filled(
            rect.shrink(2.0),
            skin.style.control_radius,
            palette.control_hover,
        );
    }
    let icon = egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0));
    egui::Image::new(egui::include_image!("../assets/icons/menu.svg"))
        .fit_to_exact_size(icon.size())
        .tint(palette.text)
        .paint_at(ui, icon);
    // Painted, so the node is declared: a screen reader hearing "button" at the
    // end of a tab strip has learnt nothing.
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, "menu"));
    if response.has_focus() {
        response.surrender_focus();
    }

    let mut chosen = None;
    // A menu of three words, not of a whole row: without a width it takes the
    // strip's.
    let button = response.rect;
    let mut popup = egui::Popup::menu(&response).width(180.0);
    if let Some(tip) = skin.style.tip_height {
        // A body hung off the button's edge cannot point its beak at the middle of
        // the button, and a beak is the whole reason the body is where it is.
        popup = dtr_widgets::point_at(ui, popup, button, tip);
    }
    let popup = popup.show(|ui| {
        ui.set_min_width(180.0);
        // The chord is asked of the live map, so a rebinding shows the key a
        // person actually has — and a switched-off binding shows none, which is
        // the truth rather than the default's key.
        let chord = |action: crate::keys::Action| keymap.bound(action).into_iter().next();
        let mut entry = |ui: &mut Ui, label: &str, chord: Option<String>, item: Menu| {
            let picked = match chord {
                Some(chord) => dtr_widgets::menu_row_chord(ui, skin, label, &chord),
                None => dtr_widgets::menu_row(ui, skin, label, false),
            };
            if picked {
                chosen = Some(item);
                ui.close();
            }
        };
        entry(
            ui,
            "preferences",
            chord(crate::keys::Action::Preferences),
            Menu::Preferences,
        );
        entry(
            ui,
            "keyboard map",
            chord(crate::keys::Action::Keymap),
            Menu::Keymap,
        );
        // The two ways to configure the window, and then what it is: the same
        // split a desktop application menu draws before its About.
        dtr_widgets::menu_separator(ui, skin);
        entry(ui, "about", None, Menu::About);
    });
    // Drawn after the frame, so the beak lands on top of the body and its shadow
    // rather than under them, and aimed at the button's own rectangle: which
    // edge it grows from, and where along it, are the widget's to work out.
    if let Some(popup) = popup {
        dtr_widgets::menu_tip(&popup.response, skin, button);
    }
    chosen
}

/// The platform's own picture of a shell's program, uploaded once per egui
/// context.
///
/// The pixels are cached by the platform, because a shell call is not a frame's
/// work; the texture is cached here, because a texture belongs to a context and
/// the window, a capture and a test harness each have their own.
fn system_icon(ui: &Ui, program: &str) -> Option<egui::TextureId> {
    // Globally unique rather than salted: the store is the whole context's, and
    // every row must find exactly the texture its own program uploaded.
    let key = egui::Id::unique(("dtr-shell-icon", program));
    if let Some(texture) = ui
        .ctx()
        .data(|data| data.get_temp::<egui::TextureHandle>(key))
    {
        return Some(texture.id());
    }
    let icon = crate::system::app_icon(program)?;
    let image = egui::ColorImage::from_rgba_unmultiplied([icon.width, icon.height], &icon.rgba);
    let texture = ui.ctx().load_texture(
        format!("dtr-shell-icon:{program}"),
        image,
        egui::TextureOptions::LINEAR,
    );
    let id = texture.id();
    ui.ctx().data_mut(|data| data.insert_temp(key, texture));
    Some(id)
}

/// The logo a shell is known by, where the machine has no picture of its own.
///
/// These are the projects' own marks rather than glyphs drawn here — GNU Bash's
/// official logo, Zsh's, fish's and Nushell's — which is the only honest answer on
/// a system that has none: `application-x-executable` is the same picture for
/// every program there, so a row would repeat one glyph down the menu. A shell
/// with no logo of its own, `sh` or `dash` or `ksh`, has no picture rather than a
/// borrowed one. `scripts/shell-icons.sh` records where each came from and what
/// it is licensed under.
fn shell_logo(name: &str) -> Option<egui::ImageSource<'static>> {
    Some(match name {
        "bash" => egui::include_image!("../assets/icons/bash.svg"),
        "zsh" => egui::include_image!("../assets/icons/zsh.svg"),
        "fish" => egui::include_image!("../assets/icons/fish.svg"),
        "nu" | "nushell" => egui::include_image!("../assets/icons/nu.svg"),
        _ => return None,
    })
}

/// The wire a serial port leads with, and the machine a host sits on.
///
/// Lucide glyphs, drawn in the skin's text colour, because neither thing has a
/// picture of its own: no icon theme knows `/dev/ttyUSB0`, and an ssh alias is a
/// name in a config file. A cable and a rack are what the two rows are.
fn port_glyph() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/cable.svg")
}

fn host_glyph() -> egui::ImageSource<'static> {
    egui::include_image!("../assets/icons/server.svg")
}

/// What a destination's row leads with.
///
/// A shell is the machine's own icon of its program where the platform has one —
/// Windows reads the executable's resource, Linux the desktop entry a package
/// installs and the icon theme beside it — and otherwise the shell's own logo; a
/// port and a host are glyphs, because there is nothing else to ask. `None` is a
/// shell with neither, and its row keeps the column and draws nothing in it.
fn row_mark(
    ui: &Ui,
    skin: &dtr_widgets::Skin,
    destination: &crate::sources::Destination,
) -> Option<dtr_widgets::Mark<'static>> {
    match destination {
        crate::sources::Destination::Shell { program, name } => {
            if let Some(icon) = system_icon(ui, program) {
                return Some(dtr_widgets::Mark::Texture(icon));
            }
            shell_logo(name).map(dtr_widgets::Mark::Image)
        }
        crate::sources::Destination::Serial { .. } => {
            Some(dtr_widgets::Mark::mask(port_glyph(), skin.palette.text))
        }
        crate::sources::Destination::Ssh { .. } => {
            Some(dtr_widgets::Mark::mask(host_glyph(), skin.palette.text))
        }
    }
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
    // The strip is the title bar, so its height is the platform's and the chip's
    // is [`tab_chip_height`]'s; neither can be a constant in here.
    chip_height: f32,
    // What each tab's program says it is doing, one per label. Empty is the
    // same as "nothing to say", which is what every caller but the window has.
    activity: &[crate::pane::Activity],
) -> TabStrip {
    // Idempotent, and here rather than only at startup so every caller — the
    // window, a capture, a kittest harness — gets a working loader.
    install_image_loaders(ui.ctx());
    let skin = look.skin();
    let palette = skin.palette;
    let mut clicked = None;
    // The strip's own row, because `ui.horizontal` gives its child
    // `interact_size.y` — and that is the *skin's* row, which a dialog installs
    // on the context for the stock widgets egui draws inside its popups. A
    // skin's row is 34 points for Adwaita and 23 for dtr's own, so the row
    // became taller than the chip box it sits in and `Align::Center` pushed
    // the chips down past the strip's clip rect: their bottoms were cut off
    // after the preferences menu had been opened once. A chip is
    // `chip_height` tall whoever asks, and the strip measures its own row.
    ui.spacing_mut().interact_size.y = chip_height;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (index, label) in labels.iter().enumerate() {
            let is_active = index == active;
            let text = format!("{} {}", index + 1, label);
            let fg = if is_active {
                palette.on_accent
            } else {
                palette.text
            };
            // One row, broken anywhere, elided with `…` — the three settings
            // that make a long title stop at the cap instead of widening the
            // chip (`TextWrapping::max_rows` is what enables elision at all).
            let mut job = egui::text::LayoutJob::simple(
                text,
                skin.style.label_font.clone(),
                fg,
                TAB_LABEL_MAX_WIDTH,
            );
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = ui.painter().layout_job(job);
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
            let close_size = chip_height - 4.0;
            let size = egui::vec2(galley.size().x + 18.0 + close_size, chip_height);
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
            // Only when something was actually cut: a tooltip repeating a
            // two-word title is noise, and it would also change every snapshot
            // taken with the pointer over a short chip.
            if galley.elided {
                response
                    .clone()
                    .on_hover_text(format!("{} {}", index + 1, label));
            }
            // Hover is the strip's own affordance: a chip that looks the same
            // under the pointer as away from it reads as decoration. The cross's
            // own hover is asked by rect, because its widget is created after
            // the chip is painted — the cross's highlight has to land on top.
            let hovered = response.hovered() || ui.rect_contains_pointer(close_rect);
            let fill = match (is_active, hovered) {
                (true, true) => palette.accent_hover,
                (true, false) => palette.accent,
                (false, true) => palette.control_hover,
                (false, false) => palette.control,
            };
            let painter = ui.painter();
            painter.rect_filled(rect, skin.style.chip_radius, fill);
            // The program's own report, on the chip's bottom edge: a fraction
            // when it gave one, a sweeping band when it is working without a
            // number, and the whole stripe when it is a *state* — done, paused,
            // blocked or failed — because "42%" of a failed build means nothing.
            //
            // The colour is the chip's own foreground, not the accent: the
            // active chip *is* the accent, so an accent stripe on it is invisible
            // — which is what the first version drew. Only "needs you" gets the
            // palette's danger slot, and a blocked build and a failed one share
            // it because there is no warning slot to tell them apart yet.
            if let Some(activity) = activity.get(index) {
                let (what, colour) = match activity.state {
                    crate::pane::ActivityState::Idle => (None, fg),
                    crate::pane::ActivityState::Working => (
                        Some(match activity.percent {
                            Some(percent) => {
                                dtr_widgets::Stripe::Fraction(f32::from(percent) / 100.0)
                            }
                            None => dtr_widgets::Stripe::Indeterminate,
                        }),
                        fg,
                    ),
                    crate::pane::ActivityState::Paused => (
                        Some(dtr_widgets::Stripe::Fraction(
                            activity.percent.map_or(0.0, |p| f32::from(p) / 100.0),
                        )),
                        fg.gamma_multiply(0.5),
                    ),
                    crate::pane::ActivityState::Done => (Some(dtr_widgets::Stripe::State), fg),
                    crate::pane::ActivityState::Blocked | crate::pane::ActivityState::Error => {
                        (Some(dtr_widgets::Stripe::State), palette.danger)
                    }
                };
                if let Some(what) = what {
                    dtr_widgets::stripe(painter, rect, what, colour, ui.input(|i| i.time));
                }
            }
            painter.galley(
                egui::pos2(rect.left() + 7.0, rect.center().y - galley.size().y / 2.0),
                galley,
                fg,
            );
            // The cross is the crate's: two strokes, drawn, with a target of
            // its own so a press near the right-hand end closes and one on the
            // label switches. It is created after the chip is painted, or its
            // hover fill would be painted over.
            let close = dtr_widgets::cross_at(
                ui,
                &skin,
                close_rect,
                egui::Id::unique(("dtr-tab-close", index)),
                &format!("close tab {}", index + 1),
            );

            // Painted by hand, so the label's accessibility node is declared by
            // hand too — otherwise a screen reader (and the test harness) sees a
            // strip with no tabs in it at all. The cross declares its own.
            let name = format!("{} {}", index + 1, &labels[index]);
            response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));

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
            // Both responses are pointer-only: `cross_at` strips the synthetic
            // click, and the chip's own is checked against the pointer below.
            let pointer = ui.input(|i| i.pointer.any_click());
            if close.clicked() {
                clicked = Some(TabHit::Close(index));
            } else if response.clicked() && pointer {
                clicked = Some(TabHit::Select(index));
            }
        }

        // The new-tab button. Its menu is where a terminal's destinations live:
        // the shells this machine has, then the serial ports and the ssh hosts,
        // each kind behind its own separator. Pointer only, like the chips, so
        // Enter in the shell never opens a menu — `plus` returns a response
        // that cannot have been clicked by a key.
        let response = dtr_widgets::plus(
            ui,
            &skin,
            "new tab",
            egui::vec2(TAB_CHIP_HEIGHT + 4.0, TAB_CHIP_HEIGHT),
        );
        // What the body will say, measured before the body exists: a row fills
        // the width it is offered, so a body left to decide for itself takes
        // everything from the `+` to the window's edge. Every row the menu can
        // draw is in the list, the fallback and the note included, and the
        // widest of them is the width. Every destination reserves the mark column,
        // a shell even where this machine has no picture for it: the shells of one
        // machine do not all have a logo, and two starting points in one group read
        // as a mistake rather than as two kinds of row.
        let labels: Vec<String> = destinations
            .iter()
            .map(crate::sources::Destination::label)
            .collect();
        let mut rows: Vec<dtr_widgets::RowSpec> = labels
            .iter()
            .map(|label| dtr_widgets::RowSpec {
                label: label.as_str(),
                icon: true,
            })
            .collect();
        rows.push(dtr_widgets::RowSpec {
            label: "local shell",
            icon: false,
        });
        rows.push(dtr_widgets::RowSpec {
            label: "no serial ports and no ssh hosts",
            icon: false,
        });
        let width = dtr_widgets::menu_width(ui, &skin, &rows).max(220.0);

        let mut popup = egui::Popup::menu(&response).width(width);
        if let Some(tip) = skin.style.tip_height {
            // Aimed at the `+` for the same reason as the app menu — and this is
            // the menu that needs it: it is a narrow body hung off a button close
            // enough to the window's left edge that a body aligned to the
            // button's edge would be pushed back inside the window, which is when
            // a beak stops pointing at what opened it.
            popup = dtr_widgets::point_at(ui, popup, response.rect, tip);
        }
        let menu = popup.show(|ui| {
            // A machine with forty hosts gets a menu taller than the screen,
            // and a menu whose last host is below the taskbar is a host you
            // cannot pick. The body scrolls instead, with a bar that says so:
            // the plus is at the top of the window, so the screen less a strip
            // is what there is to spend.
            let max_height = (ui.ctx().content_rect().height() - 80.0).max(160.0);
            // A solid bar, not the floating one egui defaults to: a bar that
            // appears only while the wheel turns is a bar a person who has not
            // found the wheel never sees, and a floating one would paint over
            // the host names it is there to reveal.
            ui.spacing_mut().scroll.floating = false;
            egui::ScrollArea::vertical()
                .id_salt("dtr-new-tab-menu")
                .max_height(max_height)
                .auto_shrink([true, true])
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded)
                .show(ui, |ui| {
                    // The shells are their own group and they come first: a
                    // local shell is what a `+` is usually for. They are
                    // discovered rather than listed, so a machine with three
                    // installed shells gets three rows, the first being the one
                    // a bare `+` would have started.
                    let shells: Vec<&crate::sources::Destination> = destinations
                        .iter()
                        .filter(|destination| destination.is_shell())
                        .collect();
                    if shells.is_empty() {
                        // Nothing was discovered — a container with no
                        // `/etc/shells` and no shell on `PATH`. The row is
                        // still there, because the spawner walks its own list
                        // and "a terminal with no shell" is not a menu item.
                        if dtr_widgets::menu_row(ui, &skin, "local shell", false) {
                            clicked = Some(TabHit::New(None));
                            ui.close();
                        }
                    }
                    for destination in shells {
                        // The mark is the machine's own picture of the program —
                        // Windows reads it out of the executable, and Linux has
                        // none to read, so there it is the shell's logo — and a
                        // shell with neither still gets the row, indented, with
                        // nothing in the column.
                        let label = destination.label();
                        if dtr_widgets::menu_row_icon(
                            ui,
                            &skin,
                            row_mark(ui, &skin, destination),
                            &label,
                            false,
                        ) {
                            clicked = Some(TabHit::New(Some(destination.clone())));
                            ui.close();
                        }
                    }
                    // The remaining groups are separated, not just prefixed.
                    // `serial …` and `ssh …` are different verbs that lead to
                    // different panes, and a run of `/dev/tty…` rows with an
                    // `ssh` row in the middle is one where the pointer lands on
                    // the wrong half of the menu. A group that is empty takes
                    // its separator with it, so a machine with no serial ports
                    // looks like it always did.
                    for group in [
                        destinations
                            .iter()
                            .filter(|destination| destination.is_serial())
                            .collect::<Vec<_>>(),
                        destinations
                            .iter()
                            .filter(|destination| {
                                !destination.is_shell() && !destination.is_serial()
                            })
                            .collect(),
                    ] {
                        if group.is_empty() {
                            continue;
                        }
                        dtr_widgets::menu_separator(ui, &skin);
                        for destination in group {
                            let label = destination.label();
                            if dtr_widgets::menu_row_icon(
                                ui,
                                &skin,
                                row_mark(ui, &skin, destination),
                                &label,
                                false,
                            ) {
                                clicked = Some(TabHit::New(Some(destination.clone())));
                                ui.close();
                            }
                        }
                    }
                    if destinations
                        .iter()
                        .all(crate::sources::Destination::is_shell)
                    {
                        ui.label(
                            egui::RichText::new("no serial ports and no ssh hosts")
                                .font(egui::FontId::new(
                                    skin.style.label_font.size - 1.0,
                                    skin.style.label_font.family.clone(),
                                ))
                                .color(palette.dim),
                        );
                    }
                });
        });
        // Same tip as the strip's app menu, because it is the same kind of
        // button: a menubutton's popover says which button it belongs to.
        if let Some(menu) = menu {
            dtr_widgets::menu_tip(&menu.response, &skin, response.rect);
        }
    });
    // What the horizontal actually took, so a drag region can start after it.
    // Taken from the ui rather than computed from the chips: the `+` is part of
    // the strip too, and a drag that started on it would be a drag on a menu.
    let used = ui.min_rect();
    TabStrip { hit: clicked, used }
}

pub fn status_bar(ui: &mut Ui, status: &Status, look: &Look) {
    let skin = look.skin();
    let p = skin.palette;
    let s = &skin.style;
    let small = egui::FontId::new(s.label_font.size - 0.5, s.label_font.family.clone());
    let body_font = s.value_font.clone();
    // The same trap as the tab strip's: a row taller than the space the bar was
    // given is a row whose text is centred below the panel's bottom edge. The
    // bar's row is the bar's height, not the skin's row, and never more than the
    // bar's own design height.
    ui.spacing_mut().interact_size.y = ui.available_height().min(STATUS_HEIGHT);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        if status.tabs.1 > 1 {
            ui.label(
                RichText::new(format!("{}/{}", status.tabs.0, status.tabs.1))
                    .font(small.clone())
                    .color(p.accent),
            );
        }
        ui.label(
            RichText::new(status.label)
                .font(body_font.clone())
                .color(p.text),
        );
        if let Some((dtr, rts)) = status.modem {
            for (label, level) in [("DTR", dtr), ("RTS", rts)] {
                ui.label(
                    RichText::new(format!("{label}{}", if level { "●" } else { "○" }))
                        .font(small.clone())
                        .color(if level { p.accent } else { p.dim }),
                );
            }
        }
        if status.scrollback > 0 {
            ui.label(
                RichText::new(format!("{} lines up", status.scrollback))
                    .font(small.clone())
                    .color(p.accent),
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
            .font(body_font.clone())
            .color(p.dim),
        );
        if let Some(delay) = status.key_delay {
            // At the far right, in its own layout: it is an indicator, not part
            // of the row's text, and a chip that moved when RX grew would be read
            // as one. The layout takes the rest of the row and aligns from the
            // right, so nothing to its left moves.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                delay_chip(ui, delay, &skin);
            });
        }
    });
}

/// The red `DELAY 1 ms` chip, at full bar height.
///
/// An indicator rather than a button: it senses nothing, takes no hover and no
/// click, because the way to switch the mode off is the chord or the palette, and
/// a chip that looked pressable would be a third way in with no way to say so.
/// `DELAY` first because the strip is read at a glance from the corner of the eye.
///
/// The chip is painted rather than laid out as a label so its fill covers the
/// bar's own height; a painted gadget has no accessibility node unless it says
/// so, which is why the response carries the text.
fn delay_chip(ui: &mut egui::Ui, delay: std::time::Duration, skin: &dtr_widgets::Skin) {
    let palette = skin.palette;
    let style = &skin.style;
    let text = format!("DELAY {} ms", delay.as_millis());
    let font = egui::FontId::new(style.label_font.size - 0.5, style.label_font.family.clone());
    let galley = ui
        .painter()
        .layout_no_wrap(text.clone(), font, palette.on_danger);
    // The bar's own height, inside the panel's padding: the strip is 24 points
    // and its margin leaves 16, so the chip fills what it was given and no more.
    let height = ui.available_height().min(STATUS_HEIGHT);
    let width = galley.size().x + style.padding.x * 2.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, f32::from(style.control_radius), palette.danger);
    let at = rect.center() - galley.size() / 2.0;
    ui.painter().galley(at, galley, palette.on_danger);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &text));
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

    /// A config file becomes a skin: the look, its scheme and its accent, in one
    /// place. The two that ask a source ignore the other two, which is the rule
    /// the preferences menu's rows are built from.
    #[test]
    fn a_config_file_asks_for_a_skin() {
        let mut config = dtr_config::Config::default();
        config.ui.widgets = dtr_config::Widgets::Windows;
        config.ui.scheme = dtr_config::Scheme::Light;
        config.ui.accent = Some([0xED, 0x5B, 0x00]);
        let choice = Look::choice_of(&config);
        assert_eq!(choice.preset, Preset::Windows);
        assert_eq!(choice.scheme, dtr_widgets::Scheme::Light);
        assert_eq!(
            choice.accent,
            Some(egui::Color32::from_rgb(0xED, 0x5B, 0x00))
        );

        // The same choice, as the window would draw it: a light Windows chrome
        // in the accent asked for. A test has no desktop to consult, so `native`
        // falling back and `system` are both the terminal's own darkness.
        let look = Look {
            choice,
            ..Look::default()
        };
        let skin = look.skin();
        assert_eq!(skin.origin, dtr_widgets::Origin::Windows);
        assert!(!skin.palette.is_dark());
        assert_eq!(
            skin.palette.accent,
            egui::Color32::from_rgb(0xED, 0x5B, 0x00)
        );

        // Each of the four is one preset, and the old spelling is not a fifth.
        for (widgets, preset) in [
            (dtr_config::Widgets::Native, Preset::Native),
            (dtr_config::Widgets::Adwaita, Preset::Adwaita),
            (dtr_config::Widgets::Windows, Preset::Windows),
            (dtr_config::Widgets::Egui, Preset::Egui),
        ] {
            let mut config = dtr_config::Config::default();
            config.ui.widgets = widgets;
            assert_eq!(Look::choice_of(&config).preset, preset);
        }
    }

    /// Every key that makes a look, in one place.
    ///
    /// The three readers of those keys are the window at startup, the preferences
    /// card's preview and its save; a key one of them forgot is a window that
    /// changes when Save is pressed and not when the row was dragged, which is
    /// the whole bug this function exists to make impossible.
    #[test]
    fn a_config_file_becomes_a_whole_look() {
        let mut config = dtr_config::Config::default();
        config.font.size = 17.5;
        config.font.family = Some("JetBrains Mono".to_owned());
        config.font.fallback = vec!["Sarasa Mono".to_owned(), "Noto Sans CJK".to_owned()];
        config.font.path = Some(std::path::PathBuf::from("/tmp/demo.ttf"));
        config.ui.widgets = dtr_config::Widgets::Egui;
        config.ui.scheme = dtr_config::Scheme::Dark;

        let look = Look::from_config(&config, None);
        assert_eq!(look.size, 17.5);
        assert_eq!(look.family.as_deref(), Some("JetBrains Mono"));
        assert_eq!(
            look.fallback,
            ["Sarasa Mono", "Noto Sans CJK"],
            "the grid's fallback list is part of the look, not a second field"
        );
        assert_eq!(
            look.path.as_deref(),
            Some(std::path::Path::new("/tmp/demo.ttf"))
        );
        assert_eq!(look.choice.preset, Preset::Egui);
        assert_eq!(look.choice.scheme, dtr_widgets::Scheme::Dark);

        // A theme is the colours the look is built from, and the chrome's
        // surface and dim text are derived from them rather than stored.
        let theme =
            crate::theme::Theme::parse("demo", "background = #101010\nforeground = #e0e0e0\n")
                .expect("a two-line theme");
        let look = Look::from_config(&config, Some(&theme));
        assert_eq!(look.background, [0x10, 0x10, 0x10]);
        assert_eq!(look.foreground, [0xE0, 0xE0, 0xE0]);
        assert_ne!(look.dim, look.foreground, "dim is mixed, not copied");
    }

    /// The new-tab button offers every destination a terminal has, each kind
    /// behind its own separator: the shells this machine has, the serial ports,
    /// the hosts.
    ///
    /// The grouping is also the *type*, and that is the point. The menu used to
    /// decide "serial" by whether the destination began with `/`, and the app
    /// then opened every entry as `ssh` — so picking a port ran
    /// `ssh /dev/ttyUSB0`, and a `Host` someone spelled `prod/db` would have
    /// been run as a port. Both are checked here.
    #[test]
    fn the_new_tab_menu_offers_every_shell_and_each_kind_of_destination() {
        use crate::sources::Destination;

        let look = Look::default();
        let clicked = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = clicked.clone();
        let labels = vec!["demo".to_owned()];
        let destinations = vec![
            Destination::Shell {
                name: "bash".to_owned(),
                program: "/bin/bash".to_owned(),
            },
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
            if let Some(hit) =
                tab_bar(ui, &labels, 0, &destinations, &look, TAB_CHIP_HEIGHT, &[]).hit
            {
                *sink.borrow_mut() = Some(hit);
            }
        });
        harness.run();

        harness.get_by_label("new tab").click();
        harness.run();
        harness.get_by_label("shell bash").click();
        harness.run();
        assert_eq!(
            clicked.borrow().clone(),
            Some(TabHit::New(Some(Destination::Shell {
                name: "bash".to_owned(),
                program: "/bin/bash".to_owned(),
            }))),
            "a shell row opens that shell"
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

    /// A shell's mark is the shell's own logo, and only for the shells that have
    /// one: `sh` is not GNU Bash and `dash` is not fish, so borrowing a mark would
    /// be the menu telling a lie about what it is about to start. This is the
    /// embedded table only — `baash`'s mark is its package's, drawn from the
    /// desktop entry it installs, and `the_menu_draws_a_shells_own_mark` is where
    /// that path is checked.
    #[test]
    fn the_shells_with_a_logo_are_the_four_that_have_one() {
        let logo = |name: &str| shell_logo(name).and_then(|source| source.uri().map(str::to_owned));
        let known: Vec<String> = ["bash", "zsh", "fish", "nu"]
            .iter()
            .map(|name| logo(name).unwrap_or_else(|| panic!("{name} has a logo")))
            .collect();
        assert_eq!(
            logo("nushell"),
            logo("nu"),
            "two spellings of one shell are one picture"
        );
        let mut distinct = known.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            known.len(),
            "four shells, four pictures: {known:?}"
        );
        for plain in ["sh", "dash", "ksh", "tcsh", "csh", "baash"] {
            assert!(logo(plain).is_none(), "{plain} has no logo to draw");
        }
    }

    /// A serial port and a host have no picture of their own — no icon theme knows
    /// `/dev/ttyUSB0`, and an ssh alias is a name in a config file — so their rows
    /// lead with a wire and a machine, and two glyphs, or the two groups would say
    /// the same thing.
    #[test]
    fn a_port_and_a_host_have_their_own_marks() {
        let uri = |source: egui::ImageSource<'static>| source.uri().map(str::to_owned);
        let port = uri(port_glyph()).expect("the cable decodes");
        let host = uri(host_glyph()).expect("the server decodes");
        assert_ne!(port, host, "a port is not a host");
        for shell in ["bash", "zsh", "fish", "nu"] {
            assert_ne!(
                Some(port.clone()),
                shell_logo(shell).and_then(uri),
                "a port is not {shell}"
            );
        }
    }

    /// A machine where no shell could be discovered — a stripped container —
    /// still offers a shell, because the spawner walks its own list. The row
    /// answers `None`, which is "whatever the spawner finds".
    #[test]
    fn a_machine_with_no_shell_discovered_still_opens_one() {
        use crate::sources::Destination;
        let look = Look::default();
        let clicked = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = clicked.clone();
        let labels = vec!["demo".to_owned()];
        let destinations = vec![Destination::Ssh {
            alias: "web".to_owned(),
            detail: String::new(),
        }];
        let mut harness = Harness::new_ui(move |ui| {
            if let Some(hit) =
                tab_bar(ui, &labels, 0, &destinations, &look, TAB_CHIP_HEIGHT, &[]).hit
            {
                *sink.borrow_mut() = Some(hit);
            }
        });
        harness.run();

        harness.get_by_label("new tab").click();
        harness.run();
        harness.get_by_label("local shell").click();
        harness.run();
        assert_eq!(clicked.borrow().clone(), Some(TabHit::New(None)));
    }

    /// The menu with every group in it: the shells, the ports, the hosts.
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
            Destination::Shell {
                name: "bash".to_owned(),
                program: "/bin/bash".to_owned(),
            },
            Destination::Shell {
                name: "fish".to_owned(),
                program: "/usr/bin/fish".to_owned(),
            },
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
            tab_bar(ui, &labels, 0, &destinations, &look, TAB_CHIP_HEIGHT, &[]);
        });
        harness.run();

        harness.get_by_label("new tab").click();
        harness.run();
        harness.snapshot("new_tab_menu");
    }

    /// A shell whose package ships a desktop entry and an icon theme draws that
    /// mark — not a project logo drawn here, and not an empty column. The two
    /// rows are the whole contract: `demo-shell` is found by the `Exec=` of a
    /// file called `demo-shell.desktop` under a package's `share/`, and its
    /// `Icon=` is the name the theme beside it holds. `bash` has no such entry
    /// and falls back to the logo embedded for it, which is why the two rows
    /// carry different pictures.
    #[test]
    fn the_menu_draws_a_shells_own_mark() {
        use crate::sources::Destination;
        let look = Look::default();
        let labels = vec!["demo".to_owned()];
        let destinations = vec![
            Destination::Shell {
                name: "demo-shell".to_owned(),
                program: format!(
                    "{}/tests/fixtures/shell-icon/bin/demo-shell",
                    env!("CARGO_MANIFEST_DIR")
                ),
            },
            Destination::Shell {
                name: "bash".to_owned(),
                program: "/bin/bash".to_owned(),
            },
        ];
        let mut harness = Harness::new_ui(move |ui| {
            tab_bar(ui, &labels, 0, &destinations, &look, TAB_CHIP_HEIGHT, &[]);
        });
        harness.run();

        harness.get_by_label("new tab").click();
        harness.run();
        harness.snapshot("new_tab_menu_shell_icon");
    }

    /// Forty hosts do not fit on a screen, and a menu that grows past the
    /// window is a menu whose last hosts cannot be picked. The body scrolls
    /// instead — and the bar is visible, so the menu says there is more.
    #[test]
    fn a_long_new_tab_menu_scrolls_instead_of_running_off_the_window() {
        use crate::sources::Destination;
        let look = Look::default();
        let labels = vec!["demo".to_owned()];
        let destinations: Vec<Destination> = (0..40)
            .map(|index| Destination::Ssh {
                alias: format!("host{index:02}"),
                detail: format!("user{index}@10.0.0.{index}"),
            })
            .collect();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(640.0, 360.0))
            .build_ui(move |ui| {
                tab_bar(ui, &labels, 0, &destinations, &look, TAB_CHIP_HEIGHT, &[]);
            });
        harness.run();

        harness.get_by_label("new tab").click();
        // The first popup frame is a sizing pass; the scroll area's cap is
        // only observable once the menu has settled.
        harness.run();
        harness.run();
        harness.snapshot("new_tab_menu_scrolled");
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
            if let Some(hit) = tab_bar(ui, &labels, 1, &[], &look, TAB_CHIP_HEIGHT, &[]).hit {
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

    /// A long title makes a long chip, unless the chip has a ceiling.
    ///
    /// The titles are not ours — a working directory or an ssh alias with a
    /// command after it is as long as it is — so an unbounded chip is a strip
    /// that scrolls its own tabs off the right edge.
    #[test]
    fn a_long_title_stops_at_the_cap() {
        let look = Look::default();
        let long = "/home/somebody/src/a/directory/with/a/deliberately/long/name".to_owned();
        let labels = vec![long.clone(), "shell".to_owned()];
        let mut harness = Harness::new_ui(move |ui| {
            tab_bar(ui, &labels, 0, &[], &look, TAB_CHIP_HEIGHT, &[]);
        });
        harness.run();

        // The full name is still the chip's accessible name, so nothing was
        // lost from the model — only from the painted line.
        let name = format!("1 {long}");
        let node = harness.get_by_label(&name);
        let label_width = node.rect().width();
        // The chip is the label plus its padding and the cross; the node's rect
        // is the label target, which is the chip minus that furniture.
        assert!(
            label_width <= TAB_LABEL_MAX_WIDTH + 10.0,
            "a {label_width} point label is wider than the {TAB_LABEL_MAX_WIDTH} cap"
        );

        let short = harness.get_by_label("2 shell");
        assert!(
            short.rect().width() < label_width,
            "a short title should not be padded to the cap"
        );
    }

    /// A pane that is waiting for its connection says so over the pane, as a
    /// card: the state, the wait, and the two things a person can do about it.
    ///
    /// This used to be a line written into the terminal, which is a notice in
    /// the scrollback of a session with no program behind it. The assertions are
    /// on what the card draws, because the card *is* the notice now: a pane that
    /// went back to printing it would have no card for these to find.
    #[test]
    fn a_waiting_pane_draws_a_notice_over_its_own_rectangle() {
        let look = Look::default();
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = std::rc::Rc::clone(&asked);
        let waiting = Waiting {
            id: 1,
            rect: crate::layout::Rect::new(0.0, 0.0, 640.0, 400.0),
            retry: waiting_retry(),
            folded: false,
        };
        let mut harness = Harness::builder()
            .with_size(egui::vec2(640.0, 400.0))
            .build_ui(move |ui| {
                if let Some(ask) = reconnect(ui, &waiting, &look) {
                    sink.borrow_mut().push(ask);
                }
            });
        harness.run();

        // What it is, what it was doing, and where it will be put back.
        assert!(
            harness.query_by_label("reconnecting").is_some(),
            "the notice does not say what it is"
        );
        assert!(harness.query_by_label("jupyter").is_some());
        assert!(harness.query_by_label("reopening in /srv/app").is_some());
        let line = harness.get_by_label("attempt 3 · next attempt in 4 s");
        assert!(line.rect().width() > 0.0);
        // The way out of the way, said rather than guessed at.
        assert!(harness.query_by_label(FOLD_HINT).is_some());

        harness.snapshot("reconnecting");

        // Every answer is reported back to the app, which is the only thing that
        // can send to a pane, close a tab or remember a fold.
        harness.get_by_label("close tab").click();
        harness.run();
        assert_eq!(asked.borrow().as_slice(), [ReconnectAsk::Close]);
        asked.borrow_mut().clear();
        harness.get_by_label("retry now").click();
        harness.run();
        assert_eq!(asked.borrow().as_slice(), [ReconnectAsk::Now]);
        asked.borrow_mut().clear();
        // A press on the notice itself, anywhere but a button, folds it.
        harness.get_by_label("jupyter").click();
        harness.run();
        assert_eq!(asked.borrow().as_slice(), [ReconnectAsk::Fold]);
    }

    /// Folded, the notice is one bar at the top of the pane: the state in a line,
    /// the two answers, and the rest of the rectangle is the terminal's again.
    #[test]
    fn a_folded_notice_is_one_bar_over_the_pane() {
        let look = Look::default();
        let row = look.skin().style.row;
        let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = std::rc::Rc::clone(&asked);
        let waiting = Waiting {
            id: 1,
            rect: crate::layout::Rect::new(0.0, 0.0, 640.0, 400.0),
            retry: waiting_retry(),
            folded: true,
        };
        let mut harness = Harness::builder()
            .with_size(egui::vec2(640.0, 400.0))
            .build_ui(move |ui| {
                if let Some(ask) = reconnect(ui, &waiting, &look) {
                    sink.borrow_mut().push(ask);
                }
            });
        harness.run();

        for label in ["reconnecting", "jupyter", "attempt 3 · next attempt in 4 s"] {
            assert!(
                harness.query_by_label(label).is_some(),
                "the bar does not say `{label}`"
            );
        }
        // One row, at the top: the bar's own height is a button's, and nothing
        // below it is claimed, so the grid underneath is the terminal's again.
        let bar = harness.get_by_label("reconnecting").rect();
        assert!(bar.height() <= row, "{bar:?}");
        let pane = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 400.0));
        assert!(bar.top() < pane.top() + 12.0, "the bar is not at the top");
        assert!(
            row < pane.height() / 4.0,
            "a bar that took a quarter of the pane is not a bar"
        );

        harness.snapshot("reconnecting_bar");

        asked.borrow_mut().clear();
        harness.get_by_label("retry now").click();
        harness.run();
        assert_eq!(asked.borrow().as_slice(), [ReconnectAsk::Now]);
        asked.borrow_mut().clear();
        harness.get_by_label("jupyter").click();
        harness.run();
        assert_eq!(asked.borrow().as_slice(), [ReconnectAsk::Unfold]);
    }

    /// The state both shapes are drawn from: a third attempt, four seconds out,
    /// in a directory on another machine.
    fn waiting_retry() -> crate::pane::Retry {
        crate::pane::Retry {
            what: "jupyter".to_owned(),
            dir: Some("/srv/app".to_owned()),
            attempt: 3,
            when: crate::pane::When::At(
                std::time::Instant::now() + std::time::Duration::from_secs(4),
            ),
            why: None,
        }
    }

    /// The countdown is the reason this is a card and not a line of text: a wait
    /// says how long it is, and a device that is watched says there is nothing to
    /// count.
    #[test]
    fn the_wait_is_said_in_words() {
        use crate::pane::{Retry, When};
        let base = Retry {
            what: "jupyter".to_owned(),
            dir: None,
            attempt: 0,
            when: When::Dialling,
            why: None,
        };
        assert_eq!(
            retry_line(&base),
            "waiting for the host",
            "the first connection is not a retry"
        );
        assert_eq!(
            retry_line(&Retry {
                attempt: 2,
                when: When::Dialling,
                ..base.clone()
            }),
            "attempt 2 · connecting"
        );
        assert_eq!(
            retry_line(&Retry {
                attempt: 3,
                when: When::At(std::time::Instant::now() + std::time::Duration::from_secs(4)),
                ..base.clone()
            }),
            "attempt 3 · next attempt in 4 s",
            "a part second is a second: the wait is not over until it is"
        );
        assert_eq!(
            retry_line(&Retry {
                attempt: 3,
                when: When::At(std::time::Instant::now() - std::time::Duration::from_secs(1)),
                ..base.clone()
            }),
            "attempt 3 · next attempt now"
        );
        assert_eq!(
            retry_line(&Retry {
                what: "/dev/ttyUSB0".to_owned(),
                when: When::Watching,
                ..base
            }),
            "watching for the device",
            "a port has no deadline: it either turns up or it does not"
        );
    }

    /// The elided state, painted: a title that was cut ends in `…`, and the
    /// `+` is still reachable on the right of it.
    ///
    /// Worth a baseline of its own because the glyph is the only part of this
    /// that can go wrong quietly: `break_anywhere` without a right font, or a
    /// chip width computed from the *unclipped* galley, both still pass a test
    /// that only looks at numbers.
    #[test]
    fn a_shortened_title_ends_in_an_ellipsis() {
        let look = Look::default();
        let labels = vec![
            "/home/somebody/src/a/directory/with/a/deliberately/long/name".to_owned(),
            "shell".to_owned(),
        ];
        let mut harness = Harness::new_ui(move |ui| {
            tab_bar(ui, &labels, 0, &[], &look, TAB_CHIP_HEIGHT, &[]);
        });
        harness.run();
        harness.get_by_label("2 shell").hover();
        harness.run();
        harness.snapshot("tab_bar_elided");
    }

    /// Hover is the strip's only feedback, and it is painted by hand: a
    /// regression in the fill is invisible to every other test, because the
    /// pointer is never over the strip in them.
    #[test]
    fn a_chip_lights_up_under_the_pointer() {
        let look = Look::default();
        let labels = vec!["demo".to_owned(), "shell".to_owned()];
        let mut harness = Harness::new_ui(move |ui| {
            tab_bar(ui, &labels, 1, &[], &look, TAB_CHIP_HEIGHT, &[]);
        });
        harness.run();

        // Away from the strip entirely: the state every other snapshot has.
        harness.get_by_label("1 demo").hover();
        harness.run();
        harness.snapshot("tab_bar_hover");
    }

    /// A chip grows with the strip and stops at a tab: six points of air either
    /// side, so the Windows caption keeps the chip it had (31 − 12 = 19, floored
    /// at 20) and a 46-point GNOME title bar gets Adwaita's 26-point tab instead
    /// of a pill that reads as a button.
    #[test]
    fn a_chip_follows_the_strip_and_stops_at_a_tab() {
        assert_eq!(tab_chip_height(TAB_HEIGHT), TAB_CHIP_HEIGHT);
        assert_eq!(tab_chip_height(31.0), TAB_CHIP_HEIGHT);
        assert_eq!(tab_chip_height(crate::chrome::ADWAITA_TITLEBAR), 26.0);
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
            if let Some(index) = tab_bar(ui, &labels, 0, &[], &look, TAB_CHIP_HEIGHT, &[]).hit {
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
            tab_bar(ui, &labels, 1, &[], &look, TAB_CHIP_HEIGHT, &[]);
        });
        harness.run();
        harness.snapshot("tab_bar");
    }

    /// The strip's row is the chip's height, whatever row the context's style
    /// asks for.
    ///
    /// `ui.horizontal` gives its child `interact_size.y`, and a skin's row is
    /// 34 points for Adwaita and 23 for dtr's own — numbers that belong to a
    /// dialog's controls, not to a chip. This is the reported bug: open the
    /// preferences menu once (which installs the skin on the context, for the stock
    /// widgets in its popups) and every later frame drew the chips centred in a
    /// 34-point row, 7 points lower than the 20-point box they were given, with
    /// their bottoms cut off by the strip.
    #[test]
    fn a_skins_row_does_not_move_the_chips() {
        let labels = vec!["demo".to_owned()];
        let look = Look::default();
        let used = std::rc::Rc::new(std::cell::Cell::new(egui::Rect::NOTHING));
        let sink = used.clone();
        let mut harness = Harness::new_ui(move |ui| {
            ui.ctx()
                .global_style_mut(|style| style.spacing.interact_size.y = 34.0);
            let strip = tab_bar(ui, &labels, 0, &[], &look, TAB_CHIP_HEIGHT, &[]);
            sink.set(strip.used);
        });
        harness.run();
        let height = used.get().height();
        assert!(
            (height - TAB_CHIP_HEIGHT).abs() < 0.5,
            "the chip row came out {height} points tall instead of {TAB_CHIP_HEIGHT}"
        );
    }

    /// The strip's menu: a burger, and a popup that holds the window's three
    /// commands. The menu is the mouse's way in to surfaces that otherwise live
    /// behind chords.
    #[test]
    fn the_strip_menu_opens() {
        let look = Look::default();
        let keymap = crate::keys::Keymap::new(&Default::default());
        let mut harness = Harness::new_ui(move |ui| {
            install_image_loaders(ui.ctx());
            // The window installs the resolved skin on the context, and the
            // popup's own frame is drawn from it: without this the card under
            // the rows is egui's grey rather than the skin's.
            let skin = look.skin();
            skin.apply(ui.ctx());
            let rect = egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(32.0, crate::ui::TAB_HEIGHT),
            );
            let _ = app_menu(ui, rect, &skin, &keymap);
        });
        harness.run_steps(2);
        // A painted icon with no text needs the name it declares itself.
        harness.get_by_label("menu").click();
        harness.run_steps(3);
        for item in ["preferences", "keyboard map", "about"] {
            harness.get_by_label(item);
        }
        harness.snapshot("app_menu");
    }

    /// The same menu in the look the style is measured from: Adwaita's popover,
    /// with a 6-point inset, the chord column at the right and the rule before
    /// About. A menu that borrowed the card's 24 × 20 margin and reserved a tick
    /// column is the picture this one replaced.
    #[test]
    fn the_strip_menu_renders_in_the_adwaita_skin() {
        let look = Look {
            choice: dtr_widgets::Choice::new(dtr_widgets::Preset::Adwaita),
            ..Look::default()
        };
        let keymap = crate::keys::Keymap::new(&Default::default());
        let mut harness = Harness::new_ui(move |ui| {
            install_image_loaders(ui.ctx());
            // The window installs the resolved skin on the context, and the
            // popup's own frame is drawn from it: without this the card under
            // the rows is egui's grey rather than the skin's.
            let skin = look.skin();
            skin.apply(ui.ctx());
            // Near the strip's right-hand end, where the window puts it, and far
            // enough in that the menu can centre on it inside this harness: the
            // thing the picture is for is the tip landing on the middle of the
            // button rather than being dragged in from it by the corner clamp.
            let rect = egui::Rect::from_min_size(
                egui::pos2(ui.ctx().content_rect().right() - 130.0, 0.0),
                egui::vec2(32.0, crate::ui::TAB_HEIGHT),
            );
            let _ = app_menu(ui, rect, &skin, &keymap);
        });
        harness.run_steps(2);
        harness.get_by_label("menu").click();
        harness.run_steps(3);
        harness.snapshot("app_menu_adwaita");
    }

    /// The `+`'s menu in Adwaita's look, with a serial port whose line is long
    /// enough to make the body wider than the space beside the `+`. The body is
    /// pushed back to the window's edge — and the beak still lands on the middle
    /// of the `+`, which is the thing a body hung off the button's edge could not
    /// do: it was drawn 20 points to the right of it.
    #[test]
    fn the_new_tab_menu_renders_in_the_adwaita_skin() {
        use crate::sources::Destination;

        let look = Look {
            choice: dtr_widgets::Choice::new(dtr_widgets::Preset::Adwaita),
            ..Look::default()
        };
        let labels = vec!["demo".to_owned()];
        let destinations = vec![Destination::Serial {
            path: "/dev/ttyUSB0".to_owned(),
            detail: "Dual RS232-HS - FTDI - 0403:6010".to_owned(),
        }];
        let mut harness = Harness::new_ui(move |ui| {
            look.skin().apply(ui.ctx());
            let _ = tab_bar(ui, &labels, 0, &destinations, &look, TAB_CHIP_HEIGHT, &[]);
        });
        harness.run_steps(2);
        harness.get_by_label("new tab").click();
        harness.run_steps(3);
        harness.snapshot("new_tab_menu_adwaita");
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
            egui::include_image!("../../../assets/logo/dtr-icon.svg"),
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

    /// A popup is a layer above the window's own, and the grid is not.
    ///
    /// `layer_id_at` is never `None` inside the window — the root layer's rect
    /// is the whole content area, so it is found under the pointer wherever the
    /// pointer is — and "a layer is there" was read as "an egui surface is
    /// there". That made the wheel over the `+` menu scroll the terminal behind
    /// it, and a link under the pointer never lit its hand. The pair below is
    /// what `App::over_egui` has to tell apart, taken from the real popup
    /// rather than assumed, because egui's area bookkeeping is the half of it
    /// that is not dtr's.
    #[test]
    fn the_new_tab_menu_is_a_layer_above_the_grid() {
        let look = Look::default();
        let labels = vec!["demo".to_owned()];
        let mut harness = Harness::builder()
            .with_size(egui::vec2(640.0, 360.0))
            .build_ui(move |ui| {
                egui::Panel::top("strip").show(ui, |ui| {
                    tab_bar(ui, &labels, 0, &[], &look, TAB_CHIP_HEIGHT, &[]);
                });
                // The grid is not an egui widget: the window's renderer paints
                // it, and only the space is reserved here.
                ui.allocate_space(ui.available_size());
            });
        harness.run_steps(2);
        let grid = harness.ctx.content_rect().center();
        let under = harness.ctx.layer_id_at(grid).expect("the root layer");
        assert!(
            !dtr_widgets::above_the_root(under),
            "the grid is the window's own layer, not an egui surface: {under:?}"
        );

        harness.get_by_label("new tab").click();
        // The first popup frame is a sizing pass; the layer exists on the next.
        harness.run_steps(3);
        let menu = harness.get_by_label("local shell").rect().center();
        let popup = harness.ctx.layer_id_at(menu).expect("the menu's layer");
        assert!(
            dtr_widgets::above_the_root(popup),
            "the menu is above the root layer: {popup:?}"
        );
    }

    /// A status strip with only the parts the chip's test cares about.
    fn status_strip(key_delay: Option<std::time::Duration>) -> Status<'static> {
        Status {
            label: "demo",
            tabs: (1, 1),
            rx: 1_024,
            tx: 512,
            rx_rate: 0.0,
            tx_rate: 0.0,
            scrollback: 0,
            modem: Some((true, true)),
            counters: true,
            key_delay,
        }
    }

    /// A harness the height of the bar, with the panel's own padding, so the chip
    /// is measured at the size it will be on screen rather than at a dialog row's.
    fn status_harness(look: Look, key_delay: Option<std::time::Duration>) -> Harness<'static> {
        Harness::builder()
            .with_size(egui::vec2(720.0, STATUS_HEIGHT))
            .build_ui(move |ui| {
                egui::Frame::new()
                    .fill(look.skin().palette.card)
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| status_bar(ui, &status_strip(key_delay), &look));
            })
    }

    /// Delayed input is a mode, and the strip has to say so: on, there is a red
    /// `DELAY 1 ms` chip; off, there is nothing — not a dim chip, not a zero.
    #[test]
    fn the_delay_chip_is_on_the_strip_only_when_the_mode_is_on() {
        let mut harness =
            status_harness(Look::default(), Some(std::time::Duration::from_millis(1)));
        harness.run();
        harness.get_by_label("DELAY 1 ms");

        let mut harness = status_harness(Look::default(), None);
        harness.run();
        assert!(
            harness.query_by_label("DELAY 1 ms").is_none(),
            "the chip must not be drawn when the mode is off"
        );
    }

    /// What the chip looks like in dtr's own chrome, at the bar's height: a
    /// baseline rather than a claim, so a later change to the strip cannot move
    /// it silently.
    #[test]
    fn the_status_strip_renders_with_the_delay_chip() {
        let mut harness =
            status_harness(Look::default(), Some(std::time::Duration::from_millis(1)));
        harness.run();
        harness.snapshot("status_delay");
    }
}
