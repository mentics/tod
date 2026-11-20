//! The process review (`doc/requests/design.md` 11.2,
//! `doc/lifecycle/self-improvement.md`): the part of tod's self-improvement
//! loop that turns accumulated lessons into proposed changes to tod's own
//! process.
//!
//! The daemon calls [`start_due`] on its tick. When a parent node is
//! configured (`process_review.parent` in `tod.yml`), at least
//! `min_lessons` lessons are new and `every_hours` passed since the last run
//! took lessons in, the new lessons are grouped by target and cause
//! ([`groups`]) and **one node per run** is made under the parent
//! ([`start_run`]), its description listing every group with the lessons and
//! the nodes they came from. The lessons move to `grouped`
//! (`improvement_lessons.run_node_id`), so the next run does not take them
//! again.
//!
//! The run node is an ordinary Lifecycle node: it inherits the parent's Files
//! location, so the first launch makes its own worktree (the session works in
//! it, `ProtocolKind::ProcessReview`'s `cwd`), and the user reaches the
//! drafted change the way they reach any node's work: through the lifecycle,
//! where it is reviewed and approved. The session records which lessons its
//! changes address with `tod-cli lessons apply --ref <commit>`; those become
//! `applied`. When a project (not tod) owns a lesson's target and
//! `process_review.project_parent` is set, that group goes to a second node
//! under it, in the same run.
//!
//! Nothing here calls a model: the autopilot's `process_review_step`
//! (`autopilot/process_review.rs`) starts the session when the node is run.

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use tod_store::fleet::FleetStore;
use tod_store::interview::InterviewCommand;
use tod_store::lessons::{self, CATEGORY_BRIEF_UNCLEAR, CATEGORY_NECESSARY, Lesson, STATUS_NEW};
use tod_store::outline::repos::{NodeRepo, OutlineRepo};
use tod_store::outline::{Capability, CreatePosition, EXTRA_CONTENT_DETAILS, OutlineMutation};
use tod_store::settings::ProcessReviewSettings;
use uuid::Uuid;

const HOUR_MS: i64 = 3_600_000;

/// The actor of everything the app writes for the review.
pub const ACTOR: &str = "process-review";

/// What a group says when its lessons ask for no change.
const NO_CHANGE: &str = "no change wanted: asking was right (counter-evidence for the others)";

/// Lessons that share a target and a cause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub target: String,
    pub category: String,
    pub lessons: Vec<Lesson>,
}

impl Group {
    /// The title the group is shown under.
    pub fn heading(&self) -> &str {
        if self.target.is_empty() { NO_CHANGE } else { &self.target }
    }

    /// Whether the target is a project's own file (a project skill, its
    /// `CLAUDE.md`), not tod's: such a group goes under `project_parent` when
    /// one is set.
    pub fn project_scope(&self) -> bool {
        let t = self.target.to_lowercase();
        t.starts_with("project") || t.contains(".claude/skills")
    }
}

/// `lessons` grouped by (target, cause): the triage brief guidance first
/// (`brief_unclear` is the first target of the review), then the biggest
/// groups.
pub fn groups(lessons: &[Lesson]) -> Vec<Group> {
    let mut by_key: BTreeMap<(String, String), Vec<Lesson>> = BTreeMap::new();
    for lesson in lessons {
        let target = if lesson.category == CATEGORY_NECESSARY { String::new() } else { lesson.target.clone() };
        by_key.entry((target, lesson.category.clone())).or_default().push(lesson.clone());
    }
    let mut out: Vec<Group> = by_key
        .into_iter()
        .map(|((target, category), lessons)| Group { target, category, lessons })
        .collect();
    out.sort_by(|a, b| {
        let first = |g: &Group| g.category != CATEGORY_BRIEF_UNCLEAR;
        first(a)
            .cmp(&first(b))
            .then(b.lessons.len().cmp(&a.lessons.len()))
            .then(a.target.cmp(&b.target))
    });
    out
}

/// The new lessons, when a review is due: a parent is configured, at least
/// `min_lessons` are new, and `every_hours` passed since a run last took
/// lessons in (since the oldest new lesson, when none ever did).
pub fn due(conn: &rusqlite::Connection, settings: &ProcessReviewSettings, now: i64) -> Result<Option<Vec<Lesson>>> {
    if settings.parent.is_none() {
        return Ok(None);
    }
    let new = lessons::list(conn, Some(STATUS_NEW), None)?;
    if new.is_empty() || (new.len() as u64) < u64::from(settings.min_lessons) {
        return Ok(None);
    }
    let since = match lessons::last_grouped_at(conn)? {
        Some(at) => at,
        None => new.iter().map(|l| l.created_at).min().unwrap_or(now),
    };
    if now - since < i64::from(settings.every_hours) * HOUR_MS {
        return Ok(None);
    }
    Ok(Some(new))
}

