# Reading the GTK palette on Linux

A decision memo for the Linux half of `native`. Read `docs/ui.md` first: it says
what a skin is and why `native` exists. This resolves its "Not decided: Linux
accent" and leaves a smaller list behind.

The question is narrow: when `[ui] widgets = "native"` on Linux, where do the
colours come from, and how does dtr read them **without linking GLib**? No
gtk-rs, no glib-sys, no gio, no `pkg-config`. Anything that needs the library
has to be replaced by a file, a binary or a subprocess. The constraint is the
maintainer's, not a preference of this memo; §9.1 evaluates the GLib route on
its merits and says what it would buy.

## 1. Recommendation

**Measured.** The surfaces a libadwaita app shows are Adwaita's, and they are fixed
constants for a given version. Only two things vary: the colour scheme and the
accent, and the XDG desktop portal publishes both
(`org.freedesktop.appearance`). libadwaita itself reads that portal; the strings
are in the library.

So:

- read the portal over the session bus with **`zbus`** (a pure-Rust D-Bus
  client, MIT, no GLib link) on its **`tokio`** transport, because dtr already
  runs tokio: one connection and two `Read` calls, with a 500 ms ceiling on the
  whole reading;
- read the *theme name* from `GTK_THEME` and `settings.ini` — the two places a
  plain reader can look — and refuse a name that is not Adwaita rather than
  painting Adwaita's surfaces on Breeze; no subprocess, and `None` when nothing
  says which scheme it is;
- build `Palette::adwaita(dark, accent)` from constants measured out of
  `libadwaita-1.so`, and `Skin::adwaita(dark, accent)` from it — which also
  carries `Style::adwaita()`, because Adwaita's *metrics* are in the same
  stylesheet and a palette over dtr's 23-point rows is theming, not a window on
  this desktop (measured in §6.1);
- keep reading: the portal publishes `SettingChanged` and Windows sends
  `WM_SETTINGCHANGE`, so the skin a window started with is not the one it has to
  keep (§6.2);
- do **not** parse libadwaita's gresource and do **not** parse user CSS in v1.
  A parser that cannot evaluate `color-mix()`, `oklab()` or the injected accent
  would produce a palette GTK does not show, which is worse than the dtr skin.

The only new dependency is **`zbus` 5** (MIT), on its `tokio` transport, and it
was taken (§6.3). It is a D-Bus client, not a toolkit: it links no GLib, it
reuses the runtime dtr already runs, and it reads the same service `rfd`'s
`xdg-portal` backend already uses for the file dialog. It does bring a tree — 77
lock entries in a probe, against zero crates for the `busctl --json=short`
variant in §4.2, which is the same design with a subprocess instead of a client.
That was the trade: about 2 ms once per process against a dependency tree that
every future `cargo deny` run has to re-approve.

**Measured against GTK itself.** A throwaway GTK4/libadwaita probe (§3.2.1)
resolves `window_bg_color #222226`, `accent_bg_color #3584E4`,
`accent_fg_color #FFFFFF` and the light set, and they are exactly the values the
portal plus the extracted constants produce. The simple route is therefore not
a guess on this machine; it is the same answer with fewer moving parts. The
probe is a development check, not a build input (§9.1).

The failure this prevents: `native` on Linux meaning "dtr" while the user is
looking at an Adwaita desktop, and a half-parsed `gtk.css` producing a third
look that matches neither.

## 2. What was wrong

`native` on Linux resolves to the `dtr` skin "by decision: there is no portable
system palette" (`docs/ui.md` §Two presets). That was a defensible answer when
it was written. It is no longer the whole truth:

- the XDG portal now publishes `color-scheme` and `accent-color`, and
  libadwaita reads them;
- libadwaita's surfaces are not a user preference at all — they are one
  stylesheet compiled into `libadwaita-1.so`, with a light and a dark half;
- so "the GTK palette" is a function of two values plus a library version.

**Measured, this machine.** NixOS 26.11 (Zokor), kernel 7.3.0, Wayland.
`XDG_CURRENT_DESKTOP=niri`, `DESKTOP_SESSION=niri`, so the session reports
**niri, not GNOME**; GNOME's settings schemas and portals are installed, and are what
GTK apps here use. `GTK_THEME` is unset. GTK 4.22.4 and libadwaita 1.9.3 are in
the store. `gsettings`, `dconf`, `gdbus` and `busctl` are on `PATH`;
`pkg-config` is not, which does not matter because nothing here links anything.
`Cargo.lock` has no `glib`, `gtk`, `gio`, `gdk`, `pango` or `gobject` crate, and
`rfd` is built with `xdg-portal`, not `gtk3`, so the tree is already GTK-free by
policy.

## 3. Where the colours live

There are three different answers, and they are not the same palette.

### 3.1 Stock GTK4 without libadwaita: the Default theme

**Measured.** GTK 4.22.4 ships no `share/themes` directory. Its themes are
gresources inside `libgtk-4.so.1`:

```
$ gresource list …/libgtk-4.so.1 | grep /theme/
/org/gtk/libgtk/theme/Default/Default-dark.css
/org/gtk/libgtk/theme/Default/Default-light.css
/org/gtk/libgtk/theme/Default/gtk.css
…
```

`gtk.css` is a 599-byte wrapper of four `@media` blocks, each an
`@import url("resource:///org/gtk/libgtk/theme/Default/Default-light.css")`
(or `-dark`, `-hc`, `-hc-dark`). `Default-dark.css` is 146,583 bytes with 37
`@define-color` declarations and **no** `:root`, `var()`, `oklab()` or
`color-mix()`:

| token | dark | light |
|---|---|---|
| `theme_bg_color` | `#353535` | `#f6f5f4` |
| `theme_fg_color` | `#eeeeec` | `#2e3436` |
| `theme_base_color` | `#2d2d2d` | `#ffffff` |
| `theme_selected_bg_color` | `#15539e` | `#3584e4` |
| `borders` | `#1b1b1b` | `#cdc7c2` |
| `accent_color` | `#3584E4` | `#3584E4` |

The accent is **hard-coded**. A plain GTK4 app does not follow the user's accent;
only an app that links libadwaita does. "Stock GTK4 with the Adwaita theme" is
therefore a category error on GTK 4.22: Adwaita is not GTK's theme any more.
GTK's own fallback is Default, and it is a different palette (`#353535` dark
against libadwaita's `#222226`).

### 3.2 libadwaita: Adwaita is a gresource, not a file

**Measured.** `libadwaita-1.9.3` ships no CSS under `share/`; its only theme
files are compiled into `lib/libadwaita-1.so.0`:

```
$ gresource list …/libadwaita-1.so.0 | grep styles
/org/gnome/Adwaita/styles/gtk.css
/org/gnome/Adwaita/styles/assets/…
$ gresource extract … /org/gnome/Adwaita/styles/gtk.css | wc -c
431418
```

The extracted stylesheet is one 431,418-byte file containing **both** schemes:
the light values are plain `@define-color` declarations, and the dark values sit
inside `@media (prefers-color-scheme: dark) { … }` (line 94). It uses both
mechanisms the question asks about:

- **`@define-color`** for the concrete values: 149 declarations, e.g.
  `window_bg_color #fafafb`, `card_bg_color RGB(255 255 255 / 8%)`,
  `card_fg_color RGB(0 0 6 / 80%)`;
- **`:root { --name: … }`** as an alias layer:
  `--window-bg-color: @window_bg_color;`, 90 occurrences of
  `--accent-bg-color`.

The measured gap that decides the design: **`@accent_bg_color` is never defined
in the CSS.** The library injects it at startup from a format string in the
`.so`:

```
$ strings -a …/libadwaita-1.so.0 | grep accent_bg_color
@define-color accent_bg_color %s;
@define-color accent_fg_color white;
```

The nine accent values are in the library too:

```
$ strings -a …/libadwaita-1.so.0 | grep -o '#[0-9a-f]\{6\}' | sort -u
#2190a4  #3584e4  #3a944a  #6f8396  #9141ac
#c88800  #d56199  #e62d42  #ed5b00
```

So a parser that reads only the file cannot get the accent at all — the value is
a runtime injection — and `accent_fg_color` is *always white* in 1.9.3, not
contrast-adaptive.

The direct values a mapping can use (the probe in §3.2.1 resolves the same
names):

| `Palette` slot | Adwaita name | dark | light |
|---|---|---|---|
| `window` | `window_bg_color` | `#222226` | `#fafafb` |
| `well` | `view_bg_color` | `#1d1d20` | `#ffffff` |
| `card` | `dialog_bg_color` / `popover_bg_color` | `#36363a` | `#fafafb` |
| `text` | `window_fg_color` | `white` | `RGB(0 0 6 / 80%)` |
| `danger` | `destructive_bg_color` | `@red_4` `#c01c28` | `@red_3` `#e01b24` |
| `on_danger` | `destructive_fg_color` | `white` | `white` |
| `on_accent` | `accent_fg_color` (injected) | `white` | `white` |

The rest of the sheet is not simple: 493 `color-mix(`, 34 `oklab(`, 293
`currentColor`, 46 gradients, 200 space-separated `RGB(`, 5 `rgb(`, 0 `rgba(`,
0 `hsl(`, 0 `@import`. That matters for §7.

### 3.2.1 What GTK itself resolves

**Measured.** A throwaway GTK4/libadwaita probe (compiled in `/tmp`, not part
of dtr, not a dependency) ran against the live Wayland session and resolved the
theme colours on a `GtkLabel`'s style context. Dark is the session's own
scheme; light is forced with `ADW_DEBUG_COLOR_SCHEME=prefer-light`.

| name | dark (resolved) | light (resolved) |
|---|---|---|
| `window_bg_color` | `#222226` | `#FAFAFB` |
| `window_fg_color` | `#FFFFFF` | `#000006` @ 0.80 |
| `view_bg_color` | `#1D1D20` | `#FFFFFF` |
| `view_fg_color` | `#FFFFFF` | `#000006` @ 0.80 |
| `card_bg_color` | `#FFFFFF` @ 0.08 | `#FFFFFF` |
| `dialog_bg_color` | `#36363A` | `#FAFAFB` |
| `popover_bg_color` | `#36363A` | `#FFFFFF` |
| `headerbar_bg_color` | `#2E2E32` | `#FFFFFF` |
| `accent_bg_color` | `#3584E4` | `#3584E4` |
| `accent_fg_color` | `#FFFFFF` | `#FFFFFF` |
| `destructive_bg_color` | `#C01C28` | `#E01B24` |
| `shade_color` | `#000006` @ 0.25 | `#000006` @ 0.07 |

The style manager reported `dark=1 high_contrast=0 supports_accent=1
accent_enum=0` (blue), and `accent_rgba` was `#3584E4` — the portal value
exactly.

These are the same numbers as the extracted declarations in §3.2, which is the
cross-check that the vendored constants are not a guess. Two details the
declarations alone do not show: `card_bg_color` really is *translucent* (8 %
white dark, opaque white light), so the `Palette` slot must composite it; and
light `window_fg_color` is `#000006` at 0.80, which composites to `#323237`
over `#FAFAFB`. The light `popover_bg_color` is `#FFFFFF` while
`dialog_bg_color` is `#FAFAFB`; the `card` slot follows the dialog, which is
what dtr's "dialog or panel fill" means.

The probe also printed GTK's own complaint about the user stylesheet:

```
Gtk-WARNING: Theme parser error: gtk.css:5:1-122: Failed to import:
Error opening file …/share/themes/Adwaita-dark/gtk-4.0/gtk.css: No such file or directory
```

§3.4's dangling import is therefore not an inference: GTK fails it too.

### 3.3 A readable theme: adw-gtk3

**Measured.** The installed `adw-gtk3 6.5` is the one GTK4 theme with files on
disk:

```
~/.nix-profile/share/themes/adw-gtk3{,-dark}/gtk-4.0/
  gtk.css               69 B   @import url('libadwaita.css'); @import url('libadwaita-tweaks.css');
  gtk-dark.css          69 B   (identical)
  libadwaita.css        431,772 B  a near-copy of libadwaita's gresource
  libadwaita-tweaks.css 2,226 B    @define-color accent_bg_color @blue_3; @define-color accent_fg_color white; …
```

So a *readable* Adwaita-shaped palette does exist on this machine. The user's
config does not name it (§3.4).

### 3.4 The user override: a dangling import

**Measured.** `~/.config/gtk-4.0/` contains:

```
gtk.css        -> /nix/store/vkgp…-hm_gtk4.0gtk.css     5 lines, below
settings.ini   -> /nix/store/bh3d…-hm_gtk4.0settings.ini
colors.css       4,998 B, @define-color …_breeze … (Breeze/KDE)
```

`gtk.css` is a comment plus one import:

```css
/**
 * GTK 4 reads the theme configured by gtk-theme-name, but ignores it.
 * It does however respect user CSS, so import the theme from here.
**/
@import url("file:///nix/store/lkmmi3xlvzxw9a2899r3vigy5k04bv60-adw-gtk3-6.5/share/themes/Adwaita-dark/gtk-4.0/gtk.css");
```

That path **does not exist**:

```
$ ls …/adw-gtk3-6.5/share/themes/
adw-gtk3  adw-gtk3-dark
$ find /nix/store -type d -path '*/share/themes/Adwaita-dark/gtk-4.0'
(no output)
```

GTK itself reports the same failure when it initialises the user stylesheet
(`Theme parser error: gtk.css:5:1-122: Failed to import … No such file or
directory`, §3.2.1), so the dangling import is measured, not inferred.

`gtk-dark.css` does not exist in `~/.config/gtk-4.0/`. `settings.ini` names a
theme, a font and an icon theme, not a colour:

```ini
[Settings]
gtk-font-name=Geist 11
gtk-icon-theme-name=Adwaita
gtk-theme-name=Adwaita-dark
```

`colors.css` is Breeze's GTK3 palette (`@define-color theme_bg_color_breeze
#202326`, names suffixed `_breeze`), imported only by
`~/.config/gtk-3.0/gtk.css` (`@import 'colors.css';`), not by the GTK4 file. The
two configs use different stylesheets; neither is a reliable palette for GTK4.

**Measured conclusion.** There is no readable file on this machine that
actually resolves to the palette a libadwaita app uses:

