//! Live check (a real, billed Blaxel sandbox, deleted at the end) of the
//! orchestrator's GitHub access through its proxy:
//! `cargo run -p tod-core --example e2e_orchestrator_github -- <real data root> <scratch root> <probe sandbox name> <owner/repo> <pr number>`
//! needs `scripts/build-sandbox-binaries.sh` first, a Blaxel account in
//! `<real data root>/sandboxes.toml`, and a stored GitHub token that reads the
//! repository. It deploys a throwaway orchestrator (`<probe sandbox name>`),
//! gives it the stored token through `orchestrator_github::apply`, seeds a
//! node in `approved` with the (unmerged) pull request linked and its GitHub
//! wait due, and checks that the orchestrator itself read GitHub: the wait was
//! re-recorded for the next look, and no sandbox was poked. No token is printed.
use std::path::{Path, PathBuf};
use tod_core::cloud_sync::{self, Orchestrator};
use tod_store::fleet::FleetStore;
use tod_store::fleet::sandbox::{Sandboxes, set_data_root};
use tod_store::orchestrator_github::{Config, Entry};
use tod_store::outline::{Capability, CreatePosition, OutlineMutation};
use tod_store::waits::{NewWait, WaitRepo};
use uuid::Uuid;

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let real = PathBuf::from(a.next().expect("real data root"));
    let scratch = PathBuf::from(a.next().expect("scratch root"));
    let name = a.next().expect("probe sandbox name");
    let repo = a.next().expect("owner/repo");
    let pr: i64 = a.next().expect("pr number").parse()?;
    let (owner, repo_name) = repo.split_once('/').expect("owner/repo");

    // A scratch root that shares the real account and credentials but names
    // the probe as its orchestrator.
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)?;
    let toml = std::fs::read_to_string(real.join("sandboxes.toml"))?;
    let head = toml.split("[[sandbox]]").next().unwrap_or_default();
    std::fs::write(scratch.join("sandboxes.toml"), head.replace("[blaxel]\n", &format!("[blaxel]\norchestrator = \"{name}\"\n")))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(real.join("credentials"), scratch.join("credentials"))?;
    set_data_root(&scratch);

    let github_token = tod_store::credentials::CredentialStore::from_data_root(&real)
        .get(tod_store::credentials::CredentialKind::GithubToken)
        .expect("a stored GitHub token");
    let sandboxes = Sandboxes::load(&scratch)?;
    let bx = sandboxes.blaxel()?;
    let account = sandboxes.account()?.clone();
    let bin = Path::new("target/sandbox");
    let read = |n: &str| std::fs::read(bin.join(n)).unwrap_or_else(|e| panic!("target/sandbox/{n}: {e}"));
    let (orchestrator_bin, tod_cli, relay) = (read("tod-orchestrator"), read("tod-cli"), read("tod-relay"));
    let spec = tod_sandbox::orchestrator::Spec {
        name: &name,
        volume: None,
        move_data: false,
        backup: None,
        image: &account.default_image,
        region: &account.region,
        memory_mb: account.memory_mb,
        orchestrator: &orchestrator_bin,
        tod_cli: &tod_cli,
        relay: &relay,
        blaxel_workspace: bx.workspace(),
        blaxel_token: bx.token(),
    };

    let result = (|| -> anyhow::Result<()> {
        let url = tod_sandbox::orchestrator::provision(&bx, &spec, &mut |m| eprintln!("progress: {m}"))?;
        println!("probe orchestrator up at {url}");

        // The token, as the settings would hold it.
        let id = "e2e-probe";
        Config { tokens: vec![Entry { id: id.into(), label: "e2e probe".into(), repos: vec![format!("{owner}/*")] }] }.save(&scratch)?;
        tod_store::orchestrator_github::set_token(&scratch, id, &github_token)?;
        let applied = tod_core::orchestrator_github::apply(&scratch);
        tod_store::orchestrator_github::delete_token(&scratch, id);
        println!("apply: {:?}", applied?);

        // A node in `approved`, its pull request linked, its GitHub wait due.
        let app = scratch.join("app");
        let fleet = FleetStore::open(&app)?;
        fleet.enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })?;
        fleet.writer().flush()?;
        let list_id = fleet.list_outline_lists()?[0].id;
        let node = Uuid::new_v4();
        fleet.enqueue_outline(OutlineMutation::CreateNode {
            node_id: Some(node), list_id, parent_id: None, anchor_id: None, position: CreatePosition::Below, title: "e2e".into(),
        })?;
        fleet.enqueue_outline(OutlineMutation::EnableCapabilities { node_id: node, capabilities: vec![Capability::Ticket] })?;
        fleet.writer().flush()?;
        fleet.enqueue(tod_store::fleet::FleetMutation::UpdateTaskLinkedPrs {
            id: node.to_string(),
            linked_prs: vec![format!("https://github.com/{owner}/{repo_name}/pull/{pr}")],
        })?;
        fleet.writer().flush()?;
        let _ = fleet.reload_if_stale();
        tod_core::lifecycle::set_lifecycle(&fleet, node, "approved")?;
        let (wait, user) = {
            let conn = rusqlite::Connection::open(app.join("tod.db"))?;
            tod_store::cloud_nodes::upsert(&conn, node, "no-such-sandbox", "x", 1)?;
            let w = WaitRepo::new(&conn).create(node, &NewWait::event(format!("github:pr:{pr}:merged"), 0))?.id;
            (w, ())
        };
        let _ = user;
        let _ = fleet.reload_if_stale();
        let (orch, user) = cloud_sync::resolve(&scratch)?;
        let snap = scratch.join("snap.db");
        tod_store::sync::snapshot(&app.join("tod.db"), &snap)?;
        orch.seed(&user, &std::fs::read(&snap)?)?;

        // The wake, due now: the orchestrator's timer takes it.
        let base = format!("{}/port/{}", url.trim_end_matches('/'), tod_sandbox::orchestrator::PORT);
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let wake = serde_json::json!({"id": wait.to_string(), "user": user, "node": node.to_string(), "sandbox": "no-such-sandbox", "at": 0});
        let resp = agent
            .post(&format!("{base}/wakes"))
            .header("Authorization", &format!("Bearer {}", bx.token()))
            .header("X-Tod-User", &user)
            .send_json(&wake)?;
        println!("wake accepted: {}", resp.status());

        // Give the timer a moment, then read what the orchestrator wrote.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            let mut resp = agent
                .get(&format!("{base}/users/{user}/snapshot"))
                .header("Authorization", &format!("Bearer {}", bx.token()))
                .call()?;
            let bytes = resp.body_mut().with_config().limit(1 << 30).read_to_vec()?;
            let copy = scratch.join("remote.db");
            std::fs::write(&copy, bytes)?;
            let conn = rusqlite::Connection::open(&copy)?;
            let waits = WaitRepo::new(&conn).list_pending_for_node(node)?;
            let lost: Option<i64> = conn.query_row(
                "SELECT lost_at FROM cloud_nodes WHERE node_id = ?1",
                rusqlite::params![tod_store::outline::uuid_blob::uuid_to_blob(node)],
                |r| r.get(0),
            )?;
            let moved = waits.iter().find(|w| w.id != wait);
            if let Some(w) = moved {
                println!("the orchestrator read GitHub itself: the wait was re-recorded: {:?}", w.detail);
                println!("sandbox poked (marked lost): {}", lost.is_some());
                anyhow::ensure!(lost.is_none(), "the gate poked a sandbox it should have left asleep");
                return Ok(());
            }
            if lost.is_some() {
                anyhow::bail!("the orchestrator poked the node's sandbox instead of answering the wait itself (it could not read GitHub; see its log)");
            }
            if std::time::Instant::now() > deadline {
                anyhow::bail!("nothing happened to the wait within 90s");
            }
        }
    })();
    if std::env::var("E2E_KEEP").is_ok() {
        println!("kept {name}");
    } else {
        bx.delete(&name)?;
        println!("deleted {name}");
    }
    result
}
