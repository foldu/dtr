//! The platform's own chrome, when this machine has one.
//!
//! `native` means "look like a window on this machine", and only Windows tells
//! us how: light or dark from `AppsUseLightTheme` under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`, and the
//! user's accent from DWM under `HKCU\Software\Microsoft\Windows\DWM` — note the
//! *different* path; `AccentColor` is not under `CurrentVersion`, and a run that
//! asks the wrong key silently gets WinUI's default blue. Linux has no portable
//! system palette — the desktop's theme lives in a dozen formats behind a dozen
//! libraries — so `native` is the dtr skin there, by decision.
//!
//! The reading is here rather than in `dtr-widgets` because it is platform code
//! and this is where the platform code lives; the crate only knows how to build
//! a Windows palette from the two values.

use dtr_widgets::Skin;
use egui::Color32;

/// The skin `native` should draw, or `None` where the machine has no answer.
///
/// Read once: `Look::skin` is asked by every surface on every frame, and a
/// registry read per frame — with its two wide-string allocations — is not a
/// thing to do sixty times a second to learn something that changes when the
/// user picks a theme.
pub fn native_skin() -> Option<Skin> {
    static SKIN: std::sync::OnceLock<Option<Skin>> = std::sync::OnceLock::new();
    SKIN.get_or_init(platform_skin).clone()
}

fn platform_skin() -> Option<Skin> {
    // A preview switch, and the only way to look at the Windows chrome from a
    // machine that has no Windows palette: `DTR_NATIVE_SKIN=windows-dark`
    // answers where the registry would, and it wins on Windows too, because a
    // preview that the platform could overrule is not a preview.
    if let Some(forced) = preview_skin(std::env::var("DTR_NATIVE_SKIN").ok().as_deref()) {
        return Some(forced);
    }
    #[cfg(windows)]
    {
        registry_skin()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// `DTR_NATIVE_SKIN`'s values, kept pure so a test can check them without an
/// environment or a cache: the two Windows skins, and nothing else. An unknown
/// value is ignored rather than fatal — a typo in a debugging switch must not
/// stop a terminal from starting.
fn preview_skin(value: Option<&str>) -> Option<Skin> {
    match value {
        Some("windows-dark") => Some(Skin::windows_dark()),
        Some("windows-light") => Some(Skin::windows_light()),
        _ => None,
    }
}

/// Which skin a run settled on, for `-v`: "the user asked for native and got it"
/// and "the user asked for native and got dtr" are different bugs.
pub fn describe(skin: &Skin) -> &'static str {
    if skin.style.mono {
        "dtr"
    } else if skin.palette.is_dark() {
        "windows-dark"
    } else {
        "windows-light"
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

/// DWM stores an accent as `0xAABBGGRR` — the byte order of a `COLORREF`, which
/// is little-endian RGB — while everything else here speaks `RRGGBB`.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn accent_from_dwm(value: u32) -> Color32 {
    let r = (value & 0x0000_00FF) as u8;
    let g = ((value & 0x0000_FF00) >> 8) as u8;
    let b = ((value & 0x00FF_0000) >> 16) as u8;
    Color32::from_rgb(r, g, b)
}

/// Decode `AccentPalette`: eight colours, four bytes each, in RGBx order.
///
/// Measured on a Windows 11 guest whose accent was `#0078D4`; the blob was
/// `99ebff00 4cc2ff00 0091f800 0078d400 0067c000 003e9200 001a6800 f7630c00`
/// — **R first**, which is the opposite of `AccentColor`'s `COLORREF` order two
/// keys over. Reading it as BGRA decodes a plausible red where the blue belongs,
/// so the order is pinned by a test with those bytes.
///
/// The order is light3, light2, light1, base, dark1, dark2, dark3, and then a
/// colour that is not a shade of the accent at all: `#F7630C` on that machine,
/// Windows' second accent. A blob that is not 32 bytes is refused rather than
/// padded, because a wrong length means the format changed.
pub fn ladder_from_registry(bytes: &[u8]) -> Option<dtr_widgets::AccentLadder> {
    if bytes.len() != 32 {
        return None;
    }
    let at = |index: usize| -> Color32 {
        let b = &bytes[index * 4..index * 4 + 3];
        Color32::from_rgb(b[0], b[1], b[2])
    };
    Some(dtr_widgets::AccentLadder {
        light3: at(0),
        light2: at(1),
        light1: at(2),
        base: at(3),
        dark1: at(4),
        dark2: at(5),
        dark3: at(6),
        secondary: at(7),
    })
}

#[cfg(windows)]
fn registry_skin() -> Option<Skin> {
    use dtr_widgets::{WINDOWS_ACCENT_DARK, WINDOWS_ACCENT_LIGHT};

    // `AppsUseLightTheme`: 1 is light and 0 is dark. Absent means the user never
    // chose, and Windows 11's default is light.
    let light = read_dword(
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
    )
    .map_or(true, |value| value != 0);
    let accent = read_dword(r"Software\Microsoft\Windows\DWM", "AccentColor").map(accent_from_dwm);
    // The ladder Windows keeps for its own controls, so a hover lands on the
    // same variant a WinUI button's would instead of an alpha blend of the base.
    let ladder = read_binary(
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent",
        "AccentPalette",
    )
    .and_then(|bytes| ladder_from_registry(&bytes));
    Some(Skin::windows_with_accent(
        !light,
        // WinUI's default accent, shown when the user never chose one
        // (`AccentColor` is absent then, not zero).
        accent.unwrap_or(if light {
            WINDOWS_ACCENT_LIGHT
        } else {
            WINDOWS_ACCENT_DARK
        }),
        ladder,
    ))
}

/// A `REG_BINARY` value, up to 64 bytes: the accent palette is 32 and nothing
/// else this module reads is binary, so a bigger answer is not something to
/// allocate for.
#[cfg(windows)]
fn read_binary(key: &str, name: &str) -> Option<Vec<u8>> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            hkey: isize,
            sub_key: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut core::ffi::c_void,
            data_size: *mut u32,
        ) -> i32;
    }

    const HKEY_CURRENT_USER: isize = -2147483647;
    // RRFRT_REG_BINARY is 0x8; 0x40 is `RRF_RT_REG_LINK`, and asking for the
    // wrong type makes the value look absent — which is how this first shipped
    // and why the guest's hover came back as the alpha blend.
    const RRF_RT_REG_BINARY: u32 = 0x0000_0008;

    let wide = |text: &str| {
        OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (key, name) = (wide(key), wide(name));
    let mut bytes = vec![0u8; 64];
    let mut size = bytes.len() as u32;
    // SAFETY: both strings are NUL-terminated and outlive the call, and the
    // buffer's length is handed over as its capacity.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            bytes.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 || size as usize > bytes.len() {
        return None;
    }
    bytes.truncate(size as usize);
    Some(bytes)
}

