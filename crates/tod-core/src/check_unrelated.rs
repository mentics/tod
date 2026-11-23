//! A failing check the pull request agent says the change did not cause: the
//! one request the user gets for it, and what answering it does. Spec:
//! `doc/lifecycle/pr-readiness.md`, "A failing check that is not the change's".
//!
//! A failing check is agent work (`pr_readiness::Work::FailingChecks`), but a
//! check can fail for a reason the change has nothing to do with: a lock file
//! in a root the change does not touch, a service that is down, a workflow the
//! change only triggers. The agent records that with `tod-cli pr checks
//! unrelated <check> --reason <evidence>`, which raises ONE pending decision
//! on the node (protocol [`PROTOCOL`], reason `broken`) naming the pull
//! request, the check and the agent's evidence, with two options: carry on
//! without the check, or fix it in this pull request.
//!
//! While the question is pending the check is not agent work, so no turn is
//! spent on it and "two turns changed nothing" cannot fire for it. The answer
//! stands on the node, like a bot the user said to go on without
//! ([`crate::bot_silence`]), for that check on that pull request:
//!
//! - **carry on without it**: the check is not work and not a failure for
//!   the babysitter, and the gate's `mergeable` criterion accepts GitHub's
//!   `unstable` when only waived checks fail;
//! - **fix it in this pull request**: the check is work again, the agent is
//!   told the user decided so, and may not say again that it is not the
//!   change's.
//!
//! **Checked, not asked** (`doc/requests/design.md` 4.2.2). A failure the base
//! branch has too ([`tod_store::github::BaseFailure`]) is waived by tod itself
//! ([`settle_confirmed`]): the request is recorded and answered by tod
//! (`decided_by = tod`), with the comparison as its rationale, and the check
//! is noted in the node's Activity and offered as a suggestion
//! ([`file_pre_existing_suggestion`]). An agent's `pr checks unrelated` claim
//! is put through the same comparison: a confirmed one is waived, an
//! unconfirmed one is refused back to the agent as work, with no request to
//! the user. Only a check two quiet agent turns left standing
//! ([`Raised::QuietTurns`]) still asks the user (until triage takes it).
//!
//! A pending question whose check no longer fails (re-run green, fixed by a
//! push) is withdrawn by the autopilot.

use tod_store::decisions::{Decision, DecisionRepo, NewDecision, REASON_BROKEN};
use tod_store::settings::{PrReadinessSettings, UnrelatedCheck, UnrelatedStanding};
use uuid::Uuid;

/// The `protocol` the request is recorded with.
pub const PROTOCOL: &str = "check_unrelated";

/// Recheck a parked CI request even without a GitHub webhook.
pub const RECHECK_MS: i64 = 5 * 60 * 1000;

/// States in which the babysitter can reconcile CI requests.
pub fn babysits_ci(lifecycle: &str) -> bool {
    matches!(lifecycle, "pr_draft" | "pr_review")
}

/// The option that fixes the check in this pull request.
pub const FIX_HERE: &str = "Fix it in this pull request";

const CARRY_ON_PREFIX: &str = "It's not this change's: carry on without \"";

/// What the agent found, for the wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// `owner/repo#number`.
    pub pr: String,
    pub pr_url: String,
    pub check: String,
    /// The head commit the agent judged.
    pub head: String,
    /// How the check fails (`tod_store::github::failure_signature`), empty
    /// when not known: the answer holds for later heads while it fails the
    /// same way.
    pub signature: String,
    /// The evidence: the agent's, as it gave it, or what tod compared
    /// ([`Raised::BaseBranch`]).
    pub reason: String,
    /// Who raised it, which decides the wording.
    pub raised: Raised,
}

/// Who put the question to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Raised {
    /// The pull request's agent (`tod-cli pr checks unrelated`).
    Agent,
    /// tod, because the failure is also on the base branch
    /// (`tod_store::github::BaseFailure`): `reason` is its proof.
    BaseBranch,
    /// tod, because two agent turns in a row changed nothing while the check
    /// failed and the agent never said it was not the change's: the user
    /// decides instead of the run stopping `pr_stuck`.
    QuietTurns,
}

