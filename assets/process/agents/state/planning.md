# State: `planning`

**Gate:** `planning` → `ready`. The app checks it: every requirement that acts in `active`, `merged`, or `released` is traced to a plan step, and the phase is certified. The certificate covers the node's own obligations and its plan steps with their obligation links; any change to them makes it stale.

## Your work

1. First, regenerate the node's summary from its details and its now-settled design with the `content` noun (`summary` type, overwrite).
2. Write the plan yourself with the `plan` noun: a dependency graph of plan steps, not a flat document. Most of a plan follows mechanically from settled obligations; write everything you can determine.
   - Each step is a discrete, well-scoped unit of work, named after the constructions design decided, where any exist.
   - Each step is an **action**, never a check: checking that the steps were done and the obligations hold is what `verifying` and `released` are for, so "verify X" is not a step.
   - Give each step the **phase** that can actually take it (`plan` noun, `--phase`). Most are `active`: implementation. An action only a later phase can take goes to that phase: `merged` for part of the release (a migration that must precede the deploy, a production setting, a feature flag), `released` for one that needs the release in place (a backfill against production data), `verifying` for an action verification needs beyond checking. Nothing in `active` may need the merge, a deploy, or production: implementation has to finish before any of those happen. An obligation that only a later phase's step delivers is verified in that phase, and the obligation says which phase that is (where it acts, `merged` or `released`): give it a step of that phase. An obligation that acts in `verifying` or `design` needs no step.
   - Express ordering only where it is real (`depends-on`). Independent steps get no dependency between them, so they can run in parallel.
   - Link each step to the requirement(s) it delivers (`satisfies`). Its `verified` status later stands in for the check of that link.
   - List assumptions explicitly: as plan steps when they are work, as obligations when they are requirements.
3. Resolve planning and implementation unknowns here. Spikes are complete, or deferred with decision trees, before the phase is done.
4. Drain parked items: promote every open parked interview memory item to a plan step or obligation, or discard it.
5. Work that belongs to a different releasable chunk (a follow-up that needs this one released first, or an out-of-scope bug or opportunity you found) does not go in this plan. Offer it as a new node: see "Follow-up work" in the base doc.
6. Ask the user only for a decision you genuinely cannot make yourself: a tradeoff with no right answer the obligations and code settle, or a gap design left open that turns on what the user wants. Offer options with your recommendation first.

## Done when

- [ ] Every requirement maps to at least one plan step that satisfies it.
- [ ] The step graph is **actionable**: each step is well-scoped and buildable from; dependencies are well-formed, with no step blocked on one that can never complete and no artificial chain where steps could run in parallel.
- [ ] Every step is an action, in the phase that can take it: no `active` step needs the merge, a deploy, or production, and no step is only a check.
- [ ] The plan conforms to the design-phase and requirements-phase obligations, the node's and its ancestors'.
- [ ] **Constraints, both directions:** the plan does nothing a constraint forbids, and does everything one requires.
- [ ] Assumptions are listed; spikes are complete or deferred with decision trees.
- [ ] No open parked item remains on the node.
- [ ] Obligations and plan are reconciled: nothing the plan does contradicts an obligation, and nothing an obligation asks for is missing from the plan.

There is no `ready` state agent: once the gate passes, the app starts `active` as soon as the `ready` → `active` gate does.
