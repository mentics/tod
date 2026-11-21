//! Code review findings: what a review conversation's agent found in a node's
//! change, and the response each one gets.
//!
//! A finding belongs to its node, not to the conversation that recorded it:
//! the `review` → `approved` gate asks that every finding on the node has a
//! response, whichever review found it. The agent records findings through
//! `tod-cli review`; the user (or an agent addressing them) responds with a
//! status and a note. Spec: `doc/conversation/protocols.md` §4c.
//!
//! Findings that share a **root cause** (the assumption or pattern that made
//! them possible) carry it in the same words, so the fix covers the class
//! ([`group_by_root_cause`]). What each review round checked, lens by lens, is
//! its [`coverage`].

use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

pub mod coverage;
pub mod round;

pub use coverage::{
    CoverageEntry, CoverageGaps, CoveragePass, CoverageResult, LatestCoverage, Lens, NewCoverage,
    done_report, is_done_report, lens, required_lenses,
};

/// tod's review lenses, in the order a review sweeps them
/// (`doc/lifecycle/find-it-first.md` 6c): the one list the review surface
/// (`surface/review.md`) and the coverage check read.
pub const LENSES: [Lens; 11] = [
    Lens { slug: "obligations", title: "Obligations" },
    Lens { slug: "data-assumptions", title: "Data assumptions" },
    Lens { slug: "external-systems", title: "External systems" },
    Lens { slug: "persisted-results", title: "Persisted results" },
    Lens { slug: "interactions", title: "Interactions" },
    Lens { slug: "reach", title: "Reach" },
    Lens { slug: "data-writes", title: "Data writes" },
    Lens { slug: "concurrency", title: "Concurrency and cancellation" },
    Lens { slug: "error-paths", title: "Error paths" },
    Lens { slug: "tests", title: "Tests" },
    Lens { slug: "verification-evidence", title: "Verification evidence" },
];

/// The lenses the risk reviewer sweeps (find-it-first 6h): where defects
/// that survive review usually live. The main reviewer sweeps all of
/// [`LENSES`], so these are swept twice, by two sessions that do not share
/// their reasoning. Data writes is always here until guidance selection
/// (item 7) can say which tasks write data.
pub const RISK_LENSES: [&str; 6] = [
    "data-assumptions",
    "external-systems",
    "persisted-results",
    "interactions",
    "reach",
    "data-writes",
];

/// Which of a review round's two reviewers a review conversation is
/// (find-it-first 6h): both run at once, in fresh sessions, and record into
/// the same findings list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reviewer {
    /// Sweeps every lens.
    Main,
    /// Sweeps only [`RISK_LENSES`].
    Risk,
}

impl Reviewer {
    /// Main first, as a round starts them.
    pub const ALL: [Reviewer; 2] = [Reviewer::Main, Reviewer::Risk];

    pub fn as_str(self) -> &'static str {
        match self {
            Reviewer::Main => "main",
            Reviewer::Risk => "risk",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "main" => Ok(Reviewer::Main),
            "risk" => Ok(Reviewer::Risk),
            other => bail!("unknown reviewer `{other}` (expected main|risk)"),
        }
    }

    /// How a list names it.
    pub fn title(self) -> &'static str {
        match self {
            Reviewer::Main => "main reviewer",
            Reviewer::Risk => "risk reviewer",
        }
    }

    /// The lens slugs this reviewer must cover before it may record done.
    pub fn lenses(self) -> Vec<&'static str> {
        match self {
            Reviewer::Main => LENSES.iter().map(|l| l.slug).collect(),
            Reviewer::Risk => RISK_LENSES.to_vec(),
        }
    }
}

/// Not yet responded to.
pub const FINDING_OPEN: &str = "open";
/// Fixed; the response points at the change.
pub const FINDING_FIXED: &str = "fixed";
/// Real, but not this node's to fix.
pub const FINDING_OUT_OF_SCOPE: &str = "out_of_scope";
/// Not critical, beyond the requirements, or not worth the cost.
pub const FINDING_DECLINED: &str = "declined";
/// Not a defect after all: the fix agent's pushback, with a note saying why.
pub const FINDING_REJECTED: &str = "rejected";

/// Real but not worth another round, or not worth fixing at all: tod set it
/// aside under the scoring rules (`doc/requests/design.md` 5.1). The response
/// is tod's reason ("nit", "minor after round 2"). It does not count against
/// a review; the user can reopen it, which makes it block like a `major`.
pub const FINDING_LEFT: &str = "left";

/// Every finding status, in the order a status menu lists them.
pub const FINDING_STATUSES: [&str; 6] = [
    FINDING_OPEN,
    FINDING_FIXED,
    FINDING_OUT_OF_SCOPE,
    FINDING_DECLINED,
    FINDING_REJECTED,
    FINDING_LEFT,
];

