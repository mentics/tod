use super::*;
use crate::fleet::schema;
use crate::interview::{ACTOR_USER, InterviewCommand, PHASE_REQUIREMENTS};
use crate::outline::repos::plan_steps::HandoffReason;
use crate::outline::repos::{ListRepo, NodeRepo, ObligationRepo, OutlineRepo, PlanStepRepo};
use crate::outline::types::{Capability, OutlineEntry};
use crate::outline::uuid_blob::uuid_to_blob;
use crate::outline::{
    CreatePosition, KIND_CONSTRAINT, KIND_REQUIREMENT, OutlineMutation, ReorderDirection,
};
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::Value;
use std::path::{Path, PathBuf};
use uuid::Uuid;

type M = OutlineMutation;

struct Fx {
    dir: PathBuf,
    conn: Connection,
    list: Uuid,
    /// Two top-level Spec nodes.
    n1: Uuid,
    n2: Uuid,
    conv: Uuid,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn setup() -> Fx {
    let dir = std::env::temp_dir().join(format!("tod-conversation-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let conn = schema::open_writer_connection(&dir.join("tod.db")).unwrap();
    let list = ListRepo::new(&conn).create("t", "T").unwrap().id;
    let n1 = add_node(&conn, list, None, 0, "One");
    let n2 = add_node(&conn, list, None, 1, "Two");
    let conv = ConversationRepo::new(&conn)
        .create(Focus::Node(n1), ProtocolKind::Outline, Some("claude"), None, None)
        .unwrap()
        .id;
    ConversationRepo::new(&conn)
        .append_turn(
            conv,
            TurnRole::User,
            "Tighten up the auth requirements please",
        )
        .unwrap();
    Fx {
        dir,
        conn,
        list,
        n1,
        n2,
        conv,
    }
}

fn add_node(
    conn: &Connection,
    list: Uuid,
    parent: Option<Uuid>,
    ordinal: i32,
    title: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let slug = format!("{}-{}", title.to_lowercase(), &id.simple().to_string()[..6]);
    NodeRepo::new(conn)
        .create_with_id(id, &slug, title)
        .unwrap();
    NodeRepo::new(conn)
        .enable_capability(id, Capability::Spec)
        .unwrap();
    OutlineRepo::new(conn)
        .insert(&OutlineEntry {
            node_id: id,
            list_id: list,
            parent_id: parent,
            ordinal,
            collapsed: false,
        })
        .unwrap();
    id
}

fn add_obligation(conn: &Connection, node: Uuid, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    let repo = ObligationRepo::new(conn);
    let index = repo
        .list_ids_for_kind(node, KIND_REQUIREMENT)
        .unwrap()
        .len();
    repo.insert_at(
        id,
        node,
        KIND_REQUIREMENT,
        index,
        None,
        body,
        PHASE_REQUIREMENTS,
    )
    .unwrap();
    id
}

fn add_step(conn: &Connection, node: Uuid, body: &str) -> Uuid {
    let id = Uuid::new_v4();
    let repo = PlanStepRepo::new(conn);
    let index = repo.list_ids_for_node(node).unwrap().len();
    repo.insert_at(id, node, index, body).unwrap();
    id
}

/// Run `cmd` the way the fleet writer does: one transaction, rolled back on error.
fn run(conn: &Connection, actor: &str, cmd: InterviewCommand) -> Result<Value> {
    let tx = conn.unchecked_transaction()?;
    conn.execute(
        "UPDATE interview_actor SET actor = ?1 WHERE id = 1",
        [actor],
    )?;
    let out = crate::interview::execute(conn, Path::new("."), actor, &cmd)?;
    conn.execute("UPDATE interview_actor SET actor = 'user' WHERE id = 1", [])?;
    tx.commit()?;
    Ok(out)
}

impl Fx {
    fn agent(&self, mutation: M) -> Result<Value> {
        run(
            &self.conn,
            &actor_for(self.conv),
            InterviewCommand::Outline {
                mutation,
                target: None,
            },
        )
    }

    fn edit(&self, mutation: M) -> Result<Value> {
        run(
            &self.conn,
            ACTOR_USER,
            InterviewCommand::ConversationEdit {
                conversation_id: self.conv,
                mutation,
            },
        )
    }

    fn reverse(
        &self,
        action_ids: Vec<i64>,
        include_dependents: bool,
        force: bool,
    ) -> Result<ReverseOutcome> {
        let value = run(
            &self.conn,
            ACTOR_USER,
            InterviewCommand::ReverseConversationActions {
                conversation_id: self.conv,
                action_ids,
                include_dependents,
                force,
            },
        )?;
        Ok(serde_json::from_value(value)?)
    }

    fn changes(&self) -> Vec<NetChange> {
        net_changes(&self.conn, self.conv).unwrap()
    }

    fn change(&self, id: Uuid) -> Option<NetChange> {
        self.changes().into_iter().find(|c| c.id == id)
    }

    fn snap(&self, entity: Entity, id: Uuid) -> Option<EntitySnapshot> {
        snapshot(&self.conn, entity, id).unwrap()
    }

    fn action_count(&self) -> i64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM conversation_actions", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// Reverse everything the change set shows for `id`.
    fn reverse_item(&self, id: Uuid) -> ReverseOutcome {
        let change = self.change(id).expect("item is in the change set");
        self.reverse(change.action_ids, false, false).unwrap()
    }
}

fn applied(outcome: ReverseOutcome) -> Vec<i64> {
    match outcome {
        ReverseOutcome::Applied { new_action_ids } => new_action_ids,
        other => panic!("expected applied, got {other:?}"),
    }
}

fn every_mutation() -> Vec<(M, Option<ActionKind>)> {
    use ActionKind::*;
    let id = Uuid::new_v4;
    let snap = || EntitySnapshot::PlanStep {
        node_id: Uuid::nil(),
        ordinal: 1,
        body: String::new(),
        status: "pending".into(),
        note: None,
        reason: None,
        depends_on: vec![],
        satisfies: vec![],
    };
    vec![
        (
            M::CreateList {
                slug: "s".into(),
                title: "t".into(),
            },
            None,
        ),
        (
            M::CreateNode {
                node_id: Some(id()),
                list_id: id(),
                parent_id: None,
                anchor_id: None,
                position: CreatePosition::Child,
                title: "t".into(),
            },
            Some(Create),
        ),
        (
            M::UpdateNodeTitle {
                node_id: id(),
                title: "t".into(),
            },
            Some(Edit),
        ),
        (
            M::SetNodeCollapsed {
                node_id: id(),
                collapsed: true,
            },
            None,
        ),
        (
            M::ReparentNode {
                node_id: id(),
                parent_id: None,
                ordinal: 0,
            },
            Some(Move),
        ),
        (
            M::ReorderSibling {
                node_id: id(),
                direction: ReorderDirection::Up,
            },
            Some(Move),
        ),
        (
            M::EnableCapabilities {
                node_id: id(),
                capabilities: vec![],
            },
            None,
        ),
        (
            M::DisableCapability {
                node_id: id(),
                capability: Capability::Spec,
                archive_payload: String::new(),
            },
            None,
        ),
        (
            M::CreateObligation {
                obligation_id: Some(id()),
                node_id: id(),
                kind: KIND_REQUIREMENT.into(),
                after_id: None,
                before: false,
                section: None,
                body: "b".into(),
                phase: PHASE_REQUIREMENTS.into(),
            },
            Some(Create),
        ),
        (
            M::UpdateObligationBody {
                obligation_id: id(),
                body: "b".into(),
            },
            Some(Edit),
        ),
        (
            M::UpdateObligationVisualDesign {
                obligation_id: id(),
                path: None,
            },
            Some(Edit),
        ),
        (
            M::UpdateObligationSection {
                obligation_id: id(),
                section: None,
            },
            Some(Edit),
        ),
        (
            M::UpdateObligationPhase {
                obligation_id: id(),
                phase: "design".into(),
            },
            Some(Edit),
        ),
        (
            M::RenameObligationSection {
                node_id: id(),
                kind: KIND_REQUIREMENT.into(),
                old_section: None,
                new_section: "s".into(),
            },
            None,
        ),
        (
            M::DeleteObligation {
                obligation_id: id(),
            },
            Some(Delete),
        ),
        (M::RestoreObligation { rev: 1 }, None),
        (
            M::MoveObligation {
                obligation_id: id(),
                target_node_id: id(),
            },
            Some(Move),
        ),
        (M::DeleteNode { node_id: id() }, Some(Delete)),
        (
            M::RestoreNodeSubtree {
                archive_id: id(),
                root_node_id: id(),
            },
            None,
        ),
        (
            M::ReorderObligation {
                obligation_id: id(),
                direction: ReorderDirection::Down,
            },
            Some(Move),
        ),
        (
            M::RestoreObligationRow {
                obligation_id: id(),
                snapshot: snap(),
            },
            None,
        ),
        (
            M::PlaceObligation {
                id: id(),
                node_id: id(),
                ordinal: 1,
            },
            Some(Move),
        ),
        (
            M::CreatePlanStep {
                step_id: Some(id()),
                node_id: id(),
                after_id: None,
                before: false,
                body: "b".into(),
            },
            Some(Create),
        ),
        (
            M::UpdatePlanStepBody {
                step_id: id(),
                body: "b".into(),
            },
            Some(Edit),
        ),
        (
            M::UpdatePlanStepStatus {
                step_id: id(),
                status: "ready".into(),
                note: None,
                reason: None,
            },
            Some(Edit),
        ),
        (M::DeletePlanStep { step_id: id() }, Some(Delete)),
        (
            M::ReorderPlanStep {
                step_id: id(),
                direction: ReorderDirection::Up,
            },
            Some(Move),
        ),
        (
            M::AddPlanStepDependency {
                step_id: id(),
                depends_on_step_id: id(),
            },
            Some(Edit),
        ),
        (
            M::RemovePlanStepDependency {
                step_id: id(),
                depends_on_step_id: id(),
            },
            Some(Edit),
        ),
        (
            M::LinkPlanStepObligation {
                step_id: id(),
                obligation_id: id(),
            },
            Some(Edit),
        ),
        (
            M::UnlinkPlanStepObligation {
                step_id: id(),
                obligation_id: id(),
            },
            Some(Edit),
        ),
        (
            M::RestorePlanStep {
                step_id: id(),
                snapshot: snap(),
            },
            None,
        ),
        (
            M::PlacePlanStep {
                id: id(),
                ordinal: 1,
            },
            Some(Move),
        ),
        (
            M::PlaceNode {
                node_id: id(),
                parent_id: None,
                index: 0,
            },
            Some(Move),
        ),
        (
            M::SetExtraContent {
                node_id: id(),
                content_type: "details".into(),
                body: String::new(),
            },
            None,
        ),
        (
            M::SetLifecycle {
                node_id: id(),
                state: "active".into(),
            },
            None,
        ),
        (
            M::ApplyGateResults {
                node_id: id(),
                results: vec![],
                forward_state: None,
                source: String::new(),
            },
            None,
        ),
        (
            M::SetGeneratorConfig {
                node_id: id(),
                data_source_type: String::new(),
                config_json: String::new(),
            },
            None,
        ),
        (M::DeleteGeneratorConfig { node_id: id() }, None),
        (
            M::CreateManagedNode {
                node_id: Some(id()),
                list_id: id(),
                parent_id: id(),
                title: String::new(),
                external_id: String::new(),
                source_type: String::new(),
                generator_node_id: id(),
                tags: vec![],
                body: String::new(),
            },
            None,
        ),
        (
            M::UpdateManagedNode {
                node_id: id(),
                title: String::new(),
                tags: vec![],
                body: String::new(),
            },
            None,
        ),
        (
            M::DeleteManagedNodes {
                generator_node_id: id(),
            },
            None,
        ),
        (M::DeleteManagedNode { node_id: id() }, None),
        (
            M::SetManagedNodeLink {
                node_id: id(),
                generator_node_id: id(),
                external_id: String::new(),
                source_type: String::new(),
            },
            None,
        ),
        (
            M::ClearManagedNodeLinks {
                generator_node_id: id(),
            },
            None,
        ),
        (
            M::ClearStaleCopyLinks {
                generator_node_id: id(),
                external_id: String::new(),
            },
            None,
        ),
        (
            M::RefreshLinkedCopy {
                node_id: id(),
                title: None,
                tags: None,
                body: None,
            },
            None,
        ),
        (
            M::PasteManagedNodeCopy {
                source_node_id: id(),
                list_id: id(),
                parent_id: None,
                ordinal: 0,
            },
            None,
        ),
        (
            M::SetRefreshStatus {
                node_id: id(),
                status: String::new(),
                error: None,
            },
            None,
        ),
    ]
}

#[test]
fn classify_covers_every_mutation_kind() {
    for (mutation, expected) in every_mutation() {
        let got = classify(&mutation);
        assert_eq!(got.map(|(kind, _, _)| kind), expected, "{mutation:?}");
        if let Some((_, entity, _)) = got {
            // Every recorded mutation is either a create (which the inverse
            // turns into a delete) or has an inverse built from a snapshot.
            let expected_entity = match &mutation {
                M::CreateNode { .. }
                | M::UpdateNodeTitle { .. }
                | M::ReparentNode { .. }
                | M::ReorderSibling { .. }
                | M::PlaceNode { .. }
                | M::DeleteNode { .. } => Entity::Node,
                M::CreatePlanStep { .. }
                | M::UpdatePlanStepBody { .. }
                | M::UpdatePlanStepStatus { .. }
                | M::DeletePlanStep { .. }
                | M::ReorderPlanStep { .. }
                | M::PlacePlanStep { .. }
                | M::AddPlanStepDependency { .. }
                | M::RemovePlanStepDependency { .. }
                | M::LinkPlanStepObligation { .. }
                | M::UnlinkPlanStepObligation { .. } => Entity::PlanStep,
                _ => Entity::Obligation,
            };
            assert_eq!(entity, expected_entity, "{mutation:?}");
        }
    }
    // A create is recorded only once it has an id; `normalize` gives it one.
    let bare = M::CreatePlanStep {
        step_id: None,
        node_id: Uuid::new_v4(),
        after_id: None,
        before: false,
        body: "b".into(),
    };
    assert!(classify(&bare).is_none());
    assert!(matches!(
        classify(&normalize(bare)),
        Some((ActionKind::Create, Entity::PlanStep, _))
    ));
}

#[test]
fn net_ops_fold_per_item() {
    let fx = setup();
    let original = add_obligation(&fx.conn, fx.n1, "Original.");

    // create + edit = Added
    let created = Uuid::new_v4();
    fx.agent(M::CreateObligation {
        obligation_id: Some(created),
        node_id: fx.n1,
        kind: KIND_REQUIREMENT.into(),
        after_id: None,
        before: false,
        section: None,
        body: "Draft.".into(),
        phase: PHASE_REQUIREMENTS.into(),
    })
    .unwrap();
    fx.agent(M::UpdateObligationBody {
        obligation_id: created,
        body: "Final.".into(),
    })
    .unwrap();
    let change = fx.change(created).unwrap();
    assert_eq!(change.op, NetOp::Added);
    assert_eq!(change.before, None);
    assert_eq!(change.current.as_ref().unwrap().text(), "Final.");
    assert_eq!(change.action_ids.len(), 2);

    // edit + edit = Edited, with the earliest before
    fx.agent(M::UpdateObligationBody {
        obligation_id: original,
        body: "Second.".into(),
    })
    .unwrap();
    fx.agent(M::UpdateObligationBody {
        obligation_id: original,
        body: "Third.".into(),
    })
    .unwrap();
    let change = fx.change(original).unwrap();
    assert_eq!(change.op, NetOp::Edited);
    assert_eq!(change.before.as_ref().unwrap().text(), "Original.");
    assert_eq!(change.current.as_ref().unwrap().text(), "Third.");

    // create + delete = hidden
    let fleeting = Uuid::new_v4();
    fx.agent(M::CreatePlanStep {
        step_id: Some(fleeting),
        node_id: fx.n1,
        after_id: None,
        before: false,
        body: "Gone soon.".into(),
    })
    .unwrap();
    fx.agent(M::DeletePlanStep { step_id: fleeting }).unwrap();
    assert!(fx.change(fleeting).is_none());

    // A deleted item shows as deleted; a moved one as moved, with where it came from.
    let doomed = add_obligation(&fx.conn, fx.n2, "Doomed.");
    fx.agent(M::DeleteObligation {
        obligation_id: doomed,
    })
    .unwrap();
    assert_eq!(fx.change(doomed).unwrap().op, NetOp::Deleted);
    let mover = add_obligation(&fx.conn, fx.n2, "Mover.");
    fx.agent(M::MoveObligation {
        obligation_id: mover,
        target_node_id: fx.n1,
    })
    .unwrap();
    let change = fx.change(mover).unwrap();
    assert_eq!(change.op, NetOp::Moved);
    assert_eq!(change.node_id, Some(fx.n1));
    assert!(matches!(
        &change.context[..],
        [ContextRef { phrase, target: ContextTarget::Node { id, label } }]
            if phrase == "from" && *id == fx.n2 && label == "Two"
    ));

    // Grouped by node in tree order: n1's items, then n2's.
    let order: Vec<Option<Uuid>> = fx.changes().iter().map(|c| c.node_id).collect();
    assert_eq!(
        order,
        vec![Some(fx.n1), Some(fx.n1), Some(fx.n1), Some(fx.n2)]
    );

    // The picker entry carries the change count and opening words.
    let listed = ConversationRepo::new(&fx.conn)
        .list_for_focus(Focus::Node(fx.n1))
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].change_count, 4);
    assert_eq!(listed[0].opening, "Tighten up the auth requirements please");
}

#[test]
fn a_deletion_followed_by_a_create_in_the_same_turn_reads_as_replaced() {
    let fx = setup();
    let old = add_obligation(&fx.conn, fx.n1, "Old wording.");
    fx.agent(M::DeleteObligation { obligation_id: old })
        .unwrap();
    let new = Uuid::new_v4();
    fx.agent(M::CreateObligation {
        obligation_id: Some(new),
        node_id: fx.n1,
        kind: KIND_REQUIREMENT.into(),
        after_id: None,
        before: false,
        section: None,
        body: "New wording.".into(),
        phase: PHASE_REQUIREMENTS.into(),
    })
    .unwrap();
    let change = fx.change(old).unwrap();
    assert!(matches!(
        &change.context[..],
        [ContextRef { phrase, target: ContextTarget::Item { id, .. } }]
            if phrase == "replaced by" && *id == new
    ));

    let keep = add_obligation(&fx.conn, fx.n2, "Same thing.");
    let dup = add_obligation(&fx.conn, fx.n2, "same  thing.");
    ConversationRepo::new(&fx.conn)
        .append_turn(fx.conv, TurnRole::User, "Dedupe")
        .unwrap();
    fx.agent(M::DeleteObligation { obligation_id: dup })
        .unwrap();
    let change = fx.change(dup).unwrap();
    assert!(matches!(
        &change.context[..],
        [ContextRef { phrase, target: ContextTarget::Item { id, .. } }]
            if phrase == "duplicate of" && *id == keep
    ));
}

/// Apply `mutation` as the agent on `entity`/`id`, reverse it, and reverse the
/// reversal, checking the item's state at each step.
fn round_trip(fx: &Fx, entity: Entity, id: Uuid, mutation: M, op: NetOp) {
    let pre = fx.snap(entity, id);
    let rows_before = fx.action_count();
    fx.agent(mutation.clone()).unwrap();
    assert_eq!(fx.action_count(), rows_before + 1, "{mutation:?}");
    let post = fx.snap(entity, id);
    assert_ne!(pre, post, "{mutation:?} changed nothing");
    assert_eq!(fx.change(id).unwrap().op, op, "{mutation:?}");

    // Earlier cases on the same item are already reversed, so reversing the
    // item reverses only this one.
    let reversed = applied(fx.reverse_item(id));
    assert_eq!(reversed.len(), 1);
    assert_eq!(fx.snap(entity, id), pre, "reversing {mutation:?}");
    assert_eq!(fx.change(id).unwrap().op, NetOp::Reversed, "{mutation:?}");

    // Re-applying the item would replay the earlier cases too; replay just this.
    let reapplied = applied(fx.reverse(reversed, false, false).unwrap());
    assert_eq!(fx.snap(entity, id), post, "re-applying {mutation:?}");
    assert_eq!(fx.change(id).unwrap().op, op, "{mutation:?}");

    // And back once more, so the item is as it started for the next case.
    applied(fx.reverse(reapplied, false, false).unwrap());
    assert_eq!(fx.snap(entity, id), pre, "reversing again {mutation:?}");
}

#[test]
fn nodes_round_trip_through_reversal() {
    let fx = setup();
    let a = add_node(&fx.conn, fx.list, Some(fx.n1), 0, "A");
    let b = add_node(&fx.conn, fx.list, Some(fx.n1), 1, "B");
    let c = add_node(&fx.conn, fx.list, Some(fx.n1), 2, "C");

    let created = Uuid::new_v4();
    round_trip(
        &fx,
        Entity::Node,
        created,
        M::CreateNode {
            node_id: Some(created),
            list_id: fx.list,
            parent_id: None,
            anchor_id: Some(b),
            position: CreatePosition::Below,
            title: "New".into(),
        },
        NetOp::Added,
    );
    round_trip(
        &fx,
        Entity::Node,
        b,
        M::UpdateNodeTitle {
            node_id: b,
            title: "Bee".into(),
        },
        NetOp::Edited,
    );
    round_trip(
        &fx,
        Entity::Node,
        b,
        M::ReorderSibling {
            node_id: b,
            direction: ReorderDirection::Up,
        },
        NetOp::Moved,
    );
    round_trip(
        &fx,
        Entity::Node,
        c,
        M::ReparentNode {
            node_id: c,
            parent_id: Some(fx.n2),
            ordinal: 0,
        },
        NetOp::Moved,
    );
    round_trip(
        &fx,
        Entity::Node,
        a,
        M::DeleteNode { node_id: a },
        NetOp::Deleted,
    );
    let children: Vec<Uuid> = {
        let mut entries: Vec<_> = OutlineRepo::new(&fx.conn)
            .list_for_list(fx.list)
            .unwrap()
            .into_iter()
            .filter(|e| e.parent_id == Some(fx.n1))
            .collect();
        entries.sort_by_key(|e| e.ordinal);
        entries.into_iter().map(|e| e.node_id).collect()
    };
    assert_eq!(children, vec![a, b, c]);
}

#[test]
fn obligations_round_trip_through_reversal() {
    let fx = setup();
    let first = add_obligation(&fx.conn, fx.n1, "First.");
    let second = add_obligation(&fx.conn, fx.n1, "Second.");
    let third = add_obligation(&fx.conn, fx.n1, "Third.");
    fx.conn
        .execute(
            "UPDATE node_obligations SET visual_design_path = 'mock.html' WHERE id = ?1",
            params![uuid_to_blob(second)],
        )
        .unwrap();

    let created = Uuid::new_v4();
    round_trip(
        &fx,
        Entity::Obligation,
        created,
        M::CreateObligation {
            obligation_id: Some(created),
            node_id: fx.n1,
            kind: KIND_CONSTRAINT.into(),
            after_id: None,
            before: false,
            section: Some("Limits".into()),
            body: "Created.".into(),
            phase: PHASE_REQUIREMENTS.into(),
        },
        NetOp::Added,
    );
    for mutation in [
        M::UpdateObligationBody {
            obligation_id: second,
            body: "Reworded.".into(),
        },
        M::UpdateObligationSection {
            obligation_id: second,
            section: Some("Core".into()),
        },
        M::UpdateObligationPhase {
            obligation_id: second,
            phase: "design".into(),
        },
        M::UpdateObligationVisualDesign {
            obligation_id: second,
            path: None,
        },
    ] {
        round_trip(&fx, Entity::Obligation, second, mutation, NetOp::Edited);
    }
    round_trip(
        &fx,
        Entity::Obligation,
        first,
        M::ReorderObligation {
            obligation_id: first,
            direction: ReorderDirection::Down,
        },
        NetOp::Moved,
    );
    round_trip(
        &fx,
        Entity::Obligation,
        second,
        M::MoveObligation {
            obligation_id: second,
            target_node_id: fx.n2,
        },
        NetOp::Moved,
    );
    round_trip(
        &fx,
        Entity::Obligation,
        second,
        M::DeleteObligation {
            obligation_id: second,
        },
        NetOp::Deleted,
    );
    let ids: Vec<Uuid> = ObligationRepo::new(&fx.conn)
        .list_ids_for_kind(fx.n1, KIND_REQUIREMENT)
        .unwrap();
    assert_eq!(ids, vec![first, second, third]);
}

#[test]
fn plan_steps_round_trip_through_reversal() {
    let fx = setup();
    let s1 = add_step(&fx.conn, fx.n1, "One.");
    let s2 = add_step(&fx.conn, fx.n1, "Two.");
    let s3 = add_step(&fx.conn, fx.n1, "Three.");
    let o = add_obligation(&fx.conn, fx.n1, "Linked.");
    let steps = PlanStepRepo::new(&fx.conn);
    steps.add_dependency(s2, s1).unwrap();
    steps.link_obligation(s2, o).unwrap();

    let created = Uuid::new_v4();
    round_trip(
        &fx,
        Entity::PlanStep,
        created,
        M::CreatePlanStep {
            step_id: Some(created),
            node_id: fx.n1,
            after_id: Some(s1),
            before: false,
            body: "Inserted.".into(),
        },
        NetOp::Added,
    );
    for mutation in [
        M::UpdatePlanStepBody {
            step_id: s2,
            body: "Deux.".into(),
        },
        M::UpdatePlanStepStatus {
            step_id: s2,
            status: "in_progress".into(),
            note: None,
            reason: None,
        },
        M::UpdatePlanStepStatus {
            step_id: s2,
            status: "blocked".into(),
            note: Some("Pick one.".into()),
            reason: Some(HandoffReason::Decision {
                options: vec!["This".into(), "That".into()],
            }),
        },
        M::AddPlanStepDependency {
            step_id: s3,
            depends_on_step_id: s1,
        },
        M::RemovePlanStepDependency {
            step_id: s2,
            depends_on_step_id: s1,
        },
        M::LinkPlanStepObligation {
            step_id: s3,
            obligation_id: o,
        },
        M::UnlinkPlanStepObligation {
            step_id: s2,
            obligation_id: o,
        },
    ] {
        let step = match &mutation {
            M::AddPlanStepDependency { step_id, .. }
            | M::LinkPlanStepObligation { step_id, .. } => *step_id,
            _ => s2,
        };
        round_trip(&fx, Entity::PlanStep, step, mutation, NetOp::Edited);
    }
    // Reopening a step the agent handed back, then reversing that, gives the
    // step back its reason and note.
    steps
        .update_status(
            s3,
            "blocked",
            Some("Obligations disagree."),
            Some(&HandoffReason::Conflict {
                obligations: vec![o, Uuid::new_v4()],
            }),
        )
        .unwrap();
    round_trip(
        &fx,
        Entity::PlanStep,
        s3,
        M::UpdatePlanStepStatus {
            step_id: s3,
            status: "in_progress".into(),
            note: None,
            reason: None,
        },
        NetOp::Edited,
    );
    assert!(matches!(
        steps.get(s3).unwrap().unwrap().reason,
        Some(HandoffReason::Conflict { .. })
    ));
    round_trip(
        &fx,
        Entity::PlanStep,
        s3,
        M::ReorderPlanStep {
            step_id: s3,
            direction: ReorderDirection::Up,
        },
        NetOp::Moved,
    );
    round_trip(
        &fx,
        Entity::PlanStep,
        s2,
        M::DeletePlanStep { step_id: s2 },
        NetOp::Deleted,
    );
    assert_eq!(steps.list_ids_for_node(fx.n1).unwrap(), vec![s1, s2, s3]);
    assert_eq!(steps.list_dependencies(s2).unwrap(), vec![s1]);
    assert_eq!(steps.list_obligations(s2).unwrap(), vec![o]);
}

#[test]
fn reversing_and_reapplying_an_item_replays_all_its_actions_in_order() {
    let fx = setup();
    let step = Uuid::new_v4();
    fx.agent(M::CreatePlanStep {
        step_id: Some(step),
        node_id: fx.n1,
        after_id: None,
        before: false,
        body: "Draft.".into(),
    })
    .unwrap();
    fx.agent(M::UpdatePlanStepBody {
        step_id: step,
        body: "Final.".into(),
    })
    .unwrap();
    let post = fx.snap(Entity::PlanStep, step);

    assert_eq!(applied(fx.reverse_item(step)).len(), 2);
    assert!(fx.snap(Entity::PlanStep, step).is_none());
    assert_eq!(fx.change(step).unwrap().op, NetOp::Reversed);

    assert_eq!(applied(fx.reverse_item(step)).len(), 2);
    assert_eq!(fx.snap(Entity::PlanStep, step), post);
    let change = fx.change(step).unwrap();
    assert_eq!(change.op, NetOp::Added);
    assert_eq!(change.action_ids.len(), 2);
}

#[test]
fn plan_step_dependency_and_link_changes_show_in_the_diff() {
    let fx = setup();
    let s1 = add_step(&fx.conn, fx.n1, "One.");
    let s2 = add_step(&fx.conn, fx.n1, "Two.");
    let o = add_obligation(&fx.conn, fx.n1, "Linked.");
    fx.agent(M::AddPlanStepDependency {
        step_id: s2,
        depends_on_step_id: s1,
    })
    .unwrap();
    fx.agent(M::LinkPlanStepObligation {
        step_id: s2,
        obligation_id: o,
    })
    .unwrap();
    // Re-adding an existing edge is a no-op; reversing it must not remove it.
    fx.agent(M::AddPlanStepDependency {
        step_id: s2,
        depends_on_step_id: s1,
    })
    .unwrap();
    let change = fx.change(s2).unwrap();
    assert_eq!(change.op, NetOp::Edited);
    let deps = |s: &Option<EntitySnapshot>| match s {
        Some(EntitySnapshot::PlanStep {
            depends_on,
            satisfies,
            ..
        }) => (depends_on.clone(), satisfies.clone()),
        other => panic!("{other:?}"),
    };
    assert_eq!(deps(&change.before), (vec![], vec![]));
    assert_eq!(deps(&change.current), (vec![s1], vec![o]));

    let newest = *change.action_ids.last().unwrap();
    applied(fx.reverse(vec![newest], false, false).unwrap());
    assert_eq!(deps(&fx.snap(Entity::PlanStep, s2)), (vec![s1], vec![o]));

    applied(fx.reverse_item(s2));
    assert_eq!(deps(&fx.snap(Entity::PlanStep, s2)), (vec![], vec![]));
}

#[test]
fn a_deleted_node_subtree_comes_back_with_its_plan_steps() {
    let fx = setup();
    let child = add_node(&fx.conn, fx.list, Some(fx.n1), 0, "Child");
    let o = add_obligation(&fx.conn, child, "Child obligation.");
    let s1 = add_step(&fx.conn, fx.n1, "Parent step.");
    let s2 = add_step(&fx.conn, child, "Child step.");
    fx.conn
        .execute(
            "UPDATE node_plan_steps SET status = 'implemented' WHERE id = ?1",
            params![uuid_to_blob(s2)],
        )
        .unwrap();
    let steps = PlanStepRepo::new(&fx.conn);
    steps.add_dependency(s2, s1).unwrap();
    steps.link_obligation(s2, o).unwrap();
    // A step outside the subtree depending on one inside it.
    let outside = add_step(&fx.conn, fx.n2, "Outside.");
    steps.add_dependency(outside, s1).unwrap();

    fx.agent(M::DeleteNode { node_id: fx.n1 }).unwrap();
    assert!(steps.get(s2).unwrap().is_none());
    assert!(steps.list_dependencies(outside).unwrap().is_empty());
    applied(fx.reverse_item(fx.n1));

    let restored = steps.get(s2).unwrap().unwrap();
    assert_eq!(restored.body, "Child step.");
    assert_eq!(restored.status, "implemented");
    assert!(steps.get(s1).unwrap().is_some());
    assert_eq!(steps.list_dependencies(s2).unwrap(), vec![s1]);
    assert_eq!(steps.list_obligations(s2).unwrap(), vec![o]);
    assert!(ObligationRepo::new(&fx.conn).get(o).unwrap().is_some());
}

#[test]
fn a_user_edit_is_recorded_and_outside_changes_are_stale() {
    let fx = setup();
    let o = add_obligation(&fx.conn, fx.n1, "Original.");
    fx.agent(M::UpdateObligationBody {
        obligation_id: o,
        body: "Agent.".into(),
    })
    .unwrap();
    fx.edit(M::UpdateObligationBody {
        obligation_id: o,
        body: "User.".into(),
    })
    .unwrap();
    let actions = ConversationRepo::new(&fx.conn).actions(fx.conv).unwrap();
    assert_eq!(actions.len(), 2);
    assert_eq!(actions[1].actor, ActionActor::User);
    assert!(stale(&fx.conn, fx.conv, &[o]).unwrap().is_empty());

    // A change made outside the conversation makes the item stale…
    ObligationRepo::new(&fx.conn)
        .update_body(o, "Elsewhere.")
        .unwrap();
    assert_eq!(stale(&fx.conn, fx.conv, &[o]).unwrap(), vec![o]);
    // …so reversing asks first, and applies nothing.
    let ids = fx.change(o).unwrap().action_ids;
    match fx.reverse(ids.clone(), false, false).unwrap() {
        ReverseOutcome::NeedsConfirmation {
            conflicts,
            dependents,
        } => {
            assert_eq!(conflicts.len(), 1);
            assert_eq!(conflicts[0].id, o);
            assert!(dependents.is_empty());
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        ObligationRepo::new(&fx.conn).get(o).unwrap().unwrap().body,
        "Elsewhere."
    );
    // Forcing overwrites it with the state before the conversation.
    applied(fx.reverse(ids, false, true).unwrap());
    assert_eq!(
        ObligationRepo::new(&fx.conn).get(o).unwrap().unwrap().body,
        "Original."
    );
}

#[test]
fn reversing_a_created_node_offers_its_dependents() {
    let fx = setup();
    let node = Uuid::new_v4();
    fx.agent(M::CreateNode {
        node_id: Some(node),
        list_id: fx.list,
        parent_id: Some(fx.n1),
        anchor_id: None,
        position: CreatePosition::Child,
        title: "Fresh".into(),
    })
    .unwrap();
    fx.agent(M::EnableCapabilities {
        node_id: node,
        capabilities: vec![Capability::Spec],
    })
    .unwrap();
    let o = Uuid::new_v4();
    fx.agent(M::CreateObligation {
        obligation_id: Some(o),
        node_id: node,
        kind: KIND_REQUIREMENT.into(),
        after_id: None,
        before: false,
        section: None,
        body: "Under the new node.".into(),
        phase: PHASE_REQUIREMENTS.into(),
    })
    .unwrap();
    let step = Uuid::new_v4();
    fx.agent(M::CreatePlanStep {
        step_id: Some(step),
        node_id: fx.n2,
        after_id: None,
        before: false,
        body: "Elsewhere.".into(),
    })
    .unwrap();
    let dependent_step = add_step(&fx.conn, fx.n2, "Depends.");
    fx.agent(M::AddPlanStepDependency {
        step_id: dependent_step,
        depends_on_step_id: step,
    })
    .unwrap();
    // The capability change is not recorded (D11).
    assert_eq!(fx.action_count(), 4);

    let node_actions = fx.change(node).unwrap().action_ids;
    let obligation_actions = fx.change(o).unwrap().action_ids;
    assert_eq!(
        dependents(&fx.conn, fx.conv, &node_actions).unwrap(),
        obligation_actions
    );
    let step_actions = fx.change(step).unwrap().action_ids;
    assert_eq!(
        dependents(&fx.conn, fx.conv, &step_actions).unwrap(),
        fx.change(dependent_step).unwrap().action_ids
    );

    match fx.reverse(node_actions.clone(), false, false).unwrap() {
        ReverseOutcome::NeedsConfirmation {
            conflicts,
            dependents,
        } => {
            assert!(conflicts.is_empty());
            assert_eq!(dependents.iter().map(|c| c.id).collect::<Vec<_>>(), vec![o]);
        }
        other => panic!("{other:?}"),
    }
    assert!(fx.snap(Entity::Node, node).is_some());

    let applied_ids = applied(fx.reverse(node_actions, true, false).unwrap());
    assert_eq!(applied_ids.len(), 2);
    assert!(fx.snap(Entity::Node, node).is_none());
    assert!(fx.snap(Entity::Obligation, o).is_none());
    assert_eq!(fx.change(node).unwrap().op, NetOp::Reversed);
    assert_eq!(fx.change(o).unwrap().op, NetOp::Reversed);
}

#[test]
fn a_batch_reverse_is_atomic() {
    let fx = setup();
    let doomed = add_obligation(&fx.conn, fx.n2, "Doomed.");
    let edited = add_obligation(&fx.conn, fx.n1, "Before.");
    fx.agent(M::DeleteObligation {
        obligation_id: doomed,
    })
    .unwrap();
    fx.agent(M::UpdateObligationBody {
        obligation_id: edited,
        body: "After.".into(),
    })
    .unwrap();
    // Restoring `doomed` will fail: its node no longer takes obligations.
    fx.conn
        .execute(
            "DELETE FROM node_capabilities WHERE node_id = ?1",
            params![uuid_to_blob(fx.n2)],
        )
        .unwrap();
    let ids: Vec<i64> = fx
        .changes()
        .iter()
        .flat_map(|c| c.action_ids.clone())
        .collect();
    assert_eq!(ids.len(), 2);
    let rows = fx.action_count();
    assert!(fx.reverse(ids, false, true).is_err());

    // The edit, reversed first (newest-first), was rolled back with the rest.
    assert_eq!(
        ObligationRepo::new(&fx.conn)
            .get(edited)
            .unwrap()
            .unwrap()
            .body,
        "After."
    );
    assert_eq!(fx.action_count(), rows);
    let actions = ConversationRepo::new(&fx.conn).actions(fx.conv).unwrap();
    assert!(actions.iter().all(|a| a.reversed_by.is_none()));
}

#[test]
fn unsure_flags_live_on_the_change_set() {
    let fx = setup();
    let o = add_obligation(&fx.conn, fx.n1, "Original.");
    let flag = |reason: &str| {
        run(
            &fx.conn,
            &actor_for(fx.conv),
            InterviewCommand::FlagConversationItem {
                conversation_id: fx.conv,
                entity: Entity::Obligation,
                entity_id: o,
                reason: reason.into(),
            },
        )
    };
    let err = flag("Not sure.").unwrap_err();
    assert!(err.to_string().contains("only items it changed"), "{err}");

    fx.agent(M::UpdateObligationBody {
        obligation_id: o,
        body: "Guess.".into(),
    })
    .unwrap();
    flag("Not sure.").unwrap();
    assert_eq!(fx.change(o).unwrap().flag.as_deref(), Some("Not sure."));

    // A user edit settles the doubt.
    fx.edit(M::UpdateObligationBody {
        obligation_id: o,
        body: "Settled.".into(),
    })
    .unwrap();
    assert_eq!(fx.change(o).unwrap().flag, None);

    // So does a reversal.
    flag("Still unsure.").unwrap();
    applied(fx.reverse_item(o));
    assert_eq!(fx.change(o).unwrap().flag, None);

    // And clearing it explicitly.
    flag("Again.").unwrap();
    run(
        &fx.conn,
        ACTOR_USER,
        InterviewCommand::UnflagConversationItem {
            conversation_id: fx.conv,
            entity: Entity::Obligation,
            entity_id: o,
        },
    )
    .unwrap();
    assert_eq!(fx.change(o).unwrap().flag, None);
}

#[test]
fn agent_writes_record_one_action_and_other_actors_record_none() {
    let fx = setup();
    let o = add_obligation(&fx.conn, fx.n1, "Original.");
    let out = fx
        .agent(M::UpdateObligationBody {
            obligation_id: o,
            body: "Agent.".into(),
        })
        .unwrap();
    assert_eq!(fx.action_count(), 1);
    let action = ConversationRepo::new(&fx.conn)
        .action(out["action"].as_i64().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(action.actor, ActionActor::Agent);
    assert_eq!(action.turn_seq, 1);
    assert_eq!(action.node_id, Some(fx.n1));

    // Unrecorded kinds still run.
    fx.agent(M::SetExtraContent {
        node_id: fx.n1,
        content_type: "details".into(),
        body: "Some details.".into(),
    })
    .unwrap();
    assert_eq!(fx.action_count(), 1);

    // Users and interview agents write exactly as before, unrecorded.
    for actor in [ACTOR_USER.to_string(), Uuid::new_v4().to_string()] {
        run(
            &fx.conn,
            &actor,
            InterviewCommand::Outline {
                mutation: M::UpdateObligationBody {
                    obligation_id: o,
                    body: actor.clone(),
                },
                target: Some(o),
            },
        )
        .unwrap();
    }
    assert_eq!(fx.action_count(), 1);

    // An unknown conversation is an error, and nothing is written.
    let err = run(
        &fx.conn,
        &actor_for(Uuid::new_v4()),
        InterviewCommand::Outline {
            mutation: M::UpdateObligationBody {
                obligation_id: o,
                body: "Lost.".into(),
            },
            target: None,
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
    assert_ne!(
        ObligationRepo::new(&fx.conn).get(o).unwrap().unwrap().body,
        "Lost."
    );
    assert!(
        run(
            &fx.conn,
            "conversation:not-a-uuid",
            InterviewCommand::Outline {
                mutation: M::UpdateObligationBody {
                    obligation_id: o,
                    body: "Lost.".into()
                },
                target: None,
            },
        )
        .is_err()
    );
}

#[test]
fn conversations_are_listed_per_focus_and_keep_their_turns() {
    let fx = setup();
    let repo = ConversationRepo::new(&fx.conn);
    let project = repo.create(Focus::Project, ProtocolKind::Outline, None, None, None).unwrap();
    let o = add_obligation(&fx.conn, fx.n1, "Focus.");
    let focus = Focus::Obligation { node: fx.n1, id: o };
    let about = repo.create(focus, ProtocolKind::Outline, None, None, None).unwrap();
    assert_eq!(repo.get(about.id).unwrap().unwrap().focus, focus);
    assert_eq!(
        repo.latest_for_focus(Focus::Project).unwrap().unwrap().id,
        project.id
    );
    assert_eq!(repo.latest_for_focus(focus).unwrap().unwrap().id, about.id);
    assert!(repo.latest_for_focus(Focus::Node(fx.n2)).unwrap().is_none());
    assert_eq!(repo.list_for_focus(Focus::Node(fx.n1)).unwrap().len(), 1);

    repo.append_turn(project.id, TurnRole::User, "Hi").unwrap();
    repo.append_turn(project.id, TurnRole::Agent, "").unwrap();
    repo.append_turn(project.id, TurnRole::Rotation, "")
        .unwrap();
    let turns = repo.turns(project.id).unwrap();
    assert_eq!(
        turns.iter().map(|t| (t.seq, t.role)).collect::<Vec<_>>(),
        vec![
            (1, TurnRole::User),
            (2, TurnRole::Agent),
            (3, TurnRole::Rotation)
        ]
    );
    assert_eq!(repo.max_user_seq(project.id).unwrap(), 1);

    repo.set_agent_session(project.id, Some("sess-1"), Some("Project chat"))
        .unwrap();
    let row = repo.get(project.id).unwrap().unwrap();
    assert_eq!(row.agent_session_id.as_deref(), Some("sess-1"));
    assert_eq!(row.session_name.as_deref(), Some("Project chat"));
    repo.set_agent_session(project.id, None, None).unwrap();
    let row = repo.get(project.id).unwrap().unwrap();
    assert_eq!(row.agent_session_id, None);
    assert_eq!(row.session_name.as_deref(), Some("Project chat"));

    // The newest conversation for a focus comes first.
    let newer = repo.create(Focus::Project, ProtocolKind::Outline, None, None, None).unwrap();
    fx.conn
        .execute(
            "UPDATE conversations SET updated_at = updated_at + 1000 WHERE id = ?1",
            params![uuid_to_blob(newer.id)],
        )
        .unwrap();
    let listed = repo.list_for_focus(Focus::Project).unwrap();
    assert_eq!(
        listed.iter().map(|s| s.conversation.id).collect::<Vec<_>>(),
        vec![newer.id, project.id]
    );
    assert_eq!(listed[1].opening, "Hi");
}

#[test]
fn the_v33_to_v34_migration_runs_twice_cleanly() {
    let dir = std::env::temp_dir().join(format!("tod-conversation-mig-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tod.db");
    for _ in 0..2 {
        let conn = schema::open_writer_connection(&path).unwrap();
        let version: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, schema::CURRENT_USER_VERSION);
        for table in [
            "conversations",
            "conversation_turns",
            "conversation_actions",
            "conversation_flags",
        ] {
            let exists: bool = conn
                .prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
                .unwrap()
                .exists([table])
                .unwrap();
            assert!(exists, "{table}");
        }
        conn.pragma_update(None, "user_version", 33).unwrap();
    }
    let conn = schema::open_writer_connection(&path).unwrap();
    let version: i32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, schema::CURRENT_USER_VERSION);
    drop(conn);
    let _ = std::fs::remove_dir_all(dir);
}

/// A report recorded mid-turn belongs to the turn in progress: the agent's
/// own turn is appended only after it, so reading "since the turn started"
/// finds it, and a later turn does not see an earlier one's.
#[test]
fn a_report_is_recorded_against_the_turn_in_progress() {
    let fx = setup();
    let repo = ConversationRepo::new(&fx.conn);
    let first = repo.max_user_seq(fx.conv).unwrap();
    repo.record_report(fx.conv, &serde_json::json!({ "passed": 1 }))
        .unwrap();
    repo.record_report(fx.conv, &serde_json::json!({ "passed": 2 }))
        .unwrap();
    repo.append_turn(fx.conv, TurnRole::Agent, "").unwrap();
    assert_eq!(
        repo.report_since(fx.conv, first).unwrap(),
        Some(serde_json::json!({ "passed": 2 })),
        "the turn's last record replaces its earlier one"
    );

    let second = repo
        .append_turn(fx.conv, TurnRole::Continuation, "Keep going")
        .unwrap()
        .seq;
    assert_eq!(repo.report_since(fx.conv, second).unwrap(), None);
    assert_eq!(
        repo.latest_report(fx.conv).unwrap(),
        Some(serde_json::json!({ "passed": 2 }))
    );
}

/// A turn left waiting when the app stopped is closed with an error turn at
/// the next startup; an answered one is left alone.
#[test]
fn startup_closes_turns_left_waiting_on_the_agent() {
    let fx = setup();
    let repo = ConversationRepo::new(&fx.conn);
    let answered = repo
        .create(Focus::Node(fx.n2), ProtocolKind::Outline, None, None, None)
        .unwrap()
        .id;
    repo.append_turn(answered, TurnRole::User, "Hi").unwrap();
    repo.append_turn(answered, TurnRole::Agent, "").unwrap();

    assert_eq!(repo.close_interrupted_turns("Interrupted").unwrap(), vec![fx.conv]);
    let last = repo.turns(fx.conv).unwrap().pop().unwrap();
    assert_eq!((last.role, last.body.as_str()), (TurnRole::Error, "Interrupted"));
    assert!(repo.close_interrupted_turns("Interrupted").unwrap().is_empty());
}

#[test]
fn snapshot_with_removed_handoff_reason_still_reads() {
    let json = r#"{"entity":"plan_step","node_id":"00000000-0000-0000-0000-000000000001","ordinal":1,"body":"b","status":"blocked","reason":{"kind":"external"},"depends_on":[],"satisfies":[]}"#;
    let snap: EntitySnapshot = serde_json::from_str(json).unwrap();
    assert!(matches!(snap, EntitySnapshot::PlanStep { reason: None, .. }));
}

#[test]
fn a_gate_check_and_an_on_entry_run_name_the_transition_they_belong_to() {
    let fx = setup();
    let repo = ConversationRepo::new(&fx.conn);
    let gate = repo
        .create(Focus::Node(fx.n1), ProtocolKind::GateCheck, None, None, None)
        .unwrap();
    repo.set_transition(gate.id, "verifying", "review").unwrap();
    let entry = repo
        .create(Focus::Node(fx.n1), ProtocolKind::OnEntry, None, None, None)
        .unwrap();
    repo.set_transition(entry.id, "review", "review").unwrap();

    let gate = repo.get(gate.id).unwrap().unwrap();
    assert_eq!(gate.from_state.as_deref(), Some("verifying"));
    assert_eq!(gate.to_state.as_deref(), Some("review"));
    assert_eq!(
        gate.transition_label().as_deref(),
        Some("verifying \u{2192} review")
    );
    assert_eq!(
        repo.get(entry.id).unwrap().unwrap().transition_label().as_deref(),
        Some("on entry to review")
    );
    // Other kinds carry none.
    let plain = repo.get(fx.conv).unwrap().unwrap();
    assert_eq!(plain.transition_label(), None);
}
