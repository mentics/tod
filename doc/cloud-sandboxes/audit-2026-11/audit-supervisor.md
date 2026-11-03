# Audit: the wake path inside a node sandbox

Scope: `crates/tod-supervisor/src/*`, `crates/tod-relay/src/{server,hold,reaper}.rs`, `crates/tod-core/src/scheduler.rs`, `crates/tod-core/src/autopilot/{mod,state}.rs`, `crates/tod-store/src/{waits,cloud_activity}.rs`, plus the app-side code that waits for the heartbeat (`crates/tod-core/src/cloud_sync.rs` 1560-1700, 1737-1785) and the node installer (`crates/tod-sandbox/src/node.rs`). Read-only; checkout at f6ca8ff0 (main). All line numbers are in that tree.

Legend: severity is blocks-work / wastes-time / cosmetic. "Verified" means I traced the code. "Hypothesis" means the code allows it but I have no log evidence; each has a cheap way to confirm.

How the wake path works, in one paragraph. A poke (schedule, orchestrator, or app) hits `POST /poke` on the relay (`server.rs:329`). The relay takes a 60 s `poke` lease, then either SIGUSR1s the supervisor pid it started or spawns `tod-supervisor wake`. `wake()` (`lib.rs:288`) takes a 120 s hold lease (renewed by a thread), opens/seeds the replica, pulls, applies `answers::before_wake`, checks waits, restores transcripts, and only THEN calls `Beating::start` (`lib.rs:361`), which is the first thing the app can see, unless the wake ends early through the two `end_cloud_beat` calls (`lib.rs:308`, `329`). The app counts a supervisor as heard if a `cloud_activity` row is stamped >= `since - 30 s` or any supervisor change arrived (`cloud_sync.rs:1585-1596`). Idle rows count.

---

## 1. The first heartbeat comes last, and every failure before it is invisible (root of the "has not reported in after 150 seconds" reports)

Severity: blocks-work (the node may be fine, or may be dead, and the app cannot tell which).

Where: `lib.rs:288-361`, `main.rs:28-55,57-160`, `replica.rs:84-155, 182-210`, `orchestrator.rs:40-42, 69-119`, `cloud_sync.rs:1585-1596, 1656-1676`.

Scenario. The supervisor writes nothing the app can see until one of three points: `end_cloud_beat` on LeftStopped (`lib.rs:308`), `end_cloud_beat` on StillWaiting (`lib.rs:329`), or `Beating::start` (`lib.rs:361`). Everything before those points is silent, and every `?` before them ends the process with exit 1 and no heartbeat. Concretely, each of these produces the 150 s message:

| # | Path (in execution order) | Verified outcome |
|---|---|---|
| a | `main.rs:98-110`: `TOD_USER`/`TOD_NODE`/orchestrator URL/media bundle missing | `run()` Err, exit 1, nothing sent |
| b | `Replica::open` `replica.rs:97`: `changes_after(i64::MAX)` fails after 6 attempts (about 8 s of backoff plus per-attempt time) | Err, exit 1 |
| c | `Replica::open` seeding, `replica.rs:114`: `orchestrator.snapshot()` downloads the whole DB (370 MB) through the sandbox MITM proxy with a **300 s global timeout per attempt and 6 attempts** (`orchestrator.rs:40-42`, `request` loop). A download slower than 300 s restarts from byte 0. Then `std::fs::write` of 370 MB, then a second full SQLite backup copy (`sync::restore` -> `copy_database`), then `FleetStore::open` and `mutation_socket::start`. The orchestrator takes the snapshot under the user's sync lock (`sync/mod.rs:748-751`), so concurrent seeds and the app's own sync serialize behind it. | No heartbeat until all of that finishes. 150 s is easily exceeded and the supervisor is working correctly. This is the likely cause of the "Run in the cloud" and "sandbox replacement" cases (both give a fresh state dir, so a fresh seed). |
| d | `replica.rs:216-223` `pull()` bails if `seed_id` changed | Err, exit 1 |
| e | `answers::before_wake` Err (reading stop questions; `save()` of `stop-answers.json`) | Err, exit 1 |
| f | `waits::check` (`lib.rs:321`): `command_succeeds` (`waits.rs:60-80`) runs a user-recorded shell command with `.output()` and **no timeout**. A hung `gh`/`curl` check blocks forever | hang, no heartbeat; hold lease keeps being renewed (finding 7) |
| g | `push()` at `lib.rs:323` Err | Err, exit 1 |
| h | StillWaiting: `schedule(&store,&config)?` at `lib.rs:325` runs BEFORE `end_cloud_beat` at `lib.rs:329`. A Blaxel API error (`delete_schedules_named` then `create_schedule`, `scheduler.rs:175-182`) returns Err | exit 1, no idle beat, and nothing is scheduled (finding 3) |
| i | `Mirror::restore` (`lib.rs:342`, `transcripts.rs:153-171`): lists every transcript of the node and fetches each full file sequentially over the same 300 s/6-retry HTTP client, on a new sandbox | slow, no heartbeat. Errors are only `warn!`ed (`lib.rs:345`), so resume silently degrades |
| j | `Beating::start` -> `cloud_beat` (`replica.rs:275-302`) bails if the node row is not in the replica, or the push fails | Err, exit 1 |
| k | `Autopilot::new` Err (state file unreadable, `state.rs:104-112`) happens after the beat, but the run then ends with an idle beat and exit 1 | no wake scheduled (finding 3) |

Why the app cannot see why. Failures are logged through `ProblemLayer` / `problems::report` into the sandbox's local problem log. That log is forwarded to the app only by `Replica::forward_problems`, which runs inside a successful `pull()` or a `push()` that has changes (`replica.rs:224,265`). In every row above except j and k, no pull/push has happened, so the message never leaves the sandbox. The relay's own `problem(Warn, "poke: supervisor pid N exited 1")` (`server.rs:361`) is in the same log. The app's text tells the user to "check the log panel for the cloud's problems" (`views/cloud_node.rs:96-99`), which is empty in exactly these cases. The only thing that remains is the relay's stderr (the process API's logs for process `tod-relay`) and `/opt/tod/logs`.

