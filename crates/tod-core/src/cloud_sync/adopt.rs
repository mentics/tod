//! Adopting a sandbox a node was started in before its Files location was the
//! one record of it (`doc/runner-menu/implementation.md`, B5).
//!
//! A node started before B2/B3 has a `cloud_nodes` row, and maybe a legacy
//! `cloud-sync.json` record, naming a sandbox, but no location row. The app
//! reports such a node and acts on nothing until it is adopted: the location
//! is recorded naming that sandbox, with the checkout and branch found by
//! asking the sandbox (a schema migration cannot). Nothing is guessed:
//!
//! - [`decide`] is the pure rule: the one sandbox the records name must exist
//!   at Blaxel and be able to run, be a node's (`tod-kind` is `node` or
//!   `node-base`), not be another node's (`TOD_NODE`), and no other sandbox
//!   may claim the node. The automatic form (at the app's start) also needs
//!   the sandbox to say it is this node's; the user's own form (the runner
//!   menu's "Adopt existing sandbox") accepts a sandbox that does not say
//!   whose it is.
//! - [`adopt_with`] runs it over a [`Backend`]: the relay must answer before
//!   anything is written, then the store records the location
//!   (`provision::adopt_location`, whose write also moves the cloud row's copy).
//! - A sandbox Blaxel no longer has is said, never replaced: Start remakes it.
//!   A sandbox whose name does not follow `tod-<slug>-<6hex>` is adopted by
//!   its recorded name (names are enforced only at creation).
//!
//! Blocks on the network: never call it on the UI thread.

use super::{CloudSyncState, STATE_FILE, client_id, sandbox_dead_reason, sandbox_deleted_by_user};
use anyhow::{Context, Result, anyhow, bail};
use tod_sandbox::blaxel::SandboxInfo;
use tod_store::cloud_nodes;
use tod_store::fleet::{FleetMutation, FleetStore, provision, session_log};

/// The sandboxes the node's old records name, when the node is in the state
/// adoption is for; `None` otherwise (it has a sandbox location, its Files do
/// not run in a sandbox, nothing names a sandbox, the record is lost or was
/// deleted on purpose, or it is another client's). Store and file reads only.
pub fn claimed_sandboxes(fleet: &FleetStore, root: &std::path::Path, node_id: &str) -> Result<Option<Vec<String>>> {
    let uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    if session_log::node_sandbox(fleet, node_id)?.is_some() {
        return Ok(None);
    }
    let in_sandbox = fleet.resolve_files_for_node(node_id)?.is_some_and(|files| files.runs_in_sandbox());
    if !in_sandbox || sandbox_deleted_by_user(root, node_id) {
        return Ok(None);
    }
    let me = client_id(root)?;
    let mut names: Vec<String> = Vec::new();
    let mut push = |name: &str| {
        if !names.iter().any(|n| n == name) {
            names.push(name.to_string());
        }
    };
    if let Some(row) = fleet.read(|c| cloud_nodes::get(c, uuid))? {
        if row.lost_at.is_some() || row.ownership.owner_client.as_deref().is_some_and(|owner| owner != me) {
            return Ok(None);
        }
        push(&row.sandbox);
    }
    if let Some(legacy) = CloudSyncState::load(root)?.nodes.get(node_id)
        && legacy.lost_at.is_none()
    {
        push(&legacy.sandbox);
    }
    Ok((!names.is_empty()).then_some(names))
}

/// Whether the node is in the state adoption is for (the runner menu's
/// entry shows then). It says nothing of whether adoption will succeed.
pub fn adoptable(fleet: &FleetStore, root: &std::path::Path, node_id: &str) -> Result<bool> {
    Ok(claimed_sandboxes(fleet, root, node_id)?.is_some())
}

/// What the pure rule says to do.
#[derive(Debug, Clone)]
pub enum Verdict {
    /// Adopt this sandbox (its relay must still answer).
    Adopt(SandboxInfo),
    /// Blaxel has no such sandbox, or it can no longer run: leave the node
    /// for Start to remake.
    Gone { sandbox: String, why: String },
    /// Not decided, and not guessed: why.
    Refused(String),
}

