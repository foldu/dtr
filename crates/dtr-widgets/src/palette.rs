//! The colours a widget may use, by role rather than by blend factor.
//!
//! Every value here is a *slot*: `control` is the fill of an idle control,
//! `border_strong` is the one a hovered control gets, `scrim` is what dims the
//! window behind a modal. The chrome used to ask for
//! `mix(surface, foreground, 0.16)` at thirty call sites, with fifteen different
//! factors standing in for those roles, and a style cannot change a rule that is
//! a literal inside a button.
//!
//! Two sources fill the slots. [`Palette::dtr`] derives them from a theme the way
//! the chrome always has — a surface lifted from the terminal background, dim
//! text part-way to the foreground, the theme's cursor as the accent.
//! [`Palette::windows`] takes them from Microsoft's own WinUI 3 theme resources,
//! so the native style on Windows is the OS's palette rather than a guess at it:
//! the numbers are `controls/dev/CommonStyles/Common_themeresources_any.xaml` in
//! `microsoft/microsoft-ui-xaml`, composited where the XAML leaves them partly
//! transparent (a control fill is 6 % white in dark mode, not a colour).
//!
//! Two more are the same idea for a platform that may be nowhere near the
//! machine: [`Palette::adwaita`] from libadwaita's stylesheet and [`Palette::egui`]
//! from egui's own `Visuals`. The first two read the *platform*, and only
//! `native` uses them; the last two belong to a look the window carries with it.

use egui::Color32;

/// The colours every widget draws with. One slot, one meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// The window behind everything; what the terminal grid sits on.
    pub window: Color32,
    /// The dialog or panel fill.
    pub card: Color32,
    /// An inset surface inside a card: a list, a well, a row that is not a control.
    pub well: Color32,
    /// The window's own title bar and tab strip: Adwaita's `headerbar_bg_color`,
    /// which is part of the window rather than a raised container. A preset with
    /// no such distinction uses its card.
    pub headerbar: Color32,
    /// An idle control: a button, a field, a combo box.
    pub control: Color32,
    pub control_hover: Color32,
    pub control_active: Color32,
    /// The hairline every control has.
    pub border: Color32,
    /// The border a hovered or focused control has.
    pub border_strong: Color32,
    /// The stroke of a control that has to be *seen* before anything points at
    /// it: a checkbox's box, a text field's resting edge. WinUI's
    /// `ControlStrongStrokeColorDefault` is far stronger than the hairline — a
    /// checkbox painted in `border` is one nobody can find, let alone tick.
    pub control_strong: Color32,
    /// A rule between groups; carries its own alpha.
    pub divider: Color32,
    pub text: Color32,
    /// Secondary text: labels, hints, counts.
    pub dim: Color32,
    pub disabled: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub accent_active: Color32,
    /// Text and glyphs drawn *on* the accent.
    pub on_accent: Color32,
    pub selection: Color32,
    pub on_selection: Color32,
    /// What dims the window behind a modal.
    pub scrim: Color32,
    pub danger: Color32,
    pub on_danger: Color32,
    /// The two-tone focus ring; a style with `Focus::None` does not draw it.
    pub focus_outer: Color32,
    pub focus_inner: Color32,
}

/// The named colours a desktop's own stylesheet repainted, where it can.
///
/// `native` starts from libadwaita's measured constants ([`Palette::adwaita`]);
/// a user's `gtk.css` may legally redefine every one of them, and this is the set
/// dtr maps. `None` keeps the constant. A colour may carry alpha — the dark
/// sheet's `card_bg_color` is 8 % white — and [`Palette::adwaita_with`]
/// composites it over the surface it is drawn on, which is what GTK does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    pub window: Option<Color32>,
    pub card: Option<Color32>,
    pub well: Option<Color32>,
    pub headerbar: Option<Color32>,
    pub text: Option<Color32>,
    pub accent: Option<Color32>,
    pub on_accent: Option<Color32>,
    pub danger: Option<Color32>,
    pub on_danger: Option<Color32>,
}

impl Overrides {
    /// Whether the stylesheet said anything dtr can use; `-v` uses it to tell a
    /// session with a `gtk.css` from one without.
    pub fn is_empty(&self) -> bool {
        self.window.is_none()
            && self.card.is_none()
            && self.well.is_none()
            && self.headerbar.is_none()
            && self.text.is_none()
            && self.accent.is_none()
            && self.on_accent.is_none()
            && self.danger.is_none()
            && self.on_danger.is_none()
    }
}

/// The accent ladder Windows keeps beside the base colour.
///
/// Windows computes six variants of the accent for its own controls — the
/// pale ones and the dark ones — and stores the lot in the registry, so a
/// window that wants to look like Windows can hover into the *same* variants
/// rather than approximating them with alpha. The field names are the WinUI
/// resource names (`SystemAccentColorLight1`, `...Dark2`).
///
/// Entry order is not the order they are declared in: see
/// `crate::system::ladder_from_registry`, which had to decode it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccentLadder {
    pub light3: Color32,
    pub light2: Color32,
    pub light1: Color32,
    pub base: Color32,
    pub dark1: Color32,
    pub dark2: Color32,
    pub dark3: Color32,
    /// The eighth entry, which Windows fills with the second ("Start")
    /// accent rather than a shade. Kept so the decoding accounts for every
    /// byte it was given.
    pub secondary: Color32,
}