What I checked and ruled out or downgraded from your list:
- Poke bridge lapsing before the supervisor takes its hold: no realistic path. `HoldGuard::take` is the first thing in `wake()` (`lib.rs:295`), before `Replica::open`, and the renewal thread (`supervisor/hold.rs:79-95`) is independent of the main thread. The 60 s `poke` lease would only lapse if the supervisor took more than 60 s to exec. (Its failure mode is the reverse: the lease outlives the supervisor, see finding 8.)
- `answers::before_wake` LeaveStopped and `waits::check` Waiting: both DO write an idle beat, so the app counts them as heard. They are not 150 s causes (but LeaveStopped is a different bug, finding 2), unless `schedule()` fails first (row h).
- Heartbeat written locally but push fails: `cloud_beat` pushes in the same call and returns Err (`replica.rs:300`), so `Beating::start` fails loudly (row j). The periodic thread only `warn!`s (`beat.rs:46-50`) and the row goes stale, which is the intended "shown as lost" behavior.
- Clock skew: see finding 15. The stamp comes from the sandbox clock and is compared with the app clock with only 30 s of tolerance. The ps evidence in finding 9 suggests the guest clock is correct after resume, so I rate it unlikely but unproven.
- SIGUSR1 before the handler: real but narrow, see finding 4.
- Relay not running after resume: see finding 9. If it is not running, the app's own poke (`cloud_sync.rs:1770-1779`) gets an HTTP error and reports it as "poking sandbox ... got 502", not as 150 s of silence.

How to classify the 20 incidents from existing data: every startup phase logs `startup phase done` or, past 15 s, `a slow start: this phase delayed the first heartbeat` (`lib.rs:274-285`, WARN level, so it is forwarded after the first pull). Search the app's problem log and the relay/supervisor stderr around each incident for that phrase. A phase named "open the local copy" means row c; "restore agent transcripts" means row i; no phase line at all means the supervisor never got past `Replica::open`/main (rows a, b, or a crash).

Proposed fix. (1) Make the first sign of life independent of the replica: before `Replica::open`, send a minimal `cloud_activity` change (what = "starting: <phase>") straight to `Orchestrator::push_changes` (the table has a FK to `nodes`, which the orchestrator has), and update the phase text as startup proceeds. (2) In `main()`'s error arm (`main.rs:49-53`), after `problems::report`, make a best-effort direct `orchestrator.push_problems` plus a terminal `cloud_activity` row ("failed: <error>") so the app shows the real reason; do the same around the `?`s in `wake()` (wrap `wake` in an inner function and a `finally`). (3) Reorder `lib.rs:325/329` so the idle beat is written first, then `schedule`. (4) Put real timeouts on the seed: stream the snapshot to disk with per-read timeouts instead of one 300 s `timeout_global`, support `Range` resume, and do not copy twice (restore directly, or open the downloaded file in place). (5) Put a timeout on `command_succeeds` (kill after e.g. 60 s, treat as failed). (6) App side: on the 150 s timeout, fetch the relay log and problem dir through the sandbox filesystem API (the app already has the token, `cloud_sync.rs` uses `bx.upload`/`bx.get`) and show the last 20 lines instead of a generic message.

Regression test: feasible. `crates/tod-supervisor/tests/wake.rs` already runs the supervisor in-process against `tod_orchestrator::Server`; add a test that points `Orchestrator` at a server that sleeps or returns 500 for `/snapshot`, and assert that a `cloud_activity` row with a "starting"/"failed" `what` reaches the orchestrator within N seconds. A test for row h: a `Scheduler` fake that returns Err, expecting the idle beat to be pushed first.

---

## 2. A node whose last stop question was answered "Leave it stopped" silently ignores "Run with a new session" (and every other explicit user run); the app reports success

Severity: blocks-work.

Where: `lib.rs:303-313` vs `lib.rs:318-320` and `385-389`; `answers.rs:69-94`; `stop_questions.rs:111-125`.

Confirmed control flow. In `wake()`:
1. `answers::before_wake` runs at line 303. For `Answer::Stop` it returns `Before::LeaveStopped` (`answers.rs:74-76`) unconditionally. It never looks at the state dir.
2. The `LeaveStopped` arm (`lib.rs:304-311`) calls `end_cloud_beat` and returns `Woke::LeftStopped`.
3. `fresh_requested(&config.state_dir)` is first evaluated at line 318, after that return, and `take_fresh_request` (line 387) is never reached. The `fresh-session` file stays in the state dir.

This matches the relay log: "left stopped (the user left it stopped (supervisor:budget))" at the wake the fresh-session request triggered. Consequences beyond what you saw:
- The app's `wake_node_tracked_with` sees the idle row `end_cloud_beat` just wrote (`supervisor_heard`, `cloud_sync.rs:1585-1590`), so `fresh_session()` in `views/cloud_node.rs:117-138` shows "Asked for a new session: woke the supervisor" as a success. The user is told it worked.
- The same wake swallows "Run in the cloud", "Check now", and reinstall for such a node, because `latest()` reads the answered decision on the orchestrator's DB, which a reinstall does not change. The answered decision is no longer pending, so the user has no obvious place to flip it.
- The `fresh-session` file persists. If the user later answers "Keep going", the stale request is taken then and starts a new session unexpectedly.
- A "Leave it stopped" on `supervisor:budget` means a later explicit run would hit `BudgetExhausted` again anyway unless the budget is granted (`answers.rs:80-82`).

Exact fix. Treat an explicit user run request as a newer answer than the old stop answer:
- In `wake()`, compute `let explicit = fresh_requested(&config.state_dir);` before `before_wake` and pass it: `answers::before_wake(&store, node, &state_dir, budget, explicit)`.
- In `before_wake`, when `latest.answer == Answer::Stop` and `explicit`: log it, add `latest.answer_key` to a new `superseded: Vec<String>` in `stop-answers.json` (so later scheduled wakes do not re-apply the old Stop; otherwise the first scheduled wake after the fresh run would LeaveStopped again), and fall through as `Continue`. For `BUDGET`, also `budget_grants += 1`, exactly like Keep going. In the `Stop` arm, also return Go if `acted.superseded` contains the key.
- Leave `take_fresh_request` where it is (it now runs).
- Better, because it covers every explicit run and keeps the decision history truthful: have the app, in `cloud_sync::wake_node_with` (and `start_in_cloud`), append a "Keep going" user answer to the node's latest stop question when `stop_questions::latest` is `Stop`, and sync it before the poke. The supervisor-side guard above stays as the second line.
- Independently, a LeftStopped wake must not look like success: the app should show "Left stopped because you answered X" using the wake outcome (see design change D3), not just the idle beat.

