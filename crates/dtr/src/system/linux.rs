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

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use dtr_widgets::{ADWAITA_ACCENT, Skin};
use egui::Color32;

use super::{Icon, Platform, Reading};

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
        compio::time::timeout(BUDGET, gather())
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
                compio::time::sleep(RETRY).await;
                continue;
            };
            let Ok(proxy) = zbus::Proxy::new(&connection, PORTAL, PORTAL_PATH, PORTAL_IFACE).await
            else {
                compio::time::sleep(RETRY).await;
                continue;
            };
            let Ok(mut stream) = proxy.receive_signal("SettingChanged").await else {
                compio::time::sleep(RETRY).await;
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
            compio::time::sleep(RETRY).await;
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

/// The picture this machine has for a program, read the way a launcher reads it.
///
/// A launcher does not guess from a file name: the program's entry under
/// `share/applications` says `Icon=`, and the icon theme the same package
/// installs holds the picture under that name. `baash` ships both, which is why
/// this exists — it is the one shell on this machine with a mark of its own to
/// read. The words in an entry are, in order: `Exec=` or `TryExec=` says which
/// program it is, `Icon=` says what to draw.
///
/// `None` is a program no package gave an entry, which is most shells: the
/// caller falls back to the project's own logo, and then to no mark at all.
pub(super) fn app_icon(program: &str) -> Option<Icon> {
    let roots = search_roots(program);
    let icon = entry_icon(program, &roots)?;
    let named = Path::new(&icon);
    if named.is_absolute() {
        return load(named);
    }
    in_theme(&icon, &roots)
}

/// The `share/` roots worth asking, nearest first: the package the program came
/// from, then the XDG data directories a desktop reads.
///
/// The program's own prefix is asked first because it is the only one that holds
/// a *package's* entry: `~/.nix-profile/share` is on `XDG_DATA_DIRS` here, but a
/// shell built into a store path nowhere else is, and its share directory is
/// still beside its binary.
fn search_roots(program: &str) -> Vec<PathBuf> {
    let mut roots = package_shares(program);
    if let Some(data) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        push_unique(&mut roots, data.into());
    } else if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        push_unique(&mut roots, Path::new(&home).join(".local/share"));
    }
    let dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("/usr/local/share:/usr/share"));
    for dir in std::env::split_paths(&dirs) {
        push_unique(&mut roots, dir);
    }
    roots
}

/// The `share/` directories of the packages a program may have come from: the
/// path it was found at, and the file that path resolves to.
///
/// Both, because on a NixOS system `/run/current-system/sw/bin/baash` is a
/// profile and the file behind it is a store path, and each keeps a `share/` the
/// other may not — the profile merges every package's desktop entries, the store
/// path has the one the program's own package installed.
fn package_shares(program: &str) -> Vec<PathBuf> {
    let exe = Path::new(program);
    let mut shares = Vec::new();
    for exe in [Some(exe.to_path_buf()), exe.canonicalize().ok()]
        .into_iter()
        .flatten()
    {
        if let Some(prefix) = exe.parent().and_then(Path::parent) {
            push_unique(&mut shares, prefix.join("share"));
        }
    }
    shares
}

fn push_unique(roots: &mut Vec<PathBuf>, root: PathBuf) {
    if !roots.contains(&root) {
        roots.push(root);
    }
}

/// The `Icon=` of the entry that runs `program`.
///
/// A program with no entry of its own — most shells — costs a walk of every
/// `applications/` directory, and a menu asks about every shell it offers in one
/// frame. So the listings and the parsed files are kept: the first shell of the
/// first menu pays for the walk, and the rest are look-ups.
fn entry_icon(program: &str, roots: &[PathBuf]) -> Option<String> {
    for root in roots {
        let files = listing(&root.join("applications"));
        for path in files.iter() {
            let Some(entry) = parsed(path) else {
                continue;
            };
            if entry.runs(program)
                && let Some(icon) = entry.icon
            {
                return Some(icon);
            }
        }
    }
    None
}

/// One directory's `.desktop` files, in a fixed order.
///
/// A directory listing has no order, and two entries naming one program would
/// otherwise pick a different picture between runs.
fn listing(dir: &Path) -> Arc<Vec<PathBuf>> {
    static LISTINGS: OnceLock<Mutex<HashMap<PathBuf, Arc<Vec<PathBuf>>>>> = OnceLock::new();
    let listings = LISTINGS.get_or_init(Default::default);
    let mut listings = listings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(listing) = listings.get(dir) {
        return listing.clone();
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("desktop"))
        })
        .collect();
    files.sort();
    let files = Arc::new(files);
    listings.insert(dir.to_owned(), files.clone());
    files
}

/// One desktop file, parsed once for the run.
fn parsed(path: &Path) -> Option<Entry> {
    static ENTRIES: OnceLock<Mutex<HashMap<PathBuf, Option<Entry>>>> = OnceLock::new();
    let entries = ENTRIES.get_or_init(Default::default);
    let mut entries = entries
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(entry) = entries.get(path) {
        return entry.clone();
    }
    let entry = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| parse(&text));
    entries.insert(path.to_owned(), entry.clone());
    entry
}

