# The runner menu, and one sandbox per node: implementation plan

The design is [design.md](design.md). **Stage A** rebuilds the runner menu and
adds Migrate to…, on the records as they are; **Stage B** merges the two
sandbox creators and the two records, moves tod's files into the golden, and
makes the `done` delete automatic. Each item lands on its own with its tests
green (`cargo nextest run -p <crate> …`, ~120 s timeout) and
`cargo check --workspace --all-targets` clean.

## Before anything: land the checkbox as it is

**A0.** Commit the uncommitted "Delete sandbox" lifecycle-config work (schema
v92, `lifecycle_config::{add_delete_sandbox, resolve_delete_sandbox,
set_delete_sandbox, FINAL_PHASE}`, `OutlineMutation::SetNodeLifecycleDeleteSandbox`,
the editor's "Done" block, `lost::delete_sandboxes_at_final_phase` in
`sync_once`). Its trigger and record move in B4; the stored setting does not
change. Schema numbers below start at v93.

## Stage A: the runner menu and Migrate to…

| Item | Depends on | Touches |
|---|---|---|
| A1 menu model | A0 | new `unified/panels/runner_menu.rs` |
| A2 runner place | A0 | new `tod-core/src/runner_place.rs`, `tod-agentd/src/runners.rs` |
| A3 Check now | A0 | `tod-core/src/lifecycle/`, `autopilot/github_wait.rs` |
| A4 supervised pause, stop, fresh budget | A0 | `tod-store/src/node_requests.rs`, schema, `tod-supervisor`, `cloud_sync.rs` (request functions) |
| A5 Migrate: store, sessions, editor | A0 | `fleet/places.rs`, `fleet/store.rs`, `fleet/session_log.rs`, `ui/files_impact.rs`, `views/task_edit/mod.rs` |
| A6 Reset environment | A0 | new `tod-core/src/environment_reset.rs`, `cloud_sync.rs` (`ensure_node_sandbox`) |
| A7 Delete sandbox on the Files sandbox | A0 | `cloud_sync/lost.rs` |
| A8 wire the panel | A1–A7 | `unified/panels/task.rs`, `views/cloud_node.rs` |
| A9 words and docs | A8 | `next_actor.rs` strings, docs |

**Parallel:** A1, A2, A3, A5 and A7 touch disjoint files. A4 and A6 both edit
`cloud_sync.rs` (different functions): run them one after the other, or in
parallel expecting a merge. **Sequential:** A8 after A1–A7 (land A2 with A8:
A2 alone makes the old `StartHere` fail for sandbox nodes); A9 last.

### A1. The menu as a pure function

- New `crates/tod-ui/src/unified/panels/runner_menu.rs`: `RunnerInputs`,
  `RunState`, `EndedHow`, `Place` (design, "The runner's state"); the new
  `RunnerAction` (`Start, Resume, ResumeFreshBudget, RunFresh, Pause, StopNow,
  CheckNow, MigrateTo, ResetEnvironment, UpdateWorkspace, DeleteSandbox,
  DeleteSandboxAnyway, SyncNow`) with labels; `RunnerActions`; `fn
  menu(&RunnerInputs) -> RunnerActions`. No GPUI, no store.
- Tests: one per row of the design's table, and the rules: after
  `EndedHow::Budget` no Resume, Resume with a fresh budget the default, Run with
  a new session present; no Check now while Active or HandTurn; no Delete
  sandbox for a host or dev container, but Delete sandbox shown while Active;
  no Migrate to… for a node with children (`RunnerInputs.has_children`); no
  Reset for a host worktree; Sync now only when supervised; when done only
  Delete sandbox, Update workspace and Sync now (where they apply); Stop now
  the default for a hand-started turn; an empty menu while busy.
- Done: compiles and passes; nothing calls it yet.

### A2. Where the runner runs

- New `crates/tod-core/src/runner_place.rs`: `RunnerPlace { Local, Supervisor }`,
  `of(fleet, node)` from `resolve_files_for_node(..).runs_in_sandbox()`; no
  Files is `Local`. Store reads only (UI-thread safe).
