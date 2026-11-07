//! Which installed fonts can actually be a terminal grid.
//!
//! Listing every family the system knows is easy and useless: half of them
//! cannot produce a monospaced grid, and a font you can select but cannot use is
//! worse than a font that is not offered. So each family is probed — loaded,
//! asked for `M` and `i`, and kept only if both exist and advance the same
//! width. That is the definition of "works" this terminal actually relies on.

use std::sync::OnceLock;

/// Families that pass the probe, sorted case-insensitively.
///
/// Probing parses a font per family, so the answer is cached: opening the
/// settings menu twice must not cost two full font scans. The names of a
/// machine's fonts do not change while the terminal runs in any way worth
/// noticing.
pub fn usable_families() -> Vec<String> {
    static CACHE: OnceLock<Vec<String>> = OnceLock::new();
    CACHE.get_or_init(probe_all).clone()
}

fn probe_all() -> Vec<String> {
    let started = std::time::Instant::now();
    let mut collection = fontique::Collection::new(fontique::CollectionOptions::default());
    let mut cache = fontique::SourceCache::default();
    let mut usable = Vec::new();
    for id in collection.family_ids().collect::<Vec<_>>() {
        let Some(family) = collection.family(id) else {
            continue;
        };
        let name = family.name().to_owned();
        if family.fonts().iter().any(|font| {
            font.load(Some(&mut cache))
                .is_some_and(|blob| works(blob.data(), font.index()))
        }) {
            usable.push(name);
        }
    }
    usable.sort_by_key(|name| name.to_lowercase());
    usable.dedup();
    tracing::debug!(
        families = usable.len(),
        elapsed_ms = started.elapsed().as_millis(),
        "probed system fonts"
    );
    usable
}

/// Does this face draw a monospaced grid at all?
pub(crate) fn works(data: &[u8], index: u32) -> bool {
    let Some(font) = swash::FontRef::from_index(data, index as usize) else {
        return false;
    };
    let charmap = font.charmap();
    // A text font maps the alphanumerics. Icon fonts — material symbols, an
    // octicon set — are monospaced squares and pass an advance-width test
    // happily, but they have no alphabet: that is the difference worth testing.
    let alphanumerics = ('a'..='z')
        .chain('A'..='Z')
        .chain('0'..='9')
        .all(|ch| charmap.map(ch) != 0);
    if !alphanumerics {
        return false;
    }
    // Equal advances are the whole point; zero means the glyph is missing even
    // though the charmap claimed it.
    let metrics = font.glyph_metrics(&[]);
    let width = metrics.advance_width(charmap.map('M'));
    if width <= 0.0 {
        return false;
    }
    // A square is also monospaced. Text faces advance about half an em; icon
    // fonts — the ones that map letters but are drawings — advance a whole em.
    let upem = f32::from(font.metrics(&[]).units_per_em).max(1.0);
    if !(0.35..=0.75).contains(&(width / upem)) {
        return false;
    }
    ['i', 'l', 'W', '0']
        .iter()
        .all(|ch| (metrics.advance_width(charmap.map(*ch)) - width).abs() < 0.01)
}

/// Family names that a user may want to see first, in this order.
///
/// Not a whitelist: everything that passes the probe is offered, this only
/// decides what the list opens on.
#[cfg(test)]
pub const PREFERRED: &[&str] = &[
    "JetBrains Mono",
    "Hack",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Fira Code",
    "Ubuntu Mono",
    "Noto Sans Mono",
    "Cascadia Mono",
    "Consolas",
];

