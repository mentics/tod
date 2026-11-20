//! A review's coverage: for each lens, what the reviewer checked in which
//! files, what it found (or why the lens does not apply), and which of the
//! project's files it read for it; and the saturation pass that ends the
//! review (`doc/lifecycle/find-it-first.md` 6c, 6e, 6g).
//!
//! One review round is one review conversation, so the record is keyed by the
//! conversation, and `review done` is refused ([`record_done`]) until every
//! lens the conversation must sweep ([`required_lenses`]) has an entry and a
//! saturation pass that found nothing was recorded after the last finding the
//! conversation added. That makes a skipped step visible instead of silent.
//!
//! Entries are append-only: a later entry for the same lens adds to the record
//! rather than replacing it.

use super::LENSES;
use crate::conversation::{ConversationRepo, Focus, ProtocolKind};
use crate::outline::uuid_blob::{blob_to_uuid_sql, now_ms, uuid_to_blob};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// One of tod's review lenses: a kind of failure every review sweeps the
/// change for, the same for every project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lens {
    /// What the coverage record and `tod-cli review coverage --lens` call it.
    pub slug: &'static str,
    /// What the user reads.
    pub title: &'static str,
}

/// The lens `slug` names, if any.
pub fn lens(slug: &str) -> Option<&'static Lens> {
    let wanted = slug.trim().to_ascii_lowercase().replace('_', "-");
    LENSES.iter().find(|l| l.slug == wanted)
}

/// The lenses review conversation `conversation_id` must record coverage for
/// before it can record the review done: every lens.
///
/// This is the one place that decides it. A round run by more than one
/// reviewer, each with its own assignment (find-it-first 6h: a main review of
/// every lens, a risk review of a few), answers here from the conversation,
/// and every check and message follows.
pub fn required_lenses(_conn: &Connection, _conversation_id: Uuid) -> Result<Vec<&'static Lens>> {
    Ok(LENSES.iter().collect())
}

/// Which pass an entry records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoveragePass {
    /// The sweep through one lens (its slug).
    Lens(String),
    /// The pass after the sweep, made on the premise that defects were
    /// missed.
    Saturation,
}

impl CoveragePass {
    /// The lens's title, or "Saturation pass".
    pub fn title(&self) -> String {
        match self {
            CoveragePass::Lens(slug) => lens(slug).map_or_else(|| slug.clone(), |l| l.title.to_string()),
            CoveragePass::Saturation => "Saturation pass".to_string(),
        }
    }
}

/// What a pass found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageResult {
    /// Checked, and nothing wrong.
    None,
    /// The findings it recorded (or found already recorded) on the node.
    Findings(Vec<Uuid>),
    /// The lens does not apply to this change, and why.
    NotApplicable(String),
}

const RESULT_NONE: &str = "none";
const RESULT_FINDINGS: &str = "findings";
const RESULT_NOT_APPLICABLE: &str = "n/a";

/// What an agent submits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewCoverage {
    pub pass: CoveragePass,
    /// The changed files (repo-relative) this entry covers.
    #[serde(default)]
    pub files: Vec<String>,
    /// What was checked, and how.
    #[serde(default)]
    pub checked: Option<String>,
    pub result: CoverageResult,
    /// The project's files read for it: its review guidance, the skills it
    /// routes to, the code a lens needed (6g).
    #[serde(default)]
    pub read: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageEntry {
    pub id: Uuid,
    pub node_id: Uuid,
    pub conversation_id: Uuid,
    pub pass: CoveragePass,
    pub files: Vec<String>,
    pub checked: Option<String>,
    pub result: CoverageResult,
    pub read: Vec<String>,
    /// How many findings the conversation had recorded when this entry was:
    /// a saturation pass counts only if none was recorded after it.
    pub findings_before: i64,
    pub created_at: i64,
}