- `crates/tod-agentd/src/runners.rs`: `Runners::start` refuses a `Supervisor`
  node, naming why; `resume_saved` and the reconciler (`decide`) never start
  one, and report a saved local run of a node that has since moved to a
  sandbox instead of resuming it.
- Tests: `of` for host, dev container, sandbox, inherited, no Files; the daemon
  refuses and does not restart a sandbox node.

### A3. Check now is the gate check

- `crates/tod-core/src/lifecycle/mod.rs` (or `lifecycle/check_now.rs`):
  `check_now(fleet, node, data_root) -> Result<CheckNow>`, `CheckNow { gate:
  GateCheck, advanced: Option<&'static str>, github: Option<GithubCheck> }`:
  `phase::settle_gate`, then `lifecycle::advance` when `gate.clear()`, the same
  pair `Autopilot::gate` calls; in `pr_draft`, `approved`, `merged` also
  `autopilot::github_wait::check_github`. Refuses (an error, not a no-op) when
  the node has an activity lease of kind `run`: the caller only offers it idle,
  and this keeps a `tod-cli` or second window honest.
- `autopilot/github_wait.rs`: the old `check_now` goes (its one caller is
  `task.rs`).
- Fix the stale `LifecycleController::check_gate` mentions in
  `views/lifecycle_control.rs`'s module comment here or in A9.
- Tests (`interview::test_support::fixture()`,
  `pr_readiness::set_feed_override`): clear advances; failing returns the
  criteria and does not move; a leased node is refused; `pr_draft` with a
  waiting feed records the wait; a paused runner's outcome is untouched.

### A4. Pause, Stop now and a fresh budget for a supervised node

- `crates/tod-store/src/node_requests.rs`: kinds `pause`, `stop_now`,
  `fresh_budget`. The `kind` column has a `CHECK`, so this is a table rebuild:
  schema **v93** (`CURRENT_USER_VERSION`), idempotent, rows kept. The table is
  synced: bump the sync contract (`sync::contract`) so an older orchestrator is
  told to redeploy.
- `crates/tod-supervisor/src/lib.rs`: `Hook::at` stops with
  `Outcome::Stopped { reason: "the user paused it" }` on a pending `pause`
  (consumed); `Hook::watch` cancels the turn on a `stop-now` marker file (as
  `fresh_marker` does), and `at` consumes the row; `answers::before_wake_with`
  grants a budget on `fresh_budget` (the same `granted` a "Keep going" budget
  answer gives). Neither `pause` nor `stop_now` counts as a request to run.
- `crates/tod-core/src/cloud_sync.rs`: `wake_node_requesting` handles them:
  `pause` records, syncs and pokes; `stop_now` also uploads the marker
  (`SUPERVISOR_STATE_ROOT/<node>/stop-now`); `fresh_budget` like `keep_going`.
- Tests: the new kinds and the migration keeping old rows; supervisor hook
  tests for pause at a boundary and stop mid-turn; `answers` grants on
  `fresh_budget`; `next_actor` shows a paused supervised node as `You::Stopped`
  and a spent budget recognisably (A1's `EndedHow::Budget`).
- Risk: an older supervisor leaves the rows pending; the contract bump says so.

### A5. Migrate to…: store, sessions, editor

- `crates/tod-store/src/fleet/places.rs`: `migrate_impact(fleet, node,
  place_id)`: `provision::locations_affected_by`, keeping those whose recipe
  differs from the place's `recipe_key()` (at most the node's own);
  `migrate(fleet, node, place_id)`: enable Files on the node if it is not its
  own (`OutlineMutation::EnableCapabilities`), then
  `FleetMutation::SetNodeFilesPlace`, one flush. **Both refuse a node with
  children** with "You cannot migrate a node that has children." (an
  `outline` child count), so the panel, the task editor and any `tod-cli`
  path get the same refusal. Moving or pinning children is a later extension,
  not planned.
