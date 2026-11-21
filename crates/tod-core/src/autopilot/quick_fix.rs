//! The quick fix inside the autopilot (`doc/requests/design.md` section 5):
//! rung 1 of the escalation ladder, tried on a failing check of tod's own after
//! a configured fixer has failed or is absent, and before the full Fix protocol.
//!
//! Each failing check gets one attempt per run of failures (the check's key is
//! kept in `LoopCounters::quick_fix_tried` until the checks are clear). A check
//! that passes again needs no Fix and no new review round; what is still failing
//! goes to Fix with the quick fix's notes in its finding.

use super::*;
use anyhow::Context as _;
use tod_store::quick_fixes::{self, OUTCOME_ESCALATED, QuickFix, SOURCE_CHECK};

impl Autopilot {
    /// Try a quick fix on each of `failed` that may have one. Returns the checks
    /// that still fail and the notes the attempts left (`(check name, notes)`),
    /// or the outcome that stops the run.
    pub(super) fn quick_fix_checks<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        lifecycle: &str,
        fingerprint: &str,
        failed: &[&tod_store::check_runs::CheckRun],
    ) -> Result<std::result::Result<QuickFixed, Outcome>> {
        let node = self.node;
        self.escalate_interrupted(fleet)?;
        let configured = fleet.read(|conn| crate::checks::configured(conn, node, lifecycle))?;
        let mut still: Vec<tod_store::check_runs::CheckRun> = Vec::new();
        let mut notes: Vec<(String, String)> = Vec::new();
        for run in failed {
            let key = quick_fixes::check_key(&run.dir, &run.command);
            let skip = configured
                .iter()
                .find(|c| c.command == run.command && c.dir.trim().trim_matches('/') == run.dir.trim().trim_matches('/'))
                .is_some_and(|c| c.skip_quick_fix);
            if skip || self.state.loops.quick_fix_tried.contains(&key) {
                still.push((*run).clone());
                continue;
            }
            self.state.loops.quick_fix_tried.push(key.clone());
            self.save()?;
            let name = crate::checks::run_name(run);
            let mut fix = QuickFix::open(node, SOURCE_CHECK, lifecycle, &name, &key, &run.command, &run.output, fingerprint);
            crate::conversation::quick_fix::save(fleet, &fix)?;
            hook.working_on(&format!("a quick fix for {name}"));
            if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::QuickFix, None)? {
                self.state.loops.quick_fix_tried.retain(|k| k != &key);
                self.save()?;
                self.escalate_interrupted(fleet)?;
                return Ok(Err(outcome));
            }
            fix = fleet
                .read(|conn| quick_fixes::get(conn, fix.id))?
                .with_context(|| format!("the quick fix for {name} was not recorded"))?;
            if fix.is_open() {
                fix.finish(OUTCOME_ESCALATED);
                crate::conversation::quick_fix::save(fleet, &fix)?;
            }
            if fix.outcome == OUTCOME_ESCALATED {
                notes.push((name, fix.notes.clone()));
                still.push((*run).clone());
            }
        }
        Ok(Ok(QuickFixed { still_failing: still, notes }))
    }

    /// A quick fix left open by a run that stopped in the middle of it has no
    /// session to resume: it is recorded as escalated, saying so.
    fn escalate_interrupted(&mut self, fleet: &FleetStore) -> Result<()> {
        let node = self.node;
        if let Some(mut open) = fleet.read(|conn| quick_fixes::open_for_node(conn, node))? {
            if open.notes.is_empty() {
                open.notes = "The quick fix was interrupted before it finished.".to_string();
            }
            open.finish(OUTCOME_ESCALATED);
            crate::conversation::quick_fix::save(fleet, &open)?;
        }
        Ok(())
    }
}

/// What the quick fixes of one checks round left.
pub(super) struct QuickFixed {
    pub still_failing: Vec<tod_store::check_runs::CheckRun>,
    pub notes: Vec<(String, String)>,
}

