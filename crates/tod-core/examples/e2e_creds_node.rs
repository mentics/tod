//! Live check (real, billed Blaxel sandbox; the user's real credentials in the
//! credential store): an INTERACTIVE Files "Cloud sandbox" node's proxy rules,
//! a real Claude turn through `AgentEnvironment::Sandbox`, git/gh over HTTPS
//! through the GitHub rule, and `tod-cli environment request` over the tunnel.
//! `e2e_creds_node <data root> <tod-cli> <image>` (image from `e2e_env_bake`).
//! Never prints a credential value (every output is redacted against the
//! stored values); deletes what it made.
use std::io::Write;
use std::process::{Command, Stdio};
use tod_agent::{AgentLaunchOptions, AgentPlatform, AgentProvider, AgentRunState, RoutingAgentProvider, SessionPurpose, SessionTurn};
use tod_store::fleet::repos::task::FleetTask;
use tod_store::fleet::sandbox::{SandboxExec, Sandboxes};
use tod_store::fleet::{FleetMutation, FleetStore, provision, session_log};
use tod_store::outline::{OutlineMutation, types::Capability};
use tod_store::{CredentialKind, CredentialStore};

const V1: &str = "DUMMY-node-secret-one-515151";
const V2: &str = "DUMMY-node-secret-two-626262";

thread_local! { static SECRETS: std::cell::RefCell<Vec<String>> = Default::default(); }

fn redact(s: &str) -> String {
    let mut s = s.replace(V1, "<<V1>>").replace(V2, "<<V2>>");
    SECRETS.with(|v| {
        for x in v.borrow().iter() {
            if x.len() >= 8 {
                s = s.replace(x.as_str(), "<<REAL-TOKEN>>");
            }
        }
    });
    s
}

