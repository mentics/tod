# The runner menu, and one sandbox per node

Status: built (November 2026) and live-checked
([live-check-2026-11.md](live-check-2026-11.md); what is still unverified is
listed there and in [implementation.md](implementation.md), "What remains").
This document describes the system as built. The plan that built it is
[implementation.md](implementation.md).

## The problem it solved

A node whose Files location is a cloud sandbox used to have two creators that
made differently configured sandboxes (one for chat, shells and agent turns,
one for the autonomous supervisor), two records of which sandbox it had
(`node_files_locations.sandbox` and `cloud_nodes.sandbox`), a runner menu that
exposed the split, and a final-phase delete that only ran inside an
orchestrator sync. Now there is **one sandbox per node**, called **the node's
sandbox**: the chat panel, shells, agent turns, lifecycle phases and the
supervisor all use it. The **golden** is the sandbox a project's node
sandboxes are forked from (the repository's checkout and the toolchain).

Facts that held before and still shape the code:

- **Local runs outlive the app.** `tod-agentd` hosts them
  (`crates/tod-agentd/src/runners.rs`), so Resume is never needed because the
  app closed, locally or in the cloud.
- **Where the gate is checked.** The gate is checked in one place, the
  autopilot's `gate` step (`Autopilot::gate`, `autopilot/mod.rs`):
  `phase::settle_gate` (derived criteria from `gate::derived`, saved as
  evaluations), then `lifecycle::advance` when clear. Check now calls the same
  two functions (`tod_core::lifecycle::check_now`). No view has a gate button
  or a `LifecycleController::check_gate`: `LifecycleController` only reloads
  the criteria last recorded, and `crates/tod-ui/src/conversation/` is gone.
- **Supervised nodes take requests.** `node_requests` kinds are `run`,
  `fresh_session`, `check_now`, `keep_going`, `pause`, `stop_now` and
  `fresh_budget`, fixed by a SQL `CHECK`.


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

A sandbox location either makes the node's sandbox from an image or forks the
repository's **golden** (`SandboxFrom::Fork("golden")`, resolved by
`Sandboxes::resolve_golden`). There is one golden per repository
(`owner/name`, as `cloud_nodes::normalize_repo` writes it), in
`[blaxel.goldens]` of `sandboxes.toml`, and the location names the repository
(`DevContainerSetting::repo_url`), so `golden` is never a guess among several.
An account-wide `golden = "..."` is refused when the file is read. A golden is never kept per
build, and only moves forward.

What it holds, and what each node's fork still gets:

| In the golden (built once per tod build) | Per node, after the fork |
|---|---|
| Toolchain: git, curl, tar, bash, ssh tools, Node >= 20.10, `claude-agent-acp`, `claude` | Environment: `TOD_NODE`, `TOD_USER`, orchestrator URL, scheduler, workspace (`fork_with_envs`) |
| The repository checkout | Proxy rules and labels: the user's and the node's credentials, Blaxel, orchestrator (`update_proxy`), `tod-kind=node`, `tod-creds` |
| tod's files: the relay, `tod-cli` shim and Linux `tod-cli`, the supervisor, the process, media and skills bundles, the node manifest (a **tod golden**, below) | Nothing to upload: the relay is restarted, and the Ready check fails naming the golden if it lacks this build's files |
| Labels `tod-install` (the install stamp), `tod-schema`, `tod-wire`, `tod-built` (commit time), `tod-kind=golden`, `tod-from=<original>` (or `tod-origin=true` for one built from an image), `tod-repo=<owner>--<name>` (the repository it holds; read from the listing to find a node's repository, so no golden is woken or asked), `keep-when-idle=true` (spared by the workspace's idle prune; a gone golden is built again), `ready` | The branch checked out, submodules on it; supervisor settings; session logs and runner state restored |

