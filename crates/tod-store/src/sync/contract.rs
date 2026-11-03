//! The version contract between the orchestrator and its clients (the app,
//! the daemon, the supervisor): `{wire, schema, build}`.
//!
//! The orchestrator's `GET /health` returns its own; a client sends its own
//! with every request (`X-Tod-Wire`, `X-Tod-Schema`) and judges the
//! orchestrator's with [`judge`]. This replaces deciding "out of date" by a
//! hash of the sources.
//!
//! - `wire` is bumped only when the sync protocol itself changes.
//! - `schema` is the fleet database's `user_version`; the orchestrator opens
//!   the user's database with its own build, so it can read only schemas up to
//!   its own.
//! - `build` is informational (which build answered); it never decides.
//!
//! An orchestrator that predates the contract answers `/health` with `ok`
//! and no numbers: [`Contract::parse_health`] reads that as wire 0, schema 0.

use serde::{Deserialize, Serialize};

/// The sync protocol's version. Bump it only when the protocol changes in a
/// way an older peer cannot take.
pub const WIRE: u32 = 1;
/// Request header carrying the sender's [`WIRE`].
pub const WIRE_HEADER: &str = "X-Tod-Wire";
/// Request header carrying the sender's database schema (`user_version`).
pub const SCHEMA_HEADER: &str = "X-Tod-Schema";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contract {
    pub wire: u32,
    pub schema: u32,
    #[serde(default)]
    pub build: String,
}

/// What a client concludes about an orchestrator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Same wire, and the orchestrator's schema is not older: nothing to do.
    Compatible,
    /// The orchestrator's wire or schema is lower than this client's: it
    /// should be redeployed.
    Older,
    /// The orchestrator's wire or schema is higher than this client reads:
    /// this app must be updated; nothing is applied.
    Newer,
}

impl Contract {
    /// This build's contract; `build` is the caller's build stamp.
    pub fn current(build: impl Into<String>) -> Self {
        Self { wire: WIRE, schema: crate::fleet::schema::CURRENT_USER_VERSION as u32, build: build.into() }
    }

    /// Reads a `/health` body: the JSON contract, or (an orchestrator from
    /// before the contract answered `ok`) wire 0, schema 0.
    pub fn parse_health(body: &str) -> Self {
        serde_json::from_str::<Self>(body.trim()).unwrap_or(Self { wire: 0, schema: 0, build: String::new() })
    }

    /// `wire 1, schema 87, build abc`, for messages.
    pub fn describe(&self) -> String {
        if self.wire == 0 && self.schema == 0 {
            "wire 0 (no version reported), schema unknown".to_string()
        } else if self.build.is_empty() {
            format!("wire {}, schema {}", self.wire, self.schema)
        } else {
            format!("wire {}, schema {}, build {}", self.wire, self.schema, self.build)
        }
    }

    /// The headers a client sends with a request.
    pub fn headers(&self) -> [(&'static str, String); 2] {
        [(WIRE_HEADER, self.wire.to_string()), (SCHEMA_HEADER, self.schema.to_string())]
    }
}

/// Judges the orchestrator (`theirs`) from this client (`mine`). A newer
/// either-number wins over an older other: this client cannot read it.
pub fn judge(mine: &Contract, theirs: &Contract) -> Verdict {
    if theirs.wire > mine.wire || theirs.schema > mine.schema {
        Verdict::Newer
    } else if theirs.wire < mine.wire || theirs.schema < mine.schema {
        Verdict::Older
    } else {
        Verdict::Compatible
    }
}

/// The refusal for a [`Verdict::Newer`] orchestrator, naming both versions.
pub fn newer_message(mine: &Contract, theirs: &Contract) -> String {
    format!(
        "the orchestrator is newer than this app can read ({}; this app is {}): update tod",
        theirs.describe(),
        mine.describe()
    )
}

/// What the orchestrator concludes from a request's headers: `Err` carries
/// the 426 body when the client is newer than this orchestrator (it would
/// write rows this one rejects). Headers a client does not send (a
/// supervisor from before the contract) are not judged.
pub fn check_client(ours: &Contract, wire: Option<&str>, schema: Option<&str>) -> Result<(), String> {
    let parse = |v: Option<&str>| v.and_then(|s| s.trim().parse::<u32>().ok());
    let (cw, cs) = (parse(wire), parse(schema));
    if cw.is_some_and(|w| w > ours.wire) || cs.is_some_and(|s| s > ours.schema) {
        return Err(format!(
            "this orchestrator ({}) is older than the client (wire {}, schema {}): redeploy it \
             (the app does so itself; or `tod-sandbox orchestrator`)",
            ours.describe(),
            cw.map_or("?".into(), |v| v.to_string()),
            cs.map_or("?".into(), |v| v.to_string()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(wire: u32, schema: u32) -> Contract {
        Contract { wire, schema, build: "b".into() }
    }

    #[test]
    fn compatible_when_equal_whatever_the_build() {
        let mut theirs = c(1, 87);
        theirs.build = "other".into();
        assert_eq!(judge(&c(1, 87), &theirs), Verdict::Compatible);
    }

    #[test]
    fn an_older_wire_or_schema_is_older() {
        assert_eq!(judge(&c(2, 87), &c(1, 87)), Verdict::Older);
        assert_eq!(judge(&c(1, 88), &c(1, 87)), Verdict::Older);
    }

    #[test]
    fn a_newer_wire_or_schema_is_newer_even_if_the_other_is_older() {
        assert_eq!(judge(&c(1, 87), &c(1, 88)), Verdict::Newer);
        assert_eq!(judge(&c(2, 90), &c(1, 91)), Verdict::Newer);
    }

    #[test]
    fn a_newer_schema_is_refused_naming_both_versions() {
        let msg = newer_message(&c(1, 87), &c(1, 90));
        assert!(msg.contains("schema 90") && msg.contains("schema 87"), "{msg}");
    }

    #[test]
    fn a_health_of_ok_is_wire_zero() {
        let theirs = Contract::parse_health("ok");
        assert_eq!((theirs.wire, theirs.schema), (0, 0));
        assert_eq!(judge(&c(1, 87), &theirs), Verdict::Older);
    }

    #[test]
    fn health_json_round_trips() {
        let body = serde_json::to_string(&c(1, 87)).unwrap();
        assert_eq!(Contract::parse_health(&body), c(1, 87));
    }

    #[test]
    fn the_orchestrator_refuses_only_a_newer_client() {
        assert!(check_client(&c(1, 87), Some("1"), Some("87")).is_ok());
        assert!(check_client(&c(1, 87), None, None).is_ok());
        assert!(check_client(&c(1, 87), Some("1"), Some("86")).is_ok());
        assert!(check_client(&c(1, 87), Some("2"), Some("87")).is_err());
        assert!(check_client(&c(1, 87), Some("1"), Some("88")).is_err());
    }
}
