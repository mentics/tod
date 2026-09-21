//! Seed a tree whose committed Spec descendants have pending incoming changes
//! (`doc/conversation/incoming-changes.md`), for looking at §6 in the app.
//!
//! ```bash
//! cargo run -p tod-store --example seed_incoming_sandbox -- <data-root>
//! ```

use std::path::PathBuf;
use tod_store::fleet::FleetStore;
use tod_store::outline::{CreatePosition, OutlineMutation as M, types::Capability};
use uuid::Uuid;

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(std::env::args().nth(1).expect("data root argument"));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    let store = FleetStore::open(&root)?;
    store.enqueue_outline(M::CreateList {
        slug: "incoming".into(),
        title: "Incoming".into(),
    })?;
    store.writer().flush()?;
    let list_id = store.list_outline_lists()?[0].id;

    let make = |title: &str, parent: Option<Uuid>| -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        store.enqueue_outline(M::CreateNode {
            node_id: Some(id),
            list_id,
            parent_id: parent,
            anchor_id: parent,
            position: if parent.is_some() {
                CreatePosition::Child
            } else {
                CreatePosition::Below
            },
            title: title.into(),
        })?;
        store.enqueue_outline(M::EnableCapabilities {
            node_id: id,
            capabilities: vec![Capability::Spec, Capability::Lifecycle],
        })?;
        store.writer().flush()?;
        Ok(id)
    };
    let project = make("Checkout redesign", None)?;
    let feature = make("Payment form", Some(project))?;
    let sub = make("Card validation", Some(feature))?;
    let _draft = make("Receipt email", Some(project))?;
    let other = make("Unrelated project", None)?;
    // A component used by reference: "Saved cards" is not under "Card
    // field", only names it with `[[slug]]` (§3).
    let component = make("Card field", Some(other))?;
    let saved = make("Saved cards", None)?;
    // What `--agent mock` concludes when these nodes are checked against
    // their incoming changes (`affects none|plan|obligations: <note>`).
    for (node, details) in [
        (
            feature,
            "affects obligations: the form needs a requirement saying which card digits it may show",
        ),
        (
            sub,
            "affects plan: the error-logging step has to mask the card number first",
        ),
    ] {
        store.enqueue_outline(M::SetExtraContent {
            node_id: node,
            content_type: tod_store::outline::types::EXTRA_CONTENT_DETAILS.into(),
            body: details.into(),
        })?;
    }
    store.writer().flush()?;
    store.reload_if_stale().ok();

    let constraint = |body: &str| -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        store.enqueue_outline(M::CreateObligation {
            obligation_id: Some(id),
            node_id: project,
            kind: tod_store::outline::KIND_CONSTRAINT.into(),
            after_id: None,
            before: false,
            section: None,
            body: body.into(),
            phase: "requirements".into(),
        })?;
        store.writer().flush()?;
        Ok(id)
    };
    // Present before the descendants committed: rewording it later shows a
    // before and an after.
    let pci = constraint("Never store the full card number")?;
    let conn = rusqlite::Connection::open(store.writer().db_path())?;
    for (node, state) in [(feature, "ready"), (sub, "active")] {
        conn.execute(
            "INSERT OR REPLACE INTO node_lifecycle (node_id, state, updated_at) VALUES (?1, ?2, 0)",
            rusqlite::params![node.as_bytes().to_vec(), state],
        )?;
    }

    let obligation = |node: Uuid, kind: &str, body: &str| -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        store.enqueue_outline(M::CreateObligation {
            obligation_id: Some(id),
            node_id: node,
            kind: kind.into(),
            after_id: None,
            before: false,
            section: None,
            body: body.into(),
            phase: "requirements".into(),
        })?;
        store.writer().flush()?;
        Ok(id)
    };
    let slug = store
        .get_node(&component.to_string())?
        .expect("component")
        .slug;
    obligation(
        component,
        tod_store::outline::KIND_REQUIREMENT,
        "Shows the card brand and the last four digits",
    )?;
    obligation(
        saved,
        tod_store::outline::KIND_REQUIREMENT,
        &format!("Lists each saved card as a [[{slug}]]"),
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO node_lifecycle (node_id, state, updated_at) VALUES (?1, 'approved', 0)",
        rusqlite::params![saved.as_bytes().to_vec()],
    )?;
    println!("component slug: {slug} (id {component})");

    constraint("Every page loads in under 1 second on 3G")?;
    store.enqueue_outline(M::UpdateObligationBody {
        obligation_id: pci,
        body: "Never store or log the full card number".into(),
    })?;
    store.writer().flush()?;
    println!("seeded {}", root.display());
    Ok(())
}
