# State: `planning`

**Forward gate:** `planning` → `ready`

## On entry

1. Read lifecycle state, obligations (including design-phase, if any), existing plan steps (`tod-cli plan list`), open parked interview memory, and the interview history.
2. Regenerate the node's summary from its details and now-settled design (overwriting the requirements-only version written on entering `design`): `tod-cli content set --node <UUID> --type summary --body <TEXT>` (overwrite, not append).
3. Draft the plan yourself, directly with `tod-cli plan` (see **Plan steps** below) — before considering whether to open an interview. Most of a plan follows mechanically from settled design obligations; generate everything you can determine on your own right now, on this transition, rather than leaving it for interview turns to build up piecemeal.
4. If plan steps exist and cover everything you can determine, and gate criteria pass → verify conformance and traceability, then proceed to exit.

## Responsibilities

### Implementation interview

The interview is not a required step — it exists only for decisions you genuinely cannot make yourself. After writing the plan, open an **implementation interview** via the app only if real questions remain: a tradeoff with no obvious right answer, a gap design left open, or a parked item that needs the user's judgment to resolve. If nothing meets that bar, do not open one — proceeding straight to the gate with a self-drafted, fully-traceable plan is the normal, unremarkable path, not something to record as a waiver.

When an interview does run, it should probe only the genuine unknowns:

- Order/slicing decisions you couldn't resolve yourself.
- Assumptions you can't accept on your own — accept or convert to requirements.

Prefer questions with proposals when constructions are clear; do not re-ask settled obligations.

**Drain parked items** — promote or discard every open parked interview memory item, whatever phase it was parked for; promoting one usually means turning it into a plan step (or an obligation, when it turns out to be a requirement); none may remain blocking the `planning` → `ready` gate. Promote directly yourself where you can; only route to the interview the ones that need the user's call.

### Research and spikes

Resolve **planning/implementation** unknowns here. Spikes belong in `design` or `planning` only—complete or defer with decision trees before exit.

### Plan steps (required)

Write or refine the node's plan yourself as a dependency graph of **plan steps** (`tod-cli plan`), not a flat document — do this directly on entry, not by waiting on an interview:

- Each step is a discrete, well-scoped unit of work (`plan add --body`), named after the constructions decided by design-phase obligations, where any exist.
- Express ordering only where real: link a step to what must land first with `--depends-on`/`plan depend`. Independent steps get no dependency between them — this is how work is dispatched in parallel across agents/worktrees, so don't force a linear chain onto work that isn't actually sequential.
- Traceability: link each step to the requirement(s) it delivers via `--satisfies`/`plan satisfy`, and drive it to `verified` as the check for that link.
- Conforms to design-phase and requirements-phase obligations.
- List assumptions explicitly — as their own plan steps when they represent work, or via interview memory/obligations when they don't, following the rest of this doc's conventions.

### Human look-over

In **interactive** mode, give the human opportunity to review before `ready`. **Autonomous** mode waives that look-over when other gate criteria pass.

### Reconcile

Reconcile obligations (requirements- and design-phase) and plan before exit.

## Forward gate rules (`planning` → `ready`)

Apply these prose rules in addition to checklist criteria the app sends for this transition:

- Implementation interview done, or not needed (nothing left that required the user's judgment) — either is a pass; only an interview left genuinely incomplete blocks the gate. Trust the question maker's own `interview exhausted` declaration (check current status via `tod-cli`) as strong evidence of completeness — it applies this same bar. Fail this criterion only when you can point to a specific, concrete decision it missed; a general sense that more discussion is possible is not grounds to fail a completed interview.
- Plan steps exist and the graph is **actionable**: each step is well-scoped and buildable from, and dependencies (`plan depend`) are well-formed — no step blocked on one that can never complete, no artificial chain where steps could run in parallel instead.
- Plan steps conform to design-phase and applicable requirements-phase obligations (node + ancestors).
- **Constraints, both directions** — check this node's constraints and every inherited one (listed under Inherited context). Answer two questions; the criterion passes only if both are yes:
  1. Is the plan free of anything a constraint forbids? (Many constraints say what must *not* be done.)
  2. Does the plan do everything a constraint requires?
  If either is no, set that criterion's `gate_results` row to `outcome: fail` (the reply is then `result: blocked`) and, in the row's `detail`, name each constraint and what in the plan breaks or misses it.
- Requirements are **traceable**: each maps to one or more plan steps via `--satisfies`, and those steps' `verified` status stands in for their verifiable checks (success criteria when present, otherwise the measurable statement).
- **Research** for planning/implementation questions done in-phase (ancestor obligations updated where useful).
- Needed **spikes** complete, **or** deferred spikes enumerated with decision trees.
- **Parked items (hard):** all open parked interview memory fully processed (promoted into obligations/extra content or explicitly discarded with the human). No open parked item may remain on the node.
- **Human look-over:** interactive mode requires opportunity; autonomous mode waives it when the rest of the gate passes.

Living checklist items for this transition are stored in the app database; return `gate_results` for each when gate-checking.

## Exit

When the `planning` → `ready` gate passes, return `forward_lifecycle: ready`. There is **no `ready` state agent** — the app or human starts `active` via the `ready` → `active` gate.

## Blockers

Missing traceability (a requirement with no `--satisfies`-linked step, or a step never reaching `verified`), a malformed or non-actionable step graph, or mid-`active` questions that indicate missing intent → stay in `planning` or `blocked`.
