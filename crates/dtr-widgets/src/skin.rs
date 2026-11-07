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

use crate::palette::Palette;
use crate::style::{Preset, Style};

/// Windows 11's default accent, light and dark.
///
/// Only a fallback: the real one is the user's, read from the system. It exists so
/// a snapshot and a first run have something recognisable rather than a
/// placeholder colour.
pub const WINDOWS_ACCENT_LIGHT: Color32 = Color32::from_rgb(0x00, 0x5F, 0xB8);
pub const WINDOWS_ACCENT_DARK: Color32 = Color32::from_rgb(0x60, 0xCD, 0xFF);

/// Everything a widget draws with.
#[derive(Clone, Debug, PartialEq)]
pub struct Skin {
    pub palette: Palette,
    pub style: Style,
}

impl Skin {
    /// The terminal's own chrome, over whatever palette the theme produced.
    pub fn dtr(palette: Palette) -> Self {
        Self {
            palette,
            style: Style::dtr(),
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
        }
    }

    pub fn windows_light() -> Self {
        Self::windows(false, WINDOWS_ACCENT_LIGHT)
    }

    pub fn windows_dark() -> Self {
        Self::windows(true, WINDOWS_ACCENT_DARK)
    }

    /// The skin a preset names.
    ///
    /// `native` is the platform's own when the caller could read one, and the
    /// dtr skin otherwise. The reading lives in the caller rather than here
    /// because it is platform code — the Windows registry, and nothing on Linux,
    /// where there is no portable system palette and `native` is the dtr style by
    /// decision rather than by accident.
    pub fn resolve(preset: Preset, dtr: Palette, native: Option<Self>) -> Self {
        match preset {
            Preset::Dtr => Self::dtr(dtr),
            Preset::Native => native.unwrap_or_else(|| Self::dtr(dtr)),
        }
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
        style.spacing.menu_margin = s.card_margin;

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

    /// Without a system palette, `native` is the dtr style — the decision, not a
    /// silent failure.
    #[test]
    fn native_falls_back_to_dtr_where_there_is_no_system_palette() {
        assert_eq!(
            Skin::resolve(Preset::Native, dtr_palette(), None),
            Skin::dtr(dtr_palette())
        );
        assert_eq!(
            Skin::resolve(Preset::Dtr, dtr_palette(), None),
            Skin::dtr(dtr_palette())
        );
    }

    /// A preset is a request, not a promise: `native` uses the system skin when
    /// there is one and `dtr` never does, even then.
    #[test]
    fn the_system_skin_is_used_by_native_alone() {
        let system = Skin::windows_dark();
        assert_eq!(
            Skin::resolve(Preset::Native, dtr_palette(), Some(system.clone())),
            system
        );
        assert_eq!(
            Skin::resolve(Preset::Dtr, dtr_palette(), Some(system)),
            Skin::dtr(dtr_palette())
        );
    }
}
