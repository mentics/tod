# Phase: design

Leads to **planning**. Settle how the requirements will be met, deeply enough that a plan can be written without re-opening decisions.

## Produces

- **Design-phase obligations** (`tod-cli obligations add --kind requirement|constraint`, tagged phase `design` automatically) — one decision per obligation, stated so a planner can act on it, with `--section` naming the area it belongs to. Never restate a requirements-phase obligation that already covers it; design only adds or narrows what requirements left open.
- **Obligation changes** when a design decision binds or reveals a requirements-phase obligation — edit it in place (`obligations update`) rather than adding a duplicate.

Step-by-step implementation detail is **parked** for planning.

## Ask about, roughly in order

1. **Parked detail** for design — the user already volunteered it, so confirm it rather than asking again.
2. **Costly-to-reverse choices** — data shapes, storage, protocols, public interfaces, named constructions. Nothing here is truly irreversible, but locking it in design is far cheaper than unwinding it after planning or implementation has built on top of it.
3. **A concrete, checkable way to verify each requirement** — a command, a test, or an observable behavior that proves it's done.
4. **Appearance and layout** of anything the user will see. The user must accept these; don't infer them.
5. **Open unknowns** — resolve each, or record it as a deliberate spike with a decision tree: *if we find X, we do Y*.

When external references matter, say whether each one is **required** or only a **guideline**.

## Done when

- Every costly-to-reverse choice is decided or has a decision tree.
- Each requirement has a concrete, checkable way to be verified.
- User-visible appearance is accepted, or the user has waived it.
- No open memory is parked for design.