/// Decides whether `claimed` (what the node's old records name) is adopted,
/// against Blaxel's `listing`. `automatic` is the stricter form that runs
/// without the user: the sandbox must say it is this node's.
pub fn decide(node_id: &str, claimed: &[String], listing: &[SandboxInfo], automatic: bool) -> Verdict {
    let [name] = claimed else {
        return Verdict::Refused(format!(
            "the records of node {node_id} name more than one sandbox ({}); it is not clear which is its own, so none is adopted",
            claimed.join(", ")
        ));
    };
    let Some(info) = listing.iter().find(|s| &s.name == name) else {
        return Verdict::Gone { sandbox: name.clone(), why: format!("Blaxel no longer has sandbox {name}") };
    };
    if let Some(why) = sandbox_dead_reason(info) {
        return Verdict::Gone { sandbox: name.clone(), why };
    }
    match info.label("tod-kind") {
        Some("node" | "node-base") => {}
        other => {
            return Verdict::Refused(format!(
                "sandbox {name} is not a node's sandbox (its tod-kind is {}), so it is not adopted for node {node_id}",
                other.unwrap_or("not set")
            ));
        }
    }
    match info.env("TOD_NODE") {
        Some(owner) if owner != node_id => {
            return Verdict::Refused(format!("sandbox {name} belongs to node {owner}, not node {node_id}; it is not adopted"));
        }
        None if automatic => {
            return Verdict::Refused(format!(
                "sandbox {name} does not say which node it is for, so it is not adopted automatically; \
                 use \"Adopt existing sandbox\" in the node's runner menu to adopt it"
            ));
        }
        _ => {}
    }
    let rivals: Vec<&str> = listing
        .iter()
        .filter(|s| &s.name != name && s.env("TOD_NODE") == Some(node_id) && sandbox_dead_reason(s).is_none())
        .map(|s| s.name.as_str())
        .collect();
    if !rivals.is_empty() {
        return Verdict::Refused(format!(
            "node {node_id}'s records name sandbox {name}, but {} also say they are for it; \
             it is not clear which is its own, so none is adopted",
            rivals.join(", ")
        ));
    }
    Verdict::Adopt(info.clone())
}

/// What asking Blaxel and the sandbox takes, so the rule runs without either.
pub trait Backend {
    /// The workspace's sandboxes.
    fn list(&mut self) -> Result<Vec<SandboxInfo>>;
    /// Whether `sandbox`'s relay answers as a node's. Also makes the sandbox
    /// known to this machine's `sandboxes.toml`, so commands can reach it.
    fn verify_relay(&mut self, sandbox: &SandboxInfo) -> Result<()>;
    /// Where the checkout is in `sandbox`, and the branch it is on.
    fn probe_checkout(&mut self, sandbox: &str) -> Result<(String, String)>;
}

/// What an adoption did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Adopted { sandbox: String, repo: String, branch: String },
    /// Nothing to adopt: the node is not in the state adoption is for.
    NotNeeded,
    /// Blaxel no longer has the sandbox; nothing was changed.
    Gone { sandbox: String, why: String },
}

impl Outcome {
    /// For the panel and the log.
    pub fn message(&self) -> String {
        match self {
            Outcome::Adopted { sandbox, repo, branch } => {
                format!("Adopted sandbox {sandbox}: its checkout is {repo}, on branch {branch}.")
            }
            Outcome::NotNeeded => "Nothing to adopt: the node's Files location already records its sandbox, or the node has none to adopt.".into(),
            Outcome::Gone { sandbox, why } => {
                format!("{why}, so sandbox {sandbox} was not adopted and nothing was changed. Start makes the node a new one.")
            }
        }
    }
}