fn one_line(text: &str, chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= chars {
        return flat;
    }
    let mut out: String = flat.chars().take(chars).collect();
    out.push_str(" ...");
    out
}

/// The groups as markdown: each lesson with its id, where it came from (the
/// node's slug), what it says and, when the user said something, their words.
pub fn render_groups(conn: &rusqlite::Connection, groups: &[Group]) -> Result<String> {
    let nodes = NodeRepo::new(conn);
    let mut out = String::new();
    for group in groups {
        let _ = writeln!(out, "### {} ({}, {} lesson(s))\n", group.heading(), group.category, group.lessons.len());
        for lesson in &group.lessons {
            let slug = nodes.get(lesson.node_id)?.map(|n| n.slug).unwrap_or_else(|| lesson.node_id.simple().to_string());
            let _ = writeln!(
                out,
                "- lesson `{}` ({}) on node `{slug}`: {}",
                &lesson.id.simple().to_string()[..8],
                lesson.source,
                one_line(&lesson.summary, 400)
            );
            if !lesson.user_quote.trim().is_empty() {
                let _ = writeln!(out, "  - the user said: \"{}\"", one_line(&lesson.user_quote, 400));
            }
        }
        out.push('\n');
    }
    Ok(out)
}

/// What the session is given: the `brief_unclear` lessons with the user's
/// words first (the first target is triage's brief guidance), then every
/// group the run took in.
pub fn render_context(conn: &rusqlite::Connection, run_node: Uuid) -> Result<String> {
    let taken = lessons::for_run(conn, run_node)?;
    if taken.is_empty() {
        bail!("no lessons were grouped into node {run_node}: it is not a process-review run");
    }
    let mut out = String::new();
    let unclear: Vec<&Lesson> = taken.iter().filter(|l| l.category == CATEGORY_BRIEF_UNCLEAR).collect();
    if !unclear.is_empty() {
        out.push_str(
            "### First target: the brief guidance in `surface/triage.md`\n\nThese requests reached the user with a brief that was hard to take in. The user's own words are the evidence: improve the guidance so the next brief is clear.\n\n",
        );
        let nodes = NodeRepo::new(conn);
        for lesson in unclear {
            let slug = nodes.get(lesson.node_id)?.map(|n| n.slug).unwrap_or_else(|| lesson.node_id.simple().to_string());
            let quote = if lesson.user_quote.trim().is_empty() {
                "(the user gave no words)".to_string()
            } else {
                format!("\"{}\"", one_line(&lesson.user_quote, 600))
            };
            let _ = writeln!(
                out,
                "- lesson `{}` on node `{slug}`: {}\n  - the user said: {quote}",
                &lesson.id.simple().to_string()[..8],
                one_line(&lesson.summary, 400)
            );
        }
        out.push('\n');
    }
    out.push_str("### Every lesson in this run, by target and cause\n\n");
    out.push_str(&render_groups(conn, &groups(&taken))?);
    Ok(out)
}

/// Whether the session for `node` is still to run: lessons were grouped into
/// it and no process-review conversation exists for it yet.
pub fn owed(conn: &rusqlite::Connection, node: Uuid) -> Result<bool> {
    if lessons::for_run(conn, node)?.is_empty() {
        return Ok(false);
    }
    let started = tod_store::conversation::ConversationRepo::new(conn)
        .latest_for_focus_with_protocol(tod_store::conversation::Focus::Node(node), tod_store::conversation::ProtocolKind::ProcessReview)?;
    Ok(started.is_none())
}

fn resolve_parent(conn: &rusqlite::Connection, raw: &str) -> Result<Uuid> {
    if let Ok(id) = Uuid::parse_str(raw) {
        return Ok(id);
    }
    NodeRepo::new(conn)
        .get_by_slug(raw)?
        .map(|n| n.id)
        .with_context(|| format!("process_review parent node `{raw}` does not exist"))
}