/// What [`Autopilot::run_ci_quick_fixes`] did.
pub(super) enum CiQuick {
    /// No quick fix was tried.
    Nothing,
    /// One was tried: read the pull request again.
    Worked,
    /// The run has to stop.
    Stop(Outcome),
}

impl Autopilot {
    /// Settle the CI quick fixes still open, from what the pull request shows
    /// now. One whose commit is the pull request's head decides: the check no
    /// longer failing is success, still failing is an escalation (to the pull
    /// request agent, with the quick fix's notes); a head still running waits.
    /// One that left no commit, or belongs to an earlier stay, is escalated.
    pub(super) fn resolve_ci_quick_fixes(&mut self, fleet: &FleetStore, live: &[crate::pr_readiness::LivePr]) -> Result<()> {
        let node = self.node;
        let stay = fleet.read(|conn| tod_store::phase::PhaseRepo::new(conn).stay_started_at(node))?;
        let open: Vec<QuickFix> = fleet.read(|conn| {
            Ok(quick_fixes::list_for_node(conn, node)?
                .into_iter()
                .filter(|f| f.is_open() && f.source == tod_store::quick_fixes::SOURCE_CI)
                .collect())
        })?;
        for mut fix in open {
            if stay.is_some_and(|stay| fix.started_at < stay) {
                fix.notes = format!("{} It was left open by an earlier stay.", fix.notes).trim().to_string();
                fix.finish(OUTCOME_ESCALATED);
            } else if fix.head.is_empty() {
                if fix.notes.is_empty() {
                    fix.notes = "The quick fix made no commit.".to_string();
                }
                fix.finish(OUTCOME_ESCALATED);
            } else {
                let seen: Vec<&crate::pr_readiness::LivePr> =
                    live.iter().filter(|p| p.assessment.head_sha.as_deref() == Some(fix.head.as_str())).collect();
                if seen.is_empty() || seen.iter().any(|p| p.assessment.checks_running) {
                    continue;
                }
                let still = seen.iter().any(|p| p.assessment.checks_to_fix().iter().any(|c| c.eq_ignore_ascii_case(&fix.check_name)));
                fix.finish(if still { OUTCOME_ESCALATED } else { tod_store::quick_fixes::OUTCOME_SUCCEEDED });
            }
            crate::conversation::quick_fix::save(fleet, &fix)?;
        }
        Ok(())
    }