/// Whether a status stands aside from a review's verdict: the user dismissed
/// the finding (`out_of_scope`, `declined`) or tod left it ([`FINDING_LEFT`]).
/// Every reader that asks whether a review "recorded nothing that stands"
/// asks this, so they cannot drift.
pub fn is_dismissed(status: &str) -> bool {
    matches!(status, FINDING_OUT_OF_SCOPE | FINDING_DECLINED | FINDING_LEFT)
}

/// The statuses the user answers a finding with from its menu. `rejected` is
/// the fix agent's, and needs a note the menu cannot give; a finding already
/// rejected keeps it in its menu.
pub const USER_FINDING_STATUSES: [&str; 4] = [
    FINDING_OPEN,
    FINDING_FIXED,
    FINDING_OUT_OF_SCOPE,
    FINDING_DECLINED,
];

/// The statuses a fix conversation's agent may give a finding: it fixes it,
/// or pushes back with a note. Out of scope and declined are the user's call.
pub const FIX_AGENT_STATUSES: [&str; 2] = [FINDING_FIXED, FINDING_REJECTED];

/// How much a finding matters, most first (`doc/requests/design.md` 5.1):
/// `blocker` is wrong or breaks an obligation or production; `major` should be
/// fixed before merge; `minor` is worth fixing and fine to leave; `nit` is
/// style, wording or taste.
pub const IMPORTANCES: [&str; 4] = ["blocker", "major", "minor", "nit"];
pub const IMPORTANCE_BLOCKER: &str = "blocker";
pub const IMPORTANCE_MAJOR: &str = "major";
pub const IMPORTANCE_MINOR: &str = "minor";
pub const IMPORTANCE_NIT: &str = "nit";

/// How much harm leaving it does: its reach into users, data, production,
/// other code. Most first.
pub const IMPACTS: [&str; 3] = ["high", "medium", "low"];

/// What the fix takes, cheapest first: `mechanical` is a tool or a one-line
/// rule (formatting, length, a rename, a stale comment); `simple` is a local
/// edit with no design; `involved` is design, several files, or tests.
pub const COMPLEXITIES: [&str; 3] = ["mechanical", "simple", "involved"];
pub const COMPLEXITY_MECHANICAL: &str = "mechanical";
pub const COMPLEXITY_SIMPLE: &str = "simple";
pub const COMPLEXITY_INVOLVED: &str = "involved";

/// How the one rule a mechanical finding cites is worded for matching:
/// case, runs of whitespace and surrounding punctuation do not tell two apart
/// (`tod-cli review add --rule`). Three findings with the same key, across
/// nodes, file a suggestion to add a check (`tod_core::review_scoring`).
pub fn rule_key(rule: &str) -> String {
    rule.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFinding {
    pub id: Uuid,
    pub node_id: Uuid,
    /// The review conversation that recorded it, while that conversation
    /// exists.
    pub conversation_id: Option<Uuid>,
    /// Order of recording on the node, from 1.
    pub seq: i64,
    /// `blocker`, `major`, `minor` or `nit` ([`IMPORTANCES`]).
    pub importance: String,
    /// `high`, `medium` or `low` ([`IMPACTS`]).
    pub impact: String,
    /// `mechanical`, `simple` or `involved` ([`COMPLEXITIES`]).
    pub complexity: String,
    /// Repo-relative path, when the finding is about one place.
    pub file: Option<String>,
    pub line: Option<i64>,
    /// The defect, in a sentence.
    pub summary: String,
    /// Why it matters: the inputs or state that go wrong, and how.
    pub detail: Option<String>,
    pub status: String,
    /// The response: a pointer to the fix, or why it is out of scope,
    /// declined, or rejected.
    pub response: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// The pull request review thread the finding was posted as, by a review
    /// in `pr_draft` (`tod_core::pr_draft_review`): its GraphQL id. `None`
    /// when it was not posted, or was posted in the review's text because no
    /// line of the change could hold it.
    pub pr_thread: Option<String>,
    /// When it was posted on the pull request; `None` until then.
    pub pr_posted_at: Option<i64>,
    /// The assumption or pattern that made it possible, in the same words
    /// for every finding that shares it (`tod-cli review add --root-cause`).
    pub root_cause: Option<String>,
    /// Which of the round's reviewers recorded it (schema v105); `None` for
    /// a finding from before there were two, or recorded outside a review
    /// conversation (a check, the user).
    pub reviewer: Option<Reviewer>,
    /// The one rule a mechanical finding cites, in the same words each time
    /// (`tod-cli review add --rule`); `None` for a finding that is not about a
    /// rule. Three of the same rule file a suggestion to add a check.
    pub rule: Option<String>,
    /// The quick fix (`node_quick_fixes.id`) that fixed it and whose commit the
    /// small-change review found clean: the finding is settled and needs no
    /// round of its own (`doc/requests/design.md` 5.1). `None` otherwise.
    pub quick_fix: Option<Uuid>,
}

impl ReviewFinding {
    /// Whether it stands aside from a review's verdict ([`is_dismissed`]).
    pub fn dismissed(&self) -> bool {
        is_dismissed(&self.status)
    }

    /// Whether a review that is otherwise done still counts it against the
    /// review: not dismissed, and not a fix a quick fix made that the small
    /// review cleared. A fix by the Fix agent still counts: only another
    /// review can say the change is right.
    pub fn stands(&self) -> bool {
        !self.dismissed() && !(self.status == FINDING_FIXED && self.quick_fix.is_some())
    }

    /// `file:line`, `file`, or nothing.
    pub fn location(&self) -> Option<String> {
        let file = self.file.as_deref()?;
        Some(match self.line {
            Some(line) => format!("{file}:{line}"),
            None => file.to_string(),
        })
    }

    pub fn is_open(&self) -> bool {
        self.status == FINDING_OPEN
    }
}

/// What an agent submits.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NewFinding {
    pub importance: String,
    pub impact: String,
    pub complexity: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line: Option<i64>,
    pub summary: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub root_cause: Option<String>,
    /// The rule a mechanical finding cites ([`ReviewFinding::rule`]).
    #[serde(default)]
    pub rule: Option<String>,
}

