//! What the tree shows of `tod_core::next_actor`: who is next for each node.
//!
//! Everything here is pure (it reads a [`NodeStanding`] and says what to
//! draw, count, filter, or say) so it can be tested without a window. The
//! views never derive a "who" of their own: they call these.
//!
//! * [`glyph`] picks the one glyph in a row's status cell,
//! * [`tooltip_lines`] says the same in words,
//! * [`totals`] and [`rollup`] count for the summary strip and for collapsed
//!   branches,
//! * [`visible`] is `filter_and_sort_tasks` plus the strip's filter and the
//!   "waiting longest" order over [`NodeStanding::since`].

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Local};
use tod_core::next_actor::{
    ExternalWait, NextActor, NodeStanding, WaitKind, Work, YouReason,
};
use tod_core::runner_status::format_elapsed;
use tod_core::task::model::{
    ListWorkingSet, SortKey, TaskItem, filter_and_sort_tasks,
};

/// The four ways a node can be placed for the summary strip and its filters
/// (the persisted [`ActorBucket`]). A node that is `Done` (or has no
/// standing) is in none.
pub use tod_core::task::model::ActorBucket as Bucket;

/// Which bucket `standing` is in.
pub fn bucket_of(standing: &NodeStanding) -> Option<Bucket> {
    match &standing.actor {
        NextActor::Done => None,
        NextActor::Working { .. } => Some(Bucket::Running),
        NextActor::Others { .. } => Some(Bucket::OnOthers),
        NextActor::You { .. } if standing.deferred.is_some() => Some(Bucket::Deferred),
        NextActor::You { .. } => Some(Bucket::OnMe),
    }
}

/// A failure: the user has to act because something broke or stopped, not
/// because something was asked. Counted apart from requests so one cannot
/// hide among the others.
pub fn is_failure(standing: &NodeStanding) -> bool {
    matches!(
        &standing.actor,
        NextActor::You { why: YouReason::Failed { .. } | YouReason::Stopped { .. }, .. }
    )
}

/// An `Others` wait that is itself in trouble: retrying, or overdue.
pub fn is_others_trouble(standing: &NodeStanding) -> bool {
    matches!(&standing.actor, NextActor::Others { wait } if wait.retry.is_some() || wait.overdue)
}

/// Counts of nodes by actor.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// On me, because something is asked (a request, a permission, paused,
    /// not started).
    pub requests: usize,
    /// On me, because something failed or stopped.
    pub failures: usize,
    pub on_others: usize,
    /// Of `on_others`: retrying or overdue.
    pub others_trouble: usize,
    pub running: usize,
    pub deferred: usize,
}

impl Counts {
    pub fn on_me(&self) -> usize {
        self.requests + self.failures
    }

