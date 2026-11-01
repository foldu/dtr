//! What a macro can do to the world outside the interpreter.
//!
//! TTL is not a language that computes for its own sake: every macro in the
//! corpus talks to a terminal, a file or a person, and the dialogs of §7.6 are
//! where a person comes in. The interpreter therefore does its I/O through
//! [`Host`] and never touches the terminal, a window or `std::fs` itself. That
//! boundary is what makes a macro testable — [`RecordingHost`] answers the
//! dialogs and writes down what it was shown, so the `messagebox` at the end of
//! a bug-report macro becomes an assertion instead of a window nobody is
//! looking at.
//!
//! Every method returns what Tera Term would return, not what a person would
//! say: `yesnobox` answers a bool, `listbox` an index that is −1 when
//! cancelled, `inputbox` `None` when cancelled. The command that called it
//! turns that into `result` and `inputstr` (§7.6), which is the layer above.

use crate::files::Files;

/// A date and time for `gettime`/`getdate` (§7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// 0 = Sunday, as `strftime`'s `%w` reports it.
    pub weekday: u32,
    /// Offset from UTC in minutes, for `%z`.
    pub utc_offset_minutes: i32,
}

pub trait Host {
    /// §7.6: `<message> <title>`. Blocks until answered; the answer is thrown
    /// away.
    fn messagebox(&mut self, message: &str, title: &str);

    /// §7.6: Yes (`true`) or No (`false`), which becomes `result` 1 or 0.
    fn yesnobox(&mut self, message: &str, title: &str) -> bool;

    /// §7.6: the answer becomes `inputstr`; `None` means cancelled.
    fn inputbox(&mut self, message: &str, title: &str, default: &str) -> Option<String>;

    /// §7.6: as `inputbox`, with the characters masked.
    fn passwordbox(&mut self, message: &str, title: &str) -> Option<String>;

    /// §7.6: `save` distinguishes "Save as" from "Open"; `None` means
    /// cancelled.
    fn filenamebox(&mut self, title: &str, save: bool, initial_dir: Option<&str>)
    -> Option<String>;

    /// §7.6: the chosen directory; `None` means cancelled.
    fn dirnamebox(&mut self, title: &str, initial_dir: Option<&str>) -> Option<String>;

    /// §7.6: a 0-based index into `items`, or `None` for cancelled (−1).
    ///
    /// `selected` is the caller's preselection, if it has one.
    fn listbox(
        &mut self,
        message: &str,
        title: &str,
        items: &[String],
        selected: Option<i32>,
    ) -> Option<i32>;

    /// §7.6: the one dialog that does not block. Calling it again replaces the
    /// message.
    fn statusbox(&mut self, message: &str, title: &str);

    /// §7.6: close the status box.
    fn closesbox(&mut self);

    /// §7.6: where the next dialog appears, in screen pixels.
    fn setdlgpos(&mut self, x: Option<i32>, y: Option<i32>);

    /// `dispstr`: a line in the macro window. Tera Term writes it to the
    /// window the macro runs in, not to the terminal.
    fn dispstr(&mut self, text: &str);

    /// `beep`: the sound type is 0…5 (§7.6).
    fn beep(&mut self, kind: i32);

    /// `show`: the macro window's own state — less than 0 hides it, 0
    /// minimises it, above 0 restores it (§7.6).
    fn show_macro_window(&mut self, flag: i32);

    /// `pause`/`mpause`. The host decides whether time actually passes: a test
    /// host must be able to say "noted, but do not sleep for ten minutes".
    fn pause(&mut self, millis: u64);

    /// Read a file for `include`. `None` is `Can't open file.` (§8).
    fn read(&mut self, path: &str) -> Option<Vec<u8>>;

    /// `flushrecv`: throw away what is already in the macro's receive queue.
    fn flush_recv(&mut self);

    /// A session setting a macro can change without the macro caring what it
    /// does: `setecho`, `enablekeyb`, `setsync`, `setmulticastname`.
    fn note_setting(&mut self, name: &str, value: &str);

    /// The local clock for `gettime`/`getdate`.
    fn now(&self) -> DateTime {
        crate::time::now_utc().unwrap_or(DateTime {
            year: 1970,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            weekday: 4,
            utc_offset_minutes: 0,
        })
    }

    // -- §7.1, the link and the byte stream ---------------------------------

    /// `testlink`'s answer, and what `connect` leaves behind: 0 unlinked,
    /// 1 linked but disconnected, 2 connected.
    fn link_state(&mut self) -> i32;

    /// `connect '<parameters>'` — the parameters are Tera Term's own command
    /// line, so the host decides what a serial port or a host name means.
    fn connect(&mut self, parameters: &str) -> bool;

    fn disconnect(&mut self);