/// The option that carries on without the check. It carries the key (the
/// check and the pull request) that [`key_of`] reads back.
pub fn carry_on_option(pr: &str, check: &str, head: &str, signature: &str) -> String {
    if signature.is_empty() {
        format!("{CARRY_ON_PREFIX}{check}\" ({pr} @ {head})")
    } else {
        format!("{CARRY_ON_PREFIX}{check}\" ({pr} @ {head} ~ {signature})")
    }
}

const BASE_SOURCE: &str = "tod found, without the agent, that the same failure is on the base branch";
const QUIET_SOURCE: &str = "tod raised this itself: the pull request's agent took two turns in a row";

/// Who raised `decision`, read back from its wording.
pub fn raised_of(decision: &Decision) -> Raised {
    if decision.question.contains(BASE_SOURCE) {
        Raised::BaseBranch
    } else if decision.question.contains(QUIET_SOURCE) {
        Raised::QuietTurns
    } else {
        Raised::Agent
    }
}

/// The options, in order: the agent's judgement first.
pub fn options(pr: &str, check: &str, head: &str, signature: &str) -> Vec<String> {
    vec![carry_on_option(pr, check, head, signature), FIX_HERE.to_string()]
}

/// The request's question, in the shape the stance asks for: what to decide
/// first, then why, with the agent's evidence.
pub fn question(claim: &Claim) -> String {
    format!(
        "Decide whether the failing check \"{check}\" on pull request {pr} ({url}) is fixed in that pull request or carried on without.\n\n\
         {source}\n\n\
         Until you answer, tod does not send the agent back for this check. Carrying on without it leaves the check failing on GitHub, and the pull request stays blocked from merging there if the project requires it. {scope}",
        scope = if claim.signature.is_empty() && claim.raised != Raised::Agent {
            format!(
                "Your answer is for commit {} only: if a later push still fails the check, you are asked again.",
                claim.head.chars().take(7).collect::<String>()
            )
        } else if claim.signature.is_empty() {
            format!(
                "Your answer is for commit {} only: if a later push fails the check, the agent must say again that it is not the change's, and a carry-on you gave here is then applied without asking you again.",
                claim.head.chars().take(7).collect::<String>()
            )
        } else {
            "Your answer holds for later commits while the check fails the same way (same check, same failing step and error); you are asked again only if it starts failing differently.".to_string()
        },
        check = claim.check,
        pr = claim.pr,
        url = claim.pr_url,
        source = match claim.raised {
            Raised::BaseBranch => format!(
                "{BASE_SOURCE}, so this change did not cause it. Its evidence: {}",
                claim.reason.trim()
            ),
            Raised::QuietTurns => format!(
                "{QUIET_SOURCE} on this check and changed nothing, and did not say the check is not this change's. {} \
                 If this change did cause it, answer \"{FIX_HERE}\" and tell the agent what to change; if it did not (open the check's log), carry on without it.",
                claim.reason.trim()
            ),
            Raised::Agent => format!("The pull request's agent says this change did not cause it. Its evidence: {}", claim.reason.trim()),
        },
    )
}

/// The decision to record for `claim`.
pub fn new_decision(claim: &Claim) -> NewDecision {
    NewDecision {
        question: question(claim),
        options: options(&claim.pr, &claim.check, &claim.head, &claim.signature),
        evidence: Vec::new(),
        reason: REASON_BROKEN.to_string(),
        moves: Vec::new(),
        ..Default::default()
    }
}

/// The pull request (`owner/repo#number`), check and head commit a
/// check-unrelated decision is about.
pub fn key_of(decision: &Decision) -> Option<(String, String, String)> {
    let (pr, check, head, _) = key_and_signature(decision)?;
    Some((pr, check, head))
}

/// The failure signature a check-unrelated decision was raised for; empty
/// when it was raised without one.
pub fn signature_of(decision: &Decision) -> String {
    key_and_signature(decision).map(|k| k.3).unwrap_or_default()
}

fn key_and_signature(decision: &Decision) -> Option<(String, String, String, String)> {
    if decision.protocol.as_deref() != Some(PROTOCOL) {
        return None;
    }
    let rest = decision.options.first()?.strip_prefix(CARRY_ON_PREFIX)?;
    let (check, pr) = rest.rsplit_once("\" (")?;
    let (pr, head) = pr.strip_suffix(')')?.rsplit_once(" @ ")?;
    let (head, signature) = head.split_once(" ~ ").unwrap_or((head, ""));
    Some((pr.to_string(), check.to_string(), head.to_string(), signature.to_string()))
}

