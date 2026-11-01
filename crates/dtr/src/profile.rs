//! In-process CPU profiling.
//!
//! `perf` is the right tool when the kernel lets you use it, and it does not
//! exist on Windows at all. This is the fallback that always works: sample our
//! own threads with a signal-driven profiler and report on the way out. It is
//! not free — a signal and a backtrace walk per sample — so it only starts when
//! `--cpu-profile` asks for it.
//!
//! The text report comes first, because it answers the question: a flamegraph
//! needs a browser and an eye, while a screen of self time says where the run
//! went. Pass a path ending in `.svg` to also get the graph.

use std::path::Path;

pub struct Guard {
    #[cfg(target_os = "linux")]
    inner: Option<pprof::ProfilerGuard<'static>>,
    /// How long we sampled, so the report can say how busy the process was
    /// rather than only where the samples landed.
    started: std::time::Instant,
    /// Written on drop as well as explicitly: an empty profile file is worse
    /// than no file, because it looks like success.
    path: std::path::PathBuf,
    written: std::cell::Cell<bool>,
    #[cfg(not(target_os = "linux"))]
    _never: std::marker::PhantomData<()>,
}

impl Guard {
    /// Start sampling into `path`, or return `None` where profiling is not
    /// available.
    pub fn start(path: &Path) -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            // 997 Hz: prime, so samples do not line up with frame or poll
            // timings and invent a hotspot that is really the refresh rate.
            match pprof::ProfilerGuardBuilder::default()
                .frequency(997)
                .blocklist(&["libc", "libgcc", "pthread", "vdso"])
                .build()
            {
                Ok(guard) => {
                    tracing::info!(path = %path.display(), "cpu profiler started");
                    Some(Self {
                        inner: Some(guard),
                        started: std::time::Instant::now(),
                        path: path.to_path_buf(),
                        written: std::cell::Cell::new(false),
                    })
                }
                Err(err) => {
                    tracing::warn!(%err, "could not start the CPU profiler");
                    None
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            tracing::warn!("CPU profiling is only wired up on Linux");
            None
        }
    }

    /// Report, and write a flamegraph when the path ends in `.svg`.
    ///
    /// Errors are logged, never fatal: failing to write a profile must not fail
    /// the run that was being profiled.
    pub fn finish(self, path: &Path) {
        let _ = path;
        self.write();
    }

    #[cfg(target_os = "linux")]
    fn write(&self) {
        if self.written.replace(true) {
            return;
        }
        let Some(guard) = self.inner.as_ref() else {
            return;
        };
        let started = self.started;
        match guard.report().build() {
            Ok(report) => {
                if report.data.is_empty() {
                    tracing::warn!(
                        "no samples were collected; nothing written (was the run shorter than a sampling tick?)"
                    );
                    return;
                }
                report_text(&report, started.elapsed());
                write_flamegraph(&report, &self.path);
            }
            Err(err) => tracing::warn!(%err, "build profile report"),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn write(&self) {}
}

#[cfg(target_os = "linux")]
impl Drop for Guard {
    fn drop(&mut self) {
        // Every orderly exit, including an unwinding panic, writes the profile.
        self.write();
    }
}

/// Self time per function, hottest first, on stderr.
#[cfg(target_os = "linux")]
fn report_text(report: &pprof::Report, elapsed: std::time::Duration) {
    use std::collections::HashMap;

    // A stack's own cost is its leaf frame; everything above it is the caller
    // waiting. `frames[0]` is the leaf — pprof reverses the order for display.
    let mut self_time: HashMap<String, isize> = HashMap::new();
    let mut total = 0isize;
    for (stack, count) in &report.data {
        total += count;
        let name = stack
            .frames
            .iter()
            .filter_map(|frame| frame.first())
            .filter_map(|symbol| symbol.name.as_ref())
            .map(|raw| {
                let text = String::from_utf8_lossy(raw);
                (short_name(&text), text.into_owned())
            })
            // The sampler walks a stack inside its own signal handler, so the
            // leaf of every stack is the unwinder. Skipping those — by their
            // mangled name as well as the demangled one, since a closure inside
            // pprof demangles to something unhelpful — is the difference
            // between a report and a joke.
            .find(|(short, raw)| !is_profiler_frame(short) && !is_profiler_frame(raw))
            .map(|(short, _)| short)
            .unwrap_or_else(|| "<unknown>".to_owned());
        *self_time.entry(name).or_default() += count;
    }

    let mut rows: Vec<(String, isize)> = self_time.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    let total = total.max(1);
    // 997 Hz across all threads: samples / (rate × seconds) is the fraction of
    // one core the whole process used. A report of mostly startup work in a
    // 3-second run means the program was idle, not that it is fast.
    let busy = 100.0 * total as f64 / (997.0 * elapsed.as_secs_f64().max(0.001));
    tracing::info!(
        "self time, top {} of {total} samples over {:.1}s (≈{busy:.0}% of one core)",
        rows.len().min(25),
        elapsed.as_secs_f64()
    );
    for (name, count) in rows.iter().take(25) {
        tracing::info!(
            "{:>5.1}%  {:>6}  {name}",
            100.0 * *count as f64 / total as f64,
            count
        );
    }
}

/// Frames that only exist because a profiler is running.
#[cfg(target_os = "linux")]
fn is_profiler_frame(name: &str) -> bool {
    const NOISE: &[&str] = &[
        "backtrace::",
        "pprof::",
        "_Unwind_",
        "__restore_rt",
        "__libc_sigaction",
        "std::sys::backtrace",
        "core::fmt",
        "libunwind",
        "unwind",
    ];
    if NOISE.iter().any(|prefix| name.starts_with(prefix)) {
        return true;
    }
    // Mangled v0 symbols hide the module path in a way the demangler may not
    // resolve for closures; a substring test catches those.
    name.contains("pprof") || name.contains("backtrace") || name.contains("perf_signal")
}

/// Demangle, then drop generic arguments and the module path's noise.
#[cfg(target_os = "linux")]
fn short_name(raw: &str) -> String {
    let demangled = rustc_demangle::try_demangle(raw)
        .map(|name| format!("{name:#}"))
        .unwrap_or_else(|_| raw.to_owned());
    // `<dtr::ui::Draw as core::ops::X>::draw` and `dtr::ui::draw::<T>` are the
    // same function as far as this report is concerned.
    let demangled = match demangled.find('<') {
        Some(index) => demangled[..index].trim().to_owned(),
        None => demangled,
    };
    if demangled.is_empty() {
        raw.to_owned()
    } else {
        demangled
    }
}

#[cfg(target_os = "linux")]
fn write_flamegraph(report: &pprof::Report, path: &Path) {
    match std::fs::File::create(path) {
        Ok(file) => match report.flamegraph(file) {
            Ok(()) => tracing::info!(path = %path.display(), "wrote flamegraph"),
            Err(err) => tracing::warn!(%err, "write flamegraph"),
        },
        Err(err) => tracing::warn!(%err, path = %path.display(), "create flamegraph"),
    }
}
