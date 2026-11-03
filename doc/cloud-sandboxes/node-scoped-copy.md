# A node's copy holds only that node

Status: implemented (`tod_store::sync::slice`); not yet deployed to the
development orchestrator.

## The problem

Before this change, an autonomous node's supervisor (`tod-supervisor`, in the node's sandbox)
runs the lifecycle `Autopilot`, which reads and writes a `FleetStore`. So the
supervisor kept a local copy of the user's database
(`/var/lib/tod-supervisor/<node>/tod.db`, `tod_supervisor::replica`):

- seeded from `GET /users/<u>/snapshot`, which is `sync::snapshot`, a
  byte-for-byte copy of the **whole** database;
- brought up to date before every step and turn from `GET
  /users/<u>/changes`, which was **not filtered**: every change to every node
  in every project, every conversation turn included;
- pushing back what the supervisor itself writes (its conversations,
  lifecycle moves, heartbeats, live turns).

The agent does not use this copy: its `tod-cli` goes to the orchestrator's
`/cli`, except `pr`, `secrets` and `db` (`cli_relay::LOCAL_NOUNS`), which run
the real `tod-cli` against it.

So every node sandbox held, and kept current, everything the user has. No
credentials (`SYNCED_TABLES` has none), but every project's obligations,
plans and transcripts, readable by the agent and by the repository's code.
The autopilot needs only a small part of it.

## The change

The orchestrator serves a node's supervisor a **slice**: that node's own
rows, the parts of its ancestors that a node inherits, and the rows those
rows reference. The snapshot and the change feed are both cut to it. The
supervisor, the autopilot and the local nouns are unchanged: they open a
`FleetStore` as before and find what they look for.

The app's sync is unchanged: it still sends and receives everything.

### What is in a node's slice

For node `N` with ancestors `A` (`outline::ancestor_chain`, which needs only
the ancestors' `outline_entries`):

| Part | Tables | Why |
|---|---|---|
| Structure | `nodes`, `outline_entries` for `A ∪ {N}`; the `lists` row they are in | `ancestor_chain`, slugs, titles |
| Inherited, for `A` | `node_capabilities`, `node_lifecycle`, `node_fields`, `node_files`, `node_agent`, `node_environment`, `node_lifecycle_config`, `node_groups`, `node_obligations`, `node_extra_content`, `node_tags` | `resolve_files_for_node`, `resolve_agent_for_node`, `environment::resolve` (so `secrets`), `lifecycle_config::resolve`, `node_context`'s inherited context (Spec summary, constraints, requirements) |
| Everything, for `N` | every synced table, by owner (below) | the autopilot, protocols, gates, waits, stop questions, `pr` |
| Referenced | a row any of the above references by foreign key, and what that row references in turn (not the rows that reference it) | so the copy passes `PRAGMA foreign_key_check` |

Left out: other nodes, the ancestors' conversations, plans, verdicts,
findings, decisions and events, `N`'s descendants, project-focus
conversations, and media not linked from the slice.

The "Inherited" list is fixed in code (`sync::slice::INHERITED_TABLES`). A
test runs every resolver the supervisor uses against a slice and against the
full database and compares the results, so a resolver that starts reading
another ancestor table fails a test rather than a run.

Descendants are left out because nothing the supervisor runs reads them (the
gates, `node_context`, and the conversation context only go up the chain).
The agent still sees them: its `tod-cli` goes to the orchestrator. If a gate
ever needs children, they come in as structure only (`nodes`,
`outline_entries`), the same way as ancestors.

### Who owns a row

The feed has to decide, per logged change, whether it is in a slice. Today
`sync_changes.node_id` (`sync::node_expr`) is the row's own `node_id`
column, and `NULL` for tables without one: `conversations` and the
`conversation_*` tables, the plan step's `_deps`, `_obligations` and
`_notes`, `decision_answers`, `lists`, `media_assets`. Those are exactly the
largest ones (turns), so `node_id` alone cannot scope the feed.

A new column `sync_changes.scope_node_id` records the **owner**, written by
the same generated triggers:

| Table | Owner |
|---|---|
| `nodes` | `id` |
| `node_subtree_archives` | `root_node_id` |
| has `node_id`, except `conversation_actions` | `node_id` |
| `conversations` | the focused node (`focus_id` for a node focus, `focus_node_id` for an obligation or plan step; `NULL` for a project focus: in no slice) |
| `conversation_turns`, `_flags`, `_reports` | the conversation's |
| `conversation_actions` | the conversation's; an action in no conversation (a direct edit, an undo) is its own `node_id`'s |
| `node_plan_step_deps`, `_obligations`, `_notes` | the step's `node_id` |
| `decision_answers` | the decision's `node_id` |
| `lists`, `media_assets` | `NULL`; decided at export (below) |

A conversation's action is owned by its conversation, not by the node the
action touched (its own `node_id`): a conversation on `N` that renames a
child is still `N`'s history.

`node_id` keeps its current meaning, since `impact_handler`, `answers` and
`wait_changes` read it. A trigger looks the owner up when the row changes,
so a delete still knows it (the parent conversation or step may be deleted
in the same cascade, which is why this is not done at export time from the
current row). `owner_expr` holds the rules; `every_synced_table_has_an_owner_rule`
fails when a synced table has none and is not in `UNOWNED_TABLES`, so a new
synced table cannot leak into every slice or silently miss one.

### The snapshot

`GET /users/<u>/snapshot?slice=1` returns the slice of the node named by
`X-Tod-Node` (which the supervisor already sends) instead of the whole
database; `?slice=1` without that header is a 400, and so is a node the
user's database does not have. (Why a parameter and not the header alone:
see Migration.)

