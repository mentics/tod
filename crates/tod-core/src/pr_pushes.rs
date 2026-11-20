//! Nothing reaches the pull request unreviewed (`doc/lifecycle/find-it-first.md`,
//! section 8): the heads a node's pull request moves to, checked against what
//! tod's own review cleared and what tod itself pushed.
//!
//! With the review steps on in `pr_draft` and `pr_review` (`crate::pr_draft_review`),
//! every change is reviewed on the local head before the pull request agent is told
//! to push it. A head that reaches the pull request any other way (an agent
//! that pushed when told only to commit, the user's conversation told to "push
//! now", a push from another checkout) is a defect in the process. So around
//! every agent turn the autopilot runs on a node in `pr_draft` or `pr_review`,
//! and when a conversation the user directs on such a node ends, [`check`]
//! reads each linked pull request's head (one uncached look,
//! `PrFeed::pushed_head`) and records each new one (`tod_store::pr_pushes`):
//! the first seen in a `pr_draft` stay as its baseline, one tod expected
//! (`expected`: the push turn's local head, GitHub's Update branch) or one its
//! review cleared as accounted for, and anything else as **unreviewed**, with
//! the turn it appeared around, a failed note in the node's Activity, and the
//! journey's `DataChanged` row. In `pr_draft` the autopilot then reviews that
//! head at once ([`review_owed_now`]); there is no `pre-push` hook.
//!
//! A conversation the user directs commits on the node's branch rather than
//! pushing (`workspace/codebase.md`, "Pushing"). [`after_conversation`] finds
//! those commits ([`held_commits`]), tells the user they are reviewed before
//! they are pushed, and wakes the node's run, whose `pr_draft` or `pr_review`
//! loop adopts them as a change awaiting review (`Autopilot::babysit`), so tod's Review →
//! Fix loop and the checks run on them before the pull request agent pushes.

use crate::conversation::protocol::RunNotice;
use crate::pr_readiness::{LivePr, PrFeed, pr_label};
use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;
use tod_store::conversation::ProtocolKind;
use tod_store::fleet::{FleetMutation, FleetStore, Workdir};
use tod_store::github::NodePr;
use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};
use tod_store::outline::uuid_blob::now_ms;
use tod_store::phase::PhaseRepo;
use tod_store::pr_pushes::{
    PrPush, PrPushRepo, VERDICT_BASELINE, VERDICT_REVIEWED, VERDICT_TOD, VERDICT_UNREVIEWED,
};
use uuid::Uuid;

/// The states whose pull request is watched.
pub const STATES: [&str; 2] = ["pr_draft", "pr_review"];

/// A commit id, shortened for a message.
pub fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// The node's state when its pull request's heads are watched: in
/// [`STATES`], with the review steps on (with them off nothing is reviewed
/// before it is pushed, so nothing could be told apart) and a linked pull
/// request. `None` otherwise.
pub fn watched(conn: &Connection, node: Uuid) -> Result<Option<(String, Vec<NodePr>)>> {
    let Some(state) = tod_store::outline::repos::NodeRepo::new(conn).get_lifecycle(node)? else {
        return Ok(None);
    };
    if !STATES.contains(&state.as_str()) || !crate::pr_draft_review::enabled(conn, node)? {
        return Ok(None);
    }
    let prs = tod_store::github::NodePrRepo::new(conn).read(node)?.prs;
    Ok((!prs.is_empty()).then_some((state, prs)))
}