    pub fn of(&self, bucket: Bucket) -> usize {
        match bucket {
            Bucket::OnMe => self.on_me(),
            Bucket::OnOthers => self.on_others,
            Bucket::Running => self.running,
            Bucket::Deferred => self.deferred,
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Counts::default()
    }

    fn add(&mut self, standing: &NodeStanding) {
        match bucket_of(standing) {
            None => {}
            Some(Bucket::OnMe) if is_failure(standing) => self.failures += 1,
            Some(Bucket::OnMe) => self.requests += 1,
            Some(Bucket::OnOthers) => {
                self.on_others += 1;
                if is_others_trouble(standing) {
                    self.others_trouble += 1;
                }
            }
            Some(Bucket::Running) => self.running += 1,
            Some(Bucket::Deferred) => self.deferred += 1,
        }
    }

    fn merge(&mut self, other: &Counts) {
        self.requests += other.requests;
        self.failures += other.failures;
        self.on_others += other.on_others;
        self.others_trouble += other.others_trouble;
        self.running += other.running;
        self.deferred += other.deferred;
    }
}

/// The summary strip's counts: every node once.
pub fn totals(tasks: &[TaskItem], standings: &HashMap<String, NodeStanding>) -> Counts {
    let mut counts = Counts::default();
    for task in tasks {
        if let Some(standing) = standings.get(&task.id) {
            counts.add(standing);
        }
    }
    counts
}

/// The counts of everything below each node (not the node itself, which has
/// its own glyph), so a collapsed branch says whether anything under it is on
/// me. Only nodes with something below to count appear.
pub fn rollup(tasks: &[TaskItem], standings: &HashMap<String, NodeStanding>) -> HashMap<String, Counts> {
    let parents: HashMap<&str, Option<&str>> = tasks
        .iter()
        .map(|t| (t.id.as_str(), t.parent_id.as_deref()))
        .collect();
    let mut out: HashMap<String, Counts> = HashMap::new();
    for task in tasks {
        let Some(standing) = standings.get(&task.id) else {
            continue;
        };
        let mut own = Counts::default();
        own.add(standing);
        if own.is_empty() {
            continue;
        }
        let mut cur = task.parent_id.as_deref();
        let mut guard = 0;
        while let Some(id) = cur {
            out.entry(id.to_string()).or_default().merge(&own);
            cur = parents.get(id).copied().flatten();
            guard += 1;
            if guard > 10_000 {
                break;
            }
        }
    }
    out
}

/// Ids of the nodes in `bucket` and every ancestor of one, so the filtered
/// tree keeps its context.
fn matching_with_ancestors(
    tasks: &[TaskItem],
    standings: &HashMap<String, NodeStanding>,
    bucket: Bucket,
) -> HashSet<String> {
    let by_id: HashMap<&str, &TaskItem> = tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut ids = HashSet::new();
    for task in tasks {
        if standings.get(&task.id).and_then(bucket_of) != Some(bucket) {
            continue;
        }
        let mut cur = Some(task);
        while let Some(t) = cur {
            if !ids.insert(t.id.clone()) {
                break;
            }
            cur = t.parent_id.as_deref().and_then(|p| by_id.get(p).copied());
        }
    }
    ids
}

/// What the tree knows about who is next. The bucket it shows is the working
/// set's `actor_filter`, which is persisted.
#[derive(Debug, Default, Clone)]
pub struct ActorView {
    pub standings: HashMap<String, NodeStanding>,
}

/// The rows the strip's filter leaves: the nodes in the chosen bucket and
/// their ancestors, or all of them with no filter.
pub fn restrict(
    tasks: &[TaskItem],
    standings: &HashMap<String, NodeStanding>,
    filter: Option<Bucket>,
) -> Vec<TaskItem> {
    match filter {
        // Nothing reported yet (the app is starting): a filter restored from
        // the last run would hide every row until the first report.
        Some(_) if standings.is_empty() => tasks.to_vec(),
        Some(bucket) => {
            let keep = matching_with_ancestors(tasks, standings, bucket);
            tasks.iter().filter(|t| keep.contains(&t.id)).cloned().collect()
        }
        None => tasks.to_vec(),
    }
}

/// `filter_and_sort_tasks` with the strip's filter applied, and with the
/// "waiting longest" sort ordering by the standing's `since` rather than by
/// request age: the nodes on me first (or, while a bucket is chosen, that
/// bucket), longest-waiting first.
pub fn visible(
    tasks: &[TaskItem],
    search: &str,
    ws: &ListWorkingSet,
    view: &ActorView,
) -> Vec<TaskItem> {
    let mut base = restrict(tasks, &view.standings, ws.actor_filter);
    if ws.sort_key != SortKey::WaitingLongest {
        return filter_and_sort_tasks(&base, search, ws);
    }
    // The model sorts by `needs_you_count` and `waiting_since`, which count
    // requests only. Stamp the actor's own onto a copy for the sort.
    for task in &mut base {
        let standing = view.standings.get(&task.id);
        let waits = standing.is_some_and(|s| match ws.actor_filter {
            Some(bucket) => bucket_of(s) == Some(bucket),
            None => s.on_you(),
        });
        match standing.and_then(|s| s.since()).filter(|_| waits) {
            Some(since) => {
                task.needs_you_count = task.needs_you_count.max(1);
                task.waiting_since = DateTime::from_timestamp_millis(since);
            }
            None => {
                task.needs_you_count = 0;
                task.waiting_since = None;
            }
        }
    }
    let originals: HashMap<&str, &TaskItem> = tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    filter_and_sort_tasks(&base, search, ws)
        .into_iter()
        .map(|t| originals.get(t.id.as_str()).map_or(t, |o| (*o).clone()))
        .collect()
}

// ---------------------------------------------------------------- glyphs

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// The processor is working: today's ring.
    Ring,
    /// On me: today's filled triangle.
    Triangle,
    /// On others: an outlined diamond.
    Diamond,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Running,
    NeedsYou,
    /// Failed, stopped, overdue.
    Failure,
    /// Retrying.
    Warning,
    /// Waiting on others, or deferred.
    Muted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    None,
    Cross,
    Clock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    pub shape: Shape,
    pub tone: Tone,
    pub mark: Mark,
}

