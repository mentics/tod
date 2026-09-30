//! Live check (real, billed Blaxel sandbox) of the Environment proxy rules:
//! `cargo run -p tod-store --example e2e_env_proxy -- <data root> <sandbox name>`.
//! Needs the Blaxel workspace in `<data root>/sandboxes.toml`, its API key in
//! the credential store, and `TOD_RELAY_BIN`. Uses a dummy secret, prints no
//! credential, and deletes the sandbox at the end.
use tod_sandbox::node::CustomCredential;
use tod_store::fleet::sandbox::{NewSandboxSource, Sandboxes, interactive_proxy_is_current, set_data_root};

/// Only the Environment's credentials (no GitHub/Linear/Claude) in the proxy.
fn nc(custom: &[CustomCredential]) -> tod_sandbox::node::NodeCredentials {
    tod_sandbox::node::NodeCredentials { custom: custom.to_vec(), ..Default::default() }
}

fn proxy_is_current(info: &tod_sandbox::blaxel::SandboxInfo, custom: &[CustomCredential]) -> bool {
    interactive_proxy_is_current(info, &nc(custom), Default::default())
}

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let root = std::path::PathBuf::from(a.next().expect("data root"));
    let name = a.next().expect("sandbox name");
    let keep = std::env::var("E2E_KEEP").is_ok();
    set_data_root(&root);
    let mut sb = Sandboxes::load(&root)?;
    let dummy = "DUMMY-proxy-secret-e2e-424242";
    let custom = vec![CustomCredential {
        name: "echo_key".into(),
        hosts: vec!["httpbin.org".into()],
        header: "X-Dummy-Key".into(),
        template: "Bearer {value}".into(),
        secret_value: dummy.into(),
    }];
    let mut p = |s: &str| eprintln!("progress: {s}");
    let url = sb.create_with_proxy(&name, &NewSandboxSource::Image(String::new()), false, false, &nc(&custom), &mut p)?;
    let bx = sb.blaxel()?;
    let info = bx.get(&name)?.expect("exists");
    println!("tod-creds label present: {:?}; proxy_is_current: {}", info.label("tod-creds").is_some(), proxy_is_current(&info, &custom));
    let run = |cmd: &str| -> anyhow::Result<()> {
        let r = bx.run(&url, cmd, 90)?;
        let out = r.output().replace(dummy, "<<LEAKED-VALUE>>");
        println!("$ {cmd}\nexit={}\n{}\n", r.exit_code, out.chars().take(1500).collect::<String>());
        Ok(())
    };
    run("which curl wget python3 | head -3")?;
    run("curl -s -m 30 https://httpbin.org/headers")?;
    run("curl -s -m 30 -o /dev/null -w 'github=%{http_code}\n' https://github.com; curl -s -m 30 -o /dev/null -w 'pypi=%{http_code}\n' https://pypi.org/simple/")?;
    run(&format!("env | grep -c '{dummy}'; grep -rsl '{dummy}' /root /home /etc /tmp /opt /var/tmp 2>/dev/null | head -3; echo scan-done"))?;
    run("env | grep -i proxy | sed 's/=.*//'")?;
    if keep {
        println!("kept {name}");
    } else {
        sb.delete(&bx, &name)?;
        println!("deleted {name}; get -> {:?}", bx.get(&name)?.map(|i| i.status));
    }
    Ok(())
}