    /// Rung 1 for a failing CI check (after a configured fixer failed or is
    /// absent, before the pull request agent): a quick fix in the node's
    /// checkout, on the pull request's own head, once per check and head, and
    /// never again for a check whose quick fix escalated in this stay. The
    /// commit is held for the small-change review in `pr_draft`
    /// ([`Self::small_review_and_push`]), else for the review the agent's own
    /// commits get.
    pub(super) fn run_ci_quick_fixes<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        live: &[crate::pr_readiness::LivePr],
        settings: &tod_store::PrReadinessSettings,
        phase: &str,
    ) -> Result<CiQuick> {
        if self.state.pr_change.is_some() {
            return Ok(CiQuick::Nothing);
        }
        let node = self.node;
        let stay = fleet.read(|conn| tod_store::phase::PhaseRepo::new(conn).stay_started_at(node))?;
        for p in live {
            let a = &p.assessment;
            let Some(head) = a.head_sha.clone() else { continue };
            if a.merged || head.is_empty() || a.checks_running {
                continue;
            }
            for check in a.checks_to_fix() {
                if settings.skip_quick_fix.iter().any(|s| s.eq_ignore_ascii_case(&check))
                    || a.infra_failures.iter().any(|i| i.check.eq_ignore_ascii_case(&check))
                {
                    continue;
                }
                let history = fleet.read(|conn| quick_fixes::for_check(conn, node, tod_store::quick_fixes::SOURCE_CI, &check))?;
                if history.iter().any(|f| f.fingerprint == head)
                    || history.iter().any(|f| f.outcome == OUTCOME_ESCALATED && stay.is_none_or(|stay| f.started_at >= stay))
                {
                    continue;
                }
                let Some(cwd) = tod_store::fleet::provision::launch_cwd_if_made(fleet, &node.to_string())? else { continue };
                let dir = crate::pr_draft_review::repo_dir(&cwd, &p.pr.owner, &p.pr.repo)?;
                // Only on the pull request's own head: a fix on top of commits
                // nobody pushed would hold them back with it.
                if crate::pr_draft_review::local_head(&dir)? != head {
                    continue;
                }
                let signature =
                    a.failing_signatures.iter().find(|(n, _)| n.eq_ignore_ascii_case(&check)).map(|(_, s)| s.clone()).unwrap_or_default();
                let output = if signature.is_empty() { format!("The CI check \"{check}\" is failing on {}.", p.pr.url) } else { signature };
                let fix = QuickFix::open(node, tod_store::quick_fixes::SOURCE_CI, phase, &check, &check, &check, &output, &head);
                crate::conversation::quick_fix::save(fleet, &fix)?;
                hook.working_on(&format!("a quick fix for {check}"));
                if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::QuickFix, None)? {
                    self.escalate_interrupted(fleet)?;
                    return Ok(CiQuick::Stop(outcome));
                }
                let mut done = fleet
                    .read(|conn| quick_fixes::get(conn, fix.id))?
                    .with_context(|| format!("the quick fix for {check} was not recorded"))?;
                if done.head.is_empty() && done.is_open() {
                    if done.notes.is_empty() {
                        done.notes = "The quick fix made no commit.".to_string();
                    }
                    done.finish(OUTCOME_ESCALATED);
                    crate::conversation::quick_fix::save(fleet, &done)?;
                } else if !done.head.is_empty()
                    && let Some(outcome) = self.small_review_and_push(fleet, agent, hook, phase)?
                {
                    return Ok(CiQuick::Stop(outcome));
                }
                return Ok(CiQuick::Worked);
            }
        }
        Ok(CiQuick::Nothing)
    }

    /// The small-change review (`doc/requests/design.md` 4.2, last paragraph)
    /// of the commit the latest quick fix made, in `pr_draft` only: one cheap
    /// reviewer reads the diff. Clean, tod pushes the commit itself and records
    /// its head as reviewed (`crate::pr_pushes::record_small_reviewed`), so
    /// no review round is owed. A finding leaves the commit unpushed, where its
    /// unpushed state hands it to the full review
    /// (`crate::pr_pushes::held_for`), the findings staying open on the node.
    /// Outside `pr_draft` nothing is reviewed here and nothing pushed.
    pub(super) fn small_review_and_push<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        lifecycle: &str,
    ) -> Result<Option<Outcome>> {
        use crate::conversation::small_review;
        if lifecycle != crate::pr_draft_review::STATE {
            return Ok(None);
        }
        let node = self.node;
        let Some(fix) = small_review::reviewed_fix(fleet, node)? else { return Ok(None) };
        let reviewed = fleet.read(|conn| {
            Ok(tod_store::conversation::ConversationRepo::new(conn)
                .conversations_for_focus(Focus::Node(node))?
                .into_iter()
                .any(|c| c.protocol == ProtocolKind::SmallReview && c.created_at >= fix.started_at))
        })?;
        if reviewed {
            return Ok(None);
        }
        let prs = fleet.read(|conn| tod_store::github::NodePrRepo::new(conn).read(node))?.prs;
        if prs.is_empty() {
            return Ok(None);
        }
        hook.working_on("a small review of the quick fix");
        if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::SmallReview, None)? {
            return Ok(Some(outcome));
        }
        let review = fleet
            .read(|conn| {
                Ok(tod_store::conversation::ConversationRepo::new(conn)
                    .latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::SmallReview)?)
            })?
            .context("the small review left no conversation")?;
        let findings = small_review::findings_of(fleet, node, review.id)?;
        let title = if findings.is_empty() {
            "The small review of the quick fix found nothing: tod pushes it".to_string()
        } else {
            format!("The small review of the quick fix found {} problem(s): the full review takes the commit", findings.len())
        };
        let entry = tod_store::node_timeline::TimelineEntry::new(
            node,
            tod_store::node_timeline::KIND_NOTE,
            lifecycle,
            title,
            !findings.is_empty(),
            findings.iter().map(|f| f.summary.clone()).collect::<Vec<_>>().join("\n"),
        );
        fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
        fleet.writer().flush()?;
        if !findings.is_empty() {
            return Ok(None);
        }
        let cwd = tod_store::fleet::provision::resolve_launch_cwd(fleet, &node.to_string())
            .context("finding the node's checkout, to push the reviewed quick fix")?;
        let mut targets = Vec::new();
        for pr in &prs {
            let dir = crate::pr_draft_review::repo_dir(&cwd, &pr.owner, &pr.repo)?;
            let local = crate::pr_draft_review::local_head(&dir)?;
            let latest = fleet.read(|conn| tod_store::pr_pushes::PrPushRepo::new(conn).latest(node, &crate::pr_readiness::pr_label(pr)))?;
            targets.push((pr.clone(), local, latest.map(|l| l.head)));
        }
        for (_, local, _) in &targets {
            self.remember_tod_push(local.clone())?;
        }
        tod_store::fleet::provision::push_node_branch(fleet, &node.to_string())?;
        for (pr, local, previous) in targets {
            if previous.as_deref() != Some(local.as_str()) {
                crate::pr_pushes::record_small_reviewed(fleet, node, &pr, previous, &local, lifecycle, review.id)?;
            }
            crate::pr_readiness::forget_snapshot(&pr);
        }
        Ok(None)
    }

    /// What the pull request agent is told when a CI check's quick fix did not
    /// make it pass: the notes of each such attempt of this stay whose check is
    /// still failing. Empty when there is none.
    pub(super) fn quick_fix_addendum(&self, fleet: &FleetStore, live: &[crate::pr_readiness::LivePr]) -> Result<String> {
        let node = self.node;
        let stay = fleet.read(|conn| tod_store::phase::PhaseRepo::new(conn).stay_started_at(node))?;
        let failing: Vec<String> = live.iter().flat_map(|p| p.assessment.checks_to_fix()).collect();
        let rows = fleet.read(|conn| quick_fixes::list_for_node(conn, node))?;
        let mut out = String::new();
        for fix in rows.iter().filter(|f| {
            f.source == tod_store::quick_fixes::SOURCE_CI
                && f.outcome == OUTCOME_ESCALATED
                && stay.is_none_or(|stay| f.started_at >= stay)
                && failing.iter().any(|c| c.eq_ignore_ascii_case(&f.check_name))
        }) {
            let notes = if fix.notes.trim().is_empty() { "(it recorded nothing)" } else { fix.notes.trim() };
            out.push_str(&format!(
                "\nA quick fix (a small, cheap session limited to this one check) tried \"{}\" first and did not make it pass. What it did and left undone:\n{notes}\n",
                fix.check_name
            ));
        }
        Ok(out)
    }
}

