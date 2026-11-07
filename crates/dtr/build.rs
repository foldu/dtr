//! Embeds the app icon in the Windows executable.
//!
//! Explorer, the shell's "pin to taskbar" and Alt-Tab read an icon out of the
//! PE's resource directory, not out of a running window: `WM_SETICON` — which
//! winit sets from the app's `window_icon` — only covers a live window, and
//! everything that lists the *file* has nothing else to go on. Without the
//! resource, `dtr.exe` shows the generic application icon everywhere it is
//! named.
//!
//! `zig rc` is Microsoft's resource compiler by another name, and it emits a
//! COFF object holding the `.rsrc` section, which links like any other object.
//! Zig is already the Windows toolchain (`cargo zigbuild`), so this needs no new
//! dependency and no wine. The script is written into `OUT_DIR` rather than the
//! tree because its one line names an absolute path to the artwork, and it uses
//! forward slashes because a backslash is an escape in a resource script.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=../../assets/logo/dtr.ico");
    println!("cargo:rerun-if-env-changed=ZIG");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let icon = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/logo/dtr.ico")
        .canonicalize()
        .expect("assets/logo/dtr.ico is part of the source");

    // Resource id 1: the shell takes the lowest-numbered icon group as the
    // application's own.
    let script = out.join("dtr.rc");
    let icon = icon.display().to_string().replace('\\', "/");
    std::fs::write(&script, format!("1 ICON \"{icon}\"\n")).expect("write the resource script");

    let object = out.join("dtr-icon.o");
    let zig = std::env::var("ZIG").unwrap_or_else(|_| "zig".to_string());
    let status = Command::new(&zig)
        .args(["rc", "/fo"])
        .arg(&object)
        .arg(&script)
        .status()
        .unwrap_or_else(|err| {
            panic!("cannot run `{zig} rc`; it is part of the `zig` the Windows build already uses: {err}")
        });
    assert!(
        status.success(),
        "`{zig} rc` failed for {}",
        script.display()
    );

    println!("cargo:rustc-link-arg-bins={}", object.display());
}
