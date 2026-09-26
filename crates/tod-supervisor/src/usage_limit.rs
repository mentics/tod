//! Claude's usage limits (design: "Usage limits").
//!
//! When the subscription's limit is reached, Claude Code ends the turn with
//! a message instead of work — as the turn's error through
//! `claude-code-acp`, or as its reply text. The shapes seen so far:
//!
//! - `Claude usage limit reached. Your limit will reset at 3pm (America/New_York).`
//! - `5-hour limit reached ∙ resets 2am`
//! - `Claude AI usage limit reached|1760000000` (the reset as epoch seconds)
//!
//! [`detect`] recognises one and reads when it resets; the supervisor then
//! records an `until` wait for that time ([`record`]) and sleeps like for
//! any other wait. A reset time that cannot be read is taken as an hour
//! from now: waking early only finds the limit again and waits again.

use anyhow::Result;
use chrono::{DateTime, Duration as ChronoDuration, FixedOffset, NaiveTime, TimeZone, Utc};
use tod_store::fleet::FleetStore;
use tod_store::interview::InterviewCommand;
use tod_store::waits::NewWait;
use uuid::Uuid;

/// When an unreadable reset is assumed to be.
pub const FALLBACK: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// A usage limit found in an agent's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageLimit {
    /// When it resets, ms since the epoch.
    pub reset_at_ms: i64,
    /// Whether that was read from the message (else it is [`FALLBACK`]).
    pub parsed: bool,
}

/// Whether `text` says a usage limit was reached, and when it resets.
pub fn detect(text: &str, now_ms: i64) -> Option<UsageLimit> {
    let lower = text.to_lowercase();
    let is_limit = lower.contains("usage limit reached")
        || lower.contains("hour limit reached")
        || lower.contains("weekly limit reached")
        || lower.contains("hit your limit")
        || (lower.contains("limit reached") && lower.contains("reset"));
    if !is_limit {
        return None;
    }
    let reset = epoch_suffix(text).or_else(|| clock_time(&lower, now_ms)).filter(|at| *at > now_ms);
    Some(match reset {
        Some(reset_at_ms) => UsageLimit { reset_at_ms, parsed: true },
        None => UsageLimit { reset_at_ms: now_ms + FALLBACK.as_millis() as i64, parsed: false },
    })
}

/// `…|1760000000` (seconds) or `…|1760000000000` (ms).
fn epoch_suffix(text: &str) -> Option<i64> {
    let (_, tail) = text.trim().rsplit_once('|')?;
    let digits: String = tail.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
    let n: i64 = digits.parse().ok()?;
    match digits.len() {
        10 => Some(n * 1000),
        13 => Some(n),
        _ => None,
    }
}

/// `resets 2am`, `reset at 3:30pm (Europe/Berlin)`, `resets 15:00 (UTC)`:
/// the next such time after `now_ms`.
fn clock_time(lower: &str, now_ms: i64) -> Option<i64> {
    let at = lower.find("reset")?;
    let rest = &lower[at..];
    // Skip "resets"/"reset" and an optional "at".
    let mut words = rest.split_whitespace().skip(1).peekable();
    if words.peek() == Some(&"at") {
        words.next();
    }
    let token = words.next()?.trim_end_matches(['.', ',', ')']);
    let time = parse_clock(token)?;
    let offset = match rest.find('(').zip(rest.find(')')) {
        Some((open, close)) if open < close => zone_offset(rest[open + 1..close].trim())?,
        _ => FixedOffset::east_opt(0)?,
    };
    let now: DateTime<FixedOffset> = Utc.timestamp_millis_opt(now_ms).single()?.with_timezone(&offset);
    let mut candidate = offset.from_local_datetime(&now.date_naive().and_time(time)).single()?;
    if candidate <= now {
        candidate += ChronoDuration::days(1);
    }
    Some(candidate.timestamp_millis())
}

