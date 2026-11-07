//! The platform's own chrome, and keeping it current.
//!
//! `native` means "look like a window on this machine", and what that means is
//! platform code: Windows keeps a scheme and an accent in the registry, and a
//! GNOME session states the same two facts through the XDG settings portal.
//! [`Platform`] is the shape both are written in — read the facts, turn them
//! into a skin — so a reader is a reader rather than a dialect, and `-v` has one
//! way to say which one answered and why one could not.
//!
//! The reading is here rather than in `dtr-widgets` because it is I/O and this is
//! where the platform code lives; the crate only knows how to build a palette
//! from the facts.
//!
//! It is *live*: a desktop changes its scheme at sunset and its accent when the
//! user picks one, and a terminal that keeps the chrome it started with is a
//! window that stopped matching everything beside it. So the answer sits in one
//! cell that a frame reads, and a watcher rewrites that cell when the platform
//! says it changed — `WM_SETTINGCHANGE` on Windows, `SettingChanged` on the
//! portal — then asks for a repaint.

mod windows;

#[cfg(target_os = "linux")]
mod linux;

use std::sync::{OnceLock, RwLock};

use dtr_widgets::{AccentLadder, Origin, Skin};
use egui::Color32;

/// What a platform says about its own chrome, in the shape every platform states
/// it.
#[derive(Clone, Debug)]
pub struct Reading {
    /// Dark scheme, as the desktop means it: the surfaces, not the terminal
    /// theme, which the user may have made something else entirely.
    pub dark: bool,
    /// The user's accent. `None` where the platform has no such preference and
    /// its own palette has a default worth keeping.
    pub accent: Option<Color32>,
    /// The shades the OS's own controls hover and press through, where it keeps
    /// them. Windows does; Adwaita computes them from the accent, so this is
    /// written but never read on Linux.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub ladder: Option<AccentLadder>,
}

/// One platform's answer to "what does a window look like here".
///
/// The trait is the shape, not the sharing: Windows reads two registry values
/// and Linux asks a portal, and the two have nothing in common but the questions
/// asked. What they do share is [`Resolution`] — the one way `-v` says which
/// reader answered — so a platform is a reader plus a palette and the resolution
/// is written once.
pub trait Platform {
    /// Where the answer came from, for `-v`: `windows-registry`, `portal`.
    fn source(&self) -> &'static str;

    /// The facts, or why this machine cannot supply them. The reason reaches
    /// `-v`, so it names what was wrong rather than "failed".
    async fn read(&self) -> Result<Reading, String>;

    /// The skin those facts make. Per platform, because the constants are.
    fn skin(&self, reading: &Reading) -> Skin;
}

/// What a platform answered, including the answer "nothing" — and why.
#[derive(Clone, Debug)]
pub struct Resolution {
    pub skin: Option<Skin>,
    /// Which reader answered, or `none` where the platform has no reader.
    pub source: &'static str,
    /// Why there is no skin; `None` exactly when `skin` is `Some`.
    pub reason: Option<String>,
}

async fn resolve(platform: &impl Platform) -> Resolution {
    match platform.read().await {
        Ok(reading) => Resolution {
            skin: Some(platform.skin(&reading)),
            source: platform.source(),
            reason: None,
        },
        Err(reason) => Resolution {
            skin: None,
            source: platform.source(),
            reason: Some(reason),
        },
    }
}

/// The answer, in the one place a frame can read it.
///
/// A cell rather than something threaded through `Look` because `Look::skin` is
/// asked by every surface on every frame and has no access to the app, and the
/// watcher writes it from a thread that has no access to the app either. The
/// lock is only held for the clone a frame needs.
static STATE: OnceLock<RwLock<Resolution>> = OnceLock::new();

fn state() -> &'static RwLock<Resolution> {
    STATE.get_or_init(|| RwLock::new(first_reading()))
}

/// The skin `native` should draw, or `None` where the machine has no answer.
///
/// The first call does the read: its caller is synchronous — `-v` about to print
/// the answer, or a frame about to draw it — so there is nothing to defer to, and
/// every later call is a read of the cell.
pub fn native_skin() -> Option<Skin> {
    let state = state()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    state.skin.clone()
}

/// The same answer with its provenance, which `-v` logs.
pub fn resolution() -> Resolution {
    state()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn first_reading() -> Resolution {
    if let Some(preview) = preview() {
        return preview;
    }
    if let Some(offline) = offline() {
        return offline;
    }
    // The read is async because the watcher re-runs it on a runtime, and the first
    // one has no runtime to re-use: a current-thread one is the smallest way to
    // ask the same question from a caller that cannot wait for an answer it is
    // about to draw.
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return Resolution {
            skin: None,
            source: "none",
            reason: Some("no runtime for the native read".to_owned()),
        };
    };
    runtime.block_on(read_platform())
}

