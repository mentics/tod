# Incoming changes: re-evaluating nodes when what they inherit changes

Status: implemented.

## Problem

A node is built against more than its own obligations. It inherits every Spec
ancestor's constraints (`tod_core::node_context::render_inherited_context`),
and it can reference a reusable component by writing `[[slug]]` in an
obligation (`tod_store::outline::references`). When a constraint on an ancestor or
a component's obligations change, every node built on the old version may now
be wrong, and some may not be affected at all.

Today nothing notices. `tod_store::lifecycle_baseline` snapshots only a
node's *own* obligations and plan when it enters `ready`, and
`tod_core::lifecycle_validity` compares only against that. A new root
constraint leaves a hundred `approved` descendants looking valid.

## Principle

A change is recorded once, **fanned out** cheaply to the nodes it could
affect, and **evaluated** later, when the user asks or a gate check needs it.
Editing stays cheap. The user can make many changes across the tree without
starting any agent sessions, and a node collects everything pending against it
so one evaluation covers all of it.

Ancestor constraints and component references use the same process. The only
difference is how the targets are found.

## 1. Every change is recorded, whoever makes it

Fan-out needs a recorded change to point to. Today only conversation changes
are recorded: `conversation_actions` rows (with before/after) are written by
`record_and_execute` inside `run_interview`, and `conversation_id` is
`NOT NULL`. A user adding or editing an obligation from the obligations view
calls `enqueue_outline(OutlineMutation::CreateObligation | UpdateObligationBody
| DeleteObligation …)` directly. That goes into the Ctrl+Z history
(`fleet::undo`) but leaves no action row.

**Change:** node, obligation, and plan-step mutations are recorded as actions
regardless of source. Generalize the action log so a row's source is a
conversation *or* a direct user edit (`conversation_id` nullable, or a
separate `source` column), written in the same transaction as the mutation,
on the writer path every `enqueue_outline` caller shares. An agent change and a
user change then look the same to everything downstream, including fan-out
and net-change projection.

Ctrl+Z of a direct edit is recorded as a reversal of its action, so pending
entries that point at it cancel the same way a conversation reversal does.

## 2. What fans out

For ancestors, fan-out covers only what reaches a descendant's context: if it
isn't inherited, it doesn't propagate. A component reference is a different
path, not inheritance. The component's obligations are not put into its
users' context; an agent looks a referenced node up through `tod-cli` when it
needs it, most often while planning. Because a user can depend on any of a
component's obligations, requirements included, any obligation change on a
component fans out to its users.

| Change | Fans out to |
|---|---|
| A constraint-kind obligation on a Spec node is added, reworded, re-kinded (to or from constraint), or deleted | every Spec descendant (`subtree_node_ids`) |
| Any obligation on a node that others reference with `[[slug]]` is added, reworded, or deleted | every node with a `node_references` edge to it (below) |
| Requirements, details, summaries of ancestors | nothing (not inherited) |

Requirements affecting descendants' plan steps is a known gap, deliberately
out of scope here.

### Targets are filtered before queuing

A target is queued only if it could have built on the old version:

- it has the Spec capability, and
- its lifecycle state is `ready` or later. A node in `proposed` or `design`
  hasn't committed to anything and will read the new constraint from its
  inherited context anyway.

Any other cheap, deterministic exclusion belongs in this filter, not in the
evaluation.

## 3. Reference edges

Resolving `[[slug]]` references by scanning all obligation text on every change
doesn't scale, so the app maintains the edges itself:

```sql
CREATE TABLE node_references (
    obligation_id  BLOB NOT NULL,   -- the referencing obligation
    from_node_id   BLOB NOT NULL,   -- the node that owns it
    to_node_id     BLOB NOT NULL,   -- the node its [[slug]] names
    PRIMARY KEY (obligation_id, to_node_id)
);
CREATE INDEX node_references_to ON node_references(to_node_id);
```

Maintained in the same transaction as the mutation:

- **create / reword** an obligation: re-parse `referenced_slugs`, replace that
  obligation's edges.
