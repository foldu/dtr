//! A stand-in for `dtr-files`: calls `org.dtr.Files.OpenWindow` on a running
//! daemon, waits for the manager window, optionally drives one navigation with
//! real X11 key events, and waits for the daemon's capture of the window.
//!
//! This is the verification client `docs/manager.md` §6 describes:
//!
//! ```text
//! manager_call --address "$DBUS_SESSION_BUS_ADDRESS" \
//!              --path /tmp/scratch --navigate \
//!              --capture /tmp/dtr-portal-manager.png
//! ```
//!
//! The daemon is started by D-Bus activation (the test installs the two `.service`
//! files in an `XDG_DATA_DIRS` directory), runs with `DTR_PORTAL_CAPTURE` and
//! `DTR_PORTAL_CAPTURE_AFTER_MS`, and photographs its first shown window — which
//! is this manager window, because the picker's is hidden. The keys are sent with
//! the XTEST extension, i.e. as the X server sees a keyboard: the window's own
//! `KeyboardInput` handler moves the listing, so the PNG shows the directory the
//! navigation reached and the status bar names it.
//!
//! Exit status 0 means the call was answered, the window was found on the X
//! server, the keys were delivered, and the capture appeared with the requested
//! dimensions.
//!
//! The window is closed before this exits, so the daemon's idle timeout — and the
//! run — end by themselves.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dtr_portal::{FILES_INTERFACE, FILES_OBJECT_PATH, FILES_SERVICE_NAME};
use zbus::zvariant::OwnedValue;

/// The two keys one navigation is: `Right` moves the cursor one cell (the default
/// grid layout), `Enter` opens the folder under it.
const RIGHT: u32 = 0xff53;
const ENTER: u32 = 0xff0d;

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let mut address: Option<String> = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
    let mut path: Option<PathBuf> = None;
    let mut capture: Option<PathBuf> = None;
    let mut navigate = false;
    let mut timeout = Duration::from_secs(30);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => address = args.next(),
            "--path" => path = args.next().map(PathBuf::from),
            "--capture" => capture = args.next().map(PathBuf::from),
            "--navigate" => navigate = true,
            "--timeout" => {
                let seconds: u64 = args
                    .next()
                    .expect("--timeout needs seconds")
                    .parse()
                    .expect("seconds");
                timeout = Duration::from_secs(seconds);
            }
            other => panic!("unknown argument {other}"),
        }
    }
    let address = address.expect("--address or DBUS_SESSION_BUS_ADDRESS");
    match run(&address, path, capture, navigate, timeout) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("FAILED: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(
    address: &str,
    path: Option<PathBuf>,
    capture: Option<PathBuf>,
    navigate: bool,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    let connection = pollster::block_on(async {
        zbus::connection::Builder::address(address)
            .map_err(|err| anyhow::anyhow!("{err}"))?
            .build()
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))
    })?;

    // A directory with something in it, so the picture is a listing and not an
    // empty pane: one folder to navigate into (with its own file, so the second
    // picture cannot be mistaken for the first) and two files.
    let scratch = match path {
        Some(path) => path,
        None => {
            let scratch = tempfile::tempdir()?.keep();
            let pictures = scratch.join("pictures");
            std::fs::create_dir(&pictures)?;
            std::fs::write(pictures.join("inside.png"), b"not really a png")?;
            std::fs::write(scratch.join("notes.txt"), b"notes")?;
            std::fs::write(scratch.join("photo.png"), b"not really a png")?;
            scratch
        }
    };
    anyhow::ensure!(scratch.is_dir(), "{} is not a directory", scratch.display());

    let path_text = scratch.to_string_lossy().into_owned();
    let options: HashMap<String, OwnedValue> = HashMap::new();
    let proxy = pollster::block_on(zbus::Proxy::new(
        &connection,
        FILES_SERVICE_NAME,
        FILES_OBJECT_PATH,
        FILES_INTERFACE,
    ))?;
    let reply = pollster::block_on(proxy.call_method("OpenWindow", &(path_text, options)))?;
    let (handle,): (String,) = reply.body().deserialize()?;
    println!("opened manager window {handle}");

    // The window is created when the method answers; it is mapped a moment later.
    let window = wait_for_window(deadline)?;
    println!("manager window 0x{window:x}");
    if navigate {
        // The keys go through XTEST: the X server delivers them like a keyboard's,
        // which is what exercises the window's own input path rather than a hook
        // this example would be the only user of.
        focus(window)?;
        std::thread::sleep(Duration::from_millis(300));
        press(RIGHT)?;
        std::thread::sleep(Duration::from_millis(80));
        press(ENTER)?;
        println!("sent Right, Enter to 0x{window:x}");
    }

    if let Some(capture) = &capture {
        let (width, height) = wait_for_capture(capture, deadline)?;
        println!("captured {width} × {height} at {}", capture.display());
    }

    // Close the window so the daemon's idle timeout ends the run: the tidy exit
    // is what proves the window was a window and not a process.
    let _ = pollster::block_on(proxy.call_method("CloseWindow", &(handle,)));
    Ok(())
}