/// `check`'s standing on `pr` at `head` for `node`, from the node's
/// check-unrelated decisions; `None` when never raised for that head (or
/// withdrawn). What was decided for another head does not count.
pub fn standing(
    conn: &rusqlite::Connection,
    node: Uuid,
    pr: &str,
    check: &str,
    head: &str,
    signature: &str,
) -> anyhow::Result<Option<UnrelatedStanding>> {
    Ok(tod_store::settings::unrelated_standing(&standings(conn, node)?, pr, check, head, signature))
}

/// Every standing the node has, one per check, pull request and head: a
/// pending question, else the latest answer.
pub fn standings(conn: &rusqlite::Connection, node: Uuid) -> anyhow::Result<Vec<UnrelatedCheck>> {
    let repo = DecisionRepo::new(conn);
    // (pr, check, head) -> (when, standing); a pending question is "now".
    let mut found: Vec<(UnrelatedCheck, i64)> = Vec::new();
    for decision in repo.list_for_node(node)? {
        let Some((pr, check, head, signature)) = key_and_signature(&decision) else { continue };
        let (standing, at) = match decision.status.as_str() {
            "pending" => (UnrelatedStanding::Pending, i64::MAX),
            "answered" => {
                let Some(found) = repo.get_with_answers(decision.id)? else { continue };
                let Some(answer) = found.answers.last() else { continue };
                match answer.option {
                    Some(1) => (UnrelatedStanding::Waived, answer.answered_at),
                    Some(_) => (UnrelatedStanding::FixHere, answer.answered_at),
                    None => continue,
                }
            }
            _ => continue,
        };
        match found
            .iter_mut()
            .find(|(u, _)| {
                u.pr.eq_ignore_ascii_case(&pr) && u.check.eq_ignore_ascii_case(&check) && u.head.eq_ignore_ascii_case(&head)
            })
        {
            Some(entry) if entry.1 <= at => *entry = (UnrelatedCheck { pr, check, head, signature, standing, at }, at),
            Some(_) => {}
            None => found.push((UnrelatedCheck { pr, check, head, signature, standing, at }, at)),
        }
    }
    Ok(found.into_iter().map(|(u, _)| u).collect())
}

/// `settings` with the node's standings, for every reader of its pull
/// requests ([`crate::bot_silence::settings_for_node`] calls this).
pub fn apply(conn: &rusqlite::Connection, node: Uuid, settings: &mut PrReadinessSettings) -> anyhow::Result<()> {
    settings.unrelated_checks = standings(conn, node)?;
    Ok(())
}

/// Whether every one of `failing` is a check the user said to carry on
/// without, on `pr` at `head`; `signatures` are the checks' failure
/// signatures (`PrStatus::failing_signatures`). False for an empty list: a
/// failure GitHub named nothing for is never waived.
pub fn all_waived(
    settings: &PrReadinessSettings,
    pr: &str,
    head: &str,
    failing: &[String],
    signatures: &[(String, String)],
) -> bool {
    !failing.is_empty()
        && failing.iter().all(|c| {
            let sig = signatures.iter().find(|(n, _)| n.eq_ignore_ascii_case(c)).map(|(_, s)| s.as_str()).unwrap_or_default();
            tod_store::settings::unrelated_standing(&settings.unrelated_checks, pr, c, head, sig)
                == Some(UnrelatedStanding::Waived)
        })
}

