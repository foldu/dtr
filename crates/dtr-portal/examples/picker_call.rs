//! A stand-in for `xdg-desktop-portal`: calls `OpenFile` on a running
//! `dtr-portal`, waits for the picker's window capture, checks what the window is
//! (its X11 transient hint), and then closes the request.
//!
//! This is the verification client `docs/portal.md` §9 describes. The daemon is
//! started under Xvfb with `DTR_PORTAL_CAPTURE=/tmp/picker.png`; this program
//! gives it a request with a real X11 parent window, waits until that PNG exists,
//! reads its dimensions, and — before closing — asks the X server whether the
//! picker's window carries `WM_TRANSIENT_FOR == parent` and
//! `_NET_WM_STATE_MODAL`, which is the one part of a portal dialog that cannot
//! be seen in a screenshot.
//!
//! ```text
//! picker_call --address "$DBUS_SESSION_BUS_ADDRESS" \
//!             --capture /tmp/picker.png --parent
//! ```
//!
//! Exit status 0 means every check passed and the request answered cancel (the
//! person did not choose anything: the client cannot press a button).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dtr_portal::{INTERFACE, OBJECT_PATH, REQUEST_INTERFACE, SERVICE_NAME};
use x11rb::rust_connection::RustConnection as XConnection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// The parent window this client makes, when `--parent` is asked for: 200 × 150
/// at (40, 40), the way an application's window would be.
const PARENT: (i16, i16) = (40, 40);
const PARENT_SIZE: (u16, u16) = (200, 150);

fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let mut address: Option<String> = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
    let mut capture: PathBuf = PathBuf::from("/tmp/dtr-portal-picker.png");
    let mut capture_given = false;
    let mut parent = false;
    let mut timeout = Duration::from_secs(30);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--address" => address = args.next(),
            "--capture" => {
                capture = PathBuf::from(args.next().expect("--capture needs a path"));
                capture_given = true;
            }
            "--parent" => parent = true,
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
    if !capture_given {
        eprintln!("note: waiting for the default capture path {capture:?}");
    }

    match run(&address, &capture, parent, timeout) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("FAILED: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(address: &str, capture: &Path, want_parent: bool, timeout: Duration) -> anyhow::Result<()> {
    let connection = pollster::block_on(async {
        zbus::connection::Builder::address(address)
            .map_err(|err| anyhow::anyhow!("{err}"))?
            .build()
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))
    })?;

    // A real X11 window for the picker to be transient for. `x11rb` talks to the
    // same server that Xvfb is running, so the id is one the daemon's winit can
    // use — which is the whole point of `x11:0x…`.
    let x = want_parent.then(open_x11);
    let (parent_id, x_connection) = match x {
        Some(Ok((id, connection, screen))) => (Some(id), Some((connection, screen))),
        Some(Err(err)) => return Err(err),
        None => (None, None),
    };
    let parent_window = match parent_id {
        Some(id) => format!("x11:0x{id:x}"),
        None => String::new(),
    };

    // A directory with something in it, so the picture is a listing and not an
    // empty pane: two files, a folder, and a filter to draw the footer's slot.
    let scratch = tempfile::tempdir()?;
    std::fs::write(scratch.path().join("photo.png"), b"not really a png")?;
    std::fs::write(scratch.path().join("notes.txt"), b"notes")?;
    std::fs::create_dir(scratch.path().join("pictures"))?;

    let handle =
        OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/1_1/xvfb_picker")
            .expect("a request path");
    let mut options: HashMap<String, OwnedValue> = HashMap::new();
    options.insert(
        "current_folder".to_owned(),
        OwnedValue::try_from(Value::from(
            scratch.path().as_os_str().as_encoded_bytes().to_vec(),
        ))
        .expect("folder bytes"),
    );
    options.insert(
        "accept_label".to_owned(),
        OwnedValue::try_from(Value::from("Open".to_owned())).expect("accept label"),
    );
    options.insert(
        "filters".to_owned(),
        OwnedValue::try_from(Value::from(vec![(
            "Images".to_owned(),
            vec![(0u32, "*.png".to_owned())],
        )]))
        .expect("filters"),
    );

    // The method call is the interaction: it returns when the person answers (or
    // the frontend closes), which here is the `Close()` below.
    let called = {
        let connection = connection.clone();
        let handle = handle.clone();
        std::thread::spawn(move || {
            pollster::block_on(async move {
                let proxy =
                    zbus::Proxy::new(&connection, SERVICE_NAME, OBJECT_PATH, INTERFACE).await?;
                let message = proxy
                    .call_method(
                        "OpenFile",
                        &(
                            handle,
                            "org.example.App".to_owned(),
                            parent_window,
                            "Open a picture".to_owned(),
                            options,
                        ),
                    )
                    .await?;
                let wire: (u32, HashMap<String, OwnedValue>) = message.body().deserialize()?;
                Ok::<u32, zbus::Error>(wire.0)
            })
        })
    };

    // Wait for the daemon to photograph the picker. The file appearing *is* the
    // proof the window existed, was mapped, and drew a frame — but it appears
    // when the encoder creates it, so the read is retried until the PNG is
    // complete rather than assuming the first byte means the last one.
    let deadline = Instant::now() + timeout;
    let (width, height) = loop {
        anyhow::ensure!(
            Instant::now() < deadline,
            "the picker never wrote a complete {capture:?} within {timeout:?}"
        );
        if capture.exists() {
            match png_size(capture) {
                Ok(size) => break size,
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    println!("captured {} × {} at {}", width, height, capture.display());

    // The X11 hints, read from the server rather than trusted from the log.
    if let (Some(parent), Some((connection, screen))) = (parent_id, &x_connection) {
        let window = find_transient(connection, *screen, parent)?
            .ok_or_else(|| anyhow::anyhow!("no window is transient for 0x{parent:x}"))?;
        println!("window 0x{window:x} is transient for 0x{parent:x}");
        let states = atoms(connection, window, b"_NET_WM_STATE")?;
        println!("_NET_WM_STATE = {states:?}");
        anyhow::ensure!(
            states.iter().any(|atom| atom == "_NET_WM_STATE_MODAL"),
            "the picker is not modal"
        );
    }

    // The backend exports the request object at `handle` before it asks the host
    // for a window, so a frontend may only close an object that exists: a Close
    // that beats the method call is the frontend's bug, not the dialog's. Real
    // frontends know their token; this client polls to be as well-behaved.
    let deadline = Instant::now() + timeout;
    while !pollster::block_on(request_object_exists(&connection, handle.as_str())) {
        anyhow::ensure!(
            Instant::now() < deadline,
            "the request object at {} never appeared",
            handle.as_str()
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Close, the way a frontend does when it gives up: the method answers cancel
    // and the window comes down.
    pollster::block_on(async {
        let proxy = zbus::Proxy::new(
            &connection,
            SERVICE_NAME,
            handle.as_str(),
            REQUEST_INTERFACE,
        )
        .await?;
        proxy.call_method("Close", &()).await?;
        Ok::<(), zbus::Error>(())
    })?;
    let response = called
        .join()
        .expect("the OpenFile thread")
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    anyhow::ensure!(response == 1, "a close answers cancel, got {response}");
    println!("request answered response 1 (cancel)");
    Ok(())
}

/// Whether the backend has exported the per-request object yet.
async fn request_object_exists(connection: &zbus::Connection, path: &str) -> bool {
    let Ok(proxy) = zbus::Proxy::new(
        connection,
        SERVICE_NAME,
        path,
        "org.freedesktop.DBus.Introspectable",
    )
    .await
    else {
        return false;
    };
    proxy.call_method("Introspect", &()).await.is_ok()
}

/// The picker window: the direct child of the root whose `WM_TRANSIENT_FOR` is
/// the parent this client made. Under Xvfb there is no window manager, so the
/// picker is a child of the root and the hint is exactly what was written.
fn find_transient(
    x11rb_conn: &XConnection,
    screen: usize,
    parent: u32,
) -> anyhow::Result<Option<u32>> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
    let root = x11rb_conn.setup().roots[screen].root;
    let tree = x11rb_conn.query_tree(root)?.reply()?;
    for child in tree.children {
        let reply = x11rb_conn
            .get_property(
                false,
                child,
                AtomEnum::WM_TRANSIENT_FOR,
                AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()?;
        if let Some(mut values) = reply.value32()
            && values.any(|value| value == parent)
        {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

/// `_NET_WM_STATE` as atom names, so a failure says `[]` or `["…_FOCUSED"]`
/// rather than a number.
fn atoms(x11rb_conn: &XConnection, window: u32, property: &[u8]) -> anyhow::Result<Vec<String>> {
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
    let property = intern(x11rb_conn, property)?;
    let reply = x11rb_conn
        .get_property(false, window, property, AtomEnum::ATOM, 0, 64)?
        .reply()?;
    let mut names = Vec::new();
    for value in reply.value32().into_iter().flatten() {
        names.push(atom_name(x11rb_conn, value)?);
    }
    Ok(names)
}

fn intern(x11rb_conn: &XConnection, name: &[u8]) -> anyhow::Result<u32> {
    use x11rb::protocol::xproto::ConnectionExt as _;
    Ok(x11rb_conn.intern_atom(false, name)?.reply()?.atom)
}

fn atom_name(x11rb_conn: &XConnection, atom: u32) -> anyhow::Result<String> {
    use x11rb::protocol::xproto::ConnectionExt as _;
    let reply = x11rb_conn.get_atom_name(atom)?.reply()?;
    Ok(String::from_utf8_lossy(&reply.name).into_owned())
}

/// Open an X connection with one mapped window on it: its id and the screen.
fn open_x11() -> anyhow::Result<(u32, XConnection, usize)> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, WindowClass};
    let (connection, screen) = x11rb::connect(None)?;
    let root = connection.setup().roots[screen].root;
    let window = connection.generate_id()?;
    connection.create_window(
        x11rb::COPY_FROM_PARENT as u8,
        window,
        root,
        PARENT.0,
        PARENT.1,
        PARENT_SIZE.0,
        PARENT_SIZE.1,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new(),
    )?;
    connection.map_window(window)?;
    connection.flush()?;
    Ok((window, connection, screen))
}

fn png_size(path: &Path) -> anyhow::Result<(u32, u32)> {
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    let reader = png::Decoder::new(file).read_info()?;
    let info = reader.info();
    Ok((info.width, info.height))
}