/// Fallback families in the order they should be tried when the grid face lacks
/// a glyph.
///
/// Sarasa first, then Noto CJK: a terminal wants a *monospaced* CJK face, so the
/// wide characters line up with the Latin grid instead of each carrying its own
/// advance. Everything else on the machine comes after these, in the order
/// fontconfig lists it.
pub const FALLBACK: &[&str] = &[
    // Sarasa ships one family per region; any of them covers the Han block.
    "Sarasa Mono SC",
    "Sarasa Mono TC",
    "Sarasa Mono J",
    "Sarasa Mono K",
    "Sarasa Mono Slab SC",
    "Sarasa Mono Slab TC",
    "Sarasa Mono Slab J",
    "Sarasa Mono Slab K",
    "Sarasa Fixed SC",
    "Sarasa Fixed TC",
    "Sarasa Gothic SC",
    "Sarasa Gothic TC",
    "Sarasa Gothic J",
    "Sarasa Gothic K",
    "Sarasa Mono",
    "Sarasa Gothic",
    // Then the Noto CJK families, monospaced variants first.
    "Noto Sans Mono CJK SC",
    "Noto Sans Mono CJK TC",
    "Noto Sans Mono CJK JP",
    "Noto Sans Mono CJK KR",
    "Noto Sans Mono CJK",
    "Noto Serif CJK SC",
    "Noto Sans CJK SC",
    "Noto Sans CJK TC",
    "Noto Sans CJK JP",
    "Noto Sans CJK KR",
    "Noto Sans CJK",
    "Noto Sans Mono",
    "Noto Sans",
    // Then Windows' own CJK families, for the machine that has none of the
    // above. MS Gothic is the fixed-pitch one — the one a terminal wants, since
    // a proportional CJK face carries its own advance and the grid stops
    // lining up — and the rest are what is left on a stock install.
    "MS Gothic",
    "MS Mincho",
    "SimSun",
    "NSimSun",
    "MingLiU",
    "Batang",
    "Malgun Gothic",
    "Yu Gothic UI",
    "Meiryo",
    // Then the Nerd Font families. They are last because they are the answer to
    // a different question — three thousand icons that no text face has — and
    // first only among the private use area, where nothing else answers at all.
    // A patched family this list has not heard of is still found before a
    // hundred text faces: `is_nerd_font` sorts the walk by name.
    "Symbols Nerd Font Mono",
    "Symbols Nerd Font",
    "0xProto Nerd Font Mono",
    "JetBrainsMono Nerd Font Mono",
    "Hack Nerd Font Mono",
    "MesloLGL Nerd Font Mono",
    "MesloLGSDZ Nerd Font Mono",
    "FiraCode Nerd Font Mono",
    "CaskaydiaCove Nerd Font Mono",
    "VictorMono Nerd Font Mono",
    "NerdFontsSymbolsOnly",
    // Then the colour emoji. They have bitmaps and no outlines at all, which is
    // why they were left out until the colour atlas existed: before it, the walk
    // rejected them for drawing nothing, and correctly — an outline renderer
    // asked Noto Color Emoji for `😀` and got an empty image.
    "Noto Color Emoji",
    "Apple Color Emoji",
    "Segoe UI Emoji",
    "Twemoji Mozilla",
];

/// Does this family's name say it carries the Nerd Font patches?
///
/// The patcher does not rename to a scheme, it *appends* — every variant is
/// `<original> Nerd Font [Mono|Propo]` — so a substring is the portable test,
/// and it is what lets the walk put a family nobody listed ahead of the text
/// families that would never answer for `U+F015`.
pub fn is_nerd_font(name: &str) -> bool {
    name.to_lowercase().contains("nerd font")
}

/// Bytes of an installed family that can draw CJK, for egui's UI text.
///
/// egui's bundled faces are latin-only and it **panics** on a missing glyph
/// (`No glyph for "に" in Monospace`), and a terminal is exactly where CJK gets
/// typed: the IME preedit above all, but also any query a person types into the
/// palette. So the UI needs one CJK face, chosen by the same preference list the
/// grid uses — both then agree about which face a Han character comes from.
///
/// Read once, at startup, because `set_fonts` rebuilds egui's atlas.
pub fn ui_fallback() -> Option<(String, Vec<u8>)> {
    let mut collection = fontique::Collection::new(fontique::CollectionOptions::default());
    let mut cache = fontique::SourceCache::default();
    for name in FALLBACK {
        let Some(id) = collection.family_id(name) else {
            continue;
        };
        let Some(family) = collection.family(id) else {
            continue;
        };
        // Upright regular: a fallback for UI text, not a style match.
        let Some(font) = family.match_font(
            fontique::FontWidth::NORMAL,
            fontique::FontStyle::Normal,
            fontique::FontWeight::NORMAL,
            false,
        ) else {
            continue;
        };
        let Some(blob) = font.load(Some(&mut cache)) else {
            continue;
        };
        let index = font.index() as usize;
        let covers = swash::FontRef::from_index(blob.data(), index).is_some_and(|face| {
            let map = face.charmap();
            // Hiragana and a Han character: one family covers both, and testing
            // both is what tells a CJK face from one that only has kana.
            map.map('あ') != 0 && map.map('漢') != 0
        });
        if !covers {
            continue;
        }
        // Reload to own the bytes: `load(Some(&mut cache))` borrows from it.
        let owned = font.load(None)?;
        return Some(((*name).to_owned(), owned.data().to_vec()));
    }
    None
}

/// The egui font family name the caption glyphs are installed under.
pub const ICON_FAMILY: &str = "caption-icons";

