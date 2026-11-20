//! The remote-listing protocol: request, batches, one reply, the right pane.
//!
//! A remote directory is walked by `Backend::list`, which answers a channel of
//! batches (a listing is streamed; dropping the receiver cancels the walk). A
//! [`Browser`] is written for *one* whole reply — [`Browser::apply_listing`]
//! sorts and adopts the rows once — so a frontend coalesces the batches and then
//! hands the result to the pane that asked. That coalescing, the "which pane
//! asked" routing and the stale-reply rule are the same in the terminal app and
//! in the portal's manager window, and they are here.
//!
//! The two frontends run this on different machinery on purpose:
//!
//! - The terminal drains [`Listing::drain`] from its frame loop, keeping every
//!   walk in a [`Pump`] and applying what ended each frame. No runtime on the
//!   drawing thread, and the batches arrive whenever they arrive.
//! - The portal's manager owns one browser and a worker thread, so it awaits
//!   [`collect`] and posts the finished listing back as an event. Same protocol,
//!   one thread rather than a frame.
//!
//! **The stale-reply rule is the routing's, not the browser's.** A reply names
//! the machine and the directory it answers; two panes can browse one machine,
//! and `Browser::reload` leaves `awaiting` pointing at the directory the pane
//! just left while the new request is still pending — so [`waiter`] matches by
//! host *and* by directory *and* by `is_waiting_for`, and a reply that lost the
//! race is dropped rather than applied.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dtr_files::{Browser, Entry};
use dtr_vfs::{Backend, Batches};

/// One remote walk, coalescing its batches into the whole listing a browser
/// takes.
pub struct Listing {
    host: String,
    path: PathBuf,
    batches: Batches,
    entries: Vec<Entry>,
    /// The error the stream ended with, if it ended with one. A walk that fails
    /// halfway still has rows worth showing: they are applied and the message
    /// goes in the footer.
    failure: Option<String>,
}

impl Listing {
    /// Start coalescing `batches`, which came from `host`'s backend for `path`.
    pub fn new(host: impl Into<String>, path: impl Into<PathBuf>, batches: Batches) -> Self {
        Self {
            host: host.into(),
            path: path.into(),
            batches,
            entries: Vec::new(),
            failure: None,
        }
    }

    /// The machine whose backend is walking.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The directory being listed.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Take every batch that is ready.
    ///
    /// Returns `true` when the stream has ended — a burst of 157 batches costs
    /// one merge, the same rule the local listing's poll follows — and the caller
    /// then applies what arrived via [`Self::finish`]. Every batch so far is
    /// coalesced into one `Vec`, because `apply_listing` is the whole-listing
    /// answer the pane's handshake is written for.
    pub fn drain(&mut self) -> bool {
        loop {
            match self.batches.try_recv() {
                Ok(batch) => {
                    if self.absorb(batch) {
                        return true;
                    }
                }
                Err(async_channel::TryRecvError::Empty) => return false,
                Err(async_channel::TryRecvError::Closed) => return true,
            }
        }
    }

    /// Take one batch and say whether the walk has ended.
    fn absorb(&mut self, batch: Result<Vec<dtr_vfs::Entry>, anyhow::Error>) -> bool {
        match batch {
            Ok(rows) => {
                self.entries.extend(rows.into_iter().map(Entry::from));
                false
            }
            Err(err) => {
                self.failure = Some(format!("{err:#}"));
                true
            }
        }
    }

    /// The listing a browser can adopt, once [`Self::drain`] said it ended.
    pub fn finish(self) -> FinishedListing {
        FinishedListing {
            host: self.host,
            path: self.path,
            entries: self.entries,
            failure: self.failure,
        }
    }
}

/// A whole remote listing: what a browser applies in one call.
#[derive(Debug)]
pub struct FinishedListing {
    /// The machine whose backend walked.
    pub host: String,
    /// The directory asked for and answered.
    pub path: PathBuf,
    /// The rows that arrived. A walk that failed halfway still has these.
    pub entries: Vec<Entry>,
    /// Why the walk ended early, for the pane's footer.
    pub failure: Option<String>,
}

impl FinishedListing {
    /// Apply the rows, then the failure if the walk ended with one.
    ///
    /// Rows that did arrive are still worth showing — the far side has them, and
    /// an empty pane would read as an empty directory — so the footer message
    /// comes second and does not erase the listing.
    pub fn apply(self, browser: &mut Browser) {
        let Self {
            path,
            entries,
            failure,
            ..
        } = self;
        browser.apply_listing(path, entries);
        if let Some(message) = failure {
            browser.apply_failure(message);
        }
    }
}