impl Palette {
    /// The terminal's own chrome, derived from a theme exactly as it always was.
    ///
    /// `window` is the terminal background, `text` its foreground, `accent` its
    /// cursor colour (or palette 3 where a theme has none).
    pub fn dtr(window: [u8; 3], text: [u8; 3], accent: [u8; 3]) -> Self {
        let window = rgb(window);
        let text = rgb(text);
        let accent = rgb(accent);
        let card = mix(window, text, 0.10);
        let dim = mix(window, text, 0.55);
        Self {
            window,
            card,
            well: card,
            headerbar: card,
            control: mix(card, text, 0.07),
            control_hover: mix(card, text, 0.13),
            control_active: mix(card, accent, 0.16),
            border: mix(card, text, 0.16),
            border_strong: mix(card, text, 0.28),
            control_strong: mix(card, text, 0.40),
            divider: with_alpha(mix(card, text, 0.16), 46),
            text,
            dim,
            disabled: mix(dim, card, 0.35),
            accent,
            accent_hover: mix(accent, text, 0.10),
            accent_active: mix(accent, text, 0.06),
            on_accent: window,
            selection: accent,
            on_selection: window,
            scrim: Color32::from_black_alpha(140),
            danger: rgb([0xC4, 0x2B, 0x1C]),
            on_danger: Color32::WHITE,
            focus_outer: accent,
            focus_inner: card,
        }
    }

    /// Windows 11's system palette, from the WinUI 3 theme resources.
    ///
    /// `accent` is the user's system accent; the XAML derives its hover and
    /// pressed variants by drawing the same brush at 90 % and 80 %, which is what
    /// [`Palette::accent_hover`] and [`Palette::accent_active`] do here.
    pub fn windows(dark: bool, accent: Color32) -> Self {
        Self::windows_with_accent(dark, accent, None)
    }

    /// The same palette, with the OS's own hover and pressed variants when the
    /// caller could read them.
    ///
    /// WinUI hovers an accent button into `SystemAccentColorDark1` on a light
    /// theme and `Light1` on a dark one, and presses into the next step out;
    /// without the ladder this falls back to compositing the accent at 90 % and
    /// 80 %, which is the closest a single colour can get.
    pub fn windows_with_accent(dark: bool, accent: Color32, ladder: Option<AccentLadder>) -> Self {
        // WinUI's neutral ramp, per theme. The `#AARRGGBB` values are the XAML's
        // own and are composited over the base background below, because most of
        // them carry alpha and a slot here has to be a colour.
        let (base, secondary, text, dim, disabled) = if dark {
            (
                argb(0xFF20_2020),
                argb(0xFF1C_1C1C),
                Color32::WHITE,
                argb(0xC5FF_FFFF),
                argb(0x5DFF_FFFF),
            )
        } else {
            (
                argb(0xFFF3_F3F3),
                argb(0xFFEE_EEEE),
                argb(0xE400_0000),
                argb(0x9E00_0000),
                argb(0x5C00_0000),
            )
        };
        let (control, hover, active) = if dark {
            (0x0FFF_FFFF, 0x15FF_FFFF, 0x08FF_FFFF)
        } else {
            (0xB3FF_FFFF, 0x80F9_F9F9, 0x4DF9_F9F9)
        };
        let (stroke, stroke_strong, divider) = if dark {
            (0x12FF_FFFF, 0x18FF_FFFF, 0x15FF_FFFF)
        } else {
            (0x0F00_0000, 0x2900_0000, 0x0F00_0000)
        };
        // `ControlStrongStrokeColorDefault`: the checkbox and text-field edge.
        let strong = if dark { 0x8AFF_FFFF } else { 0x7200_0000 };
        let on_accent = if dark { Color32::BLACK } else { Color32::WHITE };
        let (focus_outer, focus_inner) = if dark {
            (Color32::WHITE, argb(0xB300_0000))
        } else {
            (argb(0xE400_0000), argb(0xB3FF_FFFF))
        };
        let (danger, on_danger) = if dark {
            (argb(0xFFFF_99A4), Color32::BLACK)
        } else {
            (argb(0xFFC4_2B1C), Color32::WHITE)
        };
        Self {
            window: base,
            card: base,
            well: secondary,
            headerbar: base,
            control: over(base, argb(control)),
            control_hover: over(base, argb(hover)),
            control_active: over(base, argb(active)),
            border: over(base, argb(stroke)),
            border_strong: over(base, argb(stroke_strong)),
            control_strong: over(base, argb(strong)),
            divider: over(base, argb(divider)),
            text: over(base, text),
            dim: over(base, dim),
            disabled: over(base, disabled),
            accent,
            // The ladder's steps are already composited colours from Windows, so
            // they are used as they come; only the fallback blends.
            accent_hover: match ladder {
                Some(ladder) if dark => ladder.light1,
                Some(ladder) => ladder.dark1,
                None => over(base, with_alpha(accent, 230)),
            },
            accent_active: match ladder {
                Some(ladder) if dark => ladder.light2,
                Some(ladder) => ladder.dark2,
                None => over(base, with_alpha(accent, 204)),
            },
            on_accent,
            selection: accent,
            on_selection: on_accent,
            scrim: argb(0x4D00_0000),
            danger,
            on_danger,
            focus_outer,
            focus_inner,
        }
    }