/// What [`Autopilot::settle_review_round`] needs to know about where the
/// round ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lanes {
    /// `review`: the quick fix lane runs, and the small review clears it.
    All,
    /// `pr_draft`, or tod's review of a change before it is pushed: nothing
    /// is fixed here (a commit would need its own push), so a finding that
    /// blocks goes on as before and one that does not is left.
    LeaveOnly,
}

impl Autopilot {
    /// A review round just finished: route each finding it left open by its
    /// scores (`crate::review_scoring`). Findings that do not block are
    /// settled `left` with tod's reason, noted on the node's Activity; the
    /// mechanical and simple ones that do block (and the mechanical nits) go
    /// together to one quick fix session and the small-change review; the
    /// rest stay open for the Fix protocol. A check is suggested for a rule
    /// three tasks' mechanical findings cite. `None` when the run goes on.
    pub(super) fn settle_review_round<A: AgentAccess + ?Sized, H: StepHook + ?Sized>(
        &mut self,
        fleet: &FleetStore,
        agent: &mut A,
        hook: &mut H,
        lifecycle: &str,
        round: u32,
        lanes: Lanes,
    ) -> Result<Option<Outcome>> {
        use crate::review_scoring::{self, QUICK_FIX_FAILED_NIT};
        use tod_store::review::{ReviewFinding, ReviewRepo};
        let node = self.node;
        let minor_rounds = tod_store::TodSettings::load(&tod_store::TodPaths::at(&self.config.data_root))?.review.minor_rounds;
        let open: Vec<ReviewFinding> = fleet.read(|conn| {
            let Some(lead) = ConversationRepo::new(conn).latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::Review)? else {
                return Ok(Vec::new());
            };
            let members = tod_store::review::round::conversations(conn, lead.id)?;
            Ok(ReviewRepo::new(conn)
                .list_for_node(node)?
                .into_iter()
                .filter(|f| f.is_open() && f.conversation_id.is_some_and(|c| members.contains(&c)))
                .collect())
        })?;
        if open.is_empty() {
            return Ok(None);
        }
        let mut routing = review_scoring::route(&open, round, minor_rounds);
        if lanes == Lanes::LeaveOnly {
            for finding in std::mem::take(&mut routing.quick) {
                if review_scoring::blocks(&finding, round, minor_rounds) {
                    routing.fix.push(finding);
                } else {
                    routing.leave.push((finding, "nit: left as it is".to_string()));
                }
            }
        }
        self.leave_findings(fleet, lifecycle, &routing.leave)?;
        self.suggest_checks(fleet)?;
        if routing.quick.is_empty() {
            return Ok(None);
        }

