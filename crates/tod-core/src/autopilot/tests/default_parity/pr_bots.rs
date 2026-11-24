use super::*;
use std::sync::Mutex;
use tod_store::github::{IssueComment, NodePr, PrMerge, PrSnapshot, PrStatus};

const PR: &str = "https://github.com/acme/app/pull/42";

struct BotFeed(Mutex<PrSnapshot>);

impl crate::pr_readiness::PrFeed for BotFeed {
    fn viewer_login(&self) -> Result<String, String> {
        Ok("tod-test-user".into())
    }

    fn snapshot(&self, _: &NodePr) -> Result<PrSnapshot, String> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn merge(&self, _: &NodePr) -> Result<PrMerge, String> {
        Ok(PrMerge {
            title: "Fixture pull request".into(),
            merged: false,
            merged_at: None,
        })
    }
}

#[test]
fn project_bot_threshold_applies_when_the_pull_request_leaves_draft() {
    let pair = Pair::new(automated_default(), "pr_draft", &[PR]);
    let exercise = |task: &Fixture| {
        std::fs::write(task.root.join("tod.yml"), "pr_readiness:\n  bots:\n    - name: greptile\n      min_score: 5\n      reviews_drafts: false\n")
            .unwrap();
        let conn = rusqlite::Connection::open(task.fleet.paths().db()).unwrap();
        assert!(
            conn.execute(
                "UPDATE gate_criteria SET active = 1 WHERE from_state IN ('pr_draft', 'pr_review')",
                []
            )
            .unwrap()
                > 0
        );
        task.fleet.reload_if_stale().unwrap();

        let feed = Arc::new(BotFeed(Mutex::new(PrSnapshot {
            status: PrStatus {
                mergeable: Some(true),
                mergeable_state: Some("blocked".into()),
                behind_by: Some(0),
                checks: Some("success".into()),
                head_sha: Some("abcdef1".into()),
                head_committed_at: Some("2020-01-01T00:00:00Z".into()),
                open: true,
                draft: true,
                ..Default::default()
            },
            threads: vec![],
            comments: vec![],
            reviews: vec![],
        })));
        crate::pr_readiness::set_feed_override(Some(feed.clone()));
        let mut agent = FakeAgent::new(&task.fleet);
        let now = chrono::Utc::now().timestamp_millis();
        require_timed_wait(
            run_babysitter(task, &mut agent),
            "marked ready for review",
            now,
        );
        assert_eq!(pending_specs(task), ["github:pr:42:ready_for_review"]);
        assert!(bot_decisions(task).is_empty());

        {
            let mut snapshot = feed.0.lock().unwrap();
            snapshot.status.draft = false;
            snapshot.status.reviewers = vec!["maria".into()];
            snapshot.status.mergeable_state = Some("clean".into());
            let reported = chrono::Utc::now().to_rfc3339();
            snapshot.comments.push(IssueComment {
                id: 1,
                author: Some("greptile-apps".into()),
                body: "Confidence Score: 4/5".into(),
                created_at: reported.clone(),
                updated_at: reported,
            });
        }
        let low = run_babysitter(task, &mut agent);
        assert_eq!(
            lifecycle::current_state(&task.fleet, task.node).unwrap(),
            "pr_review",
            "{low:?}"
        );
        assert!(
            agent.log.lock().unwrap().iter().any(|(kind, opening)| {
                *kind == ProtocolKind::Pr
                    && opening.contains("greptile scored it 4/5, under the 5/5 needed")
            }),
            "{low:?}"
        );

        feed.0.lock().unwrap().comments[0].body = "Confidence Score: 5/5".into();
        let now = chrono::Utc::now().timestamp_millis();
        require_timed_wait(run_babysitter(task, &mut agent), "to be merged", now);
        assert_eq!(
            lifecycle::current_state(&task.fleet, task.node).unwrap(),
            "approved"
        );
        crate::pr_readiness::set_feed_override(None);
        agent.turns
    };

    assert_eq!(exercise(&pair.workflow), exercise(&pair.legacy));
    assert_eq!(pair.graph_run().node, "approved");
}