Regression test: feasible and cheap. `crates/tod-supervisor/tests/answers.rs` has `setup`, `answer(&Setup, option)`, `wake_once`, and `fake_relay`. Add: budget question answered option 2 (Leave it stopped); write `FRESH_SESSION_REQUEST_FILE`; `wake_once` must return `Woke::Ran`, the file must be gone; a second wake with no file must not return LeftStopped for the same answer key.

---

## 3. Any `Err` out of `wake()` strands the node: its scheduled wake is cancelled at the start and only recreated on the success path

Severity: blocks-work.

Where: `lib.rs:336-338` (cancel), `lib.rs:457-475` (cleanup and schedule), `waits.rs:197-205`, `main.rs:47-54`.

Scenario. When the wake has work to do, `cancel_scheduled` (line 337) deletes the node's `wait-<id>` Blaxel schedule and removes the `scheduled-wake` file. The replacement is only created at line 473, and only when `run` returned `Ok` with `Stopped`/`Waiting`. Every other exit leaves nothing scheduled:
- `run?` at line 469 returns any error from the autopilot loop. These include `replica.push()?`/`pull()?` in `Hook::at` (`lib.rs:177-184`) after the orchestrator client's 6 retries, a `pull()` seed-id bail, `clear_live_turn`, `Autopilot::new`/`AutopilotState::load` failing on a bad state file, `usage_limit::record`, `take_fresh_request`, `context::take`, `ask`, and `schedule` itself.
- Process death: VM restart, OOM, SIGKILL, SIGUSR1 before the handler (finding 4), the 120 s kill of a schedule-launched supervisor (finding 6).
- `outcome` of `Done`, or `NeedsHuman` of any kind other than the ones that ask a decision, schedules nothing by design, but then relies on a user answer poke, which has its own lost-poke window (finding 4).

After such an exit: the heartbeat row is idle or stale (the app shows the node as lost or waiting), the relay's `problem` warning stays in the local log (finding 1), no schedule exists, the orchestrator's poke retry only covers failed pokes (`wakes.rs:234-243`, retried every 60 s, and only when the poke itself failed). The node is dead until a human acts. A 10-second orchestrator blip during a long run is enough: `request()` gives up after about 8 s of backoff.

Proposed fix. Replace "cancel, run, reschedule" with a dead-man switch. At the point where line 337 cancels, schedule a retry wake (`now + 10 min`, using a distinct schedule name such as `wait-retry-<node>`), and cancel it only on an orderly end of `wake()`. In `main()`'s error arm, schedule a backoff retry wake (1, 2, 5, 15 min, tracked in the state dir) through the scheduler, which talks to the local Blaxel API and does not need the orchestrator, so it works when the orchestrator is the thing that failed. For `scheduler = "orchestrator"` the same applies via `POST /wakes` and if that is what is down, fall back to a local `sleep N && curl .../poke` process started through the process API at `127.0.0.1:8080`. Also make `Outcome::Waiting { due_at_ms }` (`autopilot/mod.rs:94`) authoritative: if `schedule_next` finds no pending wait row for a `Waiting` outcome, schedule `due_at_ms` anyway (`waits.rs:136-146` returns `None` and `lib.rs:473-475` then silently schedules nothing).

Regression test: feasible. In `crates/tod-supervisor/tests/wake.rs`, run a wake against a server that starts failing after the first pull and a recording `Scheduler` fake; assert a retry schedule is created when `wake` returns Err.

---

## 4. Pokes are edge-triggered with no acknowledgement, so a poke can be lost three ways

Severity: blocks-work (rare per wake, but the lost event has no recovery: webhook-satisfied waits can then sleep until their deadline, up to 24 h by `DEFAULT_EVENT_DEADLINE_SECS`).

Where: `server.rs:313-374`, `signal.rs`, `lib.rs:178-180,457-479`, `main.rs:28-33`, `wakes.rs:234-243`.

4a. Lost in the exit tail (verified). The supervisor reads the poke flag only in `Hook::at` (`lib.rs:178`), and then only to log it. After the last `at`, the process still runs `beating.finish()` (HTTP push), `git::push_branch` (blocking, unbounded `git push`), `follow.stop()` (final transcript pass), `schedule()` (two Blaxel API calls), and the hold release (up to 10 s). A SIGUSR1 in that window sets `POKED` and the process exits; the relay logged "poke: signalled supervisor pid N" and returned 200. The orchestrator treats 200 as delivered (`poke_now` only retries failures). Same for the StillWaiting path: a webhook that satisfies a wait while `schedule()` runs is dropped, and the schedule it just created is for the old deadline.
   Also `alive(pid)` (`server.rs:313`) is `kill(pid, 0)`, which succeeds for a zombie that tokio has not yet waited on, so a poke after exit but before the wait task runs is also swallowed.

4b. Killed before the handler exists (verified). The relay spawns `/bin/sh -c "exec /opt/tod/tod-supervisor wake --workspace \"$(if [ -d ... ])\""` (`server.rs:103-108,347`). The tracked pid is the shell's, which is in a command substitution (a fork) and then dynamic-start of the binary for a few to tens of milliseconds before `signal::install()` (`main.rs:32`). SIGUSR1's default action terminates the process. A second poke inside that window kills the supervisor: no heartbeat, no schedule, relay logs exit 138, and the node's first poke is lost. Likely trigger: two pokes within ~30 ms (the orchestrator pokes for an answer and for a wait change in the same sync; the app's own poke races the orchestrator's).

4c. Ignored mid-turn. A poke during a long agent turn only sets a flag; the pull that would see an answered decision, a cancelled wait, or a context change happens at the end of the turn (`Syncing::poll_run` pulls on turn end, `agent.rs:123-128`; `watch()` checks only the fresh-session file, `lib.rs:138-144`). Not a loss, but "Check now" / answering a question does nothing until the turn ends (up to the 15 min hang limit, longer for slow turns).

