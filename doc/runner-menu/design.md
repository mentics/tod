# The runner menu, and one sandbox per node

Status: design (November 2026), revised with the product owner's answers.
Nothing here is built yet except the lifecycle config's "Delete sandbox"
checkbox (schema v92, uncommitted in this worktree). The plan is
[implementation.md](implementation.md).

## Today (being removed)

The code has two creators that make differently configured sandboxes for a
node. `provision::make_sandbox` (`tod-store`, through
`Sandboxes::create_with_proxy`) makes one when the node's Files location runs
in a cloud sandbox, and records it in `node_files_locations.sandbox`; it has
the repository, the relay and the user's credentials, but not the node
environment (`TOD_NODE`, `TOD_USER`, the orchestrator URL), the orchestrator
and Blaxel proxy rules, or the supervisor. `cloud_sync::ensure_node_sandbox`
(`tod-core`, behind the menu's "Run in the cloud") forks the golden or creates
from an image, with all of those, installs the supervisor, and records the name
in `cloud_nodes.sandbox`. Each tries to adopt the other's sandbox, but one made
by `make_sandbox` cannot pass the supervisor's health check (`Check::Env`
needs `TOD_NODE`); read from the code, not verified live.

This work merges them into one creator that always makes a sandbox able to
host the supervisor. The menu that exposed the split, today:

| Today's entry | What it acts on |
|---|---|
| Run in the cloud, Move to the cloud, Reinstall and restart | `cloud_sync::start_in_cloud` → `cloud_nodes` |
| Run on this machine instead, Stop running in the cloud | the local runner / `lost::stop_running_in_cloud` |
| Delete Sandbox (and without a checkpoint) | `lost::delete_node_sandbox`: only a `cloud_nodes` row; a sandbox made by `make_sandbox` alone fails with "this node does not run in the cloud" |
| Check now | `autopilot::check_now`: reads GitHub in `pr_draft`, `approved`, `merged` only |
| Poll now, Keep going, Stop | a running poll; the supervisor's "leave it stopped" answer; a hand-started conversation |