**A tod golden, and why the lineage must stay clean.** On Blaxel, updating the
network (proxy) of a sandbox whose lineage holds an *updated fork* redeploys it
from its image and drops every file written since (`scripts/repro-blaxel-network-update-wipe.sh`
reproduces it with `bl` alone; reported to Blaxel). A node's fork needs one
such update, to get its rules, so nothing above it may have been updated, and
a golden **never is**: it is a plain fork of the workspace's own golden (the
*original*, which has a proxy from its creation: a fork inherits none, and the
node's update brings it up), with tod's files installed and no network update.
Verified live: original → tod golden (files, labels) → node fork + one
`update_proxy` keeps its files and boot, and its credentials inject through the
sandbox's local proxy. Two traps found doing it, both in `refresh_with`:
the golden is forked with **no environment** (`fork_with_envs` on it makes every
node fork lose its files once tod's files are in it), and it is labelled
**once, before** tod's files go in (`label_golden`; a label update after the
install has the same effect), so nothing updates it afterwards. A node's fork
of a tod golden (`tod-from` set; it holds no proxy routes, which `fork_golden`
accepts for exactly that case) uploads no files. The original is named by
`[blaxel.golden_sources]` (a name or `prefix*`, recorded by the first refresh
from what `[blaxel.goldens]` said then); `tod-sandbox golden <owner/name>
<workspace golden or prefix*>` points a repository back at one, which is also
how a golden an older tod made (a fork that was updated, with no `tod-from`)
is replaced: `origin_of` refuses forking it, naming that command. An
original built from an image (`build_golden`) is its own original and is never
deleted by a refresh.

**A repository with no golden** (live check Pass 7, gap 15) is built, not
forked: `golden::decide_source` (pure) says `Build` when the repository has no
entry of its own, and `build_golden` makes a fresh
sandbox from the account's default image with a proxy holding the user's GitHub
token, installs tod's dependencies and files, clones `https://github.com/<repo>.git`
into `/workspace/repo` (`golden::clone_script`; submodules recursively), and
labels it like a refreshed one. A fork of another repository's golden is never
the answer, and a failure deletes what it made and says why.

**Refreshing.** `tod-sandbox golden refresh <owner/name>`
(`cloud_sync::golden::refresh_golden`), for a repository that has a golden, forks
the **original** (not the previous tod golden), labels the fork with this
build's stamp, `tod-kind=golden` and `tod-from`, checks what a fork needs
(`provision::require_fork_prerequisites`), installs tod's files, points the
repository at it, and deletes the old tod golden (never the original). One
refresh runs at a time per data root (`golden-refresh.lock`). The app does the
same before making a sandbox when the golden is older, or when the workspace's
golden it was forked from has been replaced (`ensure_golden_current`, through
`SandboxHost::current_golden`), saying so in the progress. The daemon checks
every ten minutes in the background (`tod-agentd`'s golden check,
`ensure_all_current`), so a node made later forks at once; `scripts/deploy.sh`
refreshes each repository's golden after a deploy. A golden with no `tod-install` label (made by hand) counts as
older than any build.

**Older build meets newer golden.** The install stamp is a hash, so the
golden also carries an order: the sync contract (`schema`, then `wire`) and
the build's commit time (`tod_sandbox::golden::plan`).

- same stamp: use it;
- the golden is older: refresh it forward;
- the golden is newer: **refuse**, naming both (the repository, install,
  schema), and never refresh it back, since that would break the newer users'
  sandboxes: a fork would run the newer supervisor and `tod-cli` against this
  app's older schema and sync contract;
