//! A palette and a style, and the `egui::Style` derived from them.
//!
//! The widgets that are still egui's own — a text field, a scroll area, a popup
//! frame, anything a menu draws — take their colours from `egui::Style`, not from
//! our painter. Without a derivation they keep egui's dark grey next to a
//! hand-painted card, which is exactly the two-languages-in-one-box the settings
//! menu had. [`Skin::egui_style`] is that one derivation: every stock widget
//! inherits the same surface, border, radius and control height as the painted
//! ones.
//!
//! The two-tone focus ring a native style wants is drawn by our own widgets; egui's
//! stock widgets get their hover and pressed fills from here and nothing else, so a
//! ring around a stock combo is a painted widget away (`widgets::combo` replaces
//! the stock one).

use egui::{Color32, Context, CornerRadius, Stroke, TextStyle};

use crate::palette::{Overrides, Palette};
use crate::style::{Preset, Scheme, Style};

/// Windows 11's default accent, light and dark.
///
/// Only a fallback: the real one is the user's, read from the system. It exists so
/// a snapshot and a first run have something recognisable rather than a
/// placeholder colour.
pub const WINDOWS_ACCENT_LIGHT: Color32 = Color32::from_rgb(0x00, 0x5F, 0xB8);
pub const WINDOWS_ACCENT_DARK: Color32 = Color32::from_rgb(0x60, 0xCD, 0xFF);

/// Adwaita's default accent — `--accent-blue` in libadwaita 1.9.3.
///
/// The portal always states an accent on a GNOME session that has a preference;
/// this is what a session that has none gets, because Adwaita's own default is
/// blue rather than "whatever the theme's cursor was".
pub const ADWAITA_ACCENT: Color32 = Color32::from_rgb(0x35, 0x84, 0xE4);

/// Apple's `systemBlue` — the default accent of the macOS look-alike.
///
/// Big Sur's light and dark values. They are a look-alike's constants rather
/// than a read of a Mac (there is no macOS reader in this program yet), so a
/// person who wants their own colour sets `[ui] accent`.
pub const MACOS_ACCENT_LIGHT: Color32 = Color32::from_rgb(0x00, 0x7A, 0xFF);
pub const MACOS_ACCENT_DARK: Color32 = Color32::from_rgb(0x0A, 0x84, 0xFF);

/// egui's accent, which is really its hyperlink colour: egui has no accent of
/// its own, and a link is the one thing it draws in a colour that means "act on
/// me". Two values, because egui has two.
pub const EGUI_ACCENT_DARK: Color32 = Color32::from_rgb(0x5A, 0xAA, 0xFF);
pub const EGUI_ACCENT_LIGHT: Color32 = Color32::from_rgb(0x00, 0x9B, 0xFF);

/// Which platform's chrome a skin is.
///
/// Stored rather than inferred. `-v` used to guess from the style — a monospace
/// label meant dtr, otherwise the palette's own lightness — which calls an
/// Adwaita skin "dtr" (it borrows that layout) and would call any non-monospace
/// light skin "windows", and neither is what a person reading the log needs to
/// know when `native` on their desktop produced something unexpected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Dtr,
    Windows,
    Adwaita,
    Macos,
    Egui,
}

/// Everything a widget draws with.
#[derive(Clone, Debug, PartialEq)]
pub struct Skin {
    pub palette: Palette,
    pub style: Style,
    /// Which platform's chrome this is, for `-v` and for a test that has to
    /// tell two palettes apart without looking at a pixel.
    pub origin: Origin,
}

impl Skin {
    /// The terminal's own chrome, over whatever palette the theme produced.
    ///
    /// No preset selects this any more — the menu's fourth look is
    /// [`Skin::egui`] — but it is what `native` falls back to on a machine that
    /// cannot say what its own chrome looks like, and it is the skin dtr's own
    /// snapshots are drawn in.
    pub fn dtr(palette: Palette) -> Self {
        Self {
            palette,
            style: Style::dtr(),
            origin: Origin::Dtr,
        }
    }

    /// GNOME's chrome: Adwaita's palette and metrics, over the user's accent.
    pub fn adwaita(dark: bool, accent: Color32) -> Self {
        Self::adwaita_with(dark, accent, &Overrides::default())
    }