impl CoverageEntry {
    /// The result in a few words: `none`, `n/a: <why>`, or `2 findings`.
    pub fn result_label(&self) -> String {
        match &self.result {
            CoverageResult::None => "nothing found".to_string(),
            CoverageResult::Findings(ids) if ids.len() == 1 => "1 finding".to_string(),
            CoverageResult::Findings(ids) => format!("{} findings", ids.len()),
            CoverageResult::NotApplicable(why) => format!("n/a: {why}"),
        }
    }
}

pub const CREATE_TABLE: &str = "
    CREATE TABLE IF NOT EXISTS review_coverage (
        id              BLOB PRIMARY KEY NOT NULL,
        node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
        conversation_id BLOB NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        pass            TEXT NOT NULL CHECK (pass IN ('lens','saturation')),
        lens            TEXT,
        files           TEXT NOT NULL DEFAULT '[]',
        checked         TEXT,
        result          TEXT NOT NULL CHECK (result IN ('none','findings','n/a')),
        finding_ids     TEXT NOT NULL DEFAULT '[]',
        reason          TEXT,
        read_files      TEXT NOT NULL DEFAULT '[]',
        findings_before INTEGER NOT NULL,
        created_at      INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_review_coverage_conversation
        ON review_coverage(conversation_id, created_at);
    CREATE INDEX IF NOT EXISTS idx_review_coverage_node ON review_coverage(node_id);
";

const COLUMNS: &str = "id, node_id, conversation_id, pass, lens, files, checked, result, \
     finding_ids, reason, read_files, findings_before, created_at";

/// The review conversation `conversation_id` and the node it reviews; an
/// error for any other conversation.
fn review_node(conn: &Connection, conversation_id: Uuid) -> Result<Uuid> {
    let conversation = ConversationRepo::new(conn)
        .get(conversation_id)?
        .with_context(|| format!("conversation {conversation_id} not found"))?;
    if conversation.protocol != ProtocolKind::Review {
        bail!(
            "conversation {conversation_id} is a {} conversation; only a review conversation \
             records coverage or records the review done",
            conversation.protocol.as_str()
        );
    }
    match conversation.focus {
        Focus::Node(node) => Ok(node),
        _ => bail!("review conversation {conversation_id} is not about a node"),
    }
}

/// How many findings `conversation_id` has recorded.
fn findings_recorded(conn: &Connection, conversation_id: Uuid) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM review_findings WHERE conversation_id = ?1",
        params![uuid_to_blob(conversation_id)],
        |row| row.get(0),
    )?)
}

fn clean_list(items: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items.iter().map(|i| i.trim()).filter(|i| !i.is_empty()) {
        if !out.iter().any(|o| o == item) {
            out.push(item.to_string());
        }
    }
    out
}