/// Record `claim` as a request and answer it at once as tod, "carry on
/// without", with `rationale` saying what was compared: the same standing as
/// the user's answer holds ([`standing`]), and the Answers drawer shows that
/// tod decided and why. Used for a failure the comparison with the base branch
/// confirmed ([`Raised::BaseBranch`]).
pub fn settle_confirmed(
    fleet: &tod_store::fleet::FleetStore,
    node: Uuid,
    claim: &Claim,
    rationale: &str,
) -> anyhow::Result<()> {
    use tod_store::interview::InterviewCommand;
    let made = fleet
        .interview(
            "autopilot",
            InterviewCommand::AskDecision {
                node_id: node,
                conversation_id: None,
                protocol: Some(PROTOCOL.to_string()),
                decision: new_decision(claim),
            },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let id = made
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or_else(|| anyhow::anyhow!("the recorded request has no id"))?;
    fleet
        .interview(
            "autopilot",
            InterviewCommand::SettleDecision { decision_id: id, option: 1, text: None, rationale: rationale.to_string() },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// File the suggestion that follows a waived pre-existing failure: "Open a
/// ticket: check X fails on main" (`crate::suggestions`, filed by the app,
/// never blocking the node), and note it in the node's Activity. The body
/// leaves out the base head, so a second waiver of the same check finds the
/// open suggestion instead of filing another.
pub fn file_pre_existing_suggestion(
    fleet: &tod_store::fleet::FleetStore,
    node: Uuid,
    phase: &str,
    pr: &str,
    found: &tod_store::github::BaseFailure,
) -> anyhow::Result<()> {
    use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};
    let ticket_title = format!("Check \"{}\" fails on {}", found.check, found.base_branch);
    let body = format!(
        "The check \"{}\" fails on {} as well as on {pr}, so the change did not cause it; tod waived it there.\n\nAcceptance criteria:\n- \"{}\" passes on {}.",
        found.check,
        found.base_branch,
        found.check,
        found.base_branch,
    );
    crate::suggestions::file(
        fleet,
        &crate::suggestions::FileRequest {
            node,
            title: ticket_title,
            body,
            conversation: None,
            filed_by: tod_store::suggestions::FILED_BY_APP,
        },
    )?;
    let title = format!("Suggested a ticket: check \"{}\" fails on {}", found.check, found.base_branch);
    let detail = format!("{pr}: {}", found.describe());
    let entry = TimelineEntry::new(node, KIND_NOTE, phase, title, false, detail);
    fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
    fleet.writer().flush()?;
    Ok(())
}

/// The pending check-unrelated decisions of `node`, with their key.
pub fn pending(conn: &rusqlite::Connection, node: Uuid) -> anyhow::Result<Vec<(Decision, String, String, String)>> {
    Ok(DecisionRepo::new(conn)
        .list_pending_for_node(node)?
        .into_iter()
        .filter_map(|d| key_of(&d).map(|(pr, check, head)| (d, pr, check, head)))
        .collect())
}

/// Read GitHub and reconcile a node's CI requests without preparing files,
/// launching an agent, or starting the run's time budget. The inner error
/// is a failed GitHub look, so the caller can handle its own retry policy.
pub fn recheck(
    fleet: &tod_store::fleet::FleetStore,
    node: Uuid,
    data_root: &std::path::Path,
) -> anyhow::Result<Result<usize, String>> {
    use crate::pr_readiness;
    let questions = fleet.read(|conn| pending(conn, node))?;
    if questions.is_empty() {
        return Ok(Ok(0));
    }
    let state = crate::lifecycle::current_state(fleet, node)?;
    if !babysits_ci(&state) {
        return Ok(Ok(0));
    }
    let settings = pr_readiness::settings_for_node(fleet, node, data_root)?;
    let links = fleet.read(|conn| tod_store::github::NodePrRepo::new(conn).read(node))?;
    let live = if links.prs.is_empty() {
        Vec::new()
    } else {
        let Some(feed) = pr_readiness::feed_for(data_root) else {
            return Ok(Err("GitHub credentials are not set up".into()));
        };
        match pr_readiness::live(feed.as_ref(), &links.prs, &settings) {
            Ok(live) => live,
            Err(error) => return Ok(Err(error)),
        }
    };
    Ok(Ok(reconcile(fleet, node, &state, &live, &settings, questions)?))
}

/// Reconcile CI questions against the same live PR assessment used by the
/// babysitter and by an API-only GitHub wake. Returns the number still open.
pub(crate) fn reconcile(
    fleet: &tod_store::fleet::FleetStore,
    node: Uuid,
    state: &str,
    live: &[crate::pr_readiness::LivePr],
    settings: &PrReadinessSettings,
    questions: Vec<(Decision, String, String, String)>,
) -> anyhow::Result<usize> {
    use crate::pr_readiness;
    use tod_store::interview::InterviewCommand;
    use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};
    let mut open = 0usize;
    for (decision, pr, check, head) in questions {
        // The user may have answered the same failure on another node
        // since this was raised: that answer covers this one too.
        if let Some(p) = live.iter().find(|p| pr_readiness::pr_label(&p.pr).eq_ignore_ascii_case(&pr)) {
            let signature = signature_of(&decision);
            if p.assessment.failing_checks.iter().any(|c| c.eq_ignore_ascii_case(&check)) {
                let carried = fleet.read(|conn| {
                    Ok(carried_on_elsewhere(conn, node, &pr, &check, &signature)?)
                })?;
                if let Some(carried) = carried {
                    inherit(fleet, node, decision.id, &carried, &pr, &check, state)?;
                    continue;
                }
            }
        }
        let live_pr = live.iter().find(|p| pr_readiness::pr_label(&p.pr).eq_ignore_ascii_case(&pr));
        let why = match live_pr {
            None => format!("{pr} is no longer linked to the node"),
            Some(p) if p.assessment.merged => format!("{pr} is merged"),
            Some(p) if !p.assessment.failing_checks.iter().any(|c| c.eq_ignore_ascii_case(&check)) => {
                format!("\"{check}\" no longer fails on {pr}")
            }
            // The agent judged an earlier commit: a push since may be the
            // one that broke the check, so it is judged again.
            // ...unless it still fails the same way (same signature): the
            // question stands, and so does its answer, for the new head.
            Some(p)
                if !p.assessment.head_sha.as_deref().unwrap_or_default().eq_ignore_ascii_case(&head)
                    && !same_failure(&decision, &check, &p.assessment) =>
            {
                format!("{pr} has a new head since the agent judged \"{check}\"")
            }
            Some(_) => {
                // The agent said again, on this head, that the check is
                // not the change's, and the user once said to carry on
                // without it on this pull request: apply that, and say so.
                // Only the agent's own attestation counts: a question tod
                // raised (a base failure, two quiet turns) is the user's
                // to answer again for a head whose failure is not known
                // to be the same.
                if raised_of(&decision) == Raised::Agent
                    && tod_store::settings::waived_on_earlier_head(
                        &settings.unrelated_checks,
                        &pr,
                        &check,
                        &head,
                        &signature_of(&decision),
                    )
                {
                    tracing::info!(%node, %pr, %check, "carrying an earlier carry-on forward to the new head");
                    fleet
                        .interview(
                            "autopilot",
                            InterviewCommand::AnswerDecision { decision_id: decision.id, option: Some(1), text: None },
                        )
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    let title = format!("Carried on without \"{check}\" on {pr}: you said so for an earlier commit");
                    let detail = format!(
                        "The agent said again, for the new head, that \"{check}\" is not this change's. Its question: {}",
                        decision.question
                    );
                    let entry = TimelineEntry::new(node, KIND_NOTE, state, title, false, detail);
                    fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
                    fleet.writer().flush()?;
                } else {
                    open += 1;
                }
                continue;
            }
        };
        let request_age_ms = chrono::Utc::now().timestamp_millis().saturating_sub(decision.created_at);
        tracing::info!(
            %node, %pr, %check, request = %decision.id, request_age_ms, %why,
            "a failing check the agent called unrelated is gone: withdrawing the question"
        );
        fleet
            .interview("autopilot", InterviewCommand::WithdrawDecision { decision_id: decision.id })
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let title = format!("Request withdrawn: {why}");
        let detail = format!("{why}. Withdrawn: \"{}\"", decision.question);
        let entry = TimelineEntry::new(node, KIND_NOTE, state, title, false, detail);
        fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
        fleet.writer().flush()?;
    }
    Ok(open)
}

/// Whether `check` still fails the way the check-unrelated question
/// `decision` was raised about (both signatures known and equal).
fn same_failure(decision: &tod_store::decisions::Decision, check: &str, assessment: &crate::pr_readiness::Assessment) -> bool {
    let asked = crate::check_unrelated::signature_of(decision);
    let now = assessment
        .failing_signatures
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(check))
        .map(|(_, s)| s.as_str())
        .unwrap_or_default();
    !asked.is_empty() && asked == now
}

