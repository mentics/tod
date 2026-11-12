# State: `design`

**Gate:** `design` → `planning`. The app checks it: the phase is certified. The certificate covers the task's own obligations, its design content, and its mockups; any change to them makes it stale.

## Your work

1. First, regenerate the task's summary from its details and its settled requirements with the `content` noun (`summary` type, overwrite). It is all this task's descendants see of its details and requirements.
2. Write the spec: the smallest set of obligations that gets the task built correctly. An obligation is written only where a competent implementer following the codebase would otherwise get it wrong.
   - **Rules climb:** a constraint lives on the highest task where it holds.
   - **References:** obligations reference other tasks inline as `[[slug]]` instead of restating them.
   - Obligations are in effect as soon as they are written; never ask for confirmation.
   - Every obligation you write here is introduced in the design phase (`--phase design`); the requirements you refine keep theirs. Set **where each acts** (`--acts-in`): `active` for what is built, which is nearly all of them; `verifying` for something known in advance to be tricky that needs no building, only a check verification must not skip; `merged` or `released` for what only the release can deliver. Set a requirement to act in `design` only once design obligations refine it, so that what is built and verified is the refinement, and never one that is about production after the release (a measurement after rollout, a state of production data after a cleanup run): that one acts in `released` (or `merged`), refined or not, because only that phase can show it true.
3. Resolve **design** questions here, not in `planning` or `active`. Read the code; run spikes when needed. A spike you defer needs a decision tree (outcome → action) recorded in a design-phase obligation.
4. Visual design: for anything the user will see, draw a mockup and save it with the `visual-design` noun; one requirement "matches the mockup" replaces the layout obligations it covers.
5. Ask the user only for a design choice that turns on what they want and that the requirements, the ancestors, and the codebase do not settle. Offer the options you see, with your recommendation first.

## Done when

- [ ] **Buildable:** a competent implementer, given the hierarchical context, the task's obligations, the tasks they reference, the mockups, and the codebase, would build it correctly.
- [ ] **Constraints, both directions:** the design does nothing this task's or an inherited constraint forbids, and does everything one requires.
- [ ] Every obligation reference resolves (the `obligations` noun's reference check prints nothing).
- [ ] No design question is left open that planning would have to decide; every deferred spike has its decision tree.
- [ ] **Each obligation acts where it can be shown true.** Go through them one at a time: if showing it holds needs the work merged, deployed, or run against production ("after rollout", "after the cleanup runs on prod", "a query over production finds...", a measurement of the live system), it acts in `merged` or `released`, not `active`, `verifying`, or `design`, including a requirement written in `proposed` that design refines. Fix any that do not; verification never asks the user about a phase mistake, and a wrong tag costs a whole verification round.
- [ ] The summary reflects the settled requirements.
