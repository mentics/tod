# State: `design`

**Forward gate:** `design` → `planning`

## On entry

1. Read lifecycle state, resolved obligations (including inherited and design-phase), the buildable evaluation, and plan steps (if any).
2. Regenerate the node's summary from its now-settled requirements-phase obligations: `tod-cli content set --node <UUID> --type summary --body <TEXT>` (overwrite, not append). Ancestor nodes use this to give this node's descendants a short scope statement in place of their requirements.
3. If the node is buildable and the gate checklist passes → verify upstream conformance and proceed to exit.

## Responsibilities

### Writing the spec

The node's spec is written in the app's **conversation** view: the user gives direction, and the conversation agent writes the obligations directly, places what belongs on other nodes, and flags what it is unsure of in the conversation's change set. The user reviews that change set and reverses or edits what is wrong. This session does not run sequential Q&A.

- **Goal:** the smallest set of obligations that gets the node built correctly, with the least human attention. An obligation is written only where a competent implementer following the codebase would otherwise get it wrong.
- **Rules climb:** a constraint lives on the highest node where it holds.
- **In effect at once:** obligations are in effect as soon as they are written; never ask for confirmation.
- **References:** obligations reference other nodes inline as `[[slug]]` instead of restating them.

### Research and spikes

Resolve **design** questions here—not in `planning` or `active`.

- Run spikes in subagents/worktrees when needed.
- Deferred spikes need an explicit **decision tree** (outcome → action) recorded in a design-phase obligation.

### Visual design

Visual design is part of writing the spec: for anything the user will see, a mockup is drawn before anything is asked, saved with `tod-cli visual-design save`, and one requirement "matches the mockup" replaces the layout obligations it covers.

**Implementation interview belongs in `planning`, not here.**

## Forward gate rules (`design` → `planning`)

The gate is **buildable** only: a competent implementer, given the hierarchical context, the node's obligations, the nodes they reference, the mockups, and the codebase, would build it correctly. Any obligation change touching the node resets it to pending.

- Confirming obligations is **never** required.
- Obligation references resolve (`tod-cli obligations check-refs --node <UUID>` prints `(none)`).

Living checklist items for this transition are stored in the app database; return `gate_results` for each when gate-checking.

## Exit

When the `design` → `planning` gate passes, return `forward_lifecycle: planning` (app applies).

## Blockers

A failing buildable evaluation that needs the user's direction, unenumerated deferred spikes, or upstream conflicts → `blocked` / stay in `design`.
