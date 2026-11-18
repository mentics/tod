//! Watches how fast Blaxel spend is growing and tells the user at once when
//! it is too fast.
//!
//! Blaxel's Billing API gives the cost so far this month
//! (`Blaxel::cost_between`), a total that only grows (it runs a few minutes
//! behind). Every [`SAMPLE_EVERY`] the daemon reads it and keeps the last
//! samples in `<data root>/billing-samples.json`. The cost between two
//! samples over the time between them is a rate, shown as the equivalent
//! dollars per month:
//!
//! - the **immediate** rate is the latest pair, over about five minutes;
//! - the **smoothed** rate is an exponential moving average of the last
//!   [`SMOOTH_PAIRS`] pairs (about half an hour).
//!
//! Each has its own threshold ([`Thresholds`]); crossing one raises an error
//! in the app's log (a toast) and a desktop notification, and again each
//! [`REALERT_AFTER`] while it stays over. A sandbox awake for thirty seconds
//! barely moves a five-minute pair, so a brief wake does not alert.
//!
//! The pure part ([`rates`], [`judge`]) is tested; reading Blaxel and
//! notifying are [`sample`] and [`notify`].

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SAMPLE_EVERY: Duration = Duration::from_secs(300);
/// Samples kept (a day).
const KEEP: usize = 288;
/// Pairs the smoothed rate averages (six: about half an hour).
pub const SMOOTH_PAIRS: usize = 6;
/// An alert is repeated this often while the rate stays over.
pub const REALERT_AFTER: Duration = Duration::from_secs(3600);
const MONTH_SECS: f64 = 30.0 * 86_400.0;

/// Overrides: dollars per month, as text.
pub const ENV_IMMEDIATE: &str = "TOD_BILLING_ALERT_IMMEDIATE_PER_MONTH";
pub const ENV_SMOOTHED: &str = "TOD_BILLING_ALERT_SMOOTHED_PER_MONTH";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// The latest five-minute pair, as dollars per month.
    pub immediate: f64,
    /// The moving average, as dollars per month.
    pub smoothed: f64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self { immediate: 30.0, smoothed: 20.0 }
    }
}

impl Thresholds {
    /// The defaults with either overridden by its variable; a value that is not a number is an error.
    pub fn from_env() -> Result<Self> {
        let mut t = Self::default();
        for (var, slot) in [(ENV_IMMEDIATE, &mut t.immediate), (ENV_SMOOTHED, &mut t.smoothed)] {
            if let Ok(text) = std::env::var(var) {
                *slot = text.trim().parse().with_context(|| format!("{var} is {text:?}, not a number of dollars"))?;
            }
        }
        Ok(t)
    }
}

/// One reading: when (ms since the epoch) and the month-to-date cost in USD.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub at_ms: i64,
    pub total: f64,
}

/// The rates the samples show, as dollars per month.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    pub immediate: f64,
    pub smoothed: f64,
}

/// The rate of each consecutive pair, oldest first. A pair across the month's
/// reset (the total went down) or with no time between is left out.
fn pair_rates(samples: &[Sample]) -> Vec<f64> {
    samples
        .windows(2)
        .filter_map(|w| {
            let secs = (w[1].at_ms - w[0].at_ms) as f64 / 1000.0;
            let cost = w[1].total - w[0].total;
            (secs > 0.0 && cost >= 0.0).then(|| cost / secs * MONTH_SECS)
        })
        .collect()
}

/// The immediate and smoothed rates, or `None` until there are two samples
/// in one month.
pub fn rates(samples: &[Sample]) -> Option<Rates> {
    let pairs = pair_rates(samples);
    let immediate = *pairs.last()?;
    let recent = &pairs[pairs.len().saturating_sub(SMOOTH_PAIRS)..];
    let alpha = 2.0 / (recent.len() as f64 + 1.0);
    let smoothed = recent[1..].iter().fold(recent[0], |ema, r| alpha * r + (1.0 - alpha) * ema);
    Some(Rates { immediate, smoothed })
}

/// What is over its threshold, in words (empty when nothing is).
pub fn judge(rates: Rates, t: Thresholds) -> Vec<String> {
    let mut over = Vec::new();
    if rates.immediate > t.immediate {
        over.push(format!(
            "the last five minutes ran at ${:.0}/month (limit ${:.0})",
            rates.immediate, t.immediate
        ));
    }
    if rates.smoothed > t.smoothed {
        over.push(format!("the half-hour average runs at ${:.0}/month (limit ${:.0})", rates.smoothed, t.smoothed));
    }
    over
}

fn path(root: &Path) -> PathBuf {
    root.join("billing-samples.json")
}

