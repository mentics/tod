# Who is next: the one answer to "what is this node waiting on?"

Status: design, not built.

Every node that is not `done` is in the same position: **something has to
happen before it moves, and some actor has to make it happen.** The app must
answer that for every node, in every phase, in one place, and show it so the
user can tell at a glance:

- what is waiting on **me** (what I do next),
- what is waiting on **others** (who, and for how long, so I can go ask),
- what is **being worked** right now (nothing for me to do),
- what is **done**.

This is orthogonal to the lifecycle phase. `pr_review` can be on the agent
(fixing a thread), on others (a reviewer has not looked), or on me (the
agent gave up); so can `merged` → `released`, `planning`, and the rest.

## The model

```rust
/// Who has to act for the node to move. Derived, never stored.
pub enum NextActor {
    /// Automated processing for the node is in progress (an agent turn, a gate check, a runner step). Nothing for the user to do.
    Working { what: WorkKind, since: i64 },
    /// The user has to do something; the node will not move until they do.
    You { why: YouReason, since: i64 },
    /// Someone or something else has to act. Resolves without the user.
    Others { wait: ExternalWait },
    /// Terminal. The `done` phase has this one state.
    Done,
}
```

Four actors, no fifth. Anything that needs the user is `You`, whatever the
cause; the cause is a sub-state (`why`), not another actor.

### `You`: the user must act (sub-states)

