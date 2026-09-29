# State: `design`

**Gate:** `design` → `planning`. The app checks it: the phase is certified. The certificate covers the node's own obligations, its design content, and its mockups; any change to them makes it stale.

## Your work

1. First, regenerate the node's summary from its details and its settled requirements with the `content` noun (`summary` type, overwrite). It is all this node's descendants see of its details and requirements.
2. Write the spec: the smallest set of obligations that gets the node built correctly. An obligation is written only where a competent implementer following the codebase would otherwise get it wrong.
   - **Rules climb:** a constraint lives on the highest node where it holds.
   - **References:** obligations reference other nodes inline as `[[slug]]` instead of restating them.
   - Obligations are in effect as soon as they are written; never ask for confirmation.
   - Every obligation you write here is introduced in the design phase (`--phase design`); the requirements you refine keep theirs. Set **where each acts** (`--acts-in`): `active` for what is built, which is nearly all of them; `verifying` for something known in advance to be tricky that needs no building, only a check verification must not skip; `merged` or `released` for what only the release can deliver. Set a requirement to act in `design` only once design obligations refine it, so that what is built and verified is the refinement.
3. Resolve **design** questions here, not in `planning` or `active`. Read the code; run spikes when needed. A spike you defer needs a decision tree (outcome → action) recorded in a design-phase obligation.
4. Visual design: for anything the user will see, draw a mockup and save it with the `visual-design` noun; one requirement "matches the mockup" replaces the layout obligations it covers.
5. Ask the user only for a design choice that turns on what they want and that the requirements, the ancestors, and the codebase do not settle. Offer the options you see, with your recommendation first.

## Done when

- [ ] **Buildable:** a competent implementer, given the hierarchical context, the node's obligations, the nodes they reference, the mockups, and the codebase, would build it correctly.
- [ ] **Constraints, both directions:** the design does nothing this node's or an inherited constraint forbids, and does everything one requires.
- [ ] Every obligation reference resolves (the `obligations` noun's reference check prints nothing).
- [ ] No design question is left open that planning would have to decide; every deferred spike has its decision tree.
- [ ] The summary reflects the settled requirements.
