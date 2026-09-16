# Obligations

Obligations are what a node commits to. They come in two kinds, both attached
directly to one node:

- **Requirements** — what must be true for the work to be considered done.
- **Constraints** — what bounds how the work may be carried out: technology
  choices, compatibility rules, boundaries against other work.

They are ordered within their kind, and that order is meaningful: it is the
order shown to the user in the app.

## Inheritance

The two kinds inherit differently down the outline, and the difference matters:

- An ancestor's **requirements** define that ancestor's own scope. They are
  settled and out of bounds for a descendant — a child does not re-satisfy or
  renegotiate them.
- An ancestor's **constraints** bound everything beneath it. They apply in full
  at every level of the subtree.

So when you are given an ancestor's context, expect its constraints in full and
only a summary of its requirements. That is deliberate, not truncation.

## Provenance

Every obligation records who wrote it. One written by an agent is marked
`agent` — in effect, but not confirmed by the user — and carries an
**attention** level (how likely the user is to want to change it) with a
one-line reason. Only the user, working in the app, creates a `user` one.
