//! Quota reset times and provider selection for TOD sessions.

use anyhow::{Context, Result, bail};
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, FixedOffset, Local, NaiveDate, NaiveDateTime,
    NaiveTime, TimeZone, Utc,
};
use fs2::FileExt;
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;
use tod_agent::{AgentLaunchOptions, AgentPlatform};
/// When an unreadable reset is assumed to be.
pub const FALLBACK: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// A usage limit found in an agent's message.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
        || lower.contains("usage_limit_reached")
        || lower.contains("hit your usage limit")
        || lower.contains("reached your usage limit")
        || lower.contains("hour limit reached")
        || lower.contains("weekly limit reached")
        || lower.contains("hit your limit");
    if !is_limit {
        return None;
    }
    let reset = epoch_suffix(text)
        .or_else(|| structured_reset(text))
        .or_else(|| {
            text.split_whitespace().find_map(|word| {
                DateTime::parse_from_rfc3339(word.trim_matches([',', '.', '(', ')', '"']))
                    .ok()
                    .map(|date| date.timestamp_millis())
            })
        })
        .or_else(|| codex_retry_time(&lower, now_ms))
        .or_else(|| clock_time(&lower, now_ms))
        .filter(|at| *at > now_ms);
    Some(match reset {
        Some(reset_at_ms) => UsageLimit {
            reset_at_ms,
            parsed: true,
        },
        None => UsageLimit {
            reset_at_ms: now_ms + FALLBACK.as_millis() as i64,
            parsed: false,
        },
    })
}

/// Some adapters return the quota banner as an otherwise successful reply.
pub fn banner(text: &str, now_ms: i64) -> Option<UsageLimit> {
    let first = text.trim().to_ascii_lowercase();
    let first = first.trim_start_matches("error: ");
    let starts_like_banner = [
        "usage limit reached",
        "you've hit your",
        "you have hit your",
        "hit your limit",
        "weekly limit reached",
        "5-hour limit reached",
        "5 hour limit reached",
        "you've reached your usage limit",
        "you have reached your usage limit",
    ]
    .iter()
    .any(|prefix| first.starts_with(prefix));
    (text.len() < 400 && starts_like_banner)
        .then(|| detect(text, now_ms))
        .flatten()
}

const REGISTRY: &str = "provider-quota.json";
type Resets = HashMap<AgentPlatform, UsageLimit>;

#[derive(Default)]
struct Cached {
    modified: Option<SystemTime>,
    resets: Resets,
}

static CACHE: LazyLock<Mutex<HashMap<PathBuf, Cached>>> = LazyLock::new(Mutex::default);

fn read_registry(path: &Path) -> Result<Resets> {
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("read {}", path.display()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Resets::new()),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

fn modified(path: &Path) -> Result<Option<SystemTime>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata.modified()?)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("inspect {}", path.display())),
    }
}

/// Save one observation without losing limits reported by another TOD process.
/// The registry stores providers, reset times, and whether a time was estimated.
pub fn remember(
    root: &Path,
    platform: AgentPlatform,
    limit: &UsageLimit,
    now_ms: i64,
) -> Result<()> {
    let path = root.join(REGISTRY);
    let lock = File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("provider-quota.lock"))?;
    lock.lock_exclusive()?;
    let mut resets = read_registry(&path)?;
    resets.retain(|_, limit| limit.reset_at_ms > now_ms);
    let replace = resets.get(&platform).is_none_or(|current| {
        limit.parsed && (!current.parsed || limit.reset_at_ms > current.reset_at_ms)
    });
    if replace {
        resets.insert(platform, limit.clone());
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(&resets)?)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Resolve quota eligibility from a shared memory cache. The file metadata
/// check lets the desktop see updates from its resident daemon and vice versa.
pub fn route(root: &Path, desired: AgentLaunchOptions, now_ms: i64) -> Result<AgentLaunchOptions> {
    let path = root.join(REGISTRY);
    let mut cache = CACHE.lock().unwrap_or_else(|err| err.into_inner());
    let stamp = modified(&path)?;
    let cached = cache.entry(root.to_owned()).or_default();
    if stamp != cached.modified {
        cached.resets = read_registry(&path)?;
        cached.modified = stamp;
    }
    let until = |platform| {
        cached
            .resets
            .get(&platform)
            .map(|limit| limit.reset_at_ms)
            .filter(|reset| *reset > now_ms)
    };
    let Some(reset) = until(desired.platform) else {
        return Ok(desired);
    };
    if let Some(equivalent) = equivalent(&desired) {
        if let Some(other_reset) = until(equivalent.platform) {
            bail!(
                "{} and {} usage limit reached|{}",
                desired.platform.label(),
                equivalent.platform.label(),
                reset.min(other_reset)
            );
        }
        return Ok(equivalent);
    }
    bail!("{} usage limit reached|{reset}", desired.platform.label());
}

