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

## Not enforced yet

Proposed (requirements-introduced) obligations are not required to act in
`design`. Design obligations do not yet link to the proposed one they refine,
so enforcing it would drop those obligations out of tracing. Enforce it once
that link exists.

Set with `tod-cli obligations add|update --acts-in`; see
`cli/obligations.md`.
