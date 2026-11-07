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
            border: 1.0,
            card_margin: Margin::symmetric(20, 18),
            title: Title::Eyebrow,
            section: Section::Rule,
            footer: Footer::PrimaryLast,
            focus: Focus::None,
            shadow: None,
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
}
