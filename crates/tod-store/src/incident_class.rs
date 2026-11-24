//! Classifying a failure the app sees, for cross-node incidents
//! (`doc/requests/design.md` 4.1, `doc/lifecycle/incidents.md`): its kind
//! and a stable signature, so fifty nodes failing the same way are one
//! incident. Conservative by design: what is not recognised is `work`, the
//! change's own failure, which climbs the ladder as before and never parks.
//!
//! The signature is `<kind>:<stable key>`. A known cause has a named key
//! (`auth:github`: a 401 and "token not configured" are one cause); anything
//! else keys on [`crate::failure_signature::of_text`], so ids, timestamps and
//! node-specific paths do not make two nodes' identical failures differ.

use crate::failure_signature::of_text;

/// What kind of failure it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The job's infrastructure, not the change: rerun, bounded (design 4.2).
    CiInfra,
    /// A credential is rejected or missing: one request, never per-node work.
    Auth,
    /// A service cannot be reached.
    ServiceDown,
    /// The model API is overloaded or rate limited.
    Provider,
    /// `tod-cli` or a tod shim fails with one message.
    ToolBroken,
    /// Everything else: the change's own failure.
    Work,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::CiInfra => "ci_infra",
            Kind::Auth => "auth",
            Kind::ServiceDown => "service_down",
            Kind::Provider => "provider",
            Kind::ToolBroken => "tool_broken",
            Kind::Work => "work",
        }
    }

    pub fn parse(text: &str) -> Option<Kind> {
        [Kind::CiInfra, Kind::Auth, Kind::ServiceDown, Kind::Provider, Kind::ToolBroken, Kind::Work]
            .into_iter()
            .find(|k| k.as_str() == text)
    }

    /// Whether a node failing this way is parked on an incident. A CI
    /// infrastructure failure is rerun per job (it is one job's failure, not a
    /// shared dependency), and `work` is the change's own.
    pub fn parks(self) -> bool {
        matches!(self, Kind::Auth | Kind::ServiceDown | Kind::Provider | Kind::ToolBroken)
    }
}

/// Where a failure was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A read or operation on GitHub.
    Github,
    /// An agent turn (the model API, the agent's own login).
    Agent,
    /// The `--error` of an agent-filed request.
    AgentRequest,
    /// `tod-cli` or a tod shim.
    Tool,
    Other,
}

/// What a failure is, and how to recognise the same one again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    pub kind: Kind,
    /// `<kind>:<key>`; empty for `work` with nothing to key on.
    pub signature: String,
    /// One line for the status bar and the node's "who": "GitHub auth failing".
    pub summary: String,
}

fn named(kind: Kind, key: &str, summary: &str) -> Classification {
    Classification { kind, signature: format!("{}:{key}", kind.as_str()), summary: summary.to_string() }
}

fn has(lower: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| lower.contains(n))
}

/// The JSON data Claude's ACP bridge includes after the human error message,
/// possibly following a parenthesized time zone or an error-chain prefix.
fn acp_rate_limit_data(error: &str) -> Option<serde_json::Value> {
    let (_, response) = error.split_once("ACP error")?;
    let response = response.trim_end().strip_suffix(')')?;
    response.match_indices(" ({").find_map(|(at, _)| {
        let data = serde_json::from_str::<serde_json::Value>(&response[at + 2..]).ok()?;
        (data.get("errorKind")?.as_str()? == "rate_limit").then_some(data)
    })
}

fn acp_reset_ms(data: &serde_json::Value) -> Option<i64> {
    data.get("rateLimit")?.get("resetsAt")?.as_i64()?.checked_mul(1000).filter(|at| *at > 0)
}

/// The rate-limit reset Claude ACP reported, in Unix milliseconds.
pub fn acp_rate_limit_reset_ms(error: &str) -> Option<i64> {
    acp_reset_ms(&acp_rate_limit_data(error)?)
}

/// A renewable subscription window Claude ACP reports even when it omits
/// the absolute reset. A request-rate error need not be a usage window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpUsageWindow {
    FiveHour,
    Weekly,
}

