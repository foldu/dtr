//! The adapters wgpu can see, for the preferences card.
//!
//! A cached list rather than a probe per frame: `enumerate_adapters` builds a
//! wgpu instance, and the card asks on every opening. The same names `--gpu`
//! matches by are the ones it offers, so what the list shows is what the flag
//! would pick.

use std::sync::OnceLock;

/// Every adapter wgpu reports, by name, sorted and deduplicated.
///
/// Empty when there is no backend to ask: the card then offers "auto" and the
/// name already in the file, which is still the honest set of choices.
pub fn adapters() -> &'static [String] {
    static ADAPTERS: OnceLock<Vec<String>> = OnceLock::new();
    ADAPTERS.get_or_init(|| {
        let instance = dtr_frame::instance();
        let mut names: Vec<String> =
            pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()))
                .into_iter()
                .map(|adapter| adapter.get_info().name)
                .collect();
        names.sort();
        names.dedup();
        tracing::debug!(?names, "adapters seen");
        names
    })
}