    fn unlink(&mut self);

    /// `closett`: the terminal itself goes away.
    fn close_terminal(&mut self);

    /// `send`/`sendln`/the broadcast family: bytes for the far end.
    fn send(&mut self, bytes: &[u8]);

    /// `recvln`: one line from the macro's own receive queue (§7.1 — the
    /// commands read that queue, not the wire). `None` is a timeout.
    fn read_line(&mut self) -> Option<String>;

    /// `wait`/`waitln`: which of `patterns` matched (0-based) and the text
    /// that matched, or `None` when the wait timed out. `line` asks for a whole
    /// line rather than the first occurrence anywhere.
    fn wait_for(
        &mut self,
        patterns: &[String],
        millis: i64,
        line: bool,
    ) -> Option<(usize, String)>;

    /// The file system the §7.4 commands work on.
    fn files(&mut self) -> &mut dyn Files;

    fn getenv(&mut self, name: &str) -> Option<String>;

    fn setenv(&mut self, name: &str, value: &str);

    /// `uptime`: milliseconds since the machine started.
    fn uptime_millis(&mut self) -> i64;

    // -- §7.1/§7.4 things a macro asks the machine ---------------------------

    /// `getdir`: the working directory `fileopen` resolves relative paths in.
    fn get_dir(&mut self) -> String;

    /// `setdir`.
    fn set_dir(&mut self, dir: &str);

    /// `exec`: run a command line and return its exit code (`result`).
    fn exec(&mut self, command_line: &str, wait: bool) -> i32;

    /// `getspecialfolder`: a shell folder such as `MyDocuments`.
    fn special_folder(&mut self, name: &str) -> Option<String>;

    /// `waitn`: wait for `count` bytes from the macro's receive queue.
    fn wait_bytes(&mut self, count: usize, millis: i64) -> Option<Vec<u8>>;

    /// `waitrecv`: wait for a chunk of `len` bytes with `pattern` at the
    /// 1-origin position `position`.
    fn wait_recv(
        &mut self,
        pattern: &str,
        len: usize,
        position: usize,
        millis: i64,
    ) -> Option<Vec<u8>>;
}

/// What one macro did to its host, in the order it did it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Message {
        message: String,
        title: String,
    },
    YesNo {
        message: String,
        title: String,
        yes: bool,
    },
    Input {
        message: String,
        title: String,
        masked: bool,
        answer: Option<String>,
    },
    FileName {
        title: String,
        save: bool,
        answer: Option<String>,
    },
    DirName {
        title: String,
        answer: Option<String>,
    },
    List {
        message: String,
        title: String,
        items: Vec<String>,
        selected: Option<i32>,
        answer: Option<i32>,
    },
    Status {
        message: String,
        title: String,
    },
    CloseStatus,
    DialogPosition {
        x: Option<i32>,
        y: Option<i32>,
    },
    Disp(String),
    Beep {
        kind: i32,
    },
    ShowMacroWindow {
        flag: i32,
    },
    Pause {
        millis: u64,
    },
    Connect {
        parameters: String,
    },
    Disconnect,
    Unlink,
    CloseTerminal,
    Setting {
        name: String,
        value: String,
    },
    Exec {
        command_line: String,
        wait: bool,
    },
}

/// A host that answers from script and remembers everything.
///
/// The defaults are the boring ones: No, cancel, and a clock that does not
/// move. A test says what it wants answered, runs the macro, and reads
/// [`RecordingHost::events`] — which is how "the macro told the user the port
/// was not found" becomes a comparison rather than a screenshot.
#[derive(Debug, Default)]
pub struct RecordingHost {
    events: Vec<Event>,
    /// What the next `yesnobox` answers.
    pub yes: bool,
    /// What the next `inputbox`/`passwordbox` answers; `None` is cancelled.
    pub input: Option<String>,
    /// What the next `filenamebox`/`dirnamebox` answers.
    pub path: Option<String>,
    /// What the next `listbox` answers; `None` is cancelled.
    pub choice: Option<i32>,
    /// Files `include` can find, by the path the macro writes.
    pub sources: Vec<(String, Vec<u8>)>,
    /// Total time the macro asked to wait.
    pub waited_ms: u64,
    /// The clock `gettime` reads, when a test cares what it says.
    pub clock: Option<DateTime>,
    /// `testlink`'s answer. Two is connected, which is what a macro that never
    /// calls `connect` should see if it sends something.
    pub link: i32,
    /// Everything the macro sent, in order.
    pub sent: Vec<u8>,
    /// Lines `recvln` hands out, oldest first.
    pub queue: Vec<String>,
    /// What `wait`/`waitln` matches; `None` is a timeout.
    pub wait_answer: Option<(usize, String)>,
    /// The file system `fileopen` and its relatives share.
    pub fs: crate::files::MemoryFiles,
    /// The environment `getenv` reads.
    pub env: std::collections::BTreeMap<String, String>,
    /// What `uptime` reports.
    pub uptime_ms: i64,
    /// What `getdir` answers, and where `setdir` puts it.
    pub dir: String,
    /// The exit code `exec` reports.
    pub exit_code: i32,
    /// `getspecialfolder`'s table, by folder name.
    pub special_folders: Vec<(String, String)>,
}

