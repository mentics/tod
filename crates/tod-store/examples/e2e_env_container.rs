//! Live check (real Docker container): the Environment secret path through the
//! container's `tod-cli` shim. `e2e_env_container <data root> <container> <tod-cli>`.
//! Dummy values only. Prints redacted evidence; removes its secret at the end.
use std::io::Write;
use std::process::{Command, Stdio};
use tod_store::fleet::cli_relay::{SHIM_DIR, SHIM_SCRIPT, endpoint_for_container, start};
use tod_store::fleet::repos::task::FleetTask;
use tod_store::fleet::{FleetMutation, FleetStore};
use tod_store::outline::{OutlineMutation, types::Capability};

const DUMMY: &str = "DUMMY-ctr-secret-e2e-777123";

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let root = std::path::PathBuf::from(a.next().unwrap());
    let ctr = a.next().unwrap();
    let cli = std::path::PathBuf::from(a.next().unwrap());
    std::fs::create_dir_all(&root)?;
    let store = FleetStore::open(&root)?;
    let node_id = uuid::Uuid::new_v4();
    store.enqueue(FleetMutation::InsertTask {
        task: FleetTask { id: node_id.to_string(), title: "Env ctr".into(), slug: "env-ctr".into(), lifecycle: "active".into(), repo: Some("/work".into()), branch: Some("main".into()), notes: vec![], tags: vec![], ticket: None, linked_prs: vec![] },
    })?;
    store.writer().flush()?;
    store.enqueue_outline(OutlineMutation::EnableCapabilities { node_id, capabilities: vec![Capability::Files, Capability::Environment] })?;
    store.writer().flush()?;
    drop(store);
    let host = |args: &[&str], stdin: Option<&str>| -> (Option<i32>, String) {
        let mut c = Command::new(&cli);
        c.arg("--data-root").arg(&root).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut ch = c.spawn().unwrap();
        if let Some(s) = stdin { ch.stdin.take().unwrap().write_all(s.as_bytes()).unwrap(); } else { drop(ch.stdin.take()); }
        let o = ch.wait_with_output().unwrap();
        (o.status.code(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    };
    println!("files: {:?}", host(&["capabilities", "set", "env-ctr", "files", "--dir", "/work", "--container", &ctr], None));
    println!("add-secret: {:?}", host(&["environment", "add-secret", "ctr_key", "--host", "httpbin.org", "--env-var", "CTR_KEY", "--node", "env-ctr"], None));
    println!("add-secret2: {:?}", host(&["environment", "add-secret", "unset_key", "--host", "httpbin.org", "--node", "env-ctr"], None));
    println!("set-secret: {:?}", host(&["environment", "set-secret", "ctr_key", "--node", "env-ctr"], Some(&format!("{DUMMY}\n"))).0);
    let _ = start; // (shared relay is started by endpoint_for_container)
    let ep = endpoint_for_container(&root, &ctr)?;
    let shim = format!("{SHIM_DIR}/tod-cli");
    tod_agent::devcontainer::write_file(&ctr, &tod_agent::devcontainer::ContainerFile { path: shim.clone(), contents: SHIM_SCRIPT.into(), executable: true })?;
    let redact = |s: String| s.replace(DUMMY, "<<VALUE>>");
    let run = |token: Option<&str>, args: &[&str]| -> (Option<i32>, String) {
        let mut c = Command::new(tod_agent::devcontainer::docker_bin());
        c.args(["exec", "-i", "-w", "/tmp", "-e", &format!("TOD_NODE={node_id}"), "-e", "TOD_CLI_RELAY_PORT", "-e", "TOD_CLI_RELAY_TOKEN", &ctr, &shim]).args(args);
        c.env("TOD_CLI_RELAY_PORT", ep.port.to_string()).env("TOD_CLI_RELAY_TOKEN", token.unwrap_or(&ep.token));
        let o = c.stdin(Stdio::null()).output().unwrap();
        (o.status.code(), redact(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))))
    };
    println!("--- list\n{:?}", run(None, &["environment", "list"]));
    println!("--- run inside\n{:?}", run(None, &["secrets", "run", "--env", "V=ctr_key", "--", "sh", "-c", "echo hostname=$(hostname) V=$V; head -1 /etc/os-release; echo len=${#V}; exit 7"]));
    println!("--- masked print only\n{:?}", run(None, &["secrets", "run", "--env", "V=ctr_key", "--", "sh", "-c", "echo $V; echo $V | base64 >/dev/null; echo done"]));
    println!("--- unset secret\n{:?}", run(None, &["secrets", "run", "--env", "V=unset_key", "--", "true"]));
    println!("--- unknown\n{:?}", run(None, &["secrets", "run", "--env", "V=nope", "--", "true"]));
    println!("--- wrong token\n{:?}", run(Some("deadbeef"), &["secrets", "run", "--env", "V=ctr_key", "--", "hostname"]));
    // Leak hunt: value anywhere in the container now?
    let rest = &DUMMY[1..];
    let hunt = Command::new(tod_agent::devcontainer::docker_bin()).args(["exec", &ctr, "sh", "-c", &format!("grep -rl --exclude-dir=sys --exclude-dir=dev --exclude-dir=proc -e [D]{rest} / 2>/dev/null | head; for p in /proc/[0-9]*; do tr '\\0' '\\n' < $p/environ 2>/dev/null | grep -l [D]{rest} >/dev/null && echo leak-in-$p; done; echo hunt-done")]).output()?;
    println!("--- leak hunt in container\n{}", String::from_utf8_lossy(&hunt.stdout));
    let insp = Command::new(tod_agent::devcontainer::docker_bin()).args(["inspect", &ctr]).output()?;
    println!("docker inspect contains value: {}", String::from_utf8_lossy(&insp.stdout).contains(DUMMY));
    // request from inside the container
    println!("--- request\n{:?}", run(None, &["environment", "request", "req_key", "--why", "e2e check", "--host", "httpbin.org", "--node", "env-ctr"]));
    println!("host list: {}", host(&["environment", "list", "--node", "env-ctr"], None).1);
    println!("host decisions: {:?}", host(&["decisions", "list", "--node", "env-ctr"], None));
    host(&["environment", "remove", "ctr_key", "--node", "env-ctr"], None);
    host(&["environment", "remove", "unset_key", "--node", "env-ctr"], None);
    host(&["environment", "remove", "req_key", "--node", "env-ctr"], None);
    Ok(())
}
