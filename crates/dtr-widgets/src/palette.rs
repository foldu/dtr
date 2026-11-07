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

    /// GNOME's palette, from libadwaita 1.9.3's own stylesheet.
    ///
    /// The surfaces are measured out of the gresource — `window_bg_color`,
    /// `view_bg_color`, `dialog_bg_color` — and the light foreground is the
    /// `RGB(0 0 6 / 80%)` that stylesheet declares, composited over the window
    /// because a slot here has to be a colour. The rest is Adwaita's own rule
    /// applied to them: a control is `color-mix(currentColor 10%, transparent)`
    /// over the card, a border 15 %, dim text 55 %, disabled 50 % — the three
    /// opacities in the sheet's `:root` — and a focus ring is the accent at 50 %.
    ///
    /// `on_accent` is white for *every* accent, the yellow and orange ones
    /// included, because that is what libadwaita injects. Matching the desktop
    /// means matching this rather than improving on it; the contrast complaint
    /// belongs to the library.
    pub fn adwaita(dark: bool, accent: Color32) -> Self {
        let (window, card, well, fg, danger) = if dark {
            (
                rgb([0x22, 0x22, 0x26]),
                rgb([0x36, 0x36, 0x3A]),
                rgb([0x1D, 0x1D, 0x20]),
                Color32::WHITE,
                rgb([0xC0, 0x1C, 0x28]),
            )
        } else {
            (
                rgb([0xFA, 0xFA, 0xFB]),
                rgb([0xFA, 0xFA, 0xFB]),
                rgb([0xFF, 0xFF, 0xFF]),
                // `window_fg_color` is 80 % of `#000006`; over the window that is
                // `#323237`, which is what a label actually shows.
                over(rgb([0xFA, 0xFA, 0xFB]), argb(0xCC00_0006)),
                rgb([0xE0, 0x1B, 0x24]),
            )
        };
        // `currentColor` in Adwaita is the foreground, so every derived slot is
        // a mix of it over the surface it is drawn on.
        let tinted = |t: f32| mix(card, fg, t);
        Self {
            window,
            card,
            well,
            control: tinted(0.10),
            control_hover: tinted(0.15),
            control_active: tinted(0.30),
            border: tinted(0.15),
            // Adwaita has no stronger border, and dtr needs one it can see.
            border_strong: tinted(0.25),
            control_strong: tinted(0.40),
            divider: with_alpha(tinted(0.15), 46),
            text: fg,
            dim: tinted(0.55),
            disabled: tinted(0.50),
            accent,
            accent_hover: mix(accent, fg, 0.10),
            accent_active: mix(accent, fg, 0.06),
            on_accent: Color32::WHITE,
            // A row selection is `color-mix(accent 25%, transparent)` over the
            // view, not a solid accent: the text on top stays readable.
            selection: mix(well, accent, 0.25),
            on_selection: fg,
            scrim: Color32::from_black_alpha(140),
            danger,
            on_danger: Color32::WHITE,
            focus_outer: mix(card, accent, 0.50),
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
        assert_eq!(dark.text, Color32::WHITE);
        assert_eq!(dark.danger, rgb([0xC0, 0x1C, 0x28]));
        assert!(dark.is_dark());

        let light = Palette::adwaita(false, accent);
        assert_eq!(light.window, rgb([0xFA, 0xFA, 0xFB]));
        assert_eq!(light.card, rgb([0xFA, 0xFA, 0xFB]));
        assert_eq!(light.well, Color32::WHITE);
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