/// Drive one walk to its end on the caller's runtime.
///
/// For a frontend that has a worker thread rather than a frame to poll from: the
/// ordering, the coalescing and the failure rule are the same as
/// [`Listing::drain`]'s, so both frontends cannot disagree about what a listing
/// is.
pub async fn collect(host: String, path: PathBuf, batches: Batches) -> FinishedListing {
    let mut listing = Listing::new(host, path, batches);
    loop {
        match listing.batches.recv().await {
            Ok(batch) => {
                if listing.absorb(batch) {
                    break;
                }
            }
            // The sender is gone: the walk is over.
            Err(_) => break,
        }
    }
    listing.finish()
}

/// The pane that should receive `host`'s listing of `path`, if there is one.
///
/// Two panes can browse one machine, so the match is by host *and* by the
/// directory the pane asked for — not by the focus. The directory check is the
/// stale-reply guard as well: `Browser::reload` leaves `awaiting` pointing at the
/// old directory while the new request is still pending, so a reply to the
/// directory the pane just left would still look like an answer. The pane is
/// showing the directory it asked for the whole time the walk runs, so requiring
/// `directory() == path` accepts live replies and drops the ones that lost a race
/// with a navigation.
pub fn waiter<'a>(
    browsers: impl IntoIterator<Item = &'a mut Browser>,
    host: &str,
    path: &Path,
) -> Option<&'a mut Browser> {
    browsers.into_iter().find(|browser| {
        browser.alias() == Some(host) && browser.directory() == path && browser.is_waiting_for(path)
    })
}

/// Every remote walk a frame-driven frontend has in flight.
///
/// One `Pump` per window (the terminal app has one for all its tabs and panes):
/// walk results are routed to whichever pane asked, not to the pump's owner.
#[derive(Default)]
pub struct Pump {
    listings: Vec<Listing>,
}

impl Pump {
    /// A pump with no walks yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a walk is still running, which is a state with no event to
    /// announce it: the caller's frame loop asks this to keep frames coming.
    pub fn is_empty(&self) -> bool {
        self.listings.is_empty()
    }

    /// Start a walk of `host`'s `path` on `backend`.
    ///
    /// The backend is the caller's: a frontend resolves it from its own connection
    /// pool, so the listing comes from the same connection the pane's writes use.
    pub fn start(&mut self, host: impl Into<String>, path: PathBuf, backend: &Arc<dyn Backend>) {
        let batches = backend.list(&path);
        self.listings.push(Listing::new(host, path, batches));
    }

