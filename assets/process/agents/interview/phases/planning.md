# Phase: planning

Leads to **ready**. Settle an ordered plan that someone can start working from.

## Produces

- **Plan steps** — discrete units of work (`tod-cli plan add`), each tied to the requirements it delivers via `--satisfies`/`plan satisfy` and to whatever must land first via `--depends-on`/`plan depend`. Think in terms of what can happen in parallel, not one long linear sequence — independent steps become siblings with no dependency between them, not a forced order. Advance a step's status (`plan update --status`) as work on it is scoped out and completed.
- **Obligation changes** when planning uncovers a missing or wrong requirement — recorded as ordinary requirement/constraint obligations, never tagged phase `planning`.

## Draft the plan yourself first

Plan steps are not obligations — they don't belong to the user the way requirements do. Before writing any question, draft every step you can reasonably determine yourself: read the obligations and design decisions, work out the slicing, dependencies, and `--satisfies` links, and add them with `plan add`. Most of a plan is mechanical given a settled design; the user should arrive to a plan that's mostly there, not a blank one built one question at a time.

Only ask when you genuinely can't determine the answer yourself — a real tradeoff with no obvious right call, or a gap the design phase left open. If you can draft a step and defend it, draft it; don't turn a judgment call into a question just to be safe.

This runs across many turns, not once. Your snapshot and each turn's delta already show you the current plan — treat it as the starting point, not something to re-derive. Each turn: check what's already there against what the (possibly now-changed) obligations and design call for, add what's genuinely missing, update a step whose obligations changed under it, and leave the rest alone. Never re-add a step that already exists in different words.

## Ask about, roughly in order

1. **Everything still parked**, for any phase — each item is promoted into the plan or obligations, or discarded with the user's agreement. Nothing may remain open.
2. **Order and slicing you couldn't resolve yourself** — a genuine ambiguity about what comes first or what can ship independently, not routine sequencing.
3. **Verification** — only where design left the proof method genuinely open.
4. **Assumptions** — each is either accepted by the user or turned into a requirement.

Don't reopen settled design decisions. If planning shows one is wrong, raise that as its own question.

## Done when

- Every requirement traces through a plan step to a verification.
- Assumptions are explicit and accepted.
- **No open parked memory remains on the node.** Don't declare exhaustion while any does.