/// Approximate capability tiers shared by Claude Code and Codex. ACP still
/// validates the chosen model and effort against the destination account.
pub fn equivalent(desired: &AgentLaunchOptions) -> Option<AgentLaunchOptions> {
    let model = desired.model.to_ascii_lowercase();
    let effort = desired.effort.as_str();
    let (platform, model, effort) = match desired.platform {
        AgentPlatform::Claude => {
            let (model, effort) = if model.contains("haiku") {
                ("gpt-5.6-luna", effort)
            } else if model.contains("fable") {
                (
                    "gpt-6-astra",
                    match effort {
                        "low" => "medium",
                        "medium" => "high",
                        "max" => "max",
                        _ => "xhigh",
                    },
                )
            } else if model.contains("opus") || model == "best" {
                match effort {
                    "low" | "medium" | "auto" => ("gpt-5.6-sol", "medium"),
                    _ => ("gpt-6-astra", effort),
                }
            } else {
                match effort {
                    "high" => ("gpt-5.6-sol", "medium"),
                    "xhigh" => ("gpt-5.6-sol", "high"),
                    "max" => ("gpt-5.6-sol", "max"),
                    _ => ("gpt-5.6-terra", effort),
                }
            };
            (AgentPlatform::OpenAi, model, effort)
        }
        AgentPlatform::OpenAi => {
            let model = if model.contains("luna") || model.ends_with("-mini") {
                "haiku"
            } else if model.contains("astra") {
                "opus"
            } else {
                "sonnet"
            };
            let effort = match effort {
                "none" | "minimal" => "low",
                "ultra" => "max",
                _ => tod_agent::agent_launch::CLAUDE_EFFORTS
                    .iter()
                    .copied()
                    .find(|value| *value == effort)
                    .unwrap_or("auto"),
            };
            (AgentPlatform::Claude, model, effort)
        }
        _ => return None,
    };
    Some(AgentLaunchOptions::from_settings(platform, model, effort))
}

/// `…|1760000000` (seconds) or `…|1760000000000` (ms).
fn epoch_suffix(text: &str) -> Option<i64> {
    let (_, tail) = text.trim().rsplit_once('|')?;
    let digits: String = tail
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let n: i64 = digits.parse().ok()?;
    match digits.len() {
        10 => Some(n * 1000),
        13 => Some(n),
        _ => None,
    }
}

fn structured_reset(text: &str) -> Option<i64> {
    fn reset(value: &serde_json::Value) -> Option<i64> {
        let object = value.as_object()?;
        for key in [
            "resets_at",
            "reset_at",
            "resetsAt",
            "resetAt",
            "reset_at_ms",
            "resetAtMs",
        ] {
            if let Some(value) = object.get(key) {
                if let Some(n) = value.as_i64() {
                    return Some(if (1_000_000_000..10_000_000_000).contains(&n) {
                        n * 1000
                    } else {
                        n
                    });
                }
                if let Some(at) = value
                    .as_str()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                {
                    return Some(at.timestamp_millis());
                }
            }
        }
        object.values().find_map(reset)
    }
    let json = &text[text.find('{')?..=text.rfind('}')?];
    let value = serde_json::from_str(json).ok()?;
    reset(&value)
}

