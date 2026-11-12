//! Where a program's notification goes.
//!
//! `OSC 9` and `OSC 777` ask the *person* to see something — a build finished, a
//! download failed, an agent wants approval. There are two surfaces that can say
//! so: the desktop's own notification channel, and dtr's window. Which one is
//! right is a property of the machine rather than of the program, so the app asks
//! a [`Notifier`] and falls back to the in-app toast when the answer is
//! [`Delivered::No`]. The toast is deliberately *not* a `Notifier`: it is the one
//! fallback every platform shares, and a notifier that drew a toast would let a
//! platform's channel quietly become the only channel.
//!
//! One implementation exists, on Linux: `org.freedesktop.Notifications` on the
//! session bus, which is the daemon the desktop already runs. It is behind the
//! `portal` feature because it needs the same D-Bus stack the settings portal
//! does.
//!
//! Windows has none yet, on purpose. A toast is not a call the way `Notify` is:
//! an unpackaged desktop app may only raise one after it has written a Start Menu
//! shortcut carrying an AppUserModelID, and a `Notifier` that silently wrote into
//! somebody's Start Menu is a side effect that belongs in a decision, not in a
//! fallback path. Until that decision is made the in-app toast is Windows'
//! channel, and this says so by answering `No`.

pub use crate::pane::Notice;

/// Whether the notice went somewhere the person will see outside this window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivered {
    Yes,
    No,
}

/// The channel a notice can leave dtr on.
pub trait Notifier: Send + Sync {
    /// Hand the notice over, and say whether it was taken.
    ///
    /// Called on the UI thread, so the implementation must not block: the call is
    /// a D-Bus round trip, and it belongs on the thread that owns the connection.
    fn notify(&self, notice: &Notice) -> Delivered;
}

/// No desktop channel: the in-app toast is the answer.
///
/// This is what Windows gets, and what every build without D-Bus gets. It is a
/// `Notifier` rather than an `Option` so the app has one path: ask, and show the
/// toast only when the answer is no.
pub struct NoDesktop;

impl Notifier for NoDesktop {
    fn notify(&self, _notice: &Notice) -> Delivered {
        Delivered::No
    }
}

/// The channel this build and this machine offer.
///
/// `enabled` is `[notifications] os`: off means the user asked for dtr's own
/// toast and nothing else, which is also what happens when there is no channel to
/// use.
pub fn system(enabled: bool) -> std::sync::Arc<dyn Notifier> {
    if !enabled {
        return std::sync::Arc::new(NoDesktop);
    }
    #[cfg(all(target_os = "linux", feature = "portal"))]
    {
        linux::spawn(None)
    }
    #[cfg(not(all(target_os = "linux", feature = "portal")))]
    {
        std::sync::Arc::new(NoDesktop)
    }
}

#[cfg(all(target_os = "linux", feature = "portal"))]
mod linux {
    use super::{Delivered, NoDesktop, Notice, Notifier};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The name every desktop's notification daemon owns. There is no fallback
    /// service, so no owner means no channel.
    const SERVICE: &str = "org.freedesktop.Notifications";
    const PATH: &str = "/org/freedesktop/Notifications";
    const IFACE: &str = "org.freedesktop.Notifications";
    /// What dtr calls itself in the notification, and the hint that lets a
    /// desktop attribute it to the installed `dtr.desktop`. The app name is what
    /// the daemon shows when the program sent no title of its own.
    const APP_NAME: &str = "dtr";
    /// How long a notice stays up, in milliseconds. `-1` would be the daemon's
    /// default, which differs per desktop; three seconds is short for a line of
    /// text, and this is the one number in here that is a taste decision.
    const EXPIRE_MS: i32 = 6000;

