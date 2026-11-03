//! The app's Files locations (`FilesPlace`) and what changing one touches.
//!
//! Locations are configured once and picked by nodes (see
//! [`crate::fleet::repos::files_place`]). Editing or deleting one that is in
//! use is a significant change, so it is confirmed first with its impact
//! shown: which nodes picked it, and which worktrees or sandboxes were made
//! from it that the change would leave behind. The user confirms in
//! `tod_ui`'s `files_impact` dialog, which removes those locations
//! ([`crate::fleet::provision::remove_location`], pushing each branch first)
//! and only then calls [`save`] or [`delete`].

use crate::fleet::provision::AffectedLocation;
use crate::fleet::repos::files_location::FilesLocationRepo;
use crate::fleet::repos::files_place::{FilesPlace, FilesPlaceRepo};
use crate::fleet::repos::node_files::NodeFilesRepo;
use crate::fleet::store::FleetStore;
use crate::fleet::writer::FleetMutation;
use crate::outline::OutlineMutation;
use crate::outline::types::Capability;
use anyhow::{Context, Result};

/// A node that picked a location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceUser {
    pub node_id: String,
    pub node_title: String,
}

/// What changing or deleting a location touches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaceImpact {
    /// Nodes that picked it. Nodes below them inherit it too.
    pub users: Vec<PlaceUser>,
    /// Worktrees and sandboxes made from it that the change would leave
    /// behind: all of them for a delete, those whose recipe differs for an
    /// edit.
    pub locations: Vec<AffectedLocation>,
}

impl PlaceImpact {
    pub fn is_empty(&self) -> bool {
        self.users.is_empty() && self.locations.is_empty()
    }
}

/// Every location, by name.
pub fn list(fleet: &FleetStore) -> Result<Vec<FilesPlace>> {
    fleet.reload_if_stale().ok();
    fleet.read(|conn| FilesPlaceRepo::new(conn).list())
}

/// The location `reference` names (its id, else its name).
pub fn find(fleet: &FleetStore, reference: &str) -> Result<Option<FilesPlace>> {
    fleet.reload_if_stale().ok();
    fleet.read(|conn| FilesPlaceRepo::new(conn).find(reference))
}

/// Store `place` (a new one has an empty id), refusing one that is not valid
/// with the reason. Returns it as stored. Writes: never on the UI thread.
pub fn save(fleet: &FleetStore, place: &FilesPlace) -> Result<FilesPlace> {
    let mut place = place.clone().normalized();
    let others = list(fleet)?;
    if let Some(problem) = place.problem(&others) {
        anyhow::bail!("{problem}");
    }
    if place.id.is_empty() {
        place.id = uuid::Uuid::new_v4().to_string();
    } else if !others.iter().any(|other| other.id == place.id) {
        anyhow::bail!("The location {} was deleted meanwhile", place.name);
    }
    fleet.enqueue(FleetMutation::SaveFilesPlace { place: place.clone() })?;
    fleet.writer().flush().context("Could not save the location")?;
    fleet.reload_if_stale()?;
    Ok(place)
}

/// Make `node_id`'s Files use a location with `recipe`'s settings: an existing
/// one with the same settings, else a new one (named after them when
/// `recipe.name` is empty). Returns it. Writes: never on the UI thread.
pub fn pick_matching(fleet: &FleetStore, node_id: &str, recipe: &FilesPlace) -> Result<FilesPlace> {
    let recipe = FilesPlace { id: String::new(), ..recipe.clone().normalized() };
    let existing = list(fleet)?;
    let place = match existing.iter().find(|p| p.recipe_key() == recipe.recipe_key()) {
        Some(found) => found.clone(),
        None => {
            let name = if recipe.name.is_empty() {
                crate::fleet::repos::files_place::suggested_name(&recipe, &existing)
            } else {
                recipe.name.clone()
            };
            save(fleet, &FilesPlace { name, ..recipe })?
        }
    };
    fleet.enqueue(FleetMutation::SetNodeFilesPlace {
        node_id: node_id.to_string(),
        place_id: Some(place.id.clone()),
    })?;
    fleet.writer().flush().context("Could not pick the location")?;
    fleet.reload_if_stale()?;
    Ok(place)
}