- the only user stylesheet is a dangling import;
- `gtk-theme-name=Adwaita-dark` names a theme directory that does not exist;
  GTK4 has no Adwaita theme on disk, and libadwaita chooses dark from
  `color-scheme`, not from `gtk-theme-name`;
- the only readable Adwaita copy (adw-gtk3) is not named by the config;
- the real libadwaita stylesheet is a 431 KB gresource.

That is the case the design has to be honest about.

## 4. Dark, light and accent without GLib

### 4.1 What is installed and what it says

**What "the portal" is.** The XDG desktop portal: a session-bus service at
`org.freedesktop.portal.Desktop`, object `/org/freedesktop/portal/desktop`,
interface `org.freedesktop.portal.Settings`, method
`Read(namespace, key) -> v`. The namespace is `org.freedesktop.appearance`; the
keys are `color-scheme` (`0` no preference, `1` prefer dark, `2` prefer light)
and `accent-color` (three doubles, RGB in 0..1). It is not GTK and not
GNOME-specific; here it is `xdg-desktop-portal` with
`org.freedesktop.impl.portal.desktop.gnome`, and dtr already reaches it through
`rfd`'s `xdg-portal` backend for the file dialog.

**Measured.** `gsettings` (glib 2.88.3), `dconf`, `gdbus` and `busctl` (systemd
262) are present. The session bus is at `unix:path=/run/user/1000/bus`, and
`org.freedesktop.portal.Desktop` is running.

```
$ gsettings get org.gnome.desktop.interface color-scheme
'prefer-dark'
$ gsettings get org.gnome.desktop.interface accent-color
'blue'
$ gsettings get org.gnome.desktop.interface gtk-theme
'Adwaita-dark'

$ GSETTINGS_BACKEND=memory gsettings get … color-scheme
'default'                    # schema default; dconf has an override
$ GSETTINGS_BACKEND=memory gsettings get … accent-color
'blue'                       # the schema default too
$ dconf read /org/gnome/desktop/interface/accent-color
(blank)                      # no stored key; the value is the schema default
```

The `dconf read` blank is the important one: `dconf` reports what is *stored*,
not the effective value; `gsettings` applies the schema default. A reader built
on `dconf read` would think the accent is unset and would still have to know the
default.

**Measured.** The portal answers with resolved values:

```
$ busctl --user call org.freedesktop.portal.Desktop \
    /org/freedesktop/portal/desktop org.freedesktop.portal.Settings \
    Read ss org.freedesktop.appearance color-scheme
v v u 1
$ busctl --user call … Read ss org.freedesktop.appearance accent-color
v v (ddd) 0.207843 0.517647 0.894118
$ busctl --user call … Get ss org.freedesktop.portal.Settings version
v u 2
```

`u 1` is "prefer dark" (0 = no preference, 1 = prefer dark, 2 = prefer light).
The accent triple is `#3584E4` exactly: `0.207843 × 255 = 53 = 0x35`,
`0.517647 × 255 = 132 = 0x84`, `0.894118 × 255 = 228 = 0xE4`. That is the same
`#3584e4` as libadwaita's `--accent-blue`.

**Measured.** The library proves the source: `libadwaita-1.so.0` contains
`org.freedesktop.portal.Desktop`, `org.freedesktop.portal.Settings`,
`org.gnome.desktop.interface`, `accent-color-changed`, `color-scheme-changed`,
`gtk-interface-color-scheme`, and `Invalid color scheme %s (Expected one of:
default, prefer-dark, prefer-light)`. The strings say the library consults both
the portal and GSettings; the probe (§3.2.1) confirms the portal value, and the
fallback order is reasoned from the library's error string and its two source
names rather than observed. dtr can follow the same order without the library.

**Measured failure modes.** With no session bus
(`DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent`), `busctl` and `gdbus` both
exit 1 (`Failed to connect to user scope bus`). An unknown key exits 1 with
`Call failed: Requested setting not found`. In this sandbox `gsettings` prints
`dconf-CRITICAL … unable to create file '/run/user/1000/dconf/user': Read-only
file system` but exits 0 and prints the value; stderr must be ignored when
stdout is parsed. The read-only `/run/user/1000` is a sandbox artifact, not a
NixOS behaviour.

### 4.2 The options, and their cost

| Source | What it gives | Cost (measured) | Failure |
|---|---|---|---|
| `zbus` (`tokio`) session + `Proxy::call` | scheme `Value::U32`, accent `Value::Structure([F64; 3])` | ~2.0 ms for both (20 runs, 41 ms), one connection | no bus, no portal → `Err` |
| `busctl --json=short --user call … Read` | the same values as typed JSON (`{"type":"v","data":[{"type":"v","data":{"type":"(ddd)","data":[0.2078…]}}]}`) | ~2.6 ms for both (20 pairs, 52 ms) | no bus, no portal, unknown key → exit 1 |
| `gdbus call --session …` | the same, as `(<<uint32 1>>,)` / `(<<(ddd)>>,)` | ~2.4 ms per call (20 calls, 48 ms) | same |
| `gsettings get` | scheme string, accent *name*, theme name; schema defaults applied | ~2.9 ms for both (20 pairs, 58 ms) | no schemas, no bus; dconf warnings on stderr |
| `dconf read` | stored keys only | ~2.0 ms for both (20 pairs, 39 ms) | effective-default problem above |
| `~/.config/gtk-4.0/settings.ini` | theme name, font, icon theme | ~0.75 ms per read (20 reads, 15 ms) | no colours |
| parse `~/.config/dconf/user` ourselves | stored keys | our gvdb parser | same default problem, plus a binary format to maintain |

`~/.config/dconf/user` is a `GVariant Database file, version 0` (measured with
`file`). The gvdb format is documented, but parsing it buys one fewer subprocess
and loses the schema defaults. Rejected.

The costs are per read (one connection for `zbus`, one process per key for
`busctl`, one process for `gsettings`). The first read is the only one a frame
waits for — after that the answer sits in a cell (§6) — and 2–2.6 ms once is why
that is acceptable rather than a per-frame cost: `Look::skin()` is asked by every
surface on every frame, and 2–2.6 ms × 60 is 120–156 ms/s, the same reason the
Windows registry read is cached (`crates/dtr/src/system.rs`).

**Read order, as shipped.** `zbus` first (cross-desktop, resolved values, no GLib,
typed). Then `settings.ini` and `GTK_THEME` for the theme *name*, which is the
gate and, when the portal has no preference, the scheme. Then `None`, which
`Skin::resolve` turns into the dtr skin. **`gsettings` is not used**: it was the
third source for the theme name, and it costs a subprocess for a value the two
files above already hold on every desktop that sets one (§4.2, §5).

## 5. Precedence and fallbacks