    /// A channel that owns its connection on a thread of its own.
    ///
    /// The connection is a D-Bus round trip, and the app calls this from the
    /// thread that draws: so the work moves to a thread that can afford it, and
    /// `available` is what the app reads without waiting.
    pub(super) struct Desktop {
        notices: async_channel::Sender<Notice>,
        available: Arc<AtomicBool>,
        /// Held so the thread is named and visible here; dropping the sender is
        /// what ends it, and dropping this handle does not join (a notice in
        /// flight is not worth blocking the window's close).
        _thread: std::thread::JoinHandle<()>,
    }

    impl Notifier for Desktop {
        fn notify(&self, notice: &Notice) -> Delivered {
            if !self.available.load(Ordering::Relaxed) {
                return Delivered::No;
            }
            match self.notices.try_send(notice.clone()) {
                Ok(()) => Delivered::Yes,
                Err(err) => {
                    // The queue is unbounded, so this is a closed thread.
                    tracing::debug!(%err, "no notification thread");
                    Delivered::No
                }
            }
        }
    }

    impl Desktop {
        /// Whether the thread has a daemon on the other end right now. The app
        /// reads the same flag through `notify`; this is the seam the test waits
        /// on, because "connected yet" is not observable from a `Delivered`.
        #[cfg(test)]
        fn available(&self) -> bool {
            self.available.load(Ordering::Relaxed)
        }
    }

    /// Start the channel. `address` is a bus to use instead of the session's,
    /// which exists for the test that runs its own daemon on a private bus.
    pub(super) fn spawn(address: Option<String>) -> Arc<dyn Notifier> {
        match spawn_desktop(address) {
            Ok(desktop) => Arc::new(desktop),
            Err(err) => {
                tracing::warn!(%err, "could not start the notification thread");
                Arc::new(NoDesktop)
            }
        }
    }

    /// The same, as the concrete type: a test wants the thread it is looking at,
    /// and the availability probe the app only reads through `notify`.
    #[cfg(test)]
    pub(super) fn spawn_at(address: Option<String>) -> std::io::Result<Desktop> {
        spawn_desktop(address)
    }