- `crates/tod-store/src/fleet/session_log.rs` (or a small `fleet/migrate.rs`):
  `keep_node_state(fleet, node)` before removal: for a sandbox location,
  `pull_node` (Claude logs into the mirror) and download the supervisor's
  `autopilot/<node>.json` and `stop-answers.json` into
  `<data root>/autopilot/` (confirm the replica root's path in
  `tod-supervisor/src/replica.rs` first); for host and dev container nothing,
  since the driver keeps the mirror after every turn. And
  `place_node_state(fleet, node)` for a node entering a sandbox: upload the
  autopilot state into the supervisor's state before its first wake (the
  logs already go in through `make_sandbox`'s `restore_node`). A failure stops
  the migrate, naming what.
- `crates/tod-store/src/fleet/store.rs`: `location_blocker` also names an
  activity lease of kind `run` or a live `cloud_activity` heartbeat, so no
  removal deletes a sandbox under a running runner.
- `crates/tod-ui/src/ui/files_impact.rs`: `confirm_migrate(window, cx, fleet,
  paths, node, place, on_done)`: `keep_node_state` off the UI thread, then the
  impact dialog, then `migrate`, then `on_done`.
- `crates/tod-ui/src/views/task_edit/mod.rs`: `choose_location` and
  `write_location` go through `confirm_migrate`; for a node with children the
  refusal shows as the editor's toast. (Picking a location on a parent that
  has no Files location yet, to set what its children inherit, is a different
  operation; it stays the plain `SetNodeFilesPlace` with its impact dialog, as
  today. Only Migrate refuses.)
- `cloud_sync/lost.rs`: `stop_running_in_cloud` becomes the step that drops
  the `cloud_nodes` row and syncs when a node leaves a sandbox; `confirm_migrate`
  calls it through a core wrapper.
