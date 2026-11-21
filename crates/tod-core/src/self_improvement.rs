//! What the self-improvement view shows (`doc/requests/design.md` 11.4,
//! `doc/lifecycle/self-improvement.md`): the lessons gathered, how the process
//! review grouped them, what was changed because of them, and the measures.
//!
//! This is only a projection of what the store holds. The lessons are
//! `tod_store::lessons`; the measures are `tod_store::lesson_stats::compute`,
//! the one computation `tod-cli lessons stats` also uses, so the view and the
//! command cannot disagree. Nothing here writes: the user's decline and reopen
//! go through `InterviewCommand::Lessons`.

use anyhow::Result;
use rusqlite::Connection;
use std::collections::BTreeMap;
use tod_store::lesson_stats::{self, Stats};
use tod_store::lessons::{
    self, Lesson, STATUS_APPLIED, STATUS_DECLINED, STATUS_GROUPED, STATUS_NEW,
};
use tod_store::outline::repos::NodeRepo;
use uuid::Uuid;

/// A node a lesson links to: what the view shows, and the id a click opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRef {
    pub id: Uuid,
    pub slug: String,
    pub title: String,
}

impl NodeRef {
    /// `Title (slug)`.
    pub fn label(&self) -> String {
        format!("{} ({})", self.title, self.slug)
    }
}

/// What a lesson's `applied_ref` names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppliedRef {
    /// Not applied.
    None,
    /// A git commit, whose changes can be looked at.
    Commit(String),
    /// A node (by slug) that acted on it.
    Node(NodeRef),
    /// Anything else the user or the review wrote.
    Text(String),
}

/// Whether `text` looks like a commit id: 7 to 40 hex digits.
pub fn is_commit_ref(text: &str) -> bool {
    let t = text.trim();
    (7..=40).contains(&t.len()) && t.chars().all(|c| c.is_ascii_hexdigit())
}

/// One lesson with the links the view needs resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LessonRow {
    pub lesson: Lesson,
    /// The node the lesson was recorded on.
    pub node: Option<NodeRef>,
    /// The process-review run node that took it in.
    pub run: Option<NodeRef>,
    pub applied: AppliedRef,
}

impl LessonRow {
    /// Where "show the changes" looks: the run node that made them, else the
    /// node the node-reference names, else the lesson's own node. The commit
    /// is in that node's checkout.
    pub fn changes_node(&self) -> Option<Uuid> {
        match &self.applied {
            AppliedRef::Commit(_) => {
                self.run.as_ref().or(self.node.as_ref()).map(|n| n.id)
            }
            _ => None,
        }
    }

    /// The row as text, for Copy.
    pub fn text(&self) -> String {
        let l = &self.lesson;
        let mut out = format!("[{} · {} · {}] {}", l.source, l.category, l.status, l.summary.trim());
        if !l.target.is_empty() {
            out.push_str(&format!("\nTarget: {}", l.target));
        }
        if !l.user_quote.trim().is_empty() {
            out.push_str(&format!("\nThe user said: \"{}\"", l.user_quote.trim()));
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionKind {
    /// Gathered, not yet looked at by a review.
    New,
    /// Taken in by one process-review run.
    Grouped,
    /// Acted on by one commit or node.
    Applied,
    Declined,
}

/// A group of lessons under one heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Stable across reloads: the list keeps a collapsed set by it.
    pub key: String,
    pub heading: String,
    pub kind: SectionKind,
    /// The run node a `Grouped` section is about, so its heading links.
    pub run: Option<NodeRef>,
    pub rows: Vec<LessonRow>,
}

/// Everything the view shows, from one read.
#[derive(Debug, Clone)]
pub struct Overview {
    pub sections: Vec<Section>,
    pub stats: Stats,
}

impl Overview {
    /// Every row, in the order shown.
    pub fn rows(&self) -> impl Iterator<Item = &LessonRow> {
        self.sections.iter().flat_map(|s| s.rows.iter())
    }

    pub fn lesson_count(&self) -> usize {
        self.rows().count()
    }
}

struct Resolver<'a> {
    nodes: NodeRepo<'a>,
    cache: BTreeMap<Uuid, Option<NodeRef>>,
}

impl<'a> Resolver<'a> {
    fn node(&mut self, id: Uuid) -> Result<Option<NodeRef>> {
        if let Some(found) = self.cache.get(&id) {
            return Ok(found.clone());
        }
        let found = self.nodes.get(id)?.map(|n| NodeRef { id: n.id, slug: n.slug, title: n.title });
        self.cache.insert(id, found.clone());
        Ok(found)
    }

