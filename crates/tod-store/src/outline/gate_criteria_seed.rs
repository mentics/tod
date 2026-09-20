//! Canonical gate criteria seeded on migration. Checklist labels only — prose rules
//! live in state agent role docs (`assets/process/agents/state/{state}.md`).

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::outline::uuid_blob::{now_ms, uuid_to_blob};

/// One checklist row for a forward lifecycle transition.
#[derive(Debug, Clone)]
pub struct GateCriterionSeed {
    pub id_str: &'static str,
    pub from_state: &'static str,
    pub to_state: &'static str,
    pub slug: &'static str,
    pub label: &'static str,
    pub sort_order: i32,
}

/// Stable criterion catalog (33 items across design→planning, planning→ready, verifying→review).
pub const GATE_CRITERIA: &[GateCriterionSeed] = &[
    // design → planning
    GateCriterionSeed {
        id_str: "a1000001-0001-4001-8001-00000000000c",
        from_state: "design",
        to_state: "planning",
        slug: crate::outline::BUILDABLE_CRITERION_SLUG,
        label: "Buildable: would a competent implementer given the context, obligations, referenced nodes, mockups, and codebase build it correctly?",
        sort_order: 0,
    },
    // Superseded by `buildable`; deactivated in `seed_gate_criteria`.
    GateCriterionSeed {
        id_str: "a1000001-0001-4001-8001-000000000001",
        from_state: "design",
        to_state: "planning",
        slug: "design-planning.done-criteria-clear",
        label: "Does each requirement have a concrete, checkable way to verify it's done (a command, a test, or an observable behavior)?",
        sort_order: 1,
    },
    GateCriterionSeed {
        id_str: "a1000001-0001-4001-8001-000000000002",
        from_state: "design",
        to_state: "planning",
        slug: "design-planning.constraints-known",
        label: "Do I know what I must not change (constraints)?",
        sort_order: 2,
    },
    GateCriterionSeed {
        id_str: "a1000001-0001-4001-8001-000000000003",
        from_state: "design",
        to_state: "planning",
        slug: "design-planning.irreversible-choices-locked",
        label: "Are costly-to-reverse design choices (data shapes, protocols, schemas, public interfaces) locked, or explicitly deferred with a decision tree?",
        sort_order: 3,
    },
    GateCriterionSeed {
        id_str: "a1000001-0001-4001-8001-000000000004",
        from_state: "design",
        to_state: "planning",
        slug: "design-planning.constructions-by-name",
        label: "If design-phase obligations exist, do constructions appear by name (not slogans), conforming to applicable requirements-phase obligations?",
        sort_order: 4,
    },
    GateCriterionSeed {
        id_str: "a1000001-0001-4001-8001-000000000005",
        from_state: "design",
        to_state: "planning",
        slug: "design-planning.design-questions-resolved",
        label: "Are open design questions all resolved (none left)?",
        sort_order: 5,
    },
    GateCriterionSeed {
        id_str: "a1000001-0001-4001-8001-000000000006",
        from_state: "design",
        to_state: "planning",
        slug: "design-planning.spikes-complete-or-deferred",
        label: "Needed spikes complete, or deferred spikes listed with outcome → action trees?",
        sort_order: 6,
    },
    GateCriterionSeed {
        id_str: "a1000001-0001-4001-8001-000000000007",
        from_state: "design",
        to_state: "planning",
        slug: "design-planning.third-party-libs-confirmed",
        label: "Named third-party libraries in constructions: confirmed they provide each required capability—not only a related adjacent feature?",
        sort_order: 7,
    },
    GateCriterionSeed {
        id_str: "a1000001-0001-4001-8001-000000000008",
        from_state: "design",
        to_state: "planning",
        slug: "design-planning.obligations-design-reconcile",
        label: "Reconciliation: requirements-phase and design-phase obligations (if any) agree?",
        sort_order: 8,
    },
    GateCriterionSeed {
        id_str: "a1000001-0001-4001-8001-000000000009",
        from_state: "design",
        to_state: "planning",
        slug: "design-planning.phase-overflow-soft-review",
        label: "Soft phase-overflow review: no open item that would be bad to leave uncovered until after design?",
        sort_order: 9,
    },
    GateCriterionSeed {
        id_str: "a1000001-0001-4001-8001-00000000000a",
        from_state: "design",
        to_state: "planning",
        slug: "design-planning.visual-packages-accepted-or-waived",
        label: "If the node has user-visible UI: needed visual packages accepted and linked from a design-phase obligation, or visual design explicitly waived in transcript?",
        sort_order: 10,
    },
    GateCriterionSeed {
        id_str: "a1000001-0001-4001-8001-00000000000b",
        from_state: "design",
        to_state: "planning",
        slug: "design-planning.obligation-dedupe",
        label: "Obligation dedupe: no unresolved near-duplicates vs ancestors / siblings?",
        sort_order: 11,
    },
    // planning → ready
    GateCriterionSeed {
        id_str: "a1000002-0002-4002-8002-000000000001",
        from_state: "planning",
        to_state: "ready",
        slug: "planning-ready.implementation-interview-done-or-waived",
        label: "Implementation interview done or explicitly waived?",
        sort_order: 1,
    },
    GateCriterionSeed {
        id_str: "a1000002-0002-4002-8002-000000000002",
        from_state: "planning",
        to_state: "ready",
        slug: "planning-ready.obligations-measurable-approved",
        label: "Node obligations human-approved; every requirement measurable (statement and/or non-redundant success criteria)?",
        sort_order: 2,
    },
    GateCriterionSeed {
        id_str: "a1000002-0002-4002-8002-000000000003",
        from_state: "planning",
        to_state: "ready",
        slug: "planning-ready.plan-actionable",
        label: "Plan actionable (buildable from) without inventing missing intent?",
        sort_order: 3,
    },
    GateCriterionSeed {
        id_str: "a1000002-0002-4002-8002-000000000004",
        from_state: "planning",
        to_state: "ready",
        slug: "planning-ready.plan-conforms",
        label: "Plan conforms to design-phase obligations (if any) and applicable requirements-phase obligations?",
        sort_order: 4,
    },
    GateCriterionSeed {
        id_str: "a1000002-0002-4002-8002-000000000005",
        from_state: "planning",
        to_state: "ready",
        slug: "planning-ready.requirements-traceable",
        label: "Each requirement traceable through the plan to its verifiable check?",
        sort_order: 5,
    },
    GateCriterionSeed {
        id_str: "a1000002-0002-4002-8002-000000000006",
        from_state: "planning",
        to_state: "ready",
        slug: "planning-ready.constructions-in-plan",
        label: "Design-mandated constructions named in the plan (not slogans)?",
        sort_order: 6,
    },
    GateCriterionSeed {
        id_str: "a1000002-0002-4002-8002-000000000007",
        from_state: "planning",
        to_state: "ready",
        slug: "planning-ready.assumptions-listed",
        label: "Assumptions listed and accepted (or converted to requirements)?",
        sort_order: 7,
    },
    GateCriterionSeed {
        id_str: "a1000002-0002-4002-8002-000000000008",
        from_state: "planning",
        to_state: "ready",
        slug: "planning-ready.spikes-complete-or-deferred",
        label: "Needed spikes complete, or deferred with decision trees?",
        sort_order: 8,
    },
    GateCriterionSeed {
        id_str: "a1000002-0002-4002-8002-000000000009",
        from_state: "planning",
        to_state: "ready",
        slug: "planning-ready.reconciliation-after-planning",
        label: "Reconciliation after planning edits (obligations and phase content consistent)?",
        sort_order: 9,
    },
    GateCriterionSeed {
        id_str: "a1000002-0002-4002-8002-00000000000a",
        from_state: "planning",
        to_state: "ready",
        slug: "planning-ready.phase-overflow-drained",
        label: "Hard phase-overflow drain: every item processed; none open?",
        sort_order: 10,
    },
    GateCriterionSeed {
        id_str: "a1000002-0002-4002-8002-00000000000c",
        from_state: "planning",
        to_state: "ready",
        slug: "planning-ready.no-missing-intent-for-active",
        label: "Would a mid-active question only arise from a bug/code surprise—not from missing intent?",
        sort_order: 11,
    },
    // ready → active — evaluated by the app, not an agent (`tod_core::gate::derived`)
    GateCriterionSeed {
        id_str: "a1000005-0005-4005-8005-000000000001",
        from_state: "ready",
        to_state: "active",
        slug: crate::outline::repos::gate::READY_ACTIVE_ACTION_CONFIG_SLUG,
        label: "Does the node have Agent and Files configured (with a ready directory) to do the implementation work?",
        sort_order: 1,
    },
    // active → verifying — evaluated by the app, not an agent (`tod_core::gate::derived`)
    GateCriterionSeed {
        id_str: "a1000007-0007-4007-8007-000000000001",
        from_state: "active",
        to_state: "verifying",
        slug: crate::outline::repos::gate::ACTIVE_VERIFYING_PLAN_IMPLEMENTED_SLUG,
        label: "Is every plan step implemented (none still open, failed, partial, or blocked)?",
        sort_order: 1,
    },
    // verifying → review
    GateCriterionSeed {
        id_str: "a1000003-0003-4003-8003-00000000000a",
        from_state: "verifying",
        to_state: "review",
        slug: crate::outline::repos::gate::VERIFYING_REVIEW_PLAN_VERIFIED_SLUG,
        label: "Every plan step verified (none failed verification or still unchecked)?",
        sort_order: 0,
    },
    GateCriterionSeed {
        id_str: "a1000003-0003-4003-8003-000000000001",
        from_state: "verifying",
        to_state: "review",
        slug: "verifying-review.every-requirement-checked",
        label: "Every node requirement checked and passed (statement and/or success criteria; evidence recorded)?",
        sort_order: 1,
    },
    GateCriterionSeed {
        id_str: "a1000003-0003-4003-8003-000000000002",
        from_state: "verifying",
        to_state: "review",
        slug: "verifying-review.exercised-in-running-context",
        label: "Agent exercised the work in its running context (automated E2E and/or agent-driven); harness if the environment required it?",
        sort_order: 2,
    },
    GateCriterionSeed {
        id_str: "a1000003-0003-4003-8003-000000000003",
        from_state: "verifying",
        to_state: "review",
        slug: "verifying-review.constraints-hold",
        label: "Applicable constraints still hold (including inherited)?",
        sort_order: 3,
    },
    GateCriterionSeed {
        id_str: "a1000003-0003-4003-8003-000000000004",
        from_state: "verifying",
        to_state: "review",
        slug: "verifying-review.results-traceable",
        label: "Verification results traceable to plan / design (if any) / applicable obligations?",
        sort_order: 4,
    },
    GateCriterionSeed {
        id_str: "a1000003-0003-4003-8003-000000000005",
        from_state: "verifying",
        to_state: "review",
        slug: "verifying-review.upstream-conformance-revalidated",
        label: "Upstream conformance revalidated (or unchanged-file short-circuit applied correctly)?",
        sort_order: 5,
    },
    GateCriterionSeed {
        id_str: "a1000003-0003-4003-8003-000000000006",
        from_state: "verifying",
        to_state: "review",
        slug: "verifying-review.self-code-review-done",
        label: "Self-code review completed?",
        sort_order: 6,
    },
    GateCriterionSeed {
        id_str: "a1000003-0003-4003-8003-000000000007",
        from_state: "verifying",
        to_state: "review",
        slug: "verifying-review.functionally-ready",
        label: "Functionally ready: requirements satisfied, no known bugs (nits OK)?",
        sort_order: 7,
    },
    GateCriterionSeed {
        id_str: "a1000003-0003-4003-8003-000000000008",
        from_state: "verifying",
        to_state: "review",
        slug: "verifying-review.not-deferring-to-review",
        label: "Not relying on upcoming review or human look-over to find functional defects?",
        sort_order: 8,
    },
    GateCriterionSeed {
        id_str: "a1000003-0003-4003-8003-000000000009",
        from_state: "verifying",
        to_state: "review",
        slug: "verifying-review.gate-extras-done",
        label: "Node- or ancestor-defined gate extras (if any) done?",
        sort_order: 9,
    },
    // review → approved — evaluated by the app, not an agent (`tod_core::gate::derived`).
    // Approval itself is the user's Advance once both pass.
    GateCriterionSeed {
        id_str: "a1000006-0006-4006-8006-000000000001",
        from_state: "review",
        to_state: "approved",
        slug: crate::outline::repos::gate::REVIEW_APPROVED_REVIEW_DONE_SLUG,
        label: "Independent code review recorded as finished?",
        sort_order: 1,
    },
    GateCriterionSeed {
        id_str: "a1000006-0006-4006-8006-000000000002",
        from_state: "review",
        to_state: "approved",
        slug: crate::outline::repos::gate::REVIEW_APPROVED_FINDINGS_ANSWERED_SLUG,
        label: "Every review finding answered (fixed, out of scope, declined, or rejected)?",
        sort_order: 2,
    },
];

