//! What a golden sandbox says about the tod that made it, and what a tod
//! does about a golden it finds (`doc/runner-menu/design.md`, "Older build
//! meets newer golden").
//!
//! The golden carries tod's own files (relay, shim, Linux `tod-cli`,
//! supervisor, bundles) and a fork's own proxy update wipes them again, so
//! each node's fork gets tod's files from the app, without packages
//! (`node::install_into_fork`); the golden still holds what a fork cannot
//! have installed per node (the toolchain, the checkout) and the stamp that
//! says whether this build can use it. The install stamp is a
//! hash: it says "different", not "newer". The golden is therefore also
//! labelled with an order: the sync contract (`schema`, `wire`) and the
//! commit time of the build that made it. Making one is `tod_core`'s
//! `cloud_sync::golden` (it holds the files); this module is the part that
//! needs no network and no files.

use crate::blaxel::SandboxInfo;
use anyhow::{Result, bail};

/// `tod-install=<install stamp>`.
pub const LABEL_INSTALL: &str = "tod-install";
/// `tod-schema=<database schema of the build that made it>`.
pub const LABEL_SCHEMA: &str = "tod-schema";
/// `tod-wire=<sync wire version of the build that made it>`.
pub const LABEL_WIRE: &str = "tod-wire";
/// `tod-built=<commit time, unix seconds, of the build that made it>`.
pub const LABEL_BUILT: &str = "tod-built";
/// `tod-kind=golden`: marks a sandbox tod made as a golden (and so one it may
/// delete when it makes a newer one).
pub const KIND_GOLDEN: &str = "golden";
/// `ready=true`: set last, once the golden passed its checks and has its files.
pub const LABEL_READY: &str = "ready";

/// What one tod build installs into a golden, or what a golden was given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoldenStamp {
    /// `refresh::install_stamp()`: a hash of the files installed.
    pub install: String,
    /// The database schema of that build (`Contract::schema`).
    pub schema: u32,
    /// The sync wire version of that build (`Contract::wire`).
    pub wire: u32,
    /// The commit time of that build, unix seconds.
    pub built: u64,
}

impl GoldenStamp {
    /// Schema, then wire, then commit time: the order a build is newer in.
    fn order(&self) -> (u32, u32, u64) {
        (self.schema, self.wire, self.built)
    }

    /// `install ab12cd, schema 93, wire 1, built 2026-11-03 10:00 UTC` for messages.
    pub fn describe(&self) -> String {
        format!(
            "install {}, schema {}, wire {}, built {}",
            self.install.chars().take(12).collect::<String>(),
            self.schema,
            self.wire,
            built_text(self.built)
        )
    }

    /// The labels that say it (without `ready`, which is set last).
    pub fn labels(&self) -> Vec<(&'static str, String)> {
        vec![
            (LABEL_INSTALL, self.install.clone()),
            (LABEL_SCHEMA, self.schema.to_string()),
            (LABEL_WIRE, self.wire.to_string()),
            (LABEL_BUILT, self.built.to_string()),
        ]
    }

    /// Reads a sandbox's labels. `None` when it carries no `tod-install`
    /// (a golden made by hand, or by a tod before this): the caller treats it
    /// as older than any build. A label that is there but not a number is an
    /// error, not an absent one.
    pub fn from_info(info: &SandboxInfo) -> Result<Option<Self>> {
        let Some(install) = info.label(LABEL_INSTALL).filter(|v| !v.is_empty()) else {
            return Ok(None);
        };
        let number = |key: &str| -> Result<u64> {
            let text = info.label(key).ok_or_else(|| anyhow::anyhow!("{} has {LABEL_INSTALL} but no {key} label", info.name))?;
            text.parse::<u64>().map_err(|_| anyhow::anyhow!("{}'s {key} label is {text:?}, not a number", info.name))
        };
        Ok(Some(Self {
            install: install.to_string(),
            schema: u32::try_from(number(LABEL_SCHEMA)?)?,
            wire: u32::try_from(number(LABEL_WIRE)?)?,
            built: number(LABEL_BUILT)?,
        }))
    }
}

fn built_text(secs: u64) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    let t = UNIX_EPOCH + Duration::from_secs(secs);
    // No date library here: seconds since the epoch, civil from days.
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, (rem % 3600) / 60)
}

/// Howard Hinnant's days-to-civil conversion.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// What to do about the golden found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Same stamp: fork it as it is.
    Use,
    /// Make a new golden from it and use that; `why` is said to the user.
    Refresh { why: String },
    /// The golden is newer than this build: refuse ([`refusal`]).
    Refuse,
}

/// Compares `mine` (this build's) with the golden's. `golden` is `None` when
/// it carries no stamp.
pub fn plan(mine: &GoldenStamp, golden: Option<&GoldenStamp>) -> Plan {
    let Some(theirs) = golden else {
        return Plan::Refresh { why: "it carries no tod-install label (it was not made by a tod that stamps its goldens)".to_string() };
    };
    if theirs.install == mine.install {
        return Plan::Use;
    }
    match theirs.order().cmp(&mine.order()) {
        std::cmp::Ordering::Less => Plan::Refresh {
            why: format!("it was made by an older tod ({}); this tod is {}", theirs.describe(), mine.describe()),
        },
        std::cmp::Ordering::Equal => Plan::Refresh {
            why: format!(
                "it holds different files from this tod's, of the same schema, wire, and commit ({} against {}): a build with local changes",
                theirs.describe(),
                mine.describe()
            ),
        },
        std::cmp::Ordering::Greater => Plan::Refuse,
    }
}