/// The one glyph for a standing; `None` for a done node.
pub fn glyph(standing: &NodeStanding) -> Option<Glyph> {
    let (shape, tone, mark) = match &standing.actor {
        NextActor::Done => return None,
        NextActor::Working { .. } => (Shape::Ring, Tone::Running, Mark::None),
        NextActor::You { .. } if standing.deferred.is_some() => {
            (Shape::Triangle, Tone::Muted, Mark::Clock)
        }
        NextActor::You { why: YouReason::Failed { .. } | YouReason::Stopped { .. }, .. } => {
            (Shape::Triangle, Tone::Failure, Mark::Cross)
        }
        NextActor::You { .. } => (Shape::Triangle, Tone::NeedsYou, Mark::None),
        NextActor::Others { wait } if wait.overdue => (Shape::Diamond, Tone::Failure, Mark::None),
        NextActor::Others { wait } if wait.retry.is_some() => {
            (Shape::Diamond, Tone::Warning, Mark::None)
        }
        NextActor::Others { .. } => (Shape::Diamond, Tone::Muted, Mark::None),
    };
    Some(Glyph { shape, tone, mark })
}

// -------------------------------------------------------------- tooltips

fn work_label(work: &Work) -> String {
    match work {
        Work::Conversation(protocol) => format!("a {} turn", protocol.as_str()),
        Work::GateCheck => "a gate check".to_string(),
        Work::IncomingCheck => "an incoming-changes check".to_string(),
        Work::Runner => "the autopilot run".to_string(),
        Work::Other => "background processing".to_string(),
    }
}

pub fn wait_kind_label(kind: WaitKind) -> &'static str {
    match kind {
        WaitKind::HumanReview => "A person's review",
        WaitKind::BotReview => "A review bot's review",
        WaitKind::Checks => "The checks",
        WaitKind::Merge => "The merge",
        WaitKind::Release => "A release",
        WaitKind::Other => "Something outside",
    }
}

pub fn you_reason_label(why: &YouReason) -> String {
    match why {
        YouReason::Request { count: 1 } => "1 request to answer".to_string(),
        YouReason::Request { count } => format!("{count} requests to answer"),
        YouReason::Permission { title } => format!("a permission is asked: {title}"),
        YouReason::Failed { error } => format!("failed: {error}"),
        YouReason::Stopped { reason } => format!("stopped: {reason}"),
        YouReason::Paused => "paused".to_string(),
        YouReason::NotStarted => "not started".to_string(),
    }
}

/// "retrying 2 of 5: <error>".
pub fn retry_line(wait: &ExternalWait) -> Option<String> {
    wait.retry
        .as_ref()
        .map(|r| format!("retrying {} of {}: {}", r.attempt, r.max, r.error))
}

/// When a deferral returns, in words.
pub fn deferral_line(until: Option<i64>) -> String {
    match until.and_then(DateTime::from_timestamp_millis) {
        Some(at) => format!("deferred until {}", at.with_timezone(&Local).format("%a %b %-d, %H:%M")),
        None => "deferred until you bring it back".to_string(),
    }
}

/// The short form at a deferred row's edge: "back Fri Oct 24, 09:00".
pub fn deferral_chip_text(until: Option<i64>) -> String {
    match until.and_then(DateTime::from_timestamp_millis) {
        Some(at) => format!("back {}", at.with_timezone(&Local).format("%a %b %-d, %H:%M")),
        None => "until I bring it back".to_string(),
    }
}