/// Adopts the sandbox `node_id`'s old records name, through `backend`.
/// Idempotent. Refusals and failures are errors naming what failed; a sandbox
/// that is gone is an [`Outcome::Gone`] (nothing is written).
pub fn adopt_with(
    fleet: &FleetStore,
    root: &std::path::Path,
    node_id: &str,
    automatic: bool,
    backend: &mut dyn Backend,
    progress: &mut dyn FnMut(&str),
) -> Result<Outcome> {
    let Some(claimed) = claimed_sandboxes(fleet, root, node_id)? else {
        return Ok(Outcome::NotNeeded);
    };
    progress("Looking at the workspace's sandboxes…");
    let listing = backend.list().context("list the workspace's sandboxes")?;
    let info = match decide(node_id, &claimed, &listing, automatic) {
        Verdict::Adopt(info) => info,
        Verdict::Gone { sandbox, why } => return Ok(Outcome::Gone { sandbox, why }),
        Verdict::Refused(why) => bail!("{why}"),
    };
    let name = info.name.clone();
    progress(&format!("Checking that {name}'s relay answers…"));
    backend.verify_relay(&info).with_context(|| format!("sandbox {name} was not adopted: its relay does not answer"))?;
    progress(&format!("Finding the checkout in {name}…"));
    let (repo, branch) = backend.probe_checkout(&name).with_context(|| format!("sandbox {name} was not adopted"))?;
    // The location and, when the node has a cloud row, its copy.
    provision::adopt_location(fleet, node_id, &name, &repo).with_context(|| format!("adopt sandbox {name}"))?;
    adopt_legacy_record(fleet, root, node_id)?;
    Ok(Outcome::Adopted { sandbox: name, repo, branch })
}

/// The node's legacy record in [`STATE_FILE`], if it has one and no row:
/// moved into the database (its sandbox now comes from the location just
/// recorded) and dropped from the file. A node with a row keeps it; the
/// file's record is dropped either way. Safe to repeat.
fn adopt_legacy_record(fleet: &FleetStore, root: &std::path::Path, node_id: &str) -> Result<()> {
    let uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    let mut state = CloudSyncState::load(root)?;
    let Some(legacy) = state.nodes.get(node_id).cloned() else { return Ok(()) };
    if fleet.read(|c| cloud_nodes::get(c, uuid))?.is_none() {
        fleet.enqueue(FleetMutation::CloudNodeUpsert { node: uuid, user: legacy.user, accepted_at: legacy.accepted_at_ms })?;
        fleet.writer().flush()?;
        let _ = fleet.reload_if_stale();
    }
    state.nodes.remove(node_id);
    state.save(root).with_context(|| format!("drop the node's old record from {STATE_FILE}"))
}

/// The real thing: Blaxel and the sandbox's relay.
pub struct Live {
    sandboxes: tod_store::fleet::sandbox::Sandboxes,
    bx: tod_sandbox::blaxel::Blaxel,
}

impl Live {
    pub fn open(root: &std::path::Path) -> Result<Self> {
        let sandboxes = tod_store::fleet::sandbox::Sandboxes::load(root)?;
        let bx = sandboxes.blaxel()?;
        Ok(Self { sandboxes, bx })
    }
}

impl Backend for Live {
    fn list(&mut self) -> Result<Vec<SandboxInfo>> {
        self.bx.list()
    }

    fn verify_relay(&mut self, info: &SandboxInfo) -> Result<()> {
        let url = info.url.clone().ok_or_else(|| anyhow!("Blaxel gives sandbox {} no URL", info.name))?;
        self.bx
            .wait_reachable(&url, std::time::Duration::from_secs(180))
            .with_context(|| format!("wake {}", info.name))?;
        let Some(manifest) = tod_sandbox::provision::relay_manifest(&self.bx, &url) else {
            bail!("the relay in {} does not answer", info.name);
        };
        if !tod_sandbox::provision::is_node_manifest(&manifest) {
            bail!("{} reports manifest {manifest:?}, which is not a node sandbox's", info.name);
        }
        // Commands reach a sandbox through its entry in sandboxes.toml.
        self.sandboxes.config.upsert(tod_sandbox::config::Sandbox {
            name: info.name.clone(),
            image: info.image.clone(),
            url: Some(url),
            agents: true,
        });
        self.sandboxes.save()
    }

