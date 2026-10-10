//! When to look again at a pull request that waits for a person's review.
//!
//! A review takes hours to days, so the check is seldom and follows the
//! reviewers' day: every [`ReviewSchedule::interval_minutes`] inside the
//! working day, and outside it (nights, weekends) one check shortly before
//! the day begins, never more than `off_hours_interval_minutes` apart.
//! Pure: the caller passes the time and the UTC offset.

use chrono::{DateTime, Datelike, Duration, FixedOffset, Timelike, Utc, Weekday};
use tod_store::settings::ReviewSchedule;

/// The offset from UTC that `schedule` means at `now`: its own, else this
/// machine's.
pub fn offset_for(schedule: &ReviewSchedule, now: DateTime<Utc>) -> FixedOffset {
    schedule
        .utc_offset_minutes
        .and_then(|m| FixedOffset::east_opt(m.saturating_mul(60)))
        .unwrap_or_else(|| *now.with_timezone(&chrono::Local).offset())
}

/// Whether `at` falls in the working day.
fn working(at: DateTime<FixedOffset>, schedule: &ReviewSchedule) -> bool {
    if schedule.weekdays_only && matches!(at.weekday(), Weekday::Sat | Weekday::Sun) {
        return false;
    }
    (schedule.start_hour..schedule.end_hour).contains(&at.hour())
}

/// The start of the next working day after `at` (its start hour, local).
fn next_day_start(at: DateTime<FixedOffset>, schedule: &ReviewSchedule) -> DateTime<FixedOffset> {
    let mut day = at.date_naive();
    for _ in 0..8 {
        let candidate = day
            .and_hms_opt(schedule.start_hour.min(23), 0, 0)
            .and_then(|t| t.and_local_timezone(*at.offset()).single());
        if let Some(c) = candidate
            && c > at
            && (!schedule.weekdays_only || !matches!(c.weekday(), Weekday::Sat | Weekday::Sun))
        {
            return c;
        }
        day = day.succ_opt().unwrap_or(day);
    }
    at + Duration::hours(24)
}

/// When to check next, given that the pull request was read at `now`.
pub fn next_check(now: DateTime<Utc>, schedule: &ReviewSchedule) -> DateTime<Utc> {
    let offset = offset_for(schedule, now);
    let local = now.with_timezone(&offset);
    let interval = Duration::minutes(schedule.interval_minutes.max(5) as i64);
    let off_cap = Duration::minutes(schedule.off_hours_interval_minutes.max(schedule.interval_minutes).max(5) as i64);
    let candidate = local + interval;
    let next = if working(candidate, schedule) {
        candidate
    } else {
        // Outside the working day: the morning's first check, or the cap,
        // whichever is sooner.
        let morning = next_day_start(local, schedule);
        morning.min(local + off_cap)
    };
    next.with_timezone(&Utc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule() -> ReviewSchedule {
        ReviewSchedule { utc_offset_minutes: Some(0), ..ReviewSchedule::default() }
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn inside_the_working_day_it_is_the_interval() {
        // Thursday 2026-01-01 is a holiday date but a Thursday.
        let next = next_check(at("2026-01-01T10:00:00Z"), &schedule());
        assert_eq!(next, at("2026-01-01T11:30:00Z"));
    }

    #[test]
    fn near_the_end_of_the_day_it_runs_to_the_next_morning_or_the_cap() {
        // 18:00 + 90m = 19:30, outside; morning is 14h away, the cap 12h.
        let next = next_check(at("2026-01-01T18:00:00Z"), &schedule());
        assert_eq!(next, at("2026-01-02T06:00:00Z"));
        // At 22:00 the morning (08:00) is 10h away: sooner than the cap.
        let night = next_check(at("2026-01-01T22:00:00Z"), &schedule());
        assert_eq!(night, at("2026-01-02T08:00:00Z"));
    }

    #[test]
    fn a_weekend_is_skipped_to_monday_morning_in_capped_steps() {
        // Friday 2026-01-02 17:00 + 90m = 18:30, still working.
        assert_eq!(next_check(at("2026-01-02T17:00:00Z"), &schedule()), at("2026-01-02T18:30:00Z"));
        // Friday 18:00 -> 19:30 is off; Monday 08:00 is far, so the cap: +12h.
        assert_eq!(next_check(at("2026-01-02T18:00:00Z"), &schedule()), at("2026-01-03T06:00:00Z"));
        // Saturday 06:00 -> +12h = 18:00 Saturday.
        assert_eq!(next_check(at("2026-01-03T06:00:00Z"), &schedule()), at("2026-01-03T18:00:00Z"));
        // Sunday 20:00: Monday 08:00 is 12h away.
        assert_eq!(next_check(at("2026-01-04T20:00:00Z"), &schedule()), at("2026-01-05T08:00:00Z"));
    }

    #[test]
    fn the_offset_moves_the_day() {
        let mut s = schedule();
        s.utc_offset_minutes = Some(-300); // UTC-5: 10:00 UTC is 05:00 local.
        let next = next_check(at("2026-01-01T10:00:00Z"), &s);
        // 05:00 local is off hours; the morning (08:00 local = 13:00 UTC) is 3h away.
        assert_eq!(next, at("2026-01-01T13:00:00Z"));
    }

    #[test]
    fn it_is_always_later_than_now() {
        let s = schedule();
        let mut t = at("2026-01-01T00:00:00Z");
        for _ in 0..24 * 14 {
            assert!(next_check(t, &s) > t);
            t += Duration::hours(1);
        }
    }
}