/// Makes the run's node under `parent` with `groups` in its description and
/// takes their lessons in. Returns the node.
fn make_node(fleet: &FleetStore, parent: Uuid, title: &str, groups: &[Group], description: &str) -> Result<Uuid> {
    let list_id = fleet
        .read(|conn| Ok(OutlineRepo::new(conn).get_entry(parent)?.map(|e| e.list_id)))?
        .with_context(|| format!("the process_review parent {parent} has no outline entry"))?;
    let node = Uuid::new_v4();
    fleet.enqueue_outline(OutlineMutation::CreateNode {
        node_id: Some(node),
        list_id,
        parent_id: Some(parent),
        anchor_id: None,
        position: CreatePosition::Child,
        title: title.to_string(),
    })?;
    fleet.enqueue_outline(OutlineMutation::EnableCapabilities {
        node_id: node,
        capabilities: vec![Capability::Spec, Capability::Lifecycle],
    })?;
    fleet.enqueue_outline(OutlineMutation::SetExtraContent {
        node_id: node,
        content_type: EXTRA_CONTENT_DETAILS.to_string(),
        body: description.to_string(),
    })?;
    fleet.writer().flush()?;
    fleet.reload_if_stale()?;
    let ids: Vec<Uuid> = groups.iter().flat_map(|g| g.lessons.iter().map(|l| l.id)).collect();
    fleet
        .interview(ACTOR, InterviewCommand::Lessons(lessons::Op::Group { ids, run_node: node }))
        .map_err(|e| anyhow::anyhow!("could not take the lessons into the review node {node}: {e}"))?;
    Ok(node)
}

fn description(conn: &rusqlite::Connection, groups: &[Group], project: bool) -> Result<String> {
    let lessons: usize = groups.iter().map(|g| g.lessons.len()).sum();
    let mut out = format!(
        "tod's process review: {lessons} lesson(s) in {} group(s) from the escalation ladder, triage, send-backs, incidents and the user.\n\n\
         {}\n\n",
        groups.len(),
        if project {
            "These belong to the project's own files (a project skill, its `CLAUDE.md`). Draft the changes in this node's worktree; the user approves them through the lifecycle."
        } else {
            "Draft changes to tod's context fragments, role docs, triage guidance, the request template and project skills in this node's worktree; the user approves them through the lifecycle."
        }
    );
    out.push_str(&render_groups(conn, groups)?);
    Ok(out)
}

/// Starts a review run now for `lessons`: the node (or two) under the
/// configured parent(s). Returns the nodes made.
pub fn start_run(fleet: &FleetStore, settings: &ProcessReviewSettings, lessons: &[Lesson], now: i64) -> Result<Vec<Uuid>> {
    let parent_raw = settings.parent.as_deref().context("process_review.parent is not set")?;
    let all = groups(lessons);
    let (project, tod): (Vec<Group>, Vec<Group>) = if settings.project_parent.is_some() {
        all.into_iter().partition(Group::project_scope)
    } else {
        (Vec::new(), all)
    };
    let date = chrono::DateTime::from_timestamp_millis(now).map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_default();
    let mut made = Vec::new();
    for (raw, groups, label, is_project) in [
        (Some(parent_raw), tod, "tod process review", false),
        (settings.project_parent.as_deref(), project, "project process review", true),
    ] {
        let Some(raw) = raw else { continue };
        if groups.is_empty() {
            continue;
        }
        let (parent, text) = fleet.read(|conn| Ok((resolve_parent(conn, raw)?, description(conn, &groups, is_project)?)))?;
        made.push(make_node(fleet, parent, &format!("{label} {date}"), &groups, &text)?);
    }
    Ok(made)
}