Proposed fix. Make pokes level-triggered. (1) Relay: keep `poke_gen: AtomicU64` and `seen_gen`; every poke increments it. Add `POST /poke-seen?gen=N` (or the supervisor passes the gen it started under in an env var and re-reads `GET /poke-gen`). In the wait task at `server.rs:359`, after `child.wait()`, if `poke_gen > seen_gen`, start a new supervisor. (2) Supervisor: call `poke-seen` at start of `wake()`, at each `at`, and as the last step before the hold is released; if a poke arrived since the last look, loop back to the start of `wake()` instead of exiting. (3) For 4b, in the relay's `cmd` set `pre_exec(|| { libc::signal(SIGUSR1, SIG_IGN); Ok(()) })`; an ignored disposition survives `exec`, and `signal::install()` replaces it with the handler. (4) For 4a also check `/proc/<pid>/stat` state is not `Z` in `alive`. (5) For 4c, have `Hook::watch` check `signal::take_poke()` and do one cheap `replica.pull()` + `answers`/`context` check at most every 10 s; stop the turn only if something actionable changed.

Regression test: feasible for 4a/4b in the relay. `tod-relay` is Linux-only, and CI runs Linux. Add an integration test that starts the relay `server::main` on an ephemeral port with `--supervisor-cmd` set to a script that sleeps 1 s then exits 0, POSTs `/poke` twice (once at +0.5 s, once at +1.05 s, i.e. in the exit tail) and asserts the script ran twice; and one that pokes twice within 5 ms and asserts one child was not killed. The supervisor side (loop back when poked at the end) can be tested in `tests/wake.rs` with `signal::poke()` (already exposed) called from a fake `Scheduler::schedule`.

---

## 5. Replica seeding is unbounded and re-done more often than necessary

Severity: wastes-time (blocks the first heartbeat, see finding 1) and a ticking time bomb: it gets worse as the DB grows.

Where: `replica.rs:84-155`, `orchestrator.rs:40-42,131-133`, `sync/mod.rs:748-759`.

Scenario. A seed happens when: the state dir is new (new or replaced sandbox: every "Run in the cloud"), the cursors file is missing or unparsable, `seed_id` differs, or `tod.db` is missing (`replica.rs:88-108`). Costs, in order: orchestrator `copy_database` under the user's sync lock; the full response body into a `Vec` (370 MB of RAM) via `read_to_end` (`orchestrator.rs:101-104`); `std::fs::write` of 370 MB; `sync::restore` (another full backup copy); `FleetStore::open`; `flush_on_quit`. The request has `timeout_global(300 s)` covering the body; on timeout it retries from byte 0, up to 6 times, so the worst case is about 30 minutes of silence. Also:
- `save_cursors` (`replica.rs:169-174`) writes a temp file and renames it with no fsync. After a VM crash/restart the cursors file can be empty or missing while `tod.db` is intact, which triggers a full reseed (silently: only an info log). `Cursors` parse failures use `.ok()` (`replica.rs:88-91`).
- The supervisor needs only this node's subtree and shared settings, not the user's whole database, yet it receives all of it, including whatever makes it 370 MB.

Proposed fix. Short term: stream to disk with a read timeout instead of a global one and resume with `Range` (the orchestrator already has the file), drop the double copy (open the downloaded file as `tod.db` directly), fsync the cursors file and write it atomically after the DB is closed cleanly, and log the reason a reseed happens as WARN with the cause (missing cursors vs `seed_id`). Medium term: find what makes the DB 370 MB (conversation turns, `conversation_live`, agent traffic, transcripts) and exclude it from the snapshot sent to supervisors; see design change D5.

Regression test: feasible. `tests/reseed.rs` exists; add one that serves `/snapshot` slowly (a few KB/s with a 1 s read timeout) and asserts resume or progress.

---

## 6. Two supervisors can run at once, and a schedule-launched one is killed after 120 s

Severity: blocks-work.

Where: `node.rs:897-899` (`wake_command`), `scheduler.rs:164-166`, `blaxel.rs:773-781`, `server.rs:62-63,329-374`, `node.rs:703-706`, `lib.rs:288-300`.

6a. Verified. The schedule command is `curl -fsS -m 20 -X POST http://127.0.0.1:2222/poke || exec /opt/tod/tod-supervisor wake ...`. If curl fails for any reason (relay restarting, busy for more than 20 s, connection reset), the supervisor is exec'd directly by the process API, not by the relay. The relay does not know its pid, so the next poke starts a second supervisor on the same state dir, the same SQLite replica, and the same agent session. There is no lock: `wake()` takes none, and `mutation_socket::start` just overwrites its port file. (Same for the app's "Reinstall and restart": `node::start` kills the relay at `node.rs:703` and starts a new one with an empty `supervisor` mutex while the old supervisor, a separate process, may still run.)

6b. Verified. The schedule's process is created with `"timeout": 120` and `keepAlive: true` (`blaxel_schedule_body`: `schedule_body(..., true, 120)`). `exec` keeps it the same process, so the process API kills the directly-started supervisor 120 s after it starts. It dies mid-seed or mid-turn, with no hold release, no heartbeat end, and no reschedule (finding 3). The fallback supervisor also has no relay for its hold (`HoldGuard::take` only warns, `supervisor/hold.rs:75-77`), so the sandbox can go to standby under it.

Proposed fix. (1) A single-instance lock: at the very start of `wake()` (before `Replica::open`), `flock(LOCK_EX|LOCK_NB)` on `<state_dir>/supervisor.lock` and write the pid. If held, send SIGUSR1 to that pid and exit 0 (with the level-triggered poke from finding 4, mark the poke as pending). This makes "one supervisor per node" independent of how it was started. (2) Change `wake_command` so it never `exec`s the supervisor under a 120 s timeout: on curl failure, start the relay through the process API (`curl -X POST 127.0.0.1:8080/process` with `{"name":"tod-relay","command":"exec /opt/tod/tod-relay --port 2222","restartOnFailure":true,"maxRestarts":100,"timeout":0}`), then poke again. (3) `HoldGuard::take` must return an error when the relay cannot be reached and the supervisor must report it rather than run unheld (the project rule: errors are never hidden).