    /// Adwaita's metrics over the named colours a user stylesheet repainted.
    ///
    /// The metrics are Adwaita's whatever the user's file says: a stylesheet
    /// that changes a colour is what GTK shows, and one that changes a row
    /// height is not something dtr can follow without becoming a GTK theme
    /// engine (see `docs/gtk-palette.md` §9).
    pub fn adwaita_with(dark: bool, accent: Color32, overrides: &Overrides) -> Self {
        Self {
            palette: Palette::adwaita_with(dark, accent, overrides),
            style: Style::adwaita(),
            origin: Origin::Adwaita,
        }
    }

    /// Windows 11's chrome over Windows 11's palette.
    pub fn windows(dark: bool, accent: Color32) -> Self {
        Self::windows_with_accent(dark, accent, None)
    }

    /// The platform's skin, with the OS's own accent ladder for its hover and
    /// pressed states. See [`Palette::windows_with_accent`].
    pub fn windows_with_accent(
        dark: bool,
        accent: Color32,
        ladder: Option<crate::palette::AccentLadder>,
    ) -> Self {
        Self {
            palette: Palette::windows_with_accent(dark, accent, ladder),
            style: Style::native(),
            origin: Origin::Windows,
        }
    }

    pub fn windows_light() -> Self {
        Self::windows(false, WINDOWS_ACCENT_LIGHT)
    }

    pub fn windows_dark() -> Self {
        Self::windows(true, WINDOWS_ACCENT_DARK)
    }

    /// Apple's macOS look-alike: AppKit's semantic palette and a regular Mac
    /// control's metrics.
    ///
    /// This is a *look the window carries*, not a read of a Mac — see
    /// [`Palette::macos`] and [`Style::macos`]. `native` on macOS is still the
    /// terminal's own chrome, because nothing here has measured the real
    /// desktop.
    pub fn macos(dark: bool, accent: Color32) -> Self {
        Self {
            palette: Palette::macos(dark, accent),
            style: Style::macos(),
            origin: Origin::Macos,
        }
    }

    pub fn macos_light() -> Self {
        Self::macos(false, MACOS_ACCENT_LIGHT)
    }

    pub fn macos_dark() -> Self {
        Self::macos(true, MACOS_ACCENT_DARK)
    }

    /// egui's own look: its greys, its metrics, its shadow.
    pub fn egui(dark: bool, accent: Color32) -> Self {
        Self {
            palette: Palette::egui(dark, accent),
            style: Style::egui(),
            origin: Origin::Egui,
        }
    }

    /// egui's look with egui's own accent, for the preview switch and a test.
    pub fn egui_light() -> Self {
        Self::egui(false, EGUI_ACCENT_LIGHT)
    }

    pub fn egui_dark() -> Self {
        Self::egui(true, EGUI_ACCENT_DARK)
    }

    /// Which of egui's two styles this skin belongs to. egui keeps a dark and a
    /// light style and picks one per frame; a palette that says it is light must
    /// not inherit the dark one's greys.
    pub fn theme(&self) -> egui::Theme {
        if self.palette.is_dark() {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        }
    }

