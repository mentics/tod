//! Deletes a node's never-used Files sandbox and forgets its location.
//! `files_reset <data root> <node uuid>`.
use tod_store::fleet::{FleetStore, provision, sandbox::Sandboxes};
use tod_store::fleet::FleetMutation;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = std::path::PathBuf::from(args.next().expect("data root"));
    let node = args.next().expect("node");
    tod_store::fleet::sandbox::set_data_root(&root);
    tod_store::paths::set_data_root(root.clone());
    println!("daemon quit: {:?}", tod_agentd_client::client::quit(&root));
    let fleet = FleetStore::open(&root)?;
    let location = provision::node_location(&fleet, &node)?.ok_or_else(|| anyhow::anyhow!("no location"))?;
    let name = location.sandbox().ok_or_else(|| anyhow::anyhow!("not a sandbox"))?.to_string();
    let mut sandboxes = Sandboxes::load(&root)?;
    let bx = sandboxes.blaxel()?;
    sandboxes.delete(&bx, &name)?;
    fleet.enqueue(FleetMutation::DeleteFilesLocation { node_id: node })?;
    fleet.writer().flush()?;
    println!("deleted {name} and forgot the location");
    Ok(())
}