- Tests: `migrate_impact` (own pick, inherited pick, same recipe); `migrate`
  and `migrate_impact` refuse a node with children with the exact message;
  `migrate` enables Files and sets the place; `location_blocker`
  sees a run lease; `keep_node_state` against a fake `Remote`
  (`session_log`'s test remotes). Live: extend
  `crates/tod-core/examples/e2e_move_node.rs` with a supervised step (start
  supervised in a sandbox, migrate to host, the same session id resumes).
- Risk: enabling Files and picking the place are two mutations; a failure
  between them leaves Files on with the inherited place (what it resolved to
  already). Say so in the error.

### A6. Reset environment

- New `crates/tod-core/src/environment_reset.rs`: `reset(fleet, node,
  progress)`: dev container → `provision::resolve_launch_cwd_with` and the relay
  shim written again (`fleet::cli_relay`); cloud sandbox →
  `ensure_node_sandbox` with a new `wake: bool` (true only when the runner was
  active or the node supervised and waiting); a host worktree is an error.
- `cloud_sync.rs`: `ensure_node_sandbox(…, wake)`; existing callers pass `true`.
- Tests: dispatch and the host refusal. The sandbox path is a live check.

### A7. Delete sandbox acts on the Files sandbox

- `cloud_sync/lost.rs`: `delete_node_sandbox[_anyway]` take the node's
  sandboxes from both records (location and `cloud_nodes`, deduplicated:
  normally one), refuse while the runner is active (A5's blocker: the caller
  stops it first, below), checkpoint with
  `fleet::node_checkpoint::checkpoint_node` (drop
  `cloud_sync::checkpoint_before_delete`), delete each, forget the location
  (`DeleteFilesLocation`), and mark a `cloud_nodes` row lost with
  `deleted_by_user` as today. Neither record naming a sandbox is the error.
- Stop then delete, in core (`lost::stop_and_delete_node_sandbox(fleet, node,
  checkpoint: bool, progress)`), for the panel's "Stop and delete": local →
  the daemon's `RunnerStop`, then wait for its run to end; supervised → the
  A4 `stop_now` request, then sync until the heartbeat ends, bounded by the
  stale-heartbeat window, after which it says so and deletes (design.md,
  "Delete sandbox"); then the delete above.
- Tests: only a location (today's failure); only a row; both naming one; a
  failed checkpoint deletes nothing; refused while a `run` lease is held;
  stop-and-delete waits for the run to end (a local fake run).

### A8. Wire the panel

- `crates/tod-ui/src/unified/panels/task.rs`: the old `RunnerAction`,
  `offers_check_now`, `runner_actions_for_place` and the `cloud_stop_answered`
  case go. `load_runner` returns what `RunnerInputs` needs (`runner_place::of`,
  whether a sandbox exists, a checkout, the list of `files_places`), off the UI
  thread; `runner_actions` builds `RunnerInputs` and calls `runner_menu::menu`.
  `perform` dispatches by place: Start, Resume, Run with a new session,
  Resume with a fresh budget to `NodeRunners` or `views::cloud_node::{run_in_cloud,
  request}`; Pause and Stop now to `NodeRunners` or the A4 requests, Stop now
  to `AgentRuns::stop` for a hand-started turn; Check now to A3, off the UI
  thread, with the result on the line; Migrate to… as a submenu, with the
  Pause / Stop now / Cancel prompt when active and a pending migration that
  calls `confirm_migrate` once the runner has ended; Reset environment to A6;
  Delete sandbox (both entries) to A7, and while the runner is active first a
  dialog, "This node is running. Stop it and delete its sandbox?" with **Stop
  and delete** (A7's stop-then-delete, the line saying "Stopping, then deleting
  the sandbox…") and **Cancel** (nothing); Sync now as today. Every action keeps
  `record_action` with `presented`.
- `views/cloud_node.rs`: drop `stop_running`; add `reset_environment`,
  `pause`, `stop_now`; the "Run in the cloud" and "Reinstall and restart" wording is
  removed from its messages.
- `unified/runners.rs`: `poll_now` loses its UI caller; keep the daemon
  command (a running run still polls on its own) or remove it with
  `RunnerPollNow` in `tod-agentd-client`.
- Tests: replace `a_cloud_node_stopped_by_the_users_answer_offers_keep_going`
  and the menu asserts in `runner_line_follows_agent_runs` (Resume for a
  stopped supervised node; Stop now default for a hand turn); delete
  `check_now_is_offered_for_a_state_that_waits_on_its_pull_requests`; Migrate
  while running asks first and migrates only after the run ends; Delete
  sandbox while running shows the confirmation and Cancel changes nothing.
- Done: the menu matches the design's table in the running app (`--agent
  mock` with a host location and a dev container; a sandbox location in the
  live root, `doc/live-verification.md`).

### A9. Words and docs

- `crates/tod-core/src/next_actor.rs`: the reasons that still name the removed "Run in the cloud" entry
  (around lines 598–612) say "Start" or "Resume"; tests follow.
  `CloudStatus::describe` likewise.
- Docs: `doc/ui/task-panel.md` (runner table, "Where it runs", "Closing the
  app"), `doc/cloud-sandboxes/autonomous-nodes.md` ("Where a node runs", "A new
  session on request"), `orchestrator.md`, `setup.md`, `doc/files-locations.md`
  (Known gaps), `doc/lifecycle/next-actor.md`, `doc/agentd.md` ("Moving a
  node": Migrate is the user's way to move), `doc/architecture.md` ("can be
  **run in the cloud**"), `.claude/CLAUDE.md` (Unified view "hands it to the
  cloud"; the `LifecycleController` / conversation-view Advance description,
  which is stale), `.claude/skills/live-verify/SKILL.md` (the runner buttons).
- No `tod-cli` syntax changes, so `doc_sync` and `context_recipes` are
  unaffected.

## Stage B: one creator, one record, the golden, automatic delete

| Item | Depends on | Touches |
|---|---|---|
| B1 one golden per project, carrying tod's files | Stage A | `tod-sandbox/src/node.rs`, `tod-sandbox/src/config.rs`, `tod-sandbox-cli/src/main.rs`, `fleet/sandbox.rs`, `tod-core/build.rs` |
| B2 one creator | B1 | `fleet/provision.rs`, `fleet/sandbox.rs`, `cloud_sync.rs`, `cloud_sync/lost.rs`, `cloud_sync/refresh.rs` |
| B3 one record | B2 | `cloud_sync.rs` (`record_cloud_node`), `cloud_sync/lost.rs`, `fleet/provision.rs` |
| B4 automatic delete at `done` | B3, A7 | new `tod-core/src/sandbox_retire.rs`, `tod-agentd` |
| B5 adopt old records | B3 | `cloud_sync/lost.rs` |
| B6 docs | B1–B5 | docs |

Sequential, except B5 alongside B4.

### B1. The golden carries tod's files

- **One golden per project.** `tod_sandbox::config`: `Account::golden` (one
  name per account) becomes a map from repository (`owner/name`, normalized as
  `cloud_nodes::normalize_repo` does) to the golden's name, with the old
  single value read as the golden of every repository until one is set (a
  migration of `sandboxes.toml`, said in the log). `tod-sandbox golden
  [<repo>] [<sandbox>]` shows or sets it; `Sandboxes::resolve_golden` takes the
  node's repository. Never one golden per build.
- `tod-sandbox golden refresh [<repo>]` (`crates/tod-sandbox-cli/src/main.rs`,
  logic in `tod_sandbox::node`): fork the project's golden, check its
  prerequisites (`provision::require_fork_prerequisites`), run `node::install`
  (relay, shim, Linux `tod-cli`, supervisor, bundles, manifest, Claude adapter),
  label it `tod-install=<refresh::install_stamp()>`, `tod-schema`, `tod-wire`
  (`sync::contract::Contract`) and `tod-built=<commit time>`, mark it `ready`,
  point the project's golden at it, then delete the old one once nothing is
  forking from it (forks are independent of their source once made).
- A library entry for the app: `ensure_golden_current(sandboxes, bx, repo,
  progress)`, run before any fork: same stamp → use; golden older (by schema,
  wire, then commit time) → refresh forward, saying so; same order and a
  different stamp → refresh, saying so; golden **newer** → refuse with the
  error in design.md ("Older build meets newer golden"), naming the
  repository and both stamps and schemas. Needs the commit time in the build
  (`tod-core/build.rs` already stamps the commit; add its time, keeping the
  rule that dev builds do not track HEAD).
- Delete the dead `node::ensure_base` and `node::fork` (no callers).
- Tests: the ordering and refusal as a pure function over labels (equal,
  older, newer, same order different stamp); the config migration; the
  refresh's steps against a fake Blaxel if `tod_sandbox` has one, otherwise a
  live check documented in `doc/cloud-sandboxes/setup.md`.

### B2. One creator

**Status: done on branch `worktree-agent-a8ab91133663eaf0b` (not live-verified).**
What landed, and where it departs from the text below:

- **Layering.** `ensure_golden_current`, the orchestrator user, the install
  files, the node's repository URL and the per-node bookkeeping stay in
  `tod-core`; `tod-store` states what it needs as the `SandboxHost` trait
  (`fleet/sandbox_host.rs`), `tod-core` implements it
  (`cloud_sync/host.rs`), and each program that can make a sandbox registers
  it once (`register_sandbox_host`: the app, `tod-agentd`, and
  `ensure_supervisor`; a program that did not gets a loud error from
  `make_sandbox`). A process-wide registration, not a parameter, because the
  callers of `resolve_launch_cwd` are in every launch path. The alternative,
  moving `TodInstallPaths`/`MediaPaths`/`SkillsPaths`, the sync contract and
  the build stamp into `tod-store`, would put install-file discovery in the
  persistence layer for no other use.
- **Creator.** `make_sandbox` chooses the sandbox (`choose_sandbox`:
  `sandbox_name`, `decide_name`), requires the orchestrator before anything is
  made, records the intent and then the location, forks from the golden
  (`SandboxHost::current_golden`) or creates from an image
  (`Sandboxes::runtime_image` wraps a non-Blaxel image; bootstrap and
  `node::install` follow), runs the node's checkout (`node::checkout`, with its
  auth retry), submodules, starts the relay (`node::start_relay`, which no
  longer starts the supervisor) and requires the relay to answer with this
  build's manifest. A fork installs nothing. A failure after the sandbox
  exists deletes it and forgets the location. `Sandboxes::create_with_proxy`
  is gone (`Sandboxes::create` remains for `tod-sandbox create`/`fork`, which
  make workspace sandboxes by hand), as are `node::provision`, `node::start`,
  `free_sandbox_name`, `node_sandbox_name`, `live_cloud_sandbox`, and the
  `e2e_env_proxy` / `e2e_env_recreate` examples that called it.
- **`ensure_supervisor`** (`cloud_sync.rs`) replaces `ensure_node_sandbox` and
  is what Start, a lost sandbox, a reinstall and Reset environment call:
  `provision::retire_unusable_sandbox` (forgets a gone or dead sandbox; the
  one-time upgrade of a sandbox whose `tod-kind` is not `node`: logs pulled,
  work checkpointed, deleted, nothing deleted if either fails), then
  `resolve_launch_cwd_with`, then `supervisor_state`: a sandbox that was
  already there and does not answer with this build's manifest, or was given an
  older install, gets this build's files (`install_into_fork`) and a restarted
  relay; one made just now never does, and not answering is an error naming the
  golden.
- **One proxy path.** Every node sandbox carries `tod-creds`, the fingerprint
  of all its proxy rules (`node::node_creds_fingerprint`); `refresh_sandbox_proxy`
  compares and updates against the same rules, and `lost::refresh_credentials`
  is a thin wrapper over it (no more delete-and-recreate for an autonomous
  node).
- **Not done here:** one record (B3): the creator still reads both the
  location and `cloud_nodes.sandbox` as "the node's record"; a sandbox made by
  `make_sandbox` is not yet written into `cloud_nodes` until `record_cloud_node`.
  No fake Blaxel exists, so the create body, the name scheme and the
  collision rule are tested as pure functions (`decide_name`,
  `sandbox_name`, `supervisor_state`), and the creation itself waits for live
  verification.

- `fleet/provision.rs::make_sandbox` is the only creator, in `tod-store`, and
  every sandbox it makes can host the supervisor: from the golden,
  `ensure_golden_current`, then `node::fork_golden` (node environment, proxy
  rules with the orchestrator and Blaxel, labels `tod-kind=node`) and only the
  per-node setup (design, "The golden sandbox"): branch, settings, session logs,
  checkpoint, relay start. From an image, `node::create` and the full
  `node::provision`. It installs tod's files on a fork never; it starts the
  supervisor only when asked (`wake`). The location is recorded as now.
- **Names:** one function, `sandbox_name(slug, node_id)` →
  `tod-<slug>-<6 hex>` at most 48 characters, replacing `free_sandbox_name`
  (and its suffix loop) and `node_sandbox_name`. Before creating: the node's
  record names a sandbox that exists → reuse it; Blaxel has the name and the
  record does not name it → the "lost track" error in design.md ("Sandbox
  names"), nothing made; a `DELETING` one by that name → wait, then the same
  error if it is still there. Delete the `"already exists"` retry in
  `make_sandbox`.
- No orchestrator → the loud error in design.md ("The model"), before anything
  is made.
- Layering: the orchestrator's URL is reachable from `tod-store`
  (`Sandboxes::url`); the orchestrator **user name** comes from
  `cloud_sync::resolve` in `tod-core`. Move that read into `tod-store`, and the
  bundle discovery (`TodInstallPaths`, `MediaPaths`, `SkillsPaths`) is needed
  only for an image source: pass it in, or move those path resolvers down.
  Check both before starting.
- `cloud_sync.rs`: `ensure_node_sandbox` becomes `ensure_supervisor(fleet,
  root, node, wake, progress)`: `provision::resolve_launch_cwd_with` (makes or
  reuses through `make_sandbox`), supervisor settings, the `Stage::Ready`
  probe, the poke when `wake`. `existing_sandbox_name`, `live_cloud_sandbox`,
  the second create/fork path and the second failure cleanup go.
- **One-time upgrade of existing sandboxes** (made by the old `make_sandbox`;
  label not `tod-kind=node`): `ensure_supervisor` replaces them once, as
  design.md says ("A one-time upgrade of existing sandboxes"): `pull_node`, checkpoint, delete, make,
  restore; a failed checkpoint or pull deletes nothing and says why.
- `lost.rs::replace`, `refresh_credentials` (one path now:
  `refresh_sandbox_proxy`), `refresh::reinstall`: through `ensure_supervisor`.
- Tests: the create body for a node (env, rules, labels) in `tod-sandbox`; the
  name scheme (length cap, id suffix, trimmed dashes); a name taken by a
  sandbox the record does not name fails with the "lost track" error and
  makes nothing; a recorded, existing sandbox is reused; an existing sandbox from the old creator
  sandbox is replaced and its checkpoint restored (fake Blaxel or live);
  `cloud_sync/tests.rs` provisioning tests moved.
- Done: `grep -n 'create_with_proxy\|node::create\b\|fork_golden' crates/` finds
  one call site of each creation path, in `make_sandbox`.

### B3. One record

- `node_files_locations.sandbox` is the record. `record_cloud_node` reads it
  and writes `cloud_nodes.sandbox` from it (the column's only writer).
  Everything in the app that acts on the sandbox (`wake_node_requesting`,
  `conversation::handoff`, `db_broker`, `refresh`, A7's delete) reads the
  location. The orchestrator keeps reading `cloud_nodes.sandbox`.
- Every change of a supervised node's location updates the copy in the same
  call (`forget_lost_sandbox`, `refresh_sandbox_proxy`'s recreate,
  `remove_location`, lost replacement). A sync-time check reports a copy that
  differs from its location (an error naming both), never a silent fix.
- Tests: the copy follows a replacement, a recreate and a removal; the
  mismatch check fires.
- Risk: another machine of the same user has no location row for a node made
  elsewhere and must not act on its sandbox (the `owner_client` check covers
  `cloud_nodes`; A7 and B4 only act through the location row).

### B4. Automatic delete at `done`

- New `crates/tod-core/src/sandbox_retire.rs`: `due(fleet) -> Vec<node>` (a
  location with a sandbox, lifecycle at or past `FINAL_PHASE`,
  `resolve_delete_sandbox` on, no `run` lease, no live heartbeat) and
  `retire_due(fleet)`: A7's delete for each, back-off per failed node,
  failures reported through `logging::buffer::record` and the problems
  channel. Replaces `lost::delete_sandboxes_at_final_phase`; its call in
  `sync_once` goes.
- `crates/tod-agentd`: call `retire_due` on its own thread (it blocks on the
  network): at start (after `sync_on_start` has pulled), whenever a commit the
  daemon writes or applies from a sync moves a node into `done`, and on a slow
  timer (a few minutes) for retries and for a heartbeat that has since ended.
- Tests: a done node with the setting on and a location is retired with no
  sync and no `cloud_nodes` row; off (itself or an ancestor) keeps it; a live
  heartbeat defers it until it ends; a failure is reported and backed off; a
  node moved to `done` by an applied sync is retired after it; a daemon
  started after the fact retires it at start.

### B5. Adopt records written before the merge

- At start, beside `adopt_legacy_records`: a `cloud_nodes` row of this client
  whose node has no location row, and whose resolved Files is a sandbox
  location, gets a location row naming that sandbox. One that no longer
  resolves to a sandbox is reported, not adopted.
- Tests: adoption, refusal of a mismatch, idempotence.

### B6. Docs

- `doc/files-locations.md` (one creator; Known gaps), `doc/cloud-sandboxes/
  autonomous-nodes.md` ("Interactive sandboxes": one creator now, the node's sandbox),
  `doc/cloud-sandboxes/setup.md` (the golden and `tod-sandbox golden refresh`),
  `blaxel-remote.md` if it describes `create_with_proxy` for nodes,
  `doc/architecture.md` ("Where the work runs"), `.claude/CLAUDE.md` ("Cloud
  sandboxes are the third place"; `cloud_nodes`).

## Open questions

None. Two proposals stand unless the owner objects (design.md, "Open
questions"): refusing an older build against a newer golden, and deleting a
supervised node's sandbox after a bounded wait when its supervisor does not
report stopping. Later extension, not planned: Migrate on a node with
children.
