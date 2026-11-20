//! The wire, end to end: a private session bus, this crate's real backend, and a
//! client that acts as `xdg-desktop-portal`.
//!
//! The pattern is `crates/dtr/src/notify.rs`'s: a `dbus-daemon` of its own, so a
//! test neither depends on nor disturbs the desktop's bus, and a loud skip when
//! the daemon is not installed (a Nix check may not ship it; a red suite for a
//! missing tool is not honest). What is different here is that the *frontend* is
//! the test: it sends the same vardicts an application's portal call becomes and
//! checks the `(u response, a{sv} results)` that comes back.
//!
//! The host is headless — the window is the follow-up — and answers through the
//! same [`Reply`] the real window will use, from the test thread. A request that
//! is deliberately left pending is how `Request.Close()` is exercised: nothing in
//! the production path knows the difference between "the person is still
//! deciding" and "the test has not answered yet".
//!
//! The signatures asserted against are the installed `xdg-desktop-portal`
//! 1.22.1's (`org.freedesktop.impl.portal.FileChooser.xml`), copied here as the
//! expected type sequence so that a change to the Rust surface fails this test
//! with a diff instead of with a frontend that cannot call the backend.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use dtr_portal::{
    Answer, Call, Chosen, Host, INTERFACE, OBJECT_PATH, Portal, REQUEST_INTERFACE, RequestId,
    SERVICE_NAME, Select,
};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// A private session bus, killed when the fixture drops.
struct Bus {
    child: std::process::Child,
    address: String,
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn private_bus() -> Option<Bus> {
    let mut child = std::process::Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(stdout), &mut line).ok()?;
    let address = line.trim().to_owned();
    (!address.is_empty()).then_some(Bus { child, address })
}

/// The headless host: records the calls a window would draw, and the cancels a
/// window would take down. Everything is unbounded and non-blocking because
/// `Host::open` runs on the D-Bus task, as the real one must not block there
/// either.
struct Headless {
    opened: async_channel::Sender<Call>,
    closed: async_channel::Sender<RequestId>,
}

impl Host for Headless {
    fn open(&mut self, call: Call) {
        let _ = self.opened.try_send(call);
    }

    fn close(&mut self, id: RequestId) {
        let _ = self.closed.try_send(id);
    }
}

/// A bus, a portal on it, and a client connection that pretends to be the
/// frontend.
struct Fixture {
    client: zbus::Connection,
    opened: async_channel::Receiver<Call>,
    closed: async_channel::Receiver<RequestId>,
}

impl Fixture {
    /// Start the daemon and the backend, and connect a frontend client.
    ///
    /// `None` means `dbus-daemon` is not on `PATH`; the caller skips loudly.
    fn start() -> Option<Self> {
        let bus = private_bus()?;
        let (opened, opened_rx) = async_channel::unbounded();
        let (closed, closed_rx) = async_channel::unbounded();
        let (ready, waiting) = mpsc::channel();
        let address = bus.address.clone();
        let serve_address = address.clone();
        std::thread::Builder::new()
            .name("dtr-portal-test".to_owned())
            .spawn(move || {
                // The daemon lives exactly as long as this thread: dropping the
                // `Bus` kills it, and the thread serves until the process ends.
                let _bus = bus;
                let portal = Portal::new(Headless { opened, closed });
                pollster::block_on(async move {
                    match portal.serve_at(&serve_address).await {
                        Ok(connection) => {
                            let _ = ready.send(Ok(()));
                            // Serve until the process ends; the connection must
                            // stay in scope, which is what this line is for.
                            std::future::pending::<()>().await;
                            drop(connection);
                        }
                        Err(err) => {
                            let _ = ready.send(Err(err.to_string()));
                        }
                    }
                });
            })
            .expect("a thread for the portal");
        match waiting
            .recv_timeout(Duration::from_secs(10))
            .expect("the portal starts")
        {
            Ok(()) => {}
            Err(err) => panic!("the portal did not serve: {err}"),
        }
        let client = pollster::block_on(async {
            let builder =
                zbus::connection::Builder::address(address.as_str()).expect("the bus address");
            builder.build().await.expect("a client connection")
        });
        Some(Self {
            client,
            opened: opened_rx,
            closed: closed_rx,
        })
    }

