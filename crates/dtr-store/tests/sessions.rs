use dtr_store::{SessionRecord, Store, now_ms};
use tempfile::TempDir;

fn store() -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(&dir.path().join("dtr.redb")).expect("open");
    (dir, store)
}

fn record(label: &str, note: Option<&str>) -> SessionRecord {
    SessionRecord {
        label: label.to_owned(),
        port: Some(label.to_owned()),
        baud: Some(115_200),
        imap: Some("cr=crlf".to_owned()),
        started_at: 1_700_000_000_000,
        note: note.map(str::to_owned),
        ..SessionRecord::default()
    }
}

#[test]
fn an_empty_logbook_has_no_sessions() {
    let (_dir, store) = store();
    assert!(store.sessions().expect("read").is_empty());
}

#[test]
fn a_session_round_trips() {
    let (_dir, store) = store();
    let id = store
        .begin_session(&record("/dev/ttyUSB0", Some("sensor board")))
        .expect("begin");
    store
        .end_session(id, 1_700_000_012_345, "closed")
        .expect("end");

    let sessions = store.sessions().expect("read");
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];
    assert_eq!(session.id, id);
    assert_eq!(session.label, "/dev/ttyUSB0");
    assert_eq!(session.baud, Some(115_200));
    assert_eq!(session.note.as_deref(), Some("sensor board"));
    assert_eq!(session.exit.as_deref(), Some("closed"));
    assert_eq!(session.duration_ms(), Some(12_345));
}

#[test]
fn a_session_without_an_end_has_no_duration() {
    let (_dir, store) = store();
    store.begin_session(&record("COM3", None)).expect("begin");
    let sessions = store.sessions().expect("read");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].duration_ms(), None);
    assert_eq!(sessions[0].exit, None);
}

#[test]
fn notes_may_contain_newlines_and_equals_signs() {
    let (_dir, store) = store();
    let note = "line one\nline two = not a field\\nor an escape";
    let id = store
        .begin_session(&record("COM3", Some(note)))
        .expect("begin");
    store
        .end_session(id, 1_700_000_001_000, "closed")
        .expect("end");

    let sessions = store.sessions().expect("read");
    assert_eq!(sessions[0].note.as_deref(), Some(note));
}

#[test]
fn sessions_are_listed_oldest_first() {
    let (_dir, store) = store();
    let mut first = record("A", None);
    first.started_at = 1_000;
    let mut second = record("B", None);
    second.started_at = 2_000;
    store.begin_session(&first).expect("begin");
    store.begin_session(&second).expect("begin");

    let labels: Vec<_> = store
        .sessions()
        .expect("read")
        .into_iter()
        .map(|session| session.label)
        .collect();
    assert_eq!(labels, ["A", "B"]);
}

#[test]
fn two_sessions_in_the_same_millisecond_get_distinct_ids() {
    let (_dir, store) = store();
    let mut first = record("A", None);
    first.started_at = 5_000;
    let mut second = record("B", None);
    second.started_at = 5_000;

    let a = store.begin_session(&first).expect("begin");
    let b = store.begin_session(&second).expect("begin");
    assert_ne!(a, b);
    assert_eq!(store.sessions().expect("read").len(), 2);
}

#[test]
fn ending_an_unknown_session_is_not_an_error() {
    let (_dir, store) = store();
    store.end_session(999, now_ms(), "closed").expect("no-op");
    assert!(store.sessions().expect("read").is_empty());
}

#[test]
fn sessions_survive_a_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("dtr.redb");
    let id = {
        let store = Store::open(&path).expect("open");
        let id = store
            .begin_session(&record("COM7", Some("kept")))
            .expect("begin");
        store
            .end_session(id, 1_700_000_005_000, "closed")
            .expect("end");
        id
    };
    let store = Store::open(&path).expect("reopen");
    let sessions = store.sessions().expect("read");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, id);
    assert_eq!(sessions[0].note.as_deref(), Some("kept"));
}
