//! The platform's own chrome, when this machine has one.
//!
//! `native` means "look like a window on this machine", and only Windows tells
//! us how: light or dark from `AppsUseLightTheme`, and the user's accent from
//! DWM, both under `HKCU\Software\Microsoft\Windows\CurrentVersion`. Linux has no
//! portable system palette — the desktop's theme lives in a dozen formats behind
//! a dozen libraries — so `native` is the dtr skin there, by decision.
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
    #[cfg(windows)]
    {
        registry_skin()
    }
    #[cfg(not(windows))]
    {
        None
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
    let accent = skin.palette.accent;
    format!("#{:02X}{:02X}{:02X}", accent.r(), accent.g(), accent.b())
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
    let accent = read_dword(
        r"Software\Microsoft\Windows\CurrentVersion\DWM",
        "AccentColor",
    )
    .map(accent_from_dwm);
    Some(Skin::windows(
        !light,
        // WinUI's default accent, shown when the user never chose one
        // (`AccentColor` is absent then, not zero).
        accent.unwrap_or(if light {
            WINDOWS_ACCENT_LIGHT
        } else {
            WINDOWS_ACCENT_DARK
        }),
    ))
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
    #[test]
    fn a_skin_describes_itself() {
        assert_eq!(describe(&Skin::windows_dark()), "windows-dark");
        assert_eq!(describe(&Skin::windows_light()), "windows-light");
    }
}