    /// The request object's path, the way a frontend builds one from a
    /// `handle_token`.
    fn handle(&self, token: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(format!(
            "/org/freedesktop/portal/desktop/request/1_1/{token}"
        ))
        .expect("a request path")
    }

    /// The next request the host should have been told about.
    fn next_call(&self) -> Call {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.opened.try_recv() {
                Ok(call) => return call,
                Err(async_channel::TryRecvError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "the host was never asked for a picker"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("the opened channel ended: {err}"),
            }
        }
    }

    /// Whether the host was told to take a window down, within the deadline.
    fn expect_closed(&self, id: &RequestId) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match self.closed.try_recv() {
                Ok(closed) => {
                    assert_eq!(&closed, id);
                    return;
                }
                Err(async_channel::TryRecvError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "the host was never told to close {id}"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(err) => panic!("the closed channel ended: {err}"),
            }
        }
    }
}

/// The answer `(response, results)` a method call produced.
type Wire = zbus::Result<(u32, HashMap<String, OwnedValue>)>;

/// Call a method on another thread and hand the answer back through a channel,
/// so a test that expects a pending request can time out instead of hanging.
fn spawn_call(
    connection: zbus::Connection,
    method: &'static str,
    handle: OwnedObjectPath,
    options: HashMap<String, OwnedValue>,
) -> mpsc::Receiver<Wire> {
    let (done, waiting) = mpsc::channel();
    std::thread::spawn(move || {
        let result = pollster::block_on(async move {
            let proxy = zbus::Proxy::new(&connection, SERVICE_NAME, OBJECT_PATH, INTERFACE).await?;
            let message = proxy
                .call_method(
                    method,
                    &(
                        handle,
                        "org.example.App".to_owned(),
                        "x11:0x1234".to_owned(),
                        "Open a file".to_owned(),
                        options,
                    ),
                )
                .await?;
            message.body().deserialize()
        });
        let _ = done.send(result);
    });
    waiting
}

fn wait(waiting: mpsc::Receiver<Wire>) -> Wire {
    waiting
        .recv_timeout(Duration::from_secs(10))
        .expect("the method call answered instead of hanging")
}

/// A vardict value, the way a sender builds one.
///
/// One helper per shape rather than one generic helper: `Value::from` is
/// implemented both per concrete type and through `Into<Structure>`, and a
/// generic `T: Into<Value>` leaves the compiler unable to choose between them.
fn owned(value: Value<'static>) -> OwnedValue {
    OwnedValue::try_from(value).expect("an owned value")
}

fn a_bool(value: bool) -> OwnedValue {
    owned(Value::from(value))
}

fn a_string(value: &str) -> OwnedValue {
    owned(Value::from(value.to_owned()))
}

fn bytes(value: Vec<u8>) -> OwnedValue {
    owned(Value::from(value))
}

fn filters(value: Vec<(String, Vec<(u32, String)>)>) -> OwnedValue {
    owned(Value::from(value))
}

fn filter(value: (String, Vec<(u32, String)>)) -> OwnedValue {
    owned(Value::from(value))
}

fn file_names(value: Vec<Vec<u8>>) -> OwnedValue {
    owned(Value::from(value))
}

fn options(entries: Vec<(&str, OwnedValue)>) -> HashMap<String, OwnedValue> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn pair(name: &str, patterns: &[(u32, &str)]) -> (String, Vec<(u32, String)>) {
    (
        name.to_owned(),
        patterns
            .iter()
            .map(|(kind, pattern)| (*kind, (*pattern).to_owned()))
            .collect(),
    )
}

fn uris(results: &mut HashMap<String, OwnedValue>) -> Vec<String> {
    let value = results.remove("uris").expect("the uris result");
    Vec::<String>::try_from(value).expect("an array of strings")
}

fn writable(results: &mut HashMap<String, OwnedValue>) -> bool {
    bool::try_from(results.remove("writable").expect("the writable result")).expect("a boolean")
}

fn current_filter(results: &mut HashMap<String, OwnedValue>) -> (String, Vec<(u32, String)>) {
    let value = results
        .remove("current_filter")
        .expect("the current_filter result");
    <(String, Vec<(u32, String)>)>::try_from(value).expect("a serialized filter")
}