In the rest of this document there is one sandbox per node, called **the
node's sandbox**. The chat panel, shells, agent turns, lifecycle phases and
the supervisor all use it. The **golden** is the sandbox a project's node
sandboxes are forked from (the repository's checkout and the toolchain).

### Other findings

- **Delete does not forget the Files record.** `delete_node_sandbox_with`
  marks the `cloud_nodes` row lost; `node_files_locations` keeps naming the
  deleted sandbox until a launch fails and `retry_if_sandbox_lost` forgets it.
- **Two checkpoint paths.** `cloud_sync::checkpoint_before_delete` (by name)
  and `fleet::node_checkpoint::checkpoint_node` (by location), both ending in
  `checkpoint::checkpoint_dir`.
- **Local runs outlive the app.** `tod-agentd` hosts them
  (`crates/tod-agentd/src/runners.rs`); `doc/ui/task-panel.md` ("Closing the
  app stops a local run") is stale. Resume is never needed because the app
  closed, locally or in the cloud.
- **The final-phase delete only ran on a sync**, and only for `cloud_nodes`
  rows (fixed in B4: `sandbox_retire`, run by the daemon).
- **No cloud Pause or Stop now exists.** `node_requests` kinds are `run`,
  `fresh_session`, `check_now`, `keep_going`, fixed by a SQL `CHECK`.
- **Where the gate is checked today.** `.claude/CLAUDE.md` and
  `views/lifecycle_control.rs`'s module comment describe a
  `LifecycleController::check_gate` and an Advance button in the conversation
  view and lifecycle panel. Neither exists in this tree:
  `LifecycleController` only reloads the criteria last recorded
  (`load_persisted`), and `crates/tod-ui/src/conversation/` is gone. The gate is
  checked in one place, the autopilot's `gate` step (`Autopilot::gate`,
  `autopilot/mod.rs`): `phase::settle_gate` (derived criteria from
  `gate::derived`, saved as evaluations), then `lifecycle::advance` when clear.
  `lifecycle::advance_after_criteria` exists but only tests call it. Check now
  calls the same two functions.

## The model

**One node, one sandbox: the one its resolved Files location gives it.**

- Where a node's files are decides where its autopilot runs:
  `tod_core::runner_place::of(fleet, node) -> RunnerPlace { Local, Supervisor }`
  from the resolved location. This machine or a dev container → the daemon's
  local runner; a cloud sandbox → the supervisor in the node's sandbox.
- To move a node, the user moves its files: **Migrate to…**.
- One creator makes the node's sandbox, and its name is recorded in one place,
  `node_files_locations.sandbox`. `cloud_nodes` stays as the orchestrator's
  bookkeeping (active nodes, owner, context and lost marks) plus a **copy** of
  the sandbox name, which the orchestrator needs to poke it, since
  `node_files_locations` is machine-local. One function writes that copy, from
  the location row. No UI names `cloud_nodes`.
- The store allows one location row and one `cloud_nodes` row per node. That
  stays; behaviour and UI assume one sandbox.
- **No orchestrator, no sandbox.** Making the node's sandbox needs a reachable
  orchestrator (its host goes in the sandbox's proxy and environment). Without
  one, making it fails loudly: "No orchestrator is reachable, so this node's
  sandbox cannot be made. Start the app (it deploys the orchestrator) or run
  `tod-sandbox orchestrator`, then try again."

## The golden sandbox

**Today.** A sandbox location either makes the node's sandbox from an image or
forks the workspace's **golden** sandbox (`SandboxFrom::Fork("golden")`,
resolved by `Sandboxes::resolve_golden`; `tod-sandbox golden <name>` points the
name at a sandbox). The golden is made outside tod (the error text names
`blaxel-sbx golden refresh`). Before using a fork, tod checks the golden
supplied (`provision::require_fork_prerequisites`): the repository checkout,
`git`, `curl`, `tar`, `gzip`, `bash`, `ps`, `scp`, an `sftp-server`, Node ≥
20.10 with npm, and `claude-agent-acp`. Then, **on every fork**,
`node::install_into_fork` uploads tod's own files: the relay, the `tod-cli`
shim, the Linux `tod-cli`, the supervisor, and the process, media and skills
bundles (hundreds of files, one upload each), and writes the manifest.
(`node::ensure_base`, a per-user "base" with tod's files in it, has no
caller.)

**Target.** Everything that is the same for every node goes into the golden,
so a fork needs only per-node setup:

| In the golden (built once per tod build) | Per node, after the fork |
|---|---|
| Toolchain: git, curl, tar, bash, ssh tools, Node ≥ 20.10, `claude-agent-acp`, `claude` | Environment: `TOD_NODE`, `TOD_USER`, orchestrator URL, scheduler, workspace (`fork_with_envs`) |
| The repository checkout (as today) | Proxy rules and labels: the user's and the node's credentials, Blaxel, orchestrator (`update_proxy`) |
| The relay, `tod-cli` shim and Linux `tod-cli`, the supervisor | The branch checked out, submodules on it |
| The process, media and skills bundles; the node manifest | Supervisor settings (`push_supervisor_settings`) |
| A label with the tod install stamp (`refresh::install_stamp`) | Session logs and the checkpoint restored; relay started; poke only if the runner is to run |

**One golden per project** (per repository, since it holds that repository's
checkout), and it only moves forward. Today `golden` is one name per Blaxel
account (`Account::golden` in `sandboxes.toml`); it becomes one per repository
(`owner/name`, as `cloud_nodes::normalize_repo` writes it). It is never kept
per build.

A golden whose tod files are older than this build is **refreshed**, not
patched per node: `tod-sandbox golden refresh` (new) forks the project's
golden, installs tod's files with `node::install`, labels it, and points the
project's golden at the result. The app runs it before making a sandbox when
the golden is older, and says so in the progress. A fork then never gets a
per-node install of tod's files. An **image** source has no golden: it gets
the full `node::provision`, as today.

**Older build meets newer golden (proposal).** The install stamp
(`refresh::install_stamp`) is a hash, so it says "different" but not "newer".
The golden is therefore labelled with the stamp **and** an order: the sync
contract (`wire`, `schema`, `sync::contract::Contract`) and the build's commit
time. Then:

- same stamp: use it;
- the golden is older: refresh it forward;
- the golden is newer: **refuse**, naming both: "The golden sandbox for
  acme/widget was refreshed by a newer tod (install <theirs>, schema <n>);
  this tod is older (install <mine>, schema <m>). Update tod to make sandboxes
  for this project." Never refresh it back, since that would break the newer
  users' sandboxes;
- same order, different stamp (two builds of one commit with local changes,
  the developer's case): treat as forward and refresh, saying so.

Refusing is right because a fork would run the newer supervisor and `tod-cli`
against this app's older schema and sync contract, which the contract check
already treats as an error for the orchestrator.

## The runner menu

A split button: the default action moves the runner along; the menu holds the
rest. Entries show whenever they make sense.

| Entry | Shown when | Does |
|---|---|---|
| **Start** (default) | idle, never run (or done and moved back), not done | Starts the autopilot where `runner_place` says: the daemon's local runner, or the supervisor in the node's sandbox (made if need be). |
| **Resume** (default) | idle, the runner ended before `done` (paused, stopped, failed, a request), **except** after a spent budget | Continues the same agent session. Locally `start(node, false)`; supervised, a `keep_going` request, which also answers a "leave it stopped" stop question (`continue_over_stop_answer`). Not shown for a run that ended only to wait: the daemon or scheduler wakes it. |
| **Resume with a fresh budget** | with Resume; and **the default** after a spent budget, when plain Resume is hidden | Resume with the turn and time budget reset; the session is kept. Locally `start(node, true)`; supervised, a new `fresh_budget` request. |
| **Run with a new session** | with Start or Resume (including after a spent budget) | The current step in a new agent session. |
| **Pause** (default while running) | the runner is active | Stops once the current turn ends. Supervised: a new `pause` request, read at the supervisor's next boundary. |
| **Stop now** | the runner is active, or a conversation started by hand is running | Cancels the turn in flight. Locally `NodeRunners::stop_now`; a hand-started conversation through `AgentRuns::stop` (today's Stop, then the default); supervised, a new `stop_now` request plus a marker file the supervisor's `watch` sees mid-turn. |
| **Check now** | the node is idle and in any state but `done` | Runs the current state's gate check now (below). Hidden while the runner is active. |
| **Migrate to…** | not done, and the node has no children | A submenu of the global locations (below). |
| **Reset environment** | the location is a dev container or a cloud sandbox | Sets the environment up again, deleting nothing (below). |
| **Update workspace to latest main** | the node has a checkout of its own | `workspace_update::update_workspace_to_main`, unchanged. |
| **Delete sandbox** | the location is a cloud sandbox and the node has one, running or not | Checkpoints uncommitted work, deletes the node's sandbox and forgets it; the next Start makes a new one and restores the checkpoint. While the runner is active, asks first (below). |
| **Delete sandbox without a checkpoint** | with Delete sandbox | The same, discarding uncommitted work. |
| **Sync now** | the node runs under a supervisor | `cloud_sync::sync_now`, saying what the sync shows of the supervisor. |

Removed: Run in the cloud, Run on this machine instead, Move to the cloud,
Stop running in the cloud (replaced by Migrate to…), Keep going (folded into
Resume), Poll now (folded into Check now; a running run already polls on its
own), Stop (folded into Stop now).

While a cloud job of the panel's own runs (`cloud_busy`), the default action
shows what it is doing, disabled, and the menu is empty, as today.

### The runner's state, wherever it runs

The menu is a pure function of one value, tested without a window:

```
RunnerInputs {
    place: Host | DevContainer | Sandbox { exists: bool },
    supervised: bool,              // runner_place == Supervisor
    run: NeverRun | Active { pausing } | HandTurn | Ended(EndedHow) | Done,
    has_checkout: bool, busy: Option<&'static str>,
}
EndedHow: Paused | Stopped | Failed | Request | Budget | Waiting
```

Locally `run` comes from `NodeRunners`. Supervised it comes from the standing
(`tod_core::next_actor`): `Working` (a live heartbeat) is Active; `You::Stopped`
or `You::Failed`, or a `cloud_activity.stopped` reason, is Ended (Budget when
the reason is the budget stop question); a pending wait with nothing stopped is
Ended(Waiting).

### Check now

The same gate check the autopilot's `gate` step makes, with no agent:
`phase::settle_gate` (which reads GitHub itself where a criterion needs it,
records the derived evaluations and a `GateResult` journey event), then, if
the gate is clear, `lifecycle::advance`. The line says "Checked: clear; moved
to `<state>`" or lists the failing criteria. In `pr_draft`, `approved` and
`merged` it also does what `check_github` does today (record or cancel the
GitHub wait), so the standing is current.

It never starts or wakes the runner: a paused node stays paused, an idle one
stays idle. It is only offered while the runner is idle, so it never races a
run's own advance. Supervised, the check runs here against the user's database
(the primary), and the move reaches the supervisor with the next sync; the
supervisor is not woken.

### Migrate to…

A submenu listing every location (`files_places`), the current one marked.
**Only for a node with no children:** the operation itself (in the store,
shared by the panel, the task editor, and any `tod-cli` path) refuses a node
with children with "You cannot migrate a node that has children.", and the
panel does not offer it there. Children are never moved or pinned; doing so
is a possible later extension. Choosing one:

1. If the runner is active, asks first: **Pause** (finish this step, then
   migrate), **Stop now** (interrupt, then migrate), or **Cancel**. The
   migration stays pending until the runner has ended (the local runner's
   state, or no live heartbeat).
2. **Keeps the sessions.** Copies the node's agent session logs and runner
   state out of the old place before it goes (below).
3. Shows the impact dialog (`ui::files_impact::confirm_files_change`) with
   the location the change leaves stale: the node's own
   (`provision::locations_affected_by`, filtered to recipes that differ from
   the chosen location's; with no children it is at most this one). Confirm
   removes them (each branch pushed first; uncommitted work offers Commit,
   Retry, Shell, Discard).
4. Enables the Files capability on the node if it is not on there, then picks
   the location on the node itself (`SetNodeFilesPlace`): an override, never
   the ancestor's choice.
5. If the node leaves a sandbox, its `cloud_nodes` row goes and the change is
   synced.

Nothing is started. The task editor's Files location picker
(`TaskEditor::choose_location`) is the same operation and shares the
implementation.

#### Sessions travel with the node

What exists (`tod_store::fleet::session_log`, `doc/agentd.md` "Moving a
node"):

- After every turn the conversation driver keeps a copy of the agent's
  Claude session log in the app's **mirror**,
  `<data root>/sandbox-sessions/<node>/<project>__<id>.jsonl`: `pull_node` for
  a sandbox, `keep_session` for this machine or a dev container
  (`driver.rs`, after a turn).
- When a conversation resumes in a different place than its last turn,
  `ConversationDriver::bring_session` copies the newest log from this
  machine or the mirror into the new place (`HostRemote`, `ContainerRemote`,
  `SandboxRemote`, `MirrorRemote`), under the new working directory's project
  name, and resumes the same session id. A new sandbox also gets the mirror
  copied in at creation (`restore_node` in `make_sandbox`).
- `crates/tod-core/examples/e2e_move_node.rs` checked it live: one
  conversation kept its session through host → dev container → sandbox →
  host, with the app's own driver. It did not cover a supervised run.

What Migrate adds:

- **Out of a sandbox,** before removal: `session_log::pull_node` from the
  sandbox, because a supervisor's turns run inside the sandbox against its own
  replica and never fill the app's mirror. If the pull fails, Migrate stops
  before removing anything and says why.
- **Runner state.** The autopilot's state (`autopilot/<node>.json`: current
  step and conversation, budget used, loop counters) lives under the run's
  data root: the app's locally, the supervisor's replica root in the sandbox,
  beside its state directory (`/var/lib/tod-supervisor/<node>`, which also
  holds `stop-answers.json`, the budget grants). Migrate copies it: downloaded
  from the sandbox when leaving one, uploaded into the sandbox (before the
  supervisor's first wake) when entering one.
- **Into a sandbox,** the mirror is copied in when the sandbox is made, and
  the supervisor's driver finds the log under the old project name in its own
  Claude directory and moves it to `/workspace/repo`'s (`bring_session`'s host
  source). This is what the code implies; a supervised move is to be checked
  live by extending `e2e_move_node`.

### Reset environment

Sets the environment up again as if starting over, deleting nothing:

- **Dev container:** `resolve_launch_cwd_with` again (branch, submodules) and
  the `tod-cli` relay shim written again (`fleet::cli_relay`).
- **Cloud sandbox:** proxy and credentials, the branch, the relay, the
  supervisor settings (and tod's files, if the sandbox predates the golden
  that carries them); the supervisor is poked only if the runner was active
  or the node was supervised and waiting.

Not offered for a worktree on this machine.

### Delete sandbox

Unchanged in behaviour, on the merged record: checkpoint
(`node_checkpoint::checkpoint_node`, the one path), delete in Blaxel, forget
the location row, and for a supervised node mark its `cloud_nodes` row lost
with `deleted_by_user`, so nothing replaces it until the next Start. "Without
a checkpoint" skips the checkpoint.

**While the runner is active** (both entries), a confirmation first: "This
node is running. Stop it and delete its sandbox?", buttons **Stop and delete**
and **Cancel**. Cancel does nothing. Stop and delete:

- **Local runner:** `NodeRunners::stop_now`, then wait until the daemon
  reports the run ended, then delete.
- **Supervised:** a `stop_now` request (row, marker file, poke), then wait
  until the node's heartbeat ends (the next sync shows it idle, or the
  heartbeat goes stale), then delete. If the supervisor does not stop within a
  bound (the stale-heartbeat window), the line says so and deletes anyway,
  since the sandbox, and the supervisor in it, is what is being deleted and
  the checkpoint has already kept the work. (Proposal; the checkpoint runs
  before the delete in either case.)

The line shows "Stopping, then deleting the sandbox…" meanwhile.

## A one-time upgrade of existing sandboxes

Sandboxes that `make_sandbox` already created in Blaxel before this change
lack the node environment and the orchestrator and Blaxel proxy rules (their
`tod-kind` label is not `node`), and Blaxel does not let tod add environment
variables to an existing sandbox (this code only ever updates the proxy,
`update_proxy`). They keep working for shells, editors and chat until they are
upgraded.

The first Start or Reset environment on such a node replaces its sandbox once,
with no question asked, because nothing is lost: save its session logs
(`pull_node`), checkpoint its uncommitted work (which also carries unpushed
commits), delete it, make the new one through the one creator, and restore
both. The progress line says "Upgrading the node's sandbox so it can run its
autopilot…". If the checkpoint or the log pull fails, nothing is deleted and the
error says why (Delete sandbox without a checkpoint is then the user's way on).

## Sandbox names

One scheme for every new sandbox: **`tod-<slug>-<id6>`**, where `<slug>` is
the node's slug through `sandbox::label_value` (lower case, letters, digits and
dashes) and `<id6>` the first six hex digits of the node's UUID. The slug part
is cut so the whole name is at most 48 characters (the cap
`sandbox::suggested_name` uses today; Blaxel accepts up to 128 per
`validate_name`, but the name also goes into the sandbox's host name, so the
existing margin is kept), and dashes trimmed at the cut. The id makes names
unique across a Blaxel workspace that several users share. **No numeric
suffix**, ever:

- The node's record names a sandbox that exists: that is the node's sandbox,
  reused. Not a conflict.
- Blaxel has a sandbox by the name and the node's record does not name it:
  tod lost track of a sandbox, which is a bug. Creation fails, loudly and
  visibly (the runner line and the log panel): "Cannot make sandbox
  tod-export-totals-3f9a1c for node "Export totals" (3f9a1c…): a sandbox by
  that name already exists, but this node's record does not point at it, so
  tod lost track of it. Check `tod-sandbox list`, the node's Files location,
  and `cloud_nodes` for this node; delete or adopt the sandbox, then try
  again." Nothing is made, renamed or deleted.
- A sandbox by that name still being deleted (status `DELETING`) after a
  Delete sandbox: the creator waits for it to go (as `ensure_node_sandbox`
  waits for a dead one today) and fails with the same kind of message if it
  does not within the wait.

Existing sandboxes keep their names (`node-<slug>` from `node_sandbox_name`,
`tod-<slug>[-n]` from `free_sandbox_name`). Both functions, and
`free_sandbox_name`'s suffix loop, are replaced by the one.

## Deleting the sandbox at `done`

The lifecycle config's "Delete sandbox" setting (`node_lifecycle_config.
delete_sandbox`, `resolve_delete_sandbox`, default on, nearest node up the
tree wins) is kept as stored. It acts automatically: no click, and not tied to
a sync.

- **Who runs it: the daemon (`tod-agentd`).** It is the one writer, it runs
  whether or not the app is open, and every change to a node's lifecycle
  passes through it: a local run, the phase chip, `tod-cli`, and the
  supervisor's changes, which its background sync (`cloud_notify`, started in
  `tod-agentd/src/server.rs`) pulls in as soon as the orchestrator says
  something changed. No user action is involved.
- **When (as built):** 30 s after the daemon starts and then every minute
  (`watch_done_sandboxes`), not on each commit: a pass is one database read
  unless a node is due, so a node that reaches `done` loses its sandbox within
  a minute, and the timer also drives retries and a heartbeat that has ended.
- **What is due:** a node at or past `done` whose `node_files_locations` row
  names a sandbox, whose setting is on, and that nothing works on (no activity
  lease, no live cloud heartbeat: a supervisor that just moved the node to
  `done` still pushes its branch after its last sync). This includes a node
  with a Files sandbox and no `cloud_nodes` row. Only the machine holding the
  location row (the one that made the sandbox) can be the one.
- **How:** the same function as the menu's Delete sandbox, with the
  checkpoint. A failure is reported (log panel, the node's problems) and retried
  after a back-off (1, 5, 15, 60 minutes, then it stops until the daemon
  restarts, saying so), never dropped. `retire_done` then drops the `cloud_nodes` row.
- **Machine off or daemon not running:** nothing happens then; the sandbox
  sits in standby (Blaxel bills almost nothing while it sleeps). The daemon's
  start-up pass catches up: `sync_on_start` brings in what the supervisor did,
  then the pass deletes every due sandbox.

## What does not change

- The supervisor and orchestrator, and how a supervised node waits, wakes,
  syncs and reports (`autonomous-nodes.md`, `orchestrator.md`).
- Locations stay global, picked by the Files capability and inherited.
- Errors are never hidden: a sandbox that cannot be made, a migrate whose
  removal or session copy fails, a final-phase delete that fails, each says
  what and why, and nothing falls back to another place to run.
- Every runner action records a journey `UserAction` with a `Presented`
  snapshot. The action ids change with the enum; analysis keyed on the old
  ids (`RunInCloud`, `KeepGoing`, `PollNow`, `StopTurn`) needs a mapping.

## Open questions

None. Two points are stated as proposals and stand unless the owner objects:
refusing an older build against a newer golden (and refreshing on equal order
with a different stamp), and deleting a supervised node's sandbox after a
bounded wait when its supervisor does not report stopping.

Possible later extension: Migrate on a node with children (moving or pinning
them).
