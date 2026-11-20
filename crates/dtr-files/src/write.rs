//! File operations, as requests a worker executes and as the one question an
//! overwrite asks.
//!
//! `docs/manager.md` §7 is the rules: **everything goes to the trash**, `Shift+Delete` is
//! the one permanent delete and it has a confirmation, and **an overwrite asks** —
//! a paste that would replace, a rename onto an existing name and a copy whose
//! destination exists are *one* card with the same three answers. This module is
//! that rule list as a state machine, and it deliberately does no I/O: the
//! browser reports a [`WriteRequest`], the app's worker executes it through
//! `dtr_vfs`'s verbs, and the answer comes back as [`Writes::succeeded`] or
//! [`Writes::failed`]. That split is what lets every branch below be tested
//! without a disk, a thread or a window.
//!
//! Three decisions are the module's own, and each is here rather than in a
//! caller so it cannot drift:
//!
//! - **One answer for the batch.** A multi-file paste that conflicts on three
//!   files asks *once*: the card says how many items are behind it and the
//!   answer applies to all of them ([`Question::remaining`]). The alternative —
//!   a card per file — is the "three dialogs that happen to agree" §7 exists to
//!   avoid, and a person who chose "replace" for a batch has said what they want
//!   for the batch. A batch answer only applies to conflicts: a destination that
//!   is free is written as asked.
//! - **`keep both` adds a number to the stem**, not to the whole name:
//!   `report.txt` becomes `report (2).txt`, and a name that already ends in
//!   ` (n)` counts from `n + 1`, so a second keep-both is `report (3).txt` and
//!   never `report (2) (2).txt` ([`next_free_name`]).
//! - **A `create_dir` collision is not an overwrite choice.** A new folder whose
//!   name is taken is a refusal with the folder's name in it, never a card whose
//!   "replace" would delete somebody's directory to put an empty one there — the
//!   trait's own comment calls it a name collision for this reason.
//!   [`WriteOp::overwritable`] is where that is decided.
//!
//! A non-conflict failure (permissions, a missing parent, the OS's own refusal)
//! **stops the batch** and goes into the pane's footer. Carrying on through a
//! list of files after a write has failed for a reason nobody has read yet is
//! how a half-completed operation becomes a mystery; the items already done stay
//! done, which is the same promise the trash makes.
//!
//! Not here, by design: undo, a transfer queue with progress (§8) and moving
//! across machines. `move_to` is the same machine's rename, and a cross-machine
//! move is a read plus a write the queue owns.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use dtr_vfs::{BackendId, Removal};
use dtr_widgets::ConflictAnswer;

/// One verb, without the machine or the overwrite answer.
///
/// Every variant carries the paths it needs and nothing else, so a request can
/// be compared, retried under a new name and answered in a test without any of
/// the objects that would execute it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteOp {
    /// One `mkdir`: the parent exists, and an existing name is a collision.
    CreateDir { at: PathBuf },
    /// The same primitive as a move: a file manager says them differently.
    Rename { from: PathBuf, to: PathBuf },
    /// One whole file's bytes, to a destination that must not exist.
    Copy { from: PathBuf, to: PathBuf },
    /// A rename inside one machine — the cross-machine case is §8's queue.
    Move { from: PathBuf, to: PathBuf },
    /// Trash or delete, recursively for a directory.
    Remove { at: PathBuf, how: Removal },
}

impl WriteOp {
    /// Where the verb writes, i.e. the path a conflict would be about.
    ///
    /// `None` for a delete, which has no destination to collide with.
    pub fn destination(&self) -> Option<&Path> {
        match self {
            Self::CreateDir { at } => Some(at),
            Self::Rename { to, .. } | Self::Copy { to, .. } | Self::Move { to, .. } => Some(to),
            Self::Remove { .. } => None,
        }
    }

    /// The path the verb reads from, when it reads one.
    ///
    /// The conflict card shows it as the "source" line; a new folder has no
    /// source and the card is never asked about one.
    pub fn source(&self) -> Option<&Path> {
        match self {
            Self::Rename { from, .. } | Self::Copy { from, .. } | Self::Move { from, .. } => {
                Some(from)
            }
            Self::CreateDir { .. } | Self::Remove { .. } => None,
        }
    }