- **delete** an obligation: remove its edges.
- **rename a node's slug / delete a node**: update or drop the edges pointing
  to it. Broken references are already reported by `references.rs`.

A migration builds the table from existing obligation text.

Ancestor edges are not stored. The subtree comes from the recursive query in
`subtree_node_ids`.

How it is kept (`tod_store::outline::references`): deletes cascade through
foreign keys, a trigger moves an edge's `from_node_id` with its obligation,
and triggers mark an obligation dirty when its text changes or a node appears
whose slug its text may name (so a reference written before its node existed
gains its edge). `OutlineMutation::execute` re-resolves the dirty ones before
it returns, in the mutation's transaction, whichever path made the change.
Slugs never change today; if they do, edges keep pointing at the node by id.

A node that both descends from a changed constraint's node and references it
gets one queue row (the key is node and action), marked `ancestor`.

## 4. The incoming queue

```sql
CREATE TABLE incoming_changes (
    node_id     BLOB NOT NULL,        -- the target
    action_id   INTEGER NOT NULL,     -- the recorded change
    via         TEXT NOT NULL CHECK (via IN ('ancestor','reference')),
    source_node BLOB NOT NULL,        -- the ancestor or component that changed
    queued_at   INTEGER NOT NULL,
    PRIMARY KEY (node_id, action_id)
);
```

- Enqueued in the same transaction as the action, for every filtered target.
- **Reversal cancels.** Reversing an action (from the conversation view or by
  Ctrl+Z) deletes its entries.
- **Net, not raw.** At evaluation time, a node's pending actions are projected
  with the same net-change rules as `net_changes`: added then deleted is
  nothing, reworded twice is one rewording. If the net is empty, the entries
  are cleared without starting an agent.

## 5. Evaluation

### When

- **On request.** The user selects one or more nodes in the tree and runs
  **Check incoming changes**.
- **Before a gate check.** When a gate check starts on a node with pending
  entries, the app runs the evaluation first, then the gate check on the
  result. The user doesn't need to start it separately.

### One session per node

Each node is evaluated in its own fresh, short-lived session, so one node's
obligations can't mix with another's. How many run at once is capped by a
setting.

There is no general "max parallel agent sessions" setting yet. Add one that is
general (not specific to this feature) so other batch work can share it.

### Context: its own recipe

This surface is narrower than every other one, so it gets its own
`ContextRecipe` (`surface/incoming-changes.md`), registered in
`ALL_RECIPES`. It does **not** use `render_inherited_context`. The node is
judged only on its own work against the changes:

- **Static:** a one-shot stance; `domain/` obligations, plan, lifecycle; `cli/`
  intro plus the `incoming` noun.
- **Dynamic:**
  - the net pending changes: each with before/after text, the source node's
    title, and whether it came through an ancestor or a reference;
  - the node's title and summary;
  - the node's own obligations, in full;
  - the node's own plan steps, in full, with status;
  - its lifecycle state.

No ancestor chain, no siblings, no other candidates.

### Verdict

The agent records one verdict through `tod-cli`. Replies are never parsed.

```
tod-cli incoming resolve <node> --affects none|plan|obligations --note "<why>"
```

| `--affects` | Meaning | Result |
|---|---|---|
| `none` | Nothing of this node's changes | entries cleared; node stays |
| `plan` | Obligations still hold; some plan steps don't | entries cleared; node should go back to `planning` |
| `obligations` | An obligation must be added, changed, or removed | entries cleared; node should go back to `design` |

The verdict and note are kept (append-only, like verdicts in
`tod_store::verification`). A `plan` / `obligations` verdict that sends the
node back is what starts its next pass (§9), so it opens that pass's work
history: "This pass began because ancestor constraint [x] 'All dialogs close
on Escape' was added. Note: the confirm dialog had no Escape handling."
`none` verdicts are kept but left out of the history, since they changed
nothing.

Resolving also records, in the node's baseline, which actions it has been
checked against. From then on the node counts as current against them.

### Moving back

The app still never moves a node itself.

