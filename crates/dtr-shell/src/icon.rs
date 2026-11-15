//! The shell's icon for a file, as pixels.
//!
//! Windows has no icon directory to read an entry's picture out of. The answer is
//! an `HICON` from the shell's icon cache: the colour page the registry associates
//! with `.pdf`, the bitmap an `.exe` carries inside it, the folder the shell draws
//! for `Downloads`. This module asks for that handle and turns it into RGBA, which
//! is as far as the crate goes: encoding it for egui is `dtr-files`' business, and
//! so is the texture.
//!
//! Three rules, all about *which* icon is asked for:
//!
//! - **A file is asked by its extension, not by itself.** `SHGFI_USEFILEATTRIBUTES`
//!   makes the shell answer from the association without opening the file, so a
//!   listing of two hundred names is two hundred registry lookups and no disk.
//! - **Except when the picture is inside the file.** An `.exe`, a `.lnk` and an
//!   `.ico` *carry* their icon, so those are asked for for real — which is what
//!   makes a tile of `dtr.exe` show dtr's own icon instead of a generic
//!   application.
//! - **A directory is asked for for real**, because the shell decorates some of
//!   them (`Downloads`, OneDrive) and only it knows which.
//!
//! The sizes are the shell's four and not a freedesktop ladder — 16, 32, 48 and
//! 256 pixels, the smallest that is at least the tile. A 48-point tile asks for 48
//! and is sharp; a 2× tile takes the 256 and lets the UI downscale it. That is why
//! the system *image list* is asked and not `SHGetFileInfo`'s own `SHGFI_ICON`:
//! the latter answers 32 and nothing else, and a 32 px picture in a 48 px tile is
//! a blur (the third fallback is still there, because a COM object can be refused).
//!
//! The conversion below is where a wrong stride shows up as a sheared picture and a
//! wrong alpha rule as a black halo, so it is plain arithmetic and is tested on any
//! machine — which is the whole reason `HICON` handling and pixel handling are
//! separate functions in this file.

use std::path::Path;

/// An icon as pixels: RGBA, straight alpha, row 0 at the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The shell's icon for `path`, at the shell size nearest above `px`.
pub fn icon(path: &Path, is_dir: bool, px: u32) -> Option<Icon> {
    platform::icon(path, is_dir, px)
}