    /// Whether an existing destination can be answered with the conflict card.
    ///
    /// A create is a name collision and a delete has no destination, so only the
    /// three verbs that write *over* a name are overwritable.
    pub fn overwritable(&self) -> bool {
        matches!(
            self,
            Self::Rename { .. } | Self::Copy { .. } | Self::Move { .. }
        )
    }

    /// The verb in a word, for the footer and the card: "trash", "delete",
    /// "copy", "move", "rename", "create".
    pub fn label(&self) -> &'static str {
        match self {
            Self::CreateDir { .. } => "create",
            Self::Rename { .. } => "rename",
            Self::Copy { .. } => "copy",
            Self::Move { .. } => "move",
            Self::Remove { how, .. } => match how {
                Removal::Trash => "trash",
                Removal::Permanent => "delete",
            },
        }
    }

    /// The path a footer names: where it writes, else where it reads.
    pub fn describe(&self) -> String {
        match self {
            Self::Remove { at, .. } => at.display().to_string(),
            _ => self
                .destination()
                .or_else(|| self.source())
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        }
    }

    /// The same verb with another destination, keeping the conflict's source.
    ///
    /// This is what "keep both" is: the operation is unchanged, the name it
    /// writes to is not.
    fn rebased(self, destination: PathBuf) -> Self {
        match self {
            Self::Rename { from, .. } => Self::Rename {
                from,
                to: destination,
            },
            Self::Copy { from, .. } => Self::Copy {
                from,
                to: destination,
            },
            Self::Move { from, .. } => Self::Move {
                from,
                to: destination,
            },
            other => other,
        }
    }
}

/// One request the app's worker executes.
///
/// The app takes it with [`Writes::take_request`], routes it through
/// `Vfs::backend(&request.backend)` and calls the verb. When the destination is
/// allowed but already exists, `replace` says how to clear it first: **remove
/// the existing destination with that removal, then run the operation**. The
/// trait deliberately never replaces anything itself, so the destructive part of
/// "replace" is spelled here rather than hidden in a backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WriteRequest {
    /// Which machine, so a worker that owns one `Vfs` can route.
    pub backend: BackendId,
    pub op: WriteOp,
    /// `Some(how)` to replace an existing destination; `None` is the backend's
    /// own conflict.
    pub replace: Option<Removal>,
}

/// The one overwrite question, while it is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    /// The path that already exists — the backend's own answer, so the card
    /// names the destination the person will actually lose.
    pub destination: PathBuf,
    /// The verb that was refused, as it stood when it failed.
    pub op: WriteOp,
    /// How many requests are still queued behind this one. The answer given to
    /// this card applies to all of them; zero means this is the last item.
    pub remaining: usize,
}

impl Question {
    /// The path the verb was writing *from*, for the card's source line.
    pub fn source(&self) -> Option<&Path> {
        self.op.source()
    }

    /// The answer currently arrives for this many items in total.
    pub fn total(&self) -> usize {
        self.remaining + 1
    }
}

/// What [`Writes::failed`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The conflict card is open; answer it with [`Writes::answer`].
    Asking,
    /// A batch answer was already given, so no card appeared: another request is
    /// waiting to be taken.
    Answered,
    /// Nothing was in flight — a worker answering twice, which is a caller bug
    /// and not something to guess about.
    Idle,
    /// Not an overwrite question (or a collision with no overwrite answer). The
    /// batch stopped; [`Writes::failure`] has the words for the footer.
    Stopped,
}

/// The requests of one operation, and the answer already given to its conflicts.
///
/// One `Writes` is one user action — "delete these three", "paste" — and it is
/// finished when [`Writes::is_done`]. The browser replaces it when the next
/// action starts.
#[derive(Debug)]
pub struct Writes {
    backend: BackendId,
    /// How a "replace" clears the destination: the trash where the backend has
    /// one, the permanent delete where it does not (`Caps::trash`).
    replace: Removal,
    queue: VecDeque<WriteOp>,
    /// A request computed by an answer, which the app has not taken yet.
    pending: Option<WriteRequest>,
    /// The verb whose request is out with the worker.
    in_flight: Option<WriteOp>,
    asking: Option<Question>,
    /// The answer given for the rest of this batch, if one was given.
    batch: Option<ConflictAnswer>,
    /// Names that already exist in the destination directory plus the ones this
    /// batch has claimed, which is what `keep both` counts past.
    taken: BTreeSet<String>,
    failure: Option<String>,
}