impl RecordingHost {
    pub fn new() -> Self {
        Self {
            // A macro that never calls `connect` should still be able to send:
            // Tera Term's own tests assume the terminal is linked.
            link: 2,
            uptime_ms: 1_000,
            ..Self::default()
        }
    }

    /// Answer `yesnobox` with Yes.
    pub fn answering_yes(mut self) -> Self {
        self.yes = true;
        self
    }

    /// Answer the next text dialog with `text`.
    pub fn answering(mut self, text: &str) -> Self {
        self.input = Some(text.to_string());
        self
    }

    /// Choose `index` in the next `listbox`.
    pub fn choosing(mut self, index: i32) -> Self {
        self.choice = Some(index);
        self
    }

    /// Make a file visible to `include`.
    pub fn with_file(mut self, path: &str, bytes: &[u8]) -> Self {
        self.sources.push((path.to_string(), bytes.to_vec()));
        self
    }

    /// Put a file where `fileopen` can find it.
    pub fn with_disk_file(mut self, path: &str, bytes: &[u8]) -> Self {
        self.fs.add(path, bytes);
        self
    }

    /// Queue a line for `recvln` to hand out.
    pub fn with_line(mut self, line: &str) -> Self {
        self.queue.push(line.to_string());
        self
    }

    /// Make `getspecialfolder` know a folder.
    pub fn with_folder(mut self, name: &str, path: &str) -> Self {
        self.special_folders
            .push((name.to_string(), path.to_string()));
        self
    }

    /// Make `wait`/`waitln` match pattern `index` (0-based) with `text`.
    pub fn matching(mut self, index: usize, text: &str) -> Self {
        self.wait_answer = Some((index, text.to_string()));
        self
    }

