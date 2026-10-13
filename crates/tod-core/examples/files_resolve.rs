//! Live check: what the app does when a node's files are needed. First
//! refreshes the node's sandbox proxy (new credentials), then makes or
//! resolves its Files location. `files_resolve <data root> <node uuid>`.
use tod_store::fleet::{FleetStore, provision};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = std::path::PathBuf::from(args.next().expect("data root"));
    let node = args.next().expect("node");
    tod_store::fleet::sandbox::set_data_root(&root);
    tod_store::paths::set_data_root(root.clone());
    // The resident daemon holds the fleet lock; stop it (as the docs say a test must).
    println!("daemon quit: {:?}", tod_agentd_client::client::quit(&root));
    let fleet = FleetStore::open(&root)?;
    let id = node;
    let mut say = |m: &str| println!("  {m}");
    println!("refresh proxy: {:?}", provision::refresh_sandbox_proxy(&fleet, &id, false, &mut say));
    match provision::resolve_launch_cwd_with(&fleet, &id, &mut say) {
        Ok((dir, warnings)) => println!("OK: {dir} warnings={warnings:?}"),
        Err(err) => println!("ERROR: {err:#}"),
    }
    Ok(())
}
