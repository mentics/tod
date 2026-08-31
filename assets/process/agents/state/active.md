# State: `active`

**Forward gate:** `active` → `verifying`

## On entry

1. Read lifecycle state, resolved obligations (including inherited), design content (if any), plan content, and child nodes when relevant.
2. If implementation already satisfies the gate (plan complete, ship-with-code tests in place) → verify upstream conformance and proceed to exit.

## Responsibilities

### Implement

Execute the **plan** honoring design and applicable obligations.

- Stay inside constraints and constructions unless a blocker forces stop.
- Decide local/reversible plan detail; ask for product intent, irreversible API/schema choices, and doc conflicts.
- Update plan as implementation learns; minor plan/design edits need not change lifecycle state. **Major** rethinks → move back to `design` or `planning`.

### Tests that ship

**Automated tests that merge/ship with the code** must be complete before leaving `active`.

### Exercise before claiming complete

When a plan step, requirement, or runnable surface is treated as **done**, the agent must already have exercised it in running context. Do not claim complete and defer first runtime check to `verifying`. Build a harness in `active` when needed.

A full running-context pass is not required after every tiny incremental edit — exercise before each completeness claim for a coherent slice (requirement, surface, plan step that delivers runnable behavior).

### Spikes

If a **spike** is needed, **do not run it in `active`**. Transition back to `design` or `planning`, run the spike there, update docs, re-pass gates.

### Scope

If scope must grow → stop; propose a new child node or explicit obligation change. Do not silently expand.

### Fan-out

May spawn subagents across code areas when separation is clear; parent merges results.

### Maintain node constraints (when the node owns cross-cutting scope)

When child nodes are added or scope evolves:

- Ensure new work conforms to this node's obligations (report conflicts; do not silently override).
- Capture clarifications in transcripts.
- Update obligations only with explicit human permission (app persists).

### Child-node decomposition (optional)

When the human wants to split work under this node, optional task-decomposition interview or **task generator** side tool may propose child nodes. Accept creates child nodes via the app — not full requirements interviews for each child.

## Reconcile

After a coherent change set, reconcile obligations, design, plan, and code before handback.

## Forward gate rules (`active` → `verifying`)

Apply these prose rules (no DB checklist items for this transition):

- Implementation is **verified complete** against plan content, design content (if any), and applicable obligations (agent checked—not merely claimed).
- Automated tests that **ship/merge with the code** are complete and included.
- Runnable surfaces / requirements treated as complete in `active` were **exercised in running context** by the agent — not left for first exercise in `verifying`.
- Extra local-only harnesses, one-off checks, and the full requirement sweep may still run in `verifying`.

## Exit

When implementation is verified complete against plan, design, and obligations, ship-with-code tests are done, and runnable slices claimed complete were exercised in context, return `forward_lifecycle: verifying`.

## Blockers

Requirement gap, needed spike, or unresolvable conflict → paused/blocked; capture new user info in transcripts when provided.