Caveat: the relay's `TOD_SUPERVISOR_ENV_*` (the Claude token in `claude_token_via = "env"` mode) lives only in the relay's own environment, so an env-mode relay cannot be restarted from inside the sandbox. In the default proxy mode (`config.rs:369-373`) it can.

Regression test: (1) feasible: `tests/wake.rs` can start two `wake()`s on the same state dir and assert the second exits without opening the replica. (2) `wake_command()` is already unit-tested as a string (`node.rs:1047`); add a test on the fallback text.

---

## 7. The supervisor's hold is renewed unconditionally, and only agent turns have a hang guard

Severity: wastes-time (cost) and a stall amplifier.

Where: `supervisor/hold.rs:79-95`, `guard.rs:160-213`, `waits.rs:73`, `git.rs:11-22,50`, `autonomous-nodes.md` "Holding the sandbox awake" ("renewed ... only while the agent is making progress").

Scenario. The doc says the lease is renewed only on progress. The code renews every 40 s from a background thread regardless of what the main thread is doing. A supervisor wedged anywhere outside an agent turn keeps the sandbox awake: `command_succeeds` (`.output()`, no timeout), `git push` (`git.rs:50`, blocking, no timeout; a proxy credential hang), a blocking orchestrator request (300 s x 6), the replica mutex held across HTTP in the heartbeat thread (`beat.rs:85-88`) versus the main thread's pushes, or `Follow::stop` joining a thread stuck in HTTP. Only the watchdog ends it, at `max_awake_secs` = 6 h (`watchdog.rs:69`), and meanwhile the relay treats the wedged pid as alive, so later pokes only SIGUSR1 it (finding 4) and the node never recovers. Conversely, while main is wedged without holding the replica mutex, the heartbeat thread keeps writing fresh beats, so the app shows "working" for a dead node and `start_in_cloud` reports success.

Proposed fix. (1) A shared `Progress` counter (`AtomicU64` of last-progress milliseconds), bumped from `Guarded::poll_run` activity changes, `Hook::at/watch`, completed pushes/pulls, and phase boundaries. `HoldGuard` renews only if the counter advanced since the last renewal; the heartbeat thread reports `stalled` after N minutes without progress. (2) A global supervisor watchdog thread: no progress for 20 minutes means record a problem, schedule a retry wake, and `std::process::exit(2)`. (3) Timeouts on `command_succeeds` (spawn, `wait_timeout`, kill) and on `git push` (`GIT_HTTP_LOW_SPEED_LIMIT`/`TIME`, plus a process timeout). (4) The relay should kill a supervisor whose heartbeat/progress file has not changed for N minutes before signalling it.

Regression test: feasible. Unit test `HoldGuard` with a `Holder` recorder and a progress source that stops advancing (extend `hold.rs` tests); a `waits::check` test with `sleep 30` and a 1 s timeout.

---

## 8. Relay hold: the keepAlive process is restarted every second for the last 20 s of a lease; the `poke` lease is never released

Severity: wastes-time (per-wake sandbox time and API churn); not a correctness bug in what stays awake.

Where: `tod-relay/src/hold.rs:32, 119-127, 208-249`; `server.rs:46, 329-330`; `supervisor/hold.rs`.

This is your item 3. Explanation, verified by reading `reconcile`:
- `timeout_secs` (`hold.rs:123-127`) sets the keepAlive process's `timeout` to the **shortest** lease still open, as a whole number of seconds (floor). So the process is clipped to expire at (or just before) the nearest lease deadline.
- `needs_restart` (`hold.rs:223-229`) restarts when `remaining <= RENEW_MARGIN (20 s) && desired > remaining`. Once a clipped lease is within 20 s of ending, `desired` is `floor(secs left on the lease)` and `remaining` is the process's exact remaining time; because the process was created with the floor, `desired > remaining` is true on almost every one-second tick (it holds whenever the lease deadline's fractional second plus tick latency drift is at least 1). Each tick therefore kills the old `sleep` (`DELETE /process/<name>/kill`) and starts a new one with the new, one-second-smaller timeout. That is the log you saw: `timeout 20s`, `19s`, ... at one per second, about 20 processes per episode (and the `-37` suffix says the relay had made 37 by then).
- The lease that is within 20 s of ending and shorter than everything else is almost always the `poke` lease: every poke takes 60 s (`server.rs:330`), and nothing releases it when the supervisor takes its own hold or exits. After a wake that finishes in 10 s, the sandbox stays held for the remaining 50 s plus about 15 s of standby delay, with ~20 kill/start cycles at the end. The doc says standby comes about 15 s after the supervisor releases.
- A crashed supervisor leaves `ext:supervisor` (120 s) behind: the relay learns about child exit at `server.rs:359` but does not release it.

Costs: per wake, about 45 extra seconds awake (so at 1-2 minute poll intervals the node is awake most of the time), two process-API calls per second for 20 s, and one named process record per call in sandbox-api (I did not check whether it prunes them; a restart-per-second pattern could grow its table, unverified).

Proposed fix. (a) The process timeout should follow the **longest** open lease (the relay itself kills the process when the last reason ends; the timeout is only a backstop if the relay dies), capped at `max_secs`; compute `desired_secs` as `max`, not `min`, in `timeout_secs`. (b) Make renewal compare with hysteresis in milliseconds: restart only when `expiry - now <= RENEW_MARGIN` and `desired_ms >= remaining_ms + RENEW_MARGIN`. (c) Start the new process before killing the old one, so there is no zero-hold gap, and log the kill error (`let _ = api("DELETE" ...)` at `hold.rs:218,234` swallows it). (d) On `HttpAction::Hold` for reason `supervisor`, release the `poke` lease; in the exit task (`server.rs:359`), release both `ext:supervisor` and `poke` when the supervisor child exits. (e) Rate-limit the `hold: could not start` problem (`hold.rs:244-247`): while the process API is unreachable (just after a resume) it records an Error every second.

