//! What a GNOME session says its chrome looks like.
//!
//! Two facts decide the native skin: the scheme, and the accent. libadwaita
//! itself reads both from the XDG settings portal (`org.freedesktop.appearance`)
//! and falls back to `gsettings`, and so does this — in that order, because the
//! portal is what the session has already agreed on and it survives a settings
//! daemon that is slow or missing. The theme *name* is the gate: see
//! [`adwaita_like`].
//!
//! Nothing here parses CSS. The surfaces are libadwaita 1.9.3's own constants,
//! measured out of its gresource and carrying a light and a dark half, so the
//! only values a session can change are the two above — see `docs/gtk-palette.md`
//! §8 for what a second version would have to read to change the rest.
//!
//! The portal half is behind the `portal` feature: without it this reads
//! `gsettings` only, and the skin does not follow the desktop. That is the build
//! for a target that will not take zbus's tree (`docs/gtk-palette.md` §10).

// The portal helpers are the dead half of a `--no-default-features` build.
#![cfg_attr(not(feature = "portal"), allow(dead_code))]

use std::time::Duration;

use dtr_widgets::{ADWAITA_ACCENT, Skin};
use egui::Color32;

use super::{Platform, Reading};

/// The ceiling on one reading.
///
/// The portal answers in 2–2.6 ms measured, so this is a guard against a stuck
/// D-Bus daemon or a `gsettings` waiting on a lock, not a delay: the first frame
/// must not wait for the desktop to wake up.
const BUDGET: Duration = Duration::from_millis(500);

/// How long to wait before trying to watch again after the bus or the portal
/// refused — a session that has neither would otherwise spin on connect.
const RETRY: Duration = Duration::from_secs(5);

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_IFACE: &str = "org.freedesktop.portal.Settings";
/// The namespace the appearance keys live in; `Read` takes it as its first
/// argument rather than as part of the interface.
const APPEARANCE: &str = "org.freedesktop.appearance";
const DESKTOP_INTERFACE: &str = "org.gnome.desktop.interface";

pub(super) struct Portal;

impl Platform for Portal {
    fn source(&self) -> &'static str {
        // Whichever reader will answer: with the feature off there is no portal
        // call to name.
        if cfg!(feature = "portal") {
            "portal"
        } else {
            "gsettings"
        }
    }

    async fn read(&self) -> Result<Reading, String> {
        tokio::time::timeout(BUDGET, gather())
            .await
            .unwrap_or_else(|_| {
                Err(format!(
                    "the desktop did not answer within {} ms",
                    BUDGET.as_millis()
                ))
            })
    }

    fn skin(&self, reading: &Reading) -> Skin {
        Skin::adwaita(reading.dark, reading.accent.unwrap_or(ADWAITA_ACCENT))
    }
}

impl Portal {
    /// Wait for the desktop to say one of the appearance keys changed.
    ///
    /// The portal re-emits these from `gsettings`, so this covers a GNOME session
    /// whatever set the value: the settings app, `gsettings` in a terminal, or a
    /// theme switcher. A session with no portal never gets here — `read` already
    /// fell back to `gsettings`, and there is no notification to subscribe to for
    /// that, so the skin stays at what it was.
    #[cfg(feature = "portal")]
    pub(super) async fn changed(&self) {
        loop {
            let Ok(connection) = zbus::Connection::session().await else {
                tokio::time::sleep(RETRY).await;
                continue;
            };
            let Ok(proxy) = zbus::Proxy::new(&connection, PORTAL, PORTAL_PATH, PORTAL_IFACE).await
            else {
                tokio::time::sleep(RETRY).await;
                continue;
            };
            let Ok(mut stream) = proxy.receive_signal("SettingChanged").await else {
                tokio::time::sleep(RETRY).await;
                continue;
            };
            // Said out loud because it is the half of this that cannot be seen from
            // the outside: "the portal answered once" and "the portal will tell me
            // when it changes" are different claims, and only one of them leaves a
            // line in `-v`.
            tracing::debug!(source = "portal", "watching the desktop for chrome changes");
            // One change is enough: the caller re-reads and comes back here.
            while let Some(message) = next_signal(&mut stream).await {
                let Ok((namespace, _, _)) =
                    message
                        .body()
                        .deserialize::<(String, String, zbus::zvariant::OwnedValue)>()
                else {
                    continue;
                };
                if namespace == APPEARANCE {
                    return;
                }
            }
            // The connection dropped without a change; start again.
            tokio::time::sleep(RETRY).await;
        }
    }

