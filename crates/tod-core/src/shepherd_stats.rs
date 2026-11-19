//! Statistics about how the lifecycle is going, computed from what tod already
//! keeps (`doc/history/improvement-log.md`, "Statistics worth collecting").
//!
//! One read-only pass over the store ([`compute`]) answers seven questions, each
//! a section of [`Stats`]. Nothing here records anything new: a figure that has
//! no source in the data is named in its section's `unavailable` list, with
//! why, so a reader never takes a missing number for a zero.
//!
//! - [`CostRow`]: per node and phase, sessions, agent turns, tool calls, wall
//!   time (conversations and their turns). Tokens are not kept anywhere.
//! - [`ReviewRow`]: the review loop per node and stay (conversations of the
//!   review protocol and the findings each recorded).
//! - [`PrDraftRow`]: the pull-request loop per node (the `pr` and review
//!   conversations of `pr_draft`/`pr_review`, findings posted as threads,
//!   webhook events from GitHub).
//! - [`RequestStats`]: decisions and their answers.
//! - [`StopStats`]: `run_state` now, and how often it changed (`journey_changes`).
//! - [`ChurnRow`]: per node, sandbox locations made and removed
//!   (`journey_changes` on `node_files_locations`) and sandbox losses
//!   (`cloud_nodes.lost_at`).
//! - [`GitRetryRow`]: per node (or workdir, when the retry was not for a
//!   node), the git calls tod retried and whether each recovered
//!   (`tod_store::git_retries`, the only section whose figures are recorded
//!   for the purpose rather than derived).
//! - [`StopMismatchStats`]: runs stopped or failed (`run_state`) whose node's
//!   standing shows something else, and for how long
//!   (`crate::stop_mismatch`; needs the data root for the saved runs).
//!
//! `journey_changes` is the app's change journal; it holds only what was
//! written since it was introduced and may be trimmed, so every figure that
//! comes from it is a count of what the journal holds (the sections say so).

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::BTreeMap;
use uuid::Uuid;

/// What to look at: every node, or one slug, since a time.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Only this node (its slug).
    pub node: Option<String>,
    /// Only what happened at or after this (Unix ms).
    pub since_ms: Option<i64>,
}

/// Which sections to compute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Cost,
    Review,
    PrDraft,
    Requests,
    Stops,
    Sandbox,
    GitRetries,
    StopMismatch,
}