/// Record one coverage entry for review conversation `conversation_id`.
pub fn add(conn: &Connection, conversation_id: Uuid, entry: &NewCoverage) -> Result<CoverageEntry> {
    let node_id = review_node(conn, conversation_id)?;
    let (pass, lens_slug) = match &entry.pass {
        CoveragePass::Lens(slug) => {
            let lens = lens(slug).with_context(|| {
                format!(
                    "unknown lens `{slug}` (expected one of {})",
                    LENSES.iter().map(|l| l.slug).collect::<Vec<_>>().join(", ")
                )
            })?;
            ("lens", Some(lens.slug))
        }
        CoveragePass::Saturation => ("saturation", None),
    };
    let checked = entry
        .checked
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let (result, finding_ids, reason) = match &entry.result {
        CoverageResult::None => (RESULT_NONE, Vec::new(), None),
        CoverageResult::Findings(ids) => {
            if ids.is_empty() {
                bail!("a result of findings names at least one finding");
            }
            let mut unique: Vec<Uuid> = Vec::new();
            for id in ids {
                let owner: Option<Vec<u8>> = conn
                    .query_row(
                        "SELECT node_id FROM review_findings WHERE id = ?1",
                        params![uuid_to_blob(*id)],
                        |row| row.get(0),
                    )
                    .optional()?;
                match owner {
                    None => bail!("finding {id} not found"),
                    Some(owner) if blob_to_uuid_sql(&owner)? != node_id => {
                        bail!("finding {id} is on another node, not the one under review")
                    }
                    Some(_) => {}
                }
                if !unique.contains(id) {
                    unique.push(*id);
                }
            }
            (RESULT_FINDINGS, unique, None)
        }
        CoverageResult::NotApplicable(why) => {
            let why = why.trim();
            if why.is_empty() {
                bail!("a lens that does not apply needs the reason it does not");
            }
            if lens_slug.is_none() {
                bail!(
                    "a saturation pass always applies: its result is none, or the findings it \
                     recorded"
                );
            }
            (RESULT_NOT_APPLICABLE, Vec::new(), Some(why.to_string()))
        }
    };
    if result != RESULT_NOT_APPLICABLE && checked.is_none() {
        bail!("say what was checked, and how");
    }
    let files = clean_list(&entry.files);
    let read = clean_list(&entry.read);
    let id = Uuid::new_v4();
    conn.execute(
        "INSERT INTO review_coverage
         (id, node_id, conversation_id, pass, lens, files, checked, result, finding_ids,
          reason, read_files, findings_before, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            uuid_to_blob(id),
            uuid_to_blob(node_id),
            uuid_to_blob(conversation_id),
            pass,
            lens_slug,
            serde_json::to_string(&files)?,
            checked,
            result,
            serde_json::to_string(&finding_ids.iter().map(Uuid::to_string).collect::<Vec<_>>())?,
            reason,
            serde_json::to_string(&read)?,
            findings_recorded(conn, conversation_id)?,
            now_ms(),
        ],
    )?;
    for_conversation(conn, conversation_id)?
        .into_iter()
        .find(|e| e.id == id)
        .context("coverage entry vanished after insert")
}

/// Every entry `conversation_id` recorded, oldest first.
pub fn for_conversation(conn: &Connection, conversation_id: Uuid) -> Result<Vec<CoverageEntry>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM review_coverage WHERE conversation_id = ?1
         ORDER BY created_at, rowid"
    ))?;
    let rows = stmt
        .query_map(params![uuid_to_blob(conversation_id)], map_row)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().collect()
}

/// A node's latest review and what it recorded of its coverage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestCoverage {
    /// The review conversation.
    pub conversation_id: Uuid,
    /// Its entries, oldest first.
    pub entries: Vec<CoverageEntry>,
    /// What it still lacks before it could be recorded done.
    pub gaps: CoverageGaps,
}

impl LatestCoverage {
    /// One line for a summary: how many required lenses have an entry, and
    /// whether the saturation pass is still owed.
    pub fn summary_label(&self) -> String {
        let covered = self.gaps.required - self.gaps.missing_lenses.len();
        let mut out = format!("Coverage {covered}/{} lenses", self.gaps.required);
        if self.gaps.saturation_missing {
            out.push_str(" · saturation pass owed");
        }
        if !self.gaps.unread_guidance.is_empty() {
            out.push_str(&format!(" · {} selected guidance unread", self.gaps.unread_guidance.len()));
        }
        out
    }
}

/// The node's latest review (the conversation of its latest round, never a
/// later group of the phase's skills) and that review's coverage; `None`
/// when it was never reviewed.
pub fn latest_for_node(conn: &Connection, node_id: Uuid) -> Result<Option<LatestCoverage>> {
    let Some(review) = ConversationRepo::new(conn)
        .latest_for_focus_with_protocol(Focus::Node(node_id), ProtocolKind::Review)?
    else {
        return Ok(None);
    };
    Ok(Some(LatestCoverage {
        conversation_id: review.id,
        entries: for_conversation(conn, review.id)?,
        gaps: gaps(conn, review.id)?,
    }))
}

