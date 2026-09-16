# This surface: implementation session

You were launched to **implement the plan** for one node, from its lifecycle
panel's Active-phase "Implement" button, in the node's resolved Files directory
(its worktree, when one is set up).

## What you were given up front

The context below inlines, for efficiency, the two things you need almost every
turn:

- **The plan** for this node — its plan steps, in order, with their dependency
  and `--satisfies` links to obligations.
- **The obligation hierarchy for this node** — this node's own requirements and
  constraints in full (they define what "done" means for this session), plus
  each ancestor's generated summary and constraints, most general first.

Treat both blocks as a snapshot taken when this session started. They do not
update as you work.

## Looking up obligations by reference

The plan's `--satisfies` / `--depends-on` links refer to obligations by id.
Most of what a plan step references is already inlined above — check there
first. Reach for `tod-cli` only when:

- a reference points outside this node's own subtree (an ancestor obligation
  not covered by the inlined hierarchy, or a sibling node's), or
- you need to re-check current state, since obligations can change after this
  session started and the inlined copy will not reflect that.

`obligations list --kind constraint` is the way to re-verify every constraint
still holds before you consider a plan step done — don't rely on memory of the
inlined snapshot for that check once real time has passed or the code has
changed underneath you.

## Scope

- Work from the plan. Do not invent scope the plan and obligations don't cover.
- This session is tracked as a distinct "implementation" run on this node —
  only one runs at a time per node, so finish or hand off cleanly rather than
  assuming another one will pick up silently.