- A `plan` / `obligations` verdict makes `lifecycle_validity::regression` report it
  like any other finding ("Ancestor constraint [x] was added: … (affects
  plan)"), with the existing orange **Move back** callout.
- After a multi-node check, a summary lists every affected node with its
  target state and offers **Move back all**, one confirmation for the batch.
- This applies at **every** state from `ready` through `done`, including
  `merged`, `released`, `learn`, and `done`. An affected node goes back
  through its lifecycle like any other. This is a deliberate exception to
  `lifecycle_validity`'s rule of leaving `merged` and later alone. That rule
  still holds for a node's own edits. It doesn't hold for incoming changes.

  The reason is that the tool supports two ways of using nodes:

  - **Product-model nodes** describe a piece of the product permanently. When
    what they inherit changes, the node itself is reworked and reimplemented.
    Pulling it out of `done` is correct.
  - **Task nodes** are done once and never redone. They sit under their own
    parents, apart from the product model, so they have essentially no
    constraint ancestors or component references, and nothing fans out to
    them.

  So no mode flag or follow-up node is needed. Where a node sits in the tree
  decides whether incoming changes can reach it.

### Before a gate check

A pending change matters to the gate only if it could invalidate what the
transition certifies. The rule is simple: any pending entry on a node whose
state is `ready` or later is evaluated before that node's gate check. If
evaluation finds the node affected, the gate check reports the regression
instead of passing.

## 6. Visibility

Pending changes are rare, so they should be hard to miss when they occur.

- **Tree row:** the Spec chip gains a count, e.g. `A·3`. At zero the count
  isn't rendered and takes no space.
- **Tree row text:** a node with pending entries renders its title in a
  distinct style. Add a named entry to `doc/ui-style-guide.yaml` (e.g.
  `node-title-pending-changes`) and implement it once.
- **Tree filter:** a quick filter toggle "Pending changes" beside the existing
  status filter toggles, showing only nodes with entries (and their ancestors
  for context).
- **Lifecycle panel:** "3 incoming changes" with the list (source, before/after)
  and **Check now**.

All of these update from store change events, not polling.

## 7. `tod-cli`

New noun `incoming`, with a `cli/incoming.md` fragment pinned by
`tod_cli::doc_sync`:

- `incoming list <node>`: the node's net pending changes.
- `incoming resolve <node> --affects … --note …`: record the verdict.

## 8. Mock agent

`--agent mock` gets directives so this can be driven end to end:
`affects none|plan|obligations: <note>`.

## 9. Passes and the `learn` output

A node that is sent back goes through its lifecycle again. Each trip is a
**pass**, and each pass is analyzed on its own.

- **The `learn` output is stored, once per pass, and never changes.** A
  `learn_outputs` table (`node_id`, `pass`, `content`, `at`) gets one row each
  time a node completes `learn`. A node can have many rows, one for each time
  it came back. Today the `learn` agent's output isn't stored as its own record
  (the state is only a gate check given `render_work_history`), so this table
  is new.
- **The work history covers only the current pass.** `render_work_history`
  shows only what happened after the node's latest `learn_outputs` row, or
  everything if there is none. Whatever mattered from an earlier pass is
  already in that pass's stored output. The underlying records (verdicts, step
  notes, findings, gate results) stay append-only. Only what the `learn` agent
  is shown is scoped.

## Build order

1. Record every mutation as an action, whatever its source (§1). This has to
   come first, since fan-out points at actions.
2. `incoming_changes` table, ancestor fan-out, the target filter, cancel on
   reversal (§2, §4).
3. Visibility: chip count, title style, tree filter (§6). Useful on its own:
   the user can see what's stale.
4. Evaluation: recipe, `tod-cli incoming`, verdict, general parallelism
   setting, regression integration, **Move back all** (§5, §7, §8).
5. Gate check runs evaluation first (§5).
6. `node_references` edges and component fan-out (§3).
7. `learn_outputs` and per-pass work history (§9). This can land at any
   point, and should land before anything is pulled out of `done` for real.

