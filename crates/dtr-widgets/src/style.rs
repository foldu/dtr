//! The shape of a control: tokens, not pictures.
//!
//! A style is more than a colour scheme. The same button drawn with a 4-point
//! radius, a 23-point row and a monospace label reads as dtr; at 32 points with a
//! system face, a hairline and a two-tone focus ring it reads as Windows. So the
//! tokens here include the few *layout* decisions a dialog needs — where the
//! title goes, how a section is announced, which footer button comes first — and
//! not only radii and heights.
//!
//! Two presets exist. [`Style::dtr`] is the chrome the terminal always had.
//! [`Style::native`] is the platform's own: the metrics are Windows 11's (a
//! 32-point control, a 4-point radius, a 1-point hairline, an 8-point overlay)
//! and the colours come from the system palette beside it in [`crate::Palette`].
//! The tokens are a value rather than a `cfg!` so both can be rendered on one
//! machine — the same trick `chrome::buttons_for` uses for the window buttons,
//! and the only way the Windows look stays honest without a VM.

use egui::epaint::Shadow;
use egui::{Color32, FontId, Margin, Vec2};

/// Which chrome a window asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// The terminal's own: monospace, tight, accent-lit.
    Dtr,
    /// The platform's own: system palette, system metrics.
    Native,
}

impl Preset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dtr => "dtr",
            Self::Native => "native",
        }
    }

    /// Parse a config value. Unknown names are the caller's error to report; a
    /// silently-ignored style is a window that never looks like it was asked to.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "dtr" => Some(Self::Dtr),
            "native" => Some(Self::Native),
            _ => None,
        }
    }
}

/// How a card announces itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Title {
    /// A small dim eyebrow above a large name — dtr's header.
    Eyebrow,
    /// A title bar with a close cross, the way a system dialog opens.
    Bar,
}

/// How a group of rows is separated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// Small dim capitals with a hairline rule to their right.
    Rule,
    /// A bold group title, no rule.
    Bold,
}

/// Which way round the footer's buttons go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Footer {
    /// `cancel  connect` — the primary last, nearest the corner.
    PrimaryLast,
    /// `connect  cancel` — Windows' order, the primary first.
    PrimaryFirst,
}

/// What a focused control gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    /// Nothing: hover is the whole affordance.
    None,
    /// A two-tone ring, drawn from `Palette::focus_outer` and `focus_inner`.
    Ring,
}

/// Everything a widget needs to know about how it should look.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    /// Whether labels are monospace. A terminal's chrome says so; a system
    /// dialog does not.
    pub mono: bool,
    pub label_font: FontId,
    pub value_font: FontId,
    pub title_font: FontId,
    /// The height of a control and of a row: the single number the density of a
    /// whole dialog follows from.
    pub row: f32,
    /// Space between rows and between sibling controls.
    pub gap: f32,
    /// Width of the label column, before a control.
    pub label_width: f32,
    /// Padding inside a control, `(x, y)`.
    pub padding: Vec2,
    pub control_radius: u8,
    pub card_radius: u8,
    /// A tab chip, a segmented choice: smaller than a card, larger than nothing.
    pub chip_radius: u8,
    /// The radius of a checkbox's box, which is not a chip's: Adwaita rounds a
    /// 20-point box by 6 (`check { border-radius: 6px }`) while a tab is a pill,
    /// and one radius for both draws either a circle that is not a checkbox or a
    /// chip that does not look like one.
    pub check_radius: u8,
    /// The side of a checkbox. Windows draws a 20-point box; dtr's sheet is
    /// denser and asks for 16, which is also the smallest size the tick inside
    /// it stays legible at.
    pub check_size: f32,
    /// Border width in points; the platform's hairline is 1.
    pub border: f32,
    pub card_margin: Margin,
    pub title: Title,
    pub section: Section,
    pub footer: Footer,
    pub focus: Focus,
    /// The card's drop shadow, when the platform has one.
    pub shadow: Option<Shadow>,
}

impl Style {
    /// dtr's own chrome: what the serial sheet and the tab strip look like today.
    pub fn dtr() -> Self {
        Self {
            mono: true,
            label_font: FontId::monospace(11.5),
            value_font: FontId::monospace(11.5),
            title_font: FontId::monospace(18.0),
            row: 23.0,
            gap: 5.0,
            label_width: 82.0,
            padding: Vec2::new(8.0, 3.0),
            control_radius: 4,
            card_radius: 10,
            chip_radius: 4,
            check_radius: 4,
            check_size: 16.0,
            border: 1.0,
            card_margin: Margin::symmetric(20, 18),
            title: Title::Eyebrow,
            section: Section::Rule,
            footer: Footer::PrimaryLast,
            focus: Focus::None,
            shadow: None,
        }
    }

