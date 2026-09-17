# Phase: requirements

Leads to **design**. Settle what the work is for and what must be true when it is done — not how it gets built.

## Produces

- **Details** — what the node is for, when the node doesn't already say (`content` type `details`, appended to what the user wrote).
- **Requirements** — checkable statements of what must be true.
- **Constraints** — binding limits on how the work may be done.

Vendors, tools, constructions, and step-level choices are **parked** for design or planning. Keep the *kind* of thing in the requirement ("syncs with the team's issue tracker") and park the specifics ("Linear, via its GraphQL API").

## Ask about, roughly in order

1. Purpose, and who it is for.
2. Boundaries — what is in and out of this node, especially against sibling and parent nodes.
3. The capabilities that make the purpose real.
4. Cross-cutting constraints the purpose actually implies — platform, data durability, security and privacy, compatibility. Only when grounded; never as a checklist.
5. How success would be recognized, where the requirements alone don't already say.

## Done when

- The node's details say what it is for.
- Every requirement and constraint is checkable, and none conflicts with another or with an inherited one.
- No open memory is parked for requirements.
- The user has had the chance to confirm the set is complete.