/// What a desktop file's `[Desktop Entry]` group says, and nothing else: an
/// action group carries an `Icon=` of its own, and answering with that one would
/// draw an action's picture for the program.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Entry {
    exec: Option<String>,
    try_exec: Option<String>,
    icon: Option<String>,
}

impl Entry {
    /// Whether this entry starts `program`.
    fn runs(&self, program: &str) -> bool {
        if let Some(command) = self.exec.as_deref().and_then(command_of) {
            if same_program(&command, program) {
                return true;
            }
        }
        self.try_exec
            .as_deref()
            .is_some_and(|try_exec| same_program(try_exec, program))
    }
}

/// The `[Desktop Entry]` group of a desktop file, or `None` where the file has
/// none.
fn parse(text: &str) -> Option<Entry> {
    let mut entry = Entry::default();
    let mut inside = false;
    let mut seen = false;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            inside = line == "[Desktop Entry]";
            seen = seen || inside;
            continue;
        }
        if !inside {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = Some(value.trim().to_owned());
        match key.trim() {
            "Exec" => entry.exec = value,
            "TryExec" => entry.try_exec = value,
            "Icon" => entry.icon = value,
            _ => {}
        }
    }
    seen.then_some(entry)
}

/// The program an `Exec=` line runs: its first word, after any `env VAR=value`
/// prefix — how a package sets an environment variable it needs. A placeholder
/// (`%U`, `%f`) is an argument and never the program.
fn command_of(exec: &str) -> Option<String> {
    let mut words = words(exec);
    if words.first().is_some_and(|word| word == "env") {
        words.remove(0);
        while words.first().is_some_and(|word| word.contains('=')) {
            words.remove(0);
        }
    }
    words.into_iter().next()
}

