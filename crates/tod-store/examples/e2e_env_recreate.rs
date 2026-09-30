//! Live check (real Blaxel sandbox, made by `e2e_env_proxy` with E2E_KEEP):
//! session-log copy-out and restore across a credential-change recreation.
//! `e2e_env_recreate <data root> <sandbox name>`. Dummy values only; deletes
//! the sandbox at the end.
use tod_sandbox::node::CustomCredential;
use tod_store::fleet::sandbox::{NewSandboxSource, Sandboxes, proxy_is_current, set_data_root};
use tod_store::fleet::session_log;

fn cred(v: &str) -> Vec<CustomCredential> {
    vec![CustomCredential {
        name: "echo_key".into(),
        hosts: vec!["httpbin.org".into()],
        header: "X-Dummy-Key".into(),
        template: "Bearer {value}".into(),
        secret_value: v.into(),
    }]
}

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let root = std::path::PathBuf::from(a.next().expect("data root"));
    let name = a.next().expect("sandbox name");
    set_data_root(&root);
    let mut sb = Sandboxes::load(&root)?;
    let bx = sb.blaxel()?;
    sb.create_with_proxy(&name, &NewSandboxSource::Image(String::new()), false, false, &cred("DUMMY-proxy-secret-e2e-424242"), &mut |s| eprintln!("progress: {s}"))?;
    let url = sb.url(&bx, &name)?;
    let sid = "11111111-2222-4333-8444-555555555555";
    // Where Claude writes its log: ${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects/<cwd, non-alphanumerics as dashes>/<id>.jsonl
    let exec = tod_store::fleet::sandbox::SandboxExec::new(&name);
    let script = format!("d=\"${{CLAUDE_CONFIG_DIR:-$HOME/.claude}}/projects/-workspace-repo\"; mkdir -p \"$d\"; printf '{{\"type\":\"user\",\"n\":1}}\n{{\"type\":\"assistant\",\"n\":2}}\n' > \"$d/{sid}.jsonl\"; echo HOME=$HOME CLAUDE_CONFIG_DIR=${{CLAUDE_CONFIG_DIR:-unset}}; ls -la \"$d\"");
    let out = exec.output("/", "sh", &["-c", &script])?;
    let r = (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned());
    println!("seed exit={}\n{}", r.0, r.1);
    let node = "e2e-node";
    {
        use tod_store::fleet::session_log::Remote;
        let r = session_log::SandboxRemote::new(&name);
        println!("list via relay: {:?}", r.list());
        println!("read_from 0: {:?}", r.read_from("-workspace-repo", sid, 0).map(|b| String::from_utf8_lossy(&b).into_owned()));
    }
    let pulled = session_log::pull_node(&root, node, &name)?;
    let local = session_log::local_dir(&root, node);
    println!("pulled {pulled} bytes; local files: {:?}", std::fs::read_dir(&local).map(|d| d.map(|e| e.unwrap().file_name()).collect::<Vec<_>>()));
    let info = bx.get(&name)?.unwrap();
    println!("with the old value current={} ; with a changed value current={}", proxy_is_current(&info, &cred("DUMMY-proxy-secret-e2e-424242")), proxy_is_current(&info, &cred("DUMMY-changed-e2e-999")));
    sb.delete(&bx, &name)?;
    for _ in 0..60 { if bx.get(&name)?.is_none_or(|i| i.status.eq_ignore_ascii_case("TERMINATED")) { break; } std::thread::sleep(std::time::Duration::from_secs(3)); }
    println!("deleted; recreating with the changed credential");
    let changed = cred("DUMMY-changed-e2e-999");
    let url = sb.create_with_proxy(&name, &NewSandboxSource::Image(String::new()), false, false, &changed, &mut |s| eprintln!("progress: {s}"))?;
    let info = bx.get(&name)?.unwrap();
    println!("recreated; proxy_is_current(changed)={}", proxy_is_current(&info, &changed));
    let before = bx.run(&url, "ls ${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects 2>&1 | head -3; echo end", 60)?;
    println!("new sandbox projects dir before restore: {}", before.output().trim());
    let restored = session_log::restore_node(&root, node, &name)?;
    println!("restored {restored} log(s)");
    let after = bx.run(&url, &format!("cat ${{CLAUDE_CONFIG_DIR:-$HOME/.claude}}/projects/-workspace-repo/{sid}.jsonl; wc -c < ${{CLAUDE_CONFIG_DIR:-$HOME/.claude}}/projects/-workspace-repo/{sid}.jsonl"), 60)?;
    println!("restored content:\n{}", after.output());
    let h = bx.run(&url, "curl -s -m 30 https://httpbin.org/headers | grep -i dummy | sed 's/DUMMY-changed-e2e-999/<<CHANGED-VALUE>>/; s/DUMMY-proxy-secret-e2e-424242/<<OLD-VALUE>>/'", 60)?;
    println!("echo after recreate: {}", h.output().trim());
    if std::env::var("E2E_KEEP").is_err() {
        sb.delete(&bx, &name)?;
        println!("deleted {name}: {:?}", bx.get(&name)?.map(|i| i.status));
    }
    Ok(())
}