    fn probe_checkout(&mut self, sandbox: &str) -> Result<(String, String)> {
        let repo = provision::find_sandbox_checkout(sandbox)?;
        let branch = provision::sandbox_checkout_branch(sandbox, &repo)?;
        Ok((repo, branch))
    }
}

/// The user's own adoption (the runner menu's "Adopt existing sandbox"), the
/// message for the panel. An error names what failed.
pub fn adopt_node_sandbox(fleet: &FleetStore, node_id: &str, progress: &mut dyn FnMut(&str)) -> Result<String> {
    let root = fleet.paths().root().to_path_buf();
    let mut live = Live::open(&root)?;
    adopt_with(fleet, &root, node_id, false, &mut live, progress).map(|outcome| outcome.message())
}

/// The nodes this client's records name a sandbox for with no location, as
/// adoption sees them: every `cloud_nodes` row and legacy record.
fn candidates(fleet: &FleetStore, root: &std::path::Path) -> Result<Vec<String>> {
    let mut ids: Vec<String> = fleet.read(cloud_nodes::list)?.iter().map(|r| r.node_id.to_string()).collect();
    ids.extend(CloudSyncState::load(root)?.nodes.keys().cloned());
    ids.sort();
    ids.dedup();
    let mut out = Vec::new();
    for id in ids {
        if uuid::Uuid::parse_str(&id).is_ok() && fleet.get_node(&id)?.is_some() && adoptable(fleet, root, &id)? {
            out.push(id);
        }
    }
    Ok(out)
}

/// The automatic form, once at the app's start: adopts a node's sandbox only
/// when nothing is in doubt (one sandbox named, it says it is this node's,
/// no other claims the node, its relay answers). Anything else is told to the
/// user through the node's problems and left for the runner menu's entry.
/// Returns the sandboxes adopted. Does nothing, and asks Blaxel nothing,
/// when no node is in the state.
pub fn adopt_at_start(fleet: &FleetStore) -> Result<Vec<String>> {
    let root = fleet.paths().root().to_path_buf();
    let nodes = candidates(fleet, &root)?;
    if nodes.is_empty() {
        return Ok(Vec::new());
    }
    let mut live = Live::open(&root)?;
    Ok(adopt_each(fleet, &root, &nodes, &mut live))
}