fn load(root: &Path) -> Result<Vec<Sample>> {
    match std::fs::read_to_string(path(root)) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("read {}", path(root).display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path(root).display())),
    }
}

fn save(root: &Path, samples: &[Sample]) -> Result<()> {
    let tmp = path(root).with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(samples)?).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path(root)).with_context(|| format!("replace {}", path(root).display()))
}

fn alert_path(root: &Path) -> PathBuf {
    root.join("billing-last-alert.txt")
}

/// When (ms since the epoch) the last alert was raised, kept on disk so a
/// restart of the daemon does not raise it again at once.
pub fn last_alert_ms(root: &Path) -> Option<i64> {
    std::fs::read_to_string(alert_path(root)).ok()?.trim().parse().ok()
}

/// Records that an alert was raised now.
pub fn record_alert(root: &Path) -> Result<()> {
    let p = alert_path(root);
    std::fs::write(&p, tod_store::outline::uuid_blob::now_ms().to_string()).with_context(|| format!("write {}", p.display()))
}

/// Whether an alert is due: none yet, or the last one was [`REALERT_AFTER`] ago.
pub fn alert_due(root: &Path) -> bool {
    last_alert_ms(root).is_none_or(|at| tod_store::outline::uuid_blob::now_ms() - at >= REALERT_AFTER.as_millis() as i64)
}

/// Reads the month's cost from Blaxel now.
fn read_total(root: &Path) -> Result<f64> {
    use chrono::{Datelike, TimeZone, Utc};
    let bx = tod_store::fleet::sandbox::Sandboxes::load(root)?.blaxel()?;
    let accounts = bx.accounts().context("find the Blaxel account for billing")?;
    let account = match accounts.as_slice() {
        [one] => one.0.clone(),
        [] => bail!("this Blaxel token can read no account, so billing cannot be read"),
        many => bail!(
            "this Blaxel token reads {} accounts ({}); billing needs one",
            many.len(),
            many.iter().map(|a| a.1.as_str()).collect::<Vec<_>>().join(", ")
        ),
    };
    let now = Utc::now();
    let start = Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0).single().context("the month's start")?;
    bx.cost_between(&account, &start.format("%Y-%m-%dT%H:%M:%SZ").to_string(), &(now + chrono::Duration::minutes(1)).format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

/// What one look found.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub total: f64,
    pub rates: Option<Rates>,
    pub over: Vec<String>,
}

/// Reads the cost, keeps the sample, and says what is over its threshold.
/// Blocks on the network: never on the UI thread.
pub fn sample(root: &Path, thresholds: Thresholds) -> Result<Reading> {
    let total = read_total(root)?;
    let mut samples = load(root)?;
    samples.push(Sample { at_ms: tod_store::outline::uuid_blob::now_ms(), total });
    let cut = samples.len().saturating_sub(KEEP);
    samples.drain(..cut);
    save(root, &samples)?;
    let rates = rates(&samples);
    let over = rates.map(|r| judge(r, thresholds)).unwrap_or_default();
    Ok(Reading { total, rates, over })
}

/// The alert that is on screen now, if the user has not dismissed it yet.
#[cfg(target_os = "macos")]
static OPEN_ALERT: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

/// An error in the app's log, which raises a toast wherever the app runs, and
/// on macOS a dialog that **stays until the user dismisses it** (a banner
/// notification goes away on its own). While one is still open, a repeat adds
/// no second dialog. A dialog that cannot be shown is itself logged.
/// The other `tod-agentd` daemons in a process listing (`ps -axo pid=,command=`
/// lines), as `pid N (data root)`: any but `own_pid`. A test root's daemon is
/// legitimate, but one nobody remembers keeps polling the orchestrator, whose
/// sandbox bills while awake, so a high rate names them.
pub fn other_daemons(listing: &str, own_pid: u32) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, command) = line.split_once(char::is_whitespace)?;
            let pid: u32 = pid.parse().ok()?;
            let program = command.split_whitespace().next()?;
            let name = program.rsplit('/').next()?;
            if pid == own_pid || !name.starts_with("tod-agentd") {
                return None;
            }
            let root = command.split_once("--data-root ").map_or("?", |(_, r)| r.trim());
            Some(format!("pid {pid} ({root})"))
        })
        .collect()
}

/// [`other_daemons`] of the machine now; empty where `ps` is not there.
pub fn running_other_daemons() -> Vec<String> {
    let Ok(out) = std::process::Command::new("ps").args(["-axo", "pid=,command="]).output() else {
        return Vec::new();
    };
    other_daemons(&String::from_utf8_lossy(&out.stdout), std::process::id())
}

