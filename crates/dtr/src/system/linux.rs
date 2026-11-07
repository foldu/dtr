//! What a GNOME session says its chrome looks like.
//!
//! Two facts decide the native skin: the scheme, and the accent, and both come
//! from the XDG settings portal (`org.freedesktop.appearance`) — the same service
//! libadwaita itself asks. The theme *name* is the gate, and it comes from the two
//! places a plain reader can look, `GTK_THEME` and `settings.ini`: see
//! [`adwaita_like`].
//!
//! Nothing here parses CSS. The surfaces are libadwaita 1.9.3's own constants,
//! measured out of its gresource and carrying a light and a dark half, so the
//! only values a session can change are the two above — see `docs/gtk-palette.md`
//! §8 for what a second version would have to read to change the rest.
//!
//! The portal half is behind the `portal` feature: without it the scheme comes
//! from the theme's own name and nothing watches, which is the build for a target
//! that will not take zbus's tree (`docs/gtk-palette.md` §10).

// The portal helpers are the dead half of a `--no-default-features` build.
#![cfg_attr(not(feature = "portal"), allow(dead_code))]

use std::time::Duration;

use dtr_widgets::{ADWAITA_ACCENT, Skin};
use egui::Color32;

use super::{Platform, Reading};

/// The ceiling on one reading.
///
/// The portal answers in 2–2.6 ms measured, so this is a guard against a stuck
/// D-Bus daemon, not a delay: the first frame must not wait for the desktop to
/// wake up.
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

pub(super) struct Portal;

impl Platform for Portal {
    fn source(&self) -> &'static str {
        // Whichever reader will answer: with the feature off there is no portal
        // call to name, only the theme's name in a file.
        if cfg!(feature = "portal") {
            "portal"
        } else {
            "gtk-config"
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
    /// The portal re-emits these from the settings daemon, so this covers a GNOME
    /// session whatever set the value: the settings app, `gsettings` in a
    /// terminal, or a theme switcher. A session with no portal never gets here,
    /// because there is nothing to subscribe to then and the skin stays at what it
    /// was.
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

    /// Without the portal there is no change to wait for: a theme name in a file
    /// has no notification, so the skin is whatever it was at startup. A future
    /// that never resolves is the honest spelling of that, rather than polling a
    /// file on a timer to notice an edit nobody makes.
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
    let theme = theme_name();
    if !adwaita_like(&theme) {
        return Err(format!("theme {theme:?} is not Adwaita"));
    }
    // The two portal values over one connection: the bus is the expensive part,
    // and asking twice would mean two of them.
    let (scheme, accent) = portal_values().await.unwrap_or((None, None));
    let dark = match scheme {
        // 0 is "no preference", which is not "light": it means the scheme is the
        // theme's, and guessing here is how a white chrome lands on a dark
        // desktop.
        Some(1) => true,
        Some(2) => false,
        _ => theme_scheme(&theme).ok_or_else(|| {
            format!("no portal, and theme {theme:?} does not say whether it is light or dark")
        })?,
    };
    Ok(Reading {
        dark,
        // No portal means no accent either. Blue is libadwaita's own default, so
        // this is the desktop's colour rather than a guess at the user's.
        accent: Some(accent.unwrap_or(ADWAITA_ACCENT)),
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

/// The theme's name, from the two places a plain reader can look: the debug
/// override and the config file. `Adwaita` is GTK's own default when neither says
/// anything, and it is what a GNOME session with no `settings.ini` is running.
///
/// The settings *daemon* would be a third, and reading it means running
/// `gsettings`: there is no read API on dconf's bus interface, by design. dtr does
/// not spawn a process to find out what colour its own chrome is.
fn theme_name() -> String {
    if let Ok(value) = std::env::var("GTK_THEME") {
        if !value.trim().is_empty() {
            return value;
        }
    }
    settings_ini_theme().unwrap_or_else(|| "Adwaita".to_owned())
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

/// The portal's scheme (as `u`) and accent (as `(ddd)`), over one connection.
///
/// A missing bus, a missing portal and a portal that answers an error are all
/// `None`: each of them means "the theme's name decides", and none of them is
/// worth stopping a terminal for.
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

/// Without the feature there is nothing to ask: the scheme comes from the theme's
/// name, and the accent is libadwaita's default.
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
}