    /// Without the portal there is no change to wait for: `gsettings` has no
    /// notification a plain reader can subscribe to, so the skin is whatever it
    /// was at startup. A future that never resolves is the honest spelling of
    /// that — the watcher stays parked instead of polling a subprocess.
    #[cfg(not(feature = "portal"))]
    pub(super) async fn changed(&self) {
        std::future::pending::<()>().await;
    }
}

/// One `.next()` is not worth a crate: `futures_util::StreamExt` would be the only
/// reason to depend on it, and `zbus` re-exports the trait.
#[cfg(feature = "portal")]
async fn next_signal(stream: &mut zbus::proxy::SignalStream<'_>) -> Option<zbus::Message> {
    use zbus::export::futures_core::Stream;
    std::future::poll_fn(|cx| std::pin::Pin::new(&mut *stream).poll_next(cx)).await
}

async fn gather() -> Result<Reading, String> {
    let theme = theme_name().await;
    if !adwaita_like(&theme) {
        return Err(format!("theme {theme:?} is not Adwaita"));
    }
    // The two portal values over one connection: the bus is the expensive
    // part, and asking twice would mean two of them.
    let (scheme, accent) = portal_values().await.unwrap_or((None, None));
    let dark = match scheme {
        // 0 is "no preference", which is not "light": it means the scheme is
        // the theme's, and guessing here is how a white chrome lands on a
        // dark desktop.
        Some(1) => true,
        Some(2) => false,
        _ => gsettings_scheme()
            .await
            .or_else(|| theme_scheme(&theme))
            .ok_or_else(|| format!("theme {theme:?} does not say whether it is light or dark"))?,
    };
    let accent = match accent {
        Some(accent) => accent,
        None => gsettings("accent-color")
            .await
            .as_deref()
            .and_then(accent_named)
            .unwrap_or(ADWAITA_ACCENT),
    };
    Ok(Reading {
        dark,
        accent: Some(accent),
        ladder: None,
    })
}

/// A theme that is not Adwaita has no palette we can read in this version, and
/// painting Adwaita's `#36363a` surfaces on Breeze is a confident lie about the
/// desktop. dtr's own chrome is at least coherent, and the reason reaches `-v`.
fn adwaita_like(theme: &str) -> bool {
    // `adw-gtk3` is the readable port of Adwaita for GTK3 apps, and the name a
    // user's `gtk-theme-name` actually has when they picked Adwaita under X11.
    matches!(
        base_name(theme).to_ascii_lowercase().as_str(),
        "adwaita" | "adw-gtk3"
    )
}

/// The theme's base name, without the `:variant` override or a `-dark`/`-light`
/// suffix: `Adwaita:dark`, `Adwaita-dark` and `Adwaita` are all "Adwaita".
fn base_name(theme: &str) -> &str {
    let name = theme.split(':').next().unwrap_or_default();
    name.strip_suffix("-dark")
        .or_else(|| name.strip_suffix("-light"))
        .unwrap_or(name)
}

/// The scheme a theme name states, when it states one.
fn theme_scheme(theme: &str) -> Option<bool> {
    // `GTK_THEME=Adwaita:dark` is an override and wins over the name.
    if let Some((_, variant)) = theme.split_once(':') {
        return match variant.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(true),
            "light" => Some(false),
            _ => None,
        };
    }
    let name = theme.to_ascii_lowercase();
    if name.ends_with("-dark") {
        Some(true)
    } else if name.ends_with("-light") {
        Some(false)
    } else {
        // A name with neither suffix is the ambiguous case the caller refuses:
        // picking one would be wrong half the time.
        None
    }
}

/// The theme's name, from the three places GTK would look: the debug override,
/// the settings daemon, and the config file. `Adwaita` is GTK's own default when
/// none of them says anything.
async fn theme_name() -> String {
    if let Ok(value) = std::env::var("GTK_THEME") {
        if !value.trim().is_empty() {
            return value;
        }
    }
    match gsettings("gtk-theme").await {
        Some(name) => name,
        None => settings_ini_theme().unwrap_or_else(|| "Adwaita".to_owned()),
    }
}