/// Bytes of the platform's own window-caption glyphs, if it has a font for them.
///
/// Windows draws its caption buttons with a private-use icon font — Segoe Fluent
/// Icons on 11 (`SegoeIcons.ttf`), Segoe MDL2 Assets on 10 (`segmdl2.ttf`) — and
/// that font is the difference between "buttons that look like Windows" and
/// "buttons in the right place". Nothing is redistributed: it is a font already
/// installed on the machine, and the codepoints are stable and public
/// (`E921` minimise, `E922` maximise, `E923` restore, `E8BB` close).
///
/// The family is checked for the close glyph, not just for the name: a
/// look-alike family without the private-use range would draw tofu in the
/// caption, which is worse than the painted fallback.
///
/// Read once, at startup: `set_fonts` rebuilds egui's atlas.
pub fn caption_icons() -> Option<(String, Vec<u8>)> {
    let mut collection = fontique::Collection::new(fontique::CollectionOptions::default());
    let mut cache = fontique::SourceCache::default();
    for name in ["Segoe Fluent Icons", "Segoe MDL2 Assets"] {
        let Some(id) = collection.family_id(name) else {
            continue;
        };
        let Some(family) = collection.family(id) else {
            continue;
        };
        let Some(font) = family.match_font(
            fontique::FontWidth::NORMAL,
            fontique::FontStyle::Normal,
            fontique::FontWeight::NORMAL,
            false,
        ) else {
            continue;
        };
        let Some(blob) = font.load(Some(&mut cache)) else {
            continue;
        };
        let index = font.index() as usize;
        let covers = swash::FontRef::from_index(blob.data(), index)
            .is_some_and(|face| face.charmap().map('\u{E8BB}') != 0);
        if !covers {
            continue;
        }
        // Reload to own the bytes: `load(Some(&mut cache))` borrows from it.
        let owned = font.load(None)?;
        return Some((ICON_FAMILY.to_owned(), owned.data().to_vec()));
    }
    None
}

/// The family to use when nothing is configured: the first preferred one that
/// is installed, else the first usable family at all.
///
/// Test-only since the settings menu became a combo box: the menu lists every
/// usable family, so nothing has to choose one on the user's behalf.
#[cfg(test)]
pub fn default_family(usable: &[String]) -> Option<String> {
    PREFERRED
        .iter()
        .find(|wanted| usable.iter().any(|name| name.eq_ignore_ascii_case(wanted)))
        .map(|name| (*name).to_owned())
        .or_else(|| usable.first().cloned())
}

#[cfg(test)]
mod tests {
    use super::usable_families;

    /// The settings menu asks for this list when it opens, and the first call
    /// builds a fontconfig collection and probes every family. That cost is why
    /// the app warms it on a worker thread at startup; if the cache is ever
    /// removed, the first `Ctrl+,` becomes a stall again - and this test says so.
    #[test]
    fn the_family_list_is_computed_once() {
        let first = std::time::Instant::now();
        let families = usable_families();
        let cold = first.elapsed();
        let second = std::time::Instant::now();
        let again = usable_families();
        let warm = second.elapsed();
        assert_eq!(families, again, "the same list, not a re-probe");
        assert!(
            warm < std::time::Duration::from_millis(5),
            "second call took {warm:?}; the cache is gone (cold was {cold:?})"
        );
    }

    use super::*;

    #[test]
    fn the_probe_rejects_a_proportional_face() {
        // DejaVu Sans (proportional) is on every machine that has DejaVu at
        // all; if it passes, the probe is not measuring anything.
        let mut collection = fontique::Collection::new(fontique::CollectionOptions::default());
        let mut cache = fontique::SourceCache::default();
        let Some(family) = collection.family_by_name("DejaVu Sans") else {
            return; // not installed here; nothing to prove
        };
        let usable = family
            .fonts()
            .iter()
            .filter_map(|font| font.load(Some(&mut cache)))
            .any(|blob| works(blob.data(), 0));
        assert!(!usable, "a proportional family passed the monospace probe");
    }

    #[test]
    fn dejavu_mono_passes_when_installed() {
        let mut collection = fontique::Collection::new(fontique::CollectionOptions::default());
        let mut cache = fontique::SourceCache::default();
        let Some(family) = collection.family_by_name("DejaVu Sans Mono") else {
            return;
        };
        let usable = family
            .fonts()
            .iter()
            .filter_map(|font| font.load(Some(&mut cache)))
            .any(|blob| works(blob.data(), 0));
        assert!(usable, "the grid font of last resort failed its own probe");
    }

    /// The UI needs a face egui can draw CJK with, or the preedit box panics on
    /// the characters an IME exists to produce. Skipped when the machine has no
    /// CJK family at all, like the font probes above.
    #[test]
    fn a_ui_fallback_covers_cjk_when_one_is_installed() {
        let Some((name, bytes)) = super::ui_fallback() else {
            eprintln!("no CJK family installed; nothing to check");
            return;
        };
        assert!(!bytes.is_empty(), "{name} produced no bytes");
        let face = swash::FontRef::from_index(&bytes, 0).expect("a face");
        let map = face.charmap();
        assert!(map.map('あ') != 0, "{name} has no kana");
        assert!(map.map('漢') != 0, "{name} has no Han");
    }

    #[test]
    fn the_list_is_sorted_and_non_empty() {
        let families = usable_families();
        if families.is_empty() {
            // A machine with no fonts is not this test's business.
            return;
        }
        let mut sorted = families.clone();
        sorted.sort_by_key(|name| name.to_lowercase());
        assert_eq!(families, sorted);
        assert!(default_family(&families).is_some());
    }
}
