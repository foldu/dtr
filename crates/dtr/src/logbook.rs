//! `dtr --logbook`: what happened, newest first.
//!
//! Times are printed in UTC. Converting to the local zone needs a tz database
//! and a decision about what "local" means for a logbook you may read on
//! another machine; the absolute instant is the honest thing to store and show.

use dtr_store::{SessionRecord, Store};

pub fn print(store: &Store) -> anyhow::Result<()> {
    let mut sessions = store.sessions()?;
    if sessions.is_empty() {
        println!("the logbook is empty; sessions are recorded as they start");
        return Ok(());
    }

    // Newest first: the session you want is almost always the last one.
    sessions.reverse();

    let mut rows: Vec<[String; 5]> = Vec::with_capacity(sessions.len());
    for session in &sessions {
        rows.push([
            timestamp(session.started_at),
            duration(session),
            session.label.clone(),
            session.exit.clone().unwrap_or_else(|| "unfinished".to_owned()),
            session.note.clone().unwrap_or_default(),
        ]);
    }

    let headers = ["WHEN (UTC)", "DURATION", "TRANSPORT", "EXIT", "NOTE"];
    let mut widths = headers.map(str::len);
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }

    let line = |cells: [&str; 5]| {
        let mut out = String::new();
        for (index, cell) in cells.iter().enumerate() {
            if index > 0 {
                out.push_str("  ");
            }
            out.push_str(cell);
            if index + 1 < cells.len() {
                for _ in cell.chars().count()..widths[index] {
                    out.push(' ');
                }
            }
        }
        out
    };

    println!("{}", line(headers));
    for row in &rows {
        println!(
            "{}",
            line([
                row[0].as_str(),
                row[1].as_str(),
                row[2].as_str(),
                row[3].as_str(),
                row[4].as_str(),
            ])
        );
    }
    println!("\n{} session(s)", sessions.len());
    Ok(())
}

fn duration(session: &SessionRecord) -> String {
    match session.duration_ms() {
        Some(ms) => human_duration(ms),
        None => "—".to_owned(),
    }
}

fn human_duration(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, (seconds / 60) % 60, seconds % 60);
    let days = hours / 24;
    if days > 0 {
        format!("{days}d{}h", hours % 24)
    } else if hours > 0 {
        format!("{hours}h{minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// `2026-10-27 17:20:11Z` from milliseconds since the Unix epoch.
fn timestamp(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    let days = (seconds / 86_400) as i64;
    let of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        of_day / 3600,
        (of_day / 60) % 60,
        of_day % 60
    )
}

/// Howard Hinnant's days-to-civil conversion: exact, no lookup tables.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::{civil_from_days, human_duration, timestamp};

    #[test]
    fn epoch_is_1970() {
        assert_eq!(timestamp(0), "1970-01-01 00:00:00Z");
    }

    #[test]
    fn known_instants_convert() {
        // `date -u -d "2026-10-27 17:20:11" +%s` = 1793121611
        assert_eq!(timestamp(1_793_121_611_000), "2026-10-27 17:20:11Z");
        // The instant I first wrote down by hand, kept because it caught the
        // mistake: 1793131211 is 20:00:11Z, not 17:20:11Z.
        assert_eq!(timestamp(1_793_131_211_000), "2026-10-27 20:00:11Z");
    }

    #[test]
    fn leap_day_exists() {
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human_duration(9_000), "9s");
        assert_eq!(human_duration(72_000), "1m12s");
        assert_eq!(human_duration(3_600_000), "1h00m");
        assert_eq!(human_duration(90_000_000), "1d1h");
    }
}
