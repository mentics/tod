//! Parses a gate-check agent's reply.
//!
//! The agent returns YAML front matter (`result`, `forward_lifecycle`,
//! `paused`) followed by a markdown findings body, and — when the request
//! carried criteria — a `---gate_results` section with one row per criterion.
//! See `assets/process/agents/state/base.md` ("Response format") for the
//! authoritative shape.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use uuid::Uuid;

/// `result` field of the front matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOutcome {
    Pass,
    Blocked,
    NeedsHuman,
    NoChange,
}

impl GateOutcome {
    fn parse(raw: &str) -> Result<Self> {
        match raw.trim() {
            "pass" => Ok(Self::Pass),
            "blocked" => Ok(Self::Blocked),
            "needs_human" => Ok(Self::NeedsHuman),
            "no_change" => Ok(Self::NoChange),
            other => bail!("unrecognized gate-check result: {other:?}"),
        }
    }

    /// Whether this outcome should advance the node's lifecycle.
    pub fn advances(self) -> bool {
        matches!(self, Self::Pass)
    }
}

#[derive(Debug, Clone)]
pub struct GateResultRow {
    pub criterion_id: Uuid,
    pub outcome: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GateCheckReply {
    pub result: GateOutcome,
    pub forward_lifecycle: Option<String>,
    pub paused: bool,
    /// Markdown findings body, after the front matter.
    pub findings: String,
    /// Present when the request carried criteria and the agent returned a
    /// `---gate_results` section.
    pub gate_results: Vec<GateResultRow>,
}

#[derive(Debug, Deserialize)]
struct FrontMatter {
    result: String,
    #[serde(default)]
    forward_lifecycle: Option<String>,
    #[serde(default)]
    paused: bool,
}

#[derive(Debug, Deserialize)]
struct GateResultRowRaw {
    criterion_id: String,
    outcome: String,
    #[serde(default)]
    detail: Option<String>,
}

/// Parse an agent's raw reply text into a [`GateCheckReply`].
pub fn parse_gate_reply(text: &str) -> Result<GateCheckReply> {
    let text = find_front_matter_start(text);
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("").trim();
    if first != "---" {
        bail!("gate-check reply did not start with YAML front matter (`---`)");
    }

    let mut front_matter_src = String::new();
    let mut consumed = "---\n".len();
    let mut closed = false;
    for line in lines {
        consumed += line.len() + 1;
        if line.trim() == "---" {
            closed = true;
            break;
        }
        front_matter_src.push_str(line);
        front_matter_src.push('\n');
    }
    if !closed {
        bail!("gate-check reply's front matter was never closed with `---`");
    }
    let front: FrontMatter =
        serde_yaml::from_str(&front_matter_src).context("failed to parse gate-check front matter")?;
    let result = GateOutcome::parse(&front.result)?;

    let rest = text.get(consumed.min(text.len())..).unwrap_or("");
    let (findings, gate_results_src) = split_gate_results_section(rest);
    let gate_results = match gate_results_src {
        Some(src) => parse_gate_results(&src)?,
        None => Vec::new(),
    };

    Ok(GateCheckReply {
        result,
        forward_lifecycle: front.forward_lifecycle,
        paused: front.paused,
        findings: findings.trim().to_string(),
        gate_results,
    })
}

/// Locate the start of the `---` front matter, tolerating the two ways a
/// model deviates from the literal format even when told not to: wrapping
/// the whole reply in a markdown code fence (the role doc shows the format
/// inside a fenced example, which reads as a template to copy), and
/// prepending a sentence of preamble before the front matter opens. Neither
/// changes the content that follows, so both are stripped rather than
/// rejected.
fn find_front_matter_start(text: &str) -> &str {
    let text = strip_wrapping_fence(text.trim());
    if text.lines().next().map(str::trim) == Some("---") {
        return text;
    }
    match text.find("\n---\n").or_else(|| text.find("\n---\r\n")) {
        Some(offset) => &text[offset + 1..],
        None => text,
    }
}

/// Strip a single leading/trailing markdown code fence (```` ``` ```` or
/// ```` ```yaml ````) that wraps the entire reply, if present.
fn strip_wrapping_fence(text: &str) -> &str {
    let Some(after_open) = text.strip_prefix("```") else {
        return text;
    };
    let after_open = after_open.trim_start_matches(|c: char| c.is_alphanumeric());
    let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);
    let trimmed_end = after_open.trim_end();
    match trimmed_end.strip_suffix("```") {
        Some(inner) => inner.trim_end(),
        None => text,
    }
}