/// A 32-bit top-down DIB in BGRA, plus the AND mask when the icon has no alpha of
/// its own, as RGBA.
///
/// `bgra` is what `GetDIBits` writes and what Windows calls the pixel order — blue
/// first. `mask` is the icon's AND mask: an old icon has no alpha channel at all
/// and the mask is where its transparency lives. A set bit is a hole there, and
/// the 1bpp-to-32bpp conversion hands it back as white.
///
/// The alpha rule is the whole of it: **an icon with any non-zero alpha uses its
/// own**, and only a picture with none of it falls back to the mask. The other way
/// round draws a modern icon's anti-aliased edge as a hard one — and every modern
/// icon has a fully transparent corner, so "any non-zero" is the right test.
#[cfg(any(windows, test))]
pub(crate) fn rgba_of(bgra: &[u8], mask: Option<&[u8]>, width: usize, height: usize) -> Vec<u8> {
    let pixels = width * height;
    if bgra.len() < pixels * 4 {
        return Vec::new();
    }
    let has_alpha = bgra.chunks_exact(4).any(|pixel| pixel[3] != 0);
    let mut rgba = Vec::with_capacity(pixels * 4);
    for pixel in 0..pixels {
        let at = pixel * 4;
        let alpha = if has_alpha {
            bgra[at + 3]
        } else {
            match mask.and_then(|mask| mask.get(at)) {
                Some(&hole) if hole != 0 => 0,
                _ => 255,
            }
        };
        rgba.extend_from_slice(&[bgra[at + 2], bgra[at + 1], bgra[at], alpha]);
    }
    rgba
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC,
        GetDIBits, GetObjectW, HBITMAP, ReleaseDC,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    };
    use windows_sys::Win32::UI::Controls::ILD_TRANSPARENT;
    use windows_sys::Win32::UI::Shell::{
        SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES,
        SHGetFileInfoW, SHGetImageList, SHIL_EXTRALARGE, SHIL_JUMBO, SHIL_LARGE, SHIL_SMALL,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};

    use super::Icon;

    /// `IID_IImageList`, which `windows-sys` does not name: the shell's system image
    /// lists are COM objects, and `SHGetImageList` wants the interface by GUID
    /// rather than the typed method the `windows` crate would give.
    const IID_IIMAGE_LIST: windows_sys::core::GUID =
        windows_sys::core::GUID::from_u128(0x46eb5926_582e_4017_9fdf_e8998daa0950);

    /// The extensions whose icon lives *inside* the file, so a lookup by name cannot
    /// find it.
    const CARRIES_ITS_OWN: &[&str] = &[
        "exe",
        "lnk",
        "ico",
        "msi",
        "scr",
        "cpl",
        "msc",
        "url",
        "appref-ms",
    ];

    pub fn icon(path: &Path, is_dir: bool, px: u32) -> Option<Icon> {
        let wide = wide(path);
        let (attributes, by_name) = source(path, is_dir);
        // The whole Win32 half of this module is `unsafe`, and the one block at its
        // entry is where that begins.
        let hicon = unsafe { hicon_for(&wide, attributes, by_name, px) }?;
        let pixels = unsafe { rgba_of_icon(hicon) };
        // The handle is ours — the list hands out a copy — so it goes whatever the
        // conversion made of it.
        unsafe { DestroyIcon(hicon) };
        pixels
    }

    fn wide(path: &Path) -> Vec<u16> {
        let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        wide.push(0);
        wide
    }

    /// What to tell the shell about the entry, and whether to let it answer without
    /// looking at the file.
    fn source(path: &Path, is_dir: bool) -> (u32, bool) {
        if is_dir {
            return (FILE_ATTRIBUTE_DIRECTORY, false);
        }
        let extension = path
            .extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let carries = CARRIES_ITS_OWN.contains(&extension.as_str());
        (FILE_ATTRIBUTE_NORMAL, !carries)
    }

    /// The icon for `wide`, from the system image list when this process may have
    /// it and from `SHGetFileInfo`'s own 32-pixel path when it may not.
    unsafe fn hicon_for(wide: &[u16], attributes: u32, by_name: bool, px: u32) -> Option<HICON> {
        unsafe {
            let extra = if by_name { SHGFI_USEFILEATTRIBUTES } else { 0 };
            let mut info: SHFILEINFOW = std::mem::zeroed();
            let found = SHGetFileInfoW(
                wide.as_ptr(),
                attributes,
                &mut info,
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_SYSICONINDEX | extra,
            );
            if found != 0 {
                if let Some(hicon) = from_list(info.iIcon, px) {
                    return Some(hicon);
                }
            }
            let mut small: SHFILEINFOW = std::mem::zeroed();
            let found = SHGetFileInfoW(
                wide.as_ptr(),
                attributes,
                &mut small,
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON | extra,
            );
            if found == 0 || small.hIcon.is_null() {
                return None;
            }
            Some(small.hIcon)
        }
    }

    /// The icon at `index` in the system list whose side is nearest above `px`.
    unsafe fn from_list(index: i32, px: u32) -> Option<HICON> {
        unsafe {
            let class = if px <= 16 {
                SHIL_SMALL
            } else if px <= 32 {
                SHIL_LARGE
            } else if px <= 48 {
                SHIL_EXTRALARGE
            } else {
                SHIL_JUMBO
            } as i32;
            let mut list: *mut c_void = std::ptr::null_mut();
            let answer = SHGetImageList(class, &IID_IIMAGE_LIST, &mut list);
            if answer < 0 || list.is_null() {
                return None;
            }
            let icon = get_icon(list, index);
            release(list);
            icon
        }
    }

    /// `IImageList::GetIcon`, by hand: `windows-sys` has `SHGetImageList` but no
    /// vtable for what it returns.
    ///
    /// The index is the interface's own order — `IUnknown`'s three, then `Add`,
    /// `ReplaceIcon`, `SetOverlayImage`, `Replace`, `AddMasked`, `Draw`, `Remove`,
    /// `GetIcon`. A wrong index would call the wrong method with these arguments, so
    /// the answer is verified before it is used: a null handle is refused, and [`dib`]
    /// refuses a bitmap that is not icon-sized before anything is read.
    unsafe fn get_icon(list: *mut c_void, index: i32) -> Option<HICON> {
        unsafe {
            let vtable = *(list as *mut *const usize);
            let get: unsafe extern "system" fn(*mut c_void, i32, u32, *mut HICON) -> i32 =
                std::mem::transmute(*vtable.add(10));
            let mut hicon: HICON = std::ptr::null_mut();
            let answer = get(list, index, ILD_TRANSPARENT, &mut hicon);
            if answer < 0 || hicon.is_null() {
                return None;
            }
            Some(hicon)
        }
    }

    unsafe fn release(list: *mut c_void) {
        unsafe {
            let vtable = *(list as *mut *const usize);
            let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                std::mem::transmute(*vtable.add(2));
            release(list);
        }
    }

    /// An `HICON` as pixels: its colour bitmap, and its mask when it has no alpha of
    /// its own.
    unsafe fn rgba_of_icon(hicon: HICON) -> Option<Icon> {
        unsafe {
            let mut info: ICONINFO = std::mem::zeroed();
            if GetIconInfo(hicon, &mut info) == 0 {
                return None;
            }
            let colour = dib(info.hbmColor);
            let mask = dib(info.hbmMask);
            // The two bitmaps belong to the copy `GetIconInfo` made, not to the icon.
            if !info.hbmColor.is_null() {
                DeleteObject(info.hbmColor);
            }
            if !info.hbmMask.is_null() {
                DeleteObject(info.hbmMask);
            }
            let (width, height, bgra) = colour?;
            let mask = mask.map(|(_, _, bytes)| bytes);
            // A monochrome icon has no colour bitmap at all: the mask is the picture,
            // and there is nothing here to tint — so the caller's drawn glyph answers
            // instead of a wrong picture.
            let rgba = super::rgba_of(&bgra, mask.as_deref(), width as usize, height as usize);
            if rgba.is_empty() {
                return None;
            }
            Some(Icon {
                width,
                height,
                rgba,
            })
        }
    }

    /// A GDI bitmap as a 32-bit top-down DIB — BGRA, row 0 at the top.
    unsafe fn dib(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
        unsafe {
            if bitmap.is_null() {
                return None;
            }
            let mut header: BITMAP = std::mem::zeroed();
            let asked = GetObjectW(
                bitmap,
                std::mem::size_of::<BITMAP>() as i32,
                &mut header as *mut BITMAP as *mut c_void,
            );
            if asked == 0 {
                return None;
            }
            let width = header.bmWidth.max(0) as u32;
            let height = header.bmHeight.max(0) as u32;
            // An icon is small. Anything else is a vtable that answered with something
            // that is not a bitmap, and drawing it is worse than a glyph.
            if width == 0 || height == 0 || width > 512 || height > 512 {
                return None;
            }
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            info.bmiHeader.biWidth = width as i32;
            // Negative, so the first row `GetDIBits` writes is the top one.
            info.bmiHeader.biHeight = -(height as i32);
            info.bmiHeader.biPlanes = 1;
            info.bmiHeader.biBitCount = 32;
            info.bmiHeader.biCompression = BI_RGB;
            let mut bytes = vec![0u8; width as usize * height as usize * 4];
            let screen = GetDC(std::ptr::null_mut());
            if screen.is_null() {
                return None;
            }
            let lines = GetDIBits(
                screen,
                bitmap,
                0,
                height,
                bytes.as_mut_ptr() as *mut c_void,
                &mut info,
                DIB_RGB_COLORS,
            );
            ReleaseDC(std::ptr::null_mut(), screen);
            if lines == 0 {
                return None;
            }
            Some((width, height, bytes))
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::Path;

    use super::Icon;

    /// There is no shell to ask. A freedesktop machine's pictures are the icon
    /// theme's, and `dtr-files` asks it next.
    pub fn icon(_path: &Path, _is_dir: bool, _px: u32) -> Option<Icon> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-pixel icon with an alpha channel: the pixels stay as they were written,
    /// and blue and red are not swapped on the way out.
    #[test]
    fn an_icon_with_alpha_keeps_its_own_pixels() {
        // BGRA, as `GetDIBits` writes it: a blue pixel and a red one, the first
        // half transparent.
        let bgra = [255, 0, 0, 0, 0, 0, 255, 255];
        assert_eq!(rgba_of(&bgra, None, 2, 1), [0, 0, 255, 0, 255, 0, 0, 255]);
    }

    /// An old icon: no alpha anywhere, so the AND mask is where its shape is. A set
    /// bit is a hole, which is how a folder's rounded edge arrives.
    #[test]
    fn an_icon_without_alpha_takes_its_shape_from_the_mask() {
        let bgra = [0, 0, 0, 0, 255, 255, 255, 0];
        let mask = [0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0];
        assert_eq!(
            rgba_of(&bgra, Some(&mask), 2, 1),
            [0, 0, 0, 255, 255, 255, 255, 0]
        );
        // With no mask either, an icon with no alpha is a solid rectangle rather
        // than an invisible one.
        assert_eq!(
            rgba_of(&bgra, None, 2, 1),
            [0, 0, 0, 255, 255, 255, 255, 255]
        );
    }

    /// A buffer that is too short is not a picture: refusing it here is one of the
    /// two things that stops a wrong vtable from painting noise.
    #[test]
    fn a_short_buffer_is_not_a_picture() {
        assert!(rgba_of(&[1, 2, 3], None, 2, 2).is_empty());
        assert!(rgba_of(&[], None, 0, 0).is_empty());
    }

    /// The mask is only consulted when the picture has no alpha of its own: an icon
    /// whose alpha is *partly* zero keeps the mask out of it, or a modern icon's
    /// rounded corner becomes a hard one and its shadow disappears.
    #[test]
    fn a_mask_does_not_touch_an_icon_that_has_alpha() {
        let bgra = [0, 0, 0, 0, 0, 0, 0, 255];
        let mask = [0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0];
        assert_eq!(
            rgba_of(&bgra, Some(&mask), 2, 1),
            [0, 0, 0, 0, 0, 0, 0, 255]
        );
    }
}
