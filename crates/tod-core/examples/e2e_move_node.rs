//! Live check (real Claude, a real dev container, a real billed Blaxel
//! sandbox): one conversation about one node keeps its agent session as the
//! node's Files settings move it host -> dev container -> cloud sandbox ->
//! host, through the app's own path (`ConversationDriver`, the session-log
//! mirror, `provision::remove_location` as the Files impact dialog runs it).
//!
//! `e2e_move_node <data root> <container> <sandbox image> <host repo>`
//! - the data root holds `sandboxes.toml` (Blaxel account) and is otherwise empty;
//! - the container is running, has `claude-agent-acp`, `CLAUDE_CODE_OAUTH_TOKEN` in its
//!   environment, and a repo at /work with an `origin`;
//! - the image holds a repo at /root/app with an `origin` (see tod-store's `e2e_env_bake`);
//! - the host repo has an `origin`.
//! The token comes from `TOD_TEST_CLAUDE_TOKEN` and is never printed. Deletes the
//! sandboxes it made.
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tod_agent::{AgentBackend, AgentLaunchOptions, AgentPlatform};
use tod_core::conversation::driver::{ConversationConfig, ConversationDriver, ConversationEvent, SharedAgentAccess};
use tod_core::media::MediaPaths;
use tod_store::conversation::{ConversationRepo, Focus, ProtocolKind, TurnRole};
use tod_store::fleet::repos::task::FleetTask;
use tod_store::fleet::sandbox::Sandboxes;
use tod_store::fleet::{DevContainerSetting, SandboxFrom, FleetMutation, FleetStore, provision, session_log};
use tod_store::outline::{OutlineMutation, types::Capability};

