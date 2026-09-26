# Autonomous nodes: implementation plan

How to build [autonomous-nodes.md](autonomous-nodes.md). Each work item is
sized for one agent, names what it depends on, and names what it owns, so
that items running at the same time do not edit the same code. Everything is
built and tested on the development account first, using its stand-ins
(the orchestrator's timer, its own disk, sandboxes created from the image).

## What the code has today

The facts that shape the plan:

- **The loop inside one protocol is headless already.**
  `tod_core::conversation::ConversationDriver` (`driver.rs`) sends turns,
  and each protocol's `progress` (`implement.rs`, `verify.rs`, `review.rs`,
  `fix.rs`, `gate_check.rs`) decides `Next::Continue` or done. It takes the
  agent through `AgentAccess` and needs no GPUI.
- **The loop across protocols does not exist.** What comes next is computed
  by `tod_core::lifecycle_next::next_step`, but only shown: the user presses
  Implement, Verify, the gate check, Advance. Advancing once the criteria
  pass (`advance_after_criteria`), waiving, and reverting live in
  `tod_ui::views::lifecycle_control::LifecycleController`, a GPUI entity.
- **`tod-cli` already runs remotely** through `tod_store::fleet::cli_relay`:
  a bash shim (`SHIM_SCRIPT`) sends the arguments, `TOD_*` environment, and
  stdin in a small framed format over TCP, and the listener runs the real
  `tod-cli` and returns exit code, stdout, and stderr. The format can be
  reused as an HTTP body. Writes from `tod-cli` go through a running app's
  mutation socket when there is one (`tod_core::interview::client`),
  otherwise to the store directly.
- **A change log exists, but not one sync can use.** `journey_changes`
  (`tod-store/src/journey_changes.rs`) records node, table, row, and
  operation for every node-scoped table, from triggers. But it deliberately
  leaves out every `conversation_*` table, carries no row contents, and is
  pruned once a journey bundle is sent.
- **`tod-relay`'s hold** (`hold.rs`) is one `keepAlive` `sleep` with a
  4-hour `timeout`, on while any reason holds. There is no lease.
- **Only `tod-relay` is built for Linux** (`x86_64-unknown-linux-musl`,
  cross-built from any OS because it has no C dependencies). Anything that
  opens a `tod-store` database pulls in bundled SQLite, which is C, so the
  supervisor and the orchestrator need a C cross-compiler (for example
  `cargo zigbuild`) or a Linux build.
- **Sandbox creation** (`tod-sandbox`: `blaxel.rs`, `provision.rs`,
  `config.rs`) declares the relay port, starts the relay, and has no proxy
  rules. `sandboxes.toml` holds the account; the default region is
  `us-was-1`.
- **Credentials** (`tod_store::CredentialStore`) already hold the user's
  GitHub token and Linear key.

## Milestones

1. **One node, start to finish, app closed** (waves 1–3): accept a node, it
   runs through its lifecycle in a sandbox, waits by sleeping, and its
   changes appear in the app when it is next opened.
2. **Live** (wave 4): the app hears about changes within seconds; the user's
   edits reach running nodes.
3. **Unattended** (wave 5): webhooks, usage limits, lost sandboxes, the
   watchdog.

## Wave 1: four items, all in parallel

**W1. Linux builds** (build, `doc/cloud-sandboxes/setup.md`)
- Make a crate that depends on `tod-store` build for
  `x86_64-unknown-linux-musl` from Windows, macOS, and Linux (try
  `cargo zigbuild`; fall back to building inside a Linux sandbox).
- A script that builds the Linux binaries the sandboxes need (the relay, the
  supervisor, the orchestrator) into `target/sandbox/`, used by provisioning.
- Owns: the build script and its doc section.