impl Default for Writes {
    fn default() -> Self {
        Self {
            backend: BackendId::Local,
            replace: Removal::Permanent,
            queue: VecDeque::new(),
            pending: None,
            in_flight: None,
            asking: None,
            batch: None,
            taken: BTreeSet::new(),
            failure: None,
        }
    }
}

impl Writes {
    /// An empty plan for `backend`, knowing the destination directory's names.
    ///
    /// `replace` is what a "replace" answer removes the destination with, so it
    /// is `Removal::Trash` exactly where `Caps::trash` is true.
    pub fn new<S: Into<String>>(
        backend: BackendId,
        replace: Removal,
        names: impl IntoIterator<Item = S>,
    ) -> Self {
        Self {
            backend,
            replace,
            taken: names.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    /// Add one verb to the batch.
    pub fn push(&mut self, op: WriteOp) {
        self.queue.push_back(op);
    }

    /// The next request, if the worker is idle and nothing is being asked.
    ///
    /// A request is in flight from here until [`Self::succeeded`] or
    /// [`Self::failed`], so a caller pumping this in a loop must report the
    /// answer before asking again — which is also what makes the plan sequential
    /// and a conflict answerable at all.
    pub fn take_request(&mut self) -> Option<WriteRequest> {
        if self.asking.is_some() || self.in_flight.is_some() {
            return None;
        }
        if let Some(request) = self.pending.take() {
            self.in_flight = Some(request.op.clone());
            return Some(request);
        }
        let op = self.queue.pop_front()?;
        self.in_flight = Some(op.clone());
        Some(WriteRequest {
            backend: self.backend.clone(),
            op,
            replace: None,
        })
    }

    /// The request finished. `true` when the whole batch is finished.
    pub fn succeeded(&mut self) -> bool {
        self.in_flight = None;
        self.is_done()
    }

    /// The request failed with `err`. See [`Failure`] for what this did.
    pub fn failed(&mut self, err: &anyhow::Error) -> Failure {
        let Some(op) = self.in_flight.take() else {
            return Failure::Idle;
        };
        let conflict = conflict_destination(err).map(Path::to_path_buf);
        if let Some(destination) = conflict
            && op.overwritable()
        {
            if let Some(answer) = self.batch {
                self.apply(answer, op, destination);
                return Failure::Answered;
            }
            self.asking = Some(Question {
                destination,
                op,
                remaining: self.queue.len(),
            });
            return Failure::Asking;
        }
        let message = format!("could not {} {}: {err:#}", op.label(), op.describe());
        self.failure = Some(message);
        // A failure nobody has read yet is not a reason to keep writing more
        // files: the batch stops here, and what is already done stays done.
        self.queue.clear();
        Failure::Stopped
    }

    /// The question this plan is asking, if any.
    pub fn question(&self) -> Option<&Question> {
        self.asking.as_ref()
    }

    /// Answer the open question. `false` when nothing was being asked.
    ///
    /// The answer is recorded as the batch's answer whenever items remain, so
    /// the next conflict is resolved without a card.
    pub fn answer(&mut self, answer: ConflictAnswer) -> bool {
        let Some(question) = self.asking.take() else {
            return false;
        };
        if question.remaining > 0 {
            self.batch = Some(answer);
        }
        self.apply(answer, question.op, question.destination);
        true
    }

    /// Drop the question and everything behind it: the person closed the card.
    ///
    /// What has already been written stays written — the card cannot undo a
    /// batch that is half done, and pretending it can would be worse than
    /// saying so.
    pub fn cancel(&mut self) {
        self.asking = None;
        self.pending = None;
        self.queue.clear();
    }

    /// The answer already given for the rest of the batch.
    pub fn batch_answer(&self) -> Option<ConflictAnswer> {
        self.batch
    }

    /// The words a footer says after a [`Failure::Stopped`].
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// Whether nothing is left to do, ask or report.
    pub fn is_done(&self) -> bool {
        self.queue.is_empty()
            && self.pending.is_none()
            && self.in_flight.is_none()
            && self.asking.is_none()
    }

    /// How many operations this plan has run or still will, excluding conflicts
    /// resolved by a retry — for a footer's "N items".
    pub fn remaining(&self) -> usize {
        self.queue.len()
            + usize::from(self.in_flight.is_some())
            + usize::from(self.pending.is_some())
    }

    /// Turn one answer into the next request (or into nothing, for skip).
    fn apply(&mut self, answer: ConflictAnswer, op: WriteOp, destination: PathBuf) {
        match answer {
            // Skipping this one writes nothing, and the batch carries on: a
            // "skip the rest" answer skips the rest's conflicts too, but a file
            // that does not conflict is still written.
            ConflictAnswer::Skip => {}
            ConflictAnswer::Replace => {
                self.pending = Some(WriteRequest {
                    backend: self.backend.clone(),
                    op,
                    replace: Some(self.replace),
                });
            }
            ConflictAnswer::KeepBoth => {
                let name = destination
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let name = next_free_name(&name, &self.taken);
                self.taken.insert(name.clone());
                let destination = match destination.parent() {
                    Some(parent) => parent.join(&name),
                    None => PathBuf::from(&name),
                };
                self.pending = Some(WriteRequest {
                    backend: self.backend.clone(),
                    op: op.rebased(destination),
                    replace: None,
                });
            }
        }
    }
}

/// The destination a [`dtr_vfs::Conflict`] names, out of an `anyhow` error.
///
/// `dtr_vfs::is_conflict` answers *whether*; the card also needs *what*, and the
/// error carries it.
fn conflict_destination(err: &anyhow::Error) -> Option<&Path> {
    err.chain()
        .find_map(|cause| cause.downcast_ref::<dtr_vfs::Conflict>())
        .map(|conflict| conflict.path.as_path())
}

/// The next name that is free in `taken`, keeping the extension where it is.
///
/// `report.txt` → `report (2).txt`, `README` → `README (2)`, `.bashrc` →
/// `.bashrc (2)` (a dotfile has no extension to preserve), and `report (2).txt`
/// → `report (3).txt` — the number belongs to the stem, so a repeated keep-both
/// counts up instead of stacking suffixes.
pub fn next_free_name(name: &str, taken: &BTreeSet<String>) -> String {
    let path = Path::new(name);
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned())
        .filter(|extension| !extension.is_empty());
    let stem = match path.file_stem() {
        Some(stem) => stem.to_string_lossy().into_owned(),
        None => name.to_owned(),
    };
    let (base, mut number) = match trailing_number(&stem) {
        Some((base, number)) => (base, number + 1),
        None => (stem, 2),
    };
    loop {
        let candidate = match &extension {
            Some(extension) => format!("{base} ({number}).{extension}"),
            None => format!("{base} ({number})"),
        };
        if !taken.contains(&candidate) {
            return candidate;
        }
        number += 1;
    }
}

/// The ` (n)` a stem already ends in, and the stem before it.
fn trailing_number(stem: &str) -> Option<(String, u32)> {
    let rest = stem.strip_suffix(')')?;
    let (base, digits) = rest.rsplit_once(" (")?;
    let number = digits.parse::<u32>().ok()?;
    (!base.is_empty()).then(|| (base.to_owned(), number))
}

/// Whether a typed name can be one file name in one directory.
///
/// The rules are the ones that would otherwise create a path or a lie: an empty
/// name, `.` and `..`, a separator (both kinds, because the same window may run
/// on either platform) and a control character. A name is *not* trimmed into
/// shape — what was typed is what is written, so a trailing space in a name is
/// the person's business and not a silent edit.
pub fn valid_name(name: &str) -> Result<(), &'static str> {
    if name.trim().is_empty() {
        return Err("type a name");
    }
    if name == "." || name == ".." {
        return Err("that is a place, not a file");
    }
    if name.contains('/') || name.contains('\\') {
        return Err("a name cannot hold a separator");
    }
    if name.chars().any(char::is_control) {
        return Err("a name cannot hold a control character");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use dtr_vfs::Conflict;

    fn write(name: &str) -> WriteOp {
        WriteOp::Copy {
            from: PathBuf::from("/src").join(name),
            to: PathBuf::from("/dst").join(name),
        }
    }

    fn conflict(path: &str) -> anyhow::Error {
        anyhow::Error::new(Conflict {
            path: PathBuf::from(path),
        })
    }

    fn taken(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// The plan hands out one request and refuses to hand out the next until it
    /// is answered: that is what makes a conflict answerable rather than racing
    /// a batch.
    #[test]
    fn a_plan_hands_out_one_request_at_a_time() {
        let mut writes = Writes::new(BackendId::Local, Removal::Trash, Vec::<String>::new());
        writes.push(write("a.txt"));
        writes.push(write("b.txt"));

        let first = writes.take_request().expect("the first");
        assert_eq!(first.op, write("a.txt"));
        assert_eq!(first.backend, BackendId::Local);
        assert_eq!(first.replace, None, "the first attempt never replaces");
        assert_eq!(writes.take_request(), None, "one request is out");

        assert!(!writes.succeeded(), "one of two is not done");
        assert_eq!(
            writes.take_request().expect("the second").op,
            write("b.txt")
        );
        assert!(writes.succeeded(), "the queue is empty");
    }

    /// Replace retries the same verb with the destination allowed, and clears it
    /// the way the backend's trash says: into the trash where there is one.
    #[test]
    fn replace_retries_with_the_destination_removed_first() {
        let mut writes = Writes::new(
            BackendId::Local,
            Removal::Trash,
            ["a.txt"].map(String::from),
        );
        writes.push(write("a.txt"));
        writes.take_request().expect("the request");

        assert_eq!(
            writes.failed(&conflict("/dst/a.txt")),
            Failure::Asking,
            "a conflict is the one card"
        );
        let question = writes.question().expect("asking").clone();
        assert_eq!(question.destination, PathBuf::from("/dst/a.txt"));
        assert_eq!(question.remaining, 0, "one item, no batch policy");
        assert_eq!(question.source(), Some(Path::new("/src/a.txt")));

        assert!(writes.answer(ConflictAnswer::Replace));
        let retry = writes.take_request().expect("the retry");
        assert_eq!(retry.op, write("a.txt"), "the verb is unchanged");
        assert_eq!(
            retry.replace,
            Some(Removal::Trash),
            "the destination is trashed, not unlinked, where a trash exists"
        );
        assert!(writes.succeeded());
    }

    /// A backend with no trash replaces permanently, and the request says so:
    /// the browser must not choose a removal the machine cannot do.
    #[test]
    fn replace_is_permanent_where_there_is_no_trash() {
        let mut writes = Writes::new(BackendId::Local, Removal::Permanent, Vec::<String>::new());
        writes.push(write("a.txt"));
        writes.take_request().expect("the request");
        assert_eq!(writes.failed(&conflict("/dst/a.txt")), Failure::Asking);
        writes.answer(ConflictAnswer::Replace);
        assert_eq!(
            writes.take_request().expect("the retry").replace,
            Some(Removal::Permanent)
        );
    }

    /// Keep both writes beside the destination: the number goes on the stem, so
    /// the extension survives and a repeat counts up.
    #[test]
    fn keep_both_asks_the_vfs_for_a_numbered_name() {
        let mut writes = Writes::new(
            BackendId::Local,
            Removal::Trash,
            ["a.txt"].map(String::from),
        );
        writes.push(WriteOp::Copy {
            from: PathBuf::from("/src/report.txt"),
            to: PathBuf::from("/dst/report.txt"),
        });
        writes.take_request().expect("the request");
        assert_eq!(writes.failed(&conflict("/dst/report.txt")), Failure::Asking);
        writes.answer(ConflictAnswer::KeepBoth);
        let retry = writes.take_request().expect("the retry");
        assert_eq!(
            retry.op,
            WriteOp::Copy {
                from: PathBuf::from("/src/report.txt"),
                to: PathBuf::from("/dst/report (2).txt"),
            }
        );
        assert_eq!(retry.replace, None, "keeping both replaces nothing");
    }

    /// Skip writes nothing and the batch carries on with the next item.
    #[test]
    fn skip_drops_the_item_and_continues() {
        let mut writes = Writes::new(BackendId::Local, Removal::Trash, Vec::<String>::new());
        writes.push(write("a.txt"));
        writes.push(write("b.txt"));
        writes.take_request().expect("a");
        assert_eq!(writes.failed(&conflict("/dst/a.txt")), Failure::Asking);
        writes.answer(ConflictAnswer::Skip);
        assert_eq!(writes.take_request().expect("b").op, write("b.txt"));
        assert!(writes.succeeded());
    }

    /// **One card, one answer for the batch.** Two more files conflict after the
    /// first answer, and neither asks again: the answer the person gave applies
    /// to the rest.
    #[test]
    fn one_answer_settles_the_rest_of_the_batch() {
        let mut writes = Writes::new(
            BackendId::Local,
            Removal::Trash,
            ["a.txt", "b.txt", "c.txt"].map(String::from),
        );
        for name in ["a.txt", "b.txt", "c.txt"] {
            writes.push(write(name));
        }
        writes.take_request().expect("a");
        assert_eq!(writes.failed(&conflict("/dst/a.txt")), Failure::Asking);
        assert_eq!(writes.question().expect("the question").remaining, 2);
        assert_eq!(writes.question().expect("the question").total(), 3);

        writes.answer(ConflictAnswer::Replace);
        assert_eq!(
            writes.batch_answer(),
            Some(ConflictAnswer::Replace),
            "the answer is recorded for the rest"
        );
        assert_eq!(
            writes.take_request().expect("a retry").replace,
            Some(Removal::Trash)
        );
        writes.succeeded();

        // b conflicts: answered by the batch, so no card and no waiting.
        assert_eq!(writes.take_request().expect("b").op, write("b.txt"));
        assert_eq!(writes.failed(&conflict("/dst/b.txt")), Failure::Answered);
        let retry = writes.take_request().expect("b retry");
        assert_eq!(retry.op, write("b.txt"));
        assert_eq!(retry.replace, Some(Removal::Trash));
        writes.succeeded();

        // c has no conflict: it is written as asked.
        let last = writes.take_request().expect("c");
        assert_eq!(last.op, write("c.txt"));
        assert_eq!(last.replace, None);
        assert!(writes.succeeded());
    }

    /// A batch that chose "keep both" gives every conflict after it a fresh
    /// name, and the numbers do not collide with each other.
    #[test]
    fn keeping_both_for_the_batch_numbers_every_conflict() {
        let mut writes = Writes::new(
            BackendId::Local,
            Removal::Trash,
            ["a.txt", "b.txt", "c.txt"].map(String::from),
        );
        for name in ["a.txt", "b.txt", "c.txt"] {
            writes.push(write(name));
        }
        writes.take_request().expect("a");
        writes.failed(&conflict("/dst/a.txt"));
        writes.answer(ConflictAnswer::KeepBoth);
        assert_eq!(
            writes.take_request().expect("a retry").op,
            WriteOp::Copy {
                from: PathBuf::from("/src/a.txt"),
                to: PathBuf::from("/dst/a (2).txt"),
            }
        );
        writes.succeeded();

        writes.take_request().expect("b");
        assert_eq!(writes.failed(&conflict("/dst/b.txt")), Failure::Answered);
        assert_eq!(
            writes.take_request().expect("b retry").op,
            WriteOp::Copy {
                from: PathBuf::from("/src/b.txt"),
                to: PathBuf::from("/dst/b (2).txt"),
            }
        );
    }

    /// A new folder whose name is taken is a refusal, not a card: "replace" on a
    /// directory would delete somebody's folder to put an empty one there.
    #[test]
    fn a_create_dir_collision_is_a_failure_not_an_overwrite() {
        let mut writes = Writes::new(BackendId::Local, Removal::Trash, Vec::<String>::new());
        writes.push(WriteOp::CreateDir {
            at: PathBuf::from("/dst/new"),
        });
        writes.take_request().expect("the mkdir");
        assert_eq!(writes.failed(&conflict("/dst/new")), Failure::Stopped);
        assert_eq!(writes.question(), None, "no card for a name collision");
        let message = writes.failure().expect("the footer's words");
        assert!(message.contains("create"), "{message}");
        assert!(message.contains("/dst/new"), "{message}");
        assert!(writes.is_done(), "the batch stopped");
    }

    /// Any other failure stops the batch rather than carrying on through files
    /// nobody has read the reason for.
    #[test]
    fn a_failure_that_is_not_a_conflict_stops_the_batch() {
        let mut writes = Writes::new(BackendId::Local, Removal::Trash, Vec::<String>::new());
        writes.push(write("a.txt"));
        writes.push(write("b.txt"));
        writes.take_request().expect("a");
        let err = anyhow::anyhow!("permission denied");
        assert_eq!(writes.failed(&err), Failure::Stopped);
        let message = writes.failure().expect("the footer's words");
        assert!(message.contains("copy"), "{message}");
        assert!(message.contains("permission denied"), "{message}");
        assert_eq!(writes.take_request(), None, "nothing else is written");
    }

    /// Closing the card drops the item and the rest, and does not pretend to
    /// undo what is already written.
    #[test]
    fn cancelling_drops_the_question_and_the_rest() {
        let mut writes = Writes::new(BackendId::Local, Removal::Trash, Vec::<String>::new());
        writes.push(write("a.txt"));
        writes.push(write("b.txt"));
        writes.take_request().expect("a");
        writes.failed(&conflict("/dst/a.txt"));
        writes.cancel();
        assert_eq!(writes.question(), None);
        assert!(writes.is_done());
        assert_eq!(writes.take_request(), None);
    }

    /// The numbering rule: stem, not whole name; an existing suffix counts up; a
    /// dotfile has no extension to keep and a taken candidate is skipped.
    #[test]
    fn a_kept_name_keeps_its_extension_and_counts_up() {
        let none = BTreeSet::new();
        assert_eq!(next_free_name("report.txt", &none), "report (2).txt");
        assert_eq!(
            next_free_name("archive.tar.gz", &none),
            "archive.tar (2).gz"
        );
        assert_eq!(next_free_name("README", &none), "README (2)");
        assert_eq!(next_free_name(".bashrc", &none), ".bashrc (2)");
        assert_eq!(next_free_name("report (2).txt", &none), "report (3).txt");
        assert_eq!(
            next_free_name("report.txt", &taken(&["report (2).txt"])),
            "report (3).txt",
            "a name already claimed in this batch is skipped"
        );
    }

    #[test]
    fn a_typed_name_has_to_be_one_name() {
        assert!(valid_name("notes.txt").is_ok());
        assert!(valid_name(" spaced name ").is_ok(), "spaces are a name");
        assert_eq!(valid_name(""), Err("type a name"));
        assert_eq!(valid_name("   "), Err("type a name"));
        assert_eq!(valid_name("."), Err("that is a place, not a file"));
        assert_eq!(valid_name(".."), Err("that is a place, not a file"));
        assert!(valid_name("a/b").is_err());
        assert!(valid_name("a\\b").is_err());
        assert!(valid_name("a\tb").is_err());
    }

    /// The two accessors the card and the footer need: where a verb reads and
    /// where it writes.
    #[test]
    fn a_verb_names_its_source_and_its_destination() {
        let rename = WriteOp::Rename {
            from: PathBuf::from("/a"),
            to: PathBuf::from("/b"),
        };
        assert_eq!(rename.source(), Some(Path::new("/a")));
        assert_eq!(rename.destination(), Some(Path::new("/b")));
        assert!(rename.overwritable());
        let remove = WriteOp::Remove {
            at: PathBuf::from("/a"),
            how: Removal::Trash,
        };
        assert_eq!(remove.destination(), None);
        assert!(!remove.overwritable());
        assert_eq!(remove.label(), "trash");
    }
}