impl Section {
    pub const ALL: [Section; 8] = [
        Section::Cost,
        Section::Review,
        Section::PrDraft,
        Section::Requests,
        Section::Stops,
        Section::Sandbox,
        Section::GitRetries,
        Section::StopMismatch,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Section::Cost => "cost",
            Section::Review => "review",
            Section::PrDraft => "pr-draft",
            Section::Requests => "requests",
            Section::Stops => "stops",
            Section::Sandbox => "sandbox",
            Section::GitRetries => "git-retries",
            Section::StopMismatch => "stop-mismatch",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|s| s.name() == text)
            .ok_or_else(|| anyhow::anyhow!("unknown section `{text}` (expected one of: {})", Self::names()))
    }

    pub fn names() -> String {
        Self::ALL.iter().map(|s| s.name()).collect::<Vec<_>>().join(", ")
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Stats {
    pub cost: Option<Cost>,
    pub review: Option<Review>,
    pub pr_draft: Option<PrDraft>,
    pub requests: Option<RequestStats>,
    pub stops: Option<StopStats>,
    pub sandbox: Option<Sandbox>,
    pub git_retries: Option<GitRetries>,
    pub stop_mismatch: Option<StopMismatchStats>,
}

// ---- cost ----

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct CostRow {
    pub node: String,
    /// The state the agent worked in (`design`, `pr_draft`, ...) when the
    /// conversation records one, else its protocol (`implementation`, `fix`).
    pub phase: String,
    pub sessions: u64,
    pub agent_turns: u64,
    pub tool_calls: u64,
    /// Time the sessions took, turn to turn, without idle gaps over 30 minutes (ms).
    pub wall_ms: i64,
    /// What the agent reported spending (`tod_store::conversation_usage`),
    /// for the turns that reported any: input not cached, cache reads, cache
    /// writes and output tokens.
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
}

impl CostRow {
    /// The share of the prompt tokens read from the cache, when any were counted.
    pub fn cache_read_share(&self) -> Option<f64> {
        let prompt = self.input_tokens + self.cache_read_tokens + self.cache_write_tokens;
        (prompt > 0).then(|| self.cache_read_tokens as f64 / prompt as f64)
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Cost {
    pub rows: Vec<CostRow>,
    /// The same figures summed over nodes, per phase.
    pub by_phase: Vec<CostRow>,
    pub unavailable: Vec<String>,
}

// ---- review ----

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ReviewRow {
    pub node: String,
    /// `review` (the code review before `pr`) or `pr_draft` (the review of the
    /// draft's feedback), from the conversation's `from_state`.
    pub stay: String,
    pub rounds: u64,
    pub findings: u64,
    pub clean_rounds: u64,
    /// Findings an earlier round's fixed finding repeats (same file and text).
    pub reopened: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Review {
    pub rows: Vec<ReviewRow>,
    pub rounds: u64,
    pub findings: u64,
    pub clean_rounds: u64,
    pub reopened: u64,
    /// Findings by status (`fixed`, `rejected`, `declined`, `out_of_scope`, `open`).
    pub by_status: BTreeMap<String, u64>,
    /// `(rejected + declined) / findings`, none without findings.
    pub rejected_or_declined_ratio: Option<f64>,
    pub unavailable: Vec<String>,
}

// ---- pr draft ----

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct PrDraftRow {
    pub node: String,
    /// Sessions of the PR agent (`pr` protocol) in `pr`, `pr_draft`, `pr_review`.
    pub pr_agent_sessions: u64,
    /// Review rounds run on the draft (`review` conversations from `pr_draft`).
    pub draft_review_rounds: u64,
    /// Findings of those rounds that were posted on the PR as threads.
    pub findings_posted_as_threads: u64,
    /// GitHub webhook events the node was sent, by kind.
    pub github_events: BTreeMap<String, u64>,
    pub pushes: u64,
    pub ci_failures: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PrDraft {
    pub rows: Vec<PrDraftRow>,
    pub pr_agent_sessions: u64,
    pub draft_review_rounds: u64,
    pub findings_posted_as_threads: u64,
    pub pushes: u64,
    pub ci_failures: u64,
    /// `ci_failures / pushes`, none without pushes.
    pub ci_failures_per_push: Option<f64>,
    pub unavailable: Vec<String>,
}

// ---- requests ----

#[derive(Debug, Clone, Default, Serialize)]
pub struct RequestStats {
    pub total: u64,
    pub pending: u64,
    pub answered: u64,
    pub withdrawn: u64,
    pub by_reason: BTreeMap<String, u64>,
    /// Answered by picking an option the agent offered.
    pub answered_with_offered_option: u64,
    /// Answered with free text.
    pub answered_with_text: u64,
    /// Answered with words that say go on, try again or it is done elsewhere:
    /// the question was probably not needed (a reading of the text, not a fact).
    pub carry_on_like: u64,
    /// Requests whose reason is `broken` or `access`: about tod's setup, not
    /// the work.
    pub about_setup: u64,
    pub answered_by_user: u64,
    pub answered_by_other: u64,
    pub median_wait_secs: Option<i64>,
    pub max_wait_secs: Option<i64>,
    /// How long the pending ones have waited, oldest first (seconds).
    pub pending_wait_secs: Vec<i64>,
    pub unavailable: Vec<String>,
}

// ---- stops ----

#[derive(Debug, Clone, Default, Serialize)]
pub struct StopStats {
    /// `run_state` rows now, by `phase` then `reason_kind`.
    pub now_by_phase: BTreeMap<String, u64>,
    pub stopped_by_reason: BTreeMap<String, u64>,
    pub stopped_by_resumable_by: BTreeMap<String, u64>,
    /// `run_state` writes the journal holds (every change of a node's run).
    pub run_state_changes_journaled: u64,
    pub unavailable: Vec<String>,
}

// ---- sandbox ----

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ChurnRow {
    pub node: String,
    /// Locations made for the node (journaled inserts).
    pub locations_made: u64,
    pub locations_removed: u64,
    /// A sandbox made after the first one: a replacement.
    pub replacements: u64,
    pub lost_now: bool,
    /// Notes on the node's activity about resetting or deleting its environment.
    pub environment_resets: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Sandbox {
    pub rows: Vec<ChurnRow>,
    pub nodes_with_replacements: u64,
    pub replacements: u64,
    pub lost_now: u64,
    pub unavailable: Vec<String>,
}

// ---- git retries ----

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct GitRetryRow {
    /// The node's slug, or the workdir for a retry made for no node.
    pub subject: String,
    /// Git calls that needed at least one retry.
    pub calls: u64,
    pub retries: u64,
    pub recovered: u64,
    pub gave_up: u64,
    pub lock_race: u64,
    pub network: u64,
    /// Calls by operation (`fetch`, `submodule update`, ...).
    pub by_op: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct GitRetries {
    pub rows: Vec<GitRetryRow>,
    pub calls: u64,
    pub retries: u64,
    pub recovered: u64,
    pub gave_up: u64,
    pub unavailable: Vec<String>,
}

// ---- stop mismatch ----

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct StopMismatchRow {
    pub node: String,
    /// The run's stop: reason kind and text.
    pub run: String,
    /// What the node's standing shows instead.
    pub shown: String,
    /// How long the run has been stopped (seconds).
    pub lasted_secs: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StopMismatchStats {
    /// Longest-lasting first.
    pub rows: Vec<StopMismatchRow>,
    pub count: u64,
    pub longest_secs: Option<i64>,
    pub unavailable: Vec<String>,
}

// ---- computing ----

/// Computes the requested sections.
///
/// [`Section::StopMismatch`] needs the data root (the saved runs behind each
/// node's standing), so asking for it here is an error: use
/// [`compute_with_root`].
pub fn compute(conn: &Connection, filter: &Filter, sections: &[Section]) -> Result<Stats> {
    compute_with_root(conn, filter, sections, None)
}

/// [`compute`], with the data root the standings are worked out from.
pub fn compute_with_root(
    conn: &Connection,
    filter: &Filter,
    sections: &[Section],
    data_root: Option<&std::path::Path>,
) -> Result<Stats> {
    let mut stats = Stats::default();
    for section in sections {
        match section {
            Section::Cost => stats.cost = Some(cost(conn, filter)?),
            Section::Review => stats.review = Some(review(conn, filter)?),
            Section::PrDraft => stats.pr_draft = Some(pr_draft(conn, filter)?),
            Section::Requests => stats.requests = Some(requests(conn, filter)?),
            Section::Stops => stats.stops = Some(stops(conn, filter)?),
            Section::Sandbox => stats.sandbox = Some(sandbox(conn, filter)?),
            Section::GitRetries => stats.git_retries = Some(git_retries(conn, filter)?),
            Section::StopMismatch => {
                let root = data_root
                    .ok_or_else(|| anyhow::anyhow!("the stop-mismatch section needs the data root to work out each node's standing"))?;
                stats.stop_mismatch = Some(stop_mismatch(conn, filter, root)?);
            }
        }
    }
    Ok(stats)
}

/// `AND` clauses for a node column (`n.slug`) and a time column; `?1` is the
/// slug (or null), `?2` the earliest time (or null).
fn scope(node_col: &str, time_col: &str) -> String {
    format!("AND (?1 IS NULL OR {node_col} = ?1) AND (?2 IS NULL OR {time_col} >= ?2)")
}

fn tool_calls_in(parts: &str) -> Result<u64> {
    let value: serde_json::Value = serde_json::from_str(parts)
        .map_err(|e| anyhow::anyhow!("a conversation turn's parts are not JSON: {e}"))?;
    Ok(value
        .as_array()
        .map(|a| a.iter().filter(|p| p.get("kind").and_then(|k| k.as_str()) == Some("tool")).count() as u64)
        .unwrap_or(0))
}

/// A gap between turns longer than this is the session sitting idle (waiting
/// for the user or a retry), not the agent working.
const IDLE_GAP_MS: i64 = 30 * 60 * 1000;

fn phase_label(protocol: &str, from_state: &str) -> String {
    if from_state.is_empty() { protocol.to_string() } else { format!("{protocol}:{from_state}") }
}

fn cost(conn: &Connection, f: &Filter) -> Result<Cost> {
    let sql = format!(
        "SELECT n.slug, c.protocol, COALESCE(c.from_state, ''), c.id, c.created_at, c.updated_at
         FROM conversations c JOIN nodes n ON n.id = COALESCE(c.focus_node_id, c.focus_id)
         WHERE 1 = 1 {}",
        scope("n.slug", "c.created_at")
    );
    let mut stmt = conn.prepare(&sql)?;
    let convs: Vec<(String, String, String, Vec<u8>, i64, i64)> = stmt
        .query_map(params![f.node, f.since_ms], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut turn_stmt = conn.prepare(
        "SELECT parts FROM conversation_turns WHERE conversation_id = ?1 AND role = 'agent'",
    )?;
    let mut rows: BTreeMap<(String, String), CostRow> = BTreeMap::new();
    let mut stamp_stmt =
        conn.prepare("SELECT created_at FROM conversation_turns WHERE conversation_id = ?1 ORDER BY seq")?;
    for (node, protocol, from_state, id, created, _updated) in convs {
        let phase = phase_label(&protocol, &from_state);
        let row = rows.entry((node.clone(), phase.clone())).or_insert_with(|| CostRow {
            node,
            phase,
            ..Default::default()
        });
        row.sessions += 1;
        let usage = tod_store::conversation_usage::conversation_totals(
            conn,
            Uuid::from_slice(&id).context("a conversation id that is not a UUID")?,
        )?;
        row.input_tokens += usage.counts.input;
        row.cache_read_tokens += usage.counts.cache_read;
        row.cache_write_tokens += usage.counts.cache_write;
        row.output_tokens += usage.counts.output;
        let stamps: Vec<i64> = stamp_stmt.query_map([&id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut previous = created;
        for at in stamps {
            // The time a turn took, unless the session sat idle before it.
            let gap = at - previous;
            if (0..=IDLE_GAP_MS).contains(&gap) {
                row.wall_ms += gap;
            }
            previous = at;
        }
        let parts: Vec<Option<String>> =
            turn_stmt.query_map([&id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for p in parts {
            row.agent_turns += 1;
            if let Some(p) = p {
                row.tool_calls += tool_calls_in(&p)?;
            }
        }
    }
    let rows: Vec<CostRow> = rows.into_values().collect();
    let mut by_phase: BTreeMap<String, CostRow> = BTreeMap::new();
    for r in &rows {
        let t = by_phase
            .entry(r.phase.clone())
            .or_insert_with(|| CostRow { node: "*".into(), phase: r.phase.clone(), ..Default::default() });
        t.sessions += r.sessions;
        t.agent_turns += r.agent_turns;
        t.tool_calls += r.tool_calls;
        t.wall_ms += r.wall_ms;
        t.input_tokens += r.input_tokens;
        t.cache_read_tokens += r.cache_read_tokens;
        t.cache_write_tokens += r.cache_write_tokens;
        t.output_tokens += r.output_tokens;
    }
    Ok(Cost {
        rows,
        by_phase: by_phase.into_values().collect(),
        unavailable: vec![
            "tokens: counted per turn from when tod began recording them (`conversation_usage`), as the agent reported them; a session before that, or whose agent reports no usage, shows none (cache is the share of the prompt read from the prompt cache)"
                .into(),
            "wall is the sum of the gaps from a session's start through its turns, leaving out any gap over 30 minutes (idle); it is time to a turn, not CPU or model time"
                .into(),
        ],
    })
}

fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn review(conn: &Connection, f: &Filter) -> Result<Review> {
    let sql = format!(
        "SELECT n.slug, COALESCE(c.from_state, ''), c.id, c.created_at
         FROM conversations c JOIN nodes n ON n.id = COALESCE(c.focus_node_id, c.focus_id)
         WHERE c.protocol = 'review' AND c.skill_group = 0 {}
         ORDER BY n.slug, c.created_at",
        scope("n.slug", "c.created_at")
    );
    let rounds: Vec<(String, String, Vec<u8>, i64)> = conn
        .prepare(&sql)?
        .query_map(params![f.node, f.since_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut finding_stmt = conn.prepare(
        "SELECT COALESCE(file, ''), summary, status FROM review_findings WHERE conversation_id = ?1 ORDER BY seq",
    )?;
    let mut rows: BTreeMap<(String, String), ReviewRow> = BTreeMap::new();
    // Per node and stay, the findings of earlier rounds that were marked fixed.
    let mut fixed: BTreeMap<(String, String), Vec<(String, String)>> = BTreeMap::new();
    let mut out = Review::default();
    for (node, stay, id, _) in rounds {
        let key = (node.clone(), stay.clone());
        let findings: Vec<(String, String, String)> =
            finding_stmt.query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        let row = rows.entry(key.clone()).or_insert_with(|| ReviewRow {
            node,
            stay: if stay.is_empty() { "review".into() } else { stay },
            ..Default::default()
        });
        row.rounds += 1;
        if findings.is_empty() {
            row.clean_rounds += 1;
        }
        let earlier = fixed.entry(key).or_default();
        let mut now_fixed = Vec::new();
        for (file, summary, status) in &findings {
            row.findings += 1;
            let sig = (file.clone(), normalize(summary));
            if earlier.contains(&sig) {
                row.reopened += 1;
            }
            *out.by_status.entry(status.clone()).or_default() += 1;
            if status == "fixed" {
                now_fixed.push(sig);
            }
        }
        earlier.extend(now_fixed);
    }
    out.rows = rows.into_values().collect();
    for r in &out.rows {
        out.rounds += r.rounds;
        out.findings += r.findings;
        out.clean_rounds += r.clean_rounds;
        out.reopened += r.reopened;
    }
    let bad = out.by_status.get("rejected").copied().unwrap_or(0)
        + out.by_status.get("declined").copied().unwrap_or(0);
    let total: u64 = out.by_status.values().sum();
    out.rejected_or_declined_ratio = (total > 0).then(|| bad as f64 / total as f64);
    out.unavailable = vec![
        "reopened is a reading: a finding counts when an earlier round's fixed finding has the same file and text; a finding reworded when it came back is missed (the finding's status history is not kept)"
            .into(),
        "findings without a conversation (its session was deleted) are in by_status but in no round".into(),
    ];
    Ok(out)
}

fn pr_draft(conn: &Connection, f: &Filter) -> Result<PrDraft> {
    let mut rows: BTreeMap<String, PrDraftRow> = BTreeMap::new();
    fn row_for(rows: &mut BTreeMap<String, PrDraftRow>, node: String) -> &mut PrDraftRow {
        rows.entry(node.clone()).or_insert_with(move || PrDraftRow { node, ..Default::default() })
    }
    // The PR agent's sessions.
    let sql = format!(
        "SELECT n.slug, COUNT(*) FROM conversations c JOIN nodes n ON n.id = COALESCE(c.focus_node_id, c.focus_id)
         WHERE c.protocol = 'pr' {} GROUP BY n.slug",
        scope("n.slug", "c.created_at")
    );
    for r in conn.prepare(&sql)?.query_map(params![f.node, f.since_ms], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))? {
        let (node, n) = r?;
        row_for(&mut rows, node).pr_agent_sessions = n;
    }
    // Review rounds on the draft.
    let sql = format!(
        "SELECT n.slug, COUNT(*) FROM conversations c JOIN nodes n ON n.id = COALESCE(c.focus_node_id, c.focus_id)
         WHERE c.protocol = 'review' AND c.skill_group = 0 AND c.from_state = 'pr_draft' {} GROUP BY n.slug",
        scope("n.slug", "c.created_at")
    );
    for r in conn.prepare(&sql)?.query_map(params![f.node, f.since_ms], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))? {
        let (node, n) = r?;
        row_for(&mut rows, node).draft_review_rounds = n;
    }
    // Findings posted as PR threads.
    let sql = format!(
        "SELECT n.slug, COUNT(*) FROM review_findings r JOIN nodes n ON n.id = r.node_id
         WHERE r.pr_posted_at IS NOT NULL {} GROUP BY n.slug",
        scope("n.slug", "r.pr_posted_at")
    );
    for r in conn.prepare(&sql)?.query_map(params![f.node, f.since_ms], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))? {
        let (node, n) = r?;
        row_for(&mut rows, node).findings_posted_as_threads = n;
    }
    // GitHub's own events.
    let sql = format!(
        "SELECT n.slug, e.kind, e.summary FROM node_events e JOIN nodes n ON n.id = e.node_id
         WHERE e.source = 'github' {}",
        scope("n.slug", "e.received_at")
    );
    for r in conn
        .prepare(&sql)?
        .query_map(params![f.node, f.since_ms], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
    {
        let (node, kind, summary) = r?;
        let row = row_for(&mut rows, node);
        *row.github_events.entry(kind.clone()).or_default() += 1;
        if kind == "push" {
            row.pushes += 1;
        }
        if kind.starts_with("check") && summary.to_lowercase().contains("failure") {
            row.ci_failures += 1;
        }
    }
    let rows: Vec<PrDraftRow> = rows.into_values().collect();
    let mut out = PrDraft { rows, ..Default::default() };
    for r in &out.rows {
        out.pr_agent_sessions += r.pr_agent_sessions;
        out.draft_review_rounds += r.draft_review_rounds;
        out.findings_posted_as_threads += r.findings_posted_as_threads;
        out.pushes += r.pushes;
        out.ci_failures += r.ci_failures;
    }
    out.ci_failures_per_push = (out.pushes > 0).then(|| out.ci_failures as f64 / out.pushes as f64);
    out.unavailable = vec![
        "bot review rounds and the bot's findings per round: a review bot's comments live on GitHub and are not stored (only the webhook events that woke the node are, in github_events)"
            .into(),
        "threads answered versus code changed: the answers are posted to GitHub and not recorded here".into(),
        "github_events exist only for nodes in a cloud sandbox, whose orchestrator routes webhooks; a node run locally has none, so pushes and ci_failures read 0 for it"
            .into(),
    ];
    Ok(out)
}

const CARRY_ON_WORDS: [&str; 9] = [
    "carry on",
    "go ahead",
    "proceed",
    "continue",
    "try again",
    "retry",
    "resolved outside",
    "no follow-up",
    "fixed, try",
];

fn requests(conn: &Connection, f: &Filter) -> Result<RequestStats> {
    let sql = format!(
        "SELECT d.id, d.reason, d.status, d.created_at FROM decisions d JOIN nodes n ON n.id = d.node_id
         WHERE 1 = 1 {}",
        scope("n.slug", "d.created_at")
    );
    let decisions: Vec<(Vec<u8>, String, String, i64)> = conn
        .prepare(&sql)?
        .query_map(params![f.node, f.since_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut answer_stmt = conn.prepare(
        "SELECT option_index, text, actor, answered_at FROM decision_answers WHERE decision_id = ?1
         ORDER BY id DESC LIMIT 1",
    )?;
    let now = tod_store::outline::uuid_blob::now_ms();
    let mut out = RequestStats::default();
    let mut waits: Vec<i64> = Vec::new();
    for (id, reason, status, created) in decisions {
        out.total += 1;
        *out.by_reason.entry(reason.clone()).or_default() += 1;
        if reason == "broken" || reason == "access" {
            out.about_setup += 1;
        }
        match status.as_str() {
            "pending" => {
                out.pending += 1;
                out.pending_wait_secs.push(((now - created) / 1000).max(0));
                continue;
            }
            "withdrawn" => {
                out.withdrawn += 1;
                continue;
            }
            _ => out.answered += 1,
        }
        let answer: Option<(Option<i64>, Option<String>, String, i64)> = {
            let mut rows = answer_stmt.query([&id])?;
            match rows.next()? {
                Some(r) => Some((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                None => None,
            }
        };
        let Some((option, text, actor, at)) = answer else { continue };
        waits.push(((at - created) / 1000).max(0));
        if option.is_some() {
            out.answered_with_offered_option += 1;
        } else {
            out.answered_with_text += 1;
        }
        let lowered = text.as_deref().unwrap_or_default().to_lowercase();
        if CARRY_ON_WORDS.iter().any(|w| lowered.contains(w)) {
            out.carry_on_like += 1;
        }
        if actor == "user" {
            out.answered_by_user += 1;
        } else {
            out.answered_by_other += 1;
        }
    }
    waits.sort_unstable();
    out.median_wait_secs = waits.get(waits.len() / 2).copied();
    out.max_wait_secs = waits.last().copied();
    out.pending_wait_secs.sort_unstable_by(|a, b| b.cmp(a));
    out.unavailable = vec![
        "carry_on_like reads the answer's words; an answer by picking an offered option is counted in answered_with_offered_option, and whether that option was 'carry on' is in the option text, which is not read"
            .into(),
        "wait is from the request's creation to its last answer, which includes time the user was not looking".into(),
    ];
    Ok(out)
}

fn stops(conn: &Connection, f: &Filter) -> Result<StopStats> {
    let sql = format!(
        "SELECT s.phase, s.reason_kind, s.resumable_by FROM run_state s JOIN nodes n ON n.id = s.node_id
         WHERE 1 = 1 {}",
        scope("n.slug", "s.updated_at")
    );
    let mut out = StopStats::default();
    for r in conn
        .prepare(&sql)?
        .query_map(params![f.node, f.since_ms], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
    {
        let (phase, reason, by) = r?;
        *out.now_by_phase.entry(phase.clone()).or_default() += 1;
        if phase == "stopped" {
            *out.stopped_by_reason.entry(reason).or_default() += 1;
            *out.stopped_by_resumable_by.entry(by).or_default() += 1;
        }
    }
    let sql = format!(
        "SELECT COUNT(*) FROM journey_changes j JOIN nodes n ON n.id = j.node_id
         WHERE j.tbl = 'run_state' {}",
        scope("n.slug", "CAST(j.at AS INTEGER)")
    );
    out.run_state_changes_journaled = conn.query_row(&sql, params![f.node, f.since_ms], |r| r.get(0))?;
    out.unavailable = vec![
        "stops over time: run_state keeps only each node's current row, so earlier stops and their reasons are gone; stopped_by_reason is the stops standing now"
            .into(),
        "stops resumed with no change by the user: nothing records what the user changed between a stop and its resume (the journal records that run_state changed, not its old or new content); it needs a record of the stop's reason when the run resumes"
            .into(),
    ];
    Ok(out)
}

fn sandbox(conn: &Connection, f: &Filter) -> Result<Sandbox> {
    let mut rows: BTreeMap<String, ChurnRow> = BTreeMap::new();
    fn row_for(rows: &mut BTreeMap<String, ChurnRow>, node: String) -> &mut ChurnRow {
        rows.entry(node.clone()).or_insert_with(move || ChurnRow { node, ..Default::default() })
    }
    let sql = format!(
        "SELECT n.slug, j.op, COUNT(*) FROM journey_changes j JOIN nodes n ON n.id = j.node_id
         WHERE j.tbl = 'node_files_locations' AND j.op IN ('insert', 'delete') {}
         GROUP BY n.slug, j.op",
        scope("n.slug", "CAST(j.at AS INTEGER)")
    );
    for r in conn
        .prepare(&sql)?
        .query_map(params![f.node, f.since_ms], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, u64>(2)?)))?
    {
        let (node, op, n) = r?;
        let row = row_for(&mut rows, node);
        if op == "insert" {
            row.locations_made = n;
        } else {
            row.locations_removed = n;
        }
    }
    let sql = format!(
        "SELECT n.slug FROM cloud_nodes c JOIN nodes n ON n.id = c.node_id WHERE c.lost_at IS NOT NULL {}",
        scope("n.slug", "c.lost_at")
    );
    for r in conn.prepare(&sql)?.query_map(params![f.node, f.since_ms], |r| r.get::<_, String>(0))? {
        row_for(&mut rows, r?).lost_now = true;
    }
    let sql = format!(
        "SELECT n.slug, COUNT(*) FROM node_timeline t JOIN nodes n ON n.id = t.node_id
         WHERE t.kind = 'note' AND (t.title LIKE '%Reset environment%' OR t.title LIKE '%Delete sandbox%') {}
         GROUP BY n.slug",
        scope("n.slug", "t.at")
    );
    for r in conn.prepare(&sql)?.query_map(params![f.node, f.since_ms], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u64>(1)?)))? {
        let (node, n) = r?;
        row_for(&mut rows, node).environment_resets = n;
    }
    let mut out = Sandbox::default();
    for r in rows.values_mut() {
        r.replacements = r.locations_made.saturating_sub(1);
    }
    out.rows = rows.into_values().collect();
    for r in &out.rows {
        out.replacements += r.replacements;
        out.lost_now += r.lost_now as u64;
        out.nodes_with_replacements += (r.replacements > 0) as u64;
    }
    out.unavailable = vec![
        "locations_made counts journal rows, and a node's files may not be a sandbox (a worktree location is counted too); the journal holds only changes since it began, so an older replacement is not counted"
            .into(),
        "time spent setting up a sandbox: nothing records when a sandbox's setup began or ended; the progress is only logged"
            .into(),
        "losses: only a loss standing now (cloud_nodes.lost_at); a loss since recovered leaves no trace".into(),
    ];
    Ok(out)
}

fn git_retries(conn: &Connection, f: &Filter) -> Result<GitRetries> {
    let mut rows: BTreeMap<String, GitRetryRow> = BTreeMap::new();
    let sql = format!(
        "SELECT COALESCE(n.slug, g.workdir), g.op, g.class, g.retries, g.outcome
         FROM git_retries g LEFT JOIN nodes n ON n.id = g.node_id
         WHERE (?1 IS NULL OR n.slug = ?1) AND (?2 IS NULL OR g.at >= ?2)"
    );
    let mut out = GitRetries::default();
    for r in conn.prepare(&sql)?.query_map(params![f.node, f.since_ms], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, u64>(3)?, r.get::<_, String>(4)?))
    })? {
        let (subject, op, class, retries, outcome) = r?;
        let row = rows.entry(subject.clone()).or_insert_with(|| GitRetryRow { subject, ..Default::default() });
        row.calls += 1;
        row.retries += retries;
        *row.by_op.entry(op).or_default() += 1;
        if class == tod_store::git_retries::CLASS_LOCK_RACE {
            row.lock_race += 1;
        } else {
            row.network += 1;
        }
        if outcome == tod_store::git_retries::OUTCOME_GAVE_UP {
            row.gave_up += 1;
        } else {
            row.recovered += 1;
        }
    }
    out.rows = rows.into_values().collect();
    for r in &out.rows {
        out.calls += r.calls;
        out.retries += r.retries;
        out.recovered += r.recovered;
        out.gave_up += r.gave_up;
    }
    out.unavailable = vec![
        "git calls that succeeded at once are not counted, so there is no retry rate, only the calls that needed one".into(),
        "retries of the shell scripts (quick branch script, sandbox checkout, golden refresh) and of a supervisor in a sandbox: the records are this machine's".into(),
        "a retry made outside any node's launch is counted against its workdir, not a node".into(),
    ];
    Ok(out)
}

fn stop_mismatch(conn: &Connection, f: &Filter, root: &std::path::Path) -> Result<StopMismatchStats> {
    let mut out = StopMismatchStats::default();
    for m in crate::stop_mismatch::find(conn, root)? {
        let slug = tod_store::outline::repos::NodeRepo::new(conn)
            .get(m.node_id)?
            .map(|n| n.slug)
            .unwrap_or_else(|| m.node_id.to_string());
        if f.node.as_ref().is_some_and(|only| *only != slug) {
            continue;
        }
        out.rows.push(StopMismatchRow { node: slug, run: m.run, shown: m.shown, lasted_secs: m.lasted_ms / 1000 });
    }
    out.count = out.rows.len() as u64;
    out.longest_secs = out.rows.first().map(|r| r.lasted_secs);
    out.unavailable = vec![
        "a run that stopped and was shown as something else earlier but no longer is: only what stands now is checked, there is no history".into(),
        "--since does not apply: a mismatch is a state, and its age is how long the run has been stopped".into(),
        "a node whose runner wrote no run_state row (a supervisor built before the table) is not checked".into(),
    ];
    Ok(out)
}

// ---- text ----

fn dur(ms: i64) -> String {
    let s = ms / 1000;
    if s >= 3600 { format!("{}h{:02}m", s / 3600, (s % 3600) / 60) } else if s >= 60 { format!("{}m{:02}s", s / 60, s % 60) } else { format!("{s}s") }
}

fn ratio(r: Option<f64>) -> String {
    r.map(|r| format!("{:.0}%", r * 100.0)).unwrap_or_else(|| "n/a".into())
}

/// A row's tokens: `in <uncached> + <cache read> read + <cache write> write,
/// <output> out, <share>% cached`, or `no tokens recorded`.
fn tokens_text(r: &CostRow) -> String {
    match r.cache_read_share() {
        None if r.output_tokens == 0 => "no tokens recorded".to_string(),
        share => format!(
            "in {} + {} read + {} write, {} out, cached {}",
            r.input_tokens,
            r.cache_read_tokens,
            r.cache_write_tokens,
            r.output_tokens,
            share.map_or("-".to_string(), |s| format!("{:.0}%", s * 100.0))
        ),
    }
}

fn notes(out: &mut String, unavailable: &[String]) {
    for u in unavailable {
        out.push_str(&format!("  not available: {u}\n"));
    }
}

impl Stats {
    /// The report as text.
    pub fn text(&self) -> String {
        let mut o = String::new();
        if let Some(c) = &self.cost {
            o.push_str("COST per node and phase (sessions, agent turns, tool calls, wall, tokens)\n");
            o.push_str("  by phase:\n");
            for r in &c.by_phase {
                o.push_str(&format!(
                    "    {:<28} {:>4} sessions {:>5} turns {:>6} tools {:>9} {}\n",
                    r.phase, r.sessions, r.agent_turns, r.tool_calls, dur(r.wall_ms), tokens_text(r)
                ));
            }
            o.push_str("  by node:\n");
            for r in &c.rows {
                o.push_str(&format!(
                    "    {:<48} {:<22} {:>3} s {:>4} t {:>5} tools {:>8} {}\n",
                    r.node, r.phase, r.sessions, r.agent_turns, r.tool_calls, dur(r.wall_ms), tokens_text(r)
                ));
            }
            notes(&mut o, &c.unavailable);
            o.push('\n');
        }
        if let Some(r) = &self.review {
            o.push_str(&format!(
                "REVIEW LOOP: {} rounds, {} findings, {} clean rounds, {} reopened, rejected/declined {}\n  by status: {:?}\n",
                r.rounds, r.findings, r.clean_rounds, r.reopened, ratio(r.rejected_or_declined_ratio), r.by_status
            ));
            for x in &r.rows {
                o.push_str(&format!(
                    "    {:<48} {:<9} rounds {:>2} findings {:>3} clean {:>2} reopened {:>2}\n",
                    x.node, x.stay, x.rounds, x.findings, x.clean_rounds, x.reopened
                ));
            }
            notes(&mut o, &r.unavailable);
            o.push('\n');
        }
        if let Some(p) = &self.pr_draft {
            o.push_str(&format!(
                "PR LOOP: {} PR-agent sessions, {} draft review rounds, {} findings posted as threads, {} pushes, {} CI failures ({} per push)\n",
                p.pr_agent_sessions,
                p.draft_review_rounds,
                p.findings_posted_as_threads,
                p.pushes,
                p.ci_failures,
                p.ci_failures_per_push.map(|v| format!("{v:.2}")).unwrap_or_else(|| "n/a".into())
            ));
            for x in &p.rows {
                o.push_str(&format!(
                    "    {:<48} pr-agent {:>2} draft-review {:>2} threads {:>2} events {:?}\n",
                    x.node, x.pr_agent_sessions, x.draft_review_rounds, x.findings_posted_as_threads, x.github_events
                ));
            }
            notes(&mut o, &p.unavailable);
            o.push('\n');
        }
        if let Some(r) = &self.requests {
            o.push_str(&format!(
                "REQUESTS: {} total, {} pending, {} answered, {} withdrawn\n  by reason: {:?}\n  answered with an offered option {}, with text {}, carry-on-like {}, about setup (broken/access) {}\n  answered by user {}, by others {}; wait median {}, max {}\n  pending waiting (s, oldest first): {:?}\n",
                r.total, r.pending, r.answered, r.withdrawn, r.by_reason, r.answered_with_offered_option,
                r.answered_with_text, r.carry_on_like, r.about_setup, r.answered_by_user, r.answered_by_other,
                r.median_wait_secs.map(|s| dur(s * 1000)).unwrap_or_else(|| "n/a".into()),
                r.max_wait_secs.map(|s| dur(s * 1000)).unwrap_or_else(|| "n/a".into()),
                r.pending_wait_secs
            ));
            notes(&mut o, &r.unavailable);
            o.push('\n');
        }
        if let Some(s) = &self.stops {
            o.push_str(&format!(
                "STOPS (standing now): runs by phase {:?}\n  stopped by reason {:?}, resumable by {:?}\n  run_state changes in the journal: {}\n",
                s.now_by_phase, s.stopped_by_reason, s.stopped_by_resumable_by, s.run_state_changes_journaled
            ));
            notes(&mut o, &s.unavailable);
            o.push('\n');
        }
        if let Some(s) = &self.sandbox {
            o.push_str(&format!(
                "SANDBOX CHURN: {} replacements over {} nodes, {} lost now\n",
                s.replacements, s.nodes_with_replacements, s.lost_now
            ));
            for x in s.rows.iter().filter(|x| x.locations_made > 0 || x.lost_now || x.environment_resets > 0) {
                o.push_str(&format!(
                    "    {:<48} made {:>2} removed {:>2} replaced {:>2} resets {:>2}{}\n",
                    x.node, x.locations_made, x.locations_removed, x.replacements, x.environment_resets,
                    if x.lost_now { " LOST" } else { "" }
                ));
            }
            notes(&mut o, &s.unavailable);
        }
        if let Some(g) = &self.git_retries {
            if !o.is_empty() {
                o.push('\n');
            }
            o.push_str(&format!(
                "GIT RETRIES: {} calls retried ({} retries): {} recovered, {} gave up\n",
                g.calls, g.retries, g.recovered, g.gave_up
            ));
            for x in &g.rows {
                let ops = x.by_op.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ");
                o.push_str(&format!(
                    "    {:<48} calls {:>3} retries {:>3} recovered {:>3} gave up {:>3} lock {:>3} network {:>3} ({ops})\n",
                    x.subject, x.calls, x.retries, x.recovered, x.gave_up, x.lock_race, x.network
                ));
            }
            notes(&mut o, &g.unavailable);
        }
        if let Some(m) = &self.stop_mismatch {
            if !o.is_empty() {
                o.push('\n');
            }
            o.push_str(&format!(
                "STOP MISMATCH: {} runs stopped or failed while the node shows something else, longest {}\n",
                m.count,
                m.longest_secs.map(|s| dur(s * 1000)).unwrap_or_else(|| "n/a".into())
            ));
            for x in &m.rows {
                o.push_str(&format!("    {:<48} {:>9} run {} | shows {}\n", x.node, dur(x.lasted_secs * 1000), x.run, x.shown));
            }
            notes(&mut o, &m.unavailable);
        }
        o.trim_end().to_string()
    }
}

#[cfg(test)]
#[path = "shepherd_stats_tests.rs"]
mod tests;