/// The error for [`Plan::Refuse`]. Never refreshed back: that would break the
/// newer tod's users' sandboxes.
pub fn refusal(repo: &str, mine: &GoldenStamp, theirs: &GoldenStamp) -> String {
    format!(
        "The golden sandbox for {repo} was refreshed by a newer tod (install {}, schema {}); this tod is older \
         (install {}, schema {}). Update tod to make sandboxes for this project.",
        theirs.install, theirs.schema, mine.install, mine.schema
    )
}

/// [`plan`], with a refusal as the error.
pub fn decide(repo: &str, mine: &GoldenStamp, golden: Option<&GoldenStamp>) -> Result<Plan> {
    match (plan(mine, golden), golden) {
        (Plan::Refuse, Some(theirs)) => bail!("{}", refusal(repo, mine, theirs)),
        (plan, _) => Ok(plan),
    }
}

/// A name for a new golden of `repo` made at `now` (unix seconds):
/// `tod-golden-<owner>-<name>-<8 hex>`, at most 48 characters, of what a
/// sandbox name may hold.
pub fn new_name(repo: &str, now: u64) -> String {
    let slug: String = repo
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    let suffix = format!("{:08x}", now & 0xffff_ffff);
    let room = 48 - "tod-golden-".len() - 1 - suffix.len();
    let slug: String = slug.chars().take(room).collect();
    format!("tod-golden-{}-{suffix}", slug.trim_end_matches('-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(install: &str, schema: u32, wire: u32, built: u64) -> GoldenStamp {
        GoldenStamp { install: install.into(), schema, wire, built }
    }

    #[test]
    fn the_same_stamp_is_used_as_it_is() {
        let s = stamp("aaa", 93, 1, 100);
        assert_eq!(plan(&s, Some(&s.clone())), Plan::Use);
        // Whatever the order says: the same files are the same.
        assert_eq!(plan(&s, Some(&stamp("aaa", 1, 1, 1))), Plan::Use);
    }

    #[test]
    fn an_older_golden_is_refreshed_forward_by_schema_then_wire_then_time() {
        let mine = stamp("bbb", 93, 1, 100);
        for older in [stamp("aaa", 92, 1, 999), stamp("aaa", 93, 0, 999), stamp("aaa", 93, 1, 99)] {
            match plan(&mine, Some(&older)) {
                Plan::Refresh { why } => assert!(why.contains("older tod"), "{why}"),
                other => panic!("{older:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_newer_golden_is_refused_naming_the_repository_and_both_stamps() {
        let mine = stamp("bbb", 93, 1, 100);
        for newer in [stamp("ccc", 94, 1, 1), stamp("ccc", 93, 2, 1), stamp("ccc", 93, 1, 101)] {
            assert_eq!(plan(&mine, Some(&newer)), Plan::Refuse);
            let err = decide("acme/widget", &mine, Some(&newer)).unwrap_err().to_string();
            assert!(err.contains("acme/widget"), "{err}");
            assert!(err.contains("install ccc") && err.contains(&format!("schema {}", newer.schema)), "{err}");
            assert!(err.contains("install bbb, schema 93"), "{err}");
            assert!(err.contains("Update tod"), "{err}");
        }
    }

    #[test]
    fn the_same_order_with_a_different_stamp_is_refreshed_and_says_so() {
        let mine = stamp("bbb", 93, 1, 100);
        match plan(&mine, Some(&stamp("aaa", 93, 1, 100))) {
            Plan::Refresh { why } => assert!(why.contains("local changes"), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_golden_with_no_stamp_is_refreshed() {
        let mine = stamp("bbb", 93, 1, 100);
        assert!(matches!(plan(&mine, None), Plan::Refresh { .. }));
    }

    #[test]
    fn labels_round_trip_and_a_bad_one_is_an_error() {
        let s = stamp("abc123", 93, 1, 1_762_000_000);
        let mut info = SandboxInfo {
            name: "g".into(),
            status: "DEPLOYED".into(),
            state: None,
            url: None,
            image: String::new(),
            labels: vec![],
            volumes: vec![],
            node_env: vec![],
        };
        assert_eq!(GoldenStamp::from_info(&info).unwrap(), None);
        for (k, v) in s.labels() {
            info.labels.push((k.to_string(), v));
        }
        assert_eq!(GoldenStamp::from_info(&info).unwrap(), Some(s));
        info.labels.retain(|(k, _)| k != LABEL_SCHEMA);
        info.labels.push((LABEL_SCHEMA.into(), "x".into()));
        assert!(GoldenStamp::from_info(&info).is_err());
        info.labels.retain(|(k, _)| k != LABEL_SCHEMA);
        assert!(GoldenStamp::from_info(&info).is_err());
    }

    #[test]
    fn a_new_golden_name_fits_a_sandbox_name() {
        let n = new_name("Acme/A-Very-Long-Repository-Name-Indeed-Yes", 1_762_000_000);
        assert!(n.len() <= 48, "{n} is {}", n.len());
        assert!(n.starts_with("tod-golden-acme-a-very"), "{n}");
        assert!(n.ends_with("-6905fc80"), "{n}");
        assert_eq!(new_name("a/b", 1), "tod-golden-a-b-00000001");
    }

    #[test]
    fn built_is_shown_as_a_date() {
        assert_eq!(built_text(0), "1970-01-01 00:00 UTC");
        assert_eq!(built_text(1_762_000_000), "2025-11-01 12:26 UTC");
    }
}
