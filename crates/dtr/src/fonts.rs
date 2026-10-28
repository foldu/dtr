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
        if family
            .fonts()
            .iter()
            .any(|font| font.load(Some(&mut cache)).is_some_and(|blob| works(blob.data(), font.index())))
        {
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
];

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
