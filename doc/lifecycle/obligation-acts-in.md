# Obligations: introduced in, acts in

An obligation carries two lifecycle phases that answer different questions.

| Field | Values | Question |
|---|---|---|
| `phase` | `requirements`, `design`, `unknown` | When was it introduced? Fixed at creation: the proposed-phase agent writes `requirements`, the design-phase agent (and later agents) write `design`. |
| `acts_in` (schema v79) | `design`, `active`, `verifying`, `merged`, `released` | Whose work is it? The lifecycle state whose agent acts on it. Default `active`. |

`acts_in` mirrors the plan-step `phase` (`plan-step-phases.md`) and uses the
same ordering.

## What reads `acts_in`

- `requirements-traceable` (planning → ready) asks a plan step only of
  requirements that need one (`NodeObligation::needs_plan_step`): not
  `design` (settled by the design work) and not `verifying` (only checked).
- `obligations-verified` and the phase-scoped `plan-verified` gates use
  `ObligationStanding.phase`, the later of `acts_in` and the phase of the
  steps that carry it. A `design`-acting obligation is never due by step.
- The lifecycle baseline and phase certificates include `acts_in`, so moving
  an obligation stales them.
- The change set records and reverses `acts_in` (`UpdateObligationActsIn`).

## Where an obligation acts: the state that can deliver it

`acts_in` is where the obligation can be **shown true**, not what it is about.
One that holds only after the merge, a deploy, or a run on production ("after
rollout, a query over `integration_status` shows...", "after the cleanup runs
on prod, a query finds zero...") acts in `released` (or `merged` for what must
precede the deploy), with a plan step of that phase that does the action.
Left `active` or `design`, verification cannot exercise it and used to ask the
user (2026-11-12: two nodes). Enforced by the role docs' Done-when checklists
(`proposed`, `design`, `planning`) and the independent evaluator's rejection
rule (`surface/evaluate.md`), not by a keyword check: wording does not decide
it, so a deterministic test would be wrong both ways.

**The verifier's rule.** From `ready` on, the app refuses an agent's
obligation edits (`conversation::settled`), with one exception: a verification
conversation may move an obligation of its node, while the node is in
`verifying`, to `merged` or `released` from an earlier state
(`settled::verify_may_retag`). It is recorded in the change set, and moving it
does not send the node back (`lifecycle_validity::moved_to_later_phase`). The
Verify loop then owes a trace: a requirement acting in `merged`/`released`
that no plan step satisfies keeps the loop going until the agent adds one
(`verify::untraced_for_later_phase`). The agent never `decisions ask`s about a
phase mistake.

## Not enforced yet

Proposed (requirements-introduced) obligations are not required to act in
`design`. Design obligations do not yet link to the proposed one they refine,
so enforcing it would drop those obligations out of tracing. Enforce it once
that link exists.

Set with `tod-cli obligations add|update --acts-in`; see
`cli/obligations.md`.
