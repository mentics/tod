# State: `design`

**Forward gate:** `design` → `planning`

## On entry

1. Read lifecycle state, resolved obligations (including inherited), design content, plan content (if any), phase-overflow items, and recent session transcripts.
2. If design is complete (or explicitly waived) and gate checklist passes → verify upstream conformance and proceed to exit.

## Responsibilities

### Design interview

Run a **design interview** via the app unless waived at the `proposed` → `design` gate.

- Probe until design-phase information is sufficient; prefer principles/clusters; do not re-ask settled obligations.
- Consume phase-overflow items tagged `design`; promote into design content or discuss with the human; mark consumed when done.
- Probe: what “done” looks like (commands/checks), irreversible choices, named **constructions**, open design questions.
- Do **not** probe for optional metadata (Links, non-goals) unless the human volunteers.
- Record waivers in session transcripts.

Do not conduct sequential Q&A in the parent session — use question maker + answer-processor invocations.

### Research and spikes

Resolve **design** questions here—not in `planning` or `active`.

- Run spikes in subagents/worktrees when needed.
- Record useful research in durable notes the app can attach to the node or repo.

Deferred spikes need an explicit **decision tree** (outcome → action) in design content or transcript.

### Visual design

When the node has **user-visible UI**, appearance and layout need human Accept before leaving design (unless waived).

- Hand off to the **visual design** side tool when appropriate.
- Link accepted packages from design content (**required** vs **guideline**).

### Design content

Produce, update, or **deliberately omit** design documentation on the node:

- Omit when obligations + plan suffice (note waiver in transcript).
- When present: intention and constructions; external references labeled **required** vs **guideline**; conform to obligations.

**Implementation interview belongs in `planning`, not here.**

### Reconcile

Before exit, reconcile obligations and design content.

## Forward gate rules (`design` → `planning`)

Apply these prose rules in addition to checklist criteria the app sends for this transition:

- Node obligations include measurable requirements (statement and/or non-redundant success criteria); constraints are measurable and verifiable.
- If design extra content exists, it conforms to applicable obligations (node + ancestors) and has **no open design questions**; alternatively design is **explicitly skipped**.
- **Research** for design questions has been done in-phase (and contributed to ancestor obligations where useful).
- Needed **spikes** are complete, **or** any deferred spikes are enumerated with an explicit decision tree (outcome → action).
- Implementation interview belongs in `planning`, not here.
- **Phase overflow (soft):** review open overflow items; nothing left that would be **bad not to cover in design** before leaving (design-shaped items must be consumed or explicitly deferred with a decision tree). Planning-only overflow may remain.
- **Obligation dedupe (blocking):** Re-check node obligations and any new cross-cutting rules introduced in design content against ancestor obligations and sibling nodes (same rules as `proposed` → `design`). Resolve duplicates/conflicts with the human before advancing; elevate when the concern is tree-wide.

Living checklist items for this transition are stored in the app database; return `gate_results` for each when gate-checking.

## Exit

When the `design` → `planning` gate passes (including **obligation dedupe**), return `forward_lifecycle: planning` (app applies).

## Blockers

Unresolved design questions, unenumerated deferred spikes, or upstream conflicts → `blocked` / stay in `design`.