    fn spawn_desktop(address: Option<String>) -> std::io::Result<Desktop> {
        let (notices, inbox) = async_channel::unbounded::<Notice>();
        let available = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&available);
        let socket = address.clone();
        let thread = std::thread::Builder::new()
            .name("dtr-notify".to_owned())
            .spawn(move || {
                let Ok(runtime) = compio::runtime::Runtime::new() else {
                    tracing::error!("no runtime for notifications");
                    return;
                };
                runtime.block_on(run(inbox, flag, socket));
            })?;
        Ok(Desktop {
            notices,
            available,
            _thread: thread,
        })
    }

    async fn run(
        inbox: async_channel::Receiver<Notice>,
        available: Arc<AtomicBool>,
        address: Option<String>,
    ) {
        // Connected before the first notice, not after it: the app asks
        // `available` to decide between the desktop and the toast, and a channel
        // that only exists once it has been used would send the first notice to
        // the wrong surface.
        let mut connection = connect(address.as_deref()).await;
        available.store(connection.is_some(), Ordering::Relaxed);
        if connection.is_none() {
            tracing::debug!("no notification daemon; notices go to the window");
        }
        while let Ok(notice) = inbox.recv().await {
            if connection.is_none() {
                // A daemon that was not running may have started since.
                connection = connect(address.as_deref()).await;
                available.store(connection.is_some(), Ordering::Relaxed);
            }
            let Some(bus) = connection.as_ref() else {
                continue;
            };
            match send(bus, &notice).await {
                Ok(()) => tracing::debug!(title = %notice.title, "notification sent"),
                Err(err) => {
                    // A bus that answered once and not the second time is gone:
                    // drop it, say so, and let the next notice try again. The app
                    // shows the toast while `available` is false.
                    tracing::warn!(%err, "could not send a notification");
                    connection = None;
                    available.store(false, Ordering::Relaxed);
                }
            }
        }
    }

    /// A connection with a notification daemon on the other end.
    ///
    /// The daemon is asked something harmless rather than asked *about*: on a
    /// normal desktop `org.freedesktop.Notifications` has no owner until the
    /// first call, because the bus starts it on demand — a `NameHasOwner` check
    /// says "no daemon" on a running GNOME session. `GetCapabilities` starts the
    /// service, answers, and shows nothing to the person.
    async fn connect(address: Option<&str>) -> Option<zbus::Connection> {
        let connection = match address {
            Some(address) => {
                let builder = zbus::connection::Builder::address(address).ok()?;
                builder.build().await.ok()?
            }
            None => zbus::Connection::session().await.ok()?,
        };
        let proxy = zbus::Proxy::new(&connection, SERVICE, PATH, IFACE)
            .await
            .ok()?;
        match proxy.call_method("GetCapabilities", &()).await {
            Ok(_) => Some(connection),
            Err(err) => {
                tracing::debug!(%err, "no notification daemon answered");
                None
            }
        }
    }

    async fn send(connection: &zbus::Connection, notice: &Notice) -> zbus::Result<()> {
        let proxy = zbus::Proxy::new(connection, SERVICE, PATH, IFACE).await?;
        let (summary, body) = parts(notice);
        let mut hints = std::collections::HashMap::new();
        hints.insert(
            "desktop-entry".to_owned(),
            zbus::zvariant::Value::from("dtr"),
        );
        // `replaces_id` is 0 on purpose: two notices from two tabs are two
        // notices, and collapsing them would hide one of the programs.
        proxy
            .call_method(
                "Notify",
                &(
                    APP_NAME,
                    0u32,
                    icon(),
                    summary.as_str(),
                    body.as_str(),
                    Vec::<String>::new(),
                    hints,
                    EXPIRE_MS,
                ),
            )
            .await?;
        Ok(())
    }

    /// The summary line and the body.
    ///
    /// `OSC 9` carries a body and no title, and `OSC 777` may carry either. A
    /// notification with an empty summary is drawn as an empty line by most
    /// daemons, so the app's own name takes the line the program did not fill.
    fn parts(notice: &Notice) -> (String, String) {
        let summary = if notice.title.is_empty() {
            APP_NAME.to_owned()
        } else {
            notice.title.clone()
        };
        let body = if notice.body.is_empty() {
            notice.title.clone()
        } else {
            notice.body.clone()
        };
        (summary, body)
    }

    /// The themed icon name.
    ///
    /// The package installs the marks under `dtr` in hicolor and
    /// `assets/dtr.desktop` names them. A development build has no theme entry,
    /// and the daemon then draws its own placeholder — better than an absolute
    /// path into a build directory, which is wrong on every other machine.
    fn icon() -> String {
        "dtr".to_owned()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::{Arc, Mutex};
        use std::time::{Duration, Instant};

        /// A private session bus, so the test neither depends on nor disturbs the
        /// desktop's own. `dbus-daemon` is not guaranteed to be installed — the
        /// Nix check may not ship it — so a test that cannot find it returns:
        /// skipping is the honest answer, a red suite for a missing tool is not.
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

        /// What the daemon saw: the three arguments dtr is responsible for.
        type Seen = Arc<Mutex<Vec<(String, String, String)>>>;

        struct Daemon {
            seen: Seen,
        }

        /// The stub daemon's D-Bus surface. Only `Notify` matters here, but a
        /// real daemon answers `GetCapabilities` too and a client that got it
        /// wrong should fail this test rather than the desktop.
        #[zbus::interface(name = "org.freedesktop.Notifications")]
        impl Daemon {
            #[allow(clippy::too_many_arguments)]
            fn notify(
                &self,
                app_name: String,
                _replaces_id: u32,
                _app_icon: String,
                summary: String,
                body: String,
                _actions: Vec<String>,
                _hints: std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
                _expire_timeout: i32,
            ) -> u32 {
                if let Ok(mut seen) = self.seen.lock() {
                    seen.push((app_name, summary, body));
                }
                1
            }

            fn get_capabilities(&self) -> Vec<String> {
                vec!["body".to_owned()]
            }
        }

        /// Serve the stub on `bus`, and answer once the name is owned — the
        /// notifier connects eagerly, so its connect must not race the daemon.
        fn serve(bus: &Bus, seen: Seen) -> std::sync::mpsc::Receiver<()> {
            let (ready, waiting) = std::sync::mpsc::channel();
            let address = bus.address.clone();
            std::thread::spawn(move || {
                let Ok(runtime) = compio::runtime::Runtime::new() else {
                    return;
                };
                runtime.block_on(async move {
                    let Ok(builder) = zbus::connection::Builder::address(address.as_str()) else {
                        return;
                    };
                    let Ok(connection) = builder
                        .name(SERVICE)
                        .expect("a service name")
                        .serve_at(PATH, Daemon { seen })
                        .expect("the daemon object")
                        .build()
                        .await
                    else {
                        return;
                    };
                    let _ = ready.send(());
                    // Serve until the process ends; the connection must stay in
                    // scope, which is what this line is for.
                    std::future::pending::<()>().await;
                    drop(connection);
                });
            });
            waiting
        }

        /// The wire, end to end: a real bus, a stub daemon, and the arguments dtr
        /// is responsible for.
        #[test]
        fn a_notice_reaches_the_notification_daemon() {
            let Some(bus) = private_bus() else {
                eprintln!("no dbus-daemon on PATH; skipping the notification test");
                return;
            };
            let seen: Seen = Arc::new(Mutex::new(Vec::new()));
            let ready = serve(&bus, Arc::clone(&seen));
            assert!(
                ready.recv_timeout(Duration::from_secs(10)).is_ok(),
                "the stub daemon never started"
            );

            let desktop = spawn_at(Some(bus.address.clone())).expect("a notification thread");
            let deadline = Instant::now() + Duration::from_secs(10);
            while !desktop.available() {
                assert!(Instant::now() < deadline, "the notifier never connected");
                std::thread::sleep(Duration::from_millis(10));
            }

            let notice = Notice {
                title: "cargo".to_owned(),
                body: "tests passed".to_owned(),
            };
            assert_eq!(desktop.notify(&notice), Delivered::Yes);

            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let first = seen.lock().ok().and_then(|seen| seen.first().cloned());
                if let Some(first) = first {
                    assert_eq!(
                        first,
                        (
                            "dtr".to_owned(),
                            "cargo".to_owned(),
                            "tests passed".to_owned()
                        )
                    );
                    return;
                }
                assert!(Instant::now() < deadline, "the daemon never saw the notice");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        /// A bus with no notification daemon on it — a bare session, or this
        /// sandbox — must answer `No`, which is what makes the app draw its own
        /// toast. Whether the connect has finished or not, the answer is the same.
        #[test]
        fn with_no_daemon_the_notice_stays_in_the_window() {
            let Some(bus) = private_bus() else {
                eprintln!("no dbus-daemon on PATH; skipping the notification test");
                return;
            };
            let desktop = spawn_at(Some(bus.address.clone())).expect("a notification thread");
            let notice = Notice {
                title: "cargo".to_owned(),
                body: "tests passed".to_owned(),
            };
            assert_eq!(desktop.notify(&notice), Delivered::No);
            // And it stays that way: a bus that has no daemon does not grow one.
            std::thread::sleep(Duration::from_millis(200));
            assert_eq!(desktop.notify(&notice), Delivered::No);
        }

        /// The pure half: what the daemon sees for each of the two protocols.
        #[test]
        fn the_app_name_fills_the_line_the_program_left_empty() {
            let osc9 = Notice {
                title: String::new(),
                body: "build finished".to_owned(),
            };
            assert_eq!(
                parts(&osc9),
                ("dtr".to_owned(), "build finished".to_owned())
            );

            let osc777 = Notice {
                title: "cargo".to_owned(),
                body: "tests passed".to_owned(),
            };
            assert_eq!(
                parts(&osc777),
                ("cargo".to_owned(), "tests passed".to_owned())
            );
        }
    }
}