fn exec(name: &str, script: &str) -> String {
    match SandboxExec::new(name).output("/root/app", "sh", &["-c", script]) {
        Ok(o) => redact(&format!("exit={} {}{}", o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => format!("exec error: {e:#}"),
    }
}

/// `sh -c script` through the agent launch path (env, cli relay, tunnel).
fn via_agent(root: &std::path::Path, name: &str, script: &str) -> String {
    let cwd = tod_store::fleet::Workdir::sandbox(name, "/root/app");
    let launch = match tod_store::fleet::dev_container::sandbox_launch_for(&cwd, root) {
        Ok(Some(l)) => l,
        other => return format!("no launch: {other:?}"),
    };
    let mut c = launch.agent_command("sh", &["-c".to_string(), script.to_string()], &[]);
    // Keep stdin open (as ACP does): an EOF there ends the attach before the output is back.
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return format!("agent launch error: {e}"),
    };
    let stdin = child.stdin.take();
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let read = |mut r: Box<dyn std::io::Read + Send>| std::thread::spawn(move || { let mut b = Vec::new(); let _ = r.read_to_end(&mut b); b });
    let (to, te) = (read(Box::new(out)), read(Box::new(err)));
    let status = child.wait();
    drop(stdin);
    let (o, e) = (to.join().unwrap_or_default(), te.join().unwrap_or_default());
    redact(&format!("exit={:?} {}{}", status.ok().and_then(|s| s.code()), String::from_utf8_lossy(&o), String::from_utf8_lossy(&e)))
}

fn claude_turn(root: &std::path::Path, name: &str, key: &str, resume: Option<String>, message: &str) -> (String, Option<String>) {
    let cwd = tod_store::fleet::Workdir::sandbox(name, "/root/app");
    let environment = tod_store::fleet::dev_container::environment_for(None, &cwd, root).expect("environment");
    println!(
        "agent launch env: {:?}",
        match &environment {
            tod_agent::AgentEnvironment::Sandbox(l) => l
                .env
                .iter()
                .map(|(k, v)| format!("{k}={}", if k.contains("TOKEN") && !v.contains("placeholder") { "<<NOT-A-PLACEHOLDER>>".to_string() } else { redact(v) }))
                .collect::<Vec<_>>(),
            _ => vec!["NOT A SANDBOX ENV".into()],
        }
    );
    let log = tod_agent::agent_traffic::shared_log();
    let mut p = RoutingAgentProvider::new(log);
    let turn = SessionTurn {
        key: key.into(),
        owner_id: "e2e".into(),
        title: "e2e creds".into(),
        cwd: std::path::PathBuf::from("/root/app"),
        options: AgentLaunchOptions::for_platform(AgentPlatform::Claude),
        resume_session_id: resume,
        opening: None,
        message: message.into(),
        images: vec![],
        purpose: SessionPurpose::Chat,
        env: vec![],
        environment,
    };
    let h = match p.send_session_turn(turn) {
        Ok(h) => h,
        Err(e) => return (format!("send error: {}", redact(&format!("{e:#}"))), None),
    };
    let t = std::time::Instant::now();
    let out = loop {
        if t.elapsed().as_secs() > 240 {
            break "TIMEOUT".to_string();
        }
        match p.poll_run(h.id.clone()) {
            Some(AgentRunState::Success(r)) => break format!("SUCCESS reply={:?}", r.map(|r| redact(&r))),
            Some(AgentRunState::Failure(e)) => break format!("FAILURE {}", redact(&e)),
            Some(AgentRunState::NeedsPermission(req)) => {
                println!("permission asked: {}", redact(&req.title));
                if let Some(o) = req.options.first() {
                    let _ = p.respond_to_permission(h.id.clone(), &o.id);
                }
            }
            _ => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    };
    let sid = p.session_id(key);
    p.close_session(key);
    (format!("{out} in {:?}", t.elapsed()), sid)
}

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let root = std::path::PathBuf::from(a.next().unwrap());
    let cli = std::path::PathBuf::from(a.next().unwrap());
    let image = a.next().unwrap();
    tod_store::fleet::sandbox::set_data_root(&root);
    tod_store::paths::set_data_root(root.clone());
    let creds = CredentialStore::from_data_root(&root);
    for k in [CredentialKind::GithubToken, CredentialKind::LinearApiKey, CredentialKind::ClaudeOauthToken] {
        let v = creds.get(k);
        println!("keyring {}: present={}", k.name(), v.is_some());
        if let Some(v) = v {
            SECRETS.with(|s| s.borrow_mut().push(v));
        }
    }
    let store = FleetStore::open(&root)?;
    let node_id = uuid::Uuid::new_v4();
    store.enqueue(FleetMutation::InsertTask {
        task: FleetTask {
            id: node_id.to_string(),
            title: "Env node".into(),
            slug: "env-node".into(),
            lifecycle: "active".into(),
            repo: Some("/root/app".into()),
            branch: None,
            notes: vec![],
            tags: vec![],
            ticket: None,
            linked_prs: vec![],
        },
    })?;
    store.writer().flush()?;
    store.enqueue_outline(OutlineMutation::EnableCapabilities { node_id, capabilities: vec![Capability::Files, Capability::Environment] })?;
    store.writer().flush()?;
    drop(store);
    let host = |args: &[&str], stdin: Option<&str>| -> String {
        let mut c = Command::new(&cli);
        c.arg("--data-root").arg(&root).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut ch = c.spawn().unwrap();
        if let Some(s) = stdin {
            ch.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
        } else {
            drop(ch.stdin.take());
        }
        let o = ch.wait_with_output().unwrap();
        redact(&format!("exit={:?} {}{}", o.status.code(), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    };
    println!("files: {}", host(&["capabilities", "set", "env-node", "files", "--dir", "/root/app", "--sandbox", &format!("image:{image}")], None));
    println!("add-secret: {}", host(&["environment", "add-secret", "node_key", "--host", "httpbin.org", "--auth", "header:X-Dummy-Key", "--node", "env-node"], None));
    host(&["environment", "set-secret", "node_key", "--node", "env-node"], Some(&format!("{V1}\n")));
    let store = FleetStore::open(&root)?;
    let nid = node_id.to_string();
    let mut progress = |m: &str| eprintln!("progress: {m}");
    let t = std::time::Instant::now();
    let made = provision::resolve_launch_cwd_with(&store, &nid, &mut progress);
    println!("make location: {:?} in {:?}", made.as_ref().map(|(d, w)| (d.to_string(), w.clone())).map_err(|e| format!("{e:#}")), t.elapsed());
    let name = provision::node_location(&store, &nid)?.and_then(|l| l.sandbox().map(str::to_string)).unwrap_or_default();
    println!("sandbox name: {name}");
    let mut slot = Some(store);
    let result = if name.is_empty() { Ok(()) } else { run_checks(&root, &mut slot, &nid, &name, &host) };
    let store = match slot.take() { Some(s) => s, None => FleetStore::open(&root)? };
    if let Err(e) = &result {
        println!("checks error: {}", redact(&format!("{e:#}")));
    }
    let final_name = provision::node_location(&store, &nid)?.and_then(|l| l.sandbox().map(str::to_string)).unwrap_or(name);
    let mut sb = Sandboxes::load(&root)?;
    let bx = sb.blaxel()?;
    if !final_name.is_empty() && bx.get(&final_name)?.is_some() {
        let _ = sb.delete(&bx, &final_name);
        println!("deleted {final_name}");
    }
    drop(store);
    println!("remove secret: {}", host(&["environment", "remove", "node_key", "--node", "env-node"], None));
    result
}

fn show_proxy(bx: &tod_sandbox::blaxel::Blaxel, name: &str) -> anyhow::Result<()> {
    let v = bx.get_json(name)?.ok_or_else(|| anyhow::anyhow!("gone"))?;
    let proxy = &v["spec"]["network"]["proxy"];
    println!("proxy enabled={}", proxy["enabled"]);
    for r in proxy["routing"].as_array().cloned().unwrap_or_default() {
        let hdrs: Vec<String> = r["headers"].as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
        println!(
            "  rule destinations={} header-names={:?} has-secret-value={}",
            r["destinations"],
            hdrs,
            r["secrets"].as_object().is_some_and(|o| o.values().any(|x| x.as_str().is_some_and(|s| !s.is_empty())))
        );
    }
    Ok(())
}

const TOKEN_PATTERNS: &str = "-e '[s]k-ant-oat01-[A-Za-z0-9_-]\\{30,\\}' -e '[g]hp_[A-Za-z0-9]\\{20,\\}' -e '[g]ithub_pat_[A-Za-z0-9_]\\{20,\\}' -e '[g]ho_[A-Za-z0-9]\\{20,\\}' -e '[l]in_api_[A-Za-z0-9]\\{20,\\}'";

fn run_checks(root: &std::path::Path, slot: &mut Option<FleetStore>, nid: &str, name: &str, host: &dyn Fn(&[&str], Option<&str>) -> String) -> anyhow::Result<()> {
    let sb = Sandboxes::load(root)?;
    let bx = sb.blaxel()?;
    let store: &FleetStore = slot.as_ref().unwrap();
    let info = bx.get(name)?.unwrap();
    println!("== ITEM 1 ==\nlabels: tod-creds present={}", info.label("tod-creds").is_some());
    show_proxy(&bx, name)?;
    println!("header via proxy (Environment rule): {}", exec(name, "curl -s -m 40 https://httpbin.org/headers | grep -i dummy"));
    // Real tokens are detected by their well-known prefixes, never by value (that would put the value in the sandbox).
    let scan = format!(
        "(env; cat /proc/*/environ 2>/dev/null | tr '\\0' '\\n'; cat /proc/*/cmdline 2>/dev/null | tr '\\0' '\\n'; grep -rIhs {p} /root /etc /opt /tmp /home /var/tmp 2>/dev/null) | grep -c {p} || true",
        p = TOKEN_PATTERNS
    );
    println!("real-token-looking strings in sandbox env/proc/files (0 expected): {}", exec(name, &scan));
    println!("sandbox-wide env names of interest: {}", exec(name, "env | grep -i -e token -e anthropic -e GH_ -e proxy | sed 's/=.*//'"));
    println!("agent-launch env in sandbox: {}", via_agent(root, name, "env | grep -e CLAUDE_CODE_OAUTH_TOKEN -e GH_TOKEN -e TOD_GITHUB_AUTH -e NODE_USE_ENV_PROXY"));
    println!("== ITEM 2 ==");
    let (r1, sid) = claude_turn(root, name, "conversation-e2e", None, "reply with the single word ok");
    println!("turn 1: {r1}\nsession id: {sid:?}");
    println!("session logs in sandbox: {}", exec(name, "find / -name '*.jsonl' -path '*projects*' 2>/dev/null | head; echo CLAUDE_CONFIG_DIR=${CLAUDE_CONFIG_DIR:-unset}"));
    let pulled = session_log::pull_node(root, nid, name)?;
    println!("pull_node bytes={pulled}; local: {:?}", walk(&session_log::local_dir(root, nid)));
    if std::env::var("E2E_SKIP_REFRESH").is_ok() {
        println!("== ITEM 4 (same sandbox) ==");
        let _ = slot.take();
        println!("{}", via_agent(root, name, "/opt/tod/bin/tod-cli environment request github_extra --why 'live e2e check' --host httpbin.org --description 'e2e' --node env-node 2>&1; echo rc=$?; /opt/tod/bin/tod-cli environment list --node env-node 2>&1; echo rc=$?"));
        println!("app store after request:
{}", host(&["environment", "list", "--node", "env-node"], None));
        return Ok(());
    }
    // Change the Environment secret and recreate.
    let node: uuid::Uuid = nid.parse()?;
    let all = store.read(|conn| tod_store::environment::resolve(conn, node))?;
    let r = all.iter().find(|r| r.entry.name == "node_key").unwrap();
    tod_store::CredentialStore::from_data_root(root).set_named(&r.account(), V2).map_err(|e| anyhow::anyhow!("{e}"))?;
    let t = std::time::Instant::now();
    let rf = tod_core::cloud_sync::lost::refresh_credentials(store, nid, false)?;
    println!("refresh_credentials: {rf:?} in {:?}", t.elapsed());
    let new = provision::node_location(store, nid)?.unwrap().sandbox().unwrap().to_string();
    println!("new sandbox: {new} (same={})", new == name);
    println!("restored logs in new sandbox: {}", exec(&new, "find / -name '*.jsonl' -path '*projects*' 2>/dev/null | head"));
    println!("header v2 via proxy: {}", exec(&new, "curl -s -m 40 https://httpbin.org/headers | grep -i dummy"));
    let (r2, sid2) = claude_turn(root, &new, "conversation-e2e", sid.clone(), "what single word did I ask you to reply with earlier? answer with just that word");
    println!("turn 2 (resume {sid:?} in new sandbox): {r2}\nsession id after: {sid2:?} (same as before: {})", sid == sid2);
    println!("== ITEM 3 (new sandbox) ==");
    println!(
        "{}",
        via_agent(
            root,
            &new,
            "echo gh=$(command -v gh || echo none); curl -s -m 30 -o /dev/null -w 'api.github.com/user, no auth header sent: http=%{http_code}\\n' https://api.github.com/user; curl -s -m 30 -o /dev/null -w 'api.github.com/user, GH_TOKEN placeholder sent: http=%{http_code}\\n' -H \"Authorization: Bearer $GH_TOKEN\" https://api.github.com/user; command -v gh >/dev/null && gh api user --jq .type; git ls-remote https://github.com/octocat/Hello-World HEAD"
        )
    );
    println!(
        "anthropic rule effect: {}",
        via_agent(root, &new, "curl -s -m 30 -o /dev/null -w 'api.anthropic.com/v1/models, placeholder sent: http=%{http_code}\\n' -H \"Authorization: Bearer $CLAUDE_CODE_OAUTH_TOKEN\" -H 'anthropic-version: 2023-06-01' -H 'anthropic-beta: oauth-2025-04-20' https://api.anthropic.com/v1/models")
    );
    println!("== ITEM 4 ==");
    let _ = slot.take();
    println!(
        "{}",
        via_agent(root, &new, "ls -la /opt/tod/bin/tod-cli; /opt/tod/bin/tod-cli environment request github_extra --why 'live e2e check' --host httpbin.org --description 'e2e' --node env-node 2>&1; echo rc=$?; /opt/tod/bin/tod-cli environment list --node env-node 2>&1; echo rc=$?; echo ---; /opt/tod/bin/tod-cli secrets run --env X=github_token -- env 2>&1 | head -5")
    );
    println!("app store after request:\n{}", host(&["environment", "list", "--node", "env-node"], None));
    Ok(())
}

fn walk(p: &std::path::Path) -> Vec<String> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(format!("{} ({} bytes)", path.strip_prefix(p).unwrap_or(&path).display(), e.metadata().map(|m| m.len()).unwrap_or(0)));
            }
        }
    }
    out
}