pub fn acp_usage_window_without_reset(error: &str) -> Option<AcpUsageWindow> {
    let data = acp_rate_limit_data(error)?;
    if acp_reset_ms(&data).is_some() {
        return None;
    }
    let kind = data.get("rateLimit").and_then(|limit| limit.get("rateLimitType")).or_else(|| data.get("rateLimitType"));
    match kind.and_then(serde_json::Value::as_str) {
        Some("five_hour") => return Some(AcpUsageWindow::FiveHour),
        Some("seven_day") => return Some(AcpUsageWindow::Weekly),
        Some(_) => return None,
        None => {}
    }
    let message = error.split_once("ACP error")?.1.split(" ({").next()?.to_ascii_lowercase();
    if has(&message, &["you've hit your 5-hour limit", "5-hour limit reached"]) {
        Some(AcpUsageWindow::FiveHour)
    } else if has(&message, &["you've hit your weekly limit", "weekly limit reached"]) {
        Some(AcpUsageWindow::Weekly)
    } else {
        None
    }
}

/// The first host-like word of `text` (`api.github.com`), when it has one.
fn host_of(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let after_scheme = lower.find("://").map(|i| &lower[i + 3..]).unwrap_or(&lower);
    after_scheme
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        .find(|w| w.contains('.') && w.chars().any(|c| c.is_ascii_alphabetic()) && !w.starts_with('.') && !w.ends_with('.'))
        .map(str::to_string)
}

/// What a brokered `tod-cli` noun (`db`, `access`) says when it runs outside a
/// cloud sandbox. It is an agent using the wrong tool for its environment, not a
/// broken tool: it never opens an incident ([`is_wrong_environment`]).
pub fn wrong_environment_message(noun: &str) -> String {
    let how = if noun == "db" {
        "query the database directly with the `db-query` skill (requires AWS SSO login)".to_string()
    } else {
        "reach the service directly with the project's own tooling, such as the `db-query` skill for a database \
         (requires AWS SSO login)"
            .to_string()
    };
    format!("`tod-cli {noun}` is only for cloud sandboxes; on this machine or in a dev container, {how}")
}

/// Whether `error` is [`wrong_environment_message`]'s.
pub fn is_wrong_environment(error: &str) -> bool {
    error.contains("is only for cloud sandboxes; on this machine or in a dev container")
}

/// Classifies `error` seen at `source`.
pub fn classify(source: Source, error: &str) -> Classification {
    if is_wrong_environment(error) {
        return Classification { kind: Kind::Work, signature: String::new(), summary: String::new() };
    }
    let lower = error.to_lowercase();
    let github = source == Source::Github || lower.contains("github");

    // Credentials first: a rejected token is never a service being down.
    if github
        && has(
            &lower,
            &[
                "rejected the token",
                "http 401",
                "bad credentials",
                "github token not configured",
                "credentials are not set up",
                "requires authentication",
            ],
        )
    {
        return named(Kind::Auth, "github", "GitHub auth failing");
    }
    if has(
        &lower,
        &[
            "expiredtoken",
            "security token included in the request is expired",
            "sso session associated with this profile has expired",
            "unable to locate credentials",
        ],
    ) {
        return named(Kind::Auth, "aws", "AWS auth failing");
    }
    if lower.contains("blaxel") && has(&lower, &["401", "unauthorized", "authentication", "invalid api key"]) {
        return named(Kind::Auth, "blaxel", "Blaxel auth failing");
    }
    if matches!(source, Source::Agent | Source::AgentRequest)
        && has(
            &lower,
            &[
                "invalid api key",
                "invalid x-api-key",
                "authentication_error",
                "please run /login",
                "oauth token has expired",
                "not logged in",
                "not signed in",
                "failed to authenticate",
            ],
        )
    {
        return named(Kind::Auth, "agent", "The agent's login is failing");
    }

    // A spent GitHub rate limit has its own wait until the reset
    // (`autopilot::github_wait`): it is not an incident.
    if github && crate::github::is_rate_limited_message(error) {
        return Classification { kind: Kind::Work, signature: String::new(), summary: String::new() };
    }

    if source == Source::Agent {
        if acp_rate_limit_data(error).is_some() {
            return named(Kind::Provider, "rate_limit", "The model API is rate limiting");
        }
        if has(&lower, &["overloaded_error", "overloaded", "529"]) {
            return named(Kind::Provider, "overloaded", "The model API is overloaded");
        }
        if has(&lower, &["rate_limit_error", "rate limit", "usage limit", "too many requests", "429"]) {
            return named(Kind::Provider, "rate_limit", "The model API is rate limiting");
        }
    }

    if github && (crate::github::is_transient_message(error) || has(&lower, &["github is unavailable"])) {
        return named(Kind::ServiceDown, "github", "GitHub unreachable");
    }
    if has(
        &lower,
        &[
            "connection refused",
            "dns error",
            "failed to lookup address",
            "could not resolve host",
            "name or service not known",
            "connection reset by peer",
            "temporary failure in name resolution",
        ],
    ) {
        let key = host_of(error).unwrap_or_else(|| match source {
            Source::Github => "github".into(),
            Source::Agent => "agent-network".into(),
            _ => "network".into(),
        });
        return Classification {
            kind: Kind::ServiceDown,
            signature: format!("service_down:{key}"),
            summary: format!("{key} unreachable"),
        };
    }

    if source == Source::Tool {
        let sig = of_text(error);
        if !sig.is_empty() {
            let first = error.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
            let first: String = first.chars().take(80).collect();
            return Classification {
                kind: Kind::ToolBroken,
                signature: format!("tool_broken:{sig}"),
                summary: format!("A tod tool is failing: {first}"),
            };
        }
    }

    if crate::github::infra_kind("failure", error).is_some() {
        let kind = crate::github::infra_kind("failure", error).unwrap_or("infrastructure");
        return named(Kind::CiInfra, &kind.replace(' ', "_"), &format!("CI {kind}"));
    }

    let sig = of_text(error);
    Classification {
        kind: Kind::Work,
        signature: if sig.is_empty() { String::new() } else { format!("work:{sig}") },
        summary: String::new(),
    }
}