/// Delete the location; nodes that picked it have none afterwards. Remove the
/// locations made from it first ([`impact`]). Writes: never on the UI thread.
pub fn delete(fleet: &FleetStore, id: &str) -> Result<()> {
    fleet.enqueue(FleetMutation::DeleteFilesPlace { id: id.to_string() })?;
    fleet.writer().flush().context("Could not delete the location")?;
    fleet.reload_if_stale()?;
    Ok(())
}

/// What replacing location `id` with `after` touches, or deleting it
/// (`after` is `None`). Including worktrees and sandboxes already stale.
pub fn impact(fleet: &FleetStore, id: &str, after: Option<&FilesPlace>) -> Result<PlaceImpact> {
    fleet.reload_if_stale().ok();
    let keep_recipe = after.map(|place| place.clone().normalized().recipe_key());
    fleet.read(|conn| {
        let node_files = NodeFilesRepo::new(conn);
        let nodes = crate::outline::repos::NodeRepo::new(conn);
        let title = |node: &str| -> Result<String> {
            let uuid = uuid::Uuid::parse_str(node)?;
            Ok(nodes.get(uuid)?.map(|n| n.title).unwrap_or_default())
        };
        let mut users = Vec::new();
        for node in FilesPlaceRepo::new(conn).nodes_using(id)? {
            users.push(PlaceUser { node_title: title(&node)?, node_id: node });
        }
        let mut locations = Vec::new();
        for location in FilesLocationRepo::new(conn).list_all()? {
            let source_place = node_files.get(&location.source_node_id)?.and_then(|f| f.place_id);
            if source_place.as_deref() != Some(id) {
                continue;
            }
            if keep_recipe.as_deref() == Some(location.recipe.as_str()) {
                continue;
            }
            locations.push(AffectedLocation {
                node_title: title(&location.node_id)?,
                node_id: location.node_id.clone(),
                location,
            });
        }
        Ok(PlaceImpact { users, locations })
    })
}

/// The refusal of every Migrate path (the panel, the task editor, `tod-cli`):
/// moving or pinning a node's children is not supported.
pub const MIGRATE_HAS_CHILDREN: &str = "You cannot migrate a node that has children.";

fn refuse_children(fleet: &FleetStore, node_id: &str) -> Result<()> {
    let uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    let has_children: bool = fleet.read(|conn| {
        Ok(conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM outline_entries WHERE parent_id = ?1)",
            [crate::outline::uuid_blob::uuid_to_blob(uuid)],
            |row| row.get(0),
        )?)
    })?;
    if has_children {
        anyhow::bail!("{MIGRATE_HAS_CHILDREN}");
    }
    Ok(())
}

/// What migrating `node_id` to location `place_id` removes: its own worktree
/// or sandbox, unless it was made from this very recipe for this very node.
/// Refuses a node with children. Reads only: still off the UI thread, since
/// it reads the store.
pub fn migrate_impact(fleet: &FleetStore, node_id: &str, place_id: &str) -> Result<Vec<AffectedLocation>> {
    fleet.reload_if_stale().ok();
    refuse_children(fleet, node_id)?;
    let place = fleet
        .read(|conn| FilesPlaceRepo::new(conn).get(place_id))?
        .with_context(|| format!("The location {place_id} was deleted meanwhile"))?;
    let keep_recipe = place.normalized().recipe_key();
    Ok(crate::fleet::provision::locations_affected_by(fleet, node_id)?
        .into_iter()
        // One made from the same settings is still used, but only for the
        // node it was made from: an ancestor's no longer matches once the
        // node picks for itself.
        .filter(|a| a.location.recipe != keep_recipe || a.location.source_node_id != node_id)
        .collect())
}