/// The `type="…"` attributes under one `<method>` in introspection XML, in
/// document order, so a signature can be compared without an XML parser.
fn method_arg_types(xml: &str, method: &str) -> Vec<String> {
    let open = format!("<method name=\"{method}\">");
    let start = xml.find(&open).unwrap_or_else(|| {
        panic!("no `{method}` in the introspection XML: {xml}");
    });
    let rest = &xml[start + open.len()..];
    let end = rest.find("</method>").expect("a method end");
    let block = &rest[..end];
    let mut types = Vec::new();
    let mut cursor = 0;
    while let Some(at) = block[cursor..].find("type=\"") {
        let from = cursor + at + "type=\"".len();
        let to = from + block[from..].find('"').expect("a closing quote");
        types.push(block[from..to].to_owned());
        cursor = to + 1;
    }
    types
}

const OPEN_FILE: &str = "OpenFile";

/// The interface 1.22.1 defines, from its XML: five in-arguments and two out.
fn expected_signature() -> Vec<&'static str> {
    vec!["o", "s", "s", "s", "a{sv}", "u", "a{sv}"]
}

#[test]
fn the_interface_on_the_bus_is_the_one_the_frontend_expects() {
    let Some(fixture) = Fixture::start() else {
        eprintln!("no dbus-daemon on PATH; skipping the file chooser portal test");
        return;
    };
    let xml = pollster::block_on(async {
        let proxy = zbus::Proxy::new(
            &fixture.client,
            SERVICE_NAME,
            OBJECT_PATH,
            "org.freedesktop.DBus.Introspectable",
        )
        .await
        .expect("an introspection proxy");
        let message = proxy
            .call_method("Introspect", &())
            .await
            .expect("introspection");
        message
            .body()
            .deserialize::<String>()
            .expect("introspection XML")
    });
    for method in ["OpenFile", "SaveFile", "SaveFiles"] {
        assert_eq!(
            method_arg_types(&xml, method),
            expected_signature(),
            "`{method}` does not match xdg-desktop-portal 1.22.1's XML"
        );
    }
    // The interface name itself is what `dtr.portal` names.
    assert!(xml.contains(INTERFACE), "{xml}");
}

/// The whole point of the crate: a realistic `OpenFile`, over a real bus,
/// answered with response 0 and percent-encoded `uris`.
#[test]
fn a_real_open_file_is_answered_over_the_bus() {
    let Some(fixture) = Fixture::start() else {
        eprintln!("no dbus-daemon on PATH; skipping the file chooser portal test");
        return;
    };
    let handle = fixture.handle("open1");
    let waiting = spawn_call(
        fixture.client.clone(),
        OPEN_FILE,
        handle.clone(),
        options(vec![
            ("multiple", a_bool(true)),
            (
                "filters",
                filters(vec![
                    pair("Images", &[(0, "*.png"), (1, "image/png")]),
                    pair("Text", &[(0, "*.txt")]),
                ]),
            ),
            (
                "current_filter",
                filter(pair("Images", &[(0, "*.png"), (1, "image/png")])),
            ),
            ("current_folder", bytes(b"/tmp/a b+c#d".to_vec())),
            ("accept_label", a_string("_Pick")),
            ("modal", a_bool(false)),
        ]),
    );

    let call = fixture.next_call();
    assert_eq!(call.id.path(), handle.as_str());
    assert_eq!(call.request.select, Select::Files);
    assert_eq!(call.request.accept, "_Pick");
    assert_eq!(call.request.folder, Some(PathBuf::from("/tmp/a b+c#d")));
    assert_eq!(
        call.request.filters,
        vec![
            dtr_portal::Filter::new("Images", ["*.png".to_owned()]),
            dtr_portal::Filter::new("Text", ["*.txt".to_owned()]),
        ]
    );
    assert_eq!(
        call.filter,
        Some(0),
        "current_filter index into the picker's list"
    );
    assert_eq!(call.window.app_id, "org.example.App");
    assert_eq!(call.window.parent_window, "x11:0x1234");
    assert_eq!(call.window.title, "Open a file");
    assert!(!call.window.modal);

    assert!(call.reply.send(Answer::Chosen(Chosen {
        paths: vec![PathBuf::from("/tmp/photo photo+#1.png")],
        filter: Some(0),
    })));

    let mut results = wait(waiting).expect("response and results");
    assert_eq!(results.0, 0);
    let uris = uris(&mut results.1);
    assert_eq!(
        uris,
        vec!["file:///tmp/photo%20photo%2B%231.png".to_owned()]
    );
    assert!(!writable(&mut results.1), "an open is read-only");
    let (name, patterns) = current_filter(&mut results.1);
    assert_eq!(name, "Images");
    assert_eq!(
        patterns,
        vec![(0, "*.png".to_owned()), (1, "image/png".to_owned())],
        "the row is echoed as the application sent it"
    );
    assert!(
        results.1.is_empty(),
        "no unexpected result keys: {results:?}"
    );
}