fn since_line(since: i64, now_ms: i64) -> String {
    format!("for {}", format_elapsed(now_ms - since))
}

/// The row's hover, in words: the actor, the reason, who or what, and age.
pub fn tooltip_lines(standing: &NodeStanding, now_ms: i64) -> Vec<String> {
    match &standing.actor {
        NextActor::Done => Vec::new(),
        NextActor::Working { what, since } => vec![
            "Working: nothing for you to do".to_string(),
            format!("{}, {}", work_label(what), since_line(*since, now_ms)),
        ],
        NextActor::You { why, since } => {
            let mut lines = vec![format!("On you: {}", you_reason_label(why))];
            lines.push(format!("Waiting {}", format_elapsed(now_ms - since)));
            if let Some(d) = &standing.deferred {
                lines.push(deferral_line(d.until));
            }
            lines
        }
        NextActor::Others { wait } => {
            let mut lines = vec![format!("On others: {}", wait_kind_label(wait.kind))];
            if let Some(who) = &wait.who {
                lines.push(format!("From {who}"));
            }
            if let Some(detail) = &wait.detail {
                lines.push(detail.clone());
            }
            lines.push(format!("Waiting {}", format_elapsed(now_ms - wait.since)));
            if let Some(line) = retry_line(wait) {
                lines.push(line);
            }
            if wait.overdue {
                lines.push("check overdue".to_string());
            } else if let Some(next) = wait.next_check_ms {
                let line = if next > now_ms {
                    format!("next check in {}", format_elapsed(next - now_ms))
                } else {
                    "next check is due".to_string()
                };
                lines.push(line);
            }
            lines
        }
    }
}

// ------------------------------------------------------- the runner line

/// How a piece of the task panel's runner line is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartTone {
    Muted,
    /// A failure, an error text, an overdue check: the failure color.
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerPart {
    pub text: String,
    pub tone: PartTone,
}

fn part(text: impl Into<String>) -> RunnerPart {
    RunnerPart { text: text.into(), tone: PartTone::Muted }
}

fn error_part(text: impl Into<String>) -> RunnerPart {
    RunnerPart { text: text.into(), tone: PartTone::Error }
}

/// The task panel's runner line says who is next in these pieces: the actor,
/// the reason, who or what, the age, the next check, the retry count and the
/// error. Rendered from the same [`NodeStanding`] as the tree.
pub fn runner_parts(standing: &NodeStanding, now_ms: i64) -> Vec<RunnerPart> {
    let age = |since: i64| format!("{}", format_elapsed(now_ms - since));
    match &standing.actor {
        NextActor::Done => vec![part("done")],
        NextActor::Working { what, since } => {
            vec![part(format!("working: {}", work_label(what))), part(age(*since))]
        }
        NextActor::You { why, since } => {
            let mut parts = Vec::new();
            match why {
                YouReason::Failed { error } => {
                    parts.push(error_part("failed"));
                    parts.push(error_part(error.clone()));
                }
                YouReason::Stopped { reason } => {
                    parts.push(error_part("stopped"));
                    parts.push(error_part(reason.clone()));
                }
                other => parts.push(part(format!("on you: {}", you_reason_label(other)))),
            }
            parts.push(part(format!("waiting {}", age(*since))));
            if let Some(d) = &standing.deferred {
                parts.push(part(deferral_line(d.until)));
            }
            parts
        }
        NextActor::Others { wait } => {
            let mut head = format!("waiting on others: {}", wait_kind_label(wait.kind).to_lowercase());
            if let Some(who) = &wait.who {
                head.push_str(&format!(" ({who})"));
            }
            let mut parts = vec![part(head)];
            if let Some(detail) = &wait.detail {
                parts.push(part(detail.clone()));
            }
            parts.push(part(format!("waiting {}", age(wait.since))));
            if let Some(line) = retry_line(wait) {
                parts.push(error_part(line));
            }
            if wait.overdue {
                parts.push(error_part("check overdue"));
            } else if let Some(next) = wait.next_check_ms.and_then(DateTime::from_timestamp_millis) {
                parts.push(part(format!(
                    "next check {}",
                    next.with_timezone(&Local).format("%a %H:%M")
                )));
            }
            parts
        }
    }
}