    /// The `egui::Style` every stock widget inherits.
    pub fn egui_style(&self, ctx: &Context) -> egui::Style {
        let p = &self.palette;
        let s = &self.style;
        let mut style = (*ctx.style_of(self.theme())).clone();

        let radius = CornerRadius::same(s.control_radius);
        let border = Stroke::new(s.border, p.border);
        let strong = Stroke::new(s.border, p.border_strong);

        let idle = egui::style::WidgetVisuals {
            bg_fill: p.control,
            weak_bg_fill: p.control,
            bg_stroke: border,
            corner_radius: radius,
            fg_stroke: Stroke::new(1.0, p.text),
            expansion: 0.0,
        };
        let hovered = egui::style::WidgetVisuals {
            bg_fill: p.control_hover,
            bg_stroke: strong,
            ..idle
        };
        let active = egui::style::WidgetVisuals {
            bg_fill: p.control_active,
            ..hovered
        };

        let widgets = &mut style.visuals.widgets;
        widgets.noninteractive = egui::style::WidgetVisuals {
            bg_fill: p.well,
            weak_bg_fill: p.well,
            bg_stroke: Stroke::new(s.border, p.divider),
            corner_radius: radius,
            fg_stroke: Stroke::new(1.0, p.dim),
            expansion: 0.0,
        };
        widgets.inactive = idle;
        widgets.hovered = hovered;
        widgets.active = active;
        // An open combo box or menu holds the pressed look while its list is down.
        widgets.open = active;

        style.visuals.dark_mode = p.is_dark();
        style.visuals.override_text_color = Some(p.text);
        style.visuals.selection = egui::style::Selection {
            bg_fill: p.selection,
            stroke: Stroke::new(1.0, p.on_selection),
        };
        style.visuals.hyperlink_color = p.accent;
        style.visuals.warn_fg_color = p.danger;
        style.visuals.error_fg_color = p.danger;
        // A text field is a control; a well is a card's inset.
        style.visuals.extreme_bg_color = p.control;
        style.visuals.text_edit_bg_color = Some(p.control);
        style.visuals.code_bg_color = p.well;
        style.visuals.faint_bg_color = p.well;
        style.visuals.panel_fill = p.card;
        style.visuals.window_fill = p.card;
        style.visuals.window_stroke = border;
        style.visuals.window_corner_radius = CornerRadius::same(s.card_radius);
        style.visuals.menu_corner_radius = CornerRadius::same(s.card_radius);
        let shadow = s.shadow.unwrap_or(egui::epaint::Shadow::NONE);
        style.visuals.window_shadow = shadow;
        style.visuals.popup_shadow = shadow;
        style.visuals.text_cursor = egui::style::TextCursorStyle {
            stroke: Stroke::new(1.0, p.accent),
            preview: false,
            ..style.visuals.text_cursor
        };

        style.spacing.interact_size = egui::vec2(0.0, s.row);
        style.spacing.button_padding = s.padding;
        style.spacing.item_spacing = egui::vec2(s.gap, s.gap);
        style.spacing.window_margin = s.card_margin;
        // A popover is not a card: see `Style::menu_margin`.
        style.spacing.menu_margin = s.menu_margin;

        style
            .text_styles
            .insert(TextStyle::Body, s.label_font.clone());
        style
            .text_styles
            .insert(TextStyle::Button, s.value_font.clone());
        style.text_styles.insert(
            TextStyle::Small,
            egui::FontId::new(s.label_font.size - 1.0, s.label_font.family.clone()),
        );
        style
            .text_styles
            .insert(TextStyle::Heading, s.title_font.clone());
        style.text_styles.insert(
            TextStyle::Monospace,
            egui::FontId::monospace(s.label_font.size),
        );

        style
    }

    /// Install this skin on a context.
    ///
    /// Sticky: egui keeps the style, so a surface that resolves a different skin
    /// has to apply it again. A dialog that forgot would draw in the window's
    /// style, which is the bug this crate exists to make impossible per widget.
    pub fn apply(&self, ctx: &Context) {
        let style = self.egui_style(ctx);
        ctx.set_theme(match self.theme() {
            egui::Theme::Dark => egui::ThemePreference::Dark,
            egui::Theme::Light => egui::ThemePreference::Light,
        });
        // Both, on purpose: `set_style_of` is what a popup or a tooltip reads
        // when it starts a new layer, and `set_global_style` is what the current
        // frame's `Ui` reads.
        ctx.set_style_of(self.theme(), style.clone());
        ctx.set_global_style(style);
    }
}

/// What the window was asked to look like: a preset, a scheme, and an accent.
///
/// The three travel together because they are one answer — `adwaita` in the light
/// with the user's orange is a different window from `adwaita` in the dark with
/// its own blue, and a caller that resolved them separately would be a caller
/// that resolved them inconsistently.
///
/// Only [`Preset::owns_its_colours`] reads the scheme and the accent; the other
/// two take theirs from where they come from. See [`Choice::skin`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    pub preset: Preset,
    pub scheme: Scheme,
    /// `None` keeps the preset's own default, so a window that never opened the
    /// preferences card keeps following a future default rather than the value of
    /// the day it was first saved.
    pub accent: Option<Color32>,
}

impl Choice {
    pub fn new(preset: Preset) -> Self {
        Self {
            preset,
            ..Self::default()
        }
    }