/// An `Exec=` value split into words, double quotes honoured: the specification
/// allows a quoted program whose path holds a space.
fn words(line: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for character in line.chars() {
        match character {
            '"' => quoted = !quoted,
            _ if character.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(character),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// Whether a command an entry names is this program.
///
/// The name is what decides — a bare `baash` is on `PATH` and is this one — and
/// an absolute path must also be the same file, or two installs of one shell in
/// one profile would share a picture they do not share.
fn same_program(command: &str, program: &str) -> bool {
    let named = Path::new(command);
    let wanted = Path::new(program);
    if named.file_name() != wanted.file_name() {
        return false;
    }
    if named.is_absolute() && wanted.is_absolute() {
        return match (named.canonicalize(), wanted.canonicalize()) {
            (Ok(named), Ok(wanted)) => named == wanted,
            _ => named == wanted,
        };
    }
    true
}

/// The picture `name` has in the icon theme, at the size a menu row wants.
///
/// `hicolor` is freedesktop's fallback theme and the one a package installs
/// into, so it is the only theme asked. The order is "the nearest to 64 px the
/// package has": a row is 18 points, so the renderer only ever scales down, and a
/// 512 px original would be a megabyte of texture for a 20 px mark. `scalable/`
/// is SVG, which `image` cannot rasterise, so it is skipped rather than guessed
/// at.
fn in_theme(name: &str, roots: &[PathBuf]) -> Option<Icon> {
    const SIZES: [&str; 9] = [
        "64x64", "48x48", "96x96", "128x128", "256x256", "512x512", "32x32", "24x24", "16x16",
    ];
    for root in roots {
        for size in SIZES {
            let path = root
                .join("icons/hicolor")
                .join(size)
                .join("apps")
                .join(format!("{name}.png"));
            if let Some(icon) = load(&path) {
                return Some(icon);
            }
        }
    }
    None
}

/// One picture, as RGBA in the top row first.
fn load(path: &Path) -> Option<Icon> {
    let bytes = std::fs::read(path).ok()?;
    let image = image::load_from_memory(&bytes).ok()?.into_rgba8();
    let (width, height) = image.dimensions();
    // A header can claim anything, and a picture bigger than this is a wall, not
    // a mark: `prepare` would upload it whole.
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return None;
    }
    Some(Icon {
        width: width as usize,
        height: height as usize,
        rgba: image.into_raw(),
    })
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

    /// A piece of a package's `share/`, laid out the way a real one is.
    fn write_entry(root: &Path, name: &str, body: &str) {
        let dir = root.join("applications");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn write_png(root: &Path, size: &str, name: &str, pixels: u32) {
        let dir = root.join("icons/hicolor").join(size).join("apps");
        std::fs::create_dir_all(&dir).unwrap();
        image::RgbaImage::from_pixel(pixels, pixels, image::Rgba([1, 2, 3, 255]))
            .save(dir.join(format!("{name}.png")))
            .unwrap();
    }

    /// The entry is found by the program it runs, not by the file it is called:
    /// `baash.desktop` is the package's choice, and a package that names its
    /// entry `org.example.baash.desktop` says the same thing in a different word.
    /// `Icon=` is then the picture's name, which is not a path.
    #[test]
    fn the_entry_that_runs_a_program_names_its_icon() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        write_entry(
            &root,
            "fish.desktop",
            "[Desktop Entry]\nType=Application\nExec=fish\nIcon=fish\n",
        );
        write_entry(
            &root,
            "org.example.baash.desktop",
            "[Desktop Entry]\nType=Application\nExec=baash --login\nIcon=baash\n",
        );
        let roots = std::slice::from_ref(&root);
        assert_eq!(
            entry_icon("/usr/bin/baash", roots).as_deref(),
            Some("baash")
        );
        assert_eq!(entry_icon("/usr/bin/fish", roots).as_deref(), Some("fish"));
        assert_eq!(entry_icon("/usr/bin/zsh", &[root]), None);
    }

    /// An action group carries an `Icon=` of its own, and it is an action's
    /// picture: the program's is the one in `[Desktop Entry]`.
    #[test]
    fn an_action_groups_icon_is_not_the_programs() {
        let entry = parse(
            "[Desktop Entry]\nExec=baash\nIcon=baash\n\
             [Desktop Action new-window]\nExec=baash --new-window\nIcon=new-window\n",
        )
        .expect("the entry parses");
        assert_eq!(entry.icon.as_deref(), Some("baash"));
        assert!(entry.runs("/run/current-system/sw/bin/baash"));
    }

    /// `env` and its assignments, and a URL placeholder, are arguments around the
    /// program — and a quoted program keeps the space in its path.
    #[test]
    fn env_and_placeholders_do_not_change_the_program() {
        for exec in [
            "baash",
            "env FOO=1 baash",
            "env FOO=1 BAR=2 baash --login",
            "/nix/store/x-baash-0.0.0/bin/baash %U",
        ] {
            let entry = parse(&format!("[Desktop Entry]\nExec={exec}\nIcon=baash\n"))
                .expect("the entry parses");
            assert!(
                entry.runs("/nix/store/x-baash-0.0.0/bin/baash"),
                "Exec={exec}"
            );
        }
        let quoted =
            parse("[Desktop Entry]\nExec=\"/opt/My Shell/bin/baash\" %U\nIcon=x\n").unwrap();
        assert!(quoted.runs("/opt/My Shell/bin/baash"));
    }

    /// A bare command is on `PATH` and is this program; an absolute one has to be
    /// the same file, or two installs of one shell would share a picture they do
    /// not share.
    #[test]
    fn an_absolute_command_names_one_install() {
        let entry = parse("[Desktop Entry]\nExec=/opt/one/baash\nIcon=x\n").expect("it parses");
        assert!(entry.runs("/opt/one/baash"));
        assert!(!entry.runs("/opt/two/baash"));
        let bare = parse("[Desktop Entry]\nExec=baash\nIcon=x\n").expect("it parses");
        assert!(bare.runs("/opt/one/baash"));
    }

    /// The theme answers at the size a row wants — nearest 64 px, because the row
    /// only ever scales down — and a name nobody installed is no picture.
    #[test]
    fn the_theme_answers_at_the_size_the_row_wants() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_png(root, "16x16", "baash", 16);
        write_png(root, "64x64", "baash", 64);
        write_png(root, "512x512", "solo", 512);
        let roots = vec![root.to_path_buf()];
        let icon = in_theme("baash", &roots).expect("baash has a picture");
        assert_eq!((icon.width, icon.height), (64, 64));
        assert_eq!(in_theme("solo", &roots).map(|icon| icon.width), Some(512));
        assert_eq!(in_theme("nobody", &roots), None);
    }

    /// A picture is read from the file it names, and a file that is not one is
    /// no picture rather than a panic.
    #[test]
    fn a_picture_is_read_from_its_own_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mark.png");
        image::RgbaImage::from_pixel(7, 5, image::Rgba([9, 8, 7, 255]))
            .save(&path)
            .unwrap();
        let icon = load(&path).expect("a PNG decodes");
        assert_eq!((icon.width, icon.height), (7, 5));
        assert_eq!(icon.rgba.len(), 7 * 5 * 4);
        assert_eq!(load(&dir.path().join("absent.png")), None);
        std::fs::write(dir.path().join("not.png"), b"not a picture").unwrap();
        assert_eq!(load(&dir.path().join("not.png")), None);
    }

    /// The program's own package is asked before the shared directories: a store
    /// path holds the entry its package installed, and the profile holds the
    /// merged ones.
    #[test]
    fn the_package_a_program_came_from_is_asked_first() {
        assert_eq!(
            package_shares("/opt/baash/bin/baash"),
            vec![PathBuf::from("/opt/baash/share")]
        );
        assert!(package_shares("/nope/bin/baash").contains(&PathBuf::from("/nope/share")));
    }
}