impl NewFinding {
    /// A finding scored `importance`, `impact`, `complexity`, with `summary`.
    pub fn scored(importance: &str, impact: &str, complexity: &str, summary: &str) -> Self {
        Self {
            importance: importance.to_string(),
            impact: impact.to_string(),
            complexity: complexity.to_string(),
            summary: summary.to_string(),
            ..Self::default()
        }
    }
}

/// The table as it was before scoring (schema v44 to v116), which the
/// migrations that built it still make; v117 rebuilds it as [`CREATE_TABLE`].
pub const CREATE_TABLE_V116: &str = "
    CREATE TABLE IF NOT EXISTS review_findings (
        id              BLOB PRIMARY KEY NOT NULL,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        conversation_id BLOB REFERENCES conversations(id) ON DELETE SET NULL,
        seq             INTEGER NOT NULL,
        severity        TEXT NOT NULL CHECK (severity IN ('high','medium','low')),
        file            TEXT,
        line            INTEGER,
        summary         TEXT NOT NULL,
        detail          TEXT,
        status          TEXT NOT NULL DEFAULT 'open'
                            CHECK (status IN ('open','fixed','out_of_scope','declined','rejected')),
        response        TEXT,
        created_at      INTEGER NOT NULL,
        updated_at      INTEGER NOT NULL,
        pr_thread       TEXT,
        pr_posted_at    INTEGER,
        root_cause      TEXT,
        reviewer        TEXT,
        UNIQUE (node_id, seq)
    );
    CREATE INDEX IF NOT EXISTS idx_review_findings_node ON review_findings(node_id, seq);
";

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS review_findings (
        id              BLOB PRIMARY KEY NOT NULL,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        conversation_id BLOB REFERENCES conversations(id) ON DELETE SET NULL,
        seq             INTEGER NOT NULL,
        importance      TEXT NOT NULL DEFAULT 'major'
                            CHECK (importance IN ('blocker','major','minor','nit')),
        impact          TEXT NOT NULL DEFAULT 'medium'
                            CHECK (impact IN ('high','medium','low')),
        complexity      TEXT NOT NULL DEFAULT 'simple'
                            CHECK (complexity IN ('mechanical','simple','involved')),
        file            TEXT,
        line            INTEGER,
        summary         TEXT NOT NULL,
        detail          TEXT,
        status          TEXT NOT NULL DEFAULT 'open'
                            CHECK (status IN ('open','fixed','out_of_scope','declined','rejected','left')),
        response        TEXT,
        created_at      INTEGER NOT NULL,
        updated_at      INTEGER NOT NULL,
        pr_thread       TEXT,
        pr_posted_at    INTEGER,
        root_cause      TEXT,
        reviewer        TEXT,
        rule            TEXT,
        quick_fix       BLOB,
        UNIQUE (node_id, seq)
    );
    CREATE INDEX IF NOT EXISTS idx_review_findings_node ON review_findings(node_id, seq);
";

