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
    // The resident daemon holds the fleet lock; stop it (as the docs say a test must).
    println!("daemon quit: {:?}", tod_agentd_client::client::quit(&root));
    tod_store::paths::set_data_root(root.clone());
    let fleet = FleetStore::open(&root)?;
    let id = node;
    let t0 = std::time::Instant::now();
    let mut say = |m: &str| println!("  [{:6.2}s] {m}", t0.elapsed().as_secs_f64());
    println!("refresh proxy: {:?}", provision::refresh_sandbox_proxy(&fleet, &id, false, &mut say));
    match provision::resolve_launch_cwd_with(&fleet, &id, &mut say) {
        Ok((dir, warnings)) => println!("OK: {dir} warnings={warnings:?}"),
        Err(err) => println!("ERROR: {err:#}"),
    }
    Ok(())
}