/// "Choose a folder": `directory` beats `multiple`, and the answer is the folder
/// on screen.
#[test]
fn a_directory_request_answers_the_folder_being_shown() {
    let Some(fixture) = Fixture::start() else {
        eprintln!("no dbus-daemon on PATH; skipping the file chooser portal test");
        return;
    };
    let handle = fixture.handle("folder1");
    let waiting = spawn_call(
        fixture.client.clone(),
        OPEN_FILE,
        handle,
        options(vec![
            ("directory", a_bool(true)),
            ("multiple", a_bool(true)),
            ("current_folder", bytes(b"/home/barnabas".to_vec())),
        ]),
    );
    let call = fixture.next_call();
    assert_eq!(call.request.select, Select::Directory);
    assert_eq!(call.request.folder, Some(PathBuf::from("/home/barnabas")));
    assert!(call.reply.send(Answer::Chosen(Chosen {
        paths: vec![PathBuf::from("/home/barnabas/Documents")],
        filter: None,
    })));

    let mut wire = wait(waiting).expect("response and results");
    assert_eq!(wire.0, 0);
    assert_eq!(
        uris(&mut wire.1),
        vec!["file:///home/barnabas/Documents".to_owned()]
    );
    assert!(
        !wire.1.contains_key("current_filter"),
        "no filter was offered"
    );
}

/// Several files: the order the picker answered is the order handed back.
#[test]
fn a_multiple_open_answers_every_chosen_file() {
    let Some(fixture) = Fixture::start() else {
        eprintln!("no dbus-daemon on PATH; skipping the file chooser portal test");
        return;
    };
    let handle = fixture.handle("many1");
    let waiting = spawn_call(
        fixture.client.clone(),
        OPEN_FILE,
        handle,
        options(vec![("multiple", a_bool(true))]),
    );
    let call = fixture.next_call();
    assert_eq!(call.request.select, Select::Files);
    assert!(call.reply.send(Answer::Chosen(Chosen {
        paths: vec![PathBuf::from("/tmp/b.txt"), PathBuf::from("/tmp/a.txt"),],
        filter: None,
    })));
    let mut wire = wait(waiting).expect("response and results");
    assert_eq!(wire.0, 0);
    assert_eq!(
        uris(&mut wire.1),
        vec![
            "file:///tmp/b.txt".to_owned(),
            "file:///tmp/a.txt".to_owned()
        ]
    );
}

/// `SaveFile`: the typed name and folder are the question, and the answer is one
/// URI and `writable: true`.
#[test]
fn a_save_file_answers_the_typed_name() {
    let Some(fixture) = Fixture::start() else {
        eprintln!("no dbus-daemon on PATH; skipping the file chooser portal test");
        return;
    };
    let handle = fixture.handle("save1");
    let waiting = spawn_call(
        fixture.client.clone(),
        "SaveFile",
        handle,
        options(vec![
            ("current_name", a_string("report")),
            ("current_folder", bytes(b"/tmp/drafts".to_vec())),
        ]),
    );
    let call = fixture.next_call();
    assert_eq!(call.request.select, Select::Save);
    assert_eq!(call.request.name, "report");
    assert_eq!(call.request.accept, "save");
    assert!(call.reply.send(Answer::Chosen(Chosen {
        paths: vec![PathBuf::from("/tmp/drafts/report(1).pdf")],
        filter: None,
    })));
    let mut wire = wait(waiting).expect("response and results");
    assert_eq!(wire.0, 0);
    assert_eq!(
        uris(&mut wire.1),
        vec!["file:///tmp/drafts/report%281%29.pdf".to_owned()]
    );
    assert!(writable(&mut wire.1), "a save is writable");
}