/// The answer another node's user gave that covers a failure
/// ([`carried_on_elsewhere`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carried {
    pub node: Uuid,
    /// The node's slug, for the record.
    pub node_name: String,
    pub decision: Uuid,
}

/// The repository part of `owner/repo#number`, lowercase.
fn repo_of(pr: &str) -> String {
    pr.rsplit_once('#').map(|(r, _)| r).unwrap_or(pr).to_ascii_lowercase()
}

/// Whether another node's user already said to carry on without `check` for
/// a failure with this `signature` in the same repository as `pr`
/// (`owner/repo#number`): one answer covers every pull request of the
/// repository that fails the same way. Only a carry-on spreads: "fix it in
/// this pull request" is about one pull request, and a failure that changed
/// its signature, or whose signature is not known (empty), is asked again.
/// The latest such carry-on on any node other than `node` wins.
pub fn carried_on_elsewhere(
    conn: &rusqlite::Connection,
    node: Uuid,
    pr: &str,
    check: &str,
    signature: &str,
) -> anyhow::Result<Option<Carried>> {
    if signature.is_empty() {
        return Ok(None);
    }
    let repo = repo_of(pr);
    let decisions = DecisionRepo::new(conn);
    let mut best: Option<(i64, Decision)> = None;
    for d in decisions.list_by_protocol(PROTOCOL)? {
        if d.node_id == node || d.status != "answered" {
            continue;
        }
        let Some((d_pr, d_check, _, d_sig)) = key_and_signature(&d) else { continue };
        if d_sig != signature || !d_check.eq_ignore_ascii_case(check) || repo_of(&d_pr) != repo {
            continue;
        }
        let Some(found) = decisions.get_with_answers(d.id)? else { continue };
        let Some(answer) = found.answers.last() else { continue };
        if answer.option != Some(1) || best.as_ref().is_some_and(|(at, _)| *at > answer.answered_at) {
            continue;
        }
        best = Some((answer.answered_at, d));
    }
    let Some((_, d)) = best else { return Ok(None) };
    let node_name = conn
        .query_row("SELECT slug FROM nodes WHERE id = ?1", [d.node_id.as_bytes().as_slice()], |r| r.get::<_, String>(0))
        .unwrap_or_else(|_| d.node_id.to_string());
    Ok(Some(Carried { node: d.node_id, node_name, decision: d.id }))
}