/// Schema v117: findings are scored (`importance`, `impact`, `complexity`)
/// instead of carrying a `severity` (`high` becomes blocker/high, `medium`
/// major/medium, `low` minor/low; complexity `simple`), may be `left`, and may
/// cite a `rule` and the `quick_fix` that fixed them. SQLite cannot drop a
/// column's CHECK, so the table is rebuilt (nothing references it); the
/// journey and sync triggers are made again by the caller. Idempotent: a
/// table with no `severity` column is left alone.
pub fn migrate_to_scored(conn: &Connection) -> Result<()> {
    let has_severity = conn
        .prepare("SELECT 1 FROM pragma_table_info('review_findings') WHERE name = 'severity'")?
        .exists([])?;
    if !has_severity {
        conn.execute_batch(CREATE_TABLE)?;
        return Ok(());
    }
    conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch("PRAGMA legacy_alter_table = ON;")?;
    tx.execute_batch(
        "DROP INDEX IF EXISTS idx_review_findings_node;
         ALTER TABLE review_findings RENAME TO review_findings_v116;",
    )?;
    tx.execute_batch("PRAGMA legacy_alter_table = OFF;")?;
    tx.execute_batch(CREATE_TABLE)?;
    tx.execute_batch(
        "INSERT INTO review_findings
             (id, node_id, conversation_id, seq, importance, impact, complexity, file, line,
              summary, detail, status, response, created_at, updated_at, pr_thread,
              pr_posted_at, root_cause, reviewer)
         SELECT id, node_id, conversation_id, seq,
                CASE severity WHEN 'high' THEN 'blocker' WHEN 'medium' THEN 'major' ELSE 'minor' END,
                severity, 'simple', file, line,
                summary, detail, status, response, created_at, updated_at, pr_thread,
                pr_posted_at, root_cause, reviewer
         FROM review_findings_v116;
         DROP TABLE review_findings_v116;",
    )?;
    tx.commit()?;
    conn.execute_batch("PRAGMA foreign_keys=ON;")?;
    // The rebuilt table lost its journey triggers (the sync ones are remade
    // by `crate::sync::install`).
    conn.execute_batch(&crate::journey_changes::create_triggers_sql())?;
    Ok(())
}

const COLUMNS: &str = "id, node_id, conversation_id, seq, importance, impact, complexity, file, line, \
     summary, detail, status, response, created_at, updated_at, pr_thread, pr_posted_at, root_cause, \
     reviewer, rule, quick_fix";

/// The `pr_thread` and `pr_posted_at` columns (idempotent, no schema bump: the
/// sync copies only the columns both sides have).
pub fn add_pr_columns(conn: &Connection) -> Result<()> {
    for (column, kind) in [("pr_thread", "TEXT"), ("pr_posted_at", "INTEGER")] {
        let has = conn
            .prepare("SELECT 1 FROM pragma_table_info('review_findings') WHERE name = ?1")?
            .exists(params![column])?;
        if !has {
            conn.execute_batch(&format!("ALTER TABLE review_findings ADD COLUMN {column} {kind}"))?;
        }
    }
    Ok(())
}

/// The `root_cause` column (schema v99; idempotent, like [`add_pr_columns`]).
pub fn add_root_cause_column(conn: &Connection) -> Result<()> {
    let has = conn
        .prepare("SELECT 1 FROM pragma_table_info('review_findings') WHERE name = 'root_cause'")?
        .exists([])?;
    if !has {
        conn.execute_batch("ALTER TABLE review_findings ADD COLUMN root_cause TEXT")?;
    }
    Ok(())
}

/// Schema v105 (idempotent): which reviewer recorded a finding
/// (`review_findings.reviewer`), and on a review conversation which reviewer
/// it is (`conversations.reviewer`) and, for the risk reviewer, the main
/// reviewer's conversation it shares the round with
/// (`conversations.review_round`; [`round`]).
pub fn add_reviewer_columns(conn: &Connection) -> Result<()> {
    for (table, column, kind) in [
        ("review_findings", "reviewer", "TEXT"),
        ("conversations", "reviewer", "TEXT"),
        ("conversations", "review_round", "BLOB"),
    ] {
        let has = conn
            .prepare(&format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"))?
            .exists(params![column])?;
        if !has {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))?;
        }
    }
    Ok(())
}

/// A root cause as findings are matched on it: case, and runs of whitespace,
/// do not tell two apart.
pub fn root_cause_key(cause: &str) -> String {
    cause.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Findings that share a root cause, or one finding without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingGroup<'a> {
    /// The cause as the group's first finding words it.
    pub root_cause: Option<&'a str>,
    pub findings: Vec<&'a ReviewFinding>,
}

/// `findings` grouped by root cause, in the order each group's first finding
/// comes; a finding with no root cause is a group of its own. A fixer sees
/// the class, not only its instances.
pub fn group_by_root_cause(findings: &[ReviewFinding]) -> Vec<FindingGroup<'_>> {
    let mut groups: Vec<(Option<String>, FindingGroup<'_>)> = Vec::new();
    for finding in findings {
        let cause = finding
            .root_cause
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty());
        let key = cause.map(root_cause_key);
        if let Some(key) = &key
            && let Some((_, group)) = groups.iter_mut().find(|(k, _)| k.as_ref() == Some(key))
        {
            group.findings.push(finding);
            continue;
        }
        groups.push((key, FindingGroup { root_cause: cause, findings: vec![finding] }));
    }
    groups.into_iter().map(|(_, group)| group).collect()
}

/// `raw` as one of `allowed`, case-insensitively.
pub fn normalize<'s>(raw: &str, allowed: &[&'s str], what: &str) -> Result<&'s str> {
    let wanted = raw.trim().to_ascii_lowercase().replace('-', "_");
    allowed
        .iter()
        .find(|s| **s == wanted)
        .copied()
        .with_context(|| format!("unknown {what} `{raw}` (expected {})", allowed.join("|")))
}

pub struct ReviewRepo<'a> {
    conn: &'a Connection,
}

