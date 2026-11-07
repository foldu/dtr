use dtr_store::{DataBits, FlowControl, Parity, SerialSettings, StopBits, Store};
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
        ..SerialSettings::default()
    };
    store
        .remember_serial("/dev/ttyUSB0", &settings)
        .expect("write");
    assert_eq!(store.serial("/dev/ttyUSB0").expect("read"), Some(settings));
}

/// Every field makes the trip, not just the two the settings menu can edit.
#[test]
fn every_framing_field_round_trips() {
    let (_dir, store) = store();
    let settings = SerialSettings {
        baud: 57_600,
        data_bits: DataBits::Seven,
        parity: Parity::Even,
        stop_bits: StopBits::Two,
        flow_control: FlowControl::Hardware,
        dtr: false,
        rts: true,
        cr_to_crlf: false,
        auto_reconnect: false,
        profile: Some("gps module".to_owned()),
    };
    store
        .remember_serial("/dev/ttyACM0", &settings)
        .expect("write");
    assert_eq!(store.serial("/dev/ttyACM0").expect("read"), Some(settings));
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
                ..SerialSettings::default()
            },
        )
        .expect("write");
    store
        .remember_serial(
            "/dev/ttyACM0",
            &SerialSettings {
                baud: 57600,
                cr_to_crlf: false,
                ..SerialSettings::default()
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
                ..SerialSettings::default()
            },
        )
        .expect("write");
    store
        .remember_serial(
            "COM3",
            &SerialSettings {
                baud: 115_200,
                cr_to_crlf: false,
                ..SerialSettings::default()
            },
        )
        .expect("write");
    assert_eq!(
        store.serial("COM3").expect("read"),
        Some(SerialSettings {
            baud: 115_200,
            cr_to_crlf: false,
            ..SerialSettings::default()
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
                    ..SerialSettings::default()
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
fn a_profile_is_written_and_read_back_by_name() {
    let (_dir, store) = store();
    let settings = SerialSettings {
        baud: 9_600,
        data_bits: DataBits::Seven,
        parity: Parity::Even,
        profile: Some("ignored; the name is the key".to_owned()),
        ..SerialSettings::default()
    };
    store.save_profile("gps module", &settings).expect("write");
    store
        .save_profile("bench board", &SerialSettings::default())
        .expect("write");

    let profiles = store.profiles().expect("read");
    assert_eq!(
        profiles
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["bench board", "gps module"]
    );
    let gps = &profiles[1].1;
    assert_eq!(gps.baud, 9_600);
    assert_eq!(gps.parity, Parity::Even);
    assert_eq!(gps.profile.as_deref(), Some("gps module"));
}

#[test]
fn profiles_survive_a_reopen() {
    let dir = TempDir::new().expect("temp dir");
    let path = dir.path().join("dtr.redb");
    {
        let store = Store::open(&path).expect("open");
        store
            .save_profile(
                "gps module",
                &SerialSettings {
                    baud: 9_600,
                    parity: Parity::Even,
                    ..SerialSettings::default()
                },
            )
            .expect("write");
    }
    let store = Store::open(&path).expect("reopen");
    let profiles = store.profiles().expect("read");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].0, "gps module");
    assert_eq!(profiles[0].1.baud, 9_600);
    assert_eq!(profiles[0].1.parity, Parity::Even);
}

#[test]
fn saving_the_same_profile_name_replaces_it() {
    let (_dir, store) = store();
    store
        .save_profile(
            "bench",
            &SerialSettings {
                baud: 9_600,
                ..SerialSettings::default()
            },
        )
        .expect("write");
    store
        .save_profile(
            "bench",
            &SerialSettings {
                baud: 115_200,
                ..SerialSettings::default()
            },
        )
        .expect("write");
    let profiles = store.profiles().expect("read");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].1.baud, 115_200);
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
