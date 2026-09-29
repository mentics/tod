# Plan steps by phase, follow-up nodes, and ticket links

Status: proposed.

## Problem

A plan step is work an agent does. Today every step is implementation work:
the Implement loop runs until every step is `implemented`. A node whose
work includes something that can only happen after release (a prod
backfill, a check against prod data) has no place to put it. The planner
put it in the plan, the step blocked on `access` ("merge and deploy
first"), and the node could not leave `active`, the state that has to
finish before any merge.

## 1. A plan step is an action, owned by a phase

Every plan step has a **phase**: the lifecycle state whose agent does it.

| Phase | Example |
|---|---|
| `active` (default) | Write the code, the migration, the backfill asset |
| `verifying` | An action verification needs that is more than checking: seed a fixture environment, record a benchmark |
| `merged` | Part of releasing: apply a migration that must precede the deploy, set a prod config value or secret, turn on a feature flag |
| `released` | Needs the release in place: run the backfill in prod, refresh derived data |

The other states (`proposed` … `ready`, `pr`, `review`, `approved`, `learn`)
hold no steps: their work is about the node, not an action in the plan.

**A step is an action, never a check.** "Verify X" is not a step: checking
that the node's steps were done and its obligations hold is what
`verifying` and `released` exist for. A step's own check is its
`verified` status.

**Status is unchanged:** `pending` → `implemented` (the action was taken)
→ `verified` (someone confirmed it took effect), with `failed`, `partial`,
and `blocked` as today. `active` steps are verified by the separate
`verifying` phase, since implementation changes a lot at once. A step of
any later phase is verified by the phase that owns it: `released` runs the
backfill, then checks its report.

**Rules the app enforces:**

- A step may depend only on steps of its own phase or an earlier one.
  `plan add` / `plan depend` refuse the reverse.
- Each gate and loop counts only steps of the phases up to its own:
  - `active-verifying.plan-steps-implemented` and the Implement loop: `active` steps.
  - `verifying-review.plan-steps-verified` and the Verify loop: `active` and `verifying` steps.
  - `merged` → `released`: `merged` steps verified, as well as the certificate.
  - `released` → `learn`: `released` steps verified, as well as the certificate.
- `plan ready` lists only steps of the node's current phase and earlier.
- `lifecycle_validity`: the "step is open / not verified" findings count
  only steps due by the node's state. Changing a step's phase after
  `ready` counts as a plan change, like rewording it (back to `planning`).
- The planning trace gate is unchanged: an obligation satisfied only by a
  `released` step is traced.

**Data:** `node_plan_steps.phase` (default `active`), part of the planning
certificate's digest. `tod-cli plan add|update … --phase <state>`, shown
in `plan list` / `plan show`, and as a `phase:` line on the step's row in the plan side pane.

## 2. Acting without the user

The goal is that the only things the user must do are review and merge the
PR, and cut the release. A `merged` or `released` step that writes to a
shared environment (prod, staging) runs **without asking** when the agent
judges it safe: small, bounded, no meaningful load, and reversible or
idempotent. It asks through `decisions` (with the timing as an option) when
it is not: heavy load, broad or irreversible, or needs a particular time.
The agent judges this when it runs the step, from a dry run where one
exists, not when it plans the step. The codebase's own rules win: if they
say prod commands need confirmation, the agent asks.

Out of scope for now: noticing that a node was released (polling the
release pipeline or tags) and moving it from `merged` to `released` without
the user.

## 3. Follow-up nodes and found work

Each releasable chunk is its own node. Two kinds of work the agent finds
belong on a **new node**, not in this plan:

- **Follow-up**: the next chunk, which needs this one released first.
- **Found work**: an out-of-scope bug or opportunity seen while working.

Either can turn up in any phase. The agent asks through `decisions`:
create a ticket for it (recommended), or leave it. On yes it:

1. Creates the issue in the tracker directly (for Linear, with the stored
   `linear_api_key` through `secrets run`). tod does not wrap the tracker's API.
2. Creates the node next to this one, with that ticket as its Ticket.
3. Adds a note on this node naming the new ticket (`node notes`). There is
   no formal relation; formalize it if it proves needed.

Then it carries on with this node's work. This node's plan and obligations
do not change, so its state still holds. Starting the new node's lifecycle
stays the user's call.

## 4. Ticket links are found by id, not stored

Today a node is tied to a generated ticket by a `managed_node_links` row
(`node_id` primary key, `generator_node_id`, `external_id`, `source_type`,
`user_modified_fields`), which only accepting or pasting creates. A node
made any other way, with the same ticket in its Ticket capability, is not
linked, and a node can be tied to only one generator.

**Rule:** a node has at most one ticket, its Ticket capability's, and a
node **is** ticket T when that ticket is T. References to other tickets go
in notes. The capability's ticket becomes a single value
(`node_fields.linked_issues` → `node_fields.ticket`; a node with several
today keeps the first, the rest move to a note). Every link is then found by
search:

- **Already accepted** (the generator's marker): some non-managed node is
  T. The tree already computes this keyed by id alone.
- **Refresh pushes changes**: to every non-managed node that is T, whichever
  generator returned it; generators returning the same ticket push the same
  data.
- **A managed node's own ticket**: its Ticket, as accept and
  `linear_import` already set on copies. Its generator is its nearest
  generator ancestor, and `source_type` comes from that generator's config.

What still has to be stored is per node: `user_modified_fields` (the fields
a refresh must not overwrite) moves to `node_fields`. `managed_node_links`
is then removed, along with its entries in journey triggers, sync, and
archive.

Many-to-many falls out: any number of generators and any number of nodes
may share a ticket.

**Performance:** `node_fields.ticket` is indexed; `generator_bench`
checks the lookup.

## Order

1. Plan-step phase: schema, `tod-cli plan`, gates, loops, validity, side-pane row.
2. Docs: `planning.md` and its evaluator (steps by phase, actions only,
   follow-ups), `verifying.md` / `merged.md` / `released.md` (work their own
   steps, the safety judgment), `cli/plan.md`.
3. Ticket links by id (section 4).
4. Follow-up nodes and found work: docs only, once 3 lands.