/// `gtk-theme-name` from `$XDG_CONFIG_HOME/gtk-4.0/settings.ini`.
fn settings_ini_theme() -> Option<String> {
    let path = dirs::config_dir()?.join("gtk-4.0").join("settings.ini");
    theme_in_ini(&std::fs::read_to_string(path).ok()?)
}

fn theme_in_ini(text: &str) -> Option<String> {
    let mut in_settings = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(heading) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            in_settings = heading.trim() == "Settings";
            continue;
        }
        if !in_settings {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "gtk-theme-name" {
            let value = value.trim().trim_matches('"');
            if !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}

/// A value from the settings daemon, unquoted.
///
/// `gsettings` prints a GVariant, so a string arrives as `'prefer-dark'` and a
/// number as `1`; the quotes are trimmed rather than parsed, and a missing schema
/// exits non-zero rather than printing a sentence worth interpreting.
///
/// On a blocking thread, because spawning a process is blocking and the caller is
/// a runtime: `tokio::process` would be one Cargo feature for one call.
async fn gsettings(key: &'static str) -> Option<String> {
    tokio::task::spawn_blocking(move || {
        let output = std::process::Command::new("gsettings")
            .args(["get", DESKTOP_INTERFACE, key])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let text = text.trim().trim_matches('\'');
        (!text.is_empty()).then(|| text.to_owned())
    })
    .await
    .ok()
    .flatten()
}

async fn gsettings_scheme() -> Option<bool> {
    match gsettings("color-scheme").await?.as_str() {
        "prefer-dark" => Some(true),
        "prefer-light" => Some(false),
        _ => None,
    }
}

/// The portal's scheme (as `u`) and accent (as `(ddd)`), over one connection.
///
/// A missing bus, a missing portal and a portal that answers an error are all
/// `None`: each of them means "ask `gsettings`", and none of them is worth
/// stopping a terminal for.
#[cfg(feature = "portal")]
async fn portal_values() -> Option<(Option<u32>, Option<Color32>)> {
    let connection = zbus::connection::Builder::session()
        .ok()?
        .method_timeout(BUDGET)
        .build()
        .await
        .ok()?;
    let proxy = zbus::Proxy::new(&connection, PORTAL, PORTAL_PATH, PORTAL_IFACE)
        .await
        .ok()?;
    let scheme = read(&proxy, "color-scheme")
        .await
        .and_then(|value| u32::try_from(unwrap_variant(value)).ok());
    let accent = read(&proxy, "accent-color").await.and_then(accent_of);
    Some((scheme, accent))
}

/// Without the portal there is nothing to ask: both values come from `gsettings`,
/// which is what `gather` falls back to when this is `None`.
#[cfg(not(feature = "portal"))]
async fn portal_values() -> Option<(Option<u32>, Option<Color32>)> {
    None
}

#[cfg(feature = "portal")]
async fn read(proxy: &zbus::Proxy<'_>, key: &str) -> Option<zbus::zvariant::OwnedValue> {
    proxy.call("Read", &(APPEARANCE, key)).await.ok()
}

/// `Read` returns a `v`, so what arrives is one `Value` deeper than the answer:
/// `OwnedValue(Value(U32(1)))` is a `u32`, not a variant to be inspected.
#[cfg(feature = "portal")]
fn unwrap_variant(value: zbus::zvariant::OwnedValue) -> zbus::zvariant::Value<'static> {
    use zbus::zvariant::Value;
    match Value::from(value) {
        Value::Value(inner) => *inner,
        other => other,
    }
}

/// `accent-color` is a structure of three components in `0..=1`.
#[cfg(feature = "portal")]
fn accent_of(value: zbus::zvariant::OwnedValue) -> Option<Color32> {
    use zbus::zvariant::{Structure, Value};
    let structure = Structure::try_from(unwrap_variant(value)).ok()?;
    match structure.fields() {
        [Value::F64(r), Value::F64(g), Value::F64(b)] => Some(from_unit(*r, *g, *b)),
        _ => None,
    }
}

