//! The relay's stderr log: one line per event, each starting with the UTC
//! time in the form the supervisor's own lines use
//! (`2026-10-28T15:15:22.068802Z`), so the two read as one timeline.

use std::time::{SystemTime, UNIX_EPOCH};

/// Writes one timestamped line to stderr, like `eprintln!`.
#[allow(unused_macros)]
macro_rules! log {
    ($($arg:tt)*) => {
        eprintln!("{} {}", $crate::log::now(), format_args!($($arg)*))
    };
}

/// The current UTC time, to the microsecond.
pub(crate) fn now() -> String {
    let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    format_utc(since.as_secs(), since.subsec_micros())
}

/// `secs` since the Unix epoch as `YYYY-MM-DDTHH:MM:SS.uuuuuuZ`.
fn format_utc(secs: u64, micros: u32) -> String {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Civil date from a day count (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{micros:06}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::format_utc;

    #[test]
    fn formats_the_epoch_and_a_known_instant() {
        assert_eq!(format_utc(0, 0), "1970-01-01T00:00:00.000000Z");
        // 2026-10-28T15:15:22.068802Z, from the supervisor's log.
        assert_eq!(format_utc(1_793_200_522, 68_802), "2026-10-28T15:15:22.068802Z");
    }

    #[test]
    fn handles_a_leap_day() {
        // 2024-02-29T23:59:59Z
        assert_eq!(format_utc(1_709_251_199, 5), "2024-02-29T23:59:59.000005Z");
    }
}