/// The daemon's pass: starts a run when one is due. Returns the nodes made.
pub fn start_due(fleet: &FleetStore, settings: &ProcessReviewSettings, now: i64) -> Result<Vec<Uuid>> {
    let Some(lessons) = fleet.read(|conn| due(conn, settings, now))? else {
        return Ok(Vec::new());
    };
    start_run(fleet, settings, &lessons, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fleet() -> (std::path::PathBuf, FleetStore, Uuid, Uuid) {
        let root = tod_test_util::scratch_dir(format!("tod-process-review-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = FleetStore::open(&root).unwrap();
        fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
        fleet.writer().flush().unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let mut ids = Vec::new();
        for title in ["Process improvements", "Some task"] {
            let id = Uuid::new_v4();
            fleet
                .enqueue_outline(OutlineMutation::CreateNode {
                    node_id: Some(id),
                    list_id,
                    parent_id: None,
                    anchor_id: None,
                    position: CreatePosition::Below,
                    title: title.into(),
                })
                .unwrap();
            ids.push(id);
        }
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        (root, fleet, ids[0], ids[1])
    }

    fn lesson(fleet: &FleetStore, node: Uuid, category: &str, target: &str, quote: &str) {
        let text = if quote.is_empty() { format!("a {category} lesson") } else { quote.to_string() };
        fleet
            .interview(
                "user",
                InterviewCommand::Lessons(lessons::Op::Add {
                    node,
                    text,
                    category: Some(category.into()),
                    target: target.into(),
                }),
            )
            .unwrap();
    }

    fn settings(parent: &str, min: u32) -> ProcessReviewSettings {
        ProcessReviewSettings { parent: Some(parent.into()), every_hours: 1, min_lessons: min, ..Default::default() }
    }

    #[test]
    fn groups_put_the_triage_brief_first_then_the_biggest() {
        let (root, fleet, _p, task) = fleet();
        lesson(&fleet, task, "app_should_handle", "tod code", "");
        lesson(&fleet, task, "app_should_handle", "tod code", "");
        lesson(&fleet, task, CATEGORY_BRIEF_UNCLEAR, "brief", "I could not follow it");
        lesson(&fleet, task, CATEGORY_NECESSARY, "ignored", "");
        let all = fleet.read(|conn| Ok(lessons::list(conn, None, None)?)).unwrap();
        let grouped = groups(&all);
        assert_eq!(grouped[0].category, CATEGORY_BRIEF_UNCLEAR);
        assert_eq!(grouped[1].lessons.len(), 2);
        let necessary = grouped.iter().find(|g| g.category == CATEGORY_NECESSARY).unwrap();
        assert!(necessary.target.is_empty(), "asking was right: no target to change");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_review_is_due_only_with_a_parent_enough_lessons_and_the_time_passed() {
        let (root, fleet, _p, task) = fleet();
        for _ in 0..3 {
            lesson(&fleet, task, "app_should_handle", "t", "");
        }
        let far = chrono::Utc::now().timestamp_millis() + 10 * HOUR_MS;
        let due_for = |s: &ProcessReviewSettings, now: i64| fleet.read(|conn| due(conn, s, now)).unwrap().is_some();
        assert!(!due_for(&ProcessReviewSettings::default(), far), "no parent: never");
        assert!(!due_for(&settings("process-improvements", 4), far), "too few lessons");
        assert!(!due_for(&settings("process-improvements", 3), chrono::Utc::now().timestamp_millis()), "too soon");
        assert!(due_for(&settings("process-improvements", 3), far));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_run_makes_a_node_under_the_parent_takes_the_lessons_in_and_is_not_repeated() {
        let (root, fleet, _parent, task) = fleet();
        lesson(&fleet, task, CATEGORY_BRIEF_UNCLEAR, "brief", "too long");
        lesson(&fleet, task, "app_should_handle", "tod code", "");
        let s = settings("process-improvements", 2);
        let far = chrono::Utc::now().timestamp_millis() + 10 * HOUR_MS;
        let made = start_due(&fleet, &s, far).unwrap();
        assert_eq!(made.len(), 1);
        let run = made[0];
        fleet.reload_if_stale().unwrap();
        let (taken, owed_now, context) = fleet
            .read(|conn| Ok((lessons::for_run(conn, run)?, owed(conn, run)?, render_context(conn, run)?)))
            .unwrap();
        assert_eq!(taken.len(), 2);
        assert!(owed_now, "the session has not run");
        assert!(context.contains("First target") && context.contains("too long"), "{context}");
        let description = fleet
            .read(|conn| {
                Ok(tod_store::outline::repos::NodeRepo::new(conn)
                    .get_extra_content(run, EXTRA_CONTENT_DETAILS)?
                    .unwrap_or_default())
            })
            .unwrap();
        assert!(description.contains("tod code") && description.contains("some-task"), "{description}");
        assert!(start_due(&fleet, &s, far).unwrap().is_empty(), "nothing is new any more");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_parent_is_an_error_naming_it_and_takes_nothing() {
        let (root, fleet, _p, task) = fleet();
        lesson(&fleet, task, "app_should_handle", "t", "");
        let far = chrono::Utc::now().timestamp_millis() + 10 * HOUR_MS;
        let err = start_due(&fleet, &settings("no-such-node", 1), far).unwrap_err();
        assert!(format!("{err:#}").contains("no-such-node"), "{err:#}");
        assert_eq!(fleet.read(|conn| Ok(lessons::list(conn, Some(STATUS_NEW), None)?)).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn project_groups_go_to_a_second_parent_when_one_is_set() {
        let (root, fleet, _p, task) = fleet();
        lesson(&fleet, task, "app_should_handle", "tod code", "");
        lesson(&fleet, task, "asker_could_decide", ".claude/skills/deploy/SKILL.md", "");
        let mut s = settings("process-improvements", 2);
        s.project_parent = Some("some-task".into());
        let far = chrono::Utc::now().timestamp_millis() + 10 * HOUR_MS;
        let made = start_due(&fleet, &s, far).unwrap();
        assert_eq!(made.len(), 2, "one node under each parent");
        let _ = std::fs::remove_dir_all(root);
    }
}