/// What changing the Files settings does (the capabilities editor's write).
fn set_files(store: &FleetStore, node: uuid::Uuid, repo: &str, dev: Option<DevContainerSetting>) -> anyhow::Result<()> {
    store.enqueue_outline(OutlineMutation::SetNodeFiles {
        node_id: node,
        repo: Some(repo.to_string()),
        branch: None,
        use_worktree: true,
        dev_container: dev,
    })?;
    store.writer().flush()?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let root = PathBuf::from(a.next().expect("data root"));
    let container = a.next().expect("container");
    let image = a.next().expect("sandbox image");
    let host_repo = a.next().expect("host repo");
    let token = std::env::var("TOD_TEST_CLAUDE_TOKEN").expect("TOD_TEST_CLAUDE_TOKEN");
    // The host's agent and the sandbox's proxy; the container has it in its own environment.
    // SAFETY: set before any other thread starts.
    unsafe { std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", &token) };
    tod_store::fleet::sandbox::set_data_root(&root);
    tod_store::paths::set_data_root(root.clone());
    tod_store::fleet::sandbox::set_claude_token(&root, &token)?;

    let store = FleetStore::open(&root)?;
    let node_id = uuid::Uuid::new_v4();
    store.enqueue(FleetMutation::InsertTask {
        task: FleetTask {
            id: node_id.to_string(),
            title: "Move node".into(),
            slug: "move-node".into(),
            lifecycle: "active".into(),
            repo: Some(host_repo.clone()),
            branch: None,
            notes: vec![],
            tags: vec![],
            ticket: None,
            linked_prs: vec![],
        },
    })?;
    store.writer().flush()?;
    store.enqueue_outline(OutlineMutation::EnableCapabilities { node_id, capabilities: vec![Capability::Files] })?;
    store.writer().flush()?;
    let nid = node_id.to_string();
    let paths = tod_store::paths::TodPaths::at(&root);
    let settings = tod_store::settings::TodSettings::load(&paths)?;

    let config = ConversationConfig {
        data_root: root.clone(),
        media: MediaPaths::discover()?,
        launch: AgentLaunchOptions::for_platform(AgentPlatform::Claude),
        settings_path: None,
        context: Default::default(),
    };
    let agent = AgentBackend::Claude.create(tod_agent::agent_traffic::shared_log());
    let mut driver = ConversationDriver::new(config, Focus::Node(node_id), ProtocolKind::Outline);

    let mut turn = |driver: &mut ConversationDriver, text: &str| -> anyhow::Result<String> {
        // What the driver is about to ask for, so a failure is not hidden by its scratch fallback.
        match provision::resolve_launch_cwd_with(&store, &nid, &mut |m| println!("    {m}")) {
            Ok((dir, warnings)) => println!("  the agent will run in {dir} {warnings:?}"),
            Err(e) => println!("  NO LOCATION: {e:#}"),
        }
        let mut access = SharedAgentAccess(&agent);
        driver.send(&store, &mut access, text)?;
        let start = Instant::now();
        loop {
            let events = driver.tick(&store, &mut access);
            if let Some(ConversationEvent::TurnFinished { error }) =
                events.iter().find(|e| matches!(e, ConversationEvent::TurnFinished { .. }))
            {
                if let Some(error) = error {
                    anyhow::bail!("turn failed: {error}");
                }
                break;
            }
            anyhow::ensure!(start.elapsed() < Duration::from_secs(420), "turn timed out");
            std::thread::sleep(Duration::from_millis(400));
        }
        let id = driver.conversation_id().unwrap();
        let turns = store.read(|c| ConversationRepo::new(c).turns(id))?;
        Ok(turns.iter().rev().find(|t| t.role == TurnRole::Agent).map(|t| t.body.clone()).unwrap_or_default())
    };

    let mirror = session_log::local_dir(&root, &nid);
    let settle_mirror = |what: &str| {
        // The copy into the mirror runs in the background after the turn.
        let start = Instant::now();
        let mut last = (0u64, Instant::now());
        while start.elapsed() < Duration::from_secs(90) {
            let size: u64 = std::fs::read_dir(&mirror)
                .map(|d| d.filter_map(|e| e.ok()).filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
                .unwrap_or(0);
            if size != last.0 {
                last = (size, Instant::now());
            } else if size > 0 && last.1.elapsed() > Duration::from_secs(4) {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        println!("  mirror after {what}: {} bytes", last.0);
    };
    // What the Files impact dialog does before a settings change.
    let leave = |settings: &tod_store::settings::TodSettings| -> anyhow::Result<()> {
        if let Some(loc) = provision::node_location(&store, &nid)? {
            println!("  removing {}", loc.describe());
            provision::remove_location(&store, &paths, settings, &nid, &mut |m| println!("    {m}"))?;
        }
        Ok(())
    };

    let failures = std::cell::RefCell::new(Vec::<String>::new());
    let check = |leg: &str, reply: &str, wants: &[&str]| {
        let missing: Vec<_> = wants.iter().filter(|w| !reply.to_uppercase().contains(&w.to_uppercase())).collect();
        println!("[{leg}] reply: {}", reply.replace('\n', " "));
        if missing.is_empty() {
            println!("[{leg}] PASS");
        } else {
            println!("[{leg}] FAIL: missing {missing:?}");
            failures.borrow_mut().push(format!("{leg}: missing {missing:?}"));
        }
    };

    let result = (|| -> anyhow::Result<()> {
        println!("== start on the host");
        set_files(&store, node_id, &host_repo, None)?;
        let r = turn(&mut driver, "My favourite fruit is pineapple. Please keep that in mind. Reply with just OK.")?;
        println!("  turn 1 reply: {}", r.replace('\n', " "));
        let session = store.read(|c| ConversationRepo::new(c).get(driver.conversation_id().unwrap()))?.and_then(|c| c.agent_session_id);
        println!("  session id: {session:?}");
        settle_mirror("host turn");

        println!("== leg 1: host -> dev container");
        leave(&settings)?;
        set_files(&store, node_id, "/work", Some(DevContainerSetting { container: Some(container.clone()), ..Default::default() }))?;
        let r = turn(&mut driver, "What is my favourite fruit? Also, my second favourite fruit is banana. Reply with the fruit I told you first, and your current working directory, nothing else.")?;
        check("leg 1 host->container", &r, &["PINEAPPLE", "/work"]);
        settle_mirror("container turn");

        println!("== leg 2: dev container -> cloud sandbox");
        leave(&settings)?;
        set_files(&store, node_id, "/root/app", Some(DevContainerSetting { sandbox: true, sandbox_from: SandboxFrom::Image(image.clone()), ..Default::default() }))?;
        let r = turn(&mut driver, "Which fruits have I told you about so far? My third favourite fruit is mango. Reply with the fruits I told you about before this message and your current working directory, nothing else.")?;
        check("leg 2 container->sandbox", &r, &["PINEAPPLE", "BANANA", "/root/app"]);
        settle_mirror("sandbox turn");

        println!("== leg 3: cloud sandbox -> host");
        leave(&settings)?;
        set_files(&store, node_id, &host_repo, None)?;
        let r = turn(&mut driver, "Which fruits have I told you about so far? Reply with all three and your current working directory, nothing else.")?;
        check("leg 3 sandbox->host", &r, &["PINEAPPLE", "BANANA", "MANGO"]);

        let id = driver.conversation_id().unwrap();
        let (conv, turns) = store.read(|c| {
            let repo = ConversationRepo::new(c);
            Ok((repo.get(id)?, repo.turns(id)?))
        })?;
        let rotations = turns.iter().filter(|t| t.role == TurnRole::Rotation).count();
        println!("session id now: {:?}; rotations (fresh sessions): {rotations}", conv.and_then(|c| c.agent_session_id));
        if rotations > 0 {
            failures.borrow_mut().push(format!("{rotations} fresh session(s) were started"));
        }
        Ok(())
    })();
    if let Err(e) = &result {
        println!("ERROR: {e:#}");
        failures.borrow_mut().push(format!("{e:#}"));
    }

    // Whatever happened, remove what it made.
    let _ = leave(&settings);
    drop(store);
    if let Ok(mut sb) = Sandboxes::load(&root)
        && let Ok(bx) = sb.blaxel()
        && let Ok(list) = bx.list()
    {
        for info in list {
            let name = info.name.clone();
            if name.contains(&nid[..8]) || name.contains("move-node") {
                let _ = sb.delete(&bx, &name);
                println!("deleted sandbox {name}");
            }
        }
    }
    let failures = failures.into_inner();
    if failures.is_empty() {
        println!("ALL LEGS PASSED");
        Ok(())
    } else {
        anyhow::bail!("{} failure(s): {failures:?}", failures.len())
    }
}