/// Make location `place_id` this node's own pick: enable Files on it if it
/// is not its own, then pick the location (an override, never the ancestor's
/// choice). Refuses a node with children. Remove what [`migrate_impact`]
/// names first. Writes: never on the UI thread.
pub fn migrate(fleet: &FleetStore, node_id: &str, place_id: &str) -> Result<()> {
    fleet.reload_if_stale().ok();
    refuse_children(fleet, node_id)?;
    let uuid = uuid::Uuid::parse_str(node_id).with_context(|| format!("node id {node_id}"))?;
    let own = fleet
        .read(|conn| crate::outline::repos::NodeRepo::new(conn).list_capabilities(uuid))?
        .contains(&Capability::Files);
    if !own {
        fleet.enqueue_outline(OutlineMutation::EnableCapabilities {
            node_id: uuid,
            capabilities: vec![Capability::Files],
        })?;
        fleet.writer().flush().context("Could not enable Files on the node")?;
    }
    // Two mutations: a failure between them leaves Files on with the
    // location it already resolved to.
    let picked = fleet
        .enqueue(FleetMutation::SetNodeFilesPlace {
            node_id: node_id.to_string(),
            place_id: Some(place_id.to_string()),
        })
        .map_err(anyhow::Error::from)
        .and_then(|_| fleet.writer().flush().map_err(anyhow::Error::from));
    if let Err(err) = picked {
        let note = if own {
            ""
        } else {
            " Files is now on for the node, using the location it already resolved to."
        };
        return Err(err.context(format!("Could not pick the location.{note}")));
    }
    fleet.reload_if_stale()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::repos::files_location::FilesLocation;
    use crate::fleet::test_util::{cleanup_fleet_root, temp_fleet_root};
    use crate::outline::types::Capability;
    use crate::outline::{CreatePosition, OutlineMutation};
    use uuid::Uuid;

    fn node(store: &FleetStore, title: &str) -> Uuid {
        if store.list_outline_lists().unwrap().is_empty() {
            store
                .enqueue_outline(OutlineMutation::CreateList { slug: "t".into(), title: "T".into() })
                .unwrap();
            store.writer().flush().unwrap();
        }
        let list_id = store.list_outline_lists().unwrap()[0].id;
        let id = Uuid::new_v4();
        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(id),
                list_id,
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Below,
                title: title.into(),
            })
            .unwrap();
        store
            .enqueue_outline(OutlineMutation::EnableCapabilities {
                node_id: id,
                capabilities: vec![Capability::Files],
            })
            .unwrap();
        store.writer().flush().unwrap();
        id
    }

    fn made(store: &FleetStore, node: Uuid, source: Uuid, place: &FilesPlace) {
        store
            .enqueue(FleetMutation::RecordFilesLocation {
                location: FilesLocation {
                    node_id: node.to_string(),
                    source_node_id: source.to_string(),
                    recipe: place.clone().normalized().recipe_key(),
                    repo: place.repo.clone(),
                    container: None,
                    worktree_path: Some(format!("/wt/{node}")),
                    worktree_lease_id: None,
                    worktree_lease_holder: None,
                    sandbox: None,
                    created_at: 0,
                },
            })
            .unwrap();
        store.writer().flush().unwrap();
    }

    #[test]
    fn editing_touches_only_what_the_recipe_change_leaves_behind() {
        let root = temp_fleet_root();
        let store = FleetStore::open(&root).unwrap();
        let place = save(
            &store,
            &FilesPlace {
                name: "App".into(),
                repo: Some("/src/app".into()),
                use_worktree: true,
                ..Default::default()
            },
        )
        .unwrap();
        let (a, b) = (node(&store, "A"), node(&store, "B"));
        for n in [a, b] {
            store
                .enqueue(FleetMutation::SetNodeFilesPlace {
                    node_id: n.to_string(),
                    place_id: Some(place.id.clone()),
                })
                .unwrap();
        }
        store.writer().flush().unwrap();
        made(&store, a, a, &place);

        // Renaming changes nothing a node works from.
        let renamed = FilesPlace { name: "Renamed".into(), ..place.clone() };
        let impact_of_rename = impact(&store, &place.id, Some(&renamed)).unwrap();
        assert_eq!(impact_of_rename.users.len(), 2);
        assert!(impact_of_rename.locations.is_empty());

        // Another directory leaves A's worktree behind.
        let moved = FilesPlace { repo: Some("/src/other".into()), ..place.clone() };
        let impact_of_move = impact(&store, &place.id, Some(&moved)).unwrap();
        assert_eq!(impact_of_move.locations.len(), 1);
        assert_eq!(impact_of_move.locations[0].node_title, "A");

        // Deleting leaves every one behind, and unpicks the nodes.
        let impact_of_delete = impact(&store, &place.id, None).unwrap();
        assert_eq!(impact_of_delete.locations.len(), 1);
        delete(&store, &place.id).unwrap();
        assert!(find(&store, "App").unwrap().is_none());
        let files = store.resolve_files_for_node(&a.to_string()).unwrap().unwrap();
        assert_eq!(files.incomplete().as_deref(), Some("Choose a location"));

        drop(store);
        cleanup_fleet_root(&root);
    }

    fn child_of(store: &FleetStore, parent: Uuid) -> Uuid {
        let list_id = store.list_outline_lists().unwrap()[0].id;
        let id = Uuid::new_v4();
        store
            .enqueue_outline(OutlineMutation::CreateNode {
                node_id: Some(id),
                list_id,
                parent_id: Some(parent),
                anchor_id: None,
                position: CreatePosition::Below,
                title: "child".into(),
            })
            .unwrap();
        store.writer().flush().unwrap();
        id
    }

    fn two_places(store: &FleetStore) -> (FilesPlace, FilesPlace) {
        let a = save(
            store,
            &FilesPlace { name: "A".into(), repo: Some("/src/a".into()), use_worktree: true, ..Default::default() },
        )
        .unwrap();
        let b = save(
            store,
            &FilesPlace { name: "B".into(), repo: Some("/src/b".into()), use_worktree: true, ..Default::default() },
        )
        .unwrap();
        (a, b)
    }

    #[test]
    fn migrating_removes_only_what_differs_and_inherited_ones() {
        let root = temp_fleet_root();
        let store = FleetStore::open(&root).unwrap();
        let (a, b) = two_places(&store);
        let n = node(&store, "N");
        let ns = n.to_string();
        store
            .enqueue(FleetMutation::SetNodeFilesPlace { node_id: ns.clone(), place_id: Some(a.id.clone()) })
            .unwrap();
        store.writer().flush().unwrap();
        made(&store, n, n, &a);
        // Own pick, another recipe: its worktree is left behind.
        let affected = migrate_impact(&store, &ns, &b.id).unwrap();
        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0].node_id, ns);
        // Same recipe, made for this node: nothing to remove.
        assert!(migrate_impact(&store, &ns, &a.id).unwrap().is_empty());
        drop(store);
        cleanup_fleet_root(&root);
    }

    #[test]
    fn an_inherited_pick_is_left_behind_even_with_the_same_recipe() {
        let root = temp_fleet_root();
        let store = FleetStore::open(&root).unwrap();
        let (a, _) = two_places(&store);
        let parent = node(&store, "P");
        store
            .enqueue(FleetMutation::SetNodeFilesPlace { node_id: parent.to_string(), place_id: Some(a.id.clone()) })
            .unwrap();
        store.writer().flush().unwrap();
        // The leaf has no Files of its own: its location comes from the parent.
        let leaf = child_of(&store, parent);
        made(&store, leaf, parent, &a);
        // The parent has a child, so it cannot migrate; the leaf can.
        let affected = migrate_impact(&store, &leaf.to_string(), &a.id).unwrap();
        assert_eq!(affected.len(), 1);
        drop(store);
        cleanup_fleet_root(&root);
    }

    #[test]
    fn migrate_enables_files_and_sets_the_place() {
        let root = temp_fleet_root();
        let store = FleetStore::open(&root).unwrap();
        let (a, _) = two_places(&store);
        let parent = node(&store, "P");
        store
            .enqueue(FleetMutation::SetNodeFilesPlace { node_id: parent.to_string(), place_id: Some(a.id.clone()) })
            .unwrap();
        store.writer().flush().unwrap();
        let leaf = child_of(&store, parent);
        let b = find(&store, "B").unwrap().unwrap();
        assert!(store.resolve_files_for_node(&leaf.to_string()).unwrap().unwrap().inherited);
        migrate(&store, &leaf.to_string(), &b.id).unwrap();
        let files = store.resolve_files_for_node(&leaf.to_string()).unwrap().unwrap();
        assert!(!files.inherited);
        assert_eq!(files.place_id.as_deref(), Some(b.id.as_str()));
        drop(store);
        cleanup_fleet_root(&root);
    }

    #[test]
    fn a_node_with_children_cannot_migrate() {
        let root = temp_fleet_root();
        let store = FleetStore::open(&root).unwrap();
        let (a, _) = two_places(&store);
        let parent = node(&store, "P");
        child_of(&store, parent);
        let err = migrate_impact(&store, &parent.to_string(), &a.id).unwrap_err();
        assert_eq!(err.to_string(), "You cannot migrate a node that has children.");
        let err = migrate(&store, &parent.to_string(), &a.id).unwrap_err();
        assert_eq!(err.to_string(), "You cannot migrate a node that has children.");
        // Nothing was picked.
        let files = store.resolve_files_for_node(&parent.to_string()).unwrap().unwrap();
        assert_ne!(files.place_id.as_deref(), Some(a.id.as_str()));
        drop(store);
        cleanup_fleet_root(&root);
    }

    #[test]
    fn a_live_run_lease_blocks_removing_the_location() {
        let root = temp_fleet_root();
        let store = FleetStore::open(&root).unwrap();
        let n = node(&store, "N");
        assert_eq!(store.location_blocker(&n.to_string()).unwrap(), None);
        let lease =
            crate::activity::ActivityLease::start(&store, n, crate::activity::KIND_RUN, "test").unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        let blocker = store.location_blocker(&n.to_string()).unwrap().expect("a run blocks");
        assert!(blocker.contains("runner"), "{blocker}");
        lease.end(&store).unwrap();
        store.writer().flush().unwrap();
        store.reload_if_stale().ok();
        assert_eq!(store.location_blocker(&n.to_string()).unwrap(), None);
        drop(store);
        cleanup_fleet_root(&root);
    }

    #[test]
    fn saving_a_bad_location_says_why_and_stores_nothing() {
        let root = temp_fleet_root();
        let store = FleetStore::open(&root).unwrap();
        let good = FilesPlace { name: "App".into(), repo: Some("/a".into()), ..Default::default() };
        save(&store, &good).unwrap();
        let err = save(&store, &FilesPlace { name: "app".into(), ..good.clone() }).unwrap_err();
        assert!(err.to_string().contains("already a location named"), "{err}");
        assert_eq!(list(&store).unwrap().len(), 1);
        // A location deleted meanwhile is not silently re-created by an edit.
        let gone = FilesPlace { id: "nope".into(), name: "X".into(), ..good.clone() };
        let err = save(&store, &gone).unwrap_err();
        assert!(err.to_string().contains("deleted meanwhile"), "{err}");
        assert_eq!(list(&store).unwrap().len(), 1);
        drop(store);
        cleanup_fleet_root(&root);
    }
}