/// `#3584E4` arrives as `0.20784313, 0.5176471, 0.89411765` — the same triple the
/// memo's probe printed, which is what pins the rounding.
#[cfg(feature = "portal")]
fn from_unit(r: f64, g: f64, b: f64) -> Color32 {
    let channel = |c: f64| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgb(channel(r), channel(g), channel(b))
}

/// GNOME 47's accent names and the colours libadwaita gives them.
fn accent_named(name: &str) -> Option<Color32> {
    let (r, g, b) = match name {
        "blue" => (0x35, 0x84, 0xE4),
        "teal" => (0x21, 0x90, 0xA4),
        "green" => (0x3A, 0x94, 0x4A),
        "yellow" => (0xC8, 0x88, 0x00),
        "orange" => (0xED, 0x5B, 0x00),
        "red" => (0xE6, 0x2D, 0x42),
        "pink" => (0xD5, 0x61, 0x99),
        "purple" => (0x91, 0x41, 0xAC),
        "slate" => (0x6F, 0x83, 0x96),
        _ => return None,
    };
    Some(Color32::from_rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The portal's own numbers for `#3584E4`, from the probe in
    /// `docs/gtk-palette.md` §6. Truncating instead of rounding gives `#34` or
    /// `#83`, which is invisible on screen and fatal to a snapshot.
    #[cfg(feature = "portal")]
    #[test]
    fn the_portal_accent_is_three_unit_floats() {
        let accent = from_unit(
            0.207_843_139_767_646_8,
            0.517_647_087_574_005_1,
            0.894_117_653_369_903_6,
        );
        assert_eq!(accent, Color32::from_rgb(0x35, 0x84, 0xE4));
        // Out of range is clamped rather than wrapped around to a random colour.
        assert_eq!(from_unit(-1.0, 2.0, 0.5), Color32::from_rgb(0, 255, 0x80));
    }

    /// A theme that is not Adwaita is refused, not approximated: `Breeze` and
    /// `Materia` have no palette we can read, and Adwaita's surfaces on them are a
    /// lie about the desktop.
    #[test]
    fn only_adwaita_and_its_ports_pass_the_gate() {
        for name in [
            "Adwaita",
            "Adwaita:dark",
            "Adwaita-dark",
            "adw-gtk3",
            "adw-gtk3-dark",
        ] {
            assert!(adwaita_like(name), "{name} should pass");
        }
        for name in [
            "Breeze",
            "Materia",
            "Adwaita-breeze",
            "HighContrast",
            "Adwaitaaa",
        ] {
            assert!(!adwaita_like(name), "{name} should be refused");
        }
    }

    /// A name with neither suffix does not say which scheme it is, and picking one
    /// would put a white chrome on a dark desktop half the time.
    #[test]
    fn a_theme_name_states_its_scheme_only_when_it_does() {
        assert_eq!(theme_scheme("Adwaita:dark"), Some(true));
        assert_eq!(theme_scheme("Adwaita:light"), Some(false));
        assert_eq!(theme_scheme("Adwaita-dark"), Some(true));
        assert_eq!(theme_scheme("adw-gtk3-light"), Some(false));
        assert_eq!(theme_scheme("Adwaita"), None);
        assert_eq!(theme_scheme("Breeze"), None);
    }

    /// The theme's name is the gate, so reading it out of a file has to be exact:
    /// `[Settings]` only, no trailing newline, no quotes.
    #[test]
    fn the_ini_file_names_the_theme() {
        let text = "\
[Settings]
gtk-theme-name=adw-gtk3-dark
gtk-icon-theme-name=Adwaita
";
        assert_eq!(theme_in_ini(text).as_deref(), Some("adw-gtk3-dark"));
        // A key outside `[Settings]` is not the one GTK reads.
        let elsewhere = "[Other]\ngtk-theme-name=Breeze\n";
        assert_eq!(theme_in_ini(elsewhere), None);
        assert_eq!(theme_in_ini("[Settings]\ngtk-theme-name=\n"), None);
    }

    #[test]
    fn the_accent_names_are_libadwaitas() {
        assert_eq!(accent_named("blue"), Some(ADWAITA_ACCENT));
        assert_eq!(
            accent_named("slate"),
            Some(Color32::from_rgb(0x6F, 0x83, 0x96))
        );
        assert_eq!(accent_named("chartreuse"), None);
    }
}