/// A preview switch, and the only way to look at another platform's chrome from a
/// machine that has none: `DTR_NATIVE_SKIN=windows-dark` answers where the
/// platform would, and it wins on that platform too, because a preview that the
/// platform could overrule is not a preview.
#[cfg(not(test))]
fn preview() -> Option<Resolution> {
    preview_skin(std::env::var("DTR_NATIVE_SKIN").ok().as_deref()).map(|skin| Resolution {
        skin: Some(skin),
        source: "preview",
        reason: None,
    })
}

/// A test reads nothing, and neither does the preview inside one.
///
/// Snapshots are compared byte-exactly in CI, so a baseline that changed with the
/// developer's accent or with whether their session bus was up is a baseline
/// nobody can compare — and the harness builds the default `Look`, whose preset
/// is `native`. The preview switch is inert here for the same reason: it is
/// process-global state, so it cannot be a per-test input either.
#[cfg(test)]
fn preview() -> Option<Resolution> {
    None
}

#[cfg(test)]
fn offline() -> Option<Resolution> {
    Some(Resolution {
        skin: None,
        source: "none",
        reason: Some("a test does not read the desktop".to_owned()),
    })
}

#[cfg(not(test))]
fn offline() -> Option<Resolution> {
    None
}

#[cfg(windows)]
async fn read_platform() -> Resolution {
    resolve(&windows::Registry).await
}

#[cfg(target_os = "linux")]
async fn read_platform() -> Resolution {
    resolve(&linux::Portal).await
}

#[cfg(not(any(windows, target_os = "linux")))]
async fn read_platform() -> Resolution {
    Resolution {
        skin: None,
        source: "none",
        reason: Some("this platform has no native palette".to_owned()),
    }
}

/// Re-read the desktop and put the answer where a frame will find it.
pub async fn refresh() {
    let reading = match preview() {
        // A preview is not overruled by the desktop either: the watcher keeps
        // running, and the look a developer asked for is the look they keep.
        Some(preview) => preview,
        None => read_platform().await,
    };
    report(&reading);
    *state()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = reading;
}

/// Keep the native skin current, on the worker runtime.
///
/// `wake` is how the repaint is asked for: a skin that changed under a frame that
/// never draws again is a skin that did not change. It is a closure rather than a
/// `UserEvent` because this module has no business knowing about the event loop.
pub fn watch(worker: &tokio::runtime::Handle, wake: impl Fn() + Send + 'static) {
    worker.spawn(async move {
        loop {
            wait_for_change().await;
            refresh().await;
            wake();
        }
    });
}

/// The platform's "your colours changed" signal, or a future that never resolves
/// on a platform that has no such signal.
#[cfg(target_os = "linux")]
async fn wait_for_change() {
    linux::Portal.changed().await;
}

#[cfg(windows)]
async fn wait_for_change() {
    CHANGED.notified().await;
}

#[cfg(not(any(windows, target_os = "linux")))]
async fn wait_for_change() {
    std::future::pending().await
}

/// Notified by the window procedure, which is why this exists as a separate door:
/// the procedure cannot await, and must not block.
#[cfg(windows)]
static CHANGED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// The desktop says its colours changed.
///
/// Called from the window procedure, so this only wakes the watcher — storing a
/// permit when none is waiting, which is what keeps a change that arrives between
/// two reads from being lost.
#[cfg(windows)]
pub fn changed() {
    CHANGED.notify_one();
}

fn report(reading: &Resolution) {
    match &reading.skin {
        Some(skin) => tracing::debug!(
            skin = describe(skin),
            accent = accent_hex(skin),
            hover = hex(skin.palette.accent_hover),
            source = reading.source,
            "chrome changed"
        ),
        None => tracing::debug!(
            source = reading.source,
            reason = reading.reason.as_deref().unwrap_or(""),
            "chrome changed"
        ),
    }
}

/// `DTR_NATIVE_SKIN`'s values, kept pure so a test can check them without an
/// environment or a cache. An unknown value is ignored rather than fatal — a typo
/// in a debugging switch must not stop a terminal from starting.
fn preview_skin(value: Option<&str>) -> Option<Skin> {
    match value {
        Some("windows-dark") => Some(Skin::windows_dark()),
        Some("windows-light") => Some(Skin::windows_light()),
        Some("adwaita-dark") => Some(Skin::adwaita(true, dtr_widgets::ADWAITA_ACCENT)),
        Some("adwaita-light") => Some(Skin::adwaita(false, dtr_widgets::ADWAITA_ACCENT)),
        _ => None,
    }
}