/// Split the body into (findings text, gate_results YAML source), locating a
/// line that starts a `---gate_results` block. Any further `---`-prefixed
/// section (`---obligation_mutations`, `---design_patch`, …) ends it.
fn split_gate_results_section(body: &str) -> (String, Option<String>) {
    let Some(start) = body.find("\n---gate_results") else {
        // Also handle the section opening the very first line of `body`.
        if let Some(rest) = body.strip_prefix("---gate_results") {
            let (src, _) = take_until_next_section(rest);
            return (String::new(), Some(src));
        }
        return (body.to_string(), None);
    };
    let findings = body[..start].to_string();
    let after_marker = &body[start + 1..];
    let after_marker = after_marker
        .strip_prefix("---gate_results")
        .unwrap_or(after_marker);
    let (src, _) = take_until_next_section(after_marker);
    (findings, Some(src))
}

/// From just after a `---section_name` marker, return the section's raw
/// source up to (not including) the next `\n---` marker line, if any.
fn take_until_next_section(rest: &str) -> (String, Option<usize>) {
    match rest.find("\n---") {
        Some(next) => (rest[..next].to_string(), Some(next)),
        None => (rest.to_string(), None),
    }
}

fn parse_gate_results(src: &str) -> Result<Vec<GateResultRow>> {
    let raw: Vec<GateResultRowRaw> =
        serde_yaml::from_str(src).context("failed to parse gate_results section")?;
    raw.into_iter()
        .map(|row| {
            let criterion_id = Uuid::parse_str(row.criterion_id.trim())
                .with_context(|| format!("invalid criterion_id: {}", row.criterion_id))?;
            Ok(GateResultRow {
                criterion_id,
                outcome: row.outcome.trim().to_string(),
                detail: row.detail,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pass_with_gate_results() {
        let text = r#"
---
result: pass
forward_lifecycle: planning
paused: false
---

# Findings

Everything checks out.

---gate_results
- criterion_id: a1000001-0001-4001-8001-000000000001
  outcome: pass
  detail: "confirmed in design doc"
- criterion_id: a1000001-0001-4001-8001-000000000002
  outcome: waived
  detail: "not applicable here"
"#;
        let reply = parse_gate_reply(text).unwrap();
        assert!(reply.result.advances());
        assert_eq!(reply.forward_lifecycle.as_deref(), Some("planning"));
        assert!(!reply.paused);
        assert!(reply.findings.contains("Everything checks out."));
        assert_eq!(reply.gate_results.len(), 2);
        assert_eq!(reply.gate_results[0].outcome, "pass");
        assert_eq!(reply.gate_results[1].outcome, "waived");
    }

    #[test]
    fn parses_blocked_without_gate_results() {
        let text = "---\nresult: blocked\nforward_lifecycle: null\npaused: true\n---\n\nMissing design content.";
        let reply = parse_gate_reply(text).unwrap();
        assert!(!reply.result.advances());
        assert_eq!(reply.forward_lifecycle, None);
        assert!(reply.paused);
        assert!(reply.gate_results.is_empty());
        assert!(reply.findings.contains("Missing design content."));
    }

    #[test]
    fn rejects_missing_front_matter() {
        assert!(parse_gate_reply("no front matter here").is_err());
    }

    #[test]
    fn tolerates_a_wrapping_code_fence() {
        let text = "```yaml\n---\nresult: pass\nforward_lifecycle: planning\npaused: false\n---\n\nLooks good.\n```";
        let reply = parse_gate_reply(text).unwrap();
        assert!(reply.result.advances());
        assert_eq!(reply.forward_lifecycle.as_deref(), Some("planning"));
        assert!(reply.findings.contains("Looks good."));
    }

    #[test]
    fn tolerates_preamble_before_the_front_matter() {
        let text = "I'll evaluate the forward gate now.\n\n---\nresult: pass\nforward_lifecycle: planning\npaused: false\n---\n\nAll criteria satisfied.";
        let reply = parse_gate_reply(text).unwrap();
        assert!(reply.result.advances());
        assert_eq!(reply.forward_lifecycle.as_deref(), Some("planning"));
        assert!(reply.findings.contains("All criteria satisfied."));
    }

    #[test]
    fn gate_results_section_stops_before_next_section() {
        let text = r#"---
result: pass
forward_lifecycle: ready
paused: false
---

Body text.

---gate_results
- criterion_id: a1000002-0002-4002-8002-000000000001
  outcome: pass
---obligation_mutations
- op: create
  kind: requirement
  node_id: a1000002-0002-4002-8002-000000000099
  body: "..."
"#;
        let reply = parse_gate_reply(text).unwrap();
        assert_eq!(reply.gate_results.len(), 1);
    }
}
