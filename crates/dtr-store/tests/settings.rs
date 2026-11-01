use dtr_store::{SerialSettings, Store};
use tempfile::TempDir;

fn store() -> (TempDir, Store) {
    let dir = TempDir::new().expect("temp dir");
    let store = Store::open(&dir.path().join("state/dtr.redb")).expect("open");
    (dir, store)
}

#[test]
fn unknown_port_has_no_settings() {
    let (_dir, store) = store();
    assert_eq!(store.serial("/dev/ttyUSB0").expect("read"), None);
}

#[test]
fn settings_round_trip() {
    let (_dir, store) = store();
    let settings = SerialSettings {
        baud: 9600,
        cr_to_crlf: false,
    };
    store
        .remember_serial("/dev/ttyUSB0", &settings)
        .expect("write");
    assert_eq!(store.serial("/dev/ttyUSB0").expect("read"), Some(settings));
}

#[test]
fn ports_are_independent() {
    let (_dir, store) = store();
    store
        .remember_serial(
            "COM3",
            &SerialSettings {
                baud: 9600,
                cr_to_crlf: true,
            },
        )
        .expect("write");
    store
        .remember_serial(
            "/dev/ttyACM0",
            &SerialSettings {
                baud: 57600,
                cr_to_crlf: false,
            },
        )
        .expect("write");

    assert_eq!(
        store.serial("COM3").expect("read").map(|s| s.baud),
        Some(9600)
    );
    assert_eq!(
        store.serial("/dev/ttyACM0").expect("read").map(|s| s.baud),
        Some(57600)
    );
    assert_eq!(store.serial("COM4").expect("read"), None);
}

#[test]
fn remembering_again_overwrites() {
    let (_dir, store) = store();
    store
        .remember_serial(
            "COM3",
            &SerialSettings {
                baud: 9600,
                cr_to_crlf: true,
            },
        )
        .expect("write");
    store
        .remember_serial(
            "COM3",
            &SerialSettings {
                baud: 115_200,
                cr_to_crlf: false,
            },
        )
        .expect("write");
    assert_eq!(
        store.serial("COM3").expect("read"),
        Some(SerialSettings {
            baud: 115_200,
            cr_to_crlf: false
        })
    );
}

#[test]
fn settings_survive_a_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("dtr.redb");
    {
        let store = Store::open(&path).expect("open");
        store
            .remember_serial(
                "/dev/ttyS0",
                &SerialSettings {
                    baud: 4800,
                    cr_to_crlf: true,
                },
            )
            .expect("write");
    }
    let store = Store::open(&path).expect("reopen");
    assert_eq!(
        store.serial("/dev/ttyS0").expect("read").map(|s| s.baud),
        Some(4800)
    );
}

#[test]
fn last_port_is_remembered() {
    let (_dir, store) = store();
    assert_eq!(store.last_port().expect("read"), None);
    store.set_last_port("/dev/ttyUSB1").expect("write");
    assert_eq!(
        store.last_port().expect("read").as_deref(),
        Some("/dev/ttyUSB1")
    );
}