    /// libadwaita's own chrome, from its 1.9.3 stylesheet.
    ///
    /// The surfaces are measured constants and the two values that vary — the
    /// scheme and the accent — are the caller's. [`Palette::adwaita_with`] takes
    /// the rest when a user stylesheet repainted them.
    pub fn adwaita(dark: bool, accent: Color32) -> Self {
        Self::adwaita_with(dark, accent, &Overrides::default())
    }

    /// The same palette, with the named colours a user's `gtk.css` redefined.
    ///
    /// GTK resolves `@window_bg_color` and its siblings through the user's
    /// stylesheet first, so a file that sets them is what a libadwaita window
    /// actually shows; the constants are only the floor. A translucent override
    /// is composited over the surface it is drawn on, which is what GTK does with
    /// the dark sheet's `card_bg_color: RGB(255 255 255 / 8%)`, and the user's
    /// own `accent_fg_color` and `destructive_fg_color` are read rather than
    /// assumed white — a Material theme paints `#000000` on orange and `#601410`
    /// on a pale pink, and white on either is unreadable.
    pub fn adwaita_with(dark: bool, accent: Color32, overrides: &Overrides) -> Self {
        let (window, card, well, headerbar, fg, danger) = if dark {
            (
                rgb([0x22, 0x22, 0x26]),
                // `dialog_bg_color`: the raised container dtr's cards draw on.
                rgb([0x36, 0x36, 0x3A]),
                rgb([0x1D, 0x1D, 0x20]),
                // `headerbar_bg_color`.
                rgb([0x2E, 0x2E, 0x32]),
                Color32::WHITE,
                rgb([0xC0, 0x1C, 0x28]),
            )
        } else {
            (
                rgb([0xFA, 0xFA, 0xFB]),
                rgb([0xFA, 0xFA, 0xFB]),
                rgb([0xFF, 0xFF, 0xFF]),
                rgb([0xFF, 0xFF, 0xFF]),
                // `window_fg_color` is 80 % of `#000006`; over the window that is
                // `#323237`, which is what a label actually shows.
                over(rgb([0xFA, 0xFA, 0xFB]), argb(0xCC00_0006)),
                rgb([0xE0, 0x1B, 0x24]),
            )
        };
        let window = overrides.window.unwrap_or(window);
        let card = overrides.card.map_or(card, |colour| over(window, colour));
        let well = overrides.well.map_or(well, |colour| over(window, colour));
        let headerbar = overrides
            .headerbar
            .map_or(headerbar, |colour| over(window, colour));
        let accent = overrides.accent.unwrap_or(accent);
        let danger = overrides.danger.unwrap_or(danger);
        // `currentColor` in Adwaita is the foreground, so every derived slot is a
        // mix of it over the surface it is drawn on. The text slot itself is that
        // foreground drawn on the window, because a token may carry alpha.
        let fg = overrides.text.unwrap_or(fg);
        let text = over(window, fg);
        let tinted = |t: f32| mix(card, fg, t);
        Self {
            window,
            card,
            well,
            headerbar,
            control: tinted(0.10),
            control_hover: tinted(0.15),
            control_active: tinted(0.30),
            border: tinted(0.15),
            // Adwaita has no stronger border, and dtr needs one it can see.
            border_strong: tinted(0.25),
            control_strong: tinted(0.40),
            divider: with_alpha(tinted(0.15), 46),
            text,
            dim: tinted(0.55),
            disabled: tinted(0.50),
            accent,
            accent_hover: mix(accent, text, 0.10),
            accent_active: mix(accent, text, 0.06),
            on_accent: overrides.on_accent.unwrap_or(Color32::WHITE),
            // A row selection is `color-mix(accent 25%, transparent)` over the
            // view, not a solid accent: the text on top stays readable.
            selection: mix(well, accent, 0.25),
            on_selection: text,
            scrim: Color32::from_black_alpha(140),
            danger,
            on_danger: overrides.on_danger.unwrap_or(Color32::WHITE),
            focus_outer: mix(card, accent, 0.50),
            focus_inner: card,
        }
    }

