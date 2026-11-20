//! Formatting a number the way more than one surface writes it.
//!
//! A byte count is the one figure a queue row, a status bar and a toast all put
//! in front of a person, and they used to spell it twice: `dtr_files`'s
//! `human_size` and the queue card's private `bytes` were copies that agreed only
//! by tests, with a comment in each asking for the other to go. This is that one
//! function, at the bottom of the graph so both crates can ask it.

/// A byte count a person can read: `512 B`, `1.5 kB`, `2.0 MB`, `3.0 GB`.
///
/// Binary multiples with the usual one-letter suffix (`kB`, not `KB`): the
/// file manager shows a directory's size next to its files' sizes, and a queue
/// row shows the same number for the same bytes.
pub fn human_size(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let value = bytes as f64;
    if value < K {
        format!("{bytes} B")
    } else if value < K * K {
        format!("{:.1} kB", value / K)
    } else if value < K * K * K {
        format!("{:.1} MB", value / (K * K))
    } else {
        format!("{:.1} GB", value / (K * K * K))
    }
}

#[cfg(test)]
mod tests {
    use super::human_size;

    #[test]
    fn a_size_is_written_the_way_every_surface_writes_it() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
        assert_eq!(human_size(1536), "1.5 kB");
        assert_eq!(human_size(2 * 1024 * 1024), "2.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
        // The boundary is the binary multiple, not the decimal one.
        assert_eq!(human_size(1024), "1.0 kB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
    }
}