pub fn notify(message: &str) {
    let message = &format!("{message} ({})", chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z"));
    crate::logging::buffer::record(
        crate::logging::buffer::Level3::Error,
        crate::logging::buffer::LogSource::App,
        format!("Blaxel spend is high: {message}"),
    );
    #[cfg(target_os = "macos")]
    {
        let mut open = OPEN_ALERT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(child) = open.as_mut() {
            match child.try_wait() {
                Ok(None) => return,
                Ok(Some(_)) => {}
                Err(err) => tracing::error!("billing watch: could not check the open alert: {err}"),
            }
        }
        let text = serde_json::to_string(message).unwrap_or_else(|_| "\"see the tod log\"".into());
        let script = format!(
            "display alert \"Blaxel spend is high\" message {text} as critical buttons {{\"Dismiss\"}} default button \"Dismiss\""
        );
        match std::process::Command::new("osascript").arg("-e").arg(script).spawn() {
            Ok(child) => *open = Some(child),
            Err(err) => tracing::error!("billing watch: could not run osascript: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn other_daemons_are_named_by_pid_and_root() {
        let ps = "  101 /Users/me/.config/tod/daemon/tod-agentd-abc --data-root /Users/me/.config/tod\n\
                  202 /Users/me/.local/share/tod-live-verify/daemon/tod-agentd-def --data-root /Users/me/.local/share/tod-live-verify\n\
                  303 /usr/bin/vim tod-agentd.rs\n\
                  404 tod-agentd-ghi\n";
        assert_eq!(
            super::other_daemons(ps, 101),
            vec!["pid 202 (/Users/me/.local/share/tod-live-verify)".to_string(), "pid 404 (?)".to_string()]
        );
    }

    use super::*;

    const MIN: i64 = 60_000;

    /// Reads the real month-to-date cost with the data root's Blaxel token
    /// (read-only): `TOD_TEST_BILLING_ROOT=<data root>`.
    #[test]
    fn the_real_billing_api_answers() {
        let Ok(root) = std::env::var("TOD_TEST_BILLING_ROOT") else { return };
        let total = read_total(std::path::Path::new(&root)).unwrap();
        println!("month-to-date cost: {total}");
        assert!(total >= 0.0);
    }

    fn s(min: i64, total: f64) -> Sample {
        Sample { at_ms: min * MIN, total }
    }

    /// $1 over five minutes is $1/300 s of a 30-day month.
    fn per_month(dollars_per_5min: f64) -> f64 {
        dollars_per_5min / 300.0 * MONTH_SECS
    }

    #[test]
    fn one_sample_has_no_rate_and_two_have_one() {
        assert_eq!(rates(&[]), None);
        assert_eq!(rates(&[s(0, 1.0)]), None);
        let r = rates(&[s(0, 1.0), s(5, 1.5)]).unwrap();
        assert!((r.immediate - per_month(0.5)).abs() < 1e-6);
        assert!((r.smoothed - r.immediate).abs() < 1e-6, "one pair: the same");
    }

    #[test]
    fn a_brief_spike_moves_the_smoothed_rate_less_than_the_immediate_one() {
        let mut v = vec![s(0, 0.0)];
        let mut total = 0.0;
        for (i, step) in [0.001, 0.001, 0.001, 0.001, 0.001, 0.2].iter().enumerate() {
            total += step;
            v.push(s(5 * (i as i64 + 1), total));
        }
        let r = rates(&v).unwrap();
        assert!(r.immediate > 5.0 * r.smoothed / 3.0 || r.immediate > r.smoothed, "{r:?}");
        assert!(r.smoothed < r.immediate);
    }

    #[test]
    fn a_month_reset_drops_the_pair_instead_of_a_negative_rate() {
        let r = rates(&[s(0, 90.0), s(5, 0.1), s(10, 0.2)]).unwrap();
        assert!((r.immediate - per_month(0.1)).abs() < 1e-6);
        assert_eq!(rates(&[s(0, 90.0), s(5, 0.1)]), None, "the only pair spans the reset");
    }

    #[test]
    fn each_threshold_alerts_on_its_own() {
        let t = Thresholds::default();
        assert!(judge(Rates { immediate: 10.0, smoothed: 10.0 }, t).is_empty());
        assert_eq!(judge(Rates { immediate: 31.0, smoothed: 10.0 }, t).len(), 1);
        assert_eq!(judge(Rates { immediate: 10.0, smoothed: 21.0 }, t).len(), 1);
        let both = judge(Rates { immediate: 40.0, smoothed: 25.0 }, t);
        assert_eq!(both.len(), 2, "{both:?}");
    }
}
