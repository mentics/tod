# State: `planning`

**Gate:** `planning` → `ready`. The app checks it: every requirement is traced to a plan step, and the phase is certified. The certificate covers the node's own obligations and its plan steps with their obligation links; any change to them makes it stale.

## Your work

1. First, regenerate the node's summary from its details and its now-settled design with the `content` noun (`summary` type, overwrite).
2. Write the plan yourself with the `plan` noun: a dependency graph of plan steps, not a flat document. Most of a plan follows mechanically from settled obligations; write everything you can determine.
   - Each step is a discrete, well-scoped unit of work, named after the constructions design decided, where any exist.
   - Express ordering only where it is real (`depends-on`). Independent steps get no dependency between them, so they can run in parallel.
   - Link each step to the requirement(s) it delivers (`satisfies`). Its `verified` status later stands in for the check of that link.
   - List assumptions explicitly: as plan steps when they are work, as obligations when they are requirements.
3. Resolve planning and implementation unknowns here. Spikes are complete, or deferred with decision trees, before the phase is done.
4. Drain parked items: promote every open parked interview memory item to a plan step or obligation, or discard it.
5. Ask the user only for a decision you genuinely cannot make yourself: a tradeoff with no right answer the obligations and code settle, or a gap design left open that turns on what the user wants. Offer options with your recommendation first.

## Done when

- [ ] Every requirement maps to at least one plan step that satisfies it.
- [ ] The step graph is **actionable**: each step is well-scoped and buildable from; dependencies are well-formed, with no step blocked on one that can never complete and no artificial chain where steps could run in parallel.
- [ ] The plan conforms to the design-phase and requirements-phase obligations, the node's and its ancestors'.
- [ ] **Constraints, both directions:** the plan does nothing a constraint forbids, and does everything one requires.
- [ ] Assumptions are listed; spikes are complete or deferred with decision trees.
- [ ] No open parked item remains on the node.
- [ ] Obligations and plan are reconciled: nothing the plan does contradicts an obligation, and nothing an obligation asks for is missing from the plan.

There is no `ready` state agent: once the gate passes, the app starts `active` as soon as the `ready` → `active` gate does.
