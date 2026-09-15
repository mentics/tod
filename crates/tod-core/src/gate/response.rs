//! Parses a gate-check agent's reply.
//!
//! The agent returns exactly **one YAML document** — `result`,
//! `forward_lifecycle`, `paused`, `findings`, and (when the request carried
//! criteria) `gate_results`, all as fields of the same document, not
//! markdown text with embedded sections. See
//! `assets/process/agents/state/base.md` ("Response format") for the
//! authoritative shape. This is a structural protocol, not a chat reply: the
//! app renders `gate_results` as a table with per-row action buttons, so
//! every row must parse or the whole check is unusable.
//!
//! Parsing is strict about the document's *shape* (one YAML mapping, known
//! fields) but tolerant of incidental wrapping a model tends to add around
//! it — a code fence, a sentence of preamble, a leading `---` document
//! marker — since none of that changes the content.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use uuid::Uuid;

/// `result` field of the reply.
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
            // `gate_results[].outcome` uses `pass | fail | waived` — a
            // different, smaller vocabulary for the same general idea, and
            // an agent that just wrote a bunch of `fail` rows sometimes
            // reaches for the same word at the top level instead of
            // `blocked`. It unambiguously means the gate didn't pass, so
            // it's accepted here rather than failing the whole reply.
            "fail" | "failed" => Ok(Self::Blocked),
            other => bail!("unrecognized gate-check result: {other:?}"),
        }
    }

    /// Whether this outcome should advance the node's lifecycle.
    pub fn advances(self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// How the user can resolve a failing criterion from within the app, as
/// reported by the agent for that row. Drives which button(s) the lifecycle
/// panel shows next to it — `Interview` alongside the always-present Waive,
/// `None` when there's no in-app destination and Waive is the only option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GateAction {
    #[default]
    None,
    Interview,
}