Regression test: feasible and cheap, because `HoldState`/`timeout_secs` are pure and take an explicit `now`. Extract `needs_restart(now, expiry, desired)` and test: with a single 15 s lease and a process created with timeout 15, simulate 15 one-second ticks with 5 ms drift and assert at most one restart. Add a test that releasing/exiting the supervisor drops `poke`.

---

## 9. What happens to the relay and a running supervisor when the sandbox "restarts"

Severity: blocks-work if a real VM restart happens; otherwise cosmetic (an observation artifact).

Where: `node.rs:703-706` (relay start), `blaxel.rs:576-603` (`restartOnFailure`, `maxRestarts: 100`), `provision.rs:37-39`, `node.rs:897-899`.

Your item 4. How the relay is started: through the sandbox process API with `restartOnFailure: true, maxRestarts: 100, timeout: 0` (`start_with_env`, `blaxel.rs:587-603`; `node.rs:706`). That restarts the relay if it exits while sandbox-api is alive. It does not survive a VM restart: the process API's table is in memory. Nothing in the node's wake path (relay, supervisor, schedule command) re-creates it. The only places that do are app-driven: `node::start`/`provision` during "Run in the cloud" or reinstall (`cloud_sync.rs:1387-1396`), and `provision.rs:302` for non-node sandboxes. The app's `wake_node_with` (fresh session, Check now) does not check or restart the relay: it pokes, and a dead relay gives an HTTP error. The schedule's fallback (finding 6) starts a supervisor without the relay.

What a mid-turn supervisor experiences:
- Standby that preserves memory: the process is frozen and continues on resume, but its TCP connections (Anthropic stream, orchestrator via the proxy) are dead. The agent turn fails or the HTTP request errors, the guard retries. Wall-clock based time counters (`AutopilotState::active_ms` via `now_ms`) include the frozen time.
- A real restart: supervisor and agent die with no cleanup. `cloud_activity` is left non-idle and goes stale after 60 s (the app shows it as lost). The scheduled wake was cancelled at wake start (finding 3), the relay is gone, so nothing wakes it. The orchestrator's pokes fail and are retried every 60 s forever (`wakes.rs:234-243`) without ever restarting the relay. The `autopilot/<node>.json` and `supervisor-sync.json` were written with rename-without-fsync (`state.rs:117-123`, `replica.rs:169-174`), so a truncated file is possible and then permanently breaks `AutopilotState::load` with a hard error on every wake.

Important caveat on your evidence. `ps` START times after a snapshot/restore can all read as "the resume time" even when nothing restarted (START is derived from boot time = wall now - uptime, and wall clock jumps forward across a snapshot while uptime does not). The relay log you quoted names `tod-relay-hold-1567-37`: the relay pid 1567 and a counter of 37 mean the same relay process lived across many holds. Compare, before and after a standby: the relay's pid, `/proc/1/stat` starttime vs `cat /proc/uptime`, and `curl -s 127.0.0.1:8080/process` (does `tod-relay` still show `running` with the same pid). If the pid is unchanged it was a memory-preserving resume and nothing restarted; if it changed, the points above apply. Either way, as a side effect, a correct wall clock at resume (which this ps artifact implies) argues against the clock-skew explanation in finding 15.

Proposed fix. Make the relay recoverable from inside the sandbox: change `wake_command` as in finding 6 so a failed poke restarts the relay through `127.0.0.1:8080` (default proxy mode only; env mode cannot, see the caveat there). Make the supervisor fsync state files. In `wake_node_with`, check the relay's liveness through the process API (`bx.process_status(url, "tod-relay")`, already implemented at `blaxel.rs:606`) and restart it if it is not running, instead of poking into a 502.

Regression test: partly. The process-status check and the wake-command text are unit-testable; the VM-restart behavior needs a live check (the live-verify skill).

---

## 10. Retries have no backoff and the failure counter resets every wake

Severity: wastes-time and a generator of human questions.

Where: `lib.rs:410-439`, `guard.rs:244-250`.

Scenario. On `NeedsHuman::AgentFailed`, `on_failure` returns `Retry` for failures 1 and 2 and the loop does `continue` immediately, so three attempts happen within seconds. A transient condition (Anthropic overload or 5xx, a network blip through the proxy, a stale credential refresh, a bad adapter start) uses up all three, then the node asks the user "Keep going?" even though it would have worked in a minute. `failures` is a local in `wake()` so it also resets on every process start, which makes the threshold unreliable in the other direction too: a node that fails once per wake never accumulates.

Proposed fix. Count consecutive failures in the state dir (or the replica), retry after a backoff by recording a short `until` wait (1, 5, 15 min) and scheduling the wake, and ask the user only after the backoff schedule is exhausted. Use the existing machinery: `usage_limit::record` already does this for limits.

Regression test: feasible with the mock agent and a fake `Scheduler` in `tests/wake.rs`; assert a wait is recorded and the question is not asked on failure 3.

---

## 11. Context-seen resets on a new state dir, and rotates sessions that should resume

Severity: wastes-time.

Where: `context.rs:34-47`, `lib.rs:195-198, 378`.

Scenario. `context::load_seen` returns 0 for a missing or unparsable file (`.ok()...unwrap_or(0)`). The orchestrator's `cloud_nodes.context_changed_at` is persistent. On any new state dir (a replaced or reinstalled sandbox) `changed_since(.., 0)` is true at the first boundary whenever the node has ever had a context change, so the supervisor stops and `context::take` closes the conversation's session and clears its `agent_session_id` (`context.rs:94-110`). The restored transcripts the supervisor just spent time downloading (finding 1 row i) are then thrown away for one rotation. A corrupted `context-seen` file does the same silently.

Proposed fix. Initialize `context_seen` to the current mark when the replica is first seeded (nothing to take on a node that has just been installed with the current context), and fail loudly on a malformed file instead of `.ok()`. Better: store `context_seen` in the replica DB (it is derived state about the same node, and a reseed then resets it consistently).

Regression test: feasible in `crates/tod-supervisor/tests/context.rs`.

---

## 12. Errors swallowed against the "errors are never hidden" rule (inventory)

Severity: wastes-time to blocks-work depending on the item.

Verified instances, with what is lost:

| Where | What | Consequence |
|---|---|---|
| `lib.rs:187-189`, `461-465` | `push_branch` failure only `warn!` at every step and at the end | The node continues (even to done/PR) with unpushed commits; the `pr` step then fails far from the cause, and a lost sandbox loses code, which is the exact thing the push exists to prevent |
| `lib.rs:342-346` | transcript restore failure only `warn!` | Session resume silently degrades to a fresh session |
| `main.rs:145-151` | scheduler `from_env` failure becomes `None` plus a warn; `schedule()` then warns "nothing will wake this node but a poke" and returns Ok (`lib.rs:213`) | A node with waits and no way to be woken goes to sleep as if all is well |
| `main.rs:152` | `TOD_SANDBOX` missing falls back to the guess `node-<uuid>` | Schedules are created against a sandbox name that probably does not exist |
| `supervisor/hold.rs:75-77` | `hold()` failure only `warn!` | The supervisor runs with no hold (finding 6b) |
| `replica.rs:125, 227, 250` | `let _ = self.store.flush_on_quit()` | If the flush fails, queued writes never reach the DB and are not exported, so conversation turns and lifecycle moves are silently not pushed |
| `replica.rs:235, 378` | `let _ = self.store.reload_if_stale()` | Stale cached view after a pull |
| `replica.rs:119, 122` | `let _ = remove_file` of the old DB and the incoming snapshot | A failed remove leaves a 370 MB file or an old DB under the new one |
| `answers.rs:41` | corrupt `stop-answers.json` read as default | `budget_grants` silently reset to 0 and old answers re-acted |
| `context.rs:35` | as finding 11 | |
| `waits.rs:154-156` | unparsable `scheduled-wake` read as `None` | A stale wake is not cancelled (harmless poke) |
| `waits.rs:186-189, 199-203` | failed cancel of a stale wake only `warn!`, then the file is cleared anyway (`waits.rs:203`) | The stale schedule can no longer be found by the supervisor; harmless poke later, but a leak |
| `relay/hold.rs:218, 234` | `let _ = api("DELETE" ..)` | A failed kill leaves a keepAlive process holding the sandbox past its release |
| `relay/hold.rs:244-247` | start failure retried every second with an Error each time | Problem-log flood |
| `transcripts.rs:75` | `DirStore::fetch` uses `unwrap_or_default()` | A read error returns empty bytes, and `restore` then overwrites the local session file with an empty file (`transcripts.rs:161-165`) |
| `tod-store/src/sandbox_http.rs:20` | CA bundle read `.ok()` | A missing `SSL_CERT_FILE` silently gives default roots and TLS failures with no hint |
| `Syncing::push`/`pull` (`agent.rs:65-77`) | failures only `warn!` | A turn is sent with unsent conversation rows, which the doc says must exist first |

Proposed fix. For each: return the error to a place that records it where the user can see it (a `NeedsHuman` outcome with the reason or a problem plus a retry wake), instead of `warn!` and continue. At minimum make the three that change what the node does: `push_branch`, scheduler absence, and hold failure.

Regression test: feasible for most as unit tests with failing fakes.

---

## 13. State files that can be stale or left behind

Severity: wastes-time, one case blocks-work.

| File (`<state_dir>/...`) | Written by | How it goes stale or is left |
|---|---|---|
| `fresh-session` | app upload, deleted by `take_fresh_request` | Left behind forever by a LeftStopped wake (finding 2); left across an outage, then executed much later, after the user has forgotten it; an Err before `lib.rs:387` leaves it, which is intended, but then the next wake runs it. No expiry or timestamp inside it |
| `scheduled-wake` | `reconcile_wake` | After a crash between `schedule` and `save_scheduled` the file is missing, so the old schedule is never cancelled; the file is deleted even when cancel failed (`waits.rs:203`); reset to nothing by a new state dir while the old sandbox's schedules... go away with the sandbox, fine |
| `context-seen` | `save_seen` plain write | Finding 11 |
| `stop-answers.json` | `answers::save` plain write | Finding 12; `handled` grows without bound |
| `autopilot/<node>.json` | `AutopilotState::save`, atomic rename but no fsync | A truncated file after a crash makes `AutopilotState::load` fail on every wake with no self-repair (`state.rs:104-112`), and `Autopilot::new` Err ends every wake (finding 3). It also survives a reseed of the DB (`Replica::open` removes only `tod.db*`), so `current.conversation_id` can point at a conversation that does not exist in the new DB |
| `supervisor-sync.json` | `save_cursors` | Finding 5 (reseed on loss; divergence if the DB is older than the cursors) |
| `settings.yaml`, `databases` context | app upload | Only refreshed by an app-driven wake; a scheduled wake uses the last upload |

Proposed fix. Put `fresh-session`, `scheduled-wake`, `context-seen`, and stop-answer bookkeeping in the replica DB (or one `state.json` written atomically with fsync); stamp requests with the time they were made and expire them; make `AutopilotState::load` treat an unparsable file as an error that moves the node to `NeedsHuman` with a clear "reset autopilot state" action rather than looping.

Regression test: feasible per file in `tests/`.

---

## 14. The heartbeat thread and the main thread share one mutex across HTTP

Severity: wastes-time (false "lost" and false "working").

Where: `beat.rs:85-88`, `replica.rs:275-302` (`cloud_beat` calls `push()` while the caller holds the lock), `lib.rs:156-160,177-184`.

Scenario. `Beating` locks the replica for the whole of `cloud_beat`, which includes a push over HTTP (up to 6 attempts of up to 300 s). While that runs, the main thread's `Hook::at`, `watch`, and `Syncing::push` block on the same lock; while the main thread is doing its own push, the next beat blocks. A slow orchestrator therefore stalls both the autopilot and the heartbeat, the app marks the node lost after 60 s (`cloud_activity::STALE_AFTER_MS`), and the supervisor is not actually dead. In the opposite direction (finding 7), a wedged main thread that is not holding the mutex still produces fresh beats.

Proposed fix. Write the beat row under the lock, then release it and do the HTTP outside (build the change list under the lock, send after). Report `stalled` in `what` when the progress counter (finding 7) has not advanced.

Regression test: feasible: a slow fake orchestrator plus a counter of beats during a long `push`.

---