/// What keeps a review conversation from being recorded done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageGaps {
    /// Required lenses with no entry.
    pub missing_lenses: Vec<&'static Lens>,
    /// How many lenses were required.
    pub required: usize,
    /// No saturation pass that found nothing was recorded after the
    /// conversation's last finding.
    pub saturation_missing: bool,
    /// The node's selected guidance files and skills this review neither had
    /// inlined in its opening nor lists as read (`kind:item`; find-it-first
    /// 6g, `crate::guidance::unread_for_review`).
    pub unread_guidance: Vec<String>,
}

impl CoverageGaps {
    pub fn is_empty(&self) -> bool {
        self.missing_lenses.is_empty() && !self.saturation_missing && self.unread_guidance.is_empty()
    }

    /// Exactly what is missing, for the agent asked to record the review done.
    pub fn message(&self) -> String {
        let mut out = String::from("the review cannot be recorded done yet; its coverage record is missing:");
        if !self.missing_lenses.is_empty() {
            out.push_str(&format!(
                "\n- an entry for {} of the {} lenses: {}. Record each with `review coverage --lens \
                 <lens>` (with `--result n/a --reason <why>` when it does not apply to this change).",
                self.missing_lenses.len(),
                self.required,
                self.missing_lenses.iter().map(|l| l.slug).collect::<Vec<_>>().join(", ")
            ));
        }
        if !self.unread_guidance.is_empty() {
            out.push_str(&format!(
                "\n- the reading of the task's selected guidance: {}. Each was selected for this change \
                 but was not in your opening; read it, apply it, and list it with `--read <path>` on the \
                 coverage entries it informed.",
                self.unread_guidance.join(", ")
            ));
        }
        if self.saturation_missing {
            out.push_str(
                "\n- a saturation pass that found nothing, recorded after this review's last \
                 finding. Go through the change again assuming you missed at least three \
                 defects, record what you find, then record the pass with `review coverage \
                 --pass saturation` (`--result none` only when it found nothing new).",
            );
        }
        out
    }
}

/// What review conversation `conversation_id` still has to record before it
/// can record the review done.
pub fn gaps(conn: &Connection, conversation_id: Uuid) -> Result<CoverageGaps> {
    let node = review_node(conn, conversation_id)?;
    let required = required_lenses(conn, conversation_id)?;
    let entries = for_conversation(conn, conversation_id)?;
    let missing_lenses = required
        .iter()
        .copied()
        .filter(|lens| {
            !entries
                .iter()
                .any(|e| matches!(&e.pass, CoveragePass::Lens(slug) if slug == lens.slug))
        })
        .collect();
    let recorded = findings_recorded(conn, conversation_id)?;
    let saturation_missing = !entries.iter().any(|e| {
        e.pass == CoveragePass::Saturation
            && e.result == CoverageResult::None
            && e.findings_before >= recorded
    });
    Ok(CoverageGaps {
        missing_lenses,
        required: required.len(),
        saturation_missing,
        unread_guidance: crate::guidance::unread_for_review(conn, conversation_id, node)?,
    })
}

/// The report `review done` records on the review conversation.
pub fn done_report() -> Value {
    json!({ "review": "done" })
}

/// Whether `report` says the review is finished.
pub fn is_done_report(report: &Value) -> bool {
    report.get("review").and_then(Value::as_str) == Some("done")
}

/// Record review conversation `conversation_id` done, refused with exactly
/// what is missing while its coverage is incomplete ([`gaps`]).
pub fn record_done(conn: &Connection, conversation_id: Uuid) -> Result<i64> {
    let gaps = gaps(conn, conversation_id)?;
    if !gaps.is_empty() {
        bail!("{}", gaps.message());
    }
    ConversationRepo::new(conn).record_report(conversation_id, &done_report())
}