/// `3pm`, `3:30pm`, `11 am`-less forms, `15:00`.
fn parse_clock(token: &str) -> Option<NaiveTime> {
    let (body, meridiem) = if let Some(b) = token.strip_suffix("am") {
        (b, Some(false))
    } else if let Some(b) = token.strip_suffix("pm") {
        (b, Some(true))
    } else {
        (token, None)
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (body.parse::<u32>().ok()?, 0),
    };
    let h = match meridiem {
        Some(pm) => {
            if h == 0 || h > 12 {
                return None;
            }
            (h % 12) + if pm { 12 } else { 0 }
        }
        None if body.contains(':') => h,
        // A bare number with no am/pm is not a time.
        None => return None,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

/// Standard-time offsets of the zones Claude reports most. Daylight saving
/// is ignored: in summer that reads the reset an hour late (never early),
/// which only costs an hour of sleep.
fn zone_offset(zone: &str) -> Option<FixedOffset> {
    let minutes = match zone.to_ascii_lowercase().as_str() {
        "utc" | "etc/utc" | "gmt" | "europe/london" | "europe/dublin" | "europe/lisbon" => 0,
        "america/new_york" | "america/toronto" | "america/detroit" | "us/eastern" => -5 * 60,
        "america/chicago" | "us/central" | "america/mexico_city" => -6 * 60,
        "america/denver" | "america/phoenix" | "us/mountain" => -7 * 60,
        "america/los_angeles" | "america/vancouver" | "us/pacific" => -8 * 60,
        "america/sao_paulo" => -3 * 60,
        "europe/berlin" | "europe/paris" | "europe/amsterdam" | "europe/madrid" | "europe/rome"
        | "europe/stockholm" | "europe/warsaw" | "europe/zurich" => 60,
        "europe/helsinki" | "europe/kiev" | "europe/kyiv" | "europe/athens" => 2 * 60,
        "asia/kolkata" | "asia/calcutta" => 5 * 60 + 30,
        "asia/singapore" | "asia/shanghai" | "asia/hong_kong" => 8 * 60,
        "asia/tokyo" | "asia/seoul" => 9 * 60,
        "australia/sydney" | "australia/melbourne" => 10 * 60,
        _ => return None,
    };
    FixedOffset::east_opt(minutes * 60)
}

/// Records an `until` wait on `node` for the reset.
pub fn record(fleet: &FleetStore, node: Uuid, limit: &UsageLimit) -> Result<()> {
    fleet
        .interview(
            crate::waits::ACTOR,
            InterviewCommand::RecordWait { node_id: node, wait: NewWait::until(limit.reset_at_ms) },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-09-26 12:00:00 UTC.
    const NOON: i64 = 1_790_424_000_000;

    fn utc(h: u32, m: u32, day_offset: i64) -> i64 {
        NOON - 12 * 3_600_000 + day_offset * 86_400_000 + (h as i64 * 60 + m as i64) * 60_000
    }

    #[test]
    fn noon_is_noon() {
        assert_eq!(Utc.timestamp_millis_opt(NOON).unwrap().to_rfc3339(), "2026-09-26T12:00:00+00:00");
    }

    #[test]
    fn reads_the_named_zone() {
        let text = "Claude usage limit reached. Your limit will reset at 3pm (America/New_York).";
        // 3pm at -5 is 20:00 UTC, later today.
        assert_eq!(detect(text, NOON), Some(UsageLimit { reset_at_ms: utc(20, 0, 0), parsed: true }));
    }

    #[test]
    fn a_time_already_past_is_tomorrow() {
        let text = "5-hour limit reached ∙ resets 2am";
        assert_eq!(detect(text, NOON), Some(UsageLimit { reset_at_ms: utc(2, 0, 1), parsed: true }));
    }

    #[test]
    fn minutes_and_24_hour_clocks() {
        let text = "Claude usage limit reached. Your limit will reset at 3:30pm (Asia/Tokyo).";
        // 15:30 at +9 is 06:30 UTC: past today, so tomorrow.
        assert_eq!(detect(text, NOON).unwrap().reset_at_ms, utc(6, 30, 1));
        let text = "Weekly limit reached, resets 18:45 (UTC)";
        assert_eq!(detect(text, NOON).unwrap().reset_at_ms, utc(18, 45, 0));
    }

    #[test]
    fn reads_the_epoch_suffix() {
        let reset = NOON / 1000 + 7200;
        let text = format!("Claude AI usage limit reached|{reset}");
        assert_eq!(detect(&text, NOON), Some(UsageLimit { reset_at_ms: reset * 1000, parsed: true }));
    }

    #[test]
    fn an_unreadable_reset_is_an_hour() {
        for text in [
            "Claude usage limit reached. Your limit will reset soon.",
            "Claude usage limit reached. Your limit will reset at 3pm (Mars/Olympus_Mons).",
            "API Error: You've hit your limit",
        ] {
            assert_eq!(
                detect(text, NOON),
                Some(UsageLimit { reset_at_ms: NOON + 3_600_000, parsed: false }),
                "{text}"
            );
        }
    }

    #[test]
    fn other_failures_are_not_limits() {
        for text in ["connection reset by peer", "rate limit exceeded", "agent hung: no activity", ""] {
            assert_eq!(detect(text, NOON), None, "{text}");
        }
    }
}