/// The signature a failing tool's text would be an incident under, for an
/// agent-filed `broken` request whose error is that tool's (the request itself
/// is `AgentRequest`, which names no tool).
pub fn tool_signature(error: &str) -> Option<String> {
    if is_wrong_environment(error) {
        return None;
    }
    let sig = of_text(error);
    (!sig.is_empty()).then(|| format!("tool_broken:{sig}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_auth_whatever_their_wording() {
        for text in [
            "GitHub rejected the token (HTTP 401): it is expired, revoked, or mistyped",
            "GitHub credentials are not set up",
            "GitHub API error: Bad credentials",
        ] {
            let c = classify(Source::Github, text);
            assert_eq!((c.kind, c.signature.as_str()), (Kind::Auth, "auth:github"), "{text}");
        }
        assert_eq!(classify(Source::Other, "An error occurred (ExpiredToken) when calling").signature, "auth:aws");
        assert_eq!(classify(Source::Agent, "Invalid API key · Please run /login").signature, "auth:agent");
        // The same login texts in an agent-filed request (Claude's and Codex's).
        for text in ["Invalid API key · Please run /login", "Codex: you are not signed in. Run `codex login`", "Failed to authenticate. API Error: 401"] {
            let c = classify(Source::AgentRequest, text);
            assert_eq!((c.kind, c.signature.as_str()), (Kind::Auth, "auth:agent"), "{text}");
        }
    }

    #[test]
    fn a_brokered_noun_outside_a_cloud_sandbox_is_agent_work_not_a_broken_tool() {
        for noun in ["db", "access"] {
            let text = wrong_environment_message(noun);
            for source in [Source::Tool, Source::AgentRequest, Source::Agent, Source::Other] {
                let c = classify(source, &text);
                assert_eq!(c.kind, Kind::Work, "{text}");
                assert!(!c.kind.parks() && c.signature.is_empty());
            }
            assert_eq!(tool_signature(&text), None);
        }
    }

    #[test]
    fn provider_service_down_and_tool_failures() {
        assert_eq!(classify(Source::Agent, "API Error: 529 overloaded_error").signature, "provider:overloaded");
        assert_eq!(classify(Source::Agent, "rate_limit_error: slow down").kind, Kind::Provider);
        assert_eq!(classify(Source::Github, "GitHub is unavailable (HTTP 503)").signature, "service_down:github");
        let down = classify(Source::Other, "error sending request for url (https://db.internal.example/x): connection refused");
        assert_eq!((down.kind, down.signature.as_str()), (Kind::ServiceDown, "service_down:db.internal.example"));
        let a = classify(Source::Tool, "tod-cli: no such table foo (pid 4411)");
        let b = classify(Source::Tool, "tod-cli: no such table foo (pid 98)");
        assert_eq!(a.kind, Kind::ToolBroken);
        assert_eq!(a.signature, b.signature);
    }

    #[test]
    fn claude_acp_rate_limit_data_is_classified_even_without_a_reset() {
        let bare = r#"ACP error -32603: Internal error: You've hit your 5-hour limit · resets 8:10pm (UTC) ({"errorKind":"rate_limit"})"#;
        let timed = r#"opening session: ACP error -32603: Internal error: You've hit your weekly limit · resets 2am (America/Los_Angeles) ({"errorKind":"rate_limit","rateLimit":{"status":"rejected","resetsAt":1795514400,"rateLimitType":"seven_day","unifiedWindows":{"five_hour":{"status":"allowed","resetsAt":1795436400},"seven_day":{"status":"rejected","resetsAt":1795514400}}}})"#;
        for error in [bare, timed] {
            assert_eq!(classify(Source::Agent, error).signature, "provider:rate_limit");
        }
        assert_eq!(acp_rate_limit_reset_ms(timed), Some(1_795_514_400_000));
        assert_eq!(acp_rate_limit_reset_ms(bare), None);
        let unrelated = r#"ACP error -32603: Internal error ({"errorKind":"other","rateLimit":{"resetsAt":1795205400}})"#;
        assert_eq!(acp_rate_limit_reset_ms(unrelated), None);
        assert_eq!(classify(Source::Agent, unrelated).kind, Kind::Work);
        assert_eq!(acp_usage_window_without_reset(bare), Some(AcpUsageWindow::FiveHour));
        assert_eq!(acp_usage_window_without_reset(timed), None);
    }

    #[test]
    fn only_identified_acp_usage_windows_have_slow_retries() {
        let weekly = r#"opening session: ACP error -32603: Internal error: You've hit your weekly limit · resets 10am (UTC) ({"errorKind":"rate_limit"})"#;
        let typed = r#"ACP error -32603: Usage exhausted ({"errorKind":"rate_limit","rateLimit":{"rateLimitType":"seven_day"}})"#;
        for error in [weekly, typed] {
            assert_eq!(acp_usage_window_without_reset(error), Some(AcpUsageWindow::Weekly), "{error}");
        }
        for error in [
            r#"ACP error -32603: Too many requests ({"errorKind":"rate_limit"})"#,
            r#"ACP error -32603: You've hit your weekly limit ({"errorKind":"auth"})"#,
            r#"ACP error -32603: You've hit your weekly limit ({"errorKind":"rate_limit","rateLimitType":"monthly"})"#,
            "You've hit your weekly limit · resets 10am (UTC)",
        ] {
            assert_eq!(acp_usage_window_without_reset(error), None, "{error}");
        }
    }

    #[test]
    fn unknown_failures_are_the_changes_own_work_and_never_park() {
        for text in ["error[E0308]: mismatched types", "assertion failed: left == right", ""] {
            let c = classify(Source::Agent, text);
            assert_eq!(c.kind, Kind::Work, "{text}");
            assert!(!c.kind.parks());
        }
        // A GitHub rate limit has its own wait to the reset.
        let limited = "GitHub rate limit reached (secondary); the token and its access are fine (epoch 1900000000)";
        assert_eq!(classify(Source::Github, limited).kind, Kind::Work);
        // CI infrastructure is rerun per job, not parked.
        let infra = classify(Source::Other, "The runner has received a shutdown signal.");
        assert_eq!(infra.kind, Kind::CiInfra);
        assert!(!infra.kind.parks());
    }

    #[test]
    fn kinds_round_trip_through_their_names() {
        for kind in [Kind::CiInfra, Kind::Auth, Kind::ServiceDown, Kind::Provider, Kind::ToolBroken, Kind::Work] {
            assert_eq!(Kind::parse(kind.as_str()), Some(kind));
        }
    }
}