    /// The skin this asks for.
    ///
    /// `native` is the platform's skin, falling back to the terminal's own
    /// (`Skin::dtr`) where this machine has no answer; it takes its scheme and
    /// accent from the platform, which is why it does not read this `Choice`'s.
    ///
    /// `system`, for the other three, is the platform's dark flag; where the
    /// platform says nothing, the terminal theme's own lightness stands in, since
    /// a light theme on a machine with no portal is the one hint there is. No hint
    /// at all is dark, which is what the built-in theme is.
    pub fn skin(&self, dtr: Palette, native: Option<Skin>) -> Skin {
        match self.preset {
            Preset::Native => native.unwrap_or_else(|| Skin::dtr(dtr)),
            preset => {
                let system = native
                    .as_ref()
                    .map_or_else(|| dtr.is_dark(), |skin| skin.palette.is_dark());
                let dark = self.scheme.dark(system);
                let accent = self
                    .accent
                    .or_else(|| preset.default_accent(dark))
                    .unwrap_or(ADWAITA_ACCENT);
                match preset {
                    Preset::Adwaita => Skin::adwaita(dark, accent),
                    Preset::Windows => Skin::windows(dark, accent),
                    Preset::Macos => Skin::macos(dark, accent),
                    // `owns_its_colours` and the arm above leave egui.
                    _ => Skin::egui(dark, accent),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dtr_palette() -> Palette {
        Palette::dtr([0x0b, 0x0f, 0x0e], [0xdc, 0xdf, 0xdd], [0xff, 0xb0, 0x00])
    }

    /// The derivation is the contract between the painted widgets and egui's own:
    /// every stock control has to come out in our colours, not egui's.
    #[test]
    fn the_egui_style_carries_the_palette() {
        let skin = Skin::dtr(dtr_palette());
        let ctx = Context::default();
        let style = skin.egui_style(&ctx);
        assert!(style.visuals.dark_mode);
        assert_eq!(style.visuals.panel_fill, skin.palette.card);
        assert_eq!(style.visuals.window_fill, skin.palette.card);
        assert_eq!(style.visuals.extreme_bg_color, skin.palette.control);
        assert_eq!(style.visuals.selection.bg_fill, skin.palette.selection);
        assert_eq!(style.spacing.interact_size.y, skin.style.row);
        assert_eq!(style.spacing.button_padding, skin.style.padding);
        assert_eq!(style.visuals.widgets.inactive.bg_fill, skin.palette.control);
        assert_eq!(
            style.visuals.widgets.hovered.bg_fill,
            skin.palette.control_hover
        );
        assert_eq!(
            style.visuals.window_corner_radius,
            CornerRadius::same(skin.style.card_radius)
        );
        assert_eq!(
            style.text_styles[&TextStyle::Body],
            skin.style.label_font,
            "stock labels must use the style's label face"
        );
    }

    /// A light system palette must not be drawn with egui's dark defaults: the
    /// whole point of the native style is that it follows the OS.
    #[test]
    fn a_light_palette_says_so() {
        let ctx = Context::default();
        let light = Skin::windows_light();
        let style = light.egui_style(&ctx);
        assert!(!style.visuals.dark_mode);
        assert_eq!(style.visuals.panel_fill, light.palette.card);
        assert!(Skin::windows_dark().egui_style(&ctx).visuals.dark_mode);
        // The two presets differ in the numbers a snapshot would catch.
        assert_ne!(
            Skin::dtr(dtr_palette()).style.row,
            light.style.row,
            "dtr and native must not render identically"
        );
    }

    /// Without a system palette, `native` is the terminal's own chrome — the
    /// decision, not a silent failure.
    #[test]
    fn native_falls_back_to_the_terminals_own_chrome() {
        assert_eq!(
            Choice::new(Preset::Native).skin(dtr_palette(), None),
            Skin::dtr(dtr_palette())
        );
        // ...and a look asked for by name never falls back: it is painted here.
        assert_eq!(
            Choice::new(Preset::Egui).skin(dtr_palette(), None),
            Skin::egui(true, crate::skin::EGUI_ACCENT_DARK)
        );
    }

    /// A preset is a request, not a promise: `native` uses the system skin when
    /// there is one and a look asked for by name never does, even then.
    #[test]
    fn the_system_skin_is_used_by_native_alone() {
        // Both platforms' skins, because "the system skin" is not a Windows idea:
        // on a GNOME desktop it is Adwaita's, and the choice does not care which.
        for system in [
            Skin::windows_dark(),
            Skin::adwaita(true, crate::skin::ADWAITA_ACCENT),
        ] {
            assert_eq!(
                Choice::new(Preset::Native).skin(dtr_palette(), Some(system.clone())),
                system
            );
            assert_eq!(
                Choice::new(Preset::Egui).skin(dtr_palette(), Some(system)),
                Skin::egui_dark()
            );
        }
    }

    /// The four painted looks are one `Choice` away from each other, and each
    /// reads the choice's own scheme and accent: `native` ignores both, which is
    /// the line between a look and a source.
    #[test]
    fn a_painted_look_takes_the_chosen_scheme_and_accent() {
        let orange = Color32::from_rgb(0xED, 0x5B, 0x00);
        let dark_system = Some(Skin::adwaita(true, crate::skin::ADWAITA_ACCENT));
        for preset in [
            Preset::Adwaita,
            Preset::Windows,
            Preset::Macos,
            Preset::Egui,
        ] {
            let choice = Choice {
                preset,
                scheme: Scheme::Light,
                accent: Some(orange),
            };
            let skin = choice.skin(dtr_palette(), dark_system.clone());
            assert_eq!(skin.palette.accent, orange);
            assert!(!skin.palette.is_dark(), "{preset:?} ignored `light`");

            // `system` follows the desktop it was handed, not the terminal theme.
            let system = Choice {
                scheme: Scheme::System,
                accent: None,
                ..choice
            }
            .skin(dtr_palette(), dark_system.clone());
            assert!(system.palette.is_dark(), "{preset:?} ignored the desktop");
            // ...and without a desktop answer, the theme's own lightness stands in.
            let themeless = Choice {
                scheme: Scheme::System,
                accent: None,
                ..choice
            }
            .skin(
                Palette::dtr([0xfa, 0xfa, 0xfb], [0x20, 0x20, 0x20], [0x35, 0x84, 0xe4]),
                None,
            );
            assert!(!themeless.palette.is_dark(), "{preset:?} ignored the theme");
        }

        // The two sources keep their own answers whatever the choice says.
        let shouted = Choice {
            preset: Preset::Native,
            scheme: Scheme::Light,
            accent: Some(orange),
        };
        assert_eq!(
            shouted.skin(dtr_palette(), dark_system),
            Skin::adwaita(true, crate::skin::ADWAITA_ACCENT)
        );
    }

    /// A preset with no accent chosen draws its own, per scheme — so the settings
    /// menu can show the value a save would keep.
    #[test]
    fn an_unchosen_accent_is_the_presets_own() {
        assert_eq!(
            Preset::Adwaita.default_accent(true),
            Some(crate::skin::ADWAITA_ACCENT)
        );
        assert_eq!(
            Preset::Windows.default_accent(false),
            Some(crate::skin::WINDOWS_ACCENT_LIGHT)
        );
        assert_eq!(
            Preset::Windows.default_accent(true),
            Some(crate::skin::WINDOWS_ACCENT_DARK)
        );
        assert_eq!(
            Preset::Macos.default_accent(false),
            Some(crate::skin::MACOS_ACCENT_LIGHT)
        );
        assert_eq!(
            Preset::Macos.default_accent(true),
            Some(crate::skin::MACOS_ACCENT_DARK)
        );
        assert_eq!(
            Preset::Egui.default_accent(false),
            Some(crate::skin::EGUI_ACCENT_LIGHT)
        );
        assert_eq!(Preset::Native.default_accent(true), None);
        // What a choice with no accent gets, per preset.
        for (preset, dark) in [
            (Preset::Adwaita, true),
            (Preset::Windows, false),
            (Preset::Macos, true),
            (Preset::Egui, true),
        ] {
            let skin = Choice {
                preset,
                scheme: if dark { Scheme::Dark } else { Scheme::Light },
                accent: None,
            }
            .skin(dtr_palette(), None);
            assert_eq!(skin.palette.accent, preset.default_accent(dark).unwrap());
        }
    }

    /// The origin is what `-v` reads, so a constructor that forgets it would make
    /// the log name the wrong platform. The dtr skin in particular is *not*
    /// identified by its monospace style: an Adwaita skin borrows neither.
    #[test]
    fn every_constructor_names_its_platform() {
        assert_eq!(Skin::dtr(dtr_palette()).origin, Origin::Dtr);
        assert_eq!(Skin::windows_dark().origin, Origin::Windows);
        assert_eq!(
            Skin::adwaita(false, crate::skin::ADWAITA_ACCENT).origin,
            Origin::Adwaita
        );
        assert_eq!(Skin::macos_dark().origin, Origin::Macos);
        assert_eq!(Skin::macos_light().origin, Origin::Macos);
        assert_eq!(Skin::egui_dark().origin, Origin::Egui);
        assert_eq!(Skin::egui_light().origin, Origin::Egui);
    }
}