/// Reads each linked pull request's head now and records each one not seen
/// before (see the module docs). `around` is the conversation whose turn just
/// ran (`None`: before a turn, or outside any); `expected` the heads tod
/// itself had pushed. Returns the unreviewed ones it recorded. Nothing when
/// the node is not [`watched`]. The inner `Err` is a failed look at GitHub,
/// naming the pull request whose head could not be read (the caller's to
/// retry); the outer one a failure of the store.
pub fn check(
    fleet: &FleetStore,
    feed: &dyn PrFeed,
    node: Uuid,
    around: Option<(Uuid, ProtocolKind)>,
    expected: &[String],
) -> Result<std::result::Result<Vec<PrPush>, String>> {
    let Some((state, prs)) = fleet.read(|conn| watched(conn, node))? else {
        return Ok(Ok(Vec::new()));
    };
    let mut unreviewed = Vec::new();
    let mut wrote = false;
    for pr in &prs {
        let label = pr_label(pr);
        let head = match feed.pushed_head(pr) {
            Ok(head) => head,
            Err(err) => {
                if wrote {
                    fleet.writer().flush()?;
                }
                return Ok(Err(format!("reading the head of {}: {err}", pr.url)));
            }
        };
        let Some(head) = head else { continue };
        let (latest, stay, cleared) = fleet.read(|conn| {
            Ok((
                PrPushRepo::new(conn).latest(node, &label)?,
                PhaseRepo::new(conn).stay_started_at(node)?,
                crate::pr_draft_review::cleared_shas(conn, node, state == "pr_review")?,
            ))
        })?;
        let new_stay = state == crate::pr_draft_review::STATE
            && latest.as_ref().is_some_and(|l| stay.is_some_and(|stay| l.seen_at < stay));
        if latest.as_ref().is_some_and(|l| l.head == head) && !new_stay {
            continue;
        }
        let verdict = if latest.is_none() || new_stay {
            VERDICT_BASELINE
        } else if expected.contains(&head) {
            VERDICT_TOD
        } else if cleared.contains(&head) {
            VERDICT_REVIEWED
        } else {
            VERDICT_UNREVIEWED
        };
        let push = PrPush {
            id: Uuid::new_v4(),
            node_id: node,
            pr: label.clone(),
            head: head.clone(),
            previous: latest.map(|l| l.head),
            verdict: verdict.to_string(),
            lifecycle: state.clone(),
            conversation_id: around.map(|(id, _)| id),
            protocol: around.map(|(_, kind)| kind.as_str().to_string()),
            seen_at: now_ms(),
        };
        let note = push.is_unreviewed().then(|| Box::new(note_for(&push, &pr.url)));
        if push.is_unreviewed() {
            tracing::warn!(node = %node, pr = %label, head = %head, around = %push.around(), "unreviewed push to the pull request");
            unreviewed.push(push.clone());
        }
        fleet.enqueue(FleetMutation::RecordPrPush { push: Box::new(push), note })?;
        wrote = true;
    }
    if wrote {
        fleet.writer().flush()?;
    }
    Ok(Ok(unreviewed))
}

/// The failed note an unreviewed push leaves in the node's Activity.
fn note_for(push: &PrPush, url: &str) -> TimelineEntry {
    let from = push.previous.as_deref().map(short).unwrap_or("unknown");
    let then = if push.lifecycle == crate::pr_draft_review::STATE {
        "tod reviews that head now."
    } else {
        "In pr_review tod reviews a change before the pull request agent pushes it, but does not review a head that \
         reached the pull request another way: move the node back to pr_draft to have tod review it."
    };
    TimelineEntry::new(
        push.node_id,
        KIND_NOTE,
        &push.lifecycle,
        format!("Unreviewed push to {}", push.pr),
        true,
        format!(
            "{url} moved from {from} to {} {}, and tod's review had not cleared {}: a push outside the reviewed flow, \
             where a change is committed, reviewed and checked, and only then pushed by the pull request agent. \
             It is a process failure, counted when the node learns. {then}",
            short(&push.head),
            push.around(),
            short(&push.head),
        ),
    )
}

/// The unreviewed pushes still at their pull request's head (`live`) that no
/// own review in this `pr_draft` stay has reviewed yet: what the autopilot
/// reviews at once. Empty outside `pr_draft` or with the review steps off.
pub fn review_owed_now(conn: &Connection, node: Uuid, live: &[LivePr]) -> Result<Vec<PrPush>> {
    if !crate::pr_draft_review::enabled(conn, node)? {
        return Ok(Vec::new());
    }
    let stay = PhaseRepo::new(conn).stay_started_at(node)?;
    let reviewed = match crate::pr_draft_review::latest_review(conn, node)? {
        Some(review) => crate::pr_draft_review::reviewed_heads(conn, review.id)?.unwrap_or_default(),
        None => Default::default(),
    };
    let current = crate::pr_draft_review::current_heads(live);
    Ok(PrPushRepo::new(conn)
        .unreviewed(node)?
        .into_iter()
        .filter(|p| p.lifecycle == crate::pr_draft_review::STATE && stay.is_none_or(|stay| p.seen_at >= stay))
        .filter(|p| current.get(&p.pr) == Some(&p.head) && reviewed.get(&p.pr) != Some(&p.head))
        .collect())
}

/// Whether the checkout `cwd` holds commits on top of the pull request's
/// head `head` (its repository's own checkout, [`crate::pr_draft_review::repo_dir`]):
/// `head` is there and an ancestor of a different `HEAD`. A checkout that does
/// not have `head` (someone pushed from elsewhere) holds nothing to push.
/// Returns the local head when it does.
pub fn held_commits(cwd: &Workdir, pr: &NodePr, head: &str) -> Result<Option<String>> {
    let dir = crate::pr_draft_review::repo_dir(cwd, &pr.owner, &pr.repo)?;
    let local = dir.git(&["rev-parse", "HEAD"]).with_context(|| format!("reading the head commit in {dir}"))?;
    if local == head {
        return Ok(None);
    }
    let object = format!("{head}^{{commit}}");
    let has = dir.git_output(&["cat-file", "-e", &object])?;
    if !has.status.success() {
        return Ok(None);
    }
    let ancestor = dir.git_output(&["merge-base", "--is-ancestor", head, "HEAD"])?;
    match ancestor.status.code() {
        Some(0) => Ok(Some(local)),
        Some(1) => Ok(None),
        _ => anyhow::bail!(
            "git merge-base --is-ancestor {head} HEAD failed in {dir}: {}",
            String::from_utf8_lossy(&ancestor.stderr).trim()
        ),
    }
}

