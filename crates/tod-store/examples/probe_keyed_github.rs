//! Live probe (a real, billed Blaxel sandbox, deleted at the end):
//! `cargo run -p tod-store --example probe_keyed_github -- <data root> <sandbox name> <owner/repo>`.
//! A proxy rule that only holds secrets (the user's stored GitHub token as
//! `gh_good`, a bad one as `gh_bad`): is the repository readable with the
//! good placeholder, refused with the bad one, and unreadable with neither?
//! Prints no token.
use std::time::Duration;
use tod_sandbox::node::{GITHUB_API_HOST, KeyedSecret, keyed_secrets_spec};
use tod_store::credentials::{CredentialKind, CredentialStore};
use tod_store::fleet::sandbox::{Sandboxes, set_data_root};

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let root = std::path::PathBuf::from(a.next().expect("data root"));
    let name = a.next().expect("sandbox name");
    let repo = a.next().expect("owner/repo");
    set_data_root(&root);
    let token = CredentialStore::from_data_root(&root).get(CredentialKind::GithubToken).expect("a stored GitHub token");
    let sb = Sandboxes::load(&root)?;
    let bx = sb.blaxel()?;
    let account = sb.account()?.clone();
    let proxy = keyed_secrets_spec(
        GITHUB_API_HOST,
        &[KeyedSecret { name: "gh_good".into(), value: token.clone() }, KeyedSecret { name: "gh_bad".into(), value: "ghp_invalid".into() }],
    );
    bx.create(&tod_sandbox::blaxel::NewSandbox {
        name: &name,
        image: &account.default_image,
        region: &account.region,
        memory_mb: 1024,
        labels: &[("tod-probe", "keyed-github")],
        proxy: Some(&proxy),
    })?;
    let result = (|| -> anyhow::Result<()> {
        let info = bx.wait_deployed(&name, Duration::from_secs(180))?;
        let url = info.url.expect("url");
        bx.run(&url, "apk add --no-cache curl >/dev/null 2>&1; which curl", 180)?;
        for (label, header) in [
            ("good placeholder", "-H 'Authorization: Bearer {{SECRET:gh_good}}'"),
            ("bad placeholder", "-H 'Authorization: Bearer {{SECRET:gh_bad}}'"),
            ("no header", ""),
        ] {
            let cmd = format!("curl -s -m 30 -o /tmp/out -w '%{{http_code}}' {header} https://api.github.com/repos/{repo}; echo; head -c 120 /tmp/out | tr -d '\\n'");
            let r = bx.run(&url, &cmd, 60)?;
            println!("{label}: {}", r.output().replace(&token, "<TOKEN>").replace('\n', " | "));
        }
        Ok(())
    })();
    bx.delete(&name)?;
    println!("deleted {name}");
    result
}
