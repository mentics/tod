//! Live check: what the app does when a node's files are needed. First
//! refreshes the node's sandbox proxy (new credentials), then makes or
//! resolves its Files location. `files_resolve <data root> <node uuid>`.
use tod_store::fleet::{FleetStore, provision};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = std::path::PathBuf::from(args.next().expect("data root"));
    let node = args.next().expect("node");
    tod_core::cloud_sync::register_sandbox_host();
    tod_store::fleet::sandbox::set_data_root(&root);
    tod_store::paths::set_data_root(root.clone());
    let fleet = open_fleet(&root)?;
    let id = node;
    let t0 = std::time::Instant::now();
    let mut say = |m: &str| println!("  [{:6.2}s] {m}", t0.elapsed().as_secs_f64());
    println!("refresh proxy: {:?}", provision::refresh_sandbox_proxy(&fleet, &id, false, &mut say));
    let result = provision::resolve_launch_cwd_with(&fleet, &id, &mut say);
    println!("  [{:6.2}s] done", t0.elapsed().as_secs_f64());
    match result {
        Ok((dir, warnings)) => println!("OK: {dir} warnings={warnings:?}"),
        Err(err) => println!("ERROR: {err:#}"),
    }
    Ok(())
}

/// The store, through the daemon when one serves `root`, so the app's
/// daemon is never stopped for this (`doc/agentd.md`, "`quit`"); opened
/// directly only when none runs.
fn open_fleet(root: &std::path::Path) -> anyhow::Result<FleetStore> {
    if tod_agentd_client::client::connect(&tod_agentd_client::Paths::new(root)).is_some() {
        return tod_agentd_client::remote::open_store(root, None);
    }
    Ok(FleetStore::open(root)?)
}