    fn applied(&mut self, lesson: &Lesson) -> Result<AppliedRef> {
        let r = lesson.applied_ref.trim();
        if lesson.status != STATUS_APPLIED || r.is_empty() {
            return Ok(AppliedRef::None);
        }
        if is_commit_ref(r) {
            return Ok(AppliedRef::Commit(r.to_string()));
        }
        if let Some(node) = self.nodes.get_by_slug(r)? {
            return Ok(AppliedRef::Node(NodeRef { id: node.id, slug: node.slug, title: node.title }));
        }
        Ok(AppliedRef::Text(r.to_string()))
    }
}

/// Group `rows` into sections: the new ones, one section per process-review
/// run (newest first), one per applied reference (most recently applied
/// first), and the declined ones. An empty section is left out.
pub fn sections(rows: Vec<LessonRow>) -> Vec<Section> {
    let mut new = Vec::new();
    let mut declined = Vec::new();
    let mut runs: BTreeMap<Uuid, (i64, Option<NodeRef>, Vec<LessonRow>)> = BTreeMap::new();
    let mut applied: BTreeMap<String, (i64, String, Vec<LessonRow>)> = BTreeMap::new();
    for row in rows {
        match row.lesson.status.as_str() {
            STATUS_NEW => new.push(row),
            STATUS_DECLINED => declined.push(row),
            STATUS_APPLIED => {
                let key = row.lesson.applied_ref.trim().to_string();
                let heading = match &row.applied {
                    AppliedRef::Node(n) => format!("Applied by node {}", n.label()),
                    AppliedRef::Commit(c) => format!("Applied by commit {c}"),
                    AppliedRef::Text(t) => format!("Applied: {t}"),
                    AppliedRef::None => "Applied".to_string(),
                };
                let entry = applied.entry(key).or_insert((0, heading, Vec::new()));
                entry.0 = entry.0.max(row.lesson.updated_at);
                entry.2.push(row);
            }
            STATUS_GROUPED => {
                let run = row.lesson.run_node_id.unwrap_or_default();
                let entry = runs.entry(run).or_insert((0, row.run.clone(), Vec::new()));
                entry.0 = entry.0.max(row.lesson.grouped_at.unwrap_or(0));
                entry.2.push(row);
            }
            _ => new.push(row),
        }
    }
    let mut out = Vec::new();
    if !new.is_empty() {
        out.push(Section {
            key: "new".into(),
            heading: "New (not yet reviewed)".into(),
            kind: SectionKind::New,
            run: None,
            rows: new,
        });
    }
    let mut runs: Vec<_> = runs.into_iter().collect();
    runs.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (id, (_, run, rows)) in runs {
        let heading = match &run {
            Some(n) => format!("Grouped by the process review: {}", n.label()),
            None => "Grouped by a process review (its node is gone)".to_string(),
        };
        out.push(Section { key: format!("run:{id}"), heading, kind: SectionKind::Grouped, run, rows });
    }
    let mut applied: Vec<_> = applied.into_iter().collect();
    applied.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (key, (_, heading, rows)) in applied {
        out.push(Section { key: format!("applied:{key}"), heading, kind: SectionKind::Applied, run: None, rows });
    }
    if !declined.is_empty() {
        out.push(Section {
            key: "declined".into(),
            heading: "Declined".into(),
            kind: SectionKind::Declined,
            run: None,
            rows: declined,
        });
    }
    out
}

/// Read the lessons and the measures. Reads only; run it off the UI thread.
pub fn load(conn: &Connection) -> Result<Overview> {
    let mut resolver = Resolver { nodes: NodeRepo::new(conn), cache: BTreeMap::new() };
    let mut all = lessons::list(conn, None, None)?;
    // Newest first inside a section.
    all.reverse();
    let mut rows = Vec::with_capacity(all.len());
    for lesson in all {
        let node = resolver.node(lesson.node_id)?;
        let run = match lesson.run_node_id {
            Some(id) => resolver.node(id)?,
            None => None,
        };
        let applied = resolver.applied(&lesson)?;
        rows.push(LessonRow { lesson, node, run, applied });
    }
    Ok(Overview { sections: sections(rows), stats: lesson_stats::compute(conn, None)? })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lesson(status: &str, run: Option<Uuid>, applied_ref: &str, at: i64) -> Lesson {
        Lesson {
            id: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
            source: "user".into(),
            decision_id: None,
            conversation_id: None,
            send_back_id: None,
            incident_id: None,
            category: "necessary".into(),
            target: String::new(),
            summary: "s".into(),
            user_quote: String::new(),
            status: status.into(),
            applied_ref: applied_ref.into(),
            created_at: at,
            updated_at: at,
            event_key: String::new(),
            run_node_id: run,
            grouped_at: run.map(|_| at),
        }
    }

    fn row(l: Lesson, applied: AppliedRef) -> LessonRow {
        LessonRow { lesson: l, node: None, run: None, applied }
    }

    #[test]
    fn commit_ids_are_told_from_slugs() {
        assert!(is_commit_ref("abc1234"));
        assert!(is_commit_ref("0123456789abcdef0123456789abcdef01234567"));
        assert!(!is_commit_ref("abc123"), "too short");
        assert!(!is_commit_ref("process-review-1"));
        assert!(!is_commit_ref("deadbeefxyz"));
    }

    #[test]
    fn sections_run_new_then_each_run_then_each_change_then_declined() {
        let run_a = Uuid::new_v4();
        let run_b = Uuid::new_v4();
        let rows = vec![
            row(lesson("declined", None, "", 1), AppliedRef::None),
            row(lesson("grouped", Some(run_a), "", 5), AppliedRef::None),
            row(lesson("grouped", Some(run_b), "", 9), AppliedRef::None),
            row(lesson("grouped", Some(run_a), "", 6), AppliedRef::None),
            row(lesson("applied", Some(run_a), "abc1234", 7), AppliedRef::Commit("abc1234".into())),
            row(lesson("applied", Some(run_a), "abc1234", 8), AppliedRef::Commit("abc1234".into())),
            row(lesson("new", None, "", 3), AppliedRef::None),
        ];
        let got = sections(rows);
        let shape: Vec<(String, usize)> = got.iter().map(|s| (s.key.clone(), s.rows.len())).collect();
        assert_eq!(
            shape,
            vec![
                ("new".to_string(), 1),
                (format!("run:{run_b}"), 1),
                (format!("run:{run_a}"), 2),
                ("applied:abc1234".to_string(), 2),
                ("declined".to_string(), 1),
            ]
        );
        assert_eq!(got[3].heading, "Applied by commit abc1234");
    }

    #[test]
    fn load_reads_the_store_and_reuses_the_stats_computation() {
        use tod_store::fleet::FleetStore;
        use tod_store::interview::InterviewCommand;
        use tod_store::outline::{CreatePosition, OutlineMutation};
        let root = tod_test_util::scratch_dir(format!("tod-self-improvement-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let fleet = FleetStore::open(&root).unwrap();
        fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
        fleet.writer().flush().unwrap();
        let list_id = fleet.list_outline_lists().unwrap()[0].id;
        let node = Uuid::new_v4();
        fleet
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(node),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: "Task".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        fleet.reload_if_stale().unwrap();
        let add = |text: &str| {
            let out = fleet
                .interview(
                    "user",
                    InterviewCommand::Lessons(lessons::Op::Add {
                        node,
                        text: text.into(),
                        category: None,
                        target: String::new(),
                    }),
                )
                .unwrap();
            out["id"].as_str().map(|s| s.parse::<Uuid>().unwrap()).unwrap()
        };
        let declined = add("one");
        add("two");
        fleet.interview("user", InterviewCommand::Lessons(lessons::Op::Decline { id: declined })).unwrap();
        let overview = fleet.read(load).unwrap();
        let keys: Vec<&str> = overview.sections.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, vec!["new", "declined"]);
        assert_eq!(overview.lesson_count(), 2);
        assert_eq!(overview.sections[0].rows[0].node.as_ref().unwrap().title, "Task");
        assert_eq!(overview.stats.lessons.total, 2);
        assert_eq!(overview.stats.render(), fleet.read(|c| Ok(lesson_stats::compute(c, None)?.render())).unwrap());
    }

    #[test]
    fn no_lessons_make_no_sections() {
        assert!(sections(Vec::new()).is_empty());
    }

    #[test]
    fn a_commit_is_looked_at_in_the_run_nodes_checkout() {
        let run = NodeRef { id: Uuid::new_v4(), slug: "run".into(), title: "Run".into() };
        let own = NodeRef { id: Uuid::new_v4(), slug: "own".into(), title: "Own".into() };
        let mut r = row(lesson("applied", None, "abc1234", 1), AppliedRef::Commit("abc1234".into()));
        r.node = Some(own.clone());
        assert_eq!(r.changes_node(), Some(own.id));
        r.run = Some(run.clone());
        assert_eq!(r.changes_node(), Some(run.id));
        r.applied = AppliedRef::Text("by hand".into());
        assert_eq!(r.changes_node(), None);
    }
}