| `YouReason` | Today's source |
|---|---|
| `Request` | `attention` items: decisions, blocked/partial plan steps, open findings, failing criteria |
| `Permission` | pending permission (`NeedsHuman::Permission`, the conversation's pending permission) |
| `Failed { error }` | agent turn failed, the runner failed, **or a transient wait ran out of retries**; also anything broken that retrying cannot fix (GitHub auth failed, sandbox gone) |
| `Stopped { reason }` | `NeedsHuman::{NoPlan, NoProgress, EvaluationStuck, PrStuck, PrBlocked}`, budget spent |
| `Paused` | the user paused the runner |
| `NotStarted` | work remains, nothing running and nothing asked: the node needs someone to start it |

`Failed`, `Stopped`, `Paused` and `NotStarted` are all "the user has to take
an action to move it", so they are `You`, and they count everywhere `Request`
counts (the tree badge, the filter, Alt+Q). Today they do not: `attention`
covers only requests, so a failed or stopped node is invisible to Alt+Q.

### `Others`: someone or something else must act

```rust
pub struct ExternalWait {
    pub kind: WaitKind,
    /// Who, when known: a reviewer's login, the checks' name, the
    /// dependency's title. Shown so the user can go and ask.
    pub who: Option<String>,
    pub detail: Option<String>,
    pub since: i64,
    /// When the app looks again, when it polls.
    pub next_check_ms: Option<i64>,
}

pub enum WaitKind {
    HumanReview,
    BotReview,
    Checks,
    Merge,
    Release,
    // `Dependency { node }` (another node must finish first) is not built:
    // the outline has no node-to-node edge yet. See "What the code says".
    /// A service could not be reached and the next look is expected to
    /// work: a short, bounded retry. Not a state of the process: it exists
    /// only until the retries run out.
    Retrying { service: String, attempt: u32, max: u32, error: String },
}
```

- **Retrying** is the "temporarily unavailable" case. It covers errors the
  caller classifies as transient (timeout, 5xx, rate limit, connection
  refused) and nothing else. Broken things (auth failed, 404, a
  sandbox that no longer exists) are never retried: they are `You::Failed`
  at once, naming what failed and why ("Errors are never hidden"). After
  `max` attempts a retrying wait becomes `You::Failed` carrying the last
  error. The retry count and next try live on the wait row, so a restart
  does not reset them.
- **Dependency** needs the outline to know that node A waits for node B.
  Whether that edge exists today is unchecked; see the open questions.
- **Scheduled** and **stale** are not states. Scheduled GitHub looks are
  just `next_check_ms` on an `Others` wait. Staleness is an age threshold
  applied when drawing (see "Showing it"), with nothing recorded.

### `Working`, `Done`

`Working` is a conversation running or an autopilot run in an agent turn.
A run that is only polling GitHub for a bot or CI is **not** `Working`: it is
`Others` (the agent is not working, the bot is). That fixes a wrinkle in
`RunnerStatus::Running { next_poll_ms }`, which today shows a polling run
as running.

### Precedence

When more than one holds, the first of these wins (as built in
`tod_core::next_actor`, `derive`):

1. `lifecycle == done` → `Done`.
2. A permission is pending → `You` (a running agent behind it is stuck on it).
3. Automated processing is live (it holds a fresh activity lease) → `Working`.
   Processing wins over open requests and a stale outcome, as `RunnerStatus`
   already did: the node is moving, and the run's own result replaces them.
4. A request is open → `You`.
5. The last run failed, stopped, or was paused → `You`. This beats a pending
   wait, so a failure is never hidden behind one.
6. A pending external wait → `Others`.
7. Otherwise → `You::NotStarted`. Nothing is ever "idle": a node with no
   actor is a node nobody has been told to move.

## One implementation

There are three overlapping answers today. They collapse into one.

| Today | Becomes |
|---|---|
| `tod_core::attention` (user-only, batched, durable) | stays as the **detail** of `You::Request` (the items to answer). It is no longer the "is it waiting" test. |
| `tod_core::runner_status::RunnerStatus` (one node, part in-memory, part derived) | **replaced** by `NextActor` for "who". What remains of it is runner controls only: is there a runner, is it paused, what the split button does. `or_scheduled`, `github_wait` and the `Awaiting` variant go into `Others`. |
| `NeedsHuman` / `Outcome::{Waiting, Stopped, ...}` (a run's outcome) | stays as the run's own report. It is **mapped** into `NextActor` in one place, not re-mapped by each view. |
| `tod_store::waits` (`github:*` rows, `due_at`, `detail`) | the durable record of every `Others` wait, including `Retrying` (attempt, max, last error) and `Dependency`. |
| `cloud_sync`'s "attention or pending waits" check | asks `NextActor` |

The module: `tod_core::next_actor`, with

```rust
pub fn for_nodes(conn: &Connection, ids: &[Uuid], runs: &RunSnapshot)
    -> Result<HashMap<Uuid, NextActor>>
```

batched like `attention::for_nodes` (no per-node round trip), computed off
the UI thread on every store change by what is now `unified/attention_feed.rs`,
and read by every view. The single-node task panel calls the same function
with one id.

**It must not depend on what is in this process's memory.** Runs are hosted
by `tod-agentd`, on other machines, or in the cloud, and the app restarts, so
an outcome that lives only in `Live` is lost. The last run outcome per node
(or what it implies: the failure, the stop reason, the wait) needs to be
read from the store or from agentd's persisted state. To confirm which of
those already persist before building (open question 2).

## Showing it

The point of the feature. Everything below reads `NextActor`; no view
derives its own.

1. **Tree row badge**, one per row, colored by actor and always present
   (a row with no badge means "done"):
   - `You`: the accent color, with the reason and the age ("needs answer ·
     2h", "failed · 14m", "stopped · budget", "not started").
   - `Others`: muted, with who and age ("Maria's review · 2d", "checks ·
     6m", "retrying GitHub 2/5").
   - `Working`: the working indicator and activity ("verifying · 6/7").
   - `Done`: nothing, or a check.
2. **Summary strip** above the tree: `3 on you · 5 on others · 2 running`.
   Each count is a filter button. Counts roll up: a parent shows its
   subtree's counts, so a collapsed branch still says whether anything in it
   is on me.
3. **Filters and sort**: "On me", "On others", "Running". Sort "waiting
   longest" works within each. Today's "needs you" filter becomes "On me".
4. **Alt+Q** walks nodes whose actor is `You`, longest first, as before, now
   including failed, stopped and not-started nodes.
5. **The "On others" list** groups by `who` (a person, "checks", a
   service) with the age, in selectable text, so the user can see "two
   things are waiting on Maria, the oldest 3 days" and go and ask. This is
   the second half of the goal.
6. **Task panel runner line** renders the same value with its detail
   (`next_check`, the retry count, the error), then the controls: Resume for
   `You::{Paused, Stopped, Failed}`, Check now for `Others`.
7. **Age emphasis**: an `Others` wait older than a threshold (a setting,
   default 24 h) draws its age in the warning color. Nothing is stored and
   there is no stale state.
8. The style guide (`doc/ui-style-guide.yaml`) gets one entry per actor
   badge; views use those, never raw colors. Reasons, errors and `who` are
   dynamic text and use `selectable_text`.

## Per phase

The same four actors apply in every phase; what differs is what produces
them:

| Phase | `You` comes from | `Others` comes from |
|---|---|---|
| proposed, design, planning | requests, phase agent asked the user, evaluation stuck | |
| active | blocked or partial steps, no plan, budget | |
| verifying, review | findings and failures to answer, stuck run | |
| pr | PR not ready; the agent handed back | |
| pr_review | `PrStuck`/`PrBlocked`, a permission, a thread the agent may not answer | human review, bot review, checks |
| approved, merged, released | failed run, auth broken | merge, release, deploy |
| learn | requests | |
| done | | |

Every phase can additionally be `Retrying` (any service call) and
`Dependency`.

## Build order

Step 1 is built: `crates/tod-core/src/next_actor.rs` (types, `derive`, batched
`for_nodes`, tests), with `WaitRepo::list_pending_for_nodes` and
`NodeRepo::get_lifecycle_entered_for_nodes`. Nothing reads it yet.

Step 2 is built in part. Done:

- **Retry fields on waits** (`waits.attempt`, `.max_attempts`, `.last_error`,
  `WaitRepo::record_failure`, `InterviewCommand::WaitFailed`). `Wait::exhausted()`
  is "the user must act". `Retrying` is not a `WaitKind` but a `retry` field on
  `ExternalWait` (the wait still has its kind); a wait whose retries are spent
  or whose error cannot be retried is `You::Failed` with the error.
- **Transient errors are classified** where GitHub errors are made
  (`GithubError::Unavailable` for 429/5xx, `is_transient`, and
  `is_transient_message` for the `String`s `PrFeed` returns, pinned to the
  variants by a test).
- **No swallowed GitHub reads.** The babysitter's two silent paths
  ("pull request not readable; leaving it to the gate", missing credentials) and
  `hold_for_github`'s `Unreadable` now go through
  `autopilot::github_look_failed`, which records the failure on a
  `github:pr:read` (or `github:release:read`) wait and ends the run waiting to
  retry (30 s, doubling, to 10 min; five attempts) or failed with the error.
  A look that reads GitHub again cancels it. `check_now` already reports its
  error to the user at once and is unchanged. Tests that relied on the old
  leniency now use a feed that reads a finished pull request
  (`FinishedGithub`); a run's thread inherits the feed override of the thread
  that started it.
- **Activity leases** (`tod_store::activity`: `node_activity`, `ActivityRepo`,
  `ActivityGuard` with a heartbeat thread, `FleetMutation::Activity*`).
  `next_actor::for_nodes` reads them: a live lifecycle lease is `Working`; a
  lifecycle lease that stopped beating is `You::Failed("processing was lost…")`
  until new work on the node clears it. A chat lease never counts for the
  actor. The local run (`autopilot::local`'s hook) holds a `run` lease while it
  works and lets it go while it waits on GitHub or a person.

Not done (step 2b, before the views can trust `Working`):

- Leases for the other processors: conversation turns the app drives itself
  (`ConversationDriver`, which holds only `&FleetStore`, so it needs a passive
  lease beaten from `tick`), `settle_gate` checks, the incoming-changes check,
  and chat turns (kind `chat`, for the decorator).
- The run's saved outcome and a pending permission as inputs from the daemon
  (`RunFacts` is still filled by the caller).
- The daemon's reconciler (desired `run` with no lease and nothing else →
  start it, or fail visibly).
- `BotReview`/`Checks` wait kinds and `who` need the babysitter to record them.
- `check_now` records no failure on the node (it reports to the user).

1. `tod_core::next_actor`: the types and `for_nodes`, with tests per row of
   the precedence list and per `YouReason`/`WaitKind`. Nothing uses it yet.
2. Make the sources durable where they are not (the last run outcome; the
   retry fields on a wait).
3. Switch the task panel's runner line from `RunnerStatus` to `NextActor`;
   delete `RunnerStatus`'s who-variants, `or_scheduled`, and `github_wait`'s
   duplicate.
4. Tree badge, summary strip, filters, Alt+Q, "On others" list, style-guide
   entries.
5. `cloud_sync` and `tod-agentd` read `NextActor` instead of their own
   attention-plus-waits checks.
6. Journeys: record a `NextActor` change per node (a node-scoped table needs
   a `journey_changes` trigger, or a documented reason it has none).

## What the code says (the four open questions, checked)

1. **No node-level dependency edge exists.** The only dependencies are
   between plan steps (`node_plan_step_deps`), inside one node. There is no
   "node A waits for node B", so **`WaitKind::Dependency` is dropped from the
   first version.** A plan step's dependency is the node's own work order,
   not an actor. Add the variant when the outline gets a node edge.
2. **Run state is only partly durable.**
   - Durable: `AutopilotState` in `autopilot/<node>.json` (the last
     `outcome`, the current conversation, the steps), and the `waits` table.
   - Not durable: `RunnerState` (`running`, `pausing`, `live`, the
     in-memory `outcome`) lives in `tod_agentd::runners` and is pushed to
     apps as `Event::Runner`. A late subscriber gets the latest; nothing
     else sees it, and the cloud supervisor keeps its own store.
   - `doc/agentd.md` already plans "desired-state rows" (run / pause / stop)
     to replace inferring "was running" from `outcome: None`. `NextActor`
     should read those rows plus the saved outcome, not `Live`. Until the
     rows exist, `next_actor` reads `autopilot/<node>.json`'s `outcome` (the
     daemon and the app share the data root) and takes `Working`/`Paused` from
     the daemon's `RunnerState` when connected.
3. **There is no retry mechanism, and one place hides errors.**
   - `waits` has `due_at` and `reschedule` ("a poll's next check, with
     backoff") but no attempt count or last error.
   - A GitHub read that fails is swallowed in two places:
     `autopilot/mod.rs` logs "pull request not readable; leaving it to the
     gate" at `info` and returns `Ok(None)`, and `hold_for_github` does the
     same with `GithubCheck::Unreadable`. Neither is shown to the user, and
     neither is counted. That breaks "Errors are never hidden" and is the
     gap `Retrying` closes.
   - Decision: the retry state goes on the **wait row** (`attempt`, `max`,
     `last_error`, schema change), and the one place that reads GitHub
     (`pr_readiness::live` and `check_github`) classifies the error as
     transient or broken. Transient → reschedule with backoff and bump
     `attempt`; broken, or `attempt == max` → the run ends `Failed` with that
     error, which is `You::Failed`. `check_now` already bails with the
     reason instead of hiding it, so it needs no change.
4. **`NotStarted` applies only to nodes with the Lifecycle capability.**
   Lifecycle states exist only on those nodes (`node_lifecycle`); every other
   node has no state to move, so it is never waiting. Among nodes with
   Lifecycle, a new one starts in `proposed` and has a phase agent whose first
   move is to ask what the node is for, so it is legitimately on the user.
   Rule: **`NotStarted` = has Lifecycle, is not `done`, and has no runner
   desired to run and no pending wait.** Make this visible but quiet: it
   counts in "On me" but not in the summary strip's alert count, and a node
   the user parked can be paused (`You::Paused`) to say "not now."
   Whether parked nodes should drop out of "On me" entirely is a product
   call (open).

### What changes in the design above

- Drop `WaitKind::Dependency` (question 1).
- Add `attempt`, `max_attempts`, `last_error` to `waits` (question 3).
- Fix the two swallowed GitHub errors as part of step 2 of the build order
  (question 3).
- Source `Paused`, `Stopped` and `Failed` from the saved outcome and the
  desired-state rows, not from `Live` (question 2).
- `NotStarted` is limited to Lifecycle nodes and quieter in the counts
  (question 4).

## Revisions after review

These supersede anything above that disagrees.

### Scope

Only nodes with the **Lifecycle** capability have a `NextActor`. Other nodes
have none and show nothing in the actor column.

### Naming: `Working`, not "agent"

The actor is the **automated processing** of the node, which is often not an
agent: a gate check (`settle_gate`), an incoming-changes check, a runner step,
a protocol turn. All of them are `Working`. `WorkKind` says which
(`Phase`, `Implement`, `Verify`, `Review`, `Fix`, `Pr`, `Evaluate`,
`GateCheck`, `IncomingCheck`), for the tooltip and the task panel.

### Persistence: push-first, observations carry their age

Two kinds of fact, treated differently:

- **Our own state** (requests, desired runner state, outcomes, deferrals) is
  authoritative and persisted. It is read, not rechecked.
- **Observations of the outside** (a PR's reviews and checks, a release) are
  persisted with `observed_at` and `source` (`push` or `poll`). A restart
  shows them at once, as of that time. The app rechecks on startup only an
  observation that has no live push source or is older than its TTL; with a
  working webhook/orchestrator feed it does not poll at all. Whatever
  receives the push (the daemon, the orchestrator, a separate session) writes
  through the same store path, and the store-change event recomputes
  `NextActor`, so it does not matter where the work ran.

`waits.detail` already holds an as-of description; `observed_at` and `source`
are added beside it.

### Errors are always visible

The rule, from "Errors are never hidden", made structural: **every failure of
anything the node waits on ends in exactly one of two visible states.**

- `Others::Retrying { service, attempt, max, error }`: the error text is on
  the badge's tooltip, in the task panel, and in the "On others" list.
- `You::Failed { error }`: the node is on the user's list, in the failure
  color, with the error.

There is no third outcome. Concretely:

- The two swallowed GitHub reads (`autopilot/mod.rs` "pull request not
  readable; leaving it to the gate" and `hold_for_github`'s `Unreadable`)
  are replaced by recording the failure on the wait. A test fails if
  `GithubCheck::Unreadable` can reach `Ok(None)`.
- **Overdue is derived, not stored.** An `Others` wait whose `next_check_ms`
  passed by more than two intervals with no new `observed_at` (the poller
  itself is dead or stuck) draws "check overdue" in the failure color. That
  catches the silent case where nothing errors because nothing runs.
- The summary strip counts failures separately from requests, so one cannot
  hide among the others.

### `Working` must be proved, not claimed

Today the tree's `lifecycle_running` comes from an in-memory set
(`feed.lifecycle`), and the daemon's "lease" is only a one-run-per-node check
in memory (`runners.rs`); I found no heartbeat. So a node can claim to be
running when nothing is. The fix is to make `Working` **unrepresentable
without evidence**:

- **Activity lease.** Everything that processes a node (an autopilot step, a
  conversation turn, a gate check, an incoming check, a chat turn) takes a
  row `node_activity(node_id, kind, owner, started_at, heartbeat_at)` and
  refreshes `heartbeat_at` every few seconds, from a guard that deletes the
  row on drop. `owner` is the process (app instance, daemon, supervisor).
- **`Working` iff a lifecycle-kind lease has a fresh heartbeat** (three
  missed beats is stale). `NextActor` never reads `Live`, a flag, or a set.
  A node cannot say Working without a lease, because that is where Working
  comes from.
- **Stale leases are reaped into a visible state.** A lease past its
  deadline is deleted and recorded as an event; the node becomes
  `You::Failed { error: "processing lost: <owner> stopped at <time>" }`, or
  is restarted by the daemon if its desired state is `run` (the restart is
  recorded too, so a flapping node shows up).
- **A reconciler** (the daemon's existing 5 s tick) checks desired state
  against evidence: desired `run`, nothing live, no pending wait, no request →
  start it; if that fails, `You::Failed` with the reason. This is the
  "supposed to be running, is not" case.
- The converse (processing with nothing recorded) is caught the same way: a
  lease for a node whose desired state is `stop` is shown and stopped.
- Chat leases (`kind = Chat`) feed only the chat decorator below, never the
  lifecycle actor.

Tests: a node with a stale lease is never `Working`; killing the owner
mid-step produces `You::Failed` within the deadline; a desired-`run` node with
no lease is restarted or fails visibly.

### Deferring a node

There is no park/defer concept today. (There is a runner **pause**, which
stops the autopilot; the node then shows `You::Paused`, and still sits on the
user's list.) Deferral is different and new: **the user's choice to take a
node off their list for a while.**

- It is not an actor. The node stays `You`; a separate `deferred_until`
  (nullable, `None` = until the user brings it back) sits beside it:
  `NodeStanding { actor: NextActor, deferred: Option<Deferral> }`.
- Stored in a table of its own (`node_deferrals`: node, `until`, created),
  written as an ordinary outline mutation, not part of a conversation's
  change set.
- Only a `You` node can be deferred. A deferred node: leaves "On me", the
  Alt+Q walk, and the alert count; draws its glyph muted with a small clock;
  shows in a "Deferred (n)" filter with its return time.
- It returns by itself at `until` (the daemon's tick, or the next recompute,
  clears the row and sends the change event) or when the user brings it back.
- Presets: 1 hour, this afternoon, tomorrow morning, 24 hours, 3 days, a week,
  a date, until I bring it back. Offered from the row's menu, the task panel,
  and the keyboard.
- A **new** failure or request arriving while deferred **wakes** the node:
  the deferral row is cleared and the node returns to "On me" with its new
  item. "New" means an item that did not exist when the user deferred it
  (compared by id and `since`), so the items the user already saw and
  deferred do not wake it. An `Others` wait that settles into `You` wakes it
  the same way.

### The status cell: one glyph per actor, a PR column, a chat decorator

What exists: one 16 px cell that **nests** a circle (lifecycle running), a
triangle (needs you) and a square (chat) so combinations fit, a ringed icon for
"a run stopped since you last selected", an incoming-changes dot, and a
separate PR cell (`status_icon_cell`, `nested_status_glyph`, `pr_icon_cell`
in `views/task_list/delegate.rs`). Nesting existed because those states
could hold together. The actor is now exclusive, so:

| Column | Shows | Notes |
|---|---|---|
| 1 Actor | one glyph: `Working` ring (today's circle), `You` triangle (today's), `Others` outlined diamond (new, muted), done: check or nothing | `You::Failed` = the triangle in the failure color with a mark; `Retrying` = the diamond in the warning color; deferred = muted triangle with a clock; unreadable/overdue = failure color. Static, as today. |
| 2 PR | as today (`pr_icon_cell`) | unchanged |
| trailing | chat decorator, below | does not reserve a column |

Reuse the existing paths (cell width, `hover_lines` tooltip, style-guide
colors); add one style-guide entry per glyph, and retire `nested_status_glyph`
and the "finished since last selected" ring for the lifecycle half (its job is
now the actor). The hover lists the actor, the reason, who, and the age, in
words.

**Chat decorator** (the chat panel is rarer and more transient, so it gets no
column). At the row's trailing edge, only when there is something to show:

- chat agent working: a spinner (the one animated element, which is fine
  because it is rare and brief);
- chat finished, or waiting on the user, and unseen: a small "new" mark;
- seen (the row selected, or the chat opened): nothing.

Most rows show nothing. It reads the chat lease for "working" and the existing
"since you last selected" bookkeeping (`finished_run`, `RunSource::Chat`) for
"unseen".