fn json_list(raw: &str, column: &str) -> Result<Vec<String>> {
    serde_json::from_str(raw).with_context(|| format!("review_coverage.{column} is not a JSON list: {raw}"))
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<CoverageEntry>> {
    let id = blob_to_uuid_sql(&row.get::<_, Vec<u8>>(0)?)?;
    let node_id = blob_to_uuid_sql(&row.get::<_, Vec<u8>>(1)?)?;
    let conversation_id = blob_to_uuid_sql(&row.get::<_, Vec<u8>>(2)?)?;
    let pass: String = row.get(3)?;
    let lens: Option<String> = row.get(4)?;
    let files: String = row.get(5)?;
    let checked: Option<String> = row.get(6)?;
    let result: String = row.get(7)?;
    let finding_ids: String = row.get(8)?;
    let reason: Option<String> = row.get(9)?;
    let read: String = row.get(10)?;
    let findings_before: i64 = row.get(11)?;
    let created_at: i64 = row.get(12)?;
    Ok((|| {
        let pass = match (pass.as_str(), lens) {
            ("saturation", _) => CoveragePass::Saturation,
            ("lens", Some(slug)) => CoveragePass::Lens(slug),
            (other, lens) => bail!("review coverage {id}: pass `{other}` with lens {lens:?}"),
        };
        let result = match result.as_str() {
            RESULT_NONE => CoverageResult::None,
            RESULT_FINDINGS => CoverageResult::Findings(
                json_list(&finding_ids, "finding_ids")?
                    .iter()
                    .map(|raw| Uuid::parse_str(raw).with_context(|| format!("finding id `{raw}`")))
                    .collect::<Result<_>>()?,
            ),
            RESULT_NOT_APPLICABLE => CoverageResult::NotApplicable(reason.unwrap_or_default()),
            other => bail!("review coverage {id}: unknown result `{other}`"),
        };
        Ok(CoverageEntry {
            id,
            node_id,
            conversation_id,
            pass,
            files: json_list(&files, "files")?,
            checked,
            result,
            read: json_list(&read, "read_files")?,
            findings_before,
            created_at,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::schema;
    use crate::outline::repos::NodeRepo;
    use crate::review::{NewFinding, ReviewRepo};

    struct Fx {
        conn: Connection,
        node: Uuid,
        review: Uuid,
    }

    fn setup() -> Fx {
        let dir = tod_test_util::scratch_dir("tod-review-coverage");
        let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
        let node = Uuid::new_v4();
        NodeRepo::new(&conn).create_with_id(node, "reviewed", "Reviewed").unwrap();
        let review = ConversationRepo::new(&conn)
            .create(Focus::Node(node), ProtocolKind::Review, None, None, None)
            .unwrap()
            .id;
        Fx { conn, node, review }
    }

    fn lens_entry(slug: &str, result: CoverageResult) -> NewCoverage {
        NewCoverage {
            pass: CoveragePass::Lens(slug.to_string()),
            files: vec!["src/lib.rs".into(), " src/lib.rs ".into(), "".into()],
            checked: Some("every caller of parse".into()),
            result,
            read: vec!["REVIEW.md".into()],
        }
    }

    fn saturation(result: CoverageResult) -> NewCoverage {
        NewCoverage {
            pass: CoveragePass::Saturation,
            files: Vec::new(),
            checked: Some("again, where the findings cluster".into()),
            result,
            read: Vec::new(),
        }
    }

    fn finding(fx: &Fx) -> Uuid {
        ReviewRepo::new(&fx.conn)
            .add(
                fx.node,
                Some(fx.review),
                &NewFinding {
                    severity: "high".into(),
                    summary: "Wrong row".into(),
                    root_cause: Some("first row assumed unique".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .id
    }

    fn sweep_all_but(fx: &Fx, skip: &[&str]) {
        for lens in LENSES.iter().filter(|l| !skip.contains(&l.slug)) {
            add(&fx.conn, fx.review, &lens_entry(lens.slug, CoverageResult::None)).unwrap();
        }
    }

    #[test]
    fn an_entry_is_stored_with_its_files_reads_and_findings() {
        let fx = setup();
        let id = finding(&fx);
        let entry = add(
            &fx.conn,
            fx.review,
            &lens_entry("Data_Assumptions", CoverageResult::Findings(vec![id, id])),
        )
        .unwrap();
        assert_eq!(entry.pass, CoveragePass::Lens("data-assumptions".into()));
        assert_eq!(entry.files, ["src/lib.rs"]);
        assert_eq!(entry.read, ["REVIEW.md"]);
        assert_eq!(entry.result, CoverageResult::Findings(vec![id]));
        assert_eq!(entry.findings_before, 1);
        assert_eq!(entry.node_id, fx.node);
        let na = add(
            &fx.conn,
            fx.review,
            &NewCoverage {
                checked: None,
                ..lens_entry(
                    "external-systems",
                    CoverageResult::NotApplicable("no API is called".into()),
                )
            },
        )
        .unwrap();
        assert_eq!(na.result_label(), "n/a: no API is called");
        let latest = latest_for_node(&fx.conn, fx.node).unwrap().unwrap();
        assert_eq!(latest.conversation_id, fx.review);
        assert_eq!(latest.entries.len(), 2);
        assert_eq!(latest.summary_label(), "Coverage 2/11 lenses · saturation pass owed");
    }

    #[test]
    fn a_bad_entry_is_refused() {
        let fx = setup();
        let refused =
            |entry: NewCoverage| add(&fx.conn, fx.review, &entry).unwrap_err().to_string();
        assert!(refused(lens_entry("style", CoverageResult::None)).contains("unknown lens `style`"));
        assert!(
            refused(NewCoverage { checked: None, ..lens_entry("reach", CoverageResult::None) })
                .contains("what was checked")
        );
        assert!(
            refused(lens_entry("reach", CoverageResult::NotApplicable(" ".into())))
                .contains("reason")
        );
        assert!(
            refused(lens_entry("reach", CoverageResult::Findings(Vec::new())))
                .contains("at least one")
        );
        assert!(
            refused(lens_entry("reach", CoverageResult::Findings(vec![Uuid::new_v4()])))
                .contains("not found")
        );
        assert!(
            refused(saturation(CoverageResult::NotApplicable("tired".into())))
                .contains("always applies")
        );

        // A finding on another node is not this review's.
        let other = Uuid::new_v4();
        NodeRepo::new(&fx.conn).create_with_id(other, "other", "Other").unwrap();
        let elsewhere = ReviewRepo::new(&fx.conn)
            .add(
                other,
                None,
                &NewFinding { severity: "low".into(), summary: "x".into(), ..Default::default() },
            )
            .unwrap()
            .id;
        assert!(
            refused(lens_entry("reach", CoverageResult::Findings(vec![elsewhere])))
                .contains("another node")
        );

        // Only a review conversation records coverage, or the review done.
        let fix = ConversationRepo::new(&fx.conn)
            .create(Focus::Node(fx.node), ProtocolKind::Fix, None, None, None)
            .unwrap()
            .id;
        let err = add(&fx.conn, fix, &lens_entry("reach", CoverageResult::None))
            .unwrap_err()
            .to_string();
        assert!(err.contains("only a review conversation"), "{err}");
        assert!(record_done(&fx.conn, fix).is_err());
    }

    #[test]
    fn done_is_refused_until_every_lens_is_covered_and_a_clean_saturation_pass_follows_the_last_finding()
     {
        let fx = setup();
        let err = record_done(&fx.conn, fx.review).unwrap_err().to_string();
        assert!(
            err.contains("an entry for 11 of the 11 lenses: obligations, data-assumptions"),
            "{err}"
        );
        assert!(err.contains("a saturation pass"), "{err}");

        sweep_all_but(&fx, &["reach", "tests"]);
        let gaps = gaps(&fx.conn, fx.review).unwrap();
        assert_eq!(
            gaps.missing_lenses.iter().map(|l| l.slug).collect::<Vec<_>>(),
            ["reach", "tests"]
        );
        assert!(gaps.saturation_missing);
        let err = record_done(&fx.conn, fx.review).unwrap_err().to_string();
        assert!(err.contains("2 of the 11 lenses: reach, tests."), "{err}");

        add(&fx.conn, fx.review, &lens_entry("reach", CoverageResult::None)).unwrap();
        add(&fx.conn, fx.review, &lens_entry("tests", CoverageResult::None)).unwrap();
        add(&fx.conn, fx.review, &saturation(CoverageResult::None)).unwrap();
        assert!(super::gaps(&fx.conn, fx.review).unwrap().is_empty());

        // A finding after the pass needs another pass; one that found
        // something does not count, a clean one after it does.
        let id = finding(&fx);
        let err = record_done(&fx.conn, fx.review).unwrap_err().to_string();
        assert!(!err.contains("lenses"), "{err}");
        assert!(err.contains("a saturation pass"), "{err}");
        add(&fx.conn, fx.review, &saturation(CoverageResult::Findings(vec![id]))).unwrap();
        assert!(super::gaps(&fx.conn, fx.review).unwrap().saturation_missing);
        add(&fx.conn, fx.review, &saturation(CoverageResult::None)).unwrap();
        record_done(&fx.conn, fx.review).unwrap();
        let report = ConversationRepo::new(&fx.conn).latest_report(fx.review).unwrap().unwrap();
        assert!(is_done_report(&report));
    }

    /// A finding recorded outside this conversation (an earlier round) does
    /// not make this round's saturation pass stale.
    #[test]
    fn only_this_rounds_findings_count_against_its_saturation_pass() {
        let fx = setup();
        sweep_all_but(&fx, &[]);
        add(&fx.conn, fx.review, &saturation(CoverageResult::None)).unwrap();
        ReviewRepo::new(&fx.conn)
            .add(
                fx.node,
                None,
                &NewFinding { severity: "low".into(), summary: "x".into(), ..Default::default() },
            )
            .unwrap();
        assert!(gaps(&fx.conn, fx.review).unwrap().is_empty());
    }

    #[test]
    fn coverage_goes_with_its_conversation() {
        let fx = setup();
        add(&fx.conn, fx.review, &lens_entry("reach", CoverageResult::None)).unwrap();
        fx.conn
            .execute("DELETE FROM conversations WHERE id = ?1", params![uuid_to_blob(fx.review)])
            .unwrap();
        let left: i64 = fx
            .conn
            .query_row("SELECT COUNT(*) FROM review_coverage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn done_is_refused_while_a_selected_guidance_file_was_neither_inlined_nor_read() {
        let fx = setup();
        let select = |kind: &str, item: &str| {
            crate::guidance::select(
                &fx.conn,
                crate::interview::ACTOR_USER,
                &crate::guidance::Selection {
                    node_id: fx.node,
                    conversation_id: None,
                    kind: kind.into(),
                    item: item.into(),
                    applies: true,
                    reason: "the change touches it".into(),
                },
            )
            .unwrap();
        };
        select(crate::guidance::KIND_FILE, "kb/CLAUDE.md");
        select(crate::guidance::KIND_SKILL, "sql-patterns");
        ConversationRepo::new(&fx.conn)
            .set_opening_context(
                fx.review,
                &format!("...\n{}\n...", crate::guidance::inline_heading("skill", "sql-patterns")),
            )
            .unwrap();
        sweep_all_but(&fx, &[]);
        add(&fx.conn, fx.review, &saturation(CoverageResult::None)).unwrap();
        let err = record_done(&fx.conn, fx.review).unwrap_err().to_string();
        assert!(err.contains("file:kb/CLAUDE.md"), "{err}");
        assert!(!err.contains("sql-patterns"), "an inlined skill needs no read: {err}");
        assert!(!err.contains("lenses"), "{err}");
        add(
            &fx.conn,
            fx.review,
            &NewCoverage { read: vec!["kb/CLAUDE.md".into()], ..lens_entry("reach", CoverageResult::None) },
        )
        .unwrap();
        record_done(&fx.conn, fx.review).unwrap();
    }
}