- same order, different stamp (two builds of one commit with local changes):
  treated as forward and refreshed, saying so.

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
| **Adopt existing sandbox** | not done, and the node's old records (`cloud_nodes`, a legacy `cloud-sync.json` record) name a sandbox its Files location does not (`cloud_sync::adopt::adoptable`) | Records that sandbox as the node's location after checking it is the node's and answers (below, "Adopting"). |
| **Check now** | the node is idle and in any state but `done` | Runs the current state's gate check now (below). Hidden while the runner is active. |
| **Migrate to…** | not done, and the node has no children | A submenu of the global locations (below). |
| **Reset environment** | the location is a dev container or a cloud sandbox | Sets the environment up again, deleting nothing (below). |
| **Update workspace to latest main** | the node has a checkout of its own | `workspace_update::update_workspace_to_main`, unchanged. |
| **Delete sandbox** | the location is a cloud sandbox and the node has one, running or not | Checkpoints uncommitted work, deletes the node's sandbox and forgets it; the next Start makes a new one and restores the checkpoint. While the runner is active, asks first (below). |
| **Delete sandbox without a checkpoint** | with Delete sandbox | The same, discarding uncommitted work. |
| **Sync now** | the node runs under a supervisor | `cloud_sync::sync_now`, saying what the sync shows of the supervisor. |

The menu is built by `runner_menu::menu` (`unified/panels/runner_menu.rs`,
`task.rs` is its one caller). While a cloud job of the panel's own runs
(`cloud_busy`), the default action shows what it is doing, disabled, and the
menu is empty. A node with entries but no default action (a run that ended
only to wait) shows a disabled "Actions" button whose caret opens the menu.

Menu entries from before this work have no counterpart under their old names;
see "Historical" at the end for where each went.

### The runner's state, wherever it runs

The menu is a pure function of one value, tested without a window:

```
RunnerInputs {
    place: Host | DevContainer | Sandbox { exists: bool },
    supervised: bool,              // runner_place == Supervisor
    run: NeverRun | Active { pausing } | HandTurn | Ended(EndedHow) | Done,
    has_checkout: bool, has_children: bool,
    adoptable: bool,               // old records name a sandbox the location lacks
    busy: Option<&'static str>,
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
  source). Live (Pass 3, 5c): the logs were restored in the new sandbox and the
  agent ran there; that the same session id then resumes was not compared, and
  neither was resuming it at a host (5b), where the run stopped at "Claude
  Code is not signed in".

### Reset environment

Sets the environment up again as if starting over, deleting nothing:

- **Dev container:** `resolve_launch_cwd_with` again (branch, submodules) and
  the `tod-cli` relay shim written again (`fleet::cli_relay`).
- **Cloud sandbox:** proxy and credentials, the branch, the relay, the
  supervisor settings, and tod's files when the sandbox does not answer with
  this build's manifest (`cloud_sync::ensure_supervisor`, the one path that
  provisions a sandbox; a recorded sandbox that is gone or dead is forgotten
  first and made again). The supervisor is poked only if the node is
  supervised and waiting on someone. Refused while the runner is active
  ("Stop it first").

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
  the checkpoint has already kept the work (the checkpoint runs before the
  delete in either case). A local run that does not end within 30 seconds
  refuses the delete with an error.

The line shows "Stopping, then deleting the sandbox…" meanwhile.

## A one-time upgrade of existing sandboxes

Sandboxes that an earlier `make_sandbox` created in Blaxel, before it made
every sandbox able to host the supervisor, lack the node environment and the orchestrator and Blaxel proxy rules (their
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

- The node's Files location names a sandbox that exists: that is the node's
  sandbox, reused. Not a conflict.
- Blaxel has a sandbox by the name and the node's Files location does not name it:
  tod lost track of a sandbox, which is a bug. Creation fails, loudly and
  visibly (the runner line and the log panel): "Cannot make sandbox
  tod-export-totals-3f9a1c for node "Export totals" (3f9a1c…): a sandbox by
  that name already exists (<status>), but this node's Files location does
  not point at it, so tod lost track of it. Check `tod-sandbox list` and the
  node's Files location; delete or adopt the sandbox, then try again. Nothing
  was made, renamed or deleted." (Seen live, Pass 3, check 3.)
- A sandbox by that name still being deleted (status `DELETING`) after a
  Delete sandbox: the creator waits up to two minutes for it to go and fails
  with the same kind of message if it is still there.

The one function is `provision::sandbox_name` (decided by `decide_name`,
settled by `choose_sandbox`); no numeric suffix exists anywhere. Sandboxes
made before it keep the names they had (`node-<slug>`, `tod-<slug>[-n]`) and
are adopted as recorded.

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

## Adopting a sandbox an older build recorded

A node with a live `cloud_nodes` row (this client's, not lost, not deleted by
the user) or a legacy `cloud-sync.json` record naming a sandbox, whose
resolved Files are a cloud sandbox, and no location row, is **adoptable**
(`cloud_sync::adopt`). Adoption records the location naming that sandbox, with
the checkout path and branch found by asking the sandbox, after checking it is
the node's: Blaxel has it and it can run, its `tod-kind` is `node` or
`node-base`, its `TOD_NODE` (when it has one) is this node, and its relay
answers as a node's. Nothing about the checkout is changed. Two entries, one
implementation: the runner menu's **Adopt existing sandbox** (the user's
form), and once at the app's start (`lost::check`, `Check::Full`) the
automatic form, which adopts only what is unambiguous. A sandbox Blaxel no
longer has is not adopted and nothing is written: the panel says so, and Start
makes a new one. Until adopted, the panel says the node's Files location
records no sandbox.

## What does not change

- The supervisor and orchestrator, and how a supervised node waits, wakes,
  syncs and reports (`autonomous-nodes.md`, `orchestrator.md`).
- Locations stay global, picked by the Files capability and inherited.
- Errors are never hidden: a sandbox that cannot be made, a migrate whose
  removal or session copy fails, a final-phase delete that fails, each says
  what and why, and nothing falls back to another place to run.
- Every runner action records a journey `UserAction` with a `Presented`
  snapshot. The action ids follow the `RunnerAction` enum.

## Open questions

None. Possible later extension: Migrate on a node with children (moving or
pinning them).

## Historical

The menu before this work, for reading older journeys, audits and sessions:
"Run in the cloud", "Move to the cloud" and "Reinstall and restart" started a
supervisor (now Start, Resume and Reset environment); "Run on this machine
instead" and "Stop running in the cloud" (now Migrate to...); "Keep going"
(now Resume; the budget stop question still has a "Keep going" answer, which
Resume also gives); "Poll now" (now Check now; a running run polls on its
own); "Stop" (now Stop now). Journey action ids from then (`RunInCloud`,
`KeepGoing`, `PollNow`, `StopTurn`) have no equivalent in the current enum.
Before stage B a second creator (`cloud_sync::ensure_node_sandbox`) made the
supervisor's sandbox and `cloud_nodes.sandbox` was the second record; both
are gone.

## A pruned sandbox is replaced by itself

Cofactr's prune deletes node sandboxes asleep over an hour, which a node
waiting on GitHub is. When a wake or feedback finds the sandbox gone, the
orchestrator marks the node lost and the app or daemon (`cloud_sync::lost`)
makes a new one through `ensure_supervisor` and pokes it, so the run continues
from its saved state. Only a node meant to run is replaced: one whose run row
is `Stopped` (paused, stopped, failed, needs someone) or `Done` is not, and a
sandbox the user deleted waits for Start. A failed replacement is tried three
times ten minutes apart, then the node is recorded failed with the last error.

The "deleted by the user" flag (`cloud-sync.json`, `deleted_by_user` plus
`deleted_sandboxes`) means exactly: the user chose Delete sandbox for that
sandbox. Retiring a done node's sandbox does not set it. `heal_deleted_flags`
drops a flag whose cloud row now names another sandbox, and a legacy flag with
no sandbox name for a node whose run row says it is meant to run. Replacement
tries are kept in `replace_tries` in the same file, so a restart does not grant
them again.