    /// Everything the macro has sent so far, as text.
    pub fn sent_text(&self) -> String {
        String::from_utf8_lossy(&self.sent).into_owned()
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// The messages of every `messagebox`, in order, without their titles.
    pub fn messages(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Message { message, .. } => Some(message.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The titles of every `messagebox`, in order.
    pub fn titles(&self) -> Vec<&str> {
        self.events
            .iter()
            .filter_map(|event| match event {
                Event::Message { title, .. } => Some(title.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Everything the macro said through `dispstr`, joined by newlines.
    pub fn disp(&self) -> String {
        let lines: Vec<&str> = self
            .events
            .iter()
            .filter_map(|event| match event {
                Event::Disp(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        lines.join("\n")
    }

    /// Forget what happened so far, keeping the answers for what comes next.
    pub fn clear(&mut self) {
        self.events.clear();
    }
}

impl Host for RecordingHost {
    fn messagebox(&mut self, message: &str, title: &str) {
        self.events.push(Event::Message {
            message: message.to_string(),
            title: title.to_string(),
        });
    }

    fn yesnobox(&mut self, message: &str, title: &str) -> bool {
        self.events.push(Event::YesNo {
            message: message.to_string(),
            title: title.to_string(),
            yes: self.yes,
        });
        self.yes
    }

    fn inputbox(&mut self, message: &str, title: &str, _default: &str) -> Option<String> {
        self.events.push(Event::Input {
            message: message.to_string(),
            title: title.to_string(),
            masked: false,
            answer: self.input.clone(),
        });
        self.input.clone()
    }

    fn passwordbox(&mut self, message: &str, title: &str) -> Option<String> {
        self.events.push(Event::Input {
            message: message.to_string(),
            title: title.to_string(),
            masked: true,
            answer: self.input.clone(),
        });
        self.input.clone()
    }

    fn filenamebox(
        &mut self,
        title: &str,
        save: bool,
        _initial_dir: Option<&str>,
    ) -> Option<String> {
        self.events.push(Event::FileName {
            title: title.to_string(),
            save,
            answer: self.path.clone(),
        });
        self.path.clone()
    }

    fn dirnamebox(&mut self, title: &str, _initial_dir: Option<&str>) -> Option<String> {
        self.events.push(Event::DirName {
            title: title.to_string(),
            answer: self.path.clone(),
        });
        self.path.clone()
    }

    fn listbox(
        &mut self,
        message: &str,
        title: &str,
        items: &[String],
        selected: Option<i32>,
    ) -> Option<i32> {
        self.events.push(Event::List {
            message: message.to_string(),
            title: title.to_string(),
            items: items.to_vec(),
            selected,
            answer: self.choice,
        });
        self.choice
    }

    fn statusbox(&mut self, message: &str, title: &str) {
        self.events.push(Event::Status {
            message: message.to_string(),
            title: title.to_string(),
        });
    }

    fn closesbox(&mut self) {
        self.events.push(Event::CloseStatus);
    }

    fn setdlgpos(&mut self, x: Option<i32>, y: Option<i32>) {
        self.events.push(Event::DialogPosition { x, y });
    }

    fn dispstr(&mut self, text: &str) {
        self.events.push(Event::Disp(text.to_string()));
    }

    fn beep(&mut self, kind: i32) {
        self.events.push(Event::Beep { kind });
    }

    fn show_macro_window(&mut self, flag: i32) {
        self.events.push(Event::ShowMacroWindow { flag });
    }

    fn pause(&mut self, millis: u64) {
        self.waited_ms = self.waited_ms.saturating_add(millis);
        self.events.push(Event::Pause { millis });
    }

    fn read(&mut self, path: &str) -> Option<Vec<u8>> {
        self.sources
            .iter()
            .find(|(name, _)| name == path)
            .map(|(_, bytes)| bytes.clone())
    }

    fn flush_recv(&mut self) {
        self.queue.clear();
    }

    fn note_setting(&mut self, name: &str, value: &str) {
        self.events.push(Event::Setting {
            name: name.to_string(),
            value: value.to_string(),
        });
    }

    fn link_state(&mut self) -> i32 {
        self.link
    }

    fn connect(&mut self, parameters: &str) -> bool {
        self.events.push(Event::Connect {
            parameters: parameters.to_string(),
        });
        self.link = 2;
        true
    }

    fn disconnect(&mut self) {
        if self.link == 2 {
            self.link = 1;
        }
        self.events.push(Event::Disconnect);
    }

    fn unlink(&mut self) {
        self.link = 0;
        self.events.push(Event::Unlink);
    }

    fn close_terminal(&mut self) {
        self.link = 0;
        self.events.push(Event::CloseTerminal);
    }

    fn send(&mut self, bytes: &[u8]) {
        self.sent.extend_from_slice(bytes);
    }

    fn read_line(&mut self) -> Option<String> {
        if self.queue.is_empty() {
            return None;
        }
        Some(self.queue.remove(0))
    }

    fn wait_for(
        &mut self,
        _patterns: &[String],
        _millis: i64,
        _line: bool,
    ) -> Option<(usize, String)> {
        self.wait_answer.clone()
    }

    fn files(&mut self) -> &mut dyn crate::files::Files {
        &mut self.fs
    }

    fn getenv(&mut self, name: &str) -> Option<String> {
        self.env.get(name).cloned()
    }

    fn setenv(&mut self, name: &str, value: &str) {
        self.env.insert(name.to_string(), value.to_string());
    }

    fn uptime_millis(&mut self) -> i64 {
        self.uptime_ms
    }

    fn get_dir(&mut self) -> String {
        self.dir.clone()
    }

    fn set_dir(&mut self, dir: &str) {
        self.dir = dir.to_string();
    }

    fn exec(&mut self, command_line: &str, wait: bool) -> i32 {
        self.events.push(Event::Exec {
            command_line: command_line.to_string(),
            wait,
        });
        // The shell the macro asked for is not run: a recorder has no business
        // starting processes. Its exit code is the canned one.
        self.exit_code
    }

    fn special_folder(&mut self, name: &str) -> Option<String> {
        // A recorder answers rather than refusing: a macro that asks for a
        // folder and is told there is none takes a path its author never
        // exercised, and the transcript it produces is about a different macro.
        Some(
            self.special_folders
                .iter()
                .find(|(folder, _)| folder.eq_ignore_ascii_case(name))
                .map(|(_, path)| path.clone())
                .unwrap_or_else(|| format!("C:/Users/macro/{name}")),
        )
    }

    fn wait_bytes(&mut self, count: usize, _millis: i64) -> Option<Vec<u8>> {
        let text = self.wait_answer.as_ref()?.1.clone();
        let bytes = text.into_bytes();
        let take = count.min(bytes.len());
        Some(bytes[..take].to_vec())
    }

    fn wait_recv(
        &mut self,
        _pattern: &str,
        _len: usize,
        _position: usize,
        _millis: i64,
    ) -> Option<Vec<u8>> {
        self.wait_answer
            .as_ref()
            .map(|(_, text)| text.clone().into_bytes())
    }

    fn now(&self) -> DateTime {
        self.clock.unwrap_or(DateTime {
            year: 2026,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            weekday: 4,
            utc_offset_minutes: 0,
        })
    }
}