    /// Drain the walks that ended and apply each to the pane that asked for it.
    ///
    /// A finished walk with no pane waiting is returned rather than dropped
    /// silently, so the caller can say in its log why a directory arrived for
    /// nobody. Applying is the only model change here; the pump never navigates.
    pub fn deliver<'a>(
        &mut self,
        browsers: impl IntoIterator<Item = &'a mut Browser>,
    ) -> Vec<FinishedListing> {
        let mut finished = Vec::new();
        let mut index = 0;
        while index < self.listings.len() {
            if self.listings[index].drain() {
                finished.push(self.listings.remove(index).finish());
            } else {
                index += 1;
            }
        }

        let mut browsers: Vec<&mut Browser> = browsers.into_iter().collect();
        let mut undelivered = Vec::new();
        for listing in finished {
            let found = waiter(
                browsers.iter_mut().map(|browser| &mut **browser),
                &listing.host,
                &listing.path,
            );
            match found {
                Some(browser) => listing.apply(browser),
                None => undelivered.push(listing),
            }
        }
        undelivered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtr_vfs::Memory;
    use std::time::{Duration, Instant};

    fn browser(host: &str, path: &str) -> Browser {
        let mut browser = Browser::remote_at(host, PathBuf::from(path));
        browser.set_caps(dtr_vfs::Caps {
            watch: false,
            dir_size: false,
            write: true,
            trash: false,
        });
        browser
    }

    fn memory() -> Arc<Memory> {
        let mem = Arc::new(Memory::new());
        mem.write("/lab/alpha.txt", b"hello");
        mem.dir("/lab/sub");
        mem
    }

    /// The handshake's routing is by host *and* directory, and a listing that
    /// lost a race with a navigation is not applied to the directory the pane is
    /// showing now.
    #[test]
    fn a_listing_goes_to_the_pane_waiting_for_it_and_not_one_that_moved_on() {
        let mut build = Browser::remote_at("build", PathBuf::from("/srv/old"));
        let mut other = Browser::remote_at("other", PathBuf::from("/srv/old"));
        assert_eq!(
            build.take_request(),
            Some(("build".to_owned(), PathBuf::from("/srv/old")))
        );
        assert_eq!(
            other.take_request(),
            Some(("other".to_owned(), PathBuf::from("/srv/old")))
        );

        // Two panes can browse one directory, and one host can be browsed twice:
        // only the host the listing names is the destination.
        let picked = waiter([&mut build, &mut other], "other", Path::new("/srv/old"))
            .expect("the pane on that host is waiting");
        assert_eq!(picked.alias(), Some("other"));

        // Navigating leaves the walk's name in `awaiting` — a second request has
        // not been taken yet — but the pane is showing another directory.
        build.navigate(PathBuf::from("/srv/new"));
        assert!(
            build.is_waiting_for(Path::new("/srv/old")),
            "the handshake still names the walk it started"
        );
        assert!(
            waiter([&mut build], "build", Path::new("/srv/old")).is_none(),
            "a reply to the directory the pane left must be dropped, not applied"
        );
        assert_eq!(
            build.take_request(),
            Some(("build".to_owned(), PathBuf::from("/srv/new"))),
            "the new directory is what it asks for next"
        );
        assert!(
            waiter([&mut build], "build", Path::new("/srv/new")).is_some(),
            "and that request is the one a reply is matched against"
        );
    }

    /// Request -> the pump starts the walk -> a frame drains it -> the pane that
    /// asked gets the whole listing, applied once.
    #[compio::test]
    async fn a_pump_lists_a_remote_directory_and_hands_it_to_its_pane() {
        let mem = memory();
        let backend: Arc<dyn Backend> = mem.clone();
        let mut pane = Browser::remote_at("mem", PathBuf::from("/lab"));
        pane.set_caps(backend.caps());
        let (host, path) = pane
            .take_request()
            .expect("a remote pane asks for its directory");

        let mut pump = Pump::new();
        pump.start(host.clone(), path.clone(), &backend);
        assert!(!pump.is_empty(), "a walk is in flight");

        // `Memory`'s walk runs on the runtime's thread, so let it finish the way a
        // frame would: drain until the listed directories are applied.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !pump.is_empty() {
            assert!(
                Instant::now() < deadline,
                "the in-memory walk never finished"
            );
            compio::time::sleep(Duration::from_millis(1)).await;
            let undelivered = pump.deliver(std::iter::once(&mut pane));
            assert!(undelivered.is_empty(), "the pane was waiting for this");
        }
        assert_eq!(pane.directory(), Path::new("/lab"));
        assert!(
            !pane.is_waiting_for(Path::new("/lab")),
            "the listing landed and the handshake is closed"
        );
        pane.select_only(1);
        assert_eq!(pane.selected_path(), Some(PathBuf::from("/lab/sub")));
        pane.select_only(2);
        assert_eq!(pane.selected_path(), Some(PathBuf::from("/lab/alpha.txt")));
    }

    /// A walk that finishes for a directory the pane has left is returned to the
    /// caller to report, never applied.
    #[compio::test]
    async fn a_walk_for_a_directory_the_pane_left_is_not_applied() {
        let mem = memory();
        let backend: Arc<dyn Backend> = mem.clone();
        let mut pane = Browser::remote_at("mem", PathBuf::from("/lab"));
        pane.set_caps(backend.caps());
        let (host, path) = pane
            .take_request()
            .expect("a remote pane asks for its directory");

        let mut pump = Pump::new();
        pump.start(host, path, &backend);
        // The person navigates before the walk lands.
        pane.navigate(PathBuf::from("/lab/sub"));

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut undelivered = Vec::new();
        while !pump.is_empty() {
            assert!(Instant::now() < deadline, "the walk never finished");
            compio::time::sleep(Duration::from_millis(1)).await;
            undelivered.extend(pump.deliver(std::iter::once(&mut pane)));
        }
        assert_eq!(undelivered.len(), 1, "the stale reply is reported");
        assert_eq!(undelivered[0].host, "mem");
        assert_eq!(undelivered[0].path, Path::new("/lab"));
        assert_eq!(
            pane.directory(),
            Path::new("/lab/sub"),
            "the pane is where the person sent it"
        );
    }

    /// The async half of the protocol, for a worker-thread frontend: same
    /// coalescing, same whole-listing answer.
    #[compio::test]
    async fn a_collected_listing_is_the_whole_listing() {
        let mem = memory();
        let backend: Arc<dyn Backend> = mem.clone();
        let listing = collect(
            "mem".to_owned(),
            PathBuf::from("/lab"),
            backend.list(Path::new("/lab")),
        )
        .await;
        assert_eq!(listing.host, "mem");
        assert_eq!(listing.path, Path::new("/lab"));
        assert!(listing.failure.is_none());
        assert_eq!(listing.entries.len(), 2, "{:?}", listing.entries);

        let mut pane = Browser::remote_at("mem", PathBuf::from("/lab"));
        pane.set_caps(backend.caps());
        let _ = pane.take_request();
        listing.apply(&mut pane);
        assert_eq!(pane.directory(), Path::new("/lab"));
        pane.select_only(1);
        assert_eq!(pane.selected_path(), Some(PathBuf::from("/lab/sub")));
    }
}