/// What a carry-on inherited from another node says in its record.
pub fn inherited_note(carried: &Carried, pr: &str, check: &str) -> String {
    format!(
        "Carried on without \"{check}\" on {pr}: you said so on {} (request {}) for the same check failing the same way in the same repository",
        carried.node_name,
        &carried.decision.simple().to_string()[..8]
    )
}

/// Answers the pending question `decision` on `node` with the carry-on
/// `carried` gave elsewhere, and records on the node's timeline whose answer
/// it was. The question is the user's to answer; this applies the answer they
/// already gave.
pub fn inherit(
    fleet: &tod_store::fleet::FleetStore,
    node: Uuid,
    decision: Uuid,
    carried: &Carried,
    pr: &str,
    check: &str,
    phase: &str,
) -> anyhow::Result<()> {
    use tod_store::interview::InterviewCommand;
    use tod_store::node_timeline::{KIND_NOTE, TimelineEntry};
    let note = inherited_note(carried, pr, check);
    fleet
        .interview(
            "autopilot",
            InterviewCommand::AnswerDecision { decision_id: decision, option: Some(1), text: Some(note.clone()) },
        )
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let entry = TimelineEntry::new(node, KIND_NOTE, phase, note.clone(), false, note);
    fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
    fleet.writer().flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim() -> Claim {
        Claim {
            pr: "Cofactr/infra#551".into(),
            pr_url: "https://github.com/Cofactr/infra/pull/551".into(),
            check: "Kasm offline validation and tests".into(),
            head: "abc1234".into(),
            signature: "sig1".into(),
            reason: "the lock file error is in modules/aws/stacks/kasm, which this change does not touch; the same check fails on vageli/retool-security-update".into(),
            raised: Raised::Agent,
        }
    }

    #[test]
    fn a_question_tod_raised_says_so_and_names_its_proof() {
        let q = question(&Claim { raised: Raised::BaseBranch, reason: "main @ abc1234 fails the same".into(), ..claim() });
        assert!(q.contains("tod found, without the agent") && q.contains("main @ abc1234"), "{q}");
        assert!(!q.contains("agent says"), "{q}");
    }

    #[test]
    fn the_wording_names_the_pull_request_the_check_and_the_evidence() {
        let q = question(&claim());
        let first = q.lines().next().unwrap();
        assert!(first.starts_with("Decide whether"), "{first}");
        assert!(first.contains("Kasm offline validation and tests") && first.contains("Cofactr/infra#551"), "{first}");
        assert!(q.contains("modules/aws/stacks/kasm"), "{q}");
        assert!(q.contains("https://github.com/Cofactr/infra/pull/551"), "{q}");
        let o = options("Cofactr/infra#551", "Kasm offline validation and tests", "abc1234", "sig1");
        assert_eq!(o[1], "Fix it in this pull request");
        assert!(o[0].starts_with("It's not this change's: carry on without"), "{o:?}");
    }

    #[test]
    fn the_key_is_read_back_from_the_decision() {
        let c = claim();
        let mut d = Decision {
            id: Uuid::new_v4(),
            node_id: Uuid::new_v4(),
            conversation_id: None,
            protocol: Some(PROTOCOL.into()),
            question: question(&c),
            options: options(&c.pr, &c.check, &c.head, &c.signature),
            evidence: Vec::new(),
            status: "pending".into(),
            created_at: 0,
            reason: REASON_BROKEN.into(),
            moves: Vec::new(),
            edits: Vec::new(),
            blocks: Vec::new(),
            new_evidence: String::new(),
            brief: Default::default(),
            error_signature: String::new(),
            cause: String::new(),
            triage_conversation_id: None,
            asked: None,
        };
        assert_eq!(key_of(&d), Some((c.pr.clone(), c.check.clone(), c.head.clone())));
        // A check whose name has quotes and parentheses still round-trips.
        d.options = options("o/r#1", "build (linux) \"x\"", "def5678", "");
        assert_eq!(
            key_of(&d),
            Some(("o/r#1".to_string(), "build (linux) \"x\"".to_string(), "def5678".to_string()))
        );
        d.protocol = Some("bot_silence".into());
        assert_eq!(key_of(&d), None);
    }

    #[test]
    fn only_waived_checks_are_waived_and_an_unnamed_failure_never_is() {
        let settings = PrReadinessSettings {
            unrelated_checks: vec![
                UnrelatedCheck { pr: "o/r#1".into(), check: "a".into(), head: "h1".into(), signature: String::new(), standing: UnrelatedStanding::Waived, at: 0 },
                UnrelatedCheck { pr: "o/r#1".into(), check: "b".into(), head: "h1".into(), signature: String::new(), standing: UnrelatedStanding::Pending, at: 0 },
            ],
            ..Default::default()
        };
        assert!(all_waived(&settings, "O/R#1", "H1", &["A".to_string()], &[]));
        assert!(!all_waived(&settings, "o/r#1", "h1", &["a".to_string(), "b".to_string()], &[]));
        assert!(!all_waived(&settings, "o/r#2", "h1", &["a".to_string()], &[]));
        assert!(!all_waived(&settings, "o/r#1", "h1", &[], &[]));
        // A waiver given for another head never covers this one.
        assert!(!all_waived(&settings, "o/r#1", "h2", &["a".to_string()], &[]));
    }

    mod across_nodes {
        use super::*;
        use tod_store::fleet::FleetStore;
        use tod_store::interview::InterviewCommand;
        use tod_store::outline::{CreatePosition, OutlineMutation};

        fn fleet_with(n: usize) -> (tempfile::TempDir, FleetStore, Vec<Uuid>) {
            let dir = tempfile::Builder::new().prefix("tod-cu-").tempdir().unwrap();
            let fleet = FleetStore::open(dir.path()).unwrap();
            fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() }).unwrap();
            fleet.writer().flush().unwrap();
            let list_id = fleet.list_outline_lists().unwrap()[0].id;
            let mut nodes = Vec::new();
            for i in 0..n {
                let node = Uuid::new_v4();
                fleet
                    .enqueue_outline(OutlineMutation::CreateNode {
                        node_id: Some(node),
                        list_id,
                        parent_id: None,
                        anchor_id: None,
                        position: CreatePosition::Below,
                        title: format!("Node {i}"),
                    })
                    .unwrap();
                nodes.push(node);
            }
            fleet.writer().flush().unwrap();
            fleet.reload_if_stale().unwrap();
            (dir, fleet, nodes)
        }

        fn ask(fleet: &FleetStore, node: Uuid, pr: &str, check: &str, sig: &str) -> Uuid {
            let claim = Claim {
                pr: pr.into(),
                pr_url: format!("https://github.com/{}", pr.replace('#', "/pull/")),
                check: check.into(),
                head: "abc1234".into(),
                signature: sig.into(),
                reason: "main fails it too".into(),
                raised: Raised::BaseBranch,
            };
            let made = fleet
                .interview(
                    "autopilot",
                    InterviewCommand::AskDecision {
                        node_id: node,
                        conversation_id: None,
                        protocol: Some(PROTOCOL.into()),
                        decision: new_decision(&claim),
                    },
                )
                .unwrap();
            Uuid::parse_str(made["id"].as_str().unwrap()).unwrap()
        }

        fn answer(fleet: &FleetStore, id: Uuid, option: i64) {
            fleet
                .interview("user", InterviewCommand::AnswerDecision { decision_id: id, option: Some(option), text: None })
                .unwrap();
        }

        fn carried(fleet: &FleetStore, node: Uuid, pr: &str, check: &str, sig: &str) -> Option<Carried> {
            fleet.read(|c| carried_on_elsewhere(c, node, pr, check, sig)).unwrap()
        }

        #[test]
        fn answering_on_one_node_resolves_the_same_failure_on_another() {
            let (_d, fleet, n) = fleet_with(2);
            let a = ask(&fleet, n[0], "acme/app#1", "pyright", "sig");
            let b = ask(&fleet, n[1], "acme/app#2", "Pyright", "sig");
            assert!(carried(&fleet, n[1], "acme/app#2", "pyright", "sig").is_none(), "nothing answered yet");
            answer(&fleet, a, 1);
            let found = carried(&fleet, n[1], "ACME/app#2", "pyright", "sig").expect("carried");
            assert_eq!((found.node, found.decision), (n[0], a));
            inherit(&fleet, n[1], b, &found, "acme/app#2", "pyright", "pr_draft").unwrap();
            let standings = fleet.read(|c| standings(c, n[1])).unwrap();
            assert_eq!(standings[0].standing, UnrelatedStanding::Waived);
            let record = fleet.read(|c| DecisionRepo::new(c).get_with_answers(b)).unwrap().unwrap();
            let text = record.answers[0].text.clone().unwrap();
            assert!(text.contains(&found.node_name) && text.contains(&a.simple().to_string()[..8]), "{text}");
        }

        #[test]
        fn another_failure_or_repository_is_asked_again() {
            let (_d, fleet, n) = fleet_with(2);
            let a = ask(&fleet, n[0], "acme/app#1", "pyright", "sig");
            answer(&fleet, a, 1);
            assert!(carried(&fleet, n[1], "acme/app#2", "pyright", "other-sig").is_none());
            assert!(carried(&fleet, n[1], "acme/other#2", "pyright", "sig").is_none());
            assert!(carried(&fleet, n[1], "acme/app#2", "mypy", "sig").is_none());
            assert!(carried(&fleet, n[1], "acme/app#2", "pyright", "").is_none(), "an unknown failure is never matched");
            assert!(carried(&fleet, n[1], "acme/app#2", "pyright", "sig").is_some());
            // The node's own answer is not "elsewhere".
            assert!(carried(&fleet, n[0], "acme/app#1", "pyright", "sig").is_none());
        }

        #[test]
        fn fixing_it_here_does_not_spread() {
            let (_d, fleet, n) = fleet_with(2);
            let a = ask(&fleet, n[0], "acme/app#1", "pyright", "sig");
            answer(&fleet, a, 2);
            assert!(carried(&fleet, n[1], "acme/app#2", "pyright", "sig").is_none());
        }
    }
}