    /// Apple's macOS palette — a look-alike, not a measurement.
    ///
    /// The slots are AppKit's semantic colours as Apple documents them: a
    /// light surface with white controls, `labelColor` at 85 % of the text
    /// colour, `secondaryLabelColor` at 50 % (55 % in the dark),
    /// `separatorColor` at 10 %, and `selectedContentBackgroundColor` for a
    /// selection. AppKit hands those out as translucent colours — a slot here
    /// has to be opaque — so each is composited over the surface it is drawn
    /// on, the same rule [`Palette::windows_with_accent`] follows for WinUI.
    ///
    /// `accent` is the caller's: [`crate::Choice`] passes Apple's `systemBlue`
    /// when the config names none, and a user's colour when it does. It is *not*
    /// read from a Mac: there is no macOS reader in this program, and a palette
    /// that guessed at a desktop nobody measured would be worse than this one,
    /// which says it is a look-alike.
    pub fn macos(dark: bool, accent: Color32) -> Self {
        let (window, card, well) = if dark {
            (
                rgb([0x1E, 0x1E, 0x1E]),
                rgb([0x1E, 0x1E, 0x1E]),
                rgb([0x16, 0x16, 0x16]),
            )
        } else {
            (
                rgb([0xEC, 0xEC, 0xEC]),
                rgb([0xEC, 0xEC, 0xEC]),
                Color32::WHITE,
            )
        };
        let ink = if dark { Color32::WHITE } else { Color32::BLACK };
        // AppKit's percentages, as alpha over the card. `labelColor` is 85 % of
        // the label ink in either appearance; the two dimmer steps differ.
        let text = over(card, with_alpha(ink, 217));
        let dim = over(card, with_alpha(ink, if dark { 140 } else { 128 }));
        let disabled = over(card, with_alpha(ink, if dark { 77 } else { 66 }));
        // `controlColor`: white in the light, a 25 % white plane in the dark.
        // A control in a dark Mac window is that plane over the window, not a
        // grey of its own, which is why the three states are three alphas.
        let control = if dark {
            over(card, with_alpha(Color32::WHITE, 20))
        } else {
            Color32::WHITE
        };
        let control_hover = if dark {
            over(card, with_alpha(Color32::WHITE, 31))
        } else {
            mix(control, text, 0.05)
        };
        let control_active = if dark {
            over(card, with_alpha(Color32::WHITE, 51))
        } else {
            mix(control, text, 0.10)
        };
        // A control's edge is `separatorColor`-weight over the control itself:
        // a 1-point hairline that separates a white button from a light window.
        let border = over(control, with_alpha(ink, if dark { 31 } else { 38 }));
        let border_strong = over(control, with_alpha(ink, if dark { 56 } else { 64 }));
        // `secondaryLabelColor` is the strongest stroke a resting control has —
        // a checkbox's box and a field's edge, which have to be findable.
        let control_strong = over(control, with_alpha(ink, if dark { 115 } else { 128 }));
        Self {
            window,
            card,
            well,
            headerbar: card,
            control,
            control_hover,
            control_active,
            border,
            border_strong,
            control_strong,
            divider: with_alpha(over(card, with_alpha(ink, 25)), 46),
            text,
            dim,
            disabled,
            accent,
            accent_hover: mix(accent, text, 0.08),
            accent_active: mix(accent, text, 0.18),
            on_accent: readable_on(accent),
            // `selectedContentBackgroundColor`: Apple's own, not the accent —
            // they differ by a shade and the selection is the one people see.
            selection: if dark {
                rgb([0x00, 0x59, 0xD1])
            } else {
                rgb([0x00, 0x64, 0xE1])
            },
            on_selection: Color32::WHITE,
            scrim: Color32::from_black_alpha(140),
            danger: if dark {
                rgb([0xFF, 0x45, 0x3A])
            } else {
                rgb([0xFF, 0x3B, 0x30])
            },
            on_danger: Color32::WHITE,
            // A Mac focus ring is the accent at 50 % outside the control, not a
            // two-tone Windows ring.
            focus_outer: mix(card, accent, 0.50),
            focus_inner: card,
        }
    }

    /// egui's own palette, read out of the `Visuals` this build of egui ships.
    ///
    /// Asked rather than copied, so the preset cannot drift from the egui the
    /// rest of the crate is compiled against. The mapping is one slot to one
    /// field:
    ///
    /// - `window` is `panel_fill`, `card` is `window_fill`, `well` is
    ///   `extreme_bg_color` (the colour egui gives a text edit, and the darkest
    ///   surface it has);
    /// - a control is a *weak* fill in egui — a button is `inactive.weak_bg_fill`,
    ///   while `bg_fill` is the filled box of a checkbox — so the three control
    ///   states come from `weak_bg_fill` and its hovered and active siblings;
    /// - `border` is the grey egui draws separators and window edges in
    ///   (`noninteractive.bg_stroke`), `border_strong` the one it hovers a widget
    ///   with, and `control_strong` the white one it presses with. egui gives a
    ///   resting control no stroke at all, and a checkbox whose edge is the fill
    ///   behind it is one nobody can find;
    /// - the text slots are `Visuals::text_color`, `weak_text_color` (60 % of it)
    ///   and `Visuals::disable`;
    /// - `selection` is egui's own selection, with the colour it writes on it.
    ///
    /// The accent is not egui's: egui has no accent, only a selection, so the
    /// caller supplies one and [`readable_on`] picks the ink for it. `faint_bg_color`
    /// is deliberately unused — it is `[5, 5, 5, 0]`, a translucent grey meant to
    /// be *added* to whatever is behind it, and a palette slot has to be a colour.
    pub fn egui(dark: bool, accent: Color32) -> Self {
        let visuals = if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        let widgets = &visuals.widgets;
        let text = visuals.text_color();
        let card = visuals.window_fill;
        Self {
            window: visuals.panel_fill,
            card,
            well: visuals.extreme_bg_color,
            headerbar: card,
            control: widgets.inactive.weak_bg_fill,
            control_hover: widgets.hovered.weak_bg_fill,
            control_active: widgets.active.weak_bg_fill,
            border: widgets.noninteractive.bg_stroke.color,
            border_strong: widgets.hovered.bg_stroke.color,
            control_strong: widgets.active.bg_stroke.color,
            divider: widgets.noninteractive.bg_stroke.color,
            text,
            dim: visuals.weak_text_color(),
            disabled: visuals.disable(text),
            accent,
            accent_hover: mix(accent, text, 0.10),
            accent_active: mix(accent, text, 0.06),
            on_accent: readable_on(accent),
            selection: visuals.selection.bg_fill,
            on_selection: visuals.selection.stroke.color,
            scrim: Color32::from_black_alpha(140),
            danger: visuals.error_fg_color,
            on_danger: Color32::WHITE,
            // egui has no focus ring (`Focus::None` in its style), so these two
            // are the values a style that asked for one would use.
            focus_outer: accent,
            focus_inner: card,
        }
    }

