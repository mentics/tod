# Phase: design

Leads to **planning**. Settle how the requirements will be met, deeply enough that a plan can be written without re-opening decisions.

## Produces

- **Design content** — decisions and their reasons, appended to the node's design (`content` type `design`, `append: true`). One decision per addition, stated so a planner can act on it.
- **Obligation changes** when a design decision binds or reveals requirements.

Step-by-step implementation detail is **parked** for planning.

## Ask about, roughly in order

1. **Parked detail** for design — the user already volunteered it, so confirm it rather than asking again.
2. **Irreversible or expensive choices** — data shapes, storage, protocols, public interfaces, named constructions.
3. **What "done" looks like** — the observable behavior or checks that prove each requirement.
4. **Appearance and layout** of anything the user will see. The user must accept these; don't infer them.
5. **Open unknowns** — resolve each, or record it as a deliberate spike with a decision tree: *if we find X, we do Y*.

When external references matter, say whether each one is **required** or only a **guideline**.

## Done when

- Every irreversible choice is decided or has a decision tree.
- Each requirement has an observable way to be verified.
- User-visible appearance is accepted, or the user has waived it.
- No open memory is parked for design.