`slice::snapshot_slice`, under the user's sync lock:

1. Create an empty database with the current schema
   (`schema::open_writer_connection`, so the same migrations and the
   `gate_criteria` catalog), and drop its triggers for the copy, so copied
   rows are the source's as they are: no sync log, no journey rows, no
   derived writes such as staleness.
2. `ATTACH` the user's database and, in one read transaction, find the
   ancestor chain and `INSERT … SELECT` each synced table's slice.
3. Close the foreign-key gaps: repeat `PRAGMA foreign_key_check`, copying
   each missing parent from the source, until none is missing (bounded, and
   an error naming the table if it does not settle or the parent is not in
   the source either).
4. Put the triggers back, mark every obligation for the references cache to
   rebuild, and record the slice in a one-row `sync_slice` table: the node,
   the slice id, and the **source's** `last_seq`, since the slice's own
   `sync_changes` is empty.

`Replica::open` reads its `pull_after` and `slice_id` from `sync_slice`
(`slice::slice_info`), and refuses a snapshot that has none (a whole
database) or is another node's.

### The feed

`GET /users/<u>/changes?after=<n>&slice=1` exports only changes in
`N`'s slice (`sync::export_changes_in` with a `slice::Scope`). A change is
in it when:

- its `scope_node_id` is `N`;
- its `scope_node_id` is in `A` and its table is in `INHERITED_TABLES`, or
  is `nodes` / `outline_entries`;
- it is a `lists` row the slice's entries are in, or a `media_assets` row a
  slice node links to (looked up at export; both are small);
- it is a row a change above references and the slice leaves out (sent
  just before that change, as it is now): for example the conversation an
  obligation's verdict was recorded in, when that is another node's.

The same grouping as `export_changes_for` applies, so the cursor semantics
do not change.

**When the slice itself changes.** Moving `N` (or an ancestor) changes `A`.
The slice id is `N`'s ancestor chain, root first (`Scope::id`). The feed
returns it (`"slice_id"`, beside `seed_id`). A replica opening with a
different one in `supervisor-sync.json` seeds a new copy, as it does when
`seed_id` changes; a pull that sees it change mid-run fails, saying the node
moved, and the next start takes a new copy (also as for `seed_id`). A copy
is a cache, and a slice is much smaller than the whole database.

### What the supervisor pushes