    /// Whether the card is dark, which decides egui's `dark_mode` and the
    /// glyph tints that have to follow it.
    pub fn is_dark(&self) -> bool {
        let c = self.card;
        let luma = 0.299 * f32::from(c.r()) + 0.587 * f32::from(c.g()) + 0.114 * f32::from(c.b());
        luma < 128.0
    }
}

/// Black or white, whichever can be read on `accent`.
///
/// Adwaita does not do this — libadwaita injects white on every accent, yellow
/// included, and matching the desktop is the point there (see
/// [`Palette::adwaita`]). A palette with no platform to match has no such licence,
/// so the one thing it must not do is draw white on a light yellow.
pub fn readable_on(accent: Color32) -> Color32 {
    let luma = 0.299 * f32::from(accent.r())
        + 0.587 * f32::from(accent.g())
        + 0.114 * f32::from(accent.b());
    if luma >= 128.0 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

/// Linear blend of two colours in sRGB; `t` of 0 is `a`, 1 is `b`.
pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let blend = |x: u8, y: u8| {
        (f32::from(x) + (f32::from(y) - f32::from(x)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_unmultiplied(
        blend(a.r(), b.r()),
        blend(a.g(), b.g()),
        blend(a.b(), b.b()),
        blend(a.a(), b.a()),
    )
}

/// `top` drawn over `base`, the way the platform's compositor would.
///
/// The XAML tokens are translucent (`ControlFillColorDefault` is 6 % white in
/// dark mode), and a palette slot has to be a colour a widget can hand to
/// `rect_filled` without knowing what it sits on.
pub fn over(base: Color32, top: Color32) -> Color32 {
    // `Color32` is premultiplied, so a translucent white's `r()` is *not* 255:
    // compositing with it would apply the alpha twice and land too dark. Ask for
    // the unmultiplied channels, which is what the XAML numbers are written in.
    let [r, g, b, a] = top.to_srgba_unmultiplied();
    let a = f32::from(a) / 255.0;
    let blend = |x: u8, y: u8| {
        (f32::from(y) * a + f32::from(x) * (1.0 - a))
            .round()
            .clamp(0.0, 255.0) as u8
    };
    match a {
        0.0 => base,
        x if x >= 1.0 => Color32::from_rgb(r, g, b),
        _ => Color32::from_rgb(blend(base.r(), r), blend(base.g(), g), blend(base.b(), b)),
    }
}

/// The same colour at a different alpha.
pub fn with_alpha(colour: Color32, alpha: u8) -> Color32 {
    let [r, g, b, _] = colour.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, alpha)
}

fn rgb(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

/// A `#AARRGGBB` constant, the way the XAML writes it.
fn argb(value: u32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        ((value >> 16) & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        (value & 0xFF) as u8,
        ((value >> 24) & 0xFF) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hover the OS chose beats a hover computed from one colour.
    #[test]
    fn the_accent_ladder_wins_over_the_blend() {
        let accent = rgb([0x00, 0x78, 0xD4]);
        let ladder = AccentLadder {
            light3: rgb([0x99, 0xEB, 0xFF]),
            light2: rgb([0x4C, 0xC2, 0xFF]),
            light1: rgb([0x00, 0x91, 0xF8]),
            base: accent,
            dark1: rgb([0x00, 0x67, 0xC0]),
            dark2: rgb([0x00, 0x3E, 0x92]),
            dark3: rgb([0x00, 0x1A, 0x68]),
            secondary: rgb([0xF7, 0x63, 0x0C]),
        };
        // Light hovers *darker*, dark hovers *lighter*: WinUI's own rule, and the
        // ladder is ordered so either step is one lookup.
        let light = Palette::windows_with_accent(false, accent, Some(ladder));
        assert_eq!(light.accent_hover, ladder.dark1);
        assert_eq!(light.accent_active, ladder.dark2);
        let dark = Palette::windows_with_accent(true, accent, Some(ladder));
        assert_eq!(dark.accent_hover, ladder.light1);
        assert_eq!(dark.accent_active, ladder.light2);
        // Without a ladder the old blend is still there, so a machine that did not
        // answer cannot break the skin.
        let blended = Palette::windows(false, accent);
        assert_ne!(blended.accent_hover, ladder.dark1);
        assert_ne!(blended.accent_hover, ladder.light1);
    }

    /// The compositing is the part that is easy to get wrong in the other
    /// direction: a slot with alpha still in it paints a translucent control,
    /// which looks like a hover that never quite lands.
    #[test]
    fn winui_slots_are_opaque() {
        for palette in [
            Palette::windows(true, Color32::from_rgb(0x60, 0xCD, 0xFF)),
            Palette::windows(false, Color32::from_rgb(0x00, 0x5F, 0xB8)),
        ] {
            for (name, colour) in [
                ("control", palette.control),
                ("control_hover", palette.control_hover),
                ("control_active", palette.control_active),
                ("border", palette.border),
                ("border_strong", palette.border_strong),
                ("control_strong", palette.control_strong),
                ("text", palette.text),
                ("dim", palette.dim),
                ("disabled", palette.disabled),
                ("divider", palette.divider),
            ] {
                assert_eq!(colour.a(), 255, "{name} kept its alpha");
            }
        }
    }

    /// A light palette has to say so: everything that asks `is_dark` — egui's
    /// own `dark_mode`, the glyph tints — follows this one answer.
    #[test]
    fn windows_light_and_dark_are_not_the_same_palette() {
        let accent = Color32::from_rgb(0x00, 0x5F, 0xB8);
        let light = Palette::windows(false, accent);
        let dark = Palette::windows(true, accent);
        assert!(!light.is_dark());
        assert!(dark.is_dark());
        // White text on a dark card, near-black text on a light one.
        assert!(dark.text.r() > 200 && light.text.r() < 60);
    }

    /// `strong` has to be stronger than the hairline in every palette, or a
    /// checkbox is drawn in a colour that reads as "disabled".
    #[test]
    fn the_strong_stroke_is_stronger_than_the_hairline() {
        let distance = |a: Color32, b: Color32| {
            (i32::from(a.r()) - i32::from(b.r())).abs()
                + (i32::from(a.g()) - i32::from(b.g())).abs()
                + (i32::from(a.b()) - i32::from(b.b())).abs()
        };
        for palette in [
            Palette::dtr([0x0b, 0x0f, 0x0e], [0xdc, 0xdf, 0xdd], [0xff, 0xb0, 0x00]),
            Palette::windows(true, Color32::from_rgb(0x60, 0xCD, 0xFF)),
            Palette::windows(false, Color32::from_rgb(0x00, 0x5F, 0xB8)),
            Palette::adwaita(true, Color32::from_rgb(0x35, 0x84, 0xE4)),
            Palette::adwaita(false, Color32::from_rgb(0x35, 0x84, 0xE4)),
            Palette::macos(true, Color32::from_rgb(0x0A, 0x84, 0xFF)),
            Palette::macos(false, Color32::from_rgb(0x00, 0x7A, 0xFF)),
            Palette::egui(true, Color32::from_rgb(0x5A, 0xAA, 0xFF)),
            Palette::egui(false, Color32::from_rgb(0x00, 0x9B, 0xFF)),
        ] {
            assert!(
                distance(palette.control_strong, palette.card)
                    > distance(palette.border, palette.card),
                "control_strong is no stronger than the hairline"
            );
        }
    }

    /// GNOME's surfaces are libadwaita's own constants, so these are the values a
    /// libadwaita window actually shows — `window_bg_color`, `view_bg_color`,
    /// `dialog_bg_color` — not a dark grey invented for the occasion.
    #[test]
    fn adwaita_surfaces_are_libadwaita_measured() {
        let accent = Color32::from_rgb(0x35, 0x84, 0xE4);
        let dark = Palette::adwaita(true, accent);
        assert_eq!(dark.window, rgb([0x22, 0x22, 0x26]));
        assert_eq!(dark.card, rgb([0x36, 0x36, 0x3A]));
        assert_eq!(dark.well, rgb([0x1D, 0x1D, 0x20]));
        // `headerbar_bg_color`: the title bar is part of the window, not a
        // popover, and a tab strip painted in the card was one shade too light.
        assert_eq!(dark.headerbar, rgb([0x2E, 0x2E, 0x32]));
        assert_eq!(dark.text, Color32::WHITE);
        assert_eq!(dark.danger, rgb([0xC0, 0x1C, 0x28]));
        assert!(dark.is_dark());

        let light = Palette::adwaita(false, accent);
        assert_eq!(light.window, rgb([0xFA, 0xFA, 0xFB]));
        assert_eq!(light.card, rgb([0xFA, 0xFA, 0xFB]));
        assert_eq!(light.well, Color32::WHITE);
        assert_eq!(light.headerbar, Color32::WHITE);
        // `window_fg_color` is 80 % of `#000006`, composited over the window.
        assert_eq!(light.text, rgb([0x32, 0x32, 0x37]));
        assert_eq!(light.danger, rgb([0xE0, 0x1B, 0x24]));
        assert!(!light.is_dark());

        // `is_dark` follows the card, and the card is the measured value — a
        // palette that says "dark" while its card is light flips egui's theme, the
        // glyph tints and the popup style together.
        assert!(dark.is_dark() && !light.is_dark());
    }

    /// The derived slots follow from the measured ones by Adwaita's own rule:
    /// 10 %, 15 % and 30 % of `currentColor` over the card for a control, its hover
    /// and its press; 55 % for dim text.
    #[test]
    fn adwaita_derives_its_controls_from_the_card() {
        let card = rgb([0x36, 0x36, 0x3A]);
        let palette = Palette::adwaita(true, Color32::from_rgb(0x35, 0x84, 0xE4));
        assert_eq!(palette.control, mix(card, Color32::WHITE, 0.10));
        assert_eq!(palette.control_hover, mix(card, Color32::WHITE, 0.15));
        assert_eq!(palette.control_active, mix(card, Color32::WHITE, 0.30));
        assert_eq!(palette.border, mix(card, Color32::WHITE, 0.15));
        // The memo's worked example: 55 % white over `#36363a`.
        assert_eq!(palette.dim, rgb([0xA5, 0xA5, 0xA6]));
        // A row selection is the accent at 25 % over the view, not a solid fill,
        // or the text on top stops being readable.
        assert_eq!(palette.selection, mix(palette.well, palette.accent, 0.25));
        assert_ne!(palette.selection, palette.accent);
    }

    /// What a customised GNOME actually shows: the user's `gtk.css` repaints
    /// every surface, and its `accent_fg_color`/`destructive_fg_color` are read
    /// rather than assumed white — a Material theme paints black on orange and
    /// `#601410` on a pale pink, and white on either is unreadable.
    #[test]
    fn adwaita_takes_the_colours_a_user_stylesheet_redefined() {
        let overrides = Overrides {
            window: Some(rgb([0x27, 0x1E, 0x1A])),
            card: Some(rgb([0x32, 0x28, 0x24])),
            well: Some(rgb([0x23, 0x1A, 0x16])),
            headerbar: Some(rgb([0x27, 0x1E, 0x1A])),
            text: Some(rgb([0xF0, 0xDF, 0xD8])),
            accent: Some(rgb([0xFF, 0x6D, 0x00])),
            on_accent: Some(Color32::BLACK),
            danger: Some(rgb([0xF2, 0xB8, 0xB5])),
            on_danger: Some(rgb([0x60, 0x14, 0x10])),
        };
        let palette = Palette::adwaita_with(true, Color32::from_rgb(0x35, 0x84, 0xE4), &overrides);
        assert_eq!(palette.window, rgb([0x27, 0x1E, 0x1A]));
        assert_eq!(palette.card, rgb([0x32, 0x28, 0x24]));
        assert_eq!(palette.well, rgb([0x23, 0x1A, 0x16]));
        assert_eq!(palette.headerbar, rgb([0x27, 0x1E, 0x1A]));
        assert_eq!(palette.text, rgb([0xF0, 0xDF, 0xD8]));
        assert_eq!(palette.accent, rgb([0xFF, 0x6D, 0x00]));
        assert_eq!(palette.on_accent, Color32::BLACK);
        assert_eq!(palette.danger, rgb([0xF2, 0xB8, 0xB5]));
        assert_eq!(palette.on_danger, rgb([0x60, 0x14, 0x10]));
        // The derived slots follow the same rule from the new base, and `is_dark`
        // still follows the card, which is the one surface a window is.
        assert_eq!(
            palette.control,
            mix(palette.card, rgb([0xF0, 0xDF, 0xD8]), 0.10)
        );
        assert_eq!(palette.selection, mix(palette.well, palette.accent, 0.25));
        assert!(palette.is_dark());
    }

    /// `card_bg_color` is 8 % white in the dark sheet and the foreground is 80 %
    /// ink in the light one: a slot has to be opaque, so a translucent override is
    /// composited over the window it is drawn on.
    #[test]
    fn adwaita_composites_a_translucent_override() {
        let translucent = Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 0x14);
        let overrides = Overrides {
            window: Some(rgb([0x20, 0x20, 0x20])),
            card: Some(translucent),
            ..Overrides::default()
        };
        let palette = Palette::adwaita_with(true, Color32::BLUE, &overrides);
        assert_eq!(palette.card, over(rgb([0x20, 0x20, 0x20]), translucent));
        assert_eq!(palette.card.a(), 255);
    }

    /// libadwaita 1.9.3 injects white `accent_fg_color` for every accent,
    /// including the yellow ones. Matching the desktop means matching this; the
    /// contrast complaint belongs to the library, not to a palette that quietly
    /// improves on it and disagrees with every native app.
    #[test]
    fn adwaita_puts_white_on_every_accent() {
        for accent in [
            Color32::from_rgb(0x35, 0x84, 0xE4), // blue
            Color32::from_rgb(0xC8, 0x88, 0x00), // yellow
            Color32::from_rgb(0xED, 0x5B, 0x00), // orange
        ] {
            for dark in [true, false] {
                assert_eq!(Palette::adwaita(dark, accent).on_accent, Color32::WHITE);
            }
        }
    }

    /// macOS' surfaces are the ones AppKit names: a light window grey with
    /// white controls, and a dark window whose controls are a white plane over
    /// it rather than a grey of their own.
    #[test]
    fn macos_derives_its_controls_from_appkit_s_semantic_colours() {
        let accent = Color32::from_rgb(0x00, 0x7A, 0xFF);
        let light = Palette::macos(false, accent);
        assert_eq!(light.window, rgb([0xEC, 0xEC, 0xEC]));
        assert_eq!(light.card, rgb([0xEC, 0xEC, 0xEC]));
        assert_eq!(light.well, Color32::WHITE);
        // `labelColor` is 85 % of black over the window, and that is #232323.
        assert_eq!(light.text, rgb([0x23, 0x23, 0x23]));
        // `secondaryLabelColor` is 50 % over the window.
        assert_eq!(light.dim, rgb([0x76, 0x76, 0x76]));
        assert_eq!(light.control, Color32::WHITE);
        assert!(!light.is_dark());
        assert_eq!(light.accent, accent);
        assert_eq!(
            light.selection,
            rgb([0x00, 0x64, 0xE1]),
            "the selection is `selectedContentBackgroundColor`, not the accent"
        );

        let dark = Palette::macos(true, Color32::from_rgb(0x0A, 0x84, 0xFF));
        assert_eq!(dark.window, rgb([0x1E, 0x1E, 0x1E]));
        assert_eq!(dark.card, rgb([0x1E, 0x1E, 0x1E]));
        // A dark control is 8 % white over the window, not a listed grey.
        assert_eq!(
            dark.control,
            over(dark.card, with_alpha(Color32::WHITE, 20))
        );
        // `labelColor` is 85 % of white over the window.
        assert_eq!(dark.text, rgb([0xDD, 0xDD, 0xDD]));
        assert!(dark.is_dark());
        assert_eq!(dark.selection, rgb([0x00, 0x59, 0xD1]));
        assert_ne!(light.accent, dark.accent, "Apple's two system blues");
    }

    /// Apple puts white on its blue, and the failsafe is the same one egui's
    /// palette uses: the ink is chosen from the accent's luminance, so a person
    /// who picks a pale yellow still gets black on it.
    #[test]
    fn macos_picks_readable_ink_on_its_accent() {
        assert_eq!(
            Palette::macos(false, Color32::from_rgb(0x00, 0x7A, 0xFF)).on_accent,
            Color32::WHITE
        );
        assert_eq!(
            Palette::macos(true, Color32::from_rgb(0x0A, 0x84, 0xFF)).on_accent,
            Color32::WHITE
        );
        assert_eq!(
            Palette::macos(false, Color32::from_rgb(0xFF, 0xCC, 0x00)).on_accent,
            Color32::BLACK
        );
    }

    /// The egui preset is egui's own numbers, asked of the egui that is compiled
    /// in: a copy of them would be a palette that stops matching the widgets it
    /// sits beside after a dependency bump.
    #[test]
    fn egui_surfaces_are_egui_s_own() {
        let accent = Color32::from_rgb(0x5A, 0xAA, 0xFF);
        for (dark, visuals) in [
            (true, egui::Visuals::dark()),
            (false, egui::Visuals::light()),
        ] {
            let palette = Palette::egui(dark, accent);
            assert_eq!(palette.window, visuals.panel_fill);
            assert_eq!(palette.card, visuals.window_fill);
            assert_eq!(palette.well, visuals.extreme_bg_color);
            // A button is a weak fill; the box of a checkbox is a strong one, and
            // using that for every control would make the card a wall of boxes.
            assert_eq!(palette.control, visuals.widgets.inactive.weak_bg_fill);
            assert_eq!(palette.control_hover, visuals.widgets.hovered.weak_bg_fill);
            assert_eq!(palette.text, visuals.text_color());
            assert_eq!(palette.selection, visuals.selection.bg_fill);
            assert_eq!(palette.accent, accent, "the caller's accent, not egui's");
            assert_eq!(palette.is_dark(), dark);
        }
    }

    /// The ink on an accent is chosen, not assumed: egui has no accent to have an
    /// opinion about, and white on a pale yellow is unreadable.
    #[test]
    fn the_ink_on_an_accent_is_the_one_that_reads() {
        // Adwaita's blue and Windows' are dark enough for white.
        assert_eq!(
            readable_on(Color32::from_rgb(0x35, 0x84, 0xE4)),
            Color32::WHITE
        );
        assert_eq!(
            readable_on(Color32::from_rgb(0x20, 0x28, 0x40)),
            Color32::WHITE
        );
        // The pale ones are not.
        assert_eq!(
            readable_on(Color32::from_rgb(0xC8, 0x88, 0x00)),
            Color32::BLACK
        );
        assert_eq!(readable_on(Color32::BLACK), Color32::WHITE);
        assert_eq!(readable_on(Color32::WHITE), Color32::BLACK);
    }

    #[test]
    fn dtr_keeps_the_theme_and_lifts_the_surface() {
        let palette = Palette::dtr([0x0b, 0x0f, 0x0e], [0xdc, 0xdf, 0xdd], [0xff, 0xb0, 0x00]);
        assert_eq!(palette.window, rgb([0x0b, 0x0f, 0x0e]));
        assert_eq!(palette.text, rgb([0xdc, 0xdf, 0xdd]));
        assert_eq!(palette.accent, rgb([0xff, 0xb0, 0x00]));
        // 10 % of the way from the terminal background to its foreground.
        assert_eq!(palette.card, mix(palette.window, palette.text, 0.10));
        assert!(palette.is_dark());
    }

    #[test]
    fn over_composites_the_way_the_platform_does() {
        let base = Color32::from_rgb(0x20, 0x20, 0x20);
        let white_6 = argb(0x0FFF_FFFF);
        let composited = over(base, white_6);
        assert_eq!(composited.a(), 255);
        // 6 % white over #202020 is a hair lighter than the base.
        assert!(composited.r() > 0x20 && composited.r() < 0x40);
        assert_eq!(over(base, Color32::TRANSPARENT), base);
        assert_eq!(over(base, Color32::WHITE), Color32::WHITE);
    }
}