/// `SaveFiles`: the names in `options.files` are appended to the chosen folder,
/// and one that is taken gains a number rather than being overwritten.
#[test]
fn a_save_files_answer_appends_the_names_to_the_folder() {
    let Some(fixture) = Fixture::start() else {
        eprintln!("no dbus-daemon on PATH; skipping the file chooser portal test");
        return;
    };
    let dir = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(dir.path().join("taken.txt"), b"already here").expect("write");
    let handle = fixture.handle("savemany1");
    let waiting = spawn_call(
        fixture.client.clone(),
        "SaveFiles",
        handle,
        options(vec![
            (
                "current_folder",
                bytes(dir.path().as_os_str().as_encoded_bytes().to_vec()),
            ),
            (
                "files",
                file_names(vec![b"taken.txt".to_vec(), b"new.txt".to_vec()]),
            ),
        ]),
    );
    let call = fixture.next_call();
    assert_eq!(call.request.select, Select::Directory);
    assert_eq!(call.request.accept, "save");
    assert!(call.reply.send(Answer::Chosen(Chosen {
        paths: vec![dir.path().to_path_buf()],
        filter: None,
    })));
    let mut wire = wait(waiting).expect("response and results");
    assert_eq!(wire.0, 0);
    let uris = uris(&mut wire.1);
    assert!(
        uris[0].ends_with("/taken%281%29.txt"),
        "an existing name is not overwritten (parentheses percent-encoded): {uris:?}"
    );
    assert!(uris[1].ends_with("/new.txt"), "{uris:?}");
    // Every URI is the folder the test chose, percent-encoded as a path.
    let folder = dir.path().to_string_lossy();
    let folder = folder.trim_start_matches('/');
    assert!(
        uris.iter()
            .all(|uri| uri.starts_with(&format!("file:///{folder}/"))),
        "{uris:?}"
    );
}

/// `Request.Close()` while the picker is pending: the method answers cancel, the
/// host is told to take the window down, and a second `Close` says the
/// interaction is over.
#[test]
fn a_close_while_pending_answers_cancel_and_closes_the_window() {
    let Some(fixture) = Fixture::start() else {
        eprintln!("no dbus-daemon on PATH; skipping the file chooser portal test");
        return;
    };
    let handle = fixture.handle("close1");
    let waiting = spawn_call(
        fixture.client.clone(),
        OPEN_FILE,
        handle.clone(),
        HashMap::new(),
    );
    let call = fixture.next_call();
    let id = call.id.clone();
    // The picker is deliberately not answered: this is the person still looking.
    assert_eq!(call.request.select, Select::File);

    let close = |path: &str| {
        pollster::block_on(async {
            let proxy =
                zbus::Proxy::new(&fixture.client, SERVICE_NAME, path, REQUEST_INTERFACE).await?;
            proxy.call_method("Close", &()).await
        })
    };

    // The request object exists while the request is pending, with the one
    // method the XML defines and no arguments.
    let xml = pollster::block_on(async {
        let proxy = zbus::Proxy::new(
            &fixture.client,
            SERVICE_NAME,
            handle.as_str(),
            "org.freedesktop.DBus.Introspectable",
        )
        .await
        .expect("an introspection proxy");
        let message = proxy
            .call_method("Introspect", &())
            .await
            .expect("introspection");
        message
            .body()
            .deserialize::<String>()
            .expect("introspection XML")
    });
    assert!(
        method_arg_types(&xml, "Close").is_empty(),
        "Request.Close takes no arguments: {xml}"
    );

    close(handle.as_str()).expect("the frontend closes the pending request");
    let wire = wait(waiting).expect("a response to the closed request");
    assert_eq!(wire.0, 1, "a close answers cancel");
    assert!(wire.1.is_empty(), "a cancel carries no results");
    fixture.expect_closed(&id);

    // The interaction is over: the object is gone, and the second Close is an
    // error rather than a request answered twice.
    assert!(
        close(handle.as_str()).is_err(),
        "the request object outlived its request"
    );
}