/// The local head when the node's checkout holds commits on top of every
/// linked pull request's head (`live`) that it has; `None` when it holds
/// none, or the checkout is not made (nothing makes it here).
pub fn held_for(fleet: &FleetStore, node: Uuid, live: &[LivePr]) -> Result<Option<String>> {
    let Some(cwd) = tod_store::fleet::provision::launch_cwd_if_made(fleet, &node.to_string())? else {
        return Ok(None);
    };
    let mut held = None;
    for p in live.iter().filter(|p| !p.assessment.merged) {
        let Some(head) = p.head_sha.as_deref() else { continue };
        if let Some(local) = held_commits(&cwd, &p.pr, head)? {
            held = Some(local);
        }
    }
    Ok(held)
}

/// Brings the node's pending waits forward to now, so its run looks again at
/// once (a wait in `pr_draft` or for a person's review).
pub fn wake_run(fleet: &FleetStore, node: Uuid) -> Result<()> {
    let waits = fleet.read(|conn| Ok(tod_store::waits::WaitRepo::new(conn).list_pending_for_node(node)?))?;
    let now = now_ms();
    for wait in waits {
        fleet.interview(
            "autopilot",
            tod_store::interview::InterviewCommand::RescheduleWait { wait_id: wait.id, due_at: now },
        )?;
    }
    Ok(())
}

/// When a conversation the user directs (`kind`) ends on a node whose pull
/// request is [`watched`]: records a head it pushed ([`check`], around this
/// conversation), and finds the commits it made and did not push
/// ([`held_commits`]). Both are told to the user, and wake the node's run,
/// which reviews either before anything more reaches the pull request.
pub fn after_conversation(
    fleet: &FleetStore,
    data_root: &Path,
    conversation_id: Uuid,
    kind: ProtocolKind,
    node: Uuid,
) -> Result<Vec<RunNotice>> {
    let Some((state, prs)) = fleet.read(|conn| watched(conn, node))? else {
        return Ok(Vec::new());
    };
    let feed = crate::pr_readiness::feed_for(data_root).ok_or_else(|| {
        anyhow::anyhow!("GitHub credentials are not set up, so whether this conversation pushed the pull request cannot be told")
    })?;
    let mut notices = Vec::new();
    let pushed = check(fleet, feed.as_ref(), node, Some((conversation_id, kind)), &[])?.map_err(anyhow::Error::msg)?;
    for push in &pushed {
        notices.push(RunNotice::Warning(format!(
            "This conversation pushed {} to {} before tod reviewed it. It is recorded as an unreviewed push{}",
            short(&push.head),
            push.pr,
            if state == crate::pr_draft_review::STATE { ", and tod reviews it now." } else { "." }
        )));
    }
    let mut held = Vec::new();
    if let Some(cwd) = tod_store::fleet::provision::launch_cwd_if_made(fleet, &node.to_string())? {
        for pr in &prs {
            let Some(head) = feed
                .pushed_head(pr)
                .map_err(|err| anyhow::anyhow!("reading the head of {}: {err}", pr.url))?
            else {
                continue;
            };
            if let Some(local) = held_commits(&cwd, pr, &head)? {
                held.push((pr_label(pr), head, local));
            }
        }
    }
    if !held.is_empty() {
        let prs: Vec<&str> = held.iter().map(|(label, _, _)| label.as_str()).collect();
        // In `pr_draft` and `pr_review` alike ([`watched`] needs the review
        // steps on): the run adopts them as a change awaiting review.
        let (title, said) = (
            "Conversation commits held for review",
            format!(
                "This conversation's commits are not on {} yet: tod reviews them first{}, and the pull request agent pushes them once that is clean.",
                prs.join(", "),
                if state == crate::pr_draft_review::STATE { " and runs the checks" } else { "" }
            ),
        );
        let detail = held
            .iter()
            .map(|(label, head, local)| format!("{label}: {} on top of the pushed {}", short(local), short(head)))
            .collect::<Vec<_>>()
            .join("; ");
        let entry = TimelineEntry::new(node, KIND_NOTE, &state, title, false, format!("{said} ({detail})"));
        fleet.enqueue(FleetMutation::RecordTimeline(Box::new(entry)))?;
        fleet.writer().flush()?;
        notices.push(RunNotice::Warning(said));
    }
    if !pushed.is_empty() || !held.is_empty() {
        wake_run(fleet, node)?;
    }
    Ok(notices)
}