/// Which skin a run settled on, for `-v`: "the user asked for native and got it"
/// and "the user asked for native and got dtr" are different bugs.
///
/// The origin is the skin's own, not a guess from its style. The guess this
/// replaces read `style.mono` and `is_dark()`, which calls an Adwaita skin "dtr"
/// — it borrows that layout — and would call any non-monospace light skin
/// "windows", and a log line naming the wrong platform is worse than no line.
pub fn describe(skin: &Skin) -> &'static str {
    match (skin.origin, skin.palette.is_dark()) {
        (Origin::Dtr, _) => "dtr",
        (Origin::Windows, true) => "windows-dark",
        (Origin::Windows, false) => "windows-light",
        (Origin::Adwaita, true) => "adwaita-dark",
        (Origin::Adwaita, false) => "adwaita-light",
    }
}

/// The skin's accent as `#RRGGBB`, for the startup log: "native, light, and the
/// user's accent" is the whole claim, and two of those three are invisible
/// without saying what colour came out.
pub fn accent_hex(skin: &Skin) -> String {
    hex(skin.palette.accent)
}

/// A colour as `#RRGGBB`, for a log line.
pub fn hex(colour: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", colour.r(), colour.g(), colour.b())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that answers what the test says, so the shared half of the two
    /// platforms — the resolution, and the reason it carries — can be checked
    /// without a registry or a session bus.
    struct Stub(Result<Reading, &'static str>);

    impl Platform for Stub {
        fn source(&self) -> &'static str {
            "stub"
        }

        async fn read(&self) -> Result<Reading, String> {
            self.0.clone().map_err(str::to_owned)
        }

        fn skin(&self, reading: &Reading) -> Skin {
            Skin::adwaita(reading.dark, dtr_widgets::ADWAITA_ACCENT)
        }
    }

    fn answer<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("a runtime")
            .block_on(future)
    }

    #[test]
    fn a_reader_that_answers_resolves_to_its_skin() {
        let reading = Reading {
            dark: true,
            accent: None,
            ladder: None,
        };
        let resolution = answer(resolve(&Stub(Ok(reading))));
        assert_eq!(
            describe(resolution.skin.as_ref().expect("a skin")),
            "adwaita-dark"
        );
        assert_eq!(resolution.source, "stub");
        assert!(resolution.reason.is_none());
    }

    #[test]
    fn a_reader_that_cannot_answer_carries_its_reason() {
        let resolution = answer(resolve(&Stub(Err("the portal is not there"))));
        assert!(resolution.skin.is_none());
        assert_eq!(resolution.source, "stub");
        assert_eq!(
            resolution.reason.as_deref(),
            Some("the portal is not there")
        );
    }

    /// The preview switch names a skin, and only a skin: no typo, no panic, and
    /// no way to ask for a `dtr` skin through the platform's door.
    #[test]
    fn the_preview_switch_knows_four_skins() {
        for (value, expected) in [
            ("windows-dark", "windows-dark"),
            ("windows-light", "windows-light"),
            ("adwaita-dark", "adwaita-dark"),
            ("adwaita-light", "adwaita-light"),
        ] {
            assert_eq!(
                preview_skin(Some(value)).map(|skin| describe(&skin)),
                Some(expected)
            );
        }
        assert!(preview_skin(Some("dtr")).is_none());
        assert!(preview_skin(Some("fancy")).is_none());
        assert!(preview_skin(None).is_none());
    }

    #[test]
    fn a_skin_describes_itself() {
        assert_eq!(describe(&Skin::windows_dark()), "windows-dark");
        assert_eq!(describe(&Skin::windows_light()), "windows-light");
        assert_eq!(
            describe(&Skin::adwaita(true, dtr_widgets::ADWAITA_ACCENT)),
            "adwaita-dark"
        );
        assert_eq!(
            describe(&Skin::adwaita(false, dtr_widgets::ADWAITA_ACCENT)),
            "adwaita-light"
        );
        // The dtr skin is dtr whatever its palette happens to be, including a
        // light one, which the old guess would have called "windows-light".
        assert_eq!(
            describe(&Skin::dtr(dtr_widgets::Palette::dtr(
                [0xfa, 0xfa, 0xfb],
                [0x20, 0x20, 0x20],
                [0x35, 0x84, 0xe4]
            ))),
            "dtr"
        );
    }
}
