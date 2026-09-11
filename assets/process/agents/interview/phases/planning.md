# Phase: planning

Leads to **ready**. Settle an ordered plan that someone can start working from.

## Produces

- **Plan content** — ordered steps, each tied to the requirements it delivers and how it will be verified (`content` type `plan`).
- **Obligation changes** when planning uncovers a missing or wrong requirement.

## Ask about, roughly in order

1. **Everything still parked**, for any phase — each item is promoted into the plan or obligations, or discarded with the user's agreement. Nothing may remain open.
2. **Order and slicing** — what comes first, what can ship independently.
3. **Verification** — how each requirement will be proven, using what design already decided.
4. **Assumptions** — each is either accepted by the user or turned into a requirement.

Don't reopen settled design decisions. If planning shows one is wrong, raise that as its own question.

## Done when

- Every requirement traces through a plan step to a verification.
- Assumptions are explicit and accepted.
- **No open parked memory remains on the node.** Don't declare exhaustion while any does.