        let findings = routing.quick;
        let cwd = tod_store::fleet::provision::resolve_launch_cwd(fleet, &node.to_string())
            .context("finding the node's checkout, for the quick fix of the review's findings")?;
        let head = cwd.git(&["rev-parse", "HEAD"]).context("read the commit the quick fix starts from")?;
        let name = format!("{} review finding(s)", findings.len());
        let fix = QuickFix::open(
            node,
            quick_fixes::SOURCE_REVIEW,
            lifecycle,
            &name,
            quick_fixes::REVIEW_KEY,
            &review_scoring::ids_text(&findings),
            &review_scoring::render_for_quick_fix(&findings),
            &head,
        );
        crate::conversation::quick_fix::save(fleet, &fix)?;
        hook.working_on(&format!("a quick fix for {name}"));
        if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::QuickFix, None)? {
            self.escalate_interrupted(fleet)?;
            return Ok(Some(outcome));
        }
        let mut done = fleet
            .read(|conn| quick_fixes::get(conn, fix.id))?
            .with_context(|| format!("the quick fix for {name} was not recorded"))?;
        let mut why = String::new();
        if done.head.is_empty() {
            why = "The quick fix made no commit.".to_string();
        } else {
            // The small-change review of that commit, not a full round.
            hook.working_on("a small review of the quick fix");
            if let Some(outcome) = self.converse(fleet, agent, hook, ProtocolKind::SmallReview, None)? {
                return Ok(Some(outcome));
            }
            let review = fleet
                .read(|conn| ConversationRepo::new(conn).latest_for_focus_with_protocol(Focus::Node(node), ProtocolKind::SmallReview))?
                .context("the small review left no conversation")?;
            let found = crate::conversation::small_review::findings_of(fleet, node, review.id)?;
            if !found.is_empty() {
                why = format!(
                    "The small review of the quick fix found {} problem(s): {}",
                    found.len(),
                    found.iter().map(|f| f.summary.clone()).collect::<Vec<_>>().join("; ")
                );
            }
        }
        if why.is_empty() {
            done.finish(tod_store::quick_fixes::OUTCOME_SUCCEEDED);
            crate::conversation::quick_fix::save(fleet, &done)?;
            let response = format!("Quick fix {}; the small review found nothing", &done.head[..done.head.len().min(8)]);
            for finding in &findings {
                fleet.interview(
                    "autopilot",
                    InterviewCommand::SettleReviewFinding { finding_id: finding.id, quick_fix: done.id, response: response.clone() },
                )?;
            }
            self.note(
                fleet,
                lifecycle,
                format!("A quick fix settled {} review finding(s); the small review found nothing", findings.len()),
                false,
                review_scoring::render_for_quick_fix(&findings),
            )?;
            return Ok(None);
        }
        // Rung 1 failed: what blocks goes to Fix with these notes (the ladder
        // lesson is recorded with the outcome); a nit it could not fix is left.
        done.notes = format!("{}\n\n{why}", done.notes.trim()).trim().to_string();
        done.finish(OUTCOME_ESCALATED);
        crate::conversation::quick_fix::save(fleet, &done)?;
        let nits: Vec<(ReviewFinding, String)> = findings
            .iter()
            .filter(|f| !review_scoring::blocks(f, round, minor_rounds))
            .map(|f| (f.clone(), QUICK_FIX_FAILED_NIT.to_string()))
            .collect();
        self.leave_findings(fleet, lifecycle, &nits)?;
        self.note(
            fleet,
            lifecycle,
            format!("A quick fix could not settle {} review finding(s): {}", findings.len(), why.lines().next().unwrap_or("")),
            true,
            done.notes.clone(),
        )?;
        Ok(None)
    }

    /// Settle `findings` as `left`, each with its reason, and note it.
    fn leave_findings(&mut self, fleet: &FleetStore, lifecycle: &str, findings: &[(tod_store::review::ReviewFinding, String)]) -> Result<()> {
        if findings.is_empty() {
            return Ok(());
        }
        for (finding, reason) in findings {
            fleet.interview(
                "autopilot",
                InterviewCommand::RespondReviewFinding {
                    finding_id: finding.id,
                    status: tod_store::review::FINDING_LEFT.to_string(),
                    response: Some(reason.clone()),
                },
            )?;
        }
        self.note(
            fleet,
            lifecycle,
            format!("tod left {} review finding(s) that do not block", findings.len()),
            false,
            findings
                .iter()
                .map(|(f, why)| format!("[{}] {}/{}/{} {}: {why}", tod_store::interview::short_id(f.id), f.importance, f.impact, f.complexity, f.summary))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// File a suggestion to add a check for each rule three tasks' mechanical
    /// findings cite (`crate::review_scoring::rule_suggestions`); a filing
    /// that fails is the error of the step.
    fn suggest_checks(&mut self, fleet: &FleetStore) -> Result<()> {
        let node = self.node;
        for suggestion in fleet.read(|conn| crate::review_scoring::rule_suggestions(conn, node))? {
            let request = crate::suggestions::FileRequest {
                node,
                title: suggestion.title(),
                body: suggestion.body(),
                conversation: None,
                filed_by: tod_store::suggestions::FILED_BY_APP,
            };
            crate::suggestions::file(fleet, &request).with_context(|| format!("filing the suggestion {:?}", request.title))?;
            self.note(fleet, "review", format!("Suggested a check: {}", suggestion.rule), false, suggestion.body())?;
        }
        Ok(())
    }

    /// A note on the node's Activity.
    fn note(&mut self, fleet: &FleetStore, lifecycle: &str, title: String, failed: bool, body: String) -> Result<()> {
        let entry = tod_store::node_timeline::TimelineEntry::new(self.node, tod_store::node_timeline::KIND_NOTE, lifecycle, title, failed, body);
        fleet.enqueue(tod_store::fleet::FleetMutation::RecordTimeline(Box::new(entry)))?;
        fleet.writer().flush()?;
        Ok(())
    }
}
