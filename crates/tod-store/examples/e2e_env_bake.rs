//! Bakes a tiny sandbox image holding a git repository at /root/app whose
//! `origin` is a bare repo at /root/origin.git in the same image (so nothing
//! can be pushed anywhere real), for the live Environment checks that need a
//! Files "Cloud sandbox" node. `e2e_env_bake <data root> <image name>`.
use tod_store::fleet::sandbox::{Sandboxes, build_dir, payload, relay_binary, set_data_root};

fn main() -> anyhow::Result<()> {
    let mut a = std::env::args().skip(1);
    let root = std::path::PathBuf::from(a.next().expect("data root"));
    let name = a.next().expect("image name");
    set_data_root(&root);
    let sb = Sandboxes::load(&root)?;
    let acct = sb.account()?.clone();
    let relay = relay_binary()?;
    let pl = payload(&relay, true);
    let dir = build_dir(&name)?;
    let repo = "RUN git config --global user.email e2e@example.invalid && git config --global user.name e2e \\n && git init -q --bare /root/origin.git && git init -q -b main /root/app && cd /root/app \\n && echo hi > README && git add . && git commit -qm init && git remote add origin /root/origin.git && git push -q origin main\n";
    let df = tod_sandbox::provision::bake_dockerfile("ubuntu:24.04", &pl).replace("ENTRYPOINT", &format!("{repo}ENTRYPOINT"));
    std::fs::write(dir.join("Dockerfile"), df)?;
    std::fs::write(dir.join("bootstrap.sh"), tod_store::fleet::sandbox::BOOTSTRAP)?;
    std::fs::write(dir.join("tod-relay"), &relay)?;
    std::fs::write(dir.join("tod-cli"), pl.tod_cli)?;
    std::fs::write(dir.join("blaxel.toml"), tod_sandbox::provision::blaxel_toml(&name, acct.memory_mb))?;
    sb.bl_push(&dir, true, &mut |m| eprintln!("{m}"))?;
    println!("built sandbox/{name}:latest");
    Ok(())
}