**W2. Sync change log** (tod-store)
- A `sync_changes` table: a number, node, table, row id, operation. Triggers
  for every table the app shows for a node, `conversation_*` included, and
  the few tables that are not node-scoped (lists, the outline's roots).
  Reuse `journey_changes`' trigger generator; keep the two logs separate
  (journeys prune theirs).
- `export_changes(after) -> Vec<Change>` with each row's current contents as
  JSON (a deleted row carries only its id), and `apply_changes(changes)`,
  which upserts or deletes by id in one transaction **without** logging the
  changes it applies (so applied changes are not sent back), and reports a
  conflict when a row's previous contents differ from what the sender saw.
- `snapshot()` / `restore()` of a whole database, for seeding.
- Tests: two stores kept equal through random edits on both sides.
- Owns: `tod-store/src/sync/`, the schema migration.

**W3. Lifecycle autopilot** (tod-core, tod-ui)
- Move what the lifecycle does without an agent (advancing after criteria,
  waiving, reverting, `enters_with_agent`) from `LifecycleController` into
  `tod_core::lifecycle` functions over `FleetStore`. `LifecycleController`
  calls them; the UI does not change.
- `tod_core::autopilot::Autopilot`: for one node, loop `next_step` → run
  that protocol's conversation through `ConversationDriver` until it is done
  → run the gate check → advance → repeat, until the node is done, a human is
  needed (a question, a `blocked` step, a failing criterion with no fix), or
  a budget (sessions, hours) runs out. Each step is saved, so a restart
  continues where it stopped.
- Tests with `--agent mock` directives, a node from `proposed` to done.
- Owns: `tod-core/src/autopilot/`, `tod-core/src/lifecycle/`,
  `views/lifecycle_control.rs`.

**W4. Relay hold leases** (tod-relay)
- A hold reason can carry a lease: `hold <reason> <secs>` renews it, and it
  ends when not renewed. The `keepAlive` process's `timeout` becomes the
  shortest lease left (renewed by replacing the process), replacing the
  4-hour cap.
- `POST /poke` on the relay's port: starts the supervisor if it is not
  running, else signals it. This is how the orchestrator and webhooks wake a
  node, with no second port.
- Owns: `tod-relay/`, `doc/cloud-sandboxes/relay-protocol.md`.

## Wave 2: the orchestrator and the node sandbox (after W1)

**W5. Orchestrator** (new crate `tod-orchestrator`, Linux)
- A small HTTP server on a declared port. Every request names the user
  (`X-Tod-User`); each user's data root is `/data/users/<user>/`, opened as a
  `FleetStore` on first use, serving its mutation socket so `tod-cli` writes
  go through one writer.
- `POST /cli`: the `cli_relay` request format as the body, run against that
  user's data root, the reply format back.
- `POST /users/<u>/seed` (a snapshot, W2), `POST /users/<u>/changes` (the
  app's changes), `GET /users/<u>/changes?after=<n>`.
- Provisioning: create the orchestrator sandbox (fixed name, the port
  declared, a public preview for webhooks later), install and start it. On
  the dev account its data is on the sandbox's own disk.
- Owns: `crates/tod-orchestrator/`, orchestrator provisioning in
  `tod-sandbox`.

**W6. Node sandboxes** (tod-sandbox, tod-store `cli_relay`)
- Create a node's sandbox from the image with proxy rules: GitHub (API and
  git), Linear, `api.blaxel.ai`, and the orchestrator's host, from the
  user's `CredentialStore` (see the design's Credentials section; git's CA
  set system-wide, `GH_TOKEN` a placeholder, `NODE_USE_ENV_PROXY=1`).
- An HTTP variant of the `tod-cli` shim: the same request body sent with
  `curl` to the orchestrator's `/cli` through the proxy, with the user and
  node in headers; retries with backoff.
- Check out the node's branch; install the relay, the shim, and the
  supervisor.
- Owns: node provisioning in `tod-sandbox`, the shim in `cli_relay.rs`.

## Wave 3: the supervisor (after W3, W4, W5, W6)

**W7. Waits and wakes** (tod-store, tod-cli, tod-core)
- A `waits` table (node, kind, match, deadline or next check, state) and
  `tod-cli wait --until | --event | --check --every` (with its `cli/` doc
  fragment); `ask` is the existing question path.
- A `Scheduler` trait: `schedule(id, sandbox, at)` and `cancel(id)`.
  `BlaxelScheduler` creates `wait-<id>` schedules on the node's own sandbox;
  `OrchestratorScheduler` calls the orchestrator's `POST /wakes` and
  `DELETE /wakes/<id>`. `sandboxes.toml` picks one (`scheduler`).
- In the orchestrator: the dev timer (wakes as rows, poke when due, hold
  itself awake while any is pending, reload on start).
