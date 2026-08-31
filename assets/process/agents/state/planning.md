# State: `planning`

**Forward gate:** `planning` → `ready`

## On entry

1. Read lifecycle state, obligations, design content (if any), existing plan content, phase-overflow items, and transcripts.
2. If plan exists and gate criteria pass → verify conformance and traceability, then proceed to exit.

## Responsibilities

### Implementation interview

Run an **implementation interview** via the app unless explicitly waived (record waiver in transcript).

Probe:

- How the work will be built step-by-step.
- How verification will prove **each requirement**.
- Assumptions—accept or convert to requirements.

Prefer write-from-decision / `proposed_text` when constructions are clear; do not re-ask settled obligations.

**Drain phase overflow** — consume or discard every open item; none may remain blocking the `planning` → `ready` gate.

### Research and spikes

Resolve **planning/implementation** unknowns here. Spikes belong in `design` or `planning` only—complete or defer with decision trees before exit.

### Plan content (required)

Write or refine an **actionable** plan on the node:

- Ordered steps and named constructions from design (if present).
- Traceability: each requirement maps through the plan to verification.
- Conforms to design (if any) and obligations.
- List assumptions explicitly.

### Human look-over

In **interactive** mode, give the human opportunity to review before `ready`. **Autonomous** mode waives that look-over when other gate criteria pass.

### Reconcile

Reconcile obligations, design (if any), and plan before exit.

## Forward gate rules (`planning` → `ready`)

Apply these prose rules in addition to checklist criteria the app sends for this transition:

- Implementation interview done or waived (waiver in transcript).
- Plan extra content exists and is **actionable** (buildable from).
- Plan conforms to design content if present and to applicable obligations (node + ancestors).
- Requirements are **traceable** through the plan to their verifiable checks (success criteria when present, otherwise the measurable statement).
- **Research** for planning/implementation questions done in-phase (ancestor obligations updated where useful).
- Needed **spikes** complete, **or** deferred spikes enumerated with decision trees.
- **Phase overflow (hard):** all open overflow items fully processed (promoted into obligations/extra content or explicitly discarded with the human). No open overflow may remain on the node.
- **Human look-over:** interactive mode requires opportunity; autonomous mode waives it when the rest of the gate passes.

Living checklist items for this transition are stored in the app database; return `gate_results` for each when gate-checking.

## Exit

When the `planning` → `ready` gate passes, return `forward_lifecycle: ready`. There is **no `ready` state agent** — the app or human starts `active` via the `ready` → `active` gate.

## Blockers

Missing traceability, non-actionable plan, or mid-`active` questions that indicate missing intent → stay in `planning` or `blocked`.
