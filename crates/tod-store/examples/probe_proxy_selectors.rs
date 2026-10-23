//! Live probe (a real, billed Blaxel sandbox, deleted at the end):
//! `cargo run -p tod-store --example probe_proxy_selectors -- <data root> <sandbox name>`.
//! Which request spellings of one host does a proxy routing rule tell apart?
//! Each rule adds `X-Rule: <its spelling>`; the secrets are dummies.
use std::time::Duration;
use tod_sandbox::node::{ProxyRule, proxy_spec};
use tod_store::fleet::sandbox::{Sandboxes, set_data_root};

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let root = std::path::PathBuf::from(a.next().expect("data root"));
    let name = a.next().expect("sandbox name");
    set_data_root(&root);
    let sb = Sandboxes::load(&root)?;
    let bx = sb.blaxel()?;
    let account = sb.account()?.clone();
    let mk = |dest: &str, header: &str, i: usize| ProxyRule {
        destination: dest.to_string(),
        header: header.into(),
        value: format!("rule{i}-{{{{SECRET:s{i}}}}}"),
        secret_name: format!("s{i}"),
        secret_value: format!("dummy{i}"),
    };
    let rules = vec![mk("httpbin.org", "X-A", 0), mk("httpbin.org", "X-B", 1), mk("example.org", "X-C", 2)];
    let proxy = proxy_spec(&rules);
    bx.create(&tod_sandbox::blaxel::NewSandbox {
        name: &name,
        image: &account.default_image,
        region: &account.region,
        memory_mb: 1024,
        labels: &[("tod-probe", "proxy")],
        proxy: Some(&proxy),
    })?;
    let result = (|| -> anyhow::Result<()> {
        let info = bx.wait_deployed(&name, Duration::from_secs(180))?;
        let url = info.url.expect("url");
        let r = bx.run(&url, "which curl wget python3 node apk apt-get; cat /etc/os-release | head -2; (apk add --no-cache curl >/dev/null 2>&1 || (apt-get update >/dev/null 2>&1 && apt-get install -y curl >/dev/null 2>&1)); which curl", 180)?;
        println!("tools: {}", r.output().trim());
        let run = |label: &str, cmd: &str| -> anyhow::Result<()> {
            let r = bx.run(&url, cmd, 60)?;
            println!("{label}: {}", r.output().trim());
            Ok(())
        };
        run("same host, two rules, two headers", "curl -s -m 30 https://httpbin.org/headers | grep -i -E 'x-a|x-b' | tr -d '\\n '; echo")?;
        run("client-sent placeholder for the rule's own secret", "curl -s -m 30 -H 'X-Client: {{SECRET:s0}}' https://httpbin.org/headers | grep -i 'x-client' | tr -d '\\n '; echo")?;
        run("client-sent placeholder for the other rule's secret", "curl -s -m 30 -H 'X-Client: {{SECRET:s1}}' https://httpbin.org/headers | grep -i 'x-client' | tr -d '\\n '; echo")?;
        run("client-sent placeholder for another host's secret", "curl -s -m 30 -H 'X-Client: {{SECRET:s2}}' https://httpbin.org/headers | grep -i 'x-client' | tr -d '\\n '; echo")?;
        Ok(())
    })();
    bx.delete(&name)?;
    println!("deleted {name}");
    result
}