/// [`adopt_at_start`] over `backend` for `nodes`, listing Blaxel once.
fn adopt_each(fleet: &FleetStore, root: &std::path::Path, nodes: &[String], backend: &mut dyn Backend) -> Vec<String> {
    struct Cached<'a> {
        inner: &'a mut dyn Backend,
        listing: Option<Vec<SandboxInfo>>,
    }
    impl Backend for Cached<'_> {
        fn list(&mut self) -> Result<Vec<SandboxInfo>> {
            if self.listing.is_none() {
                self.listing = Some(self.inner.list()?);
            }
            Ok(self.listing.clone().unwrap_or_default())
        }
        fn verify_relay(&mut self, sandbox: &SandboxInfo) -> Result<()> {
            self.inner.verify_relay(sandbox)
        }
        fn probe_checkout(&mut self, sandbox: &str) -> Result<(String, String)> {
            self.inner.probe_checkout(sandbox)
        }
    }
    let mut cached = Cached { inner: backend, listing: None };
    let mut adopted = Vec::new();
    for id in nodes {
        match adopt_with(fleet, root, id, true, &mut cached, &mut |msg| tracing::info!("cloud: adopting node {id}'s sandbox: {msg}")) {
            Ok(Outcome::Adopted { sandbox, .. }) => {
                tracing::info!("cloud: adopted sandbox {sandbox} for node {id}");
                adopted.push(sandbox);
            }
            Ok(Outcome::NotNeeded) => {}
            Ok(gone @ Outcome::Gone { .. }) => {
                session_log::report_problem(id, format!("The node's old sandbox was not adopted. {}", gone.message()))
            }
            Err(err) => session_log::report_problem(
                id,
                format!("The node's old sandbox was not adopted automatically: {err:#}. Nothing was changed."),
            ),
        }
    }
    adopted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner_place::test_support::{sandbox as sandbox_setting, set_files};
    use tod_store::outline::{CreatePosition, OutlineMutation};
    use uuid::Uuid;

    const NODE: &str = "3f9a1c2e-0000-4000-8000-000000000000";

    fn info(name: &str, kind: Option<&str>, node: Option<&str>, status: &str) -> SandboxInfo {
        SandboxInfo {
            name: name.into(),
            status: status.into(),
            state: None,
            url: Some(format!("https://{name}.example")),
            image: "img".into(),
            labels: kind.map(|k| vec![("tod-kind".to_string(), k.to_string())]).unwrap_or_default(),
            volumes: Vec::new(),
            node_env: node.map(|n| vec![("TOD_NODE".to_string(), n.to_string())]).unwrap_or_default(),
        }
    }

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_node_sandbox_that_says_it_is_the_nodes_is_adopted_by_either_form() {
        let listing = [info("old-name", Some("node"), Some(NODE), "DEPLOYED"), info("other", Some("node"), Some("x"), "DEPLOYED")];
        for automatic in [false, true] {
            // A name outside the tod-<slug>-<6hex> scheme is adopted as recorded.
            let verdict = decide(NODE, &names(&["old-name"]), &listing, automatic);
            assert!(matches!(&verdict, Verdict::Adopt(i) if i.name == "old-name"), "{verdict:?}");
        }
    }

    #[test]
    fn a_sandbox_blaxel_lacks_or_cannot_run_is_gone_and_never_guessed_at() {
        let listing = [info("dead", Some("node"), Some(NODE), "FAILED")];
        assert!(matches!(decide(NODE, &names(&["missing"]), &listing, false), Verdict::Gone { sandbox, .. } if sandbox == "missing"));
        assert!(matches!(decide(NODE, &names(&["dead"]), &listing, true), Verdict::Gone { why, .. } if why.contains("FAILED")));
    }

    #[test]
    fn two_names_or_a_rival_claim_is_ambiguous() {
        let listing = [info("a", Some("node"), Some(NODE), "DEPLOYED"), info("b", Some("node"), Some(NODE), "DEPLOYED")];
        let both = decide(NODE, &names(&["a", "b"]), &listing, false);
        assert!(matches!(&both, Verdict::Refused(why) if why.contains("more than one")), "{both:?}");
        let rival = decide(NODE, &names(&["a"]), &listing, false);
        assert!(matches!(&rival, Verdict::Refused(why) if why.contains("also say") && why.contains('b')), "{rival:?}");
        // A rival that cannot run does not count.
        let listing = [info("a", Some("node"), Some(NODE), "DEPLOYED"), info("b", Some("node"), Some(NODE), "TERMINATED")];
        assert!(matches!(decide(NODE, &names(&["a"]), &listing, false), Verdict::Adopt(_)));
    }

    #[test]
    fn a_sandbox_of_another_kind_or_another_node_is_refused() {
        let golden = [info("g", Some("golden"), None, "DEPLOYED")];
        assert!(matches!(decide(NODE, &names(&["g"]), &golden, false), Verdict::Refused(w) if w.contains("golden")));
        let unlabelled = [info("u", None, Some(NODE), "DEPLOYED")];
        assert!(matches!(decide(NODE, &names(&["u"]), &unlabelled, false), Verdict::Refused(w) if w.contains("not set")));
        let theirs = [info("t", Some("node"), Some("someone-else"), "DEPLOYED")];
        assert!(matches!(decide(NODE, &names(&["t"]), &theirs, false), Verdict::Refused(w) if w.contains("belongs to node someone-else")));
    }

    #[test]
    fn only_the_users_own_form_accepts_a_sandbox_that_does_not_say_whose_it_is() {
        let listing = [info("s", Some("node-base"), None, "DEPLOYED")];
        assert!(matches!(decide(NODE, &names(&["s"]), &listing, false), Verdict::Adopt(_)));
        assert!(matches!(decide(NODE, &names(&["s"]), &listing, true), Verdict::Refused(w) if w.contains("Adopt existing sandbox")));
    }

    struct Fx {
        root: std::path::PathBuf,
        fleet: FleetStore,
        node: Uuid,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// A node whose Files run in a cloud sandbox and that has no location.
    fn fx() -> Fx {
        let root = std::env::temp_dir().join(format!("tod-adopt-{}", Uuid::new_v4()));
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
                title: "N".into(),
            })
            .unwrap();
        fleet.writer().flush().unwrap();
        set_files(&fleet, node, sandbox_setting(), false);
        Fx { root, fleet, node }
    }

    impl Fx {
        fn id(&self) -> String {
            self.node.to_string()
        }

        /// What an older build left: a row naming the sandbox, no location.
        fn raw_row(&self, sandbox: &str) {
            let conn = rusqlite::Connection::open(self.fleet.paths().db()).unwrap();
            cloud_nodes::upsert(&conn, self.node, sandbox, "alice", 1).unwrap();
            drop(conn);
            self.fleet.reload_if_stale().unwrap();
        }

        fn legacy(&self, sandbox: &str) {
            let mut state = CloudSyncState::load(&self.root).unwrap();
            state.nodes.insert(
                self.id(),
                super::super::LegacyCloudNode { sandbox: sandbox.into(), user: "alice".into(), accepted_at_ms: 7, lost_at: None },
            );
            state.save(&self.root).unwrap();
        }

        fn located(&self) -> Option<String> {
            self.fleet.reload_if_stale().unwrap();
            session_log::node_sandbox(&self.fleet, &self.id()).unwrap()
        }

        fn row(&self) -> Option<cloud_nodes::CloudNodeRow> {
            self.fleet.reload_if_stale().unwrap();
            self.fleet.read(|c| cloud_nodes::get(c, self.node)).unwrap()
        }

        fn run(&self, automatic: bool, fake: &mut Fake) -> Result<Outcome> {
            adopt_with(&self.fleet, &self.root, &self.id(), automatic, fake, &mut |_| {})
        }
    }

    struct Fake {
        listing: Vec<SandboxInfo>,
        relay_ok: bool,
        calls: Vec<&'static str>,
    }

    impl Fake {
        fn with(listing: Vec<SandboxInfo>) -> Self {
            Self { listing, relay_ok: true, calls: Vec::new() }
        }
    }

    impl Backend for Fake {
        fn list(&mut self) -> Result<Vec<SandboxInfo>> {
            self.calls.push("list");
            Ok(self.listing.clone())
        }
        fn verify_relay(&mut self, _: &SandboxInfo) -> Result<()> {
            self.calls.push("relay");
            if self.relay_ok { Ok(()) } else { bail!("the relay does not answer") }
        }
        fn probe_checkout(&mut self, _: &str) -> Result<(String, String)> {
            self.calls.push("probe");
            Ok(("/workspace/repo".into(), "task/n".into()))
        }
    }

    #[test]
    fn a_row_with_no_location_is_adopted_and_the_cloud_copy_stays() {
        let fx = fx();
        fx.raw_row("old-name");
        assert!(adoptable(&fx.fleet, &fx.root, &fx.id()).unwrap());
        let mut fake = Fake::with(vec![info("old-name", Some("node"), Some(&fx.id()), "DEPLOYED")]);
        let outcome = fx.run(false, &mut fake).unwrap();
        assert_eq!(
            outcome,
            Outcome::Adopted { sandbox: "old-name".into(), repo: "/workspace/repo".into(), branch: "task/n".into() }
        );
        assert_eq!(fake.calls, ["list", "relay", "probe"], "the relay is checked before anything is written");
        assert_eq!(fx.located().as_deref(), Some("old-name"));
        let location = tod_store::fleet::provision::node_location(&fx.fleet, &fx.id()).unwrap().unwrap();
        assert_eq!(location.repo.as_deref(), Some("/workspace/repo"));
        assert_eq!(fx.row().unwrap().sandbox, "old-name");
        assert!(!adoptable(&fx.fleet, &fx.root, &fx.id()).unwrap(), "no longer in the state");
    }

    #[test]
    fn adopting_twice_changes_nothing_the_second_time() {
        let fx = fx();
        fx.raw_row("old-name");
        let mut fake = Fake::with(vec![info("old-name", Some("node"), Some(&fx.id()), "DEPLOYED")]);
        fx.run(false, &mut fake).unwrap();
        let mut again = Fake::with(Vec::new());
        assert_eq!(fx.run(false, &mut again).unwrap(), Outcome::NotNeeded);
        assert!(again.calls.is_empty(), "nothing asked of Blaxel");
        assert_eq!(fx.located().as_deref(), Some("old-name"));
        // The store write alone is idempotent too, and refuses another name.
        let same = tod_store::fleet::provision::adopt_location(&fx.fleet, &fx.id(), "old-name", "/workspace/repo").unwrap();
        assert_eq!(same.sandbox(), Some("old-name"));
        let err = tod_store::fleet::provision::adopt_location(&fx.fleet, &fx.id(), "another", "/w").unwrap_err();
        assert!(format!("{err:#}").contains("already records sandbox old-name"), "{err:#}");
    }

    #[test]
    fn a_legacy_json_record_is_adopted_into_a_row_and_dropped_from_the_file() {
        let fx = fx();
        fx.legacy("legacy-sbx");
        let mut fake = Fake::with(vec![info("legacy-sbx", Some("node"), Some(&fx.id()), "DEPLOYED")]);
        fx.run(true, &mut fake).unwrap();
        assert_eq!(fx.located().as_deref(), Some("legacy-sbx"));
        let row = fx.row().unwrap();
        assert_eq!((row.sandbox.as_str(), row.user.as_str(), row.accepted_at), ("legacy-sbx", "alice", 7));
        assert!(CloudSyncState::load(&fx.root).unwrap().nodes.is_empty());
        let mut again = Fake::with(Vec::new());
        assert_eq!(fx.run(true, &mut again).unwrap(), Outcome::NotNeeded);
        assert_eq!(fx.row().unwrap().sandbox, "legacy-sbx");
    }

    #[test]
    fn a_sandbox_that_is_gone_writes_nothing_and_says_so() {
        let fx = fx();
        fx.raw_row("vanished");
        let mut fake = Fake::with(vec![info("someone-elses", Some("node"), Some("x"), "DEPLOYED")]);
        let outcome = fx.run(false, &mut fake).unwrap();
        assert!(matches!(&outcome, Outcome::Gone { sandbox, .. } if sandbox == "vanished"));
        assert!(outcome.message().contains("Start makes the node a new one"));
        assert_eq!(fx.located(), None);
        assert_eq!(fx.row().unwrap().sandbox, "vanished");
    }

    #[test]
    fn a_relay_that_does_not_answer_or_a_refusal_writes_nothing_and_names_what_failed() {
        let fx = fx();
        fx.raw_row("s1");
        let mut fake = Fake::with(vec![info("s1", Some("node"), Some(&fx.id()), "DEPLOYED")]);
        fake.relay_ok = false;
        let err = format!("{:#}", fx.run(false, &mut fake).unwrap_err());
        assert!(err.contains("s1 was not adopted") && err.contains("relay does not answer"), "{err}");
        assert_eq!(fx.located(), None);

        let mut golden = Fake::with(vec![info("s1", Some("golden"), None, "DEPLOYED")]);
        let err = format!("{:#}", fx.run(false, &mut golden).unwrap_err());
        assert!(err.contains("golden"), "{err}");
        assert!(!golden.calls.contains(&"relay"));
        assert_eq!(fx.located(), None);
    }

    #[test]
    fn the_rows_and_the_legacy_record_naming_different_sandboxes_is_ambiguous() {
        let fx = fx();
        fx.raw_row("one");
        fx.legacy("two");
        let mut fake = Fake::with(vec![
            info("one", Some("node"), Some(&fx.id()), "DEPLOYED"),
            info("two", Some("node"), Some(&fx.id()), "DEPLOYED"),
        ]);
        let err = format!("{:#}", fx.run(false, &mut fake).unwrap_err());
        assert!(err.contains("more than one sandbox"), "{err}");
        assert_eq!(fx.located(), None);
    }

    #[test]
    fn a_lost_deleted_or_other_clients_row_is_not_in_the_state() {
        let fx = fx();
        fx.raw_row("s");
        let conn = rusqlite::Connection::open(fx.fleet.paths().db()).unwrap();
        cloud_nodes::mark_lost(&conn, fx.node, 5).unwrap();
        drop(conn);
        fx.fleet.reload_if_stale().unwrap();
        assert!(!adoptable(&fx.fleet, &fx.root, &fx.id()).unwrap(), "lost");

        let fx = fx2_with_row();
        super::super::set_sandbox_deleted_by_user(&fx.root, &fx.id(), true).unwrap();
        assert!(!adoptable(&fx.fleet, &fx.root, &fx.id()).unwrap(), "deleted by the user");

        let fx = fx2_with_row();
        fx.fleet
            .enqueue(FleetMutation::CloudNodeSetOwnership {
                node: fx.node,
                ownership: cloud_nodes::Ownership { workspace: None, owner_client: Some("another-machine".into()), repo: None },
            })
            .unwrap();
        fx.fleet.writer().flush().unwrap();
        fx.fleet.reload_if_stale().unwrap();
        assert!(!adoptable(&fx.fleet, &fx.root, &fx.id()).unwrap(), "another client's");
    }

    fn fx2_with_row() -> Fx {
        let fx = fx();
        fx.raw_row("s");
        fx
    }

    #[test]
    fn a_node_whose_files_are_not_a_sandbox_or_that_is_located_is_not_in_the_state() {
        let fx = fx();
        fx.raw_row("s");
        set_files(&fx.fleet, fx.node, None, false);
        assert!(!adoptable(&fx.fleet, &fx.root, &fx.id()).unwrap());
        let mut fake = Fake::with(Vec::new());
        assert_eq!(fx.run(false, &mut fake).unwrap(), Outcome::NotNeeded);
        assert!(fake.calls.is_empty());
    }

    #[test]
    fn the_automatic_form_lists_blaxel_once_and_adopts_only_what_is_unambiguous() {
        let fx = fx();
        fx.raw_row("clear");
        let mut fake = Fake::with(vec![info("clear", Some("node"), Some(&fx.id()), "DEPLOYED")]);
        let adopted = adopt_each(&fx.fleet, &fx.root, &[fx.id()], &mut fake);
        assert_eq!(adopted, ["clear"]);
        assert_eq!(fake.calls.iter().filter(|c| **c == "list").count(), 1);

        // One that does not say whose it is is left, and told to the node.
        let fx = fx2_named("quiet");
        let mut fake = Fake::with(vec![info("quiet", Some("node"), None, "DEPLOYED")]);
        assert!(adopt_each(&fx.fleet, &fx.root, &[fx.id()], &mut fake).is_empty());
        assert_eq!(fx.located(), None);
        assert!(!fake.calls.contains(&"relay"));
    }

    fn fx2_named(sandbox: &str) -> Fx {
        let fx = fx();
        fx.raw_row(sandbox);
        fx
    }
}
