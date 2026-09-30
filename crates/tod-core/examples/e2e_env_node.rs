//! Live check (real, billed Blaxel sandboxes): a Files "Cloud sandbox" node
//! made through tod gets the node's Environment proxy rules; a credential
//! change recreates it through `lost::refresh_credentials` with the branch
//! pushed first and the agent session logs copied out and back.
//! `e2e_env_node <data root> <tod-cli> <image>`; the image holds a repo at
//! /root/app whose origin is a local bare repo (see tod-store's `e2e_env_bake`).
//! Dummy values only; deletes what it made.
use std::io::Write;
use std::process::{Command, Stdio};
use tod_store::fleet::repos::task::FleetTask;
use tod_store::fleet::sandbox::{SandboxExec, Sandboxes, proxy_is_current};
use tod_store::fleet::{FleetMutation, FleetStore, provision};
use tod_store::outline::{OutlineMutation, types::Capability};

const V1: &str = "DUMMY-node-secret-one-515151";
const V2: &str = "DUMMY-node-secret-two-626262";
const SID: &str = "aaaaaaaa-1111-4222-8333-444444444444";

type Host<'a> = &'a dyn Fn(&[&str], Option<&str>) -> (Option<i32>, String);

fn redact(s: &str) -> String {
    s.replace(V1, "<<V1>>").replace(V2, "<<V2>>")
}

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let root = std::path::PathBuf::from(a.next().unwrap());
    let cli = std::path::PathBuf::from(a.next().unwrap());
    let image = a.next().unwrap();
    tod_store::fleet::sandbox::set_data_root(&root);
    tod_store::paths::set_data_root(root.clone());
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
    let host = |args: &[&str], stdin: Option<&str>| -> (Option<i32>, String) {
        let mut c = Command::new(&cli);
        c.arg("--data-root").arg(&root).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut ch = c.spawn().unwrap();
        if let Some(s) = stdin {
            ch.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
        } else {
            drop(ch.stdin.take());
        }
        let o = ch.wait_with_output().unwrap();
        (o.status.code(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    };
    println!("files: {:?}", host(&["capabilities", "set", "env-node", "files", "--dir", "/root/app", "--sandbox", &format!("image:{image}")], None));
    println!("add-secret: {:?}", host(&["environment", "add-secret", "node_key", "--host", "httpbin.org", "--auth", "header:X-Dummy-Key", "--node", "env-node"], None).1);
    host(&["environment", "set-secret", "node_key", "--node", "env-node"], Some(&format!("{V1}\n")));
    let store = FleetStore::open(&root)?;
    let nid = node_id.to_string();
    let mut progress = |m: &str| eprintln!("progress: {m}");
    let t = std::time::Instant::now();
    let made = provision::resolve_launch_cwd_with(&store, &nid, &mut progress);
    println!("make location: {:?} in {:?}", made.as_ref().map(|(d, w)| (d.to_string(), w.clone())).map_err(|e| format!("{e:#}")), t.elapsed());
    let name = match provision::node_location(&store, &nid)? {
        Some(loc) => loc.sandbox().unwrap_or_default().to_string(),
        None => String::new(),
    };
    println!("sandbox name: {name}");
    let result = if name.is_empty() { Ok(()) } else { run_checks(&root, &store, &nid, &name, &host) };
    if let Err(e) = &result {
        println!("checks error: {e:#}");
    }
    // Cleanup whatever happened.
    let final_name = provision::node_location(&store, &nid)?.and_then(|l| l.sandbox().map(str::to_string)).unwrap_or(name);
    let mut sb = Sandboxes::load(&root)?;
    let bx = sb.blaxel()?;
    if !final_name.is_empty() && bx.get(&final_name)?.is_some() {
        let _ = sb.delete(&bx, &final_name);
        println!("deleted {final_name}");
    }
    drop(store);
    println!("remove secret: {:?}", host(&["environment", "remove", "node_key", "--node", "env-node"], None).1);
    result
}

fn set_secret(store: &FleetStore, nid: &str, value: &str) -> anyhow::Result<()> {
    let node: uuid::Uuid = nid.parse()?;
    let all = store.read(|conn| tod_store::environment::resolve(conn, node))?;
    let r = all.iter().find(|r| r.entry.name == "node_key").ok_or_else(|| anyhow::anyhow!("no node_key"))?;
    tod_store::CredentialStore::from_data_root(store.paths().root())
        .set_named(&r.account(), value)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

fn exec(name: &str, script: &str) -> String {
    let e = SandboxExec::new(name);
    match e.output("/root/app", "sh", &["-c", script]) {
        Ok(o) => redact(&format!("exit={} {}{}", o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))),
        Err(e) => format!("exec error: {e:#}"),
    }
}

fn run_checks(root: &std::path::Path, store: &FleetStore, nid: &str, name: &str, host: Host) -> anyhow::Result<()> {
    let sb = Sandboxes::load(root)?;
    let bx = sb.blaxel()?;
    let custom = |v: &str| {
        vec![tod_sandbox::node::CustomCredential {
            name: "node_key".into(),
            hosts: vec!["httpbin.org".into()],
            header: "X-Dummy-Key".into(),
            template: "{value}".into(),
            secret_value: v.into(),
        }]
    };
    let info = bx.get(name)?.unwrap();
    println!("tod-env label present: {:?}; status {}", info.label("tod-env").is_some(), info.status);
    println!("proxy_is_current(v1): {}", proxy_is_current(&info, &custom(V1)));
    println!("checkout: {}", exec(name, "git branch --show-current; git status --short | wc -l; git remote -v | head -1"));
    println!("header via proxy: {}", exec(name, "curl -s -m 40 https://httpbin.org/headers | grep -i dummy"));
    println!("value anywhere (count of matching lines in env/proc/files): {}", exec(name, "(env; cat /proc/*/environ 2>/dev/null | tr '\\0' '\\n'; cat /proc/*/cmdline 2>/dev/null | tr '\\0' '\\n'; grep -rIh [D]UMMY-node-secret /root /etc /opt /tmp /home 2>/dev/null) | grep -c '[D]UMMY-node-secret' || true"));
    println!("agent path env: {}", exec(name, "echo HOME=$HOME CLAUDE_CONFIG_DIR=${CLAUDE_CONFIG_DIR:-unset}"));
    let seed = format!("d=\"${{CLAUDE_CONFIG_DIR:-$HOME/.claude}}/projects/-root-app\"; mkdir -p \"$d\"; printf '{{\"type\":\"user\",\"n\":1}}\\n{{\"type\":\"assistant\",\"n\":2}}\\n' > \"$d/{SID}.jsonl\"; ls -la \"$d\"");
    println!("seed log: {}", exec(name, &seed));
    println!("refresh (unchanged): {:?}", tod_core::cloud_sync::lost::refresh_credentials(store, nid, false)?);
    println!("dirty: {}", exec(name, "echo x > dirty.txt; git status --short"));
    set_secret(store, nid, V2)?;
    let r = tod_core::cloud_sync::lost::refresh_credentials(store, nid, false)?;
    println!("refresh (dirty, changed credential): {r:?}");
    println!("still same sandbox: {}", bx.get(name)?.is_some());
    println!("clean up dirt: {}", exec(name, "rm dirty.txt; git status --short | wc -l"));
    let t = std::time::Instant::now();
    let r = tod_core::cloud_sync::lost::refresh_credentials(store, nid, false)?;
    println!("refresh (clean, changed credential): {r:?} in {:?}", t.elapsed());
    let loc = provision::node_location(store, nid)?.unwrap();
    let new = loc.sandbox().unwrap().to_string();
    println!("new sandbox name: {new} (same={})", new == name);
    let info = bx.get(&new)?.unwrap();
    println!("new proxy_is_current(v2): {} ; with v1: {}", proxy_is_current(&info, &custom(V2)), proxy_is_current(&info, &custom(V1)));
    println!("branch after recreate: {}", exec(&new, "git branch --show-current; git log --oneline -1; git ls-remote origin | head -3"));
    println!("restored log: {}", exec(&new, &format!("cat ${{CLAUDE_CONFIG_DIR:-$HOME/.claude}}/projects/-root-app/{SID}.jsonl")));
    println!("header v2 via proxy: {}", exec(&new, "curl -s -m 40 https://httpbin.org/headers | grep -i dummy"));
    println!(
        "saved logs in data root: {:?}",
        std::fs::read_dir(tod_store::fleet::session_log::local_dir(root, nid)).map(|d| d.map(|e| e.unwrap().file_name()).collect::<Vec<_>>())
    );
    Ok(())
}
