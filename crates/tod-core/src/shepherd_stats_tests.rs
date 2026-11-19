use super::*;
use rusqlite::params;
use tod_store::fleet::schema;
use tod_store::outline::repos::NodeRepo;
use tod_store::outline::uuid_blob::uuid_to_blob;
use uuid::Uuid;

struct Fx {
    dir: std::path::PathBuf,
    conn: Connection,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn fx() -> Fx {
    let dir = tod_test_util::scratch_dir(format!("tod-shepherd-stats-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
    Fx { dir, conn }
}

fn node(conn: &Connection, slug: &str) -> Uuid {
    let id = Uuid::new_v4();
    NodeRepo::new(conn).create_with_id(id, slug, slug).unwrap();
    id
}

fn conversation(conn: &Connection, node: Uuid, protocol: &str, from: &str, created: i64, updated: i64) -> Uuid {
    let id = Uuid::new_v4();
    conn.execute(
        "INSERT INTO conversations (id, focus_kind, focus_id, focus_node_id, created_at, updated_at, protocol, from_state)
         VALUES (?1, 'node', ?2, NULL, ?3, ?4, ?5, ?6)",
        params![uuid_to_blob(id), uuid_to_blob(node), created, updated, protocol, from],
    )
    .unwrap();
    id
}

fn agent_turn(conn: &Connection, conv: Uuid, seq: i64, parts: Option<&str>) {
    conn.execute(
        "INSERT INTO conversation_turns (id, conversation_id, seq, role, body, parts, created_at)
         VALUES (?1, ?2, ?3, 'agent', '', ?4, ?5)",
        // Each turn lands 30s after the previous one; the session starts at 1000.
        params![uuid_to_blob(Uuid::new_v4()), uuid_to_blob(conv), seq, parts, 1000 + 30_000 * seq],
    )
    .unwrap();
}

fn finding(conn: &Connection, node: Uuid, conv: Uuid, seq: i64, summary: &str, status: &str) {
    conn.execute(
        "INSERT INTO review_findings (id, node_id, conversation_id, seq, severity, file, summary, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'low', 'a.rs', ?5, ?6, 1, 1)",
        params![uuid_to_blob(Uuid::new_v4()), uuid_to_blob(node), uuid_to_blob(conv), seq, summary, status],
    )
    .unwrap();
}

fn all(conn: &Connection) -> Stats {
    let root = tod_test_util::scratch_dir(format!("tod-shepherd-stats-root-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    compute_with_root(conn, &Filter::default(), &Section::ALL, Some(&root)).unwrap()
}

#[test]
fn cost_counts_sessions_turns_and_tool_calls_per_node_and_phase() {
    let f = fx();
    let n = node(&f.conn, "alpha");
    let c = conversation(&f.conn, n, "phase", "design", 1000, 61_000);
    agent_turn(
        &f.conn,
        c,
        1,
        Some(r#"[{"kind":"text","text":"x"},{"kind":"tool","id":"1","title":"t"},{"kind":"tool","id":"2","title":"u"}]"#),
    );
    agent_turn(&f.conn, c, 2, None);
    conversation(&f.conn, n, "implementation", "", 1000, 2000);
    let cost = all(&f.conn).cost.unwrap();
    let design = cost.rows.iter().find(|r| r.phase == "phase:design").unwrap();
    assert_eq!((design.sessions, design.agent_turns, design.tool_calls, design.wall_ms), (1, 2, 2, 60_000));
    assert!(cost.rows.iter().any(|r| r.phase == "implementation" && r.sessions == 1));
    assert_eq!(cost.by_phase.len(), 2);
    assert!(cost.unavailable.iter().any(|u| u.starts_with("tokens")), "tokens are named as unavailable");
}

#[test]
fn an_idle_gap_between_turns_is_not_counted_as_wall_time() {
    let f = fx();
    let n = node(&f.conn, "alpha");
    let c = conversation(&f.conn, n, "fix", "", 1000, 1000);
    agent_turn(&f.conn, c, 1, None); // 30s after the start
    // A turn two hours later: the session was idle, not working.
    f.conn
        .execute(
            "INSERT INTO conversation_turns (id, conversation_id, seq, role, body, created_at)
             VALUES (?1, ?2, 2, 'agent', '', ?3)",
            params![uuid_to_blob(Uuid::new_v4()), uuid_to_blob(c), 31_000 + 2 * 3600 * 1000],
        )
        .unwrap();
    let cost = all(&f.conn).cost.unwrap();
    assert_eq!(cost.rows[0].wall_ms, 30_000);
}

#[test]
fn a_turn_whose_parts_are_not_json_is_an_error_not_a_zero() {
    let f = fx();
    let n = node(&f.conn, "alpha");
    let c = conversation(&f.conn, n, "fix", "", 1, 2);
    agent_turn(&f.conn, c, 1, Some("not json"));
    let err = compute(&f.conn, &Filter::default(), &[Section::Cost]).unwrap_err();
    assert!(err.to_string().contains("not JSON"), "{err}");
}

#[test]
fn review_rounds_clean_rounds_reopened_and_the_rejected_ratio() {
    let f = fx();
    let n = node(&f.conn, "alpha");
    let r1 = conversation(&f.conn, n, "review", "review", 10, 11);
    let r2 = conversation(&f.conn, n, "review", "review", 20, 21);
    let _r3 = conversation(&f.conn, n, "review", "review", 30, 31);
    finding(&f.conn, n, r1, 1, "Null  deref", "fixed");
    finding(&f.conn, n, r1, 2, "Typo", "rejected");
    // Round 2 finds the fixed one again (spacing and case differ), plus a new one.
    finding(&f.conn, n, r2, 3, "null deref", "fixed");
    finding(&f.conn, n, r2, 4, "New", "declined");
    let review = all(&f.conn).review.unwrap();
    let row = &review.rows[0];
    assert_eq!((row.rounds, row.findings, row.clean_rounds, row.reopened), (3, 4, 1, 1));
    assert_eq!(review.by_status["fixed"], 2);
    assert_eq!(review.rejected_or_declined_ratio, Some(0.5));
}

#[test]
fn a_draft_review_is_its_own_stay_and_a_second_node_is_its_own_row() {
    let f = fx();
    let a = node(&f.conn, "alpha");
    let b = node(&f.conn, "beta");
    conversation(&f.conn, a, "review", "review", 1, 2);
    conversation(&f.conn, a, "review", "pr_draft", 3, 4);
    conversation(&f.conn, b, "review", "pr_draft", 3, 4);
    let stats = all(&f.conn);
    assert_eq!(stats.review.unwrap().rows.len(), 3);
    assert_eq!(stats.pr_draft.unwrap().draft_review_rounds, 2);
}

#[test]
fn pr_loop_counts_agent_sessions_threads_pushes_and_ci_failures() {
    let f = fx();
    let n = node(&f.conn, "alpha");
    conversation(&f.conn, n, "pr", "pr_draft", 1, 2);
    let r = conversation(&f.conn, n, "review", "pr_draft", 1, 2);
    finding(&f.conn, n, r, 1, "x", "fixed");
    f.conn.execute("UPDATE review_findings SET pr_posted_at = 5", []).unwrap();
    for (kind, summary) in [
        ("push", "push to b"),
        ("push", "push to b"),
        ("check_run", "check_run failure on b"),
        ("check_run", "check_run success on b"),
    ] {
        tod_store::node_events::record(&f.conn, n, "github", kind, "", summary, "{}", 9).unwrap();
    }
    let pr = all(&f.conn).pr_draft.unwrap();
    assert_eq!((pr.pr_agent_sessions, pr.findings_posted_as_threads, pr.pushes, pr.ci_failures), (1, 1, 2, 1));
    assert_eq!(pr.ci_failures_per_push, Some(0.5));
    assert!(pr.unavailable.iter().any(|u| u.contains("bot review rounds")));
}

#[test]
fn requests_count_by_reason_answer_kind_and_wait() {
    let f = fx();
    let n = node(&f.conn, "alpha");
    let ask = |reason: &str, status: &str, created: i64| {
        let id = Uuid::new_v4();
        f.conn
            .execute(
                "INSERT INTO decisions (id, node_id, question, options, evidence, status, created_at, reason)
                 VALUES (?1, ?2, 'q', '[]', '[]', ?3, ?4, ?5)",
                params![uuid_to_blob(id), uuid_to_blob(n), status, created, reason],
            )
            .unwrap();
        id
    };
    let answer = |id: Uuid, option: Option<i64>, text: Option<&str>, actor: &str, at: i64| {
        f.conn
            .execute(
                "INSERT INTO decision_answers (decision_id, option_index, text, actor, answered_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![uuid_to_blob(id), option, text, actor, at],
            )
            .unwrap();
    };
    let a = ask("broken", "answered", 0);
    answer(a, None, Some("Fixed, try again"), "user", 30_000);
    let b = ask("risk", "answered", 0);
    answer(b, Some(1), None, "shepherd:cli-1", 90_000);
    ask("conflict", "pending", 0);
    ask("other", "withdrawn", 0);
    let r = all(&f.conn).requests.unwrap();
    assert_eq!((r.total, r.pending, r.answered, r.withdrawn), (4, 1, 2, 1));
    assert_eq!((r.answered_with_text, r.answered_with_offered_option, r.carry_on_like, r.about_setup), (1, 1, 1, 1));
    assert_eq!((r.answered_by_user, r.answered_by_other), (1, 1));
    assert_eq!(r.max_wait_secs, Some(90));
    assert_eq!(r.pending_wait_secs.len(), 1);
    assert_eq!(r.by_reason["risk"], 1);
}

#[test]
fn stops_count_the_standing_ones_and_name_what_has_no_history() {
    let f = fx();
    let n = node(&f.conn, "alpha");
    f.conn
        .execute(
            "INSERT INTO run_state (node_id, phase, reason_kind, reason_text, since, resumable_by, updated_at, writer)
             VALUES (?1, 'stopped', 'no_progress', 't', 1, 'you', 1, 'w')",
            params![uuid_to_blob(n)],
        )
        .unwrap();
    let s = all(&f.conn).stops.unwrap();
    assert_eq!(s.stopped_by_reason["no_progress"], 1);
    assert_eq!(s.stopped_by_resumable_by["you"], 1);
    assert!(s.run_state_changes_journaled >= 1, "the journal trigger saw the insert");
    assert!(s.unavailable.iter().any(|u| u.contains("resumed with no change")));
}

#[test]
fn sandbox_churn_counts_locations_made_beyond_the_first_and_a_loss() {
    let f = fx();
    let n = node(&f.conn, "alpha");
    let make = |sandbox: &str| {
        f.conn
            .execute(
                "INSERT INTO node_files_locations (node_id, source_node_id, recipe, sandbox, created_at, updated_at)
                 VALUES (?1, ?1, 'r', ?2, 1, 1)",
                params![uuid_to_blob(n), sandbox],
            )
            .unwrap();
    };
    make("s1");
    f.conn.execute("DELETE FROM node_files_locations", []).unwrap();
    make("s2");
    f.conn
        .execute(
            "INSERT INTO cloud_nodes (node_id, sandbox, user_name, accepted_at, lost_at) VALUES (?1, 's2', 'u', 1, 5)",
            params![uuid_to_blob(n)],
        )
        .unwrap();
    let s = all(&f.conn).sandbox.unwrap();
    let row = &s.rows[0];
    assert_eq!((row.locations_made, row.locations_removed, row.replacements, row.lost_now), (2, 1, 1, true));
    assert_eq!((s.replacements, s.nodes_with_replacements, s.lost_now), (1, 1, 1));
}

#[test]
fn a_node_filter_and_a_since_filter_narrow_every_section() {
    let f = fx();
    let a = node(&f.conn, "alpha");
    let b = node(&f.conn, "beta");
    conversation(&f.conn, a, "implementation", "", 100, 200);
    conversation(&f.conn, b, "implementation", "", 100, 200);
    let only_a = compute(&f.conn, &Filter { node: Some("alpha".into()), since_ms: None }, &[Section::Cost]).unwrap();
    assert_eq!(only_a.cost.unwrap().rows.len(), 1);
    let late = compute(&f.conn, &Filter { node: None, since_ms: Some(150) }, &[Section::Cost]).unwrap();
    assert!(late.cost.unwrap().rows.is_empty());
}

#[test]
fn the_text_report_names_every_section_and_what_is_unavailable() {
    let f = fx();
    let text = all(&f.conn).text();
    for head in ["COST", "REVIEW LOOP", "PR LOOP", "REQUESTS", "STOPS", "SANDBOX CHURN", "GIT RETRIES", "not available"] {
        assert!(text.contains(head), "missing {head}:\n{text}");
    }
}

#[test]
fn an_unknown_section_is_an_error_naming_the_choices() {
    let err = Section::parse("bogus").unwrap_err().to_string();
    assert!(err.contains("bogus") && err.contains("pr-draft"), "{err}");
}

#[test]
fn git_retries_count_calls_retries_and_outcomes_per_node_and_workdir() {
    use tod_store::git_retries::{CLASS_LOCK_RACE, CLASS_NETWORK, GitRetry, GitRetryRepo};
    let f = fx();
    let a = node(&f.conn, "alpha");
    let b = node(&f.conn, "beta");
    let repo = GitRetryRepo::new(&f.conn);
    repo.record(&GitRetry::new(Some(a), "/r", "fetch", CLASS_LOCK_RACE, 2, true, "")).unwrap();
    repo.record(&GitRetry::new(Some(a), "/r", "submodule update", CLASS_NETWORK, 3, false, "early EOF")).unwrap();
    repo.record(&GitRetry::new(Some(b), "/r", "fetch", CLASS_NETWORK, 1, true, "")).unwrap();
    repo.record(&GitRetry::new(None, "/elsewhere", "clone", CLASS_NETWORK, 1, true, "")).unwrap();
    let g = compute(&f.conn, &Filter::default(), &[Section::GitRetries]).unwrap().git_retries.unwrap();
    assert_eq!((g.calls, g.retries, g.recovered, g.gave_up), (4, 7, 3, 1));
    let alpha = g.rows.iter().find(|r| r.subject == "alpha").unwrap();
    assert_eq!((alpha.calls, alpha.retries, alpha.recovered, alpha.gave_up, alpha.lock_race, alpha.network), (2, 5, 1, 1, 1, 1));
    assert_eq!(alpha.by_op["fetch"], 1);
    assert!(g.rows.iter().any(|r| r.subject == "/elsewhere"), "a retry for no node is counted against its workdir");
    let only = compute(&f.conn, &Filter { node: Some("beta".into()), since_ms: None }, &[Section::GitRetries]).unwrap();
    assert_eq!(only.git_retries.unwrap().calls, 1);
    let text = compute(&f.conn, &Filter::default(), &[Section::GitRetries]).unwrap().text();
    assert!(text.contains("GIT RETRIES: 4 calls retried (7 retries): 3 recovered, 1 gave up") && text.contains("not available"), "{text}");
}

#[test]
fn stop_mismatch_counts_a_stopped_run_shown_as_working_and_not_a_consistent_one() {
    use tod_store::run_state::{Draft, Phase};
    let f = fx();
    let hidden = node(&f.conn, "hidden");
    let plain = node(&f.conn, "plain");
    let now = tod_store::outline::uuid_blob::now_ms();
    for n in [hidden, plain] {
        NodeRepo::new(&f.conn).set_lifecycle(n, "active").unwrap();
        let draft = Draft {
            phase: Phase::Stopped,
            reason_kind: crate::run_state::kind::BUDGET.into(),
            reason_text: "spent its budget".into(),
            resumable_by: tod_store::run_state::RESUMABLE_BY_YOU.into(),
        };
        tod_store::run_state::set(&f.conn, n, &draft, "test", now - 90_000).unwrap();
    }
    // Something is processing `hidden`, so its standing says working.
    tod_store::activity::ActivityRepo::new(&f.conn)
        .start(Uuid::new_v4(), hidden, tod_store::activity::KIND_RUN, "test", now)
        .unwrap();
    let root = tod_test_util::scratch_dir(format!("tod-stop-mismatch-root-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let m = compute_with_root(&f.conn, &Filter::default(), &[Section::StopMismatch], Some(&root))
        .unwrap()
        .stop_mismatch
        .unwrap();
    assert_eq!(m.count, 1, "{m:?}");
    assert_eq!(m.rows[0].node, "hidden");
    assert!(m.rows[0].lasted_secs >= 90, "{m:?}");
    assert_eq!(m.rows[0].shown, "working");
    let err = compute(&f.conn, &Filter::default(), &[Section::StopMismatch]).unwrap_err().to_string();
    assert!(err.contains("data root"), "{err}");
}
