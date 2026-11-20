use super::*;
use crate::conversation::{ConversationRepo, Focus, actor_for, agent_actor_for};
use crate::fleet::schema;
use crate::interview::{ACTOR_AGENT, InterviewCommand};
use crate::node_timeline::TimelineRepo;
use crate::outline::repos::{NodeRepo, ObligationRepo};
use crate::verification::{VerdictHow, VerdictRepo};
use std::path::Path;

struct Fx {
    conn: Connection,
    node: Uuid,
}

fn setup(state: &str) -> Fx {
    let dir = tod_test_util::scratch_dir(format!("tod-send-back-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
    let node = Uuid::new_v4();
    NodeRepo::new(&conn).create_with_id(node, "n", "N").unwrap();
    NodeRepo::new(&conn).set_lifecycle(node, state).unwrap();
    Fx { conn, node }
}

fn conversation(fx: &Fx, protocol: ProtocolKind) -> Uuid {
    ConversationRepo::new(&fx.conn)
        .create(Focus::Node(fx.node), protocol, None, None, None)
        .unwrap()
        .id
}

fn obligation(fx: &Fx, index: usize, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    ObligationRepo::new(&fx.conn)
        .insert_at(id, fx.node, "requirement", index, None, body, "design")
        .unwrap();
    id
}

fn request(conversation_id: Uuid, to: &str, reason: &str) -> Request {
    Request { conversation_id, to: to.into(), reason: reason.into(), evidence: Vec::new(), obligations: Vec::new() }
}

fn send(fx: &Fx, actor: &str, request: Request) -> Result<Value> {
    crate::interview::execute(&fx.conn, Path::new("."), actor, &InterviewCommand::SendBack(request))
}

fn state(fx: &Fx) -> String {
    NodeRepo::new(&fx.conn).get_lifecycle(fx.node).unwrap().unwrap()
}

/// Back in `from` as the autopilot would bring it (not the user's move).
fn return_to(fx: &Fx, from: &str) {
    NodeRepo::new(&fx.conn).set_lifecycle(fx.node, from).unwrap();
}

#[test]
fn only_the_judging_sessions_may_send_back() {
    let fx = setup("review");
    for protocol in [ProtocolKind::Outline, ProtocolKind::Pr] {
        let id = conversation(&fx, protocol);
        let err = send(&fx, ACTOR_AGENT, request(id, "active", "wrong")).unwrap_err();
        assert!(err.to_string().contains("only a phase agent"), "{protocol:?}: {err}");
    }
    assert_eq!(state(&fx), "review");
    assert!(SendBackRepo::new(&fx.conn).list_for_node(fx.node).unwrap().is_empty());

    // A review session (no actor of its own) may.
    let review = conversation(&fx, ProtocolKind::Review);
    let out = send(&fx, ACTOR_AGENT, request(review, "active", "the build is wrong")).unwrap();
    assert_eq!(out["outcome"], OUTCOME_MOVED);
    assert_eq!(state(&fx), "active");
}

#[test]
fn an_evaluator_may_send_back_though_its_other_writes_are_refused() {
    let fx = setup("design");
    let evaluate = conversation(&fx, ProtocolKind::Evaluate);
    let out = send(&fx, &actor_for(evaluate), request(evaluate, "proposed", "the requirement rests on a false claim"))
        .unwrap();
    assert_eq!(out["outcome"], OUTCOME_MOVED);
    assert_eq!(state(&fx), "proposed");
    let row = &SendBackRepo::new(&fx.conn).list_for_node(fx.node).unwrap()[0];
    assert_eq!((row.protocol.as_str(), row.from_state.as_str()), ("evaluate", "design"));
}

#[test]
fn the_conversation_must_be_the_writers_own() {
    let fx = setup("verifying");
    let verify = conversation(&fx, ProtocolKind::Verification);
    let other = conversation(&fx, ProtocolKind::Review);
    let err = send(&fx, &agent_actor_for(verify), request(other, "active", "x")).unwrap_err();
    assert!(err.to_string().contains("not the one writing"), "{err}");
    send(&fx, &agent_actor_for(verify), request(verify, "active", "the build is wrong")).unwrap();
}

#[test]
fn the_target_must_be_earlier_and_before_pr_review() {
    let fx = setup("review");
    let review = conversation(&fx, ProtocolKind::Review);
    for (to, says) in [("review", "not later"), ("pr_draft", "not later"), ("pr_review", "cannot be sent back"), ("nowhere", "not a lifecycle state")] {
        let err = send(&fx, ACTOR_AGENT, request(review, to, "x")).unwrap_err();
        assert!(err.to_string().contains(says), "{to}: {err}");
    }
    let err = send(&fx, ACTOR_AGENT, request(review, "active", "  ")).unwrap_err();
    assert!(err.to_string().contains("needs a reason"), "{err}");
    assert_eq!(state(&fx), "review");
}

#[test]
fn back_to_verifying_reopens_only_the_named_obligations() {
    let fx = setup("review");
    let (shown, short) = (obligation(&fx, 0, "Shown live"), obligation(&fx, 1, "Only mocked"));
    let verdicts = VerdictRepo::new(&fx.conn);
    verdicts.record(fx.node, shown, None, "verified", "ran it", &VerdictHow::method("live")).unwrap();
    verdicts.record(fx.node, short, None, "verified", "a unit test", &VerdictHow::method("unit")).unwrap();
    let review = conversation(&fx, ProtocolKind::Review);

    let err = send(&fx, ACTOR_AGENT, request(review, "verifying", "evidence short")).unwrap_err();
    assert!(err.to_string().contains("--obligation"), "{err}");

    let mut asked = request(review, "verifying", "mocked where live was needed");
    asked.obligations = vec![short];
    send(&fx, ACTOR_AGENT, asked).unwrap();
    assert_eq!(state(&fx), "verifying");
    let latest = verdicts.latest_for_node(fx.node).unwrap();
    assert!(latest[&shown].is_verified(), "the other obligation stays verified");
    assert_eq!(latest[&short].status, crate::verification::VERDICT_REOPENED);
}

#[test]
fn back_to_active_reopens_every_verdict_as_before() {
    let fx = setup("review");
    let (a, b) = (obligation(&fx, 0, "A"), obligation(&fx, 1, "B"));
    let verdicts = VerdictRepo::new(&fx.conn);
    verdicts.record(fx.node, a, None, "verified", "ok", &VerdictHow::method("live")).unwrap();
    verdicts.record(fx.node, b, None, "verified", "ok", &VerdictHow::method("live")).unwrap();
    let fix = conversation(&fx, ProtocolKind::Fix);
    send(&fx, ACTOR_AGENT, request(fix, "active", "the build is wrong")).unwrap();
    let latest = verdicts.latest_for_node(fx.node).unwrap();
    assert!(!latest[&a].is_verified() && !latest[&b].is_verified());
}

#[test]
fn the_same_move_on_unchanged_work_is_stopped_with_both_reasons() {
    let fx = setup("review");
    let a = obligation(&fx, 0, "A");
    let review = conversation(&fx, ProtocolKind::Review);
    send(&fx, ACTOR_AGENT, request(review, "active", "first reason")).unwrap();
    return_to(&fx, "review");

    let out = send(&fx, ACTOR_AGENT, request(review, "active", "second reason")).unwrap();
    assert_eq!(out["outcome"], OUTCOME_STOPPED);
    assert!(out["why"].as_str().unwrap().contains("unchanged"), "{out}");
    assert_eq!(state(&fx), "review", "a refused send-back does not move the node");
    let rows = SendBackRepo::new(&fx.conn).list_for_node(fx.node).unwrap();
    assert_eq!(rows.iter().map(|r| r.outcome.as_str()).collect::<Vec<_>>(), [OUTCOME_MOVED, OUTCOME_STOPPED]);
    let stopped = SendBackRepo::new(&fx.conn).stopped_since(fx.node, 0).unwrap();
    assert_eq!(stopped[0].reason, "second reason");
    assert_eq!(stopped[0].earlier, ["first reason"], "both reasons are kept");

    // Once the work changed, the same move is allowed.
    ObligationRepo::new(&fx.conn).update_body(a, "A, corrected").unwrap();
    assert_eq!(send(&fx, ACTOR_AGENT, request(review, "active", "third reason")).unwrap()["outcome"], OUTCOME_MOVED);
}

#[test]
fn a_third_send_back_to_the_same_target_waits_for_the_user() {
    let fx = setup("review");
    let a = obligation(&fx, 0, "A");
    let review = conversation(&fx, ProtocolKind::Review);
    for (i, reason) in ["one", "two"].iter().enumerate() {
        ObligationRepo::new(&fx.conn).update_body(a, &format!("A {i}")).unwrap();
        assert_eq!(send(&fx, ACTOR_AGENT, request(review, "active", reason)).unwrap()["outcome"], OUTCOME_MOVED);
        return_to(&fx, "review");
    }
    ObligationRepo::new(&fx.conn).update_body(a, "A 3").unwrap();
    let out = send(&fx, ACTOR_AGENT, request(review, "active", "three")).unwrap();
    assert_eq!(out["outcome"], OUTCOME_STOPPED, "{out}");
    assert!(out["why"].as_str().unwrap().contains("since you last acted"), "{out}");

    // The user acts on the node (a Run request): the count starts again.
    crate::node_requests::record(&fx.conn, fx.node, crate::node_requests::KIND_RUN, "user", now_ms() + 1).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    assert_eq!(send(&fx, ACTOR_AGENT, request(review, "active", "four")).unwrap()["outcome"], OUTCOME_MOVED);
}

#[test]
fn the_target_phase_opening_and_the_activity_carry_the_send_back() {
    let fx = setup("review");
    assert_eq!(render_for_opening(&fx.conn, fx.node).unwrap(), "");
    let a = obligation(&fx, 0, "Prices come from the org's DigiKey id");
    let review = conversation(&fx, ProtocolKind::Review);
    let finding = Uuid::new_v4();
    let mut asked = request(review, "design", "the org has no DigiKey id: the approach is wrong");
    asked.evidence = vec![EvidenceRef { kind: "finding".into(), id: finding }];
    asked.obligations = vec![a];
    send(&fx, ACTOR_AGENT, asked).unwrap();

    let opening = render_for_opening(&fx.conn, fx.node).unwrap();
    assert!(opening.contains("## Dispute"), "{opening}");
    assert!(opening.contains("review session sent this task back from `review` to `design`"), "{opening}");
    assert!(opening.contains("the approach is wrong"), "{opening}");
    assert!(opening.contains(&finding.to_string()), "{opening}");
    assert!(opening.contains("Prices come from"), "{opening}");

    let entries = TimelineRepo::new(&fx.conn).list(fx.node).unwrap();
    let entry = entries.iter().find(|e| e.kind == KIND_SEND_BACK).expect("a send-back entry");
    assert_eq!(entry.phase, "design");
    assert!(entry.detail.contains("the approach is wrong"));
    let change = entries.iter().find(|e| e.kind == crate::node_timeline::KIND_PHASE && e.phase == "design").unwrap();
    assert!(entry.at >= change.at, "the entry heads the new stay");

    // Moved on by the autopilot to the next state: no longer this stay's.
    return_to(&fx, "planning");
    assert_eq!(render_for_opening(&fx.conn, fx.node).unwrap(), "");
}

#[test]
fn a_send_back_that_names_no_obligation_is_not_a_dispute() {
    let fx = setup("review");
    let review = conversation(&fx, ProtocolKind::Review);
    send(&fx, ACTOR_AGENT, request(review, "design", "the approach is wrong")).unwrap();
    let opening = render_for_opening(&fx.conn, fx.node).unwrap();
    assert!(opening.contains("## Sent back") && !opening.contains("## Dispute"), "{opening}");
    assert!(open_dispute_into(&fx.conn, fx.node, "design").unwrap().is_none());
}

#[test]
fn the_row_is_journaled() {
    let fx = setup("review");
    let review = conversation(&fx, ProtocolKind::Review);
    send(&fx, ACTOR_AGENT, request(review, "active", "x")).unwrap();
    let journaled: i64 = fx
        .conn
        .query_row("SELECT COUNT(*) FROM journey_changes WHERE tbl = 'node_send_backs'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(journaled, 1);
}

// ── Disputes ────────────────────────────────────────────────────────────

fn requirement(fx: &Fx, index: usize, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    ObligationRepo::new(&fx.conn)
        .insert_at(id, fx.node, "requirement", index, None, body, "requirements")
        .unwrap();
    id
}

fn dispute(conversation_id: Uuid, to: &str, obligation: Uuid) -> Request {
    let mut asked = request(conversation_id, to, "this obligation cannot be met: it names a field that does not exist");
    asked.obligations = vec![obligation];
    asked
}

fn resolution(conversation_id: Uuid, kind: &str, note: &str) -> Resolution {
    Resolution { conversation_id, send_back: None, kind: kind.into(), note: note.into() }
}

fn resolve_as(fx: &Fx, actor: &str, resolution: Resolution) -> Result<Value> {
    crate::interview::execute(&fx.conn, Path::new("."), actor, &InterviewCommand::ResolveDispute(resolution))
}

#[test]
fn an_implementation_session_may_send_back_only_to_dispute() {
    let fx = setup("active");
    let design = obligation(&fx, 0, "Use the supplier id");
    let req = requirement(&fx, 1, "Show the price");
    let implement = conversation(&fx, ProtocolKind::Implementation);

    // A plain send-back is not an implementation session's.
    let err = send(&fx, ACTOR_AGENT, request(implement, "design", "wrong")).unwrap_err();
    assert!(err.to_string().contains("only to dispute an obligation"), "{err}");
    // A design obligation is disputed to design, a requirement to proposed.
    let err = send(&fx, ACTOR_AGENT, dispute(implement, "proposed", design)).unwrap_err();
    assert!(err.to_string().contains("goes to `design`"), "{err}");
    let err = send(&fx, ACTOR_AGENT, dispute(implement, "design", req)).unwrap_err();
    assert!(err.to_string().contains("goes to `proposed`"), "{err}");
    assert_eq!(state(&fx), "active");

    let out = send(&fx, ACTOR_AGENT, dispute(implement, "design", design)).unwrap();
    assert_eq!(out["outcome"], OUTCOME_MOVED, "{out}");
    assert_eq!(state(&fx), "design");
    let row = SendBackRepo::new(&fx.conn).list_for_node(fx.node).unwrap().remove(0);
    assert!(row.is_open_dispute());
    assert_eq!(row.protocol, "implementation");
}

#[test]
fn a_users_obligation_cannot_be_disputed() {
    let fx = setup("active");
    let design = obligation(&fx, 0, "Use the supplier id");
    ObligationRepo::new(&fx.conn).update_authority(design, "approved").unwrap();
    let implement = conversation(&fx, ProtocolKind::Implementation);
    let err = send(&fx, ACTOR_AGENT, dispute(implement, "design", design)).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("is the user's (approved)") && message.contains("decisions ask"), "{message}");
    assert_eq!(state(&fx), "active");
}

#[test]
fn the_phase_must_settle_a_dispute_before_it_certifies() {
    let fx = setup("active");
    let design = obligation(&fx, 0, "Use the supplier id");
    let implement = conversation(&fx, ProtocolKind::Implementation);
    send(&fx, ACTOR_AGENT, dispute(implement, "design", design)).unwrap();
    let opening = render_for_opening(&fx.conn, fx.node).unwrap();
    assert!(opening.contains("## Dispute") && opening.contains("Use the supplier id"), "{opening}");
    assert!(opening.contains("implementation session"), "{opening}");

    let phase = conversation(&fx, ProtocolKind::Phase);
    let repo = crate::phase::PhaseRepo::new(&fx.conn);
    let err = repo
        .record(fx.node, "design", crate::phase::PHASE_READY, Some(phase), crate::phase::CERTIFIER_SELF, "")
        .unwrap_err();
    assert!(err.to_string().contains("dispute") && err.to_string().contains("resolve-dispute"), "{err}");
    // The user may mark the phase done.
    repo.record(fx.node, "design", crate::phase::PHASE_CERTIFY, None, crate::phase::CERTIFIER_USER, "")
        .unwrap();

    // Only the receiving state's phase agent settles it.
    let err = resolve_as(&fx, ACTOR_AGENT, resolution(implement, "upheld", "it stands")).unwrap_err();
    assert!(err.to_string().contains("phase agent"), "{err}");
    let err = resolve_as(&fx, ACTOR_AGENT, resolution(phase, "upheld", " ")).unwrap_err();
    assert!(err.to_string().contains("needs a note"), "{err}");
    let err = resolve_as(&fx, ACTOR_AGENT, resolution(phase, "maybe", "x")).unwrap_err();
    assert!(err.to_string().contains("not a resolution"), "{err}");

    // `fixed` needs a change to a disputed obligation.
    let err = resolve_as(&fx, ACTOR_AGENT, resolution(phase, "fixed", "done")).unwrap_err();
    assert!(err.to_string().contains("was changed or removed"), "{err}");
    std::thread::sleep(std::time::Duration::from_millis(3));
    ObligationRepo::new(&fx.conn).update_body(design, "Use the supplier part number").unwrap();
    let out = resolve_as(&fx, ACTOR_AGENT, resolution(phase, "fixed", "now names the part number")).unwrap();
    assert_eq!(out["outcome"], RESOLUTION_FIXED, "{out}");
    assert!(open_dispute_into(&fx.conn, fx.node, "design").unwrap().is_none());
    let row = SendBackRepo::new(&fx.conn).list_for_node(fx.node).unwrap().remove(0);
    assert_eq!((row.resolution_kind.as_str(), row.resolution.as_str()), ("fixed", "now names the part number"));
    // Nothing left to settle: a second resolution is refused.
    let err = resolve_as(&fx, ACTOR_AGENT, resolution(phase, "upheld", "again")).unwrap_err();
    assert!(err.to_string().contains("no dispute is open"), "{err}");
    let entries = TimelineRepo::new(&fx.conn).list(fx.node).unwrap();
    assert!(entries.iter().any(|e| e.kind == KIND_SEND_BACK && e.title.contains("Dispute fixed")));
}

#[test]
fn an_upheld_dispute_is_shown_to_the_disputing_phase_when_the_node_comes_forward() {
    let fx = setup("active");
    let design = obligation(&fx, 0, "Use the supplier id");
    let implement = conversation(&fx, ProtocolKind::Implementation);
    send(&fx, ACTOR_AGENT, dispute(implement, "design", design)).unwrap();
    let phase = conversation(&fx, ProtocolKind::Phase);
    resolve_as(&fx, ACTOR_AGENT, resolution(phase, "upheld", "the supplier id is the org-wide key; see the ticket"))
        .unwrap();

    // Forward again through planning to the disputing phase.
    return_to(&fx, "planning");
    assert_eq!(render_for_opening(&fx.conn, fx.node).unwrap(), "");
    return_to(&fx, "active");
    let opening = render_for_opening(&fx.conn, fx.node).unwrap();
    assert!(opening.contains("## Dispute settled"), "{opening}");
    assert!(opening.contains("upheld") && opening.contains("org-wide key"), "{opening}");
    assert!(opening.contains("cannot be met"), "{opening}");
}

#[test]
fn a_dispute_passed_up_carries_the_chain_and_is_settled_by_the_phase_above() {
    let fx = setup("active");
    let design = obligation(&fx, 0, "Use the supplier id");
    let implement = conversation(&fx, ProtocolKind::Implementation);
    send(&fx, ACTOR_AGENT, dispute(implement, "design", design)).unwrap();
    let design_agent = conversation(&fx, ProtocolKind::Phase);

    // Only design passes up.
    let out = resolve_as(
        &fx,
        ACTOR_AGENT,
        resolution(design_agent, "passed_up", "the ticket itself names the supplier id"),
    )
    .unwrap();
    assert_eq!(out["outcome"], OUTCOME_MOVED, "{out}");
    assert_eq!(state(&fx), "proposed");
    let rows = SendBackRepo::new(&fx.conn).list_for_node(fx.node).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].resolution_kind, "passed_up");
    assert_eq!(rows[1].parent_id, Some(rows[0].id));
    assert_eq!((rows[1].from_state.as_str(), rows[1].to_state.as_str()), ("design", "proposed"));
    assert!(rows[1].is_open_dispute());

    // Proposed sees the whole chain.
    let opening = render_for_opening(&fx.conn, fx.node).unwrap();
    assert!(opening.contains("## Dispute"), "{opening}");
    assert!(opening.contains("from `active` to `design`") && opening.contains("from `design` to `proposed`"), "{opening}");
    assert!(opening.contains("the ticket itself names the supplier id"), "{opening}");

    // `proposed` cannot pass it further up; it upholds it, and the original is settled too.
    let proposed_agent = conversation(&fx, ProtocolKind::Phase);
    let err = resolve_as(&fx, ACTOR_AGENT, resolution(proposed_agent, "passed_up", "x")).unwrap_err();
    assert!(err.to_string().contains("only the design phase passes"), "{err}");
    resolve_as(&fx, ACTOR_AGENT, resolution(proposed_agent, "upheld", "the ticket does say it")).unwrap();
    let rows = SendBackRepo::new(&fx.conn).list_for_node(fx.node).unwrap();
    assert_eq!(rows[1].resolution_kind, "upheld");
    assert_eq!(rows[0].resolution_kind, "upheld");
    assert!(rows[0].resolution.contains("Settled in proposed") && rows[0].resolution.contains("the ticket does say it"));
}

#[test]
fn resolving_a_dispute_is_journaled() {
    let fx = setup("active");
    let design = obligation(&fx, 0, "Use the supplier id");
    let implement = conversation(&fx, ProtocolKind::Implementation);
    send(&fx, ACTOR_AGENT, dispute(implement, "design", design)).unwrap();
    let phase = conversation(&fx, ProtocolKind::Phase);
    resolve_as(&fx, ACTOR_AGENT, resolution(phase, "upheld", "it stands")).unwrap();
    let updates: i64 = fx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM journey_changes WHERE tbl = 'node_send_backs' AND op = 'update'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(updates, 1);
}