impl GateAction {
    fn parse(raw: &str) -> Result<Self> {
        match raw.trim() {
            "" | "none" => Ok(Self::None),
            "interview" => Ok(Self::Interview),
            other => bail!("unrecognized gate_results action: {other:?}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GateResultRow {
    pub criterion_id: Uuid,
    pub outcome: String,
    pub detail: Option<String>,
    pub action: GateAction,
}

#[derive(Debug, Clone)]
pub struct GateCheckReply {
    pub result: GateOutcome,
    pub forward_lifecycle: Option<String>,
    pub paused: bool,
    /// Findings/summary text, straight from the `findings` field.
    pub findings: String,
    /// Present when the request carried criteria and the agent returned a
    /// `gate_results` list.
    pub gate_results: Vec<GateResultRow>,
}

#[derive(Debug, Deserialize)]
struct RawReply {
    result: String,
    #[serde(default)]
    forward_lifecycle: Option<String>,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    findings: String,
    #[serde(default)]
    gate_results: Vec<RawRow>,
}

#[derive(Debug, Deserialize)]
struct RawRow {
    criterion_id: String,
    outcome: String,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    action: String,
}

/// Parse an agent's raw reply text into a [`GateCheckReply`].
pub fn parse_gate_reply(text: &str) -> Result<GateCheckReply> {
    let yaml = extract_yaml_document(text);
    let raw: RawReply = match serde_yaml::from_str(yaml) {
        Ok(raw) => raw,
        Err(err) => serde_yaml::from_str(&quote_free_text_fields(yaml))
            .map_err(|_| err)
            .context("failed to parse gate-check reply as YAML")?,
    };
    let result = GateOutcome::parse(&raw.result)?;

    let gate_results = raw
        .gate_results
        .into_iter()
        .map(|row| {
            let criterion_id = Uuid::parse_str(row.criterion_id.trim())
                .with_context(|| format!("invalid criterion_id: {}", row.criterion_id))?;
            Ok(GateResultRow {
                criterion_id,
                outcome: row.outcome.trim().to_string(),
                detail: row.detail,
                action: GateAction::parse(&row.action)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(GateCheckReply {
        result,
        forward_lifecycle: raw.forward_lifecycle,
        paused: raw.paused,
        findings: raw.findings.trim().to_string(),
        gate_results,
    })
}

/// Strip incidental wrapping around the single YAML document: a markdown
/// code fence — wrapping the whole reply, or (a model narrating first,
/// *then* fencing the document — the most common real-world shape) preceded
/// by a paragraph of preamble — plus a leading `---` document marker and a
/// preamble sentence when there's no fence at all. None of these change the
/// document's content, so they're stripped rather than rejected — but
/// nothing beyond this is tolerated, since the fields inside must still
/// parse as one strict mapping.
fn extract_yaml_document(text: &str) -> &str {
    let text = text.trim();
    if let Some(fenced) = extract_fenced_block(text) {
        return fenced;
    }
    let text = match text.strip_prefix("---") {
        Some(rest) => rest.trim_start_matches(['\n', '\r', ' ']),
        None => text,
    };
    if text.trim_start().starts_with("result:") {
        return text;
    }
    // Tolerate a preamble sentence before the document actually starts. Use
    // the *last* occurrence: the prompt itself hands the model example
    // `result:`-shaped text (the response-format template in
    // `assets/process/agents/state/base.md`), so a model that quotes or
    // paraphrases that guidance before answering can put a spurious
    // `result:`-looking line earlier in the message than its real answer.
    match text.rfind("\nresult:") {
        Some(offset) => &text[offset + 1..],
        None => text,
    }
}

/// Double-quote the free-text fields (`findings`, `detail`) when the agent
/// wrote them as bare one-line plain scalars. Prose routinely contains `: `
/// ("covers the entire feature: X, Y, Z"), which YAML reads as a nested
/// mapping and rejects outright. Only used as a retry after a strict parse
/// fails, and only touches values that aren't already quoted or block
/// scalars, so it can't change a reply that parsed on its own.
fn quote_free_text_fields(yaml: &str) -> String {
    let mut out = String::with_capacity(yaml.len() + 16);
    for line in yaml.lines() {
        out.push_str(&quote_free_text_line(line).unwrap_or_else(|| line.to_string()));
        out.push('\n');
    }
    out
}

fn quote_free_text_line(line: &str) -> Option<String> {
    let body = line.trim_start();
    let indent = &line[..line.len() - body.len()];
    let (item, body) = match body.strip_prefix("- ") {
        Some(rest) => ("- ", rest),
        None => ("", body),
    };
    let (key, value) = ["findings:", "detail:"]
        .iter()
        .find_map(|key| body.strip_prefix(key).map(|value| (*key, value)))?;
    if !value.starts_with(' ') {
        return None;
    }
    let value = value.trim();
    if value.is_empty() || value.starts_with(['"', '\'', '|', '>']) {
        return None;
    }
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    Some(format!("{indent}{item}{key} \"{escaped}\""))
}

/// Find every markdown code fence (```` ``` ```` or ```` ```yaml ````) in
/// `text` and return the inner content of the one that looks like the real
/// reply, ignoring anything before the first opening fence or after the last
/// closing one.
///
/// The gate-check prompt itself contains fenced YAML the model can end up
/// echoing before its real answer: the app's own `gate_check:` request block
/// (`gate::context::render_gate_check_yaml`) and the response-format
/// template in `assets/process/agents/state/base.md` (`result: pass |
/// blocked | ...`, a literal placeholder, not real data). Neither has a
/// top-level `result:` field the way an actual reply does, so when more than
/// one fence is present, prefer the last one that has one — falling back to
/// the last fence outright only when none do, since that's still the model's
/// final word.
fn extract_fenced_block(text: &str) -> Option<&str> {
    let blocks = find_fenced_blocks(text);
    blocks
        .iter()
        .rev()
        .find(|block| has_top_level_result_field(block))
        .or_else(|| blocks.last())
        .copied()
}

fn has_top_level_result_field(block: &str) -> bool {
    block.lines().any(|line| line.starts_with("result:"))
}

fn find_fenced_blocks(text: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut search_from = 0;
    while let Some(rel_start) = text[search_from..].find("```") {
        let open_at = search_from + rel_start;
        let after_open = &text[open_at + 3..];
        let after_open = after_open.trim_start_matches(|c: char| c.is_alphanumeric());
        let after_open = after_open.strip_prefix('\n').unwrap_or(after_open);
        let content_start = text.len() - after_open.len();
        let Some(rel_end) = after_open.find("```") else {
            break;
        };
        blocks.push(after_open[..rel_end].trim_end());
        search_from = content_start + rel_end + 3;
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pass_with_gate_results() {
        let text = r#"
result: pass
forward_lifecycle: planning
paused: false
findings: |
  Everything checks out.
gate_results:
  - criterion_id: a1000001-0001-4001-8001-000000000001
    outcome: pass
    detail: "confirmed in design doc"
    action: none
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
        assert_eq!(reply.gate_results[0].action, GateAction::None);
        assert_eq!(reply.gate_results[1].outcome, "waived");
    }

    #[test]
    fn parses_blocked_with_an_interview_action() {
        let text = r#"
result: blocked
forward_lifecycle: null
paused: true
findings: "Missing design content."
gate_results:
  - criterion_id: a1000001-0001-4001-8001-000000000003
    outcome: fail
    detail: "no answer recorded for the auth question"
    action: interview
"#;
        let reply = parse_gate_reply(text).unwrap();
        assert!(!reply.result.advances());
        assert_eq!(reply.forward_lifecycle, None);
        assert!(reply.paused);
        assert_eq!(reply.gate_results.len(), 1);
        assert_eq!(reply.gate_results[0].action, GateAction::Interview);
        assert!(reply.findings.contains("Missing design content."));
    }

    #[test]
    fn parses_blocked_without_gate_results() {
        let text = "result: blocked\nforward_lifecycle: null\npaused: true\nfindings: \"Missing design content.\"\n";
        let reply = parse_gate_reply(text).unwrap();
        assert!(!reply.result.advances());
        assert_eq!(reply.forward_lifecycle, None);
        assert!(reply.paused);
        assert!(reply.gate_results.is_empty());
        assert!(reply.findings.contains("Missing design content."));
    }

    #[test]
    fn rejects_missing_result_field() {
        assert!(parse_gate_reply("no yaml document here").is_err());
    }

    #[test]
    fn treats_a_top_level_fail_result_as_blocked() {
        // Real failure: the agent used `result: fail`, borrowing the
        // gate_results outcome vocabulary (pass | fail | waived) instead of
        // the top-level one (pass | blocked | needs_human | no_change).
        let text = "result: fail\nforward_lifecycle: null\npaused: true\nfindings: \"blocked\"\n";
        let reply = parse_gate_reply(text).unwrap();
        assert!(!reply.result.advances());
        assert_eq!(reply.result, GateOutcome::Blocked);
    }

    #[test]
    fn rejects_an_unrecognized_action() {
        let text = r#"
result: blocked
paused: true
gate_results:
  - criterion_id: a1000001-0001-4001-8001-000000000003
    outcome: fail
    action: teleport
"#;
        assert!(parse_gate_reply(text).is_err());
    }

    #[test]
    fn tolerates_a_wrapping_code_fence() {
        let text = "```yaml\nresult: pass\nforward_lifecycle: planning\npaused: false\nfindings: \"Looks good.\"\n```";
        let reply = parse_gate_reply(text).unwrap();
        assert!(reply.result.advances());
        assert_eq!(reply.forward_lifecycle.as_deref(), Some("planning"));
        assert!(reply.findings.contains("Looks good."));
    }

    #[test]
    fn tolerates_a_leading_document_marker() {
        let text = "---\nresult: pass\nforward_lifecycle: planning\npaused: false\n";
        let reply = parse_gate_reply(text).unwrap();
        assert!(reply.result.advances());
        assert_eq!(reply.forward_lifecycle.as_deref(), Some("planning"));
    }

    #[test]
    fn tolerates_narration_before_a_fenced_document() {
        // Reproduces a real gate-check reply: a paragraph of reasoning, then
        // the YAML document inside a ```yaml fence. Previously this fell
        // through to the bare-preamble path, which sliced from `\nresult:`
        // but never dropped the trailing ``` fence line, so serde_yaml choked
        // on the stray backticks after the block-scalar `findings` field.
        let text = "Let me work through this.\n\nSome reasoning here.\n\n```yaml\nresult: blocked\nforward_lifecycle: null\npaused: false\nfindings: >\n  Multi-line\n  summary text.\ngate_results:\n  - criterion_id: a1000001-0001-4001-8001-000000000003\n    outcome: fail\n    detail: \"needs a decision\"\n    action: interview\n```";
        let reply = parse_gate_reply(text).unwrap();
        assert!(!reply.result.advances());
        assert!(reply.findings.contains("Multi-line summary text."));
        assert_eq!(reply.gate_results.len(), 1);
        assert_eq!(reply.gate_results[0].action, GateAction::Interview);
    }

    #[test]
    fn picks_the_real_reply_when_an_earlier_fence_echoes_the_request() {
        // Reproduces the observed "missing field `result`" bug: the model
        // quotes the app's own gate_check request block (no `result` field)
        // before giving its real, fenced verdict later in the same message.
        let text = "Here's the gate_check block I'm evaluating:\n\n```yaml\ngate_check:\n  forward_state: planning\n  criteria: []\n```\n\nAnd here is my verdict:\n\n```yaml\nresult: pass\nforward_lifecycle: planning\npaused: false\nfindings: \"All good.\"\n```";
        let reply = parse_gate_reply(text).unwrap();
        assert!(reply.result.advances());
        assert_eq!(reply.forward_lifecycle.as_deref(), Some("planning"));
    }

    #[test]
    fn falls_back_to_the_last_fence_when_none_have_a_result_field() {
        let text =
            "```yaml\ngate_check:\n  forward_state: planning\n```\n\n```yaml\nunrelated: true\n```";
        // Neither fence has `result:` — falls back to the last fence, which
        // then fails to deserialize (missing required field), same as today.
        assert!(parse_gate_reply(text).is_err());
    }

    #[test]
    fn tolerates_an_unquoted_findings_line_containing_a_colon() {
        // Real failure ("mapping values are not allowed in this context"):
        // the agent left `findings` and a row's `detail` as bare plain
        // scalars whose prose contained `: `.
        let text = "result: pass\nforward_lifecycle: ready\npaused: false\nfindings: All gate criteria pass. 24 steps cover the entire feature: DataSource trait, \"Linear\" adapter.\ngate_results:\n  - criterion_id: a1000002-0002-4002-8002-000000000001\n    outcome: pass\n    detail: Evidence: plan steps map to obligations.\n    action: none\n";
        let reply = parse_gate_reply(text).unwrap();
        assert!(reply.result.advances());
        assert_eq!(
            reply.findings,
            "All gate criteria pass. 24 steps cover the entire feature: DataSource trait, \"Linear\" adapter."
        );
        assert_eq!(
            reply.gate_results[0].detail.as_deref(),
            Some("Evidence: plan steps map to obligations.")
        );
    }

    #[test]
    fn tolerates_preamble_before_the_document() {
        let text = "I'll evaluate the forward gate now.\n\nresult: pass\nforward_lifecycle: planning\npaused: false\nfindings: \"All criteria satisfied.\"\n";
        let reply = parse_gate_reply(text).unwrap();
        assert!(reply.result.advances());
        assert_eq!(reply.forward_lifecycle.as_deref(), Some("planning"));
        assert!(reply.findings.contains("All criteria satisfied."));
    }
}
