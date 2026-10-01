# `tod-agentd`: the resident daemon

Status: **design, not implemented.** Nothing here is built; "Open questions"
lists what has not been checked against the code.

## The problem

Autonomous nodes run in two very different ways.

- **Cloud**: a headless `tod-supervisor` wakes, syncs a copy of the user's
  database, runs the lifecycle `Autopilot` for one node, records any wait,
  and exits. A scheduler (Blaxel's, or the orchestrator's timer) wakes it
  again. The app only shows what the database says.
- **Local** (this machine, or a dev container driven from it): the
  `Autopilot` runs on a thread inside the app (`autopilot::local::LocalRun`).
  Closing the app stops every run, and a wait is an in-thread sleep
  (`Autopilot::babysit`, `sleep_watching`).

The point of an autonomous node is that it runs until it needs the user, with
or without the app. And anything that waits (a bot's review, CI, and above
all a human's approval, which can take days) must not hold a process open
while it waits: in the cloud the VM has to sleep, and locally the run should
not depend on the app being open.

## The decision

Run local nodes the way cloud nodes run, with one resident process per data
root that is started by `tod`, not by the operating system.

| Cloud | Local equivalent |
|---|---|
| orchestrator: system of record, timers, webhooks, pokes | `tod-agentd`: the database, timers, spawning supervisors |
| `tod-supervisor` in the sandbox | the same `tod-supervisor`, in **direct** mode |
| Blaxel schedule / orchestrator `/wakes` | `LocalScheduler` in the daemon |
| a wake is a poke | a wake is a spawn (`tod-supervisor wake`) |

No orchestrator is needed locally: its jobs are the database (already here),
the timer (the daemon), and sleeping VMs (not applicable). The one loss is
webhooks, since a desktop has no public endpoint. Locally everything is
polled, which suits a human review checked every hour or so.

The unit of unification is the **supervisor and the waits table**, not the
orchestrator. A node can then move between host, dev container, and cloud by
changing its wake target and backend, not its logic (see "Moving a node").

## Principles

1. **An agent never waits.** It records a wait (`tod-cli wait`) or the
   babysitter does, the turn ends, and the process exits.
2. **A wake is only a poke.** A woken supervisor re-reads the waits and
   decides for itself, so a late, duplicate, or stale wake is harmless
   (`tod_core::scheduler`). The daemon may fire anything overdue at any time.
3. **The daemon is the only writer.** The app is the main writer today and
   holds the exclusive `FleetLock`, but not the only one: cloud sync,
   `cloud_nodes`, and the journey queue write through side connections (see
   "Findings"). The daemon takes the lock and absorbs those. The app and
   `tod-cli` become clients of it.
4. **No polling for display.** Changes are pushed from the writer to
   subscribers; views update from them (`FleetStore::subscribe_changes()`,
   `TaskListView::apply_live_snapshot`), and idle costs nothing.
5. **One code path.** The app no longer runs an `Autopilot` itself. A run is
   always a supervisor process the daemon starts.

## Processes

```
tod (UI)  ──read-only SQLite──┐
   │  subscribe / commands    │
   ▼                          ▼
tod-agentd  ── owns FleetStore writer, FleetLock, LocalScheduler
   │  spawns / signals
   ▼
tod-supervisor wake --node N      (one per running node, exits at a wait,
   │                               needs-human, or done)
   ▼
agent (claude / cursor), on the host, in a dev container, or in a sandbox
```

- `tod-cli` talks to the daemon, as it now talks to the app's mutation
  socket. It keeps its direct-open fallback only for the case where no
  daemon can be started (and says so).
- The app opens the database **read-only** (WAL allows readers beside the
  one writer) and does every write through the daemon.

## Starting, finding, and stopping it

**Per data root.** The data root is already how instances are isolated (the
sandbox data roots used for testing). The daemon's lock, port file, and token
live in the data root, so several data roots have several daemons.

**Started by `tod`.** On launch the app looks for a live daemon through the
port file. If none answers, it takes a start lock (so two `tod` launches
cannot race), starts one, and waits for its handshake. The daemon is
required: the app does not run without one.

**Detached.** It must outlive the app:
- Unix: a new session (`setsid`), stdio to a log file under the data root.
- Windows: `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, plus
  `CREATE_BREAKAWAY_FROM_JOB` for a `tod` launched inside a job that kills
  its children on exit. If breakaway is refused, start without it and log
  it.

**Run from a copy.** Windows will not let a running executable be replaced,
which would break `cargo build` and any updater. `tod` copies `tod-agentd`
to `<data root>/daemon/tod-agentd-<stamp>` and runs the copy.

**Transport.** Loopback TCP, replacing the existing mutation socket
(`tod_store::fleet::mutation_socket`). That socket has **no token** today (a
port number in `tod.mutation-port` is all a client needs), so the daemon adds
a per-process token written to the data root. Loopback avoids Windows
Firewall prompts. Revisit a named pipe or Unix socket if the token proves
awkward.

**Lifetime.** It lives until logout or reboot, or `quit`. There is no
operating-system registration. After a restart, nothing runs until the user
starts `tod`; waits that came due meanwhile fire then.

**`quit`** is a protocol command, as with the agent control socket: drain
(below), then exit. Anything launched for a test must be stopped this way.

## Version check

A stamp, not a semver: dev builds change on every compile.
`tod_core::CLI_BUILD_STAMP` is the pattern (`tod-core/build.rs` hashes the
source). The handshake returns `{ stamp, protocol }`.

| App vs daemon | What the app does |
|---|---|
| same stamp | connect |
| daemon older | ask it to **drain and restart** with the new copy |
| daemon newer | do **not** restart it (two builds would fight over one daemon); tell the user to update |

**Drain.** Every run is stopped at its next boundary, the same as Pause
(`LocalRun::pause`): never mid-write, and a turn in flight is allowed to end.
Runs whose desired state is "run" are started again by the new daemon, so
nothing is lost. The autopilot already resumes from its saved state and the
conversation's session id.

## Desired state and the lease

The app no longer holds a `LocalRun`. The runner line (start, pause, resume;
`doc/ui/task-panel.md`) writes **desired state** for the node: `run`, `pause`,
or `stop`. The daemon makes it so.

**One runner per node.** The daemon holds a lease per node (a row, with the
supervisor's pid and a heartbeat on the daemon's own timer). The app's own
conversation on a node and the supervisor must not both drive its agent: the
supervisor takes its change at the next stopping point, the way the cloud
supervisor takes a context change or a `SIGUSR1` poke (`tod-supervisor`
`context`, `signal`). Exactly how an app conversation and a running supervisor
share one node is an open question below.

## Waits and the scheduler

`tod_store::waits` and `tod_core::scheduler::Scheduler` already model this for
the cloud. Changes:

1. **Generalize the target.** `Scheduler::schedule(id, sandbox: &str, at)`
   becomes a `WakeTarget` (`Sandbox(name)` or `Local`). The orchestrator and
   Blaxel implementations are unchanged.
2. **`LocalScheduler`** lives in the daemon and has no storage of its own:
   the `waits` table is the truth. It reads `WaitRepo::next_due`, sleeps until
   then (re-checking at least every minute, as the orchestrator's timer does,
   and on every store change), and spawns `tod-supervisor wake --node N`.
   Anything overdue at start fires immediately.
3. **Move the settle step into `tod-core`.** `tod-supervisor/waits.rs::check`
   settles each due wait (`until`, `event`, `check`). Both the supervisor and
   the daemon call one copy.
4. **`babysit` stops instead of sleeping.** In `pr`, when what is left is only
   waiting, it returns `Outcome::Waiting` after recording a wait, on every
   path. Cloud: the supervisor reconciles the wake and exits, so the VM
   sleeps. Local: the supervisor exits and `LocalScheduler` wakes it.

### Human review

`pr_readiness::Wait` gains `HumanReview`. Today the babysitter's `Next::Clear`
(a review still missing, `mergeable_state` `blocked`) is "for a person" and
nothing watches it. It becomes a wait:

- recorded as an `event` wait whose `due_at` is the next check. In the cloud
  a review webhook satisfies it at once and the check is a backstop; locally
  the check is all there is. Match `github:pr:<n>:review:approved`, not
  `github:pr <n> review`, which is a prefix match and fires on any review,
  comments included.
- a webhook is a hint, not the answer: it fires on any approval by anyone, is
  not undone by a later changes-requested or dismissal, and can be lost. The
  settle step therefore reads the pull request from GitHub (see Findings).
- when it settles: approved advances the node (the `pr → approved` gate);
  changes requested reopens the `Pr` protocol turn.

### Cadence

One pure function, `next_check(kind, waiting_since, now, schedule) -> due_at`,
in `tod_core`, so both sides agree and it can be tested without a clock.

- **Short waits** (a bot's review, CI, GitHub computing mergeability) keep
  today's cadence (60s, slower after a while) and ignore the calendar.
- **Human review** backs off by age (for example 15 minutes in the first
  hour, hourly after, every few hours after a day) and follows a **schedule**:
  a time zone and working hours, with nights pushed to the next window and
  weekends checked rarely.
- **Check now** (a button) and an incoming event override the schedule.
- The schedule is in `pr_readiness` in `tod.yml` (per project), with a user
  override. The defaults are for review waits only.

## Progress and display

Supervisors write what they are doing into the database through the daemon: a
status row per node (the current step, waiting-on, last check), transcript
items, waits. The writer's commit already produces a change broadcast.

```
supervisor ─write─▶ daemon (writer) ─commit─▶ change broadcast
                                                   │ push, subscription socket
                                                   ▼
                          app: subscribe_changes() ─▶ diff-apply on the UI
```

- A subscription carries a change cursor, so a reconnecting app asks for
  "changes since N": no full reload, nothing polled. The cloud feed's
  `sync_changes` log is the pattern, but it cannot be the cursor as it is (it
  skips the run, shell, and notification tables the task list reads, and
  hides changes applied from the cloud feed); see "Findings".
- **Coalesce** streaming output (a few updates a second per node) so an agent
  typing does not flood the UI.
- With no app connected nothing is pushed; the app catches up on connecting.
- The app reads off the UI thread and diffs, as `apply_live_snapshot` does.
  A change touching one node costs one node.

### Waiting, visually

The status and waits rows are the one source for every view (tree glyph, task
panel runner line, filters), on local and cloud alike. A pending wait shows
its kind, how long, and when it is next checked. It is not "needs you"
(`tod_core::attention`): nobody owes anything, time has to pass. When the
wait settles the store change clears it.

## Moving a node

A node moves between this machine, a dev container, and a cloud sandbox by
changing:
- its **wake target** (`Local` or `Sandbox`),
- its **backend** (direct on the data root, or a replica synced with the
  orchestrator),
- its **Files location** (`node_files_locations`, with the stale-location
  rules and impact dialog in `doc/files-locations.md`).

Its waits and state travel, since they are rows. **Its agent session travels
too:** a move copies the session's transcript file (Claude's
`<claude dir>/projects/<project>/<id>.jsonl`) into the place the target
environment expects it, then resumes the **same session id**. A move never
starts a fresh session on purpose. The project directory is derived from the
working directory, so the copy has to put the file where the target's working
directory would look for it. This is what `tod-supervisor`'s transcript mirror
(`transcripts.rs`) already does for sandboxes (`restore`); a move generalizes
it to any direction (host, dev container, sandbox, in any order), and the
mirror's store need not be the orchestrator.

Only if a resume still fails (the file is missing or damaged) does the driver
fall back to its existing recovery, which starts a fresh session seeded from
the database (`rotate_and_start`), and it says so in the transcript. That is a
failure path, reported, not the way moves work. Context-budget rotation, which
the driver also does, is a separate thing and unchanged.

**Acceptance test** (to do last): with the mock agent, move a node
host → cloud, cloud → host, host → dev container, and dev container → host.
Each must keep its lifecycle state and plan, continue its conversation,
carry an open wait across the move, and wake from that wait on the new side.
Failures are reported, not only passes. Tests needing a container or sandbox
are gated on their `TOD_TEST_*` variables like the existing ones.

## Costs and risks

- **Machine asleep or off.** Nothing runs while the machine sleeps, and no
  scheduler can wake it unless asked to. The cloud covers work that must
  proceed regardless.
- **Single-writer move.** The writer, `FleetLock`, and mutation socket move out
  of the app. This is the largest change. The app's reads and every write
  site must be audited, including any write that today happens on the UI
  thread.
- **More agent processes.** The app and autopilot no longer share one agent
  (`SharedAgent`); each supervisor starts its own, resuming by session id.
  Count them at ~100 nodes.
- **Logout and reboot end the daemon**, and with no OS registration nothing
  restarts it until `tod` runs. The user accepted this.
- **Packaging.** `tod-agentd` and `tod-supervisor` ship beside `tod` (as
  `tod-cli` does, and `tod --verify-process-bundle` should check them).

## Findings

Researched against the code; file references are to this tree.

### The store and the writer

- **`FleetLock`** is an advisory file lock on `<root>/tod.lock`, taken in
  `FleetStore::open_without_reattach` (`store.rs:97`). Holders: the GUI, the
  orchestrator (one store per user), and the cloud supervisor on its own
  private replica root. `tod-cli` and `tod-journeys` do not take it in
  production. The lock is taken **after** `FleetLaunch::prepare`, which has
  already run migrations (`store.rs:93-117`), so a second process can migrate
  before learning it cannot open. The daemon must take the lock first.
- **The lock guards in-process invariants**, not SQLite itself (WAL already
  serializes writers): the 2s debounced mutation queue, the in-memory undo
  log (`CommandLog`, 50 entries), the projection and its broadcast, and "one
  app per data root".
- **Almost every write already goes through the writer**: `FleetMutation`
  (~35 variants) and `InterviewCommand` (conversations, decisions, phases,
  waits, reviews, and so on). The mutation socket carries only
  `OutlineMutation` and `InterviewCommand`. `FleetStore::read` cannot write
  (read-only connection, `query_only`), and non-test `tod-core`/`tod-ui` have
  no raw SQL writes.
- **Writes that bypass the writer** (~9 sites, in the app process):
  `cloud_sync.rs:370` (applies the cloud feed on its own connection),
  `cloud_nodes` upserts and removes (`cloud_sync.rs:809`,
  `cloud_sync/lost.rs`), and three journey queue methods in `store.rs`
  (`prune_journey_changes_through`, `queue_journey_submission`,
  `set_journey_submission_status`). The orchestrator and supervisor have their
  own side connections too. So "one writer" is not strictly true today.
- **The UI thread blocks on writes.** `interview()` and `writer().flush()`
  use `block_on`: ~24 flush sites in `tod-ui`, 17 in `tod-core`, and
  `interview()` called from view methods (`conversation/mod.rs:1385`,
  `ui/agent_runs.rs`, `unified/panels/*`). Over a socket each is a blocking
  round trip on the GPUI thread unless made async.
- **No seam to cut at.** `Arc<FleetStore>` is a concrete type in ~48 `tod-ui`
  and ~42 `tod-core` files. The move starts by introducing a client facade
  (`enqueue`, `flush`, `interview`, `subscribe_changes`, undo, `read`).
- **Undo is process-local** (`CommandLog`, `store.rs:100`), used by Ctrl+Z
  (`app/window.rs:1117`) and the command history. It moves with the writer or
  Ctrl+Z breaks. Socket writes (agents, `tod-cli`) already land in the user's
  undo history, since they use the same writer.
- **What the daemon protocol must carry beyond outline and interview
  commands:** the other `FleetMutation`s (tasks, runs, sessions, shells,
  notifications, files locations), a flush barrier, undo and history, the
  journey queue, cloud sync and `cloud_nodes`, launch hooks (reattach), a
  subscription, and the handshake. Reads need no RPC: the app and `tod-cli`
  already open read-only connections directly (`interview/client.rs:81`).
- **`tod-cli`'s fallback** opens the store itself when no port answers
  (`client.rs:97`), which also runs launch hooks and migrations.
- **Change notification today is coarse and in-process.**
  `subscribe_changes()` is a payload-free `()` broadcast fired by projection
  reloads after writer commits. It does not fire for another process's writes
  (nothing watches the file). A missed-wakeup window exists
  (`Notify::notify_waiters` stores no permit), which is likely why some views
  keep fallback polls (`conversation/mod.rs:120`, 8 polls at 250ms). Every
  consumer re-queries rather than diffing.
- **`PRAGMA data_version` is not a working cross-process signal here.**
  `reload_if_stale` compares values read from different connections, which
  SQLite does not define; detection really relies on four row counts changing
  (`projection.rs:107-132`). From reading the code, not run.
- **WAL and a read-only second process are fine.** WAL is set on writer
  connections, busy timeout 5s. A reader sees each commit on its next read
  transaction. The read-only app must not open until the daemon has migrated.
- **`sync_changes` as the push cursor** (monotonic `seq`, written by trigger
  in the commit's transaction, per-node and per-table) is a good fit for
  catch-up, with gaps: it excludes `agent_runs`, `shell_sessions`,
  `notifications`, `agent_sessions`, tasks, `node_files_locations`, and
  `incoming_*`; changes applied from the cloud feed are suppressed, so it never
  records them; and it is never pruned (not confirmed beyond grep). The
  `journey_changes` log is consumed destructively and cannot be shared.

### The supervisor in direct mode

- **It does not work in direct mode as it is.** `wake()` hard-wires
  `Replica::open` and an `Orchestrator` (`lib.rs:196-341`), and `main.rs`
  requires `TOD_USER` and `TOD_ORCHESTRATOR_*`. But the autopilot core needs
  nothing from `tod-ui`: `Autopilot::run_with` takes any `AgentAccess`.
- **Most cloud dependencies are already traits or options**: `Holder` (relay
  hold), `Config.transcripts`, `Config.scheduler`, `push_branch`. Cloud-only
  and to be skipped locally: the orchestrator client, the `Syncing` wrapper,
  sandbox proxy credentials. `Replica` needs a `Backend` trait whose direct
  version returns the real store and does nothing on push and pull.
- **Dropped in direct mode:** seeding and cursors, pull and push, and
  `localize_files` (which overwrites the node's Files row; locally the real
  row is the point).
- **Reusable as is, and worth moving to `tod-core`:** `waits::check`,
  `answers`, `usage_limit`, `guard`, and `context::take`.
- **To add:** `--data-root` and `--node` arguments; the user's real settings
  for `launch` and `settings_path` (the app builds them from `TodSettings` in
  `runners.rs:166`, the supervisor ignores them); a per-node `state_dir` under
  the data root; resolving the workspace from the node's Files location
  (worktree, container) rather than a plain directory, including for
  `waits::check` commands and `git push`; and installing the journey
  recorder, which is a process-global and a no-op in a process that never
  installs one.
- **GitHub credentials** for the babysitter come from the data root's
  credential store (`pr_readiness::feed_for`), so on the real root they are
  available, unlike on a sandbox replica.
- **The local runner today.** `NodeRunners` (`runners.rs`) starts a `LocalRun`
  on a thread; pause and stop are in-memory atomics. "Was running" is
  inferred from `autopilot/<node>.json` having `outcome: None`; there is no
  declared desired state. Desired-state rows replace that inference, the
  in-memory `Request`, the UI's `seen_waiting` (resume after the user
  answered), and the last-start error. The autopilot state file itself
  (`AutopilotState`) remains useful run bookkeeping and lives in the data root.
- **Session transcripts.** Claude's session files are mirrored to the
  orchestrator because sandboxes are ephemeral (`transcripts.rs`: `Mirror`,
  `restore`, `follow`). On the same host, user, and working directory nothing
  is needed. A move changes the environment, and the project directory name
  derives from the working directory, so a move **copies the file and
  resumes the same session id** (see "Moving a node"). To find out by test
  before building: whether Claude resumes a session whose file sits under a
  different project directory name than the one it was created in (the
  copy must rename it to the target's), and whether dev containers share
  `~/.claude` with the host (the mounts were not read). The driver's
  fresh-session fallback (`driver.rs:~570`, `rotate_and_start`) stays as the
  failure path only.
- **What does not travel on a move:** uncommitted workspace changes (only the
  branch is pushed, so push before moving), the supervisor's state files, and
  credentials.

### Human review and the babysitter

- **Webhooks map reviews** (`webhooks.rs:181-191`) to
  `github:pr:<n>:review:approved`, `:changes_requested`, `:commented`,
  `:dismissed`, and `:review:comment`, and the same under `github:branch:<b>:`.
  The events are listed in `doc/cloud-sandboxes/orchestrator.md:199`; **no code
  registers the webhook**, the user does it by hand. A delivery is not
  deduplicated. `spec_matches` is a word-prefix match, so
  `github:pr 12 review` matches every review state.
- **Approval cannot be told from changes-requested today.** `PrReview` holds
  only author and time (`github.rs:444`) and is used for bot-review timing.
  `reviewDecision` is never read. `mergeable_state == blocked` is the only
  human-review signal. Telling them apart needs a `state` on `PrReview` or a
  `reviewDecision` read.
- **The `pr → approved` gate** has no human-approval criterion; it enforces
  review only indirectly through `pr-approved.mergeable` (`clean` or merged).
- **When the review is missing, the run stops.** `babysit` returns `Clear`
  and the gate fails `pr-approved.mergeable`, so the run ends as
  `NeedsHuman::FailingCriteria` (`autopilot/mod.rs:445`), which counts as a
  request. On a local runner `on_attention` restarts it once the node is no
  longer waiting on the user (`runners.rs:315`); whether a failing gate
  criterion counts as waiting there is unconfirmed. On the cloud supervisor,
  only `Stopped` schedules a wake, so the node sleeps until a poke; a review
  webhook wakes it only if it has a matching pending wait or its branch
  matches (from the orchestrator docs, not traced in `lib.rs`).

### An app conversation and a supervisor on one node

- **Local today:** the view cannot send to a conversation the runner hosts
  (`driver_slot.rs`; `agent_runs.rs:807`), and the runner will not start while
  an app conversation is driving that node (`runners.rs:196`). Both are
  **in-process** (`AgentRuns`), and there is a small gap in `host_elsewhere`
  (`agent_runs.rs:264`). Nothing exists at the database level.
- **Cloud today:** no lease, lock, or holder (`cloud_nodes` has none). The
  user's chat is a separate Claude session ("kept simple on purpose",
  `autonomous-nodes.md:228-247`). The app does not obviously refuse a cloud
  node in the conversation view (unconfirmed).
- **Separate processes make these real:** two drivers appending turns to one
  conversation, a session-id race (`SetConversationSession` last write wins,
  and a stored id absent on the machine is treated as resumable,
  `driver.rs:389`), a lifecycle move racing `context::take`, and a shared
  checkout the supervisor commits.
- **The `cloud_nodes` row is synced last-writer-wins**, so a lease cannot live
  in it (the same reason `context-seen` is kept out). With the daemon as the
  only writer, a lease in a local-only table is natural.

## Decisions

Settled with the user:

1. **The daemon absorbs** cloud sync, `cloud_nodes`, and the journey queue.
2. **Undo is the user's.** The daemon keeps the undo log, each entry records
   its actor, and Ctrl+Z offers only the user's own entries. An agent's change
   is never undone by it. If something since changed what an entry touched,
   undo warns and asks, as conversation reversal does for conflicts. (Agent and
   `tod-cli` writes land in the undo log today because they use the same
   writer; that stops.)
3. **UI writes become async, for the workbench only.** The workbench is the
   unified view (`crates/tod-ui/src/unified/`: the node tree, its panels, and
   the chat drawer) and whatever it shows. Other views are not migrated and
   keep their synchronous calls. This is **required before the daemon ships**,
   not a follow-on: a synchronous call to a busy daemon would freeze the
   window. The order is: an async client facade over the in-app writer first,
   the workbench's flush and `interview()` call sites onto it, then swap the
   transport to the daemon. Views outside the workbench use a blocking
   adapter on the same facade.
4. **`tod-cli` starts the daemon** when it needs one and none is running, on
   the host only. In a dev container or cloud sandbox it reaches the host
   daemon through the relay (or the orchestrator, for a cloud node), and never
   starts anything.
5. **A moving node's session is copied and resumed**, not rotated (see
   "Moving a node").
6. **`git push` is the same everywhere** (see "Pushing the branch").
7. **A review wait is a waiting status within `pr`, not a lifecycle phase**
   (see "Human review"): the node moves out of the active list while it waits
   and comes back on approval or on requested changes. Approval is told from
   changes-requested by reading the review state (`state` on `PrReview`, or
   `reviewDecision`).
8. **Mid-turn messages are out of scope here.** A user's message to a node goes
   through the daemon (one driver, one session). When and how it reaches an
   agent that is mid-turn is designed separately.

Assumed, because the user did not object (revisit if wrong):

9. **The change cursor** is a small dedicated notify log (node, table, seq)
   written by the daemon, since `sync_changes` excludes tables the task list
   reads.

## Pushing the branch

The supervisor today pushes at `Boundary::Step` (`lib.rs:119`) and once more
when it exits (`lib.rs:324`). A **step** is one lifecycle step the autopilot
runs: a phase agent, Implement, Verify, Review, Fix, or one pull-request
turn, each a conversation run to its end. So "the end of a session" means the
end of a step, and a stop of the supervisor (a wait, needs-human, or done).
With the autopilot at the center, that is the natural point: the work in a
step is complete and consistent.

Pushing is the same on every location (host, dev container, cloud), through
the same `git::push_branch`, moved to `tod-core`. Triggers:

- at every **step boundary**, so a finished unit of work is on the remote and
  the next machine (after a move) has it;
- before the run **stops** for a wait, needs-human, or done;
- before a node **moves** between locations (uncommitted work does not
  travel, so commit then push);
- after a **fix round in `pr`**, so the pull request shows it. This is
  already a step boundary.

Not on a timer: a clock does not know whether the work is in a state worth
keeping. Failures are logged and shown, not fatal. A push is only safe on the
node's **own** branch (`node_fields.branch`, `task/<slug>` by default): a node
that shares the workspace and has no branch of its own must not push whatever
is checked out, since `push_branch` pushes `HEAD` to its same-named branch.
The push refuses a default branch.

Cloud keeps nothing different, except that its VM is not expected to
disappear; the push is the same for the reasons above, not for loss.

## Order

Scope: the workbench only (decision 3).

1. Settle step into `tod-core`; `babysit` returns `Outcome::Waiting`; the
   review state read; `HumanReview` as a wait with the cadence function. This
   alone moves a waiting-for-review node out of the way and brings it back
   locally, without the daemon.
2. The async client facade over the in-app writer, and the workbench's call
   sites onto it.
3. The daemon: spawn, lock (before migrations), handshake, version and drain;
   the single writer, absorbing cloud sync, `cloud_nodes`, and the journey
   queue; undo by actor; the notify log and push feed; `tod-cli` and the app
   on it.
4. Direct mode for the supervisor, `LocalScheduler`, desired state and
   leases; remove `LocalRun`.
5. The waiting visuals (the group, the glyph, the runner line).
6. Moves: copying the session file, and the acceptance test.

### Status and deviations

- **1, 2, 3 done.** The writer is pluggable, not rewritten: `FleetWriter` has a
  local backend (a thread) and a remote one (`RemoteWriter`, implemented by
  `tod-agentd-client`'s `DaemonWriter`); the app and `tod-cli` open the store
  as clients (`FleetStore::open_client`), so the ~90 call sites did not
  change. The protocol retries under one request id, which the daemon
  answers once. Undo is the daemon's, for the user's changes only; a client
  mirrors the history (`Command::History`). The daemon starts the cloud
  outbox/lost-sandbox check and the orchestrator notifications. The cloud
  actions the user starts in the app (`run_in_cloud`, stopping one,
  retiring a finished node, adopting old records) write `cloud_nodes`
  through the writer (`CloudNodeUpsert`/`CloudNodeRemove`). The
  orchestrator and supervisor run in the cloud on stores of their own, so
  they are not writers beside this daemon.
- **4: the daemon hosts the runs itself.** Instead of spawning
  `tod-supervisor wake` in direct mode, `tod_agentd::runners::Runners` keeps
  one `LocalRun` thread per node inside the daemon, wakes `Outcome::Waiting`
  runs when due (read against the clock every 5 s, so a machine that slept
  wakes them), continues a run whose request was answered, and starts again
  the runs a stopped daemon left without an outcome. The app's `NodeRunners`
  is a proxy: it sends start/pause/stop and shows the `RunnerState` the
  daemon pushes (`Event::Runner`; the client keeps the latest per node for a
  late subscriber). The cloud supervisor is unchanged. A run pushes the
  node's own branch at each step boundary and before it stops
  (`provision::push_node_branch`; never a default branch; nothing for a node
  with no checkout of its own).
- The daemon runs from a copy, so programs beside it are found through
  `TOD_PROGRAM_DIR` (`tod_store::install::program_dir`). `TOD_NO_DAEMON`
  stops `tod-cli` starting one (tests on throwaway roots).
- **5** done as a recess, not a group: a node whose run ended to wait shows
  `<state> · waiting` and a muted title in the tree (checked in the running
  app against a saved wait); the runner line says what and when. A "Waiting
  (n)" chip in the tree's quick filters (`awaiting_only`) keeps those nodes
  and their ancestors. A glyph is not built.
- **6** mostly. A resumed conversation brings its session log to where it now
  runs (`ConversationDriver::bring_session`: host, dev container, or
  sandbox; from the host or the node's mirror, filed under that working
  directory's project name, complete lines only; `session_log::ensure_session`
  /`transfer` over a `Remote` per place). Every turn also keeps its log in
  the node's mirror (`keep_session_in_background`; a sandbox is pulled), so a
  move from any place has a source. Verified with a real Claude: `--resume
  <id>` works from a jsonl copied under another working directory's project
  dir. Run against real places (`TOD_TEST_DEV_CONTAINER`;
  `TOD_TEST_SANDBOX` + `TOD_TEST_SANDBOX_ROOT`, and `TOD_RELAY_BIN` if the
  relay is not beside the build): a 330 KB log goes host → place under the
  place's own project name and comes back byte-identical, for a Docker
  container and for a Blaxel sandbox. That found one bug, fixed: on Windows
  a 48 KB chunk overran the 32 K command line (now 16 KB). The move chain
  (host, container, sandbox, host, container, sandbox, host; each hop brings
  the log, a turn adds a line, the mirror keeps it) ran against both real
  places without losing a turn; it found that a return to a place already
  visited kept its stale log, so `ensure_session` now takes the longest log
  among the sources. A real Claude (`claude -p --resume`) also ran it: a
  session started on the host was resumed in a dev container and in a cloud
  sandbox from the copied log, and back on the host from the log copied
  back, each knowing what the other side was told
  (`doc/cloud-sandboxes/test-image.md`). Moves of a whole node's lifecycle
  state, plan and wait are not tested as such: those live in the store,
  which does not move, so only the conversation's session log travels.
- **Test suite notes.** `tod-store --lib` passes module by module except
  three that fail or hang on this Windows machine and did so before the
  daemon work (the hang reproduced on the commit before it):
  `fleet::migration` hangs in its second test, and
  `fleet::changes::trigger_changes_when_files_settings_change` and
  `fleet::terminal::open_shell_for_node_registers_live_process` fail.