| Signal | Precedence | Use | On failure |
|---|---|---|---|
| `GTK_THEME` | first, for the *theme name* | `Adwaita`, `Adwaita:dark`, `adw-gtk3` are Adwaita-like; `:dark`/`:light` fixes the scheme | non-Adwaita name → `None` → dtr |
| portal `color-scheme` / `accent-color` | first for the *values* | 1/2 → dark/light; 0 → theme name; accent → RGB | `gsettings` |
| `gsettings` `color-scheme` | second | `prefer-dark` / `prefer-light`; `default` → theme name | `settings.ini` |
| `gsettings` `accent-color` | second | name → the nine-value table | `#3584E4` |
| `gsettings` `gtk-theme` | gate | Adwaita / adw-gtk3 → Adwaita table; anything else → dtr | `settings.ini` |
| `settings.ini` `gtk-theme-name` | gate, last | same | hard-coded `Adwaita` |
| user `gtk.css` / `gtk-dark.css` | **not read in v1** | — | — |
| `@import` | v2 only | `file://` or relative, one level; `resource://` / missing → ignore, log | — |
| ambiguous scheme (`default` + no `-dark` / `-light` name) | — | `None` | dtr |
| headless: no portal, no `gsettings` | — | `None` | dtr |

Why these choices:

- **A non-Adwaita theme is not "Adwaita with a warning".** If `gtk-theme` says
  `Breeze` or `Materia`, painting Adwaita's `#222226` surfaces on that desktop
  is a confident lie; the dtr skin is at least coherent, and `-v` can say why.
  The failure prevented: `native` matching neither the desktop nor dtr.
- **An ambiguous scheme is not a coin flip.** `color-scheme=default` plus a
  theme name with no dark/light suffix does not tell us what GTK will render.
  Picking light would be wrong half the time; `None` is the honest answer. The
  failure prevented: a white chrome on a dark desktop (or the reverse) with no
  cause in the log.
- **A missing import is not a failed read.** In v2, a `gtk.css` that imports a
  store path that has been garbage-collected — exactly what this machine has —
  should lose the override, not the whole skin. The failure prevented: a stale
  path in a dotfile disabling `native` entirely.
- **`GTK_THEME` is read, but it cannot supply a palette.** It is a name; a
  non-Adwaita name has no colour we can read in v1. The failure prevented:
  treating a debug knob as a palette source.
- **The settings daemon is not asked.** `gsettings` would be a third source for
  the theme name, and reading it means running a subprocess: dconf exposes no read
  API on the bus, by design, so this is not something zbus can do instead. A
  terminal that forks a program to find out what colour its own chrome is has
  spent more than the answer is worth. The failure prevented: a 30 ms subprocess
  on the startup path, and a `gsettings` that waits on a dconf lock. What is lost:
  a KDE user whose `gtk-theme-name` lives only in dconf keeps the dtr skin —
  which is the same answer Plasma's own `settings.ini` would have given.
- **The user's `gtk.css` is not read in v1.** It is an override, not the system
  palette; the one on this machine is a dangling import. §8 specifies what a
  v2 would read. The failure prevented: a half-parsed override producing a
  palette that is neither GTK's nor dtr's.

## 6. What `native` became

**Implemented.** `dtr-widgets` gained one palette, one style and one origin tag;
the binary gained a reader, a trait, and a watcher.

```rust
// dtr-widgets
impl Palette { pub fn adwaita(dark: bool, accent: Color32) -> Self; }
impl Style   { pub fn adwaita() -> Self; }
impl Skin    { pub fn adwaita(dark: bool, accent: Color32) -> Self; }

// dtr/src/system.rs — one shape for both platforms
pub trait Platform {
    fn source(&self) -> &'static str;
    async fn read(&self) -> Result<Reading, String>;
    fn skin(&self, reading: &Reading) -> Skin;
}
```

`Reading` is the three facts a platform can state — dark or light, the accent, and
the accent ladder where the OS keeps one (Windows does; Adwaita computes it). A
platform is a reader plus a palette, and `resolve` turns `Reading` into the
`Resolution` that `-v` prints: `{ skin, source, reason }`. `source` is the
reader's own name (`windows-registry`, `portal`, `preview`, `none`) and `reason`
is why there is no skin, so "the theme is Breeze" and "there is no session bus"
cannot look alike in the log.

Three things changed shape from the sketch this section used to carry:

- **`describe` reads an origin, not a style.** `Skin` carries
  `Origin::{Dtr, Windows, Adwaita}` set by its constructor. The old guess —
  `style.mono` means dtr, else the palette's lightness — calls an Adwaita skin
  "dtr" (it borrows neither that style's fonts nor its metrics now) and would
  call any non-monospace light skin "windows".
- **The reading is async, and the first one is the only blocking one.** The
  portal call, `gsettings` and the timeout are all `async`; `Platform::read` is an
  `async fn` and Linux's `gsettings` runs on `spawn_blocking` rather than pulling
  tokio's `process` feature in for one call. The *first* reading happens inside a
  current-thread runtime from a caller that cannot wait for an answer it is about
  to draw (`-v` is about to print it), which is the smallest way to have one
  implementation serve both.
- **The answer lives in a cell, not in a `OnceLock`.** A frame asks
  `Look::skin()` for it with no access to the app, so `system` keeps the
  `Resolution` behind an `RwLock` and hands out a clone; the watcher replaces it.
  In a test that cell is filled with "a test does not read the desktop" rather
  than with the real desktop, because a baseline that changes with the
  developer's accent is a baseline CI cannot compare.

**`-v` now says where the answer came from**, which is the whole reason the pair
of fields exists:

```
chrome skin="adwaita-dark" requested="native" accent="#3584E4" hover="#4990E7" source="portal"
chrome skin="dtr" requested="native" accent="#FFB000" source="none" reason="theme \"Materia\" is not Adwaita"
chrome skin="adwaita-light" requested="native" accent="#3584E4" hover="#357CD3" source="preview"
```

The first is this machine's real session, with `DTR_NATIVE_SKIN` unset.

### 6.1 The metrics are Adwaita's too

The earlier version of this section proposed `Style::dtr()` under an Adwaita
palette. That is a themed terminal, not a window on the desktop: dtr's 23-point
rows and monospace labels are the *terminal's* chrome, and libadwaita's are not.
`Style::adwaita()` is measured from the same stylesheet §3.2 extracted:

| Token | Value | Where |
|---|---|---|
| `row` | 34 | `spinbutton, entry { min-height: 34px }` |
| `control_radius` | 9 | `border-radius: 9px` on the same rule |
| `card_radius` | 12 | `.card { border-radius: 12px }` |
| `check_radius` | 6 | `check { border-radius: 6px }` |
| `shadow` | `0 1px 3px 1px RGB(0 0 6/7%)` | `.card`'s second shadow layer |
| `footer` | primary last | a GNOME dialog's suggested action is the right-hand one |

`check_radius` is a new token, not a reuse of `chip_radius`: Adwaita rounds a
20-point box by 6 and a `radio` by 100 %, and a checkbox drawn with the chip
radius (9 here) is a *circle*, which is a radio button. The gallery caught it —
`widgets_adwaita_dark.png` is the baseline that says so.

The font sizes are the one part not measured: GNOME names a font (`Cantarell 11`)
and leaves the size to fontconfig, and egui's fallback chain finds the same face
on the same desktop.

### 6.2 It is live

A desktop changes its scheme at sunset and its accent when the user picks one. The
watcher is one task on the worker runtime (`App::worker`, which already exists for
scanning):

- **Linux**: `org.freedesktop.portal.Settings.SettingChanged`, subscribed once per
  reading. The portal re-emits from `gsettings`, so this covers the settings app,
  `gsettings` in a terminal, and a theme switcher. A session with no portal has no
  signal to subscribe to, and the scheme then comes from the theme's name and
  stays there.
- **Windows**: `WM_SETTINGCHANGE`, from the window procedure
  `crates/dtr/src/chrome/windows.rs` already subclasses for the maximise button's
  hit test. winit forwards the *theme* half as `ThemeChanged` and the accent half
  not at all, so re-reading both registry values on the one message costs less
  than deciding which changed. The procedure cannot await: it calls
  `system::changed()`, which `notify_one`s the watcher (a permit keeps a change
  that arrives between two readings).
- The watcher re-reads, stores, logs `chrome changed`, and asks for a repaint
  through `UserEvent::Frame` — the same door every background result uses, because
  a skin that changed under a frame that never draws again did not change.

A `DTR_NATIVE_SKIN` preview is not overruled by the desktop: the watcher keeps
running, and it writes the preview back, so the look a developer asked for is the
look they keep.

**What is not verified here.** The signal itself. This sandbox mounts
`/run/user/1000/dconf/user` read-only, so `gsettings set` cannot change the
session and nothing emits `SettingChanged` — the subscription is proved (a `-v`
line), the read is proved (the `chrome` line), and the gap between them is one
line of `receive_signal`. Changing the setting on a desktop where it is writable
is the check that closes it.

### 6.3 The dependency, as taken

`zbus = { version = "5", default-features = false, features = ["tokio"] }`, under
`[target.'cfg(target_os = "linux")'.dependencies]`, so the Windows build does not
see it. The `tokio` transport reuses the runtime dtr already runs; `async-io`
(zbus's default) would add a second reactor beside it. The probe that resolved the
same two keys locks 78 packages in total, `zbus` and `zvariant` included; what the
transport costs *this* tree is **23** packages, because dtr already has tokio.

`blocking-api` is **not** enabled: the first reading is an `async fn` awaited
under `Runtime::block_on`, and the clock is the async `tokio::time::timeout` over
the whole reading. `blocking-api` existed for the probe.

The dependency is behind a cargo feature so the tree is a choice:

```toml
[features]
portal = ["dep:zbus"]
default = ["portal"]
```

Off, `Portal` is a name-only reader: `source="gtk-config"`, the scheme from
`GTK_THEME`/`settings.ini`, Adwaita's blue accent, and a watcher parked on
`std::future::pending()` because a file has no notification. `cargo check -p dtr
--no-default-features` is the build that proves it, and this machine's is
`skin="adwaita-dark" source="gtk-config"` — its `settings.ini` says
`gtk-theme-name=Adwaita-dark`.

### 6.4 Tests

`Palette::adwaita` has the same shape of tests as `Palette::windows`: the
measured surfaces are pinned (`#222226`, `#36363a`, `#1d1d20`), the derived slots
are checked against `mix(card, currentColor, …)`, `is_dark` follows the card, and
`on_accent` is white for the yellow and orange accents too, because libadwaita
injects it that way. `Style::adwaita` pins the four measured metrics. The pure
pieces of the reader — the theme gate, the scheme a name states, `settings.ini`,
the accent-name table, and the portal's `(ddd)` → `#RRGGBB` rounding — have unit
tests, and both skins are in the widget gallery
(`widgets_adwaita_dark.png`, `widgets_adwaita_light.png`), rendered on Linux the
way the Windows skins already were. The serial sheet is snapshotted in the dtr
skin, so the Adwaita chrome on the *app's* own screens is what a person sees when
they run it on GNOME, not a baseline.

## 7. The mapping, slot by slot

Direct reads are **measured** from the extracted gresource. Derived slots use
Adwaita's own measured rules; guessed slots are slots Adwaita does not have and
dtr needs.

| Slot | Source | Kind |
|---|---|---|
| `window` | `window_bg_color` | measured |
| `well` | `view_bg_color` | measured |
| `card` | `dialog_bg_color` (fallback `popover_bg_color`) | measured |
| `text` | `window_fg_color`, composited over `window` (`over()` already exists in `palette.rs`) | measured + compositing |
| `accent` | portal `accent-color`, else the nine-name table, else `#3584E4` | measured |
| `on_accent` | `accent_fg_color` = white in 1.9.3 | measured |
| `danger` | `destructive_bg_color` (`@red_3` / `@red_4`) | measured |
| `on_danger` | `destructive_fg_color` = white | measured |
| `control` | `color-mix(currentColor 10%, transparent)` over `card` | derived (rule measured) |
| `control_hover` | 15% currentColor over `card` | derived |
| `control_active` | 30% currentColor over `card` | derived |
| `border` | `--border-opacity: 15%` currentColor over `card` | derived (opacity measured) |
| `divider` | `borders` at 15% currentColor, dtr's 46 alpha | derived |
| `dim` | `--dim-opacity: 55%` currentColor over `card` | derived (55% measured) |
| `disabled` | `--disabled-opacity: 50%` currentColor over `card` | derived (50% measured) |
| `selection` | `color-mix(accent 25%, transparent)` over `view` | derived (25–30% measured) |
| `on_selection` | `view_fg_color` | derived |
| `focus_outer` | `color-mix(accent 50%, transparent)` (35 occurrences) | derived (rule measured) |
| `focus_inner` | `card` | derived |
| `border_strong` | 25% currentColor over `card` | **guess** |
| `control_strong` | 40% currentColor over `card` | **guess** |
| `accent_hover` | dtr's `mix(accent, text, 0.10)` | **guess** |
| `accent_active` | dtr's `mix(accent, text, 0.06)` | **guess** |
| `scrim` | dtr's black 140 alpha (Adwaita's `shade_color` is a 7–25% shade, not a scrim) | **guess** |

Worked examples (dark: `window = #222226`, `card = #36363a`, `text = white`):

- `control` = 10% white over `#36363a` = `#4A4A4E`;
- `border` = 15% white over `#36363a` if drawn on the card = `#545458`; over
  the control = `#656569`;
- `dim` = 55% white over `#36363a` = `#A5A5A6`.

The exact numbers belong in the constructor's tests, not this memo. The point is
that every slot has a stated provenance, and the five guesses are named as
guesses.

Two consequences worth saying out loud:

- **`on_accent` is white even for yellow and orange accents**, because that is
  what libadwaita 1.9.3 injects. Matching the desktop means matching this, not
  improving on it; the contrast complaint belongs to the library.
- **`is_dark()` follows `card`**, and `card` is measured, so the egui theme, the
  glyph tints and the popup style all flip together. A palette that says it is
  dark while its card is light is the bug that would follow from deriving
  `is_dark` from `window` or from the terminal theme instead.

## 8. Parsing, if and when we need it

**v1: no parser.** The values are either fixed (surfaces), injected (accent), or
read from the portal (scheme/accent). There is no cascade to evaluate, and a
parser would be a second, less accurate implementation of `Palette::adwaita`.
The failure prevented: a 250-line CSS subset in the shipped binary that is used
on exactly no machine, and that a user can mistake for "dtr reads my theme".

**v2: only the user override, with this subset.** `gtk.css` is the one text
layer that can legitimately differ from Adwaita, and the layer a person edits
when they want the accent magenta. The minimum honest subset is:

| Construct | Support |
|---|---|
| `@define-color <name> <value>;` | yes |
| `:root { --<name>: <value>; }` | yes, exact `:root` only |
| `@import "p"` / `@import url("p")` | yes for `file://` and relative, one level; `resource://` and gresource refused |
| `@media (prefers-color-scheme: dark or light)` | evaluated against the decided scheme before parsing; a `prefers-contrast` block is skipped |
| `#rgb`, `#rrggbb`, `#rrggbbaa` | yes |
| `rgb()`, `rgba()` (comma and space forms, `/ alpha`, `%`) | yes |
| `hsl()`, `hsla()` | yes |
| named colours (`white`, `black`, `transparent`, the CSS keywords) | yes |
| `@name`, `var(--name)` | yes, resolved in declaration order |
| `currentColor` | only when the caller supplies a base colour; otherwise drop the declaration |
| `oklab()`, `lab()`, `color-mix()`, relative colour (`from`), gradients, `calc()`, `filter`, `-gtk-*` | **no** — drop the declaration, keep the Adwaita value, log the name |

Dropping a declaration is the important rule: the slot keeps the Adwaita value
rather than becoming a guess. A user who writes
`@define-color accent_bg_color #ff00ff;` gets magenta; a user who writes
`@define-color accent_bg_color oklab(from …)` gets Adwaita's accent and a log
line, not a silently different colour.

**Where it lives.** `crates/dtr/src/system/linux/css.rs` (the binary), alongside
the reader, not in `dtr-widgets`. The crate must stay free of I/O and platform
code; the Windows precedent is the registry read in `system.rs` and the palette
constructor in `dtr-widgets`. `dtr-widgets` gains no parser, no file access and
no new dependency.

**Third-party or hand-rolled.**

| Option | Licence | What it buys | Cost |
|---|---|---|---|
| `cssparser` | MPL-2.0 (allowed by `deny.toml`) | a tokenizer | `@define-color` needs a pre-pass; we still write the declaration grammar and the resolver |
| `lightningcss` | MPL-2.0 (allowed) | a full parser/printer with `@media`, `var()`, `color-mix()` | a large tree in the shipped exe; still needs the GTK at-rule pre-pass and our cascade decisions |
| `simplecss` | Apache-2.0 OR MIT | CSS 2 declarations and selectors | its own docs say at-rules are **not supported**, so it cannot see `@define-color`, `@import` or `@media`; the pre-pass is most of the work |
| hand-rolled | none | exactly the subset above | ~250 lines and a test table; no dependency, no licence review, no `Cargo.lock` churn |

**Recommendation: hand-roll, and only in v2.** A CSS crate would parse the 431
KB sheet's syntax but not its meaning: 493 `color-mix`, 34 `oklab`, 293
`currentColor` and the injected accent are outside any general parser's job. The
dependency would buy tokenization and cost a shipped-tree review; the subset
above is small enough that the trade is not close. MPL-2.0 is allowed if that
changes, so this is a cost decision, not a licence one.

## 9. What cannot be known without GTK

**Measured limitation.** No *application* was instrumented: `strace` was
attempted and the sandbox denied it (`PTRACE_TRACEME: Operation not
permitted`), and `gtk4-demo` / `gtk4-widget-factory` are not installed. But a
throwaway GTK4/libadwaita probe was built and run (§3.2.1), and it resolves the
theme's `@define-color` values for the current display and scheme; that is how
the direct table was checked. `gnome-text-editor` is a libadwaita app (the
loader resolves `libadwaita-1.9.3` for it), and the extracted gresource sheet is
byte-identical in the two 1.9.3 outputs on this machine (`sha256 be8ce333…`).
What the probe does not show is another application's own provider cascade, or a
context whose display does not exist.

What remains unknown:

- **the cascade.** GTK applies theme, settings, application and user styles in
  priority order; libadwaita's gresource is one provider among them, and a user
  `gtk.css` wins over the theme. The probe is a bare `GtkLabel` in a detached
  `GtkWindow`, so it sees the display's providers but no application provider;
  another app's window can differ.
- **`var()` chains.** `--accent-color: oklab(from var(--accent-bg-color)
  var(--standalone-color-oklab))`; `--border-color: color-mix(in srgb,
  currentColor var(--border-opacity), transparent)`; `currentColor` is the
  widget's inherited colour, not a constant. GTK resolves these; a static
  reader would be implementing a colour engine, which is the thing the
  constraint says not to do.
- **runtime recolouring.** `AdwStyleManager` injects the accent and scheme; a
  per-app style manager can override them (`ADW_DISABLE_PORTAL`,
  `gtk-application-prefer-dark-theme`, a debug provider). The portal value is
  what the library reads, not necessarily what a particular app displays.
- **high contrast.** `@media (prefers-contrast: more)` changes the border, dim
  and disabled opacities (50/90/40%), and the portal exposes `contrast`. dtr has
  no high-contrast style; v1 ignores it. The failure prevented: a `native` skin
  that claims accessibility support it does not have.
- **the five guessed slots** in §7. They are guesses because Adwaita has no
  distinct `border_strong`, `control_strong`, `accent_hover`, `accent_active`
  or `scrim` token. Are they better than the dtr skin? As *literals*, no. As
  *formulas on the measured base*, yes: deriving them with dtr's rule from
  Adwaita's colours keeps the palette coherent, where copying a dtr literal
  would put a dtr grey in an Adwaita card. That is why §7 marks them, and why
  `Palette::adwaita`'s tests assert the same relationships the dtr palette's
  tests do (stronger than the hairline, opaque after compositing).

### 9.1 If GLib were allowed

The portal does not need GLib: `zbus` is the pure-Rust transport and the
recommended one. This section is about the *other* GLib route — linking GTK and
libadwaita, which is what the probe does. It can do everything the `zbus` route
does and more:

| What | `zbus` + constants | Linked GTK4/libadwaita |
|---|---|---|
| scheme and accent | portal, else `gsettings` | `AdwStyleManager` / `GtkSettings` |
| Adwaita surfaces | vendored constants | `GtkCssProvider` + `GtkStyleContext` |
| user `@define-color`, `var()`, `oklab()`, `color-mix()` | v2 hand parser, dropping what it cannot resolve | resolved by GTK |
| a non-Adwaita theme | dtr in v1 | resolved by GTK |
| dependencies | `zbus` (MIT) and its tree | gtk4-rs / glib-sys / gio, `pkg-config`, the GTK dev closure, libgtk and libadwaita at runtime |
| build | one Rust binary, no C toolchain | C toolchain and dev closure; dynamic libraries in the Linux package |
| display | none needed | `gtk_init` and a display (the probe used `wayland-1`) |
| API | typed `OwnedValue` from a stable D-Bus interface | `gtk_style_context_lookup_color`, deprecated since GTK 4.10 |
| answer on this machine | `#222226`, `#3584E4`, `#FFFFFF`, … | the same values (measured) |

For the slots dtr needs, the linked route is **not easier**: it produced exactly
the `zbus` route's values on this machine, and it brings the dependency the
maintainer rejects. It is the only route that resolves an arbitrary user
stylesheet without a parser, which is why §10 keeps it on the table.

If the constraint is ever relaxed, the shape should be explicit rather than a
silent dependency: either a `cfg(target_os = "linux")` module behind a Cargo
feature that is **off** by default, or a separate helper process that links GTK
and prints the resolved palette as JSON — dtr spawns it instead of `zbus`. The
helper keeps GTK out of the dtr binary and its one-file story, but it is still
a second binary to ship, still needs a display, and still violates the letter
of "no GTK". Neither should happen without the maintainer saying so.

The probe itself is a development check, not a build input. If it is kept, it
belongs in `scripts/` behind a manual command that regenerates the constants
and diffs them; it must not be a `nix flake check` gate, because CI has no
display.

## 10. Not decided

- **The v2 user-override parser.** The subset, the drop-don't-guess rule and the
  location are above; whether it is worth the ~250 lines is a separate call.
  Today's machine would gain nothing (the file is a dangling import), but a
  machine with a real `@define-color accent_bg_color` override would.
- **GTK Default's palette.** It is a second, different set of constants
  (`#353535` dark). v1 treats `Default` as non-Adwaita and falls back to dtr.
  Adding `Palette::gtk_default` would make bare GTK4 apps match too; the apps
  checked here link libadwaita, and no bare GTK4 app was found to compare, so
  it is not worth the table yet.
- **Adwaita metrics beyond the four measured ones.** `Style::adwaita()` takes
  the 34-point row, the 9-point control radius, the 12-point card and the 6-point
  checkbox from the stylesheet (§6.1). What is still guessed: the label column
  width, the gap, and the font sizes, which GNOME leaves to fontconfig. A window
  radius of 15 is measurable and not used — dtr draws no window corner, the
  compositor does.
- **KDE/Breeze.** `adw-gtk3` proves a readable theme can be parsed; a Breeze
  GTK4 theme would be a second dialect (its `@define-color` names are
  `*_breeze`), and v1 says dtr. The `gtk-theme` gate is where that decision
  would go.
- **`GTK_THEME` precedence.** v1 lets `Adwaita:dark` fix the scheme even when
  the portal says otherwise. That is what a process-local override means, but it
  is a choice, not a measurement.
- **High contrast**, per §9.
- **Version pinning.** The constants are libadwaita 1.9.3's. A library upgrade
  can change a surface without changing the portal API; the constants need the
  same "regenerate and diff" discipline as a snapshot, and a comment naming the
  version and the gresource hash.
- **Whether the GTK probe becomes a `scripts/` check.** It is the only way to
  see a libadwaita upgrade change a surface, and it needs a display, so it would
  be a manual command — never a `nix flake check` gate.
- **The `busctl` fallback.** `zbus` shipped (§6.3), so the zero-dependency
  variant in §4.2 is now the escape hatch rather than the plan: it is the same
  two values behind the same `Platform` reader, for a target that cannot take the
  tree. Nothing implements it today.
- **A linked GTK helper, if the constraint is ever relaxed.** §9.1 gives the
  shape and the costs; it is not a recommendation, and it should not happen
  without an explicit reversal.

## Appendix — commands that produced the measurements

Exact paths are this machine's; the hashes identify the outputs.

```sh
# 1. What the machine is
cat /etc/os-release; echo "$XDG_CURRENT_DESKTOP $XDG_SESSION_TYPE $GTK_THEME"
ls -la ~/.config/gtk-4.0 ~/.config/dconf
cat ~/.config/gtk-4.0/gtk.css ~/.config/gtk-4.0/settings.ini

# 2. The libraries and their gresources
GR=/nix/store/jipb8y1xzhxjga5iccw8jmas0bc0px47-glib-2.88.3-dev/bin/gresource
L=/nix/store/3dkd54d9711xghg7lq799ii9vwdmwwqa-libadwaita-1.9.3/lib/libadwaita-1.so.0
G=/nix/store/0bxxqghpd81agcp6ncz5d0rn3y4axk56-gtk4-4.22.4/lib/libgtk-4.so.1.2200.4
sha256sum "$L" "$G"          # 70fc26ec… and 67a694eb…
"$GR" list "$L" | grep styles
"$GR" extract "$L" /org/gnome/Adwaita/styles/gtk.css | wc -c    # 431418
"$GR" list "$G" | grep /theme/
"$GR" extract "$G" /org/gtk/libgtk/theme/Default/gtk.css | wc -c # 599

# 3. The counts in the sheets (on the extracted copies)
for t in '@define-color' ':root' '--accent-bg-color' 'color-mix(' 'oklab(' 'lab(' 'currentColor' 'linear-gradient' 'RGB(' 'rgb(' 'rgba(' 'hsl(' '@import'; do
  printf '%-20s %s\n' "$t" "$(grep -o -- "$t" /tmp/adw-res/gtk.css | wc -l)"
done

# 4. The accent is injected, not in the file
strings -a "$L" | grep 'accent_bg_color'
strings -a "$L" | grep -o '#[0-9a-f]\{6\}' | sort -u | grep -E '3584e4|2190a4|3a944a|c88800|ed5b00|e62d42|d56199|9141ac|6f8396'

# 5. Settings and portal
gsettings get org.gnome.desktop.interface color-scheme
gsettings get org.gnome.desktop.interface accent-color
gsettings get org.gnome.desktop.interface gtk-theme
GSETTINGS_BACKEND=memory gsettings get org.gnome.desktop.interface accent-color
dconf read /org/gnome/desktop/interface/accent-color
busctl --user call org.freedesktop.portal.Desktop /org/freedesktop/portal/desktop \
  org.freedesktop.portal.Settings Read ss org.freedesktop.appearance color-scheme
busctl --user call … Read ss org.freedesktop.appearance accent-color

# 6. Cost (20 invocations, wall clock)
time (for i in $(seq 20); do gsettings get … color-scheme >/dev/null 2>&1; gsettings get … accent-color >/dev/null 2>&1; done) # 0.058 s
time (for i in $(seq 20); do busctl … color-scheme >/dev/null 2>&1; busctl … accent-color >/dev/null 2>&1; done) # 0.052 s

# 7. The dangling import
readlink -f ~/.config/gtk-4.0/gtk.css
ls …/adw-gtk3-6.5/share/themes/            # adw-gtk3 adw-gtk3-dark
find /nix/store -type d -path '*/share/themes/Adwaita-dark/gtk-4.0'  # nothing

# 8. The tree is GTK-free
grep -niE '^name = "(glib|gtk|gio|gdk|pango|gobject)' Cargo.lock   # nothing

# 9. What could not be traced
strace -f -e openat gnome-text-editor --version   # PTRACE_TRACEME: Operation not permitted
LD_DEBUG=libs gnome-text-editor --version 2>&1 | grep libadwaita  # resolves 1.9.3

# 10. The GTK probe (development only; /tmp, not in the repo)
# 10a. Build: pkg-config from the store plus the dev closures
PKG=/nix/store/0sw8z1clv6vbd812ysnfv0jlcmn668pl-pkg-config-0.29.2/bin/pkg-config
DEVS=$(ls -d /nix/store/*-dev/lib/pkgconfig | tr '\n' ':')
PKG_CONFIG_PATH="$DEVS:/nix/store/996ivv9zi0qk8allhw8hb2my54dw8hya-libadwaita-1.9.3-dev/lib/pkgconfig" \
  "$PKG" --cflags --libs gtk4 libadwaita-1
clang $CFLAGS -o /tmp/gtkprobe /tmp/gtkprobe.c $LIBS

# 10b. Run against this session's Wayland display
LL=$("$PKG" --libs-only-L gtk4 libadwaita-1 | sed 's/-L//g' | tr ' ' ':')
LD_LIBRARY_PATH="$LL" XDG_RUNTIME_DIR=/run/user/1000 /tmp/gtkprobe
ADW_DEBUG_COLOR_SCHEME=prefer-light LD_LIBRARY_PATH="$LL" \
  XDG_RUNTIME_DIR=/run/user/1000 /tmp/gtkprobe
```

The probe is a throwaway 60-line C program; it is not committed and not a build
input. Its source:

```c
/* /tmp/gtkprobe.c — a measurement, not part of dtr. */
#include <gtk/gtk.h>
#include <adwaita.h>

static void dump(GtkStyleContext *ctx, const char *name)
{
    GdkRGBA c;
    if (gtk_style_context_lookup_color(ctx, name, &c))
        g_print("%-24s rgba(%3d,%3d,%3d,%.3f)  #%02X%02X%02X\n", name,
                (int)(c.red * 255 + 0.5), (int)(c.green * 255 + 0.5),
                (int)(c.blue * 255 + 0.5), c.alpha,
                (int)(c.red * 255 + 0.5), (int)(c.green * 255 + 0.5),
                (int)(c.blue * 255 + 0.5));
    else
        g_print("%-24s MISSING\n", name);
}

int main(void)
{
    adw_init();
    GdkDisplay *display = gdk_display_get_default();
    g_print("display=%s\n", display ? gdk_display_get_name(display) : "(none)");

    AdwStyleManager *sm = adw_style_manager_get_default();
    g_print("dark=%d high_contrast=%d supports_accent=%d accent_enum=%d\n",
            adw_style_manager_get_dark(sm),
            adw_style_manager_get_high_contrast(sm),
            adw_style_manager_get_system_supports_accent_colors(sm),
            (int)adw_style_manager_get_accent_color(sm));
    GdkRGBA *a = adw_style_manager_get_accent_color_rgba(sm);
    if (a)
        g_print("accent_rgba=#%02X%02X%02X\n",
                (int)(a->red * 255 + 0.5), (int)(a->green * 255 + 0.5),
                (int)(a->blue * 255 + 0.5));

    GtkWidget *window = gtk_window_new();
    GtkWidget *label = gtk_label_new("probe");
    gtk_window_set_child(GTK_WINDOW(window), label);
    GtkStyleContext *ctx = gtk_widget_get_style_context(label);
    dump(ctx, "window_bg_color");
    dump(ctx, "window_fg_color");
    dump(ctx, "view_bg_color");
    dump(ctx, "view_fg_color");
    dump(ctx, "card_bg_color");
    dump(ctx, "dialog_bg_color");
    dump(ctx, "popover_bg_color");
    dump(ctx, "headerbar_bg_color");
    dump(ctx, "accent_bg_color");
    dump(ctx, "accent_fg_color");
    dump(ctx, "destructive_bg_color");
    dump(ctx, "shade_color");
    gtk_window_destroy(GTK_WINDOW(window));
    return 0;
}
```

What it printed (session dark; then forced light):

```
$ /tmp/gtkprobe
display=wayland-1
dark=1 high_contrast=0 supports_accent=1 accent_enum=0
accent_rgba=#3584E4
window_bg_color          rgba( 34, 34, 38,1.000)  #222226
window_fg_color          rgba(255,255,255,1.000)  #FFFFFF
view_bg_color            rgba( 29, 29, 32,1.000)  #1D1D20
view_fg_color            rgba(255,255,255,1.000)  #FFFFFF
card_bg_color            rgba(255,255,255,0.080)  #FFFFFF
dialog_bg_color          rgba( 54, 54, 58,1.000)  #36363A
popover_bg_color         rgba( 54, 54, 58,1.000)  #36363A
headerbar_bg_color       rgba( 46, 46, 50,1.000)  #2E2E32
accent_bg_color          rgba( 53,132,228,1.000)  #3584E4
accent_fg_color          rgba(255,255,255,1.000)  #FFFFFF
destructive_bg_color     rgba(192, 28, 40,1.000)  #C01C28
shade_color              rgba(  0,  0,  6,0.250)  #000006

$ ADW_DEBUG_COLOR_SCHEME=prefer-light /tmp/gtkprobe
dark=0 high_contrast=0 supports_accent=1 accent_enum=0
accent_rgba=#3584E4
window_bg_color          rgba(250,250,251,1.000)  #FAFAFB
window_fg_color          rgba(  0,  0,  6,0.800)  #000006
view_bg_color            rgba(255,255,255,1.000)  #FFFFFF
view_fg_color            rgba(  0,  0,  6,0.800)  #000006
card_bg_color            rgba(255,255,255,1.000)  #FFFFFF
dialog_bg_color          rgba(250,250,251,1.000)  #FAFAFB
popover_bg_color         rgba(255,255,255,1.000)  #FFFFFF
headerbar_bg_color       rgba(255,255,255,1.000)  #FFFFFF
accent_bg_color          rgba( 53,132,228,1.000)  #3584E4
accent_fg_color          rgba(255,255,255,1.000)  #FFFFFF
destructive_bg_color     rgba(224, 27, 36,1.000)  #E01B24
shade_color              rgba(  0,  0,  6,0.070)  #000006
```

The `zbus` route is the shipping transport, and it is the one that was measured
against the same portal:

```sh
$ cd /tmp/zbustokio && cargo run
# The whole probe locks 78 packages; what it adds to dtr, which already has
# tokio, is 23.
$ grep -c '^name = ' /tmp/zbustokio/Cargo.lock
78
$ git -C ~/src/dtr diff Cargo.lock | grep -c '^+name = '
23
color-scheme = OwnedValue(Value(U32(1)))
accent-color = OwnedValue(Value(Structure(Structure { fields: [F64(0.2078431397676468), F64(0.5176470875740051), F64(0.8941176533699036)], signature: Structure(Dynamic { fields: [F64, F64, F64] }) })))
$ time (for i in $(seq 20); do ./target/debug/zbusprobe >/dev/null 2>&1; done)
real    0m0.041s
```

Its `Cargo.toml` and the call are small enough to show:

```toml
[package]
name = "zbusprobe"
version = "0.1.0"
edition = "2021"

[dependencies]
zbus = { version = "5.19.0", default-features = false, features = ["blocking-api", "async-io"] }
```

```rust
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::session()?;
    let proxy = Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
    )?;
    let scheme: OwnedValue =
        proxy.call("Read", &("org.freedesktop.appearance", "color-scheme"))?;
    let accent: OwnedValue =
        proxy.call("Read", &("org.freedesktop.appearance", "accent-color"))?;
    println!("{scheme:?}\n{accent:?}");
    Ok(())
}
```

The `20 runs, 41 ms` figure in §4.2 is this probe, opening a new connection each
time; dtr opens one connection and caches the result, so the per-process cost is
one connection plus two calls.
