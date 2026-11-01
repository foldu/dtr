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

    /// The local clock for `gettime`/`getdate`.
    fn now(&self) -> DateTime;
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
    pub files: Vec<(String, Vec<u8>)>,
    /// Total time the macro asked to wait.
    pub waited_ms: u64,
    /// The clock `gettime` reads, when a test cares what it says.
    pub clock: Option<DateTime>,
}

impl RecordingHost {
    pub fn new() -> Self {
        Self::default()
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
        self.files.push((path.to_string(), bytes.to_vec()));
        self
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
        self.files
            .iter()
            .find(|(name, _)| name == path)
            .map(|(_, bytes)| bytes.clone())
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
