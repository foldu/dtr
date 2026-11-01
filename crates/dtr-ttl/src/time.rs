//! `gettime`/`getdate`'s `strftime` formatting (§7.6).
//!
//! A macro that logs wants a file name like `%Y%m%d-%H%M%S`, so the directives
//! that appear in real macros are implemented and the rest pass through as
//! text: an unknown directive is not an error, because Tera Term would print it
//! too. The calendar arithmetic is Howard Hinnant's `days_from_civil` /
//! `civil_from_days`, which is the standard branch-free pair and is exact for
//! every year a 32-bit time can hold.

use crate::host::DateTime;

/// Days since 1970-01-01 for a civil date.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month = i64::from(month);
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date for a day count, with 0 = Sunday.
fn civil_from_days(day_count: i64) -> (i32, u32, u32, u32) {
    // 1970-01-01 was a Thursday, so day 0 is weekday 4. This has to be taken
    // from the caller's count, not from the shifted one below.
    let weekday = day_count.rem_euclid(7) as u32;
    let weekday = (weekday + 4) % 7;
    let days = day_count + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let doe = days - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = (year + i64::from(month <= 2)) as i32;
    (year, month, day, weekday)
}

/// Now, in UTC, from the system clock. `None` if the clock is before 1970.
pub fn now_utc() -> Option<DateTime> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(from_unix(seconds, 0))
}

/// A `DateTime` from a Unix timestamp and a UTC offset in minutes.
pub fn from_unix(seconds: i64, offset_minutes: i32) -> DateTime {
    let local = seconds + i64::from(offset_minutes) * 60;
    let days = local.div_euclid(86_400);
    let of_day = local.rem_euclid(86_400);
    let (year, month, day, weekday) = civil_from_days(days);
    DateTime {
        year,
        month,
        day,
        hour: (of_day / 3600) as u32,
        minute: (of_day % 3600 / 60) as u32,
        second: (of_day % 60) as u32,
        weekday,
        utc_offset_minutes: offset_minutes,
    }
}

/// The Unix timestamp of a `DateTime`, for `uptime`-style arithmetic.
pub fn to_unix(date: DateTime) -> i64 {
    let days = days_from_civil(date.year, date.month, date.day);
    days * 86_400 + i64::from(date.hour) * 3600 + i64::from(date.minute) * 60 + i64::from(date.second)
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// Format `date` with a `strftime`-style format. Unknown directives are copied
/// through unchanged rather than refused: a macro that logs `%q` gets `%q`.
pub fn strftime(format: &str, date: DateTime) -> String {
    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        let Some(directive) = chars.next() else {
            out.push('%');
            break;
        };
        let month = (date.month as usize).clamp(1, 12) - 1;
        let weekday = (date.weekday as usize).min(6);
        match directive {
            'Y' => out.push_str(&date.year.to_string()),
            'y' => out.push_str(&format!("{:02}", date.year.rem_euclid(100))),
            'm' => out.push_str(&format!("{:02}", date.month)),
            'd' => out.push_str(&format!("{:02}", date.day)),
            'e' => out.push_str(&format!("{:2}", date.day)),
            'H' => out.push_str(&format!("{:02}", date.hour)),
            'M' => out.push_str(&format!("{:02}", date.minute)),
            'S' => out.push_str(&format!("{:02}", date.second)),
            'j' => {
                let days = days_from_civil(date.year, date.month, date.day)
                    - days_from_civil(date.year, 1, 1);
                out.push_str(&format!("{:03}", days + 1));
            }
            'b' | 'h' => out.push_str(&MONTHS[month][..3]),
            'B' => out.push_str(MONTHS[month]),
            'a' => out.push_str(&WEEKDAYS[weekday][..3]),
            'A' => out.push_str(WEEKDAYS[weekday]),
            'w' => out.push_str(&date.weekday.to_string()),
            'u' => out.push_str(&if date.weekday == 0 { 7 } else { date.weekday }.to_string()),
            'p' => out.push_str(if date.hour < 12 { "AM" } else { "PM" }),
            'P' => out.push_str(if date.hour < 12 { "am" } else { "pm" }),
            'z' => {
                let offset = date.utc_offset_minutes;
                let sign = if offset < 0 { '-' } else { '+' };
                let offset = offset.abs();
                out.push_str(&format!("{sign}{:02}{:02}", offset / 60, offset % 60));
            }
            'Z' => out.push_str(if date.utc_offset_minutes == 0 { "UTC" } else { "" }),
            's' => out.push_str(&to_unix(date).to_string()),
            '%' => out.push('%'),
            other => {
                out.push('%');
                out.push(other);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_is_a_thursday() {
        let date = from_unix(0, 0);
        assert_eq!(
            (date.year, date.month, date.day, date.weekday),
            (1970, 1, 1, 4)
        );
    }

    #[test]
    fn a_known_timestamp_round_trips() {
        // 2024-02-29T12:34:56Z, a leap day, which is where calendar code breaks.
        let date = from_unix(1_709_209_496, 0);
        assert_eq!(
            (date.year, date.month, date.day),
            (2024, 2, 29),
            "{date:?}"
        );
        assert_eq!(to_unix(date), 1_709_209_496);
    }

    #[test]
    fn the_directives_a_logging_macro_uses() {
        let date = from_unix(1_709_209_496, 0);
        assert_eq!(strftime("%Y%m%d-%H%M%S", date), "20240229-122456");
        assert_eq!(strftime("%H:%M:%S", date), "12:24:56");
        assert_eq!(strftime("%Y-%m-%d", date), "2024-02-29");
        assert_eq!(strftime("%a %b %e", date), "Thu Feb 29");
        assert_eq!(strftime("%A, %B %d", date), "Thursday, February 29");
        assert_eq!(strftime("100%%", date), "100%");
        assert_eq!(strftime("%q", date), "%q", "unknown directives pass through");
    }

    #[test]
    fn a_positive_offset_moves_the_clock_forward() {
        let date = from_unix(0, 90);
        assert_eq!((date.hour, date.minute), (1, 30));
        assert_eq!(strftime("%z", date), "+0130");
    }

    #[test]
    fn the_day_of_year_counts_leap_days() {
        assert_eq!(strftime("%j", from_unix(1_709_209_496, 0)), "060");
    }
}