/// The manager window: the one viewable top-level with a `Files` caption.
///
/// Under Xvfb there is no window manager, so every mapped window is a child of the
/// root; the picker's warm window is unmapped and is not a candidate.
fn wait_for_window(deadline: Instant) -> anyhow::Result<u32> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
    let (connection, screen) = x11rb::connect(None)?;
    let root = connection.setup().roots[screen].root;
    loop {
        let tree = connection.query_tree(root)?.reply()?;
        for child in tree.children {
            let attributes = connection.get_window_attributes(child)?.reply()?;
            if attributes.map_state != x11rb::protocol::xproto::MapState::VIEWABLE {
                continue;
            }
            let title = connection
                .get_property(false, child, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256)?
                .reply()?;
            let title = String::from_utf8_lossy(&title.value);
            if title.contains("Files") {
                return Ok(child);
            }
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "the daemon never mapped a Files window"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Give the manager window the input focus, the way a window manager would.
fn focus(window: u32) -> anyhow::Result<()> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, InputFocus};
    let (connection, _screen) = x11rb::connect(None)?;
    connection.set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)?;
    connection.flush()?;
    let focus = connection.get_input_focus()?.reply()?;
    println!("input focus is 0x{:x} (wanted 0x{window:x})", focus.focus);
    Ok(())
}

/// One key press and release, through XTEST.
fn press(keysym: u32) -> anyhow::Result<()> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xtest::ConnectionExt as _;
    let (connection, screen) = x11rb::connect(None)?;
    let root = connection.setup().roots[screen].root;
    let keycode = keycode_for(&connection, keysym)?;
    // `type` 2 is KeyPress, 3 is KeyRelease (`XTestFakeInput`); the pointer
    // coordinates and device are unused for a key.
    connection.xtest_fake_input(2, keycode, 0, root, 0, 0, 0)?;
    connection.flush()?;
    std::thread::sleep(Duration::from_millis(30));
    connection.xtest_fake_input(3, keycode, 0, root, 0, 0, 0)?;
    connection.flush()?;
    Ok(())
}

/// The keycode whose mapping contains `keysym`, from the server's own table.
fn keycode_for(
    connection: &x11rb::rust_connection::RustConnection,
    keysym: u32,
) -> anyhow::Result<u8> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::ConnectionExt as _;
    let setup = connection.setup();
    let first = setup.min_keycode;
    let count = setup.max_keycode - first + 1;
    let mapping = connection.get_keyboard_mapping(first, count)?.reply()?;
    let per_keycode = mapping.keysyms_per_keycode;
    for (index, chunk) in mapping.keysyms.chunks(usize::from(per_keycode)).enumerate() {
        if chunk.contains(&keysym) {
            return Ok(first + index as u8);
        }
    }
    anyhow::bail!("the X server has no keycode for keysym 0x{keysym:x}")
}

/// Wait until the daemon has written a complete PNG, and read its size.
fn wait_for_capture(path: &Path, deadline: Instant) -> anyhow::Result<(u32, u32)> {
    loop {
        anyhow::ensure!(
            Instant::now() < deadline,
            "the daemon never wrote a complete {}",
            path.display()
        );
        if path.exists()
            && let Ok(size) = png_size(path)
        {
            return Ok(size);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn png_size(path: &Path) -> anyhow::Result<(u32, u32)> {
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let reader = png::Decoder::new(file).read_info()?;
    let info = reader.info();
    Ok((info.width, info.height))
}