/// Codex formats its retry time in the local timezone of its process.
fn codex_retry_time(lower: &str, now_ms: i64) -> Option<i64> {
    let (_, retry) = lower.split_once("try again ")?;
    let retry = retry.trim().trim_end_matches('.');
    let date = retry.trim_start_matches("on ").replace(" at ", " ");
    for format in ["%b %d, %Y %I:%M %p", "%b %d, %Y %I:%M%p", "%Y-%m-%d %H:%M"] {
        if let Ok(date) = NaiveDateTime::parse_from_str(&date, format) {
            return Local
                .from_local_datetime(&date)
                .latest()
                .map(|at| at.timestamp_millis());
        }
    }
    let time = parse_clock(&retry.strip_prefix("at ")?.replace(' ', ""))?;
    let now = Local.timestamp_millis_opt(now_ms).single()?;
    let today = now.date_naive();
    [today, today.succ_opt()?].into_iter().find_map(|day| {
        let at = Local.from_local_datetime(&day.and_time(time)).latest()?;
        (at > now).then(|| at.timestamp_millis())
    })
}

/// `resets 2am`, `reset at 3:30pm (Europe/Berlin)`, `resets 15:00 (UTC)`:
/// the next such time after `now_ms`.
fn clock_time(lower: &str, now_ms: i64) -> Option<i64> {
    let rest = &lower[lower.find("reset")?..];
    let (_, when) = rest.split_once(char::is_whitespace)?;
    let (when, offset) = match when.split_once('(') {
        Some((when, zone)) => (when, zone_offset(zone.split_once(')')?.0.trim())?),
        None => (when, FixedOffset::east_opt(0)?),
    };
    let now: DateTime<FixedOffset> = Utc
        .timestamp_millis_opt(now_ms)
        .single()?
        .with_timezone(&offset);
    let when = when
        .trim()
        .trim_start_matches("at ")
        .trim_start_matches("on ");
    if let Some((date, clock)) = when.split_once(" at ") {
        let time = parse_clock(&clock.trim().trim_end_matches(['.', ',']).replace(' ', ""))?;
        let date = date.trim();
        let date = NaiveDate::parse_from_str(date, "%b %d, %Y")
            .ok()
            .or_else(|| {
                let current =
                    NaiveDate::parse_from_str(&format!("{date} {}", now.year()), "%b %d %Y")
                        .ok()?;
                if current < now.date_naive() {
                    current.with_year(now.year() + 1)
                } else {
                    Some(current)
                }
            })?;
        return offset
            .from_local_datetime(&date.and_time(time))
            .single()
            .map(|at| at.timestamp_millis());
    }
    let token = when
        .split_whitespace()
        .next()?
        .trim_end_matches(['.', ',', ')']);
    let time = parse_clock(token)?;
    let mut candidate = offset
        .from_local_datetime(&now.date_naive().and_time(time))
        .single()?;
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
        assert_eq!(
            Utc.timestamp_millis_opt(NOON).unwrap().to_rfc3339(),
            "2026-09-26T12:00:00+00:00"
        );
    }

    #[test]
    fn reads_the_named_zone() {
        let text = "Claude usage limit reached. Your limit will reset at 3pm (America/New_York).";
        // 3pm at -5 is 20:00 UTC, later today.
        assert_eq!(
            detect(text, NOON),
            Some(UsageLimit {
                reset_at_ms: utc(20, 0, 0),
                parsed: true
            })
        );
    }

    #[test]
    fn a_time_already_past_is_tomorrow() {
        let text = "5-hour limit reached ∙ resets 2am";
        assert_eq!(
            detect(text, NOON),
            Some(UsageLimit {
                reset_at_ms: utc(2, 0, 1),
                parsed: true
            })
        );
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
        assert_eq!(
            detect(&text, NOON),
            Some(UsageLimit {
                reset_at_ms: reset * 1000,
                parsed: true
            })
        );
    }

    #[test]
    fn reads_a_claude_weekly_reset_with_a_calendar_date() {
        let text = "Weekly limit reached · resets Sep 29 at 3pm (UTC).";
        assert_eq!(detect(text, NOON).unwrap().reset_at_ms, utc(15, 0, 3));
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
                Some(UsageLimit {
                    reset_at_ms: NOON + 3_600_000,
                    parsed: false
                }),
                "{text}"
            );
        }
    }

    #[test]
    fn other_failures_are_not_limits() {
        for text in [
            "connection reset by peer",
            "rate limit exceeded",
            "agent hung: no activity",
            "",
        ] {
            assert_eq!(detect(text, NOON), None, "{text}");
        }
    }

    #[test]
    fn reads_codex_reset_time_from_acp_error_data() {
        let reset = (NOON / 1000) + 2 * 3600;
        let text = format!(
            "ACP error -32603: Internal error ({{\"error\":{{\"type\":\"usage_limit_reached\",\"resets_at\":{reset}}}}})"
        );
        assert_eq!(
            detect(&text, NOON),
            Some(UsageLimit {
                reset_at_ms: reset * 1000,
                parsed: true
            })
        );
        assert!(
            banner(
                "The tests confirmed that the usage limit reached message is handled.",
                NOON
            )
            .is_none()
        );
    }

    #[test]
    fn cached_limits_survive_a_restart_and_expire_individually() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let claude = AgentLaunchOptions::from_settings(AgentPlatform::Claude, "sonnet", "high");
        let codex =
            AgentLaunchOptions::from_settings(AgentPlatform::OpenAi, "gpt-5.6-sol", "medium");
        assert_eq!(route(root, claude.clone(), NOON).unwrap(), claude);
        remember(
            root,
            claude.platform,
            &UsageLimit {
                reset_at_ms: NOON + 100,
                parsed: false,
            },
            NOON,
        )
        .unwrap();
        remember(
            root,
            claude.platform,
            &UsageLimit {
                reset_at_ms: NOON + 50,
                parsed: true,
            },
            NOON,
        )
        .unwrap();
        // Discard the process's cache before the next launch, as after restart.
        CACHE.lock().unwrap().remove(root);
        assert_eq!(route(root, claude.clone(), NOON).unwrap(), codex);
        remember(
            root,
            codex.platform,
            &UsageLimit {
                reset_at_ms: NOON + 200,
                parsed: true,
            },
            NOON,
        )
        .unwrap();
        let error = route(root, claude.clone(), NOON).unwrap_err().to_string();
        assert_eq!(detect(&error, NOON).unwrap().reset_at_ms, NOON + 50);
        assert_eq!(route(root, claude.clone(), NOON + 50).unwrap(), claude);
        assert_eq!(
            route(root, codex.clone(), NOON + 50).unwrap().platform,
            AgentPlatform::Claude
        );
        assert_eq!(route(root, codex.clone(), NOON + 200).unwrap(), codex);
    }

    #[test]
    fn claude_model_tiers_translate_to_the_codex_mapping() {
        use AgentPlatform::{Claude, OpenAi};
        for (source, effort, model, mapped_effort) in [
            ("haiku", "low", "gpt-5.6-luna", "low"),
            ("sonnet[1m]", "low", "gpt-5.6-terra", "low"),
            ("sonnet", "high", "gpt-5.6-sol", "medium"),
            ("sonnet", "xhigh", "gpt-5.6-sol", "high"),
            ("sonnet", "max", "gpt-5.6-sol", "max"),
            ("opusplan", "medium", "gpt-5.6-sol", "medium"),
            ("opus", "high", "gpt-6-astra", "high"),
            ("fable", "medium", "gpt-6-astra", "high"),
            ("fable", "max", "gpt-6-astra", "max"),
        ] {
            let desired = AgentLaunchOptions::from_settings(Claude, source, effort);
            assert_eq!(
                equivalent(&desired),
                Some(AgentLaunchOptions::from_settings(
                    OpenAi,
                    model,
                    mapped_effort
                ))
            );
        }
    }
}