/// A request the backend cannot show is refused, not left waiting for a reply
/// the frontend will never send.
#[test]
fn a_malformed_option_is_refused_instead_of_hanging() {
    let Some(fixture) = Fixture::start() else {
        eprintln!("no dbus-daemon on PATH; skipping the file chooser portal test");
        return;
    };
    let handle = fixture.handle("broken1");
    let waiting = spawn_call(
        fixture.client.clone(),
        OPEN_FILE,
        handle,
        // `filters` is `a(sa(us))`; a boolean is a frontend bug.
        options(vec![("filters", a_bool(true))]),
    );
    match wait(waiting) {
        Err(zbus::Error::MethodError(name, message, _)) => {
            assert_eq!(
                name.as_str(),
                "org.freedesktop.DBus.Error.InvalidArgs",
                "{message:?}"
            );
        }
        other => panic!("expected InvalidArgs, got {other:?}"),
    }
}

/// Nothing was asked of the host, and no request object was exported: a malformed
/// request must not leave a `Close` target behind either.
#[test]
fn a_malformed_request_never_reaches_the_host() {
    let Some(fixture) = Fixture::start() else {
        eprintln!("no dbus-daemon on PATH; skipping the file chooser portal test");
        return;
    };
    let handle = fixture.handle("broken2");
    let waiting = spawn_call(
        fixture.client.clone(),
        OPEN_FILE,
        handle.clone(),
        options(vec![("current_folder", a_bool(true))]),
    );
    assert!(wait(waiting).is_err(), "the call is refused");
    assert!(
        matches!(
            fixture.opened.try_recv(),
            Err(async_channel::TryRecvError::Empty)
        ),
        "a malformed request is not shown to anybody"
    );
    // And there is no request object at the handle.
    let introspected = pollster::block_on(async {
        let proxy = zbus::Proxy::new(
            &fixture.client,
            SERVICE_NAME,
            handle.as_str(),
            "org.freedesktop.DBus.Introspectable",
        )
        .await?;
        proxy.call_method("Introspect", &()).await
    });
    assert!(
        introspected.is_err(),
        "no object should exist at the handle of a refused request"
    );
}

#[test]
fn the_save_files_uniquifier_is_bounded_and_keeps_the_first_dot() {
    // The unit half lives in `paths.rs`; this is the one case an integration
    // test can see (the names' first dot), written beside the wire rules it
    // belongs to.
    let dir = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(dir.path().join("archive.tar.gz"), b"already here").expect("write");
    assert_eq!(
        dtr_portal::unique_path(&dir.path().join("archive.tar.gz")),
        Path::new(dir.path()).join("archive(1).tar.gz").as_path()
    );
}

/// The shipped daemon, not just the library: `xdg-desktop-portal` runs the
/// binary, and if it cannot own the bus name the frontend gets a service error
/// rather than a dialog. Until the window lands its host answers cancel, so a
/// request must come back with response 1 — and with a real answer rather than a
/// hang, which is the failure mode a broken `main` would have.
#[test]
fn the_daemon_binary_serves_the_interface() {
    let Some(bus) = private_bus() else {
        eprintln!("no dbus-daemon on PATH; skipping the file chooser portal test");
        return;
    };

    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let child = Child(
        std::process::Command::new(env!("CARGO_BIN_EXE_dtr-portal"))
            .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("the dtr-portal binary starts"),
    );

    let client = pollster::block_on(async {
        let builder = zbus::connection::Builder::address(bus.address.as_str()).expect("address");
        builder.build().await.expect("a client connection")
    });

    // The daemon has to finish its handshake before it owns the name; poll the
    // bus rather than sleeping a guessed interval.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let owned = pollster::block_on(async {
            let proxy = zbus::Proxy::new(
                &client,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            )
            .await?;
            proxy
                .call::<_, _, (bool,)>("NameHasOwner", &(SERVICE_NAME,))
                .await
        })
        .map(|(owner,)| owner)
        .unwrap_or(false);
        if owned {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon never owned {SERVICE_NAME}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let handle = OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/request/1_1/binary1")
        .expect("a request path");
    let waiting = spawn_call(
        client.clone(),
        OPEN_FILE,
        handle,
        options(vec![("multiple", a_bool(false))]),
    );
    let wire = wait(waiting).expect("the daemon answers its first request");
    assert_eq!(wire.0, 1, "the placeholder host answers cancel, visibly");
    assert!(wire.1.is_empty());
    drop(child);
}