## 15. Heartbeat time is stamped on the sandbox clock and judged on the app's

Severity: wastes-time (hypothesis; low probability given the evidence in finding 9).

Where: `replica.rs:297` (`now_ms()` in the sandbox), `cloud_sync.rs:1585-1596` (30 s tolerance on `heartbeat_at >= since - 30 s`), `cloud_activity.rs:41` (`is_live`: 60 s total, of which the beat interval is 15 s and the app's sync lag up to its poll interval).

Scenario. A sandbox clock more than 30 s behind the app's makes every heartbeat look older than `since`, so a healthy supervisor is reported silent after 150 s. A clock more than about 15-20 s behind makes a working node flap to lost. In the other direction, an idle row stamped within 30 s before the click (a double click, or a wake that just finished) is accepted as hearing the new run. `waits::check` also compares `due_at` (recorded by the agent on the sandbox clock) with the sandbox clock; a clock that is behind would turn a due timer into "still waiting" and the scheduler (`MIN_LEAD_MS` clamp against the sandbox's `now_ms`) into a loop of wakes every ~10-50 s.

How to check cheaply: the relay and supervisor log lines carry UTC timestamps (`log.rs`); compare one with the app's log of the same event, right after a resume.

Proposed fix. Stamp `heartbeat_at` on the orchestrator when it applies a `cloud_activity` change (one clock for all nodes), or return the orchestrator's time in the sync report so the app compares like with like. Judge the 150 s wait by "a beat newer than the one seen before the click" (compare the previous `heartbeat_at` value, not a time), which needs no clocks.

Regression test: feasible: `cloud_sync/tests.rs` with a skewed fixture.

---

## 16. Smaller items

- `usage_limit.rs:116-137`: zone offsets ignore daylight saving; in summer a "resets 3pm (America/New_York)" reads an hour late (documented). It makes the node sleep an hour past the real reset. Use `chrono-tz` or the epoch suffix when present. Wastes-time.
- `usage_limit::record` (`usage_limit.rs:140-147`) adds a new `until` wait on each hit without cancelling the previous one; with the `FALLBACK` of 1 h for an unparsable weekly limit, a node spends a full agent start every hour. Cosmetic/wastes-time.
- `transcripts.rs:219-246`: `sync_file` reads the whole file every 2 s for every session file under the projects dir (O(size) per pass). Wastes-time on long sessions.
- `answers.rs:98-101` `still_asking` treats a non-withdrawn pending question as an existing ask; fine, but `ask()` can be called after a `Retry` loop re-enters for an unrelated reason; I found no bug.
- Watchdog `max_awake_secs` is 6 h but the autopilot budget is 8 h of active time (`autopilot/mod.rs:68-71`); a legitimate long run is released by the watchdog and must wait for its renewal (up to 40 s) to take its hold again, with a standby possible in between. Wastes-time, out of scope but related.
- `server.rs:186-229` `try_poke` peeks one TCP read; a request line split across segments falls through to the WebSocket handshake and the poke is dropped with no log. Cosmetic.
- `server.rs:347-353`: the supervisor inherits the relay's stdin and process group; "Reinstall and restart" (`bx.kill("tod-relay")`) may kill it with the relay depending on how the process API kills (unverified).

---

## Design changes that would remove whole classes of failure

D1. One supervisor per node, enforced by a lock file, and a level-triggered poke. A `flock` on the state dir ends duplicate-supervisor bugs (finding 6) regardless of who launched it. A `poke_gen` counter in the relay, acknowledged by the supervisor and re-checked before exit and by the relay after child exit, ends lost pokes (finding 4). A poke then means "look again", and cannot be dropped.

D2. A dead-man schedule instead of "cancel then reschedule". Never leave a window where no wake exists: before work, replace the wait's schedule with a retry wake; on orderly end replace it with the real one; on any error schedule a backoff retry. This removes the whole "node strands after an Err/kill" class (finding 3, parts of 6 and 9).

D3. Every wake ends by recording an outcome, even on failure. A `wake_result` (started/phase, finished: ran/still waiting/left stopped because X/failed: error) is written straight to the orchestrator without the replica, and the app waits for a result or a heartbeat, and shows the result's text. This replaces "no news after 150 s" with the real reason and removes the false successes (LeftStopped reported as success).

D4. Early liveness decoupled from the replica. A cheap "starting: <phase>" beat before seeding, stamped by the orchestrator, so slow seeding (finding 5) is visible as progress, not silence, and the app's timeout can be about progress (a phase that has not advanced in 60 s) instead of a fixed 150 s.

D5. A node-scoped replica. The supervisor needs the node's subtree and a few settings. Seed only that (compressed, resumable), or seed it during install from the app (it already uploads files through the sandbox API while the sandbox boots), instead of downloading a full-database snapshot from the orchestrator under the user's sync lock.

D6. Durable explicit user requests as rows. "Run now", "new session", "check now", "keep going" as append-only rows on the orchestrator that the supervisor consumes and acknowledges, instead of marker files plus the state of an old stop question. A request can then be neither ignored by an old answer nor left behind, and the app can show "consumed at T".

D7. Progress-bound everything. One `Progress` marker that gates hold renewal, the heartbeat's "working" status, a supervisor-level watchdog that exits and schedules a retry, and the relay's kill-if-no-progress for its supervisor. Add timeouts to every external call (`check` commands, git push, per-read HTTP timeouts rather than one global 300 s).

D8. The relay as the one owner of liveness. It starts the supervisor, releases its holds when it exits, kills a wedged one, and is itself recoverable from inside the sandbox (restart through `127.0.0.1:8080` from the wake command). Collapse the three overlapping hold concepts (poke bridge, supervisor lease, per-process timeout clipped to the shortest lease) into one lease whose backstop timeout follows the longest, and start the new `keepAlive` before killing the old.

D9. State in one place. Fold `fresh-session`, `scheduled-wake`, `context-seen`, `stop-answers.json`, and the autopilot cursor into the replica database (or one fsynced `state.json`), so a reseed resets them consistently and none can be corrupted separately.

D10. Retries as waits. Failure backoff recorded as `until` waits (like usage limits) so the node sleeps cheaply, and humans are asked only when the backoff is exhausted.