/// SQL for gate criteria tables (also appended to outline DDL).
pub const GATE_CRITERIA_DDL: &str = r#"
CREATE TABLE IF NOT EXISTS gate_criteria (
    id          BLOB PRIMARY KEY NOT NULL,
    from_state  TEXT NOT NULL,
    to_state    TEXT NOT NULL,
    slug        TEXT NOT NULL UNIQUE,
    label       TEXT NOT NULL,
    sort_order  INTEGER NOT NULL,
    active      INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_gate_criteria_transition
    ON gate_criteria(from_state, to_state, sort_order);

CREATE TABLE IF NOT EXISTS node_gate_evaluations (
    node_id         BLOB NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    criterion_id    BLOB NOT NULL REFERENCES gate_criteria(id) ON DELETE CASCADE,
    outcome         TEXT NOT NULL CHECK (outcome IN ('pass', 'fail', 'pending', 'waived')),
    detail          TEXT,
    source          TEXT NOT NULL CHECK (source IN ('agent', 'human', 'derived')),
    evaluated_at    INTEGER NOT NULL,
    action          TEXT NOT NULL DEFAULT 'none' CHECK (action IN ('none', 'interview')),
    PRIMARY KEY (node_id, criterion_id)
);
CREATE INDEX IF NOT EXISTS idx_node_gate_evaluations_node
    ON node_gate_evaluations(node_id, evaluated_at);
"#;

/// Insert canonical criteria; idempotent (`slug` is unique).
pub fn seed_gate_criteria(conn: &Connection) -> Result<()> {
    let now = now_ms();
    for row in GATE_CRITERIA {
        let id = Uuid::parse_str(row.id_str).context("invalid gate criterion seed id")?;
        conn.execute(
            "INSERT OR IGNORE INTO gate_criteria
                (id, from_state, to_state, slug, label, sort_order, active, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?7)",
            params![
                uuid_to_blob(id),
                row.from_state,
                row.to_state,
                row.slug,
                row.label,
                row.sort_order,
                now,
            ],
        )?;
        // `INSERT OR IGNORE` leaves an already-seeded row's `label` alone —
        // keep wording edits to this catalog in sync on existing installs
        // too, rather than only affecting brand-new databases.
        conn.execute(
            "UPDATE gate_criteria SET label = ?2, updated_at = ?3
             WHERE id = ?1 AND label != ?2",
            params![uuid_to_blob(id), row.label, now],
        )?;
    }
    conn.execute(
        "UPDATE gate_criteria SET active = 0, updated_at = ?1 WHERE slug = 'planning-ready.human-lookover' AND active = 1",
        params![now],
    )?;
    // design → planning requires `buildable` only.
    conn.execute(
        "UPDATE gate_criteria SET active = 0, updated_at = ?1
         WHERE from_state = 'design' AND to_state = 'planning' AND slug != ?2 AND active = 1",
        params![now, crate::outline::BUILDABLE_CRITERION_SLUG],
    )?;
    Ok(())
}