#[cfg(windows)]
fn read_dword(key: &str, name: &str) -> Option<u32> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            hkey: isize,
            sub_key: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut core::ffi::c_void,
            data_size: *mut u32,
        ) -> i32;
    }

    // HKEY_CURRENT_USER, RRF_RT_REG_DWORD: the value's type is checked for us, so
    // a string where a number was expected is an error rather than a cast.
    const HKEY_CURRENT_USER: isize = -2147483647;
    const RRF_RT_REG_DWORD: u32 = 0x0000_0010;

    let wide = |text: &str| {
        OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let sub_key = wide(key);
    let value = wide(name);
    let mut data: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            sub_key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            &mut data as *mut u32 as *mut core::ffi::c_void,
            &mut size,
        )
    };
    (status == 0).then_some(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DWM's `AccentColor` is `AABBGGRR`, so reading it as `AARRGGBB` swaps red
    /// and blue — which turns a blue accent amber and is easy to miss on a
    /// machine whose accent happens to be grey.
    #[test]
    fn the_dwm_accent_is_bgr() {
        // 0xAABBGGRR with r=0x11, g=0x22, b=0x33.
        assert_eq!(
            accent_from_dwm(0xAA33_2211),
            Color32::from_rgb(0x11, 0x22, 0x33)
        );
        assert_eq!(accent_from_dwm(0x0000_00B8), Color32::from_rgb(0xB8, 0, 0));
    }

    /// The two skins say which they are, because `-v` has to distinguish "asked
    /// for native and got it" from "asked for native and got dtr".
    /// The preview switch names a skin, and only a skin: no typo, no panic, and
    /// no way to ask for a `dtr` skin through the platform's door.
    #[test]
    fn the_preview_switch_knows_two_skins() {
        assert_eq!(
            preview_skin(Some("windows-dark")).map(|skin| describe(&skin)),
            Some("windows-dark")
        );
        assert_eq!(
            preview_skin(Some("windows-light")).map(|skin| describe(&skin)),
            Some("windows-light")
        );
        assert!(preview_skin(Some("dtr")).is_none());
        assert!(preview_skin(Some("fancy")).is_none());
        assert!(preview_skin(None).is_none());
    }

    /// The guest's real blob: eight colours, R first, and the ladder in the
    /// order Windows documents it. Reading it BGRA would put `#D47800` where the
    /// blue accent belongs and nothing would look obviously wrong.
    #[test]
    fn the_accent_palette_decodes_from_rgba() {
        let blob: Vec<u8> = (0..32)
            .map(|i| {
                [
                    0x99, 0xeb, 0xff, 0x00, 0x4c, 0xc2, 0xff, 0x00, 0x00, 0x91, 0xf8, 0x00, 0x00,
                    0x78, 0xd4, 0x00, 0x00, 0x67, 0xc0, 0x00, 0x00, 0x3e, 0x92, 0x00, 0x00, 0x1a,
                    0x68, 0x00, 0xf7, 0x63, 0x0c, 0x00,
                ][i]
            })
            .collect();
        let ladder = ladder_from_registry(&blob).expect("32 bytes");
        assert_eq!(ladder.base, Color32::from_rgb(0x00, 0x78, 0xd4));
        assert_eq!(ladder.light1, Color32::from_rgb(0x00, 0x91, 0xf8));
        assert_eq!(ladder.light2, Color32::from_rgb(0x4c, 0xc2, 0xff));
        assert_eq!(ladder.light3, Color32::from_rgb(0x99, 0xeb, 0xff));
        assert_eq!(ladder.dark1, Color32::from_rgb(0x00, 0x67, 0xc0));
        assert_eq!(ladder.dark2, Color32::from_rgb(0x00, 0x3e, 0x92));
        assert_eq!(ladder.dark3, Color32::from_rgb(0x00, 0x1a, 0x68));
        assert_eq!(ladder.secondary, Color32::from_rgb(0xf7, 0x63, 0x0c));
        // The two registry keys agree once each is decoded in its own order.
        assert_eq!(ladder.base, accent_from_dwm(0xFFD4_7800));
        // A length that is not 32 means the format changed; guessing is worse.
        assert!(ladder_from_registry(&blob[..31]).is_none());
    }

    #[test]
    fn a_skin_describes_itself() {
        assert_eq!(describe(&Skin::windows_dark()), "windows-dark");
        assert_eq!(describe(&Skin::windows_light()), "windows-light");
    }
}
