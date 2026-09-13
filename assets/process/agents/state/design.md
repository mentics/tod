# State: `design`

**Forward gate:** `design` → `planning`

## On entry

1. Read lifecycle state, resolved obligations (including inherited and design-phase), plan steps (if any), open parked interview memory, and the interview history.
2. If design is complete (or explicitly waived) and gate checklist passes → verify upstream conformance and proceed to exit.

## Responsibilities

### Design interview

Run a **design interview** via the app unless waived at the `proposed` → `design` gate.

- Probe until design-phase information is sufficient; prefer principles/clusters; do not re-ask settled obligations.
- Consume parked interview memory tagged `design`; promote it into a design-phase obligation (`tod-cli obligations add --phase design`) or discuss with the human; mark it done when consumed.
- Probe: a concrete, checkable way to verify each requirement (a command, a test, an observable behavior), costly-to-reverse choices, named **constructions**, open design questions.
- Do **not** probe for optional metadata (Links, non-goals) unless the human volunteers.
- Record waivers in interview memory.

Do not conduct sequential Q&A in the parent session — use question maker + answer-processor invocations.

### Research and spikes

Resolve **design** questions here—not in `planning` or `active`.

- Run spikes in subagents/worktrees when needed.
- Record useful research in durable notes the app can attach to the node or repo.

Deferred spikes need an explicit **decision tree** (outcome → action) recorded in a design-phase obligation or interview memory.

### Visual design

When the node has **user-visible UI**, appearance and layout need human Accept before leaving design (unless waived).

- Hand off to the **visual design** side tool when appropriate.
- Link accepted packages from the relevant design-phase obligation's body (**required** vs **guideline**).

### Design obligations

Produce, update, or **deliberately omit** design-phase obligations on the node (`tod-cli obligations add --kind requirement|constraint`, phase is set automatically from the session):

- Omit when requirements-phase obligations + plan suffice (note the waiver in interview memory).
- One decision per obligation: intention and constructions in the body; external references labeled **required** vs **guideline**; never restate a requirements-phase obligation that already covers it — narrow or add detail instead, and edit an existing obligation in place when a decision changes it rather than duplicating it.

**Implementation interview belongs in `planning`, not here.**

### Reconcile

Before exit, reconcile requirements-phase and design-phase obligations for consistency.

## Forward gate rules (`design` → `planning`)

Apply these prose rules in addition to checklist criteria the app sends for this transition:

- Node obligations include measurable requirements (statement and/or non-redundant success criteria); constraints are measurable and verifiable.
- Design-phase obligations conform to applicable requirements-phase obligations (node + ancestors) and there are **no open design questions**; alternatively design is **explicitly skipped**.
- **Research** for design questions has been done in-phase (and contributed to ancestor obligations where useful).
- Needed **spikes** are complete, **or** any deferred spikes are enumerated with an explicit decision tree (outcome → action).
- Implementation interview belongs in `planning`, not here.
- **Parked items (soft):** review open parked interview memory; nothing left that would be **bad not to cover in design** before leaving (design-parked items must be consumed or explicitly deferred with a decision tree). Items parked for planning may remain.
- **Obligation dedupe (blocking):** Re-check node obligations and any new cross-cutting rules introduced by design-phase obligations against ancestor obligations and sibling nodes (same rules as `proposed` → `design`). Resolve duplicates/conflicts with the human before advancing; elevate when the concern is tree-wide.

Living checklist items for this transition are stored in the app database; return `gate_results` for each when gate-checking.

## Exit

When the `design` → `planning` gate passes (including **obligation dedupe**), return `forward_lifecycle: planning` (app applies).

## Blockers

Unresolved design questions, unenumerated deferred spikes, or upstream conflicts → `blocked` / stay in `design`.