    /// GNOME's, from the metrics in libadwaita 1.9.3's stylesheet: a 34-point
    /// entry (`spinbutton, entry { min-height: 34px }`), a 9-point control
    /// radius (`border-radius: 9px`), a 12-point card (`.card { border-radius:
    /// 12px }`) wearing that rule's own shadow, and a bold section label.
    ///
    /// The font sizes are the one part not measured: GNOME's is a font *name*
    /// (`Cantarell 11`) resolved by fontconfig, and egui's fallback chain finds
    /// the same face on the same desktop. A headerbar, where a GNOME window
    /// puts its title, is a `Title::Bar`.
    pub fn adwaita() -> Self {
        Self {
            mono: false,
            label_font: FontId::proportional(12.0),
            value_font: FontId::proportional(12.0),
            title_font: FontId::proportional(20.0),
            row: 34.0,
            gap: 8.0,
            label_width: 110.0,
            padding: Vec2::new(12.0, 5.0),
            control_radius: 9,
            card_radius: 12,
            // A chip is a control, and Adwaita gives controls one radius.
            chip_radius: 9,
            // ...but a checkbox is a rounded square: `check { border-radius:
            // 6px }`, against `radio { border-radius: 100% }` a few lines later.
            check_radius: 6,
            check_size: 20.0,
            border: 1.0,
            card_margin: Margin::symmetric(24, 20),
            title: Title::Bar,
            section: Section::Bold,
            // A GNOME dialog's suggested action is the right-hand one.
            footer: Footer::PrimaryLast,
            focus: Focus::Ring,
            // `.card`'s box-shadow is three layers; the middle one (`0 1px 3px
            // 1px RGB(0 0 6/7%)`) is the part that says "raised".
            shadow: Some(Shadow {
                offset: [0, 1],
                blur: 3,
                spread: 1,
                color: Color32::from_rgba_unmultiplied(0, 0, 6, 18),
            }),
        }
    }

    /// Windows 11's own metrics: 32-point controls, a 4-point control radius, an
    /// 8-point overlay radius, a hairline, a two-tone ring, a shadow.
    ///
    /// `Segoe UI Variable Text` is what Windows 11 draws UI text in; the fallback
    /// chain is egui's, which finds the platform's face through fontique when the
    /// system has one.
    pub fn native() -> Self {
        Self {
            mono: false,
            label_font: FontId::proportional(12.0),
            value_font: FontId::proportional(12.0),
            title_font: FontId::proportional(20.0),
            row: 32.0,
            gap: 8.0,
            label_width: 110.0,
            padding: Vec2::new(12.0, 5.0),
            control_radius: 4,
            card_radius: 8,
            chip_radius: 4,
            check_radius: 4,
            check_size: 20.0,
            border: 1.0,
            card_margin: Margin::symmetric(24, 20),
            title: Title::Bar,
            section: Section::Bold,
            footer: Footer::PrimaryFirst,
            focus: Focus::Ring,
            shadow: Some(Shadow {
                offset: [0, 8],
                blur: 24,
                spread: 0,
                color: Color32::from_black_alpha(96),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The preset is what the config names and what the crate documents; a new
    /// spelling here without a parser arm is a setting nobody can select.
    #[test]
    fn presets_parse_their_own_names() {
        for preset in [Preset::Dtr, Preset::Native] {
            assert_eq!(Preset::parse(preset.as_str()), Some(preset));
        }
        assert_eq!(Preset::parse("NATIVE"), Some(Preset::Native));
        assert_eq!(Preset::parse("system"), None);
    }

    /// Native is denser and finer than dtr in every direction that matters, and
    /// the two are different enough that a snapshot of one cannot pass for the
    /// other — a style that renders identically is a style that is not applied.
    #[test]
    fn the_two_presets_are_actually_different() {
        let dtr = Style::dtr();
        let native = Style::native();
        assert!(native.row > dtr.row);
        assert!(native.label_width > dtr.label_width);
        assert!(native.card_radius < dtr.card_radius);
        assert!(dtr.mono && !native.mono);
        assert_eq!(dtr.focus, Focus::None);
        assert_eq!(native.focus, Focus::Ring);
        assert_eq!(dtr.footer, Footer::PrimaryLast);
        assert_eq!(native.footer, Footer::PrimaryFirst);
        assert_eq!(dtr.title, Title::Eyebrow);
        assert_eq!(native.title, Title::Bar);
        assert!(native.shadow.is_some() && dtr.shadow.is_none());
    }

    /// GNOME's metrics, from the numbers in libadwaita 1.9.3's stylesheet rather
    /// than from a guess: a 34-point entry, a 9-point control radius, a 12-point
    /// card. A native skin on Linux that kept dtr's 23-point rows would be a
    /// palette over the wrong shape, which reads as "theming" rather than as
    /// "a window on this desktop".
    #[test]
    fn adwaita_takes_its_metrics_from_the_stylesheet() {
        let adwaita = Style::adwaita();
        // `spinbutton, entry { min-height: 34px }`.
        assert_eq!(adwaita.row, 34.0);
        // `border-radius: 9px` on the same rule; `.card { border-radius: 12px }`.
        assert_eq!(adwaita.control_radius, 9);
        assert_eq!(adwaita.card_radius, 12);
        // GNOME's UI text is proportional, its dialogs put the suggested action on
        // the right, and its sections are bold labels rather than rules.
        assert!(!adwaita.mono);
        assert_eq!(adwaita.footer, Footer::PrimaryLast);
        assert_eq!(adwaita.section, Section::Bold);
        assert!(adwaita.shadow.is_some());
    }
}
