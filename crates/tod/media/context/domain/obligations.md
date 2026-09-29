# Obligations

Obligations are what a node commits to. They come in two kinds, both attached
directly to one node:

- **Requirements** — what must be true for the work to be considered done.
- **Constraints** — what bounds how the work may be carried out: technology
  choices, compatibility rules, boundaries against other work.

They are ordered within their kind, and that order is meaningful: it is the
order shown to the user in the app.

An obligation's text must stand on its own. It is stored with the outline, not
beside any repository, so it cannot link to or name a file (a repo doc, a
relative markdown link, a local path) — state what the file says instead. Such
text is refused. URLs and `[[slug]]` node references are fine.

## Inheritance

The two kinds inherit differently down the outline, and the difference matters:

- An ancestor's **requirements** define that ancestor's own scope. They are
  settled and out of bounds for a descendant — a child does not re-satisfy or
  renegotiate them.
- An ancestor's **constraints** bound everything beneath it. They apply in full
  at every level of the subtree.

So when you are given an ancestor's context, expect its constraints in full and
only a summary of its requirements. That is deliberate, not truncation.

## References

A `[[slug]]` in an obligation names another node, usually a reusable
component this node uses. A referenced node is not inherited: none of its
obligations are in your context. When you need them, most often when
planning, since the component's requirements and constraints can call for plan
steps here, look the node up by its slug and list its obligations with
`tod-cli`.


## Two phases

Every obligation carries two lifecycle states. They answer different
questions and are set separately; the app shows both.

- **Introduced in** (`phase`): the state whose work wrote it. Always
  `requirements` (written while the node is `proposed`: what the user wants,
  high level) or `design` (written in `design`, or later when verification or
  review turns up something the spec left out: how it is to be built and what
  the work must do). It records where the obligation came from and is not
  changed to say where it is acted on. A design obligation refines the requirements
  it is written for.
- **Acts in** (`acts_in`): the state whose agent takes action on it, which is
  what the gates count. It is one of `design`, `active`, `verifying`, `merged`,
  or `released`.
  - `active` (the default, and nearly always right): the work is built, so it
    needs a plan step and a verdict.
  - `verifying`: it needs no building, only checking, because the
    obligation is known in advance to be tricky and must not be missed. It
    needs no plan step; verification rules on it.
  - `merged`, `released`: only that phase can deliver it (a migration that must
    precede the deploy; a backfill against production). It needs a plan step of
    that phase, and is verified by that phase rather than by `verifying`.
  - `design`: a high-level obligation that the design phase carries out through
    design obligations that refine it. No plan step delivers it and
    verification does not rule on it; the design obligations that refine it are
    what get built and verified. Use it only when refining obligations exist.

Set `--phase` to the state you are working in. Set `--acts-in` only when it is
not `active`. Do not leave it `active` for something no plan step can build
(an obligation to check a thing, or one only a later phase can deliver): it
will fail the planning trace, or wait for a step that cannot exist.