`POST /users/<u>/changes` from a `supervisor-` client that names its node
(`X-Tod-Node`, which every supervisor sends) goes through
`sync::apply_changes_confined`: the whole batch is refused (400, naming each
row and its owner) when a row it writes is not owned by `N` or a node under
`N`, or a row it deletes was owned by another node. A delete whose owner can
no longer be read (its parent went in the same batch) is allowed; the
parent's delete is checked. Nothing stopped a supervisor from writing
anywhere before; with a slice it could only send rows it can see, but the
check makes the boundary explicit and catches a resolver bug that writes an
ancestor's row. The app's pushes are not checked.

### The local nouns

`pr`, `secrets` and `db` keep running against the copy. What they read is in
the slice: `node_fields` and conversations of `N` (`pr`), the environment
over the ancestor chain (`secrets`), and the node's environment (`db`, which
otherwise asks the orchestrator's broker).
`the_resolvers_answer_the_same_on_a_slice` checks the resolvers in
`tod-store` that read up the chain (obligations, Files, Agent, environment,
lifecycle config, the chain itself) give the same answer on a slice as on
the whole database.

## Migration

- The orchestrator adds `sync_changes.scope_node_id`. The triggers are
  recreated on every open (`sync::install`), so new changes get an owner
  immediately. Older log rows have none; that is fine because:
- A supervisor whose saved cursors have no `slice_id` reseeds (as if
  `seed_id` had changed), so no copy keeps working from the full database or
  from log rows written before the owner existed.
- An existing sandbox's full copy is deleted by that reseed
  (`Replica::open` already removes `tod.db`, `-wal`, `-shm` before
  restoring).

An old supervisor already sends `X-Tod-Node` but would read its cursor from
the slice's empty `sync_changes` and pull from 0. So the new supervisor asks
for a slice explicitly (`?slice=1` on both `snapshot` and `changes`), and a
request without it gets the full database and feed as before, until every
sandbox's supervisor is updated; then the unsliced path for supervisors can
be removed. Deploy the orchestrator first: a new supervisor against an old
orchestrator fails at open, saying the orchestrator does not cut a node's
slice (its feed has no `slice_id`).

## Where it is

- `tod_store::sync`: `scope_node_id`, `owner_expr`, `UNOWNED_TABLES`,
  `export_changes_in`, `apply_changes_confined`.
- `tod_store::sync::slice`: `Scope`, `INHERITED_TABLES`, `snapshot_slice`,
  `slice_info`, and the foreign-key closure for snapshots and the feed.
- `tod-orchestrator`: `slice_node` and `supervisor_node` in `lib.rs`, the
  `slice` and `confined_to` arguments in `sync_backend`.
- `tod_supervisor::replica` and `orchestrator`: `?slice=1`, `slice_id` in
  the cursors and the feed.
- Tests: `sync::tests` (owner rules, what a slice holds, the closure, the
  scoped feed, the confined push, the resolvers);
  `tod-supervisor/tests/wake.rs` runs a mock node through its lifecycle on a
  slice against an orchestrator in the test.

## Not covered

- **The agent's `tod-cli` can still read any node.** `/cli` picks the user's
  database from `X-Tod-User` and runs any command; an agent sandbox acts
  only on its own node "by convention, not enforcement"
  (`autonomous-nodes.md`). Scoping `/cli` is a separate change.
- **No database on the sandbox at all.** That means running the autopilot on
  the orchestrator and only the agent and git in the sandbox, as the app
  already does for Files sandboxes. It moves where GitHub is reached and how
  sandboxes sleep, and is a larger design of its own.

## Open questions

- Does any gate or protocol read a sibling or a descendant? None found
  (`gate::derived`, `node_context`, `conversation::context` only walk up),
  and the mock lifecycle in `wake.rs` runs on a slice, but no test compares
  every gate criterion on a slice with the whole database.
- Can a plan step depend on another node's step (`node_plan_step_deps`
  references `node_plan_steps` with no node check)? If so the referenced
  step comes in through the foreign-key closure; confirm the plan gates do
  not then count it as `N`'s.
