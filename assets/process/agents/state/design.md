# State: `design`

**Forward gate:** `design` → `planning`

## On entry

1. Read lifecycle state, resolved obligations (including inherited and design-phase), open choices, the buildable evaluation, and plan steps (if any).
2. Regenerate the node's summary from its now-settled requirements-phase obligations: `tod-cli content set --node <UUID> --type summary --body <TEXT>` (overwrite, not append). Ancestor nodes use this to give this node's descendants a short scope statement in place of their requirements.
3. If the node is buildable and the gate checklist passes → verify upstream conformance and proceed to exit.

## Responsibilities

### The drafting loop

The node's spec is drafted in the app's **drafting** view (`agent/drafting/drafter.md`). The user dumps, reviews `agent` obligations highest attention first, sends things elsewhere, and answers the rare choice; the drafter researches, drafts the smallest powerful set of obligations, places what belongs on other nodes, and records **buildable**. This session does not run sequential Q&A.

- **Goal:** the smallest set of obligations that gets the node built correctly, with the least human attention. An obligation is written only where a competent implementer following the codebase would otherwise get it wrong.
- **Rules climb:** a constraint lives on the highest node where it holds.
- **Provenance:** drafted obligations are `agent` and in effect. Nothing but the user touching one makes it `user`; never ask for confirmation.
- **References:** obligations reference other nodes inline as `[[slug]]` instead of restating them.

### Research and spikes

Resolve **design** questions here—not in `planning` or `active`.

- Run spikes in subagents/worktrees when needed.
- Deferred spikes need an explicit **decision tree** (outcome → action) recorded in a design-phase obligation.

### Visual design

Visual design is part of the drafting loop: for anything the user will see, a mockup is drawn before anything is asked, saved with `tod-cli visual-design save`, and one requirement "matches the mockup" replaces the layout obligations it covers.

**Implementation interview belongs in `planning`, not here.**

## Forward gate rules (`design` → `planning`)

The gate is **buildable** only: no choice is open, and the drafter judged that a competent implementer, given the hierarchical context, the node's obligations, the nodes they reference, the mockups, and the codebase, would build it correctly. Any dump, edit, or move touching the node resets it to pending.

- Confirming `agent` obligations is **never** required, and passing the gate leaves every obligation's provenance as it was.
- Obligation references resolve (`tod-cli obligations check-refs --node <UUID>` prints `(none)`).

Living checklist items for this transition are stored in the app database; return `gate_results` for each when gate-checking.

## Exit

When the `design` → `planning` gate passes, return `forward_lifecycle: planning` (app applies).

## Blockers

Open choices, a failing buildable evaluation the drafter can't resolve, unenumerated deferred spikes, or upstream conflicts → `blocked` / stay in `design`.