- Owns: `tod-store/src/waits.rs`, `tod-cli/src/wait.rs`,
  `tod-core/src/scheduler.rs`, the orchestrator's `/wakes`.

**W8. Supervisor** (new crate `tod-supervisor`, Linux)
- Runs `Autopilot` (W3) for its node in the sandbox, with the agent started
  locally (Claude Code through `claude-code-acp`, the subscription token in
  its environment). Its `tod-cli` is the shim, so every write goes to the
  orchestrator; it keeps no database of its own.
- Holds the sandbox awake through the relay's lease while it works (W4);
  when the agent records a wait, schedules the wake (W7), releases the hold,
  and exits. `tod-supervisor wake` (from a schedule or a poke) asks what it
  is waiting on and continues or sleeps again.
- Pushes the branch at the end of every session.
- Mirrors each Claude session file as it is written: to Agent Drive where the
  account has it, otherwise to the orchestrator (`POST
  /users/<u>/nodes/<n>/transcripts/<session>`, appended). On a new sandbox,
  restores them and resumes.
- Owns: `crates/tod-supervisor/`.

**W9. Accepting a node** (tod-ui, tod-core)
- A "Run in the cloud" action on a node: seeds the user's database on the
  orchestrator if it has not been (W2 snapshot), creates the node's sandbox
  (W6), and pokes it. The node shows that it runs in the cloud, and its
  lifecycle buttons are replaced by what the supervisor reports.
- The app's sync, for now on start and on demand: send the outbox, pull the
  feed, apply it (W2). The outbox is the app's own `sync_changes` after the
  last number the orchestrator accepted.
- Journey `UserAction` records for the new action.
- Owns: the new action in `tod-ui`, `tod-core/src/cloud_sync.rs`.

Milestone 1 is reached here: test it with `--agent mock` in the sandbox
first, then with Claude on a small real node.

## Wave 4: live (after milestone 1)

**W10. Telling the app** (tod-orchestrator, tod-ui)
- The orchestrator publishes one data-free message per user to a hosted
  publish/subscribe service (choose it here; ntfy is the first candidate)
  whenever it commits a change for that user, coalesced to at most one a
  second. The app subscribes while open and pulls the feed on each message.
  Questions and blocked nodes also go to the phone.
- Owns: the publisher in the orchestrator, the subscriber in the app.

**W11. The app's edits reach running nodes** (tod-core, tod-orchestrator,
tod-supervisor)
- The app sends its outbox a moment after each change, in the background.
- `tod_core::impact`: given a change and the user's active nodes, which
  nodes it affects (the node itself; an ancestor's obligations or
  constraints; a component an obligation references). Pure, unit tested.
- The orchestrator records "context changed" on each affected node and
  pokes it. The supervisor takes it at the next stopping point (rebuild the
  context, continue); moving back uses `tod_core::lifecycle_validity`.
  Interrupting is left for later.
- Owns: `tod-core/src/impact.rs`, the handling on both sides.

## Wave 5: unattended (after wave 4; items in parallel)

**W12. Webhooks** (tod-orchestrator)
- A GitHub webhook endpoint on the orchestrator's public preview URL,
  verifying signatures; routing by branch, then by open waits; the event
  recorded on the node, the wait satisfied, the node poked, its schedule
  cancelled. Linear after GitHub.

**W13. Usage limits** (tod-supervisor)
- Recognise "usage limit reached" from Claude Code, read the reset time, and
  record a wait until then.

**W14. Lost sandboxes** (tod-ui, tod-sandbox)
- On start, the app checks each of the user's cloud nodes and replaces a
  sandbox that is gone (the user's tokens are only on their machine, so only
  the app can). The orchestrator marks such nodes when it fails to poke them.

**W15. Watchdog** (new Blaxel job)
- Hourly: list the workspace's sandboxes, clear holds on any awake longer
  than its lease allows, and flag its node through the orchestrator.

**W16. Crash guards in the supervisor**
- A hung agent (no output or tool activity for N minutes) ends the session
  and retries; after K failures it asks the user. A node's budget reached
  asks the user.

## After development

On the deployment account: switch `scheduler` to `blaxel` and run the
schedules spike (design, To verify 1), move the orchestrator's data to a
volume, and create node sandboxes by forking where it is faster. Delete the
orchestrator's timer once nothing uses it.
