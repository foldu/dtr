//! What Windows says about its own chrome.
//!
//! Two registry values decide it: light or dark from `AppsUseLightTheme` under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`, and the
//! user's accent from DWM under `HKCU\Software\Microsoft\Windows\DWM` — note the
//! *different* path; `AccentColor` is not under `CurrentVersion`, and a run that
//! asks the wrong key silently gets WinUI's default blue.
//!
//! The three decoders are pure and compiled on every platform, because they are
//! what a test on Linux can hold to the guest's real bytes.

// The decoders are only *called* on Windows; the tests below are the reason they
// are compiled anywhere else.
#![cfg_attr(not(windows), allow(dead_code))]

use egui::Color32;

#[cfg(windows)]
use super::{Platform, Reading};
#[cfg(windows)]
use dtr_widgets::{Skin, WINDOWS_ACCENT_DARK, WINDOWS_ACCENT_LIGHT};

/// The registry, which is the only place Windows keeps its answer.
#[cfg(windows)]
pub(super) struct Registry;

#[cfg(windows)]
impl Platform for Registry {
    fn source(&self) -> &'static str {
        "windows-registry"
    }

    async fn read(&self) -> Result<Reading, String> {
        // `AppsUseLightTheme`: 1 is light and 0 is dark. Absent means the user
        // never chose, and Windows 11's default is light.
        let light = read_dword(
            r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
            "AppsUseLightTheme",
        )
        .map_or(true, |value| value != 0);
        Ok(Reading {
            dark: !light,
            accent: read_dword(r"Software\Microsoft\Windows\DWM", "AccentColor")
                .map(accent_from_dwm),
            // The ladder Windows keeps for its own controls, so a hover lands on
            // the same variant a WinUI button's would instead of an alpha blend of
            // the base.
            ladder: read_binary(
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent",
                "AccentPalette",
            )
            .and_then(|bytes| ladder_from_registry(&bytes)),
        })
    }

    fn skin(&self, reading: &Reading) -> Skin {
        Skin::windows_with_accent(
            reading.dark,
            reading
                .accent
                // WinUI's default accent, shown when the user never chose one
                // (`AccentColor` is absent then, not zero).
                .unwrap_or(if reading.dark {
                    WINDOWS_ACCENT_DARK
                } else {
                    WINDOWS_ACCENT_LIGHT
                }),
            reading.ladder,
        )
    }
}

/// DWM stores an accent as `0xAABBGGRR` — the byte order of a `COLORREF`, which
/// is little-endian RGB — while everything else here speaks `RRGGBB`.
fn accent_from_dwm(value: u32) -> Color32 {
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
fn ladder_from_registry(bytes: &[u8]) -> Option<dtr_widgets::AccentLadder> {
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

    /// The guest's real blob: eight colours, R first, and the ladder in the
    /// order Windows documents it. Reading it BGRA would put `#D47800` where the
    /// blue accent belongs and nothing would look obviously wrong.
    #[test]
    fn the_accent_palette_decodes_from_rgba() {
        let blob: [u8; 32] = [
            0x99, 0xeb, 0xff, 0x00, 0x4c, 0xc2, 0xff, 0x00, 0x00, 0x91, 0xf8, 0x00, 0x00, 0x78,
            0xd4, 0x00, 0x00, 0x67, 0xc0, 0x00, 0x00, 0x3e, 0x92, 0x00, 0x00, 0x1a, 0x68, 0x00,
            0xf7, 0x63, 0x0c, 0x00,
        ];
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
}