// ------------------------------------------------------------ strip text

/// The strip's words for the counts: "3 on you · 1 failed · 5 on others ·
/// 2 running", leaving out what is zero.
pub fn strip_text(counts: &Counts) -> String {
    let mut parts = Vec::new();
    if counts.requests > 0 {
        parts.push(format!("{} on you", counts.requests));
    }
    if counts.failures > 0 {
        parts.push(format!("{} failed", counts.failures));
    }
    if counts.on_others > 0 {
        parts.push(format!("{} on others", counts.on_others));
    }
    if counts.running > 0 {
        parts.push(format!("{} running", counts.running));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tod_core::next_actor::{Deferral, Retry};

    fn standing(actor: NextActor) -> NodeStanding {
        NodeStanding { node_id: uuid::Uuid::new_v4(), actor, deferred: None }
    }

    fn you(why: YouReason, since: i64) -> NodeStanding {
        standing(NextActor::You { why, since })
    }

    fn others(wait: ExternalWait) -> NodeStanding {
        standing(NextActor::Others { wait })
    }

    fn wait(since: i64) -> ExternalWait {
        ExternalWait {
            kind: WaitKind::HumanReview,
            who: Some("maria".into()),
            detail: Some("review requested".into()),
            since,
            next_check_ms: None,
            overdue: false,
            retry: None,
        }
    }

    fn working(since: i64) -> NodeStanding {
        standing(NextActor::Working { what: Work::Runner, since })
    }

    fn request(since: i64) -> NodeStanding {
        you(YouReason::Request { count: 1 }, since)
    }

    fn deferred(mut s: NodeStanding) -> NodeStanding {
        s.deferred = Some(Deferral { until: None, deferred_at: 0 });
        s
    }

    fn tasks(spec: &[(&str, Option<&str>)]) -> Vec<TaskItem> {
        let mut tasks = crate::views::task_list::fixtures::large_fixture_set(spec.len());
        for (i, (task, (id, parent))) in tasks.iter_mut().zip(spec).enumerate() {
            task.id = id.to_string();
            task.parent_id = parent.map(str::to_string);
            task.tree_ordinal = i;
        }
        tasks
    }

    fn map(entries: Vec<(&str, NodeStanding)>) -> HashMap<String, NodeStanding> {
        entries.into_iter().map(|(id, s)| (id.to_string(), s)).collect()
    }

    #[test]
    fn buckets() {
        assert_eq!(bucket_of(&working(1)), Some(Bucket::Running));
        assert_eq!(bucket_of(&request(1)), Some(Bucket::OnMe));
        assert_eq!(bucket_of(&deferred(request(1))), Some(Bucket::Deferred));
        assert_eq!(bucket_of(&others(wait(1))), Some(Bucket::OnOthers));
        assert_eq!(bucket_of(&standing(NextActor::Done)), None);
    }

    #[test]
    fn glyph_per_actor_and_variant() {
        use Mark as M;
        use Shape as S;
        use Tone as T;
        let g = |s: &NodeStanding| glyph(s).map(|g| (g.shape, g.tone, g.mark));
        assert_eq!(g(&working(1)), Some((S::Ring, T::Running, M::None)));
        assert_eq!(g(&request(1)), Some((S::Triangle, T::NeedsYou, M::None)));
        assert_eq!(g(&you(YouReason::NotStarted, 1)), Some((S::Triangle, T::NeedsYou, M::None)));
        assert_eq!(
            g(&you(YouReason::Failed { error: "x".into() }, 1)),
            Some((S::Triangle, T::Failure, M::Cross))
        );
        assert_eq!(
            g(&you(YouReason::Stopped { reason: "budget".into() }, 1)),
            Some((S::Triangle, T::Failure, M::Cross))
        );
        assert_eq!(g(&deferred(request(1))), Some((S::Triangle, T::Muted, M::Clock)));
        // Deferred wins over failed: it is off the list either way.
        assert_eq!(
            g(&deferred(you(YouReason::Failed { error: "x".into() }, 1))),
            Some((S::Triangle, T::Muted, M::Clock))
        );
        assert_eq!(g(&others(wait(1))), Some((S::Diamond, T::Muted, M::None)));
        let mut retrying = wait(1);
        retrying.retry = Some(Retry { attempt: 2, max: 5, error: "503".into() });
        assert_eq!(g(&others(retrying.clone())), Some((S::Diamond, T::Warning, M::None)));
        retrying.overdue = true;
        assert_eq!(g(&others(retrying)), Some((S::Diamond, T::Failure, M::None)));
        assert_eq!(g(&standing(NextActor::Done)), None);
    }

    #[test]
    fn counts_separate_failures_from_requests() {
        let t = tasks(&[("a", None), ("b", None), ("c", None), ("d", None), ("e", None), ("f", None)]);
        let mut retrying = wait(1);
        retrying.retry = Some(Retry { attempt: 1, max: 5, error: "x".into() });
        let s = map(vec![
            ("a", request(1)),
            ("b", you(YouReason::Failed { error: "x".into() }, 1)),
            ("c", others(retrying)),
            ("d", working(1)),
            ("e", deferred(request(1))),
            ("f", standing(NextActor::Done)),
        ]);
        let counts = totals(&t, &s);
        assert_eq!(
            counts,
            Counts { requests: 1, failures: 1, on_others: 1, others_trouble: 1, running: 1, deferred: 1 }
        );
        assert_eq!(counts.on_me(), 2);
        assert_eq!(strip_text(&counts), "1 on you · 1 failed · 1 on others · 1 running");
    }

    #[test]
    fn strip_text_omits_zeroes() {
        assert_eq!(strip_text(&Counts::default()), "");
        assert_eq!(strip_text(&Counts { running: 2, ..Default::default() }), "2 running");
    }

    #[test]
    fn rollup_counts_a_branch_under_its_ancestors() {
        let t = tasks(&[("root", None), ("mid", Some("root")), ("leaf", Some("mid")), ("other", None)]);
        let s = map(vec![("leaf", request(1)), ("mid", working(1))]);
        let r = rollup(&t, &s);
        // Below themselves nothing: a node's own state is its glyph.
        assert!(!r.contains_key("leaf"));
        assert_eq!(r["mid"].on_me(), 1);
        assert_eq!(r["mid"].running, 0);
        assert_eq!(r["root"].on_me(), 1);
        assert_eq!(r["root"].running, 1);
        assert!(!r.contains_key("other"));
        // Totals count each node once, however deep.
        assert_eq!(totals(&t, &s).on_me(), 1);
    }

    #[test]
    fn filter_keeps_ancestors_and_drops_the_rest() {
        let t = tasks(&[("root", None), ("a", Some("root")), ("b", Some("root")), ("other", None)]);
        let s = map(vec![("a", request(1)), ("b", others(wait(1)))]);
        let ids = |filter| {
            visible(
                &t,
                "",
                &ListWorkingSet { actor_filter: filter, ..ListWorkingSet::default_sort() },
                &ActorView { standings: s.clone() },
            )
            .into_iter()
            .map(|t| t.id)
            .collect::<Vec<_>>()
        };
        assert_eq!(ids(None), vec!["root", "a", "b", "other"]);
        assert_eq!(ids(Some(Bucket::OnMe)), vec!["root", "a"]);
        assert_eq!(ids(Some(Bucket::OnOthers)), vec!["root", "b"]);
        assert!(ids(Some(Bucket::Deferred)).is_empty());
    }

    #[test]
    fn waiting_longest_orders_by_the_standing_since_and_keeps_the_rows_untouched() {
        let t = tasks(&[("a", None), ("b", None), ("c", None), ("d", None)]);
        let s = map(vec![
            ("a", request(300)),
            ("b", you(YouReason::Failed { error: "x".into() }, 100)),
            ("c", others(wait(50))),
            ("d", you(YouReason::NotStarted, 200)),
        ]);
        let ws = ListWorkingSet {
            sort_key: SortKey::WaitingLongest,
            ..ListWorkingSet::default_sort()
        };
        let out = visible(&t, "", &ws, &ActorView { standings: s.clone() });
        // On me, oldest first; the rest follow in tree order.
        assert_eq!(out.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["b", "d", "a", "c"]);
        // The stamped sort values are not left on the rows.
        assert!(out.iter().all(|t| t.needs_you_count == 0 && t.waiting_since.is_none()));
        // Within a chosen bucket the same rule orders it.
        let s2 = map(vec![("a", others(wait(30))), ("b", others(wait(10))), ("c", others(wait(20)))]);
        let ws = ListWorkingSet { actor_filter: Some(Bucket::OnOthers), ..ws };
        let out = visible(&t, "", &ws, &ActorView { standings: s2 });
        assert_eq!(out.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["b", "c", "a"]);
    }

    #[test]
    fn tooltips_say_actor_reason_who_and_age() {
        let now = 10 * 3_600_000;
        let lines = tooltip_lines(&request(now - 2 * 3_600_000), now);
        assert_eq!(lines, ["On you: 1 request to answer", "Waiting 2h"]);

        let lines = tooltip_lines(&deferred(request(now - 60_000)), now);
        assert_eq!(lines.last().unwrap(), "deferred until you bring it back");

        let mut w = wait(now - 3_600_000);
        w.next_check_ms = Some(now + 120_000);
        let lines = tooltip_lines(&others(w.clone()), now);
        assert_eq!(
            lines,
            [
                "On others: A person's review",
                "From maria",
                "review requested",
                "Waiting 1h",
                "next check in 2m"
            ]
        );

        w.retry = Some(Retry { attempt: 2, max: 5, error: "GitHub 503".into() });
        w.overdue = true;
        let lines = tooltip_lines(&others(w), now);
        assert!(lines.contains(&"retrying 2 of 5: GitHub 503".to_string()));
        assert!(lines.contains(&"check overdue".to_string()));

        let lines = tooltip_lines(&working(now - 30_000), now);
        assert_eq!(lines[0], "Working: nothing for you to do");
        assert!(tooltip_lines(&standing(NextActor::Done), now).is_empty());
    }

    #[test]
    fn deferral_chip_text_names_the_return_time() {
        assert_eq!(deferral_chip_text(None), "until I bring it back");
        assert!(deferral_chip_text(Some(1_700_000_000_000)).starts_with("back "));
    }

    #[test]
    fn runner_line_parts_carry_reason_detail_next_check_retry_and_error() {
        let now = 3_600_000;
        let parts = runner_parts(&you(YouReason::Failed { error: "auth failed".into() }, 0), now);
        assert_eq!(parts[0], error_part("failed"));
        assert_eq!(parts[1], error_part("auth failed"));
        assert_eq!(parts[2], part("waiting 1h"));

        let mut w = wait(0);
        w.retry = Some(Retry { attempt: 2, max: 5, error: "503".into() });
        w.next_check_ms = Some(now + 60_000);
        let parts = runner_parts(&others(w.clone()), now);
        assert_eq!(parts[0].text, "waiting on others: a person's review (maria)");
        assert!(parts.contains(&part("review requested")));
        assert!(parts.contains(&error_part("retrying 2 of 5: 503")));
        assert!(parts.iter().any(|p| p.text.starts_with("next check ")));
        w.overdue = true;
        let parts = runner_parts(&others(w), now);
        assert!(parts.contains(&error_part("check overdue")));
        assert!(!parts.iter().any(|p| p.text.starts_with("next check ")));

        assert_eq!(runner_parts(&working(0), now)[0], part("working: the autopilot run"));
        assert_eq!(runner_parts(&standing(NextActor::Done), now), vec![part("done")]);
        let parts = runner_parts(&deferred(request(0)), now);
        assert_eq!(parts.last().unwrap().text, "deferred until you bring it back");
    }

    #[test]
    fn failure_reasons_carry_their_error() {
        assert_eq!(
            you_reason_label(&YouReason::Failed { error: "auth failed".into() }),
            "failed: auth failed"
        );
        assert_eq!(you_reason_label(&YouReason::Request { count: 3 }), "3 requests to answer");
    }
}