impl<'a> ReviewRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Record an open finding on `node_id`.
    pub fn add(
        &self,
        node_id: Uuid,
        conversation_id: Option<Uuid>,
        finding: &NewFinding,
    ) -> Result<ReviewFinding> {
        let importance = normalize(&finding.importance, &IMPORTANCES, "importance")?;
        let impact = normalize(&finding.impact, &IMPACTS, "impact")?;
        let complexity = normalize(&finding.complexity, &COMPLEXITIES, "complexity")?;
        let summary = finding.summary.trim();
        if summary.is_empty() {
            bail!("a finding needs a summary");
        }
        let file = finding
            .file
            .as_deref()
            .map(str::trim)
            .filter(|f| !f.is_empty());
        if finding.line.is_some() && file.is_none() {
            bail!("a line needs a file");
        }
        let detail = finding
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        let root_cause = finding
            .root_cause
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty());
        let rule = finding
            .rule
            .as_deref()
            .map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|r| !r.is_empty());
        let seq: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM review_findings WHERE node_id = ?1",
            params![uuid_to_blob(node_id)],
            |row| row.get(0),
        )?;
        let id = Uuid::new_v4();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO review_findings
             (id, node_id, conversation_id, seq, importance, impact, complexity, file, line,
              summary, detail, status, created_at, updated_at, root_cause, reviewer, rule)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'open', ?12, ?12, ?13,
                     (SELECT reviewer FROM conversations WHERE id = ?3), ?14)",
            params![
                uuid_to_blob(id),
                uuid_to_blob(node_id),
                conversation_id.map(uuid_to_blob),
                seq,
                importance,
                impact,
                complexity,
                file,
                finding.line,
                summary,
                detail,
                now,
                root_cause,
                rule,
            ],
        )?;
        self.get(id)?.context("finding vanished after insert")
    }

    /// Set a finding's status and response; `open` clears the response.
    /// The user answers from a status menu, so a response is optional here —
    /// `tod-cli review respond` asks an agent for one. `rejected` always needs
    /// one: a pushback nobody explained is not an answer.
    pub fn respond(&self, id: Uuid, status: &str, response: Option<&str>) -> Result<()> {
        let status = normalize(status, &FINDING_STATUSES, "status")?;
        let response = response
            .map(str::trim)
            .filter(|r| !r.is_empty() && status != FINDING_OPEN);
        if status == FINDING_REJECTED && response.is_none() {
            bail!("rejecting a finding needs a note saying why it is not a problem");
        }
        if status == FINDING_LEFT && response.is_none() {
            bail!("leaving a finding needs the reason it is left");
        }
        let was = self.get(id)?.with_context(|| format!("finding {id} not found"))?;
        // Reopening what tod left is the user saying it matters: it then
        // blocks like a `major` (a nit or minor is promoted; a blocker stays).
        let promote = status == FINDING_OPEN
            && was.status == FINDING_LEFT
            && matches!(was.importance.as_str(), IMPORTANCE_MINOR | IMPORTANCE_NIT);
        let changed = self.conn.execute(
            "UPDATE review_findings SET status = ?2, response = ?3, updated_at = ?4,
                 importance = CASE WHEN ?5 THEN 'major' ELSE importance END,
                 quick_fix = CASE WHEN ?2 = 'open' THEN NULL ELSE quick_fix END
             WHERE id = ?1",
            params![uuid_to_blob(id), status, response, now_ms(), promote],
        )?;
        if changed == 0 {
            bail!("finding {id} not found");
        }
        Ok(())
    }

    /// Change a finding's scores (the user's edit in the findings panel);
    /// `None` leaves a score as it is. Each is checked like a new finding's.
    pub fn rescore(
        &self,
        id: Uuid,
        importance: Option<&str>,
        impact: Option<&str>,
        complexity: Option<&str>,
    ) -> Result<()> {
        let was = self.get(id)?.with_context(|| format!("finding {id} not found"))?;
        let importance = match importance {
            Some(v) => normalize(v, &IMPORTANCES, "importance")?.to_string(),
            None => was.importance,
        };
        let impact = match impact {
            Some(v) => normalize(v, &IMPACTS, "impact")?.to_string(),
            None => was.impact,
        };
        let complexity = match complexity {
            Some(v) => normalize(v, &COMPLEXITIES, "complexity")?.to_string(),
            None => was.complexity,
        };
        self.conn.execute(
            "UPDATE review_findings SET importance = ?2, impact = ?3, complexity = ?4, updated_at = ?5
             WHERE id = ?1",
            params![uuid_to_blob(id), importance, impact, complexity, now_ms()],
        )?;
        Ok(())
    }

    /// Mark a finding fixed by quick fix `quick_fix`, whose commit the
    /// small-change review found clean: `response` points at the commit.
    pub fn settle_by_quick_fix(&self, id: Uuid, quick_fix: Uuid, response: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE review_findings SET status = 'fixed', response = ?2, quick_fix = ?3, updated_at = ?4
             WHERE id = ?1",
            params![uuid_to_blob(id), response.trim(), uuid_to_blob(quick_fix), now_ms()],
        )?;
        if changed == 0 {
            bail!("finding {id} not found");
        }
        Ok(())
    }

    /// Record that the finding was posted on the pull request: as the review
    /// thread `thread`, or (`None`) in the review's text.
    pub fn set_pr_posted(&self, id: Uuid, thread: Option<&str>) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE review_findings SET pr_thread = ?2, pr_posted_at = ?3, updated_at = ?3 WHERE id = ?1",
            params![uuid_to_blob(id), thread, now_ms()],
        )?;
        if changed == 0 {
            bail!("finding {id} not found");
        }
        Ok(())
    }

    /// The node's finding posted as review thread `thread`, if any.
    pub fn find_by_pr_thread(&self, node_id: Uuid, thread: &str) -> Result<Option<ReviewFinding>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM review_findings WHERE node_id = ?1 AND pr_thread = ?2"),
                params![uuid_to_blob(node_id), thread],
                map_row,
            )
            .optional()?)
    }

    pub fn get(&self, id: Uuid) -> Result<Option<ReviewFinding>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLUMNS} FROM review_findings WHERE id = ?1"),
                params![uuid_to_blob(id)],
                map_row,
            )
            .optional()?)
    }

    /// The node's findings, in the order they were recorded.
    pub fn list_for_node(&self, node_id: Uuid) -> Result<Vec<ReviewFinding>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM review_findings WHERE node_id = ?1 ORDER BY seq"
        ))?;
        let rows = stmt
            .query_map(params![uuid_to_blob(node_id)], map_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The rule every `mechanical` finding on any node cites, with the node
    /// and the finding: what `tod_core::review_scoring` matches (by
    /// [`rule_key`]) to suggest a check.
    pub fn mechanical_rules(&self) -> Result<Vec<(Uuid, Uuid, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT node_id, id, rule FROM review_findings
             WHERE complexity = 'mechanical' AND rule IS NOT NULL AND rule <> '' ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let node: Vec<u8> = r.get(0)?;
                let id: Vec<u8> = r.get(1)?;
                Ok((blob_to_uuid_sql(&node)?, blob_to_uuid_sql(&id)?, r.get::<_, String>(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Open findings across every node in `node_ids`, oldest first, in one
    /// query — for a list view over many nodes at once (e.g.
    /// `tod_core::attention`).
    pub fn list_open_for_nodes(&self, node_ids: &[Uuid]) -> Result<Vec<ReviewFinding>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = node_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT {COLUMNS} FROM review_findings
             WHERE status = 'open' AND node_id IN ({placeholders})
             ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let params: Vec<Vec<u8>> = node_ids.iter().copied().map(uuid_to_blob).collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), map_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// A finding from its full id or the 8-character prefix listings show.
    pub fn resolve(&self, raw: &str) -> Result<Uuid> {
        let raw = raw.trim();
        if let Ok(id) = Uuid::parse_str(raw) {
            return Ok(id);
        }
        let prefix = raw.to_ascii_lowercase();
        if prefix.len() < 4 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("`{raw}` is not a finding id");
        }
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM review_findings WHERE lower(hex(id)) LIKE ?1 || '%'")?;
        let ids = stmt
            .query_map(params![prefix], |row| {
                blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        match ids.as_slice() {
            [id] => Ok(*id),
            [] => bail!("no finding matches `{raw}`"),
            _ => bail!(
                "`{raw}` matches {} findings; give more of the id",
                ids.len()
            ),
        }
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReviewFinding> {
    let conversation_id: Option<Vec<u8>> = row.get(2)?;
    Ok(ReviewFinding {
        id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?,
        node_id: blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?,
        conversation_id: conversation_id
            .as_deref()
            .map(blob_to_uuid_sql)
            .transpose()?,
        seq: row.get(3)?,
        importance: row.get(4)?,
        impact: row.get(5)?,
        complexity: row.get(6)?,
        file: row.get(7)?,
        line: row.get(8)?,
        summary: row.get(9)?,
        detail: row.get(10)?,
        status: row.get(11)?,
        response: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
        pr_thread: row.get(15)?,
        pr_posted_at: row.get(16)?,
        root_cause: row.get(17)?,
        reviewer: row
            .get::<_, Option<String>>(18)?
            .map(|r| Reviewer::parse(&r))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(18, rusqlite::types::Type::Text, e.into())
            })?,
        rule: row.get(19)?,
        quick_fix: row
            .get::<_, Option<Vec<u8>>>(20)?
            .as_deref()
            .map(blob_to_uuid_sql)
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{ConversationRepo, Focus, ProtocolKind};
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;

    struct Fx {
        dir: std::path::PathBuf,
        conn: Connection,
        node: Uuid,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn setup() -> Fx {
        let dir = tod_test_util::scratch_dir(format!("tod-review-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn)
            .create_with_id(node, "reviewed", "Reviewed")
            .unwrap();
        Fx { dir, conn, node }
    }

    fn finding(summary: &str) -> NewFinding {
        NewFinding {
            importance: "Blocker".into(),
            impact: "High".into(),
            complexity: "Involved".into(),
            file: Some("src/lib.rs".into()),
            line: Some(12),
            summary: summary.into(),
            detail: Some("Empty input panics.".into()),
            root_cause: None,
            rule: None,
        }
    }

    #[test]
    fn findings_are_numbered_per_node_and_listed_in_order() {
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let first = repo.add(fx.node, None, &finding("First")).unwrap();
        let second = repo.add(fx.node, None, &finding("Second")).unwrap();
        assert_eq!((first.seq, second.seq), (1, 2));
        assert_eq!(
            (first.importance.as_str(), first.impact.as_str(), first.complexity.as_str()),
            ("blocker", "high", "involved")
        );
        assert_eq!(first.status, FINDING_OPEN);
        assert_eq!(first.location().as_deref(), Some("src/lib.rs:12"));
        let listed: Vec<_> = repo
            .list_for_node(fx.node)
            .unwrap()
            .into_iter()
            .map(|f| f.summary)
            .collect();
        assert_eq!(listed, ["First", "Second"]);
        let prefix = &first.id.simple().to_string()[..8];
        assert_eq!(repo.resolve(prefix).unwrap(), first.id);
    }

    #[test]
    fn a_finding_needs_a_summary_known_scores_and_a_file_for_its_line() {
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let mut bad = finding(" ");
        assert!(repo.add(fx.node, None, &bad).is_err());
        bad = finding("x");
        bad.importance = "critical".into();
        assert!(repo.add(fx.node, None, &bad).is_err());
        bad = finding("x");
        bad.impact = String::new();
        assert!(repo.add(fx.node, None, &bad).is_err(), "every score is required");
        bad = finding("x");
        bad.complexity = "hard".into();
        assert!(repo.add(fx.node, None, &bad).is_err());
        bad = finding("x");
        bad.file = None;
        assert!(repo.add(fx.node, None, &bad).is_err());
    }

    /// A response goes with any answer but `open`; reopening clears it.
    #[test]
    fn reopening_a_finding_clears_its_response() {
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let id = repo.add(fx.node, None, &finding("x")).unwrap().id;
        assert!(repo.respond(id, "wontfix", None).is_err());
        repo.respond(id, "out-of-scope", Some("Belongs to the parser node"))
            .unwrap();
        let got = repo.get(id).unwrap().unwrap();
        assert_eq!(got.status, FINDING_OUT_OF_SCOPE);
        assert_eq!(got.response.as_deref(), Some("Belongs to the parser node"));
        repo.respond(id, "open", Some("ignored")).unwrap();
        let got = repo.get(id).unwrap().unwrap();
        assert!(got.is_open());
        assert_eq!(got.response, None);
    }

    /// `left` is tod's own answer and needs its reason; the user reopening one
    /// makes it block like a `major` (a blocker stays a blocker), and editing
    /// the scores checks them like a new finding's.
    #[test]
    fn a_left_finding_has_a_reason_and_reopening_it_promotes_it() {
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let scored = |imp: &str| NewFinding { importance: imp.into(), ..finding(imp) };
        let nit = repo.add(fx.node, None, &scored("nit")).unwrap().id;
        let blocker = repo.add(fx.node, None, &scored("blocker")).unwrap().id;
        assert!(repo.respond(nit, FINDING_LEFT, None).is_err(), "a left finding needs a reason");
        repo.respond(nit, FINDING_LEFT, Some("nit")).unwrap();
        repo.respond(blocker, FINDING_LEFT, Some("by hand")).unwrap();
        let got = repo.get(nit).unwrap().unwrap();
        assert_eq!((got.status.as_str(), got.response.as_deref()), (FINDING_LEFT, Some("nit")));
        assert!(got.dismissed() && !got.stands());

        repo.respond(nit, FINDING_OPEN, None).unwrap();
        repo.respond(blocker, FINDING_OPEN, None).unwrap();
        let (nit, blocker) = (repo.get(nit).unwrap().unwrap(), repo.get(blocker).unwrap().unwrap());
        assert!(nit.is_open() && nit.stands());
        assert_eq!(nit.importance, IMPORTANCE_MAJOR, "reopened, it blocks like a major");
        assert_eq!(blocker.importance, IMPORTANCE_BLOCKER);

        repo.rescore(nit.id, Some("Minor"), None, Some("mechanical")).unwrap();
        let got = repo.get(nit.id).unwrap().unwrap();
        assert_eq!(
            (got.importance.as_str(), got.impact.as_str(), got.complexity.as_str()),
            ("minor", "high", "mechanical")
        );
        assert!(repo.rescore(nit.id, Some("huge"), None, None).is_err());
    }

    /// A fix a quick fix made and the small review cleared stands aside from
    /// the review's verdict; the Fix agent's own `fixed` does not.
    #[test]
    fn a_quick_fixed_finding_does_not_stand_but_a_fixers_does() {
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let a = repo.add(fx.node, None, &finding("a")).unwrap().id;
        let b = repo.add(fx.node, None, &finding("b")).unwrap().id;
        repo.respond(a, FINDING_FIXED, Some("abc")).unwrap();
        repo.settle_by_quick_fix(b, Uuid::new_v4(), "Quick fix abc123, small review clean").unwrap();
        assert!(repo.get(a).unwrap().unwrap().stands());
        let b = repo.get(b).unwrap().unwrap();
        assert!(!b.stands() && b.status == FINDING_FIXED && b.quick_fix.is_some());
        // Reopening it forgets the quick fix.
        repo.respond(b.id, FINDING_OPEN, None).unwrap();
        assert_eq!(repo.get(b.id).unwrap().unwrap().quick_fix, None);
    }

    #[test]
    fn a_rule_is_matched_without_case_spacing_or_trailing_punctuation() {
        assert_eq!(rule_key("  Docstrings  are at most\n three lines. "), rule_key("docstrings are at most three lines"));
        assert_ne!(rule_key("one rule"), rule_key("another rule"));
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let f = repo
            .add(fx.node, None, &NewFinding { rule: Some("  Docstrings   at most 3 lines ".into()), ..finding("x") })
            .unwrap();
        assert_eq!(f.rule.as_deref(), Some("Docstrings at most 3 lines"));
        assert_eq!(repo.add(fx.node, None, &NewFinding { rule: Some("  ".into()), ..finding("y") }).unwrap().rule, None);
    }

    #[test]
    fn a_rejection_needs_a_note() {
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let id = repo.add(fx.node, None, &finding("x")).unwrap().id;
        assert!(repo.respond(id, "rejected", None).is_err());
        assert!(repo.respond(id, "rejected", Some("  ")).is_err());
        repo.respond(id, "rejected", Some("Input is validated upstream"))
            .unwrap();
        let got = repo.get(id).unwrap().unwrap();
        assert_eq!(got.status, FINDING_REJECTED);
        assert!(!got.is_open());
    }

    #[test]
    fn a_posted_finding_is_found_by_its_thread() {
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let a = repo.add(fx.node, None, &finding("a")).unwrap().id;
        let b = repo.add(fx.node, None, &finding("b")).unwrap().id;
        assert_eq!(repo.get(a).unwrap().unwrap().pr_posted_at, None);
        repo.set_pr_posted(a, Some("PRRT_1")).unwrap();
        repo.set_pr_posted(b, None).unwrap();
        assert_eq!(repo.find_by_pr_thread(fx.node, "PRRT_1").unwrap().map(|f| f.id), Some(a));
        let b = repo.get(b).unwrap().unwrap();
        assert!(b.pr_posted_at.is_some() && b.pr_thread.is_none());
    }

    #[test]
    fn findings_are_grouped_by_their_root_cause() {
        let fx = setup();
        let repo = ReviewRepo::new(&fx.conn);
        let with_cause = |summary: &str, cause: Option<&str>| NewFinding {
            root_cause: cause.map(str::to_string),
            ..finding(summary)
        };
        repo.add(fx.node, None, &with_cause("a", Some("  The id is treated as unique ")))
            .unwrap();
        repo.add(fx.node, None, &with_cause("b", None)).unwrap();
        repo.add(fx.node, None, &with_cause("c", Some("the id is treated  as UNIQUE")))
            .unwrap();
        repo.add(fx.node, None, &with_cause("d", Some(" "))).unwrap();
        let findings = repo.list_for_node(fx.node).unwrap();
        assert_eq!(findings[0].root_cause.as_deref(), Some("The id is treated as unique"));
        assert_eq!(findings[3].root_cause, None);
        let groups: Vec<(Option<&str>, Vec<&str>)> = group_by_root_cause(&findings)
            .into_iter()
            .map(|g| (g.root_cause, g.findings.iter().map(|f| f.summary.as_str()).collect()))
            .collect();
        assert_eq!(
            groups,
            [
                (Some("The id is treated as unique"), vec!["a", "c"]),
                (None, vec!["b"]),
                (None, vec!["d"]),
            ]
        );
    }

    /// Findings belong to the node: deleting the conversation that recorded
    /// them keeps them.
    #[test]
    fn findings_outlive_their_conversation() {
        let fx = setup();
        let conversation = ConversationRepo::new(&fx.conn)
            .create(Focus::Node(fx.node), ProtocolKind::Review, None, None, None)
            .unwrap()
            .id;
        let repo = ReviewRepo::new(&fx.conn);
        repo.add(fx.node, Some(conversation), &finding("x"))
            .unwrap();
        fx.conn
            .execute(
                "DELETE FROM conversations WHERE id = ?1",
                params![uuid_to_blob(conversation)],
            )
            .unwrap();
        let findings = repo.list_for_node(fx.node).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].conversation_id, None);
    }
}
