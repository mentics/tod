# Cloud nodes: robustness audit and plan

> **Historical.** Written before the runner menu rebuild ([doc/runner-menu](../runner-menu/design.md)).
> The menu entries it names (Run in the cloud, Keep going, Poll now, ...) no longer exist under those
> names; "Run in the cloud" is now Start, and "Keep going" is Resume. Read it for the findings.

Status: audit of 2026-11-03 over the live data root (`~/.config/tod`), its
database, logs, the orchestrator and node sandboxes, the last three weeks
of sessions (65 catalogued problems), and the code. The goal it serves:
accept a ticket, start it, and have it run to done or to a question only
the user can answer, with nothing else to click.

The four audits and the session catalogue are in `audit-2026-11/`:

| File | Scope |
|---|---|
| [audit-supervisor.md](audit-2026-11/audit-supervisor.md) | the wake inside a node sandbox: supervisor, relay, holds, pokes, schedules (16 findings, D1–D10) |
| [audit-provisioning.md](audit-2026-11/audit-provisioning.md) | the app's side: create, fork, replace, reinstall, checkpoint, delete, credentials, Blaxel calls (15 findings, 10 design changes) |
| [audit-orchestrator.md](audit-2026-11/audit-orchestrator.md) | the orchestrator and every channel: sync, seed, notices, broker, wakes, webhooks, problems, deploy (20 findings, 10 design changes) |
| [audit-autopilot-status.md](audit-2026-11/audit-autopilot-status.md) | the lifecycle loop and what the app says about it: outcomes, waits, budgets, PR babysitting, timing (15 findings, 6 design changes) |
| [session-history.md](audit-2026-11/session-history.md) | every cloud problem the user hit in sessions Oct 20 – Nov 3, with what was fixed |

This page is the summary and the plan: what the live system showed, the
bugs fixed in this pass, the design changes that remove whole classes of
failure (ranked), the process changes, and how to verify.

## Status, end of 2026-11-03

All of section 3 (3.1 to 3.12) is on `main` and was exercised on the
live-verify root; the orchestrator is on a volume (`orchestrator_volume`) with
its databases on local disk (SQLite WAL fails on the volume), and the app
starts it again when its sandbox is up and its process is gone.

Verified live (real sandboxes, real binaries): the sliced replica (7 MB); the
health probe on fork; run state and its plain-words stops; request rows consumed
by the supervisor; the long-poll feed; the notice topic re-fetch; the version
contract (a newer client gets 426, one with no headers is accepted); wake
rebuild; the gate reading a PR with a routed token; a webhook merge waking a
node 2 s after the event; the orchestrator self-heal drill; replacing three
expired real sandboxes at once; `tod-cli db` routing (sandbox -> orchestrator
broker -> the root's daemon, signed login returned, no query run); the disk
cleanup script; the budget stop with Keep going (its hours text was wrong and is
fixed); the missing-skill stop and "Fixed, try again"; the quit dialog's rule
(against the real database: listed only with a fresh working heartbeat); a
second supervisor meeting the lock (exits 0, pokes the holder, touches nothing);
the relay adopting a supervisor it did not start; the dead-man retry wake after a
real wake error (scheduled at the orchestrator under the node id, counter 1); the
wake command's detached fallback (returns in 14 ms, own session).

Verified by tests on real processes but not live: the relay killing a stalled
relay-started supervisor (Linux process tests in Docker); the loop-detection stop
(end-to-end through `Autopilot::run_with`); the quit dialog's rendering (the
control socket's `quit` bypasses it).

Not verified live: the Settings token-apply guard (it changes the shared
orchestrator's secrets: unit-tested, the endpoint answers 404 until an apply
records names).

How it works (not gaps):

- **Sandboxes are deleted when idle, on purpose.** A GitHub workflow deletes node
  sandboxes that have sat idle for an hour, so nothing sits around building
  while nobody is using it; a node's sandbox is meant to be forked from the
  golden and running in a second or two. A node waiting on a review therefore
  usually has no sandbox, and an event for it (a webhook, a due wake) makes the
  orchestrator mark it lost; the app's daemon then makes a new one. That needs the
  user's machine (the user's tokens are only there), which is the design.
  "Its sandbox is gone; the app replaces it" is the normal path, and the
  replacement restores uncommitted work from the checkpoint. Sandboxes should
  only disappear when idle; one that vanishes while working is a bug.
- The supervisor the wake command's fallback detaches runs without the relay's
  secrets until the relay adopts it; wake schedules made before the new command
  keep the old fallback until they are recreated.
- Saving GitHub tokens in two roots that share an orchestrator asks before it
  drops names this root has no token for; a reset orchestrator disk loses the
  recorded names, so the next apply asks once.
- The cause of the orchestrator container reset at 10:43 UTC on 2026-11-03 is not
  known and is not being pursued: the volume and the app's restart cover it.

## 1. What the live system showed

Measured 2026-11-03 (UTC), live root `~/.config/tod`, six cloud nodes.

| Fact | Where | Consequence |
|---|---|---|
| "its supervisor has not reported in after 150 seconds" ~20 times in 6 days, on 5 nodes, after Run in the cloud, Run with a new session, and replacements | `log/tod.log` | the user cannot tell whether the node is working |
| 3 nodes idle in `planning` / `pr_draft` because their last stop question was answered "Leave it stopped"; a later "Run with a new session" was ignored and its request file is still in the sandbox | relay logs, `decisions`, `/var/lib/tod-supervisor/<node>/fresh-session` | the user's explicit request does nothing, silently; the app reports success |
| Those stop questions came from `the skill ci-fix-step cannot be found`: it is on the workspace repository's `main` (`.claude/skills/steps/ci-fix-step`) but the node branches are 33 commits behind | `decisions`, GitHub compare | a configuration lag is turned into "the agent failed 3 times", then a budget-style question |
| A cloud node's stop reason (PrStuck, NoProgress, EvaluationStuck, a stop answer…) is never stored anywhere the app reads; the node shows "on you: not started" | `next_actor.rs`, supervisor `main.rs` | every non-request stop is invisible; "Sync now" re-runs into the same stop |
| `sync_changes` (the sync outbox) holds 44,588 rows, all already sent, whose `old_row` JSON is 237 MB of the 322 MB database; 219 MB of it is `conversations` rows rewritten with their `opening_context` on every update | `tod.db` | every supervisor replica (367 MB) and the orchestrator copy carry it; reseeds move ~330 MB; a sandbox ran out of disk reseeding; seeding takes 7 s+ |
| 11 `waits` still `pending` with `due_at` in September–October on nodes long past those states | `waits` | "waiting on others, check overdue" forever; a stale future-dated wait blocks a run |
| 12 `tod-agentd` processes, 10 for scratch roots of sessions Oct 22–28 (3 roots deleted), beside the live daemon; 49 build copies (1.9 GB) under `daemon/` | `ps`, `daemon/` | a second root with databases answers the broker for nodes it does not own with a final error |
| `~/.config/tod` and `~/.local/share/tod-live-verify` both use `owner = "jshellman"` and so share and reseed over one orchestrator database | `sandboxes.toml` of both roots | the live-verify daemon is at 14,104 consecutive sync failures on a CHECK constraint, silently |
| Replacing a lost sandbox failed 5 times with `could not read Username for 'https://github.com'`; the same sandbox worked 10 minutes later unchanged | `agentd.log`, `tod.log`, sandbox process log | the fork's proxy rules are the golden's until a second PUT lands; nothing waits for or verifies it; a fork that fails is kept and re-fails |
| Blaxel `404 … The workload is not ready yet … Retry with exponential backoff` ×8 on commands right after a wake; 4 replacements failed on it | `agentd.log` | `wait_reachable` takes that 404 as "up"; nothing retries it |
| `upload /opt/tod/tod-relay.new: no space left on device` ×7; then Delete Sandbox refused because the checkpoint failed | `tod.log` | a full sandbox can be neither repaired nor deleted from the app; a truncated manifest made the installer take the image path |
| Relay log: `hold: awake (… timeout 20s)`, `19s`, … one keepAlive restart per second for 20 s per episode; the 60 s poke lease is never released early | sandbox process API | ~45 s of extra awake time per wake; churn in the process API |
| The install stamp made every node reinstall at each app build: 8 refreshes between 11-02 13:47 and 11-03 06:00 | `cloud-install-stamps.json`, `tod.log` | minutes per relaunch, each a chance to fail |
| checkpoint warnings on every refresh and run ("does not apply to this branch", "has uncommitted changes") | both logs | noise that hides real problems; a ref that can never apply is never cleaned |
| `database broker: orchestrator broker: 502` ×8, resets ×6, ntfy 502s, DNS failures while offline; gateway retry covers only 502/503/504 for 6 s | `agentd.log` | a standby resume longer than 6 s fails a sync; most recover on their own |
| 987 feed 502s and 136 changes 502s in the daemon log | `agentd.log` | the orchestrator sleeps and the first request after wakes it |
| A node whose sandbox the user deleted (`bom-upload-…`, `merged`) holds a pending phase question from Oct 27 | `decisions`, `cloud-sync.json` | answering it is saved but nothing says the sandbox is gone |
| Uncommitted work from Oct 30 in worktree `tod-cli-vm-connection-ee97d5`: a node-scoped database slice (`sync/slice.rs`, 591 lines, with `doc/cloud-sandboxes/node-scoped-copy.md`) | that worktree | the replica-size problem was solved once and lost |

## 2. Bugs fixed in this pass

Each landed on `main` on 2026-11-03 with regression tests (the merge of
`js/cloud-vm-robustness-audit-4a4b9e`). They need a rebuild of the sandbox
binaries, an orchestrator redeploy (`scripts/deploy.sh`), and an app relaunch
(which reinstalls idle node sandboxes); the `cloud_activity.stopped` column
(schema v87) means an old app and a new orchestrator must not be mixed for
long: relaunch the app after the deploy.

| # | Symptom | Cause | Fix |
|---|---|---|---|
| 2.1 | "Run with a new session" ignored after "Leave it stopped" | `wake` returned `LeftStopped` before reading the request file | the app answers the stop question to continue when it sends the request; the supervisor reads a Stop answer as Continue while a request is pending |
| 2.2 | 322 MB database, 367 MB replicas, out-of-disk reseeds | the sync outbox was never pruned; snapshots carried it | prune on cursor advance (app and supervisor), snapshots drop the log, one-time VACUUM when the freelist passes 64 MB (322 → 74 MB measured) |
| 2.3 | a phase skill the checkout lacks stops the node after 3 "failures" | skill resolved only in the checkout; a missing one counted as an agent failure | also read it from `origin/<default branch>`; a missing skill is a `supervisor:config` stop question ("Fixed, try again"), not retried |
| 2.4 | a cloud stop reads "not started" with no reason or action; quit dialog lists stopped nodes as running | no synced stop reason; `working_in_cloud` derived from the absence of a wait | the idle beat carries the stop reason; `next_actor` shows `You: stopped <why>` with Keep going; "working" only from a live lease or beat; a `starting` beat before seeding; the beat no longer waits on a sync's lock; plain-words `NeedsHuman`; budget 60 sessions / 16 h with clearer wording |
| 2.5 | stale pending waits forever | nothing ends a wait when a node leaves its state | waits cancelled on advance/revert/delete, past-due event waits expired, a startup sweep, one spelling for `github:pr …` specs, an overdue wait is `You`, not `Others` |
| 2.6 | fork checkout fails on GitHub auth, repeats forever | the node's proxy rules land by a second PUT nothing waits for; a failed fork is kept | `git ls-remote` readiness probe with backoff after the PUT; a fork that fails after it is made is removed; `sandboxes.toml` save errors are not swallowed and the write is atomic |
| 2.7 | not-ready 404 fails replacements; expired sandbox reused; full disk is undeletable | no error classification; `sandbox_is_dead` ignores `state`; no disk check; delete requires a checkpoint | one Blaxel error classification with bounded retry on every call; `state` honoured; free-space check and `.part` cleanup; `delete … anyway` path |
| 2.8 | relay hold churn; pokes lost | timeout clipped to the shortest lease; `alive()` true on zombies; SIGUSR1 before the handler; poke in the exit tail | clip to the longest lease with hysteresis; release the poke lease when the supervisor holds or exits; `SIG_IGN` across exec; a poke generation counter re-runs the supervisor |
| 2.9 | one rejected row wedges sync forever; a second root reseeds over the first | batch apply in one transaction; no seed ownership | per-row apply with a `sync_rejects` quarantine and a visible escalation; a seed from a newer schema or another client is refused unless taken over; atomic seed markers |
| 2.10 | the wrong daemon answers the database broker; daemons never exit | ownership checked after the lookups; no lifetime | ownership first, owner routing on the orchestrator, bounded broker poll timeouts; a daemon exits when its root is gone or idle 6 h with nothing to keep it; old build copies pruned |

| 2.11 | the live-verify root reseeds over the real root's orchestrator copy | both use `owner = "jshellman"` | the seed script gives that root `owner = "<you>-live-verify"`; the live root was changed by hand the same way |
| 2.12 | a truncated manifest after a full disk made the installer take the image path | `printf > manifest` cut short | the manifest is written to a temp file and moved into place; "Delete Sandbox without a checkpoint" in the runner menu |

Still open from the catalogue, by design decision rather than bug:
Node 20 missing from a golden fork (fix it in the image); the workspace
repository's shallow submodule clone (`--shallow-since`, in the golden).

## 3. Design changes that remove classes of failure

Ranked by how many of the observed failures each removes. Each names the
audit finding it comes from.

### 3.1 One synced `run_state` per node (autopilot D1, supervisor D3) — landed (schema v91)

Landed: `tod_store::run_state`, written by the supervisor, the local runner and the app (`starting` before seeding); read by `next_actor` with the heartbeat as fallback. See `doc/lifecycle/next-actor.md`, "The run state". Not yet: orchestrator stamping.

Today the app infers a cloud node's state from a heartbeat, waits,
decisions and plan steps, and the supervisor's own conclusion is printed to
stderr. 2.4 puts the stop reason in the beat; the full form is one row
`{phase: starting|working|waiting|stopped|done, reason_kind, reason_text,
since, resumable_by}` written by whoever runs the node (supervisor or local
runner) at every transition, stamped by the orchestrator, and read by
`next_actor` for cloud and local alike. Every `Outcome` maps to it
exhaustively. Removes: the "not started" lie, the 150 s silence (the
`starting` row arrives before seeding), false "working", and the need for
Sync now / Check now to learn anything.

### 3.2 Node-scoped replicas (provisioning 6, orchestrator 5, supervisor D5)

The uncommitted `sync/slice.rs` work (Oct 30) cuts a supervisor's copy to
its node, its ancestors' inherited rows and the foreign-key closure, with
`scope_node_id` on the feed and a confined push. With 2.2 the full copy is
~70 MB; the slice would be a few MB, seedable in under a second, and an
agent sandbox would hold only its own node's data. Resurrect it from
worktree `tod-cli-vm-connection-ee97d5` against current `main`, deploy the
orchestrator first. Also move `conversations.opening_context` into a
write-once table so an update does not copy 50 KB into the log.

### 3.3 Waits owned by lifecycle stays (autopilot D2) — landed

Done: `waits.stay` (schema v88, the node's `node_lifecycle.updated_at` when the
wait was recorded); `sweep` and the orchestrator gate end a wait whose stay
has ended (an `until` wait outlives it); `Wait::expired_by` is the one
past-due check (past due is `You`, never `Others`); `GiveUp` names each kind's
give-up behaviour; the gate absorbs `pr_review` waits for a person's review;
off-hours cadence default is 240 minutes.

2.5 cancels and expires. The full rule: record the stay id on every wait;
ending the stay ends its waits; `next_actor` has one authoritative
`expired` check so a past-due wait is a fault, never "others"; every wait
kind names its give-up behaviour. The orchestrator's gate absorbs
`pr_review` waits too (today a PR awaiting a person wakes the sandbox
every 5 minutes, 24/7: autopilot 5), and the off-hours cadence is hours.

### 3.4 Explicit user requests as durable rows (supervisor D6, autopilot D3)

"Run", "Run with a new session", "Check now", "Keep going" are marker files
and pokes today; a wake that exits early leaves them behind or loses them.
Make each a consumable row on the node (synced, with who/when), consumed by
the supervisor at the first boundary, superseding any older stop answer.
2.1 is the first step (the app answers the stop question).

Done: `node_requests` (schema v90, `tod_store::node_requests`; kinds `run`,
`fresh_session`, `check_now`, `keep_going`; `requested_by/at`, `consumed_by/at`),
synced and journaled. The app records the row (`cloud_sync::record_node_request`)
beside the 2.1 answer and the legacy marker file (kept for a supervisor built
before v90). The supervisor reads pending rows at wake: any but `check_now`
supersedes a stop answer and cancels pending waits; nothing is consumed until
the run starts (or, for `check_now`, the look is made), so an early-exit wake
leaves them. Mid-run they are consumed at the first boundary after a pull; a
local runner consumes them as `run_with` starts. Gap: a row reaches a running
supervisor only at its next pull, so a mid-turn "new session" still relies on
the marker file to cancel the turn at once.

### 3.5 One Blaxel client with classification and a health probe (provisioning D1, D2)

2.7 adds the classification and retry; the rest: a `SandboxHealth` probe
(reachable, relay up, disk free, manifest sane, `git ls-remote` through the
proxy, env intact) run after create/fork/PUT/wake and before any reuse, so
reuse is decided on health, never on `status = DEPLOYED`. A per-node
cross-process lock for provisioning; the name recorded before the first
call; delete on failure; an orphan sweeper.

A failed check reads in plain words (`HealthFailure::problem`, then a single
"What to do:" step, `next_step`), then a `Technical detail:` line with the
check name and what it saw. The step depends on `Setup`: a sandbox made by this
Start is kept and the next Start deletes it and makes another
(`NameDecision::ReplaceFailed`); an existing one is never deleted by Start, so
the step names the runner menu's Reset environment (installs tod's files again
and re-probes) and, if that fails the same way, Delete sandbox. A GitHub
failure points at the red "GitHub token" banner's Add/Replace token button.
The autopilot does not retry a failed setup itself.

### 3.6 A version contract instead of source hashes (orchestrator D1, 19)

**Done:** `tod_store::sync::contract` (`WIRE`, `Contract`, `judge`); `/health`
returns `{wire, schema, build}` (an old one's `ok` reads as wire 0); the app
sends `X-Tod-Wire`/`X-Tod-Schema`; the orchestrator answers 426 to a client
newer than itself; `sync` refuses a newer orchestrator naming both versions;
`deploy::ensure_orchestrator` updates an older one in place; the build hash
covers only `src/`, manifests and build scripts. Not done: skipping the
update while a node is mid-seed; (done: supervisors and tod-cli send them).

"Out of date" is a source hash over 12 crates, per machine. Replace it
with `/health` returning `{wire, schema, build}` and request headers
carrying the client's; the app judges compatibility from that, deploys
automatically when the orchestrator is older (it already deploys when none
exists), and refuses only on a newer schema it cannot read. Hash only the
sandbox binaries' own sources for "needs a rebuild".

### 3.7 Dead-man scheduling in the sandbox (supervisor D2, 3)

Any `Err` out of `wake` strands the node: the schedule was cancelled and
only an orderly end makes a new one. Before work, replace the wait's
schedule with a retry wake; clear it only on an orderly end; the error arm
of `main` schedules a backoff retry through the local Blaxel API. Failure
backoff expressed as `until` waits (D10) instead of three immediate
retries.

### 3.8 The relay owns liveness (supervisor D7, D8)

One lease whose backstop follows the longest holder (2.8), a progress
marker gating hold renewal and the heartbeat's "working" (a wedged
supervisor should not read as working for 6 hours), a supervisor
`flock` so two never run at once, and the relay restarting the supervisor
itself when the process API's fallback would otherwise run it untracked
and kill it at 120 s.

Landed: the hold's `keepAlive` timeout is the longest of every holder's ask
(a lease-less reason asks a rolling 10 min, not the 4 h cap); the supervisor
marks `<state_dir>/progress` (`tod_supervisor::progress`) and renews its hold
and sends heartbeats only while it is younger than the agent's hang limit
plus 10 min; `<state_dir>/supervisor.lock` (`flock`, holds the pid) makes a
second supervisor exit with `Woke::AlreadyRunning` before touching anything,
poking the holder; the relay (`liveness.rs`, every 15 s and on each poke)
kills a supervisor it started that made no progress for 40 min and adopts
(ends and restarts as its own) a supervisor it did not start. Not done: the
wake command's fallback still `exec`s the supervisor when the relay is
down (the relay's environment, which carries the supervisor's secrets, is
not recoverable from inside the sandbox), so that one runs untracked until
the relay returns and adopts it.

### 3.9 Pull that does not depend on ntfy (orchestrator D6, 8)

A failed publish marks the change announced; the cached topic can be
stale; the fallback poll stops when no cloud node row is live. Replace the
20 s poll with a long-poll on the feed itself (the orchestrator is awake
only while a request is in flight, and a long-poll is one request), keep
ntfy for the phone alerts only, and re-fetch the topic on every sync.

### 3.10 Ownership recorded everywhere (orchestrator D3)

Workspace, `owner_client`, and repository on every node row the
orchestrator holds, so the broker routes to one machine (2.10), webhooks
never match a branch across repositories or users (orchestrator 14), and
two data roots of one person cannot take each other's database (2.9).

### 3.11 Loop counters and cycle detection in one place (autopilot D6, 8)

Sessions, PR rounds, evaluator rejections and fix cycles live in
`AutopilotState` where the app can show them and renew from either side; a
cycle detector keyed on (state, failing obligation or criterion) stops a
verify → fix → verify ping-pong with a specific reason instead of spending
the budget.

### 3.12 A disk budget and lazy reinstall (provisioning D7, D9)

Check and clean before every install, wake and checkpoint; write manifest
and stamps atomically; reinstall a node sandbox only when the binaries it
runs changed, with the reason recorded, not at every app build.

## 4. Process changes

- **Every component's failure reaches the user, once, with the count.** A
  sync that fails 20 times in a row is one ERROR, not 14,000 log lines
  (2.9). An allow-list test fails on new `let _ =` / `.ok()` / `warning:`
  progress lines in the cloud modules (provisioning D10).
- **Deploy is one command and is judged by version, not by hash.**
  `scripts/deploy.sh` stays; 3.6 makes the app say exactly which of the
  orchestrator, watchdog, relay, supervisor is behind and why.
- **Live verification is a separate orchestrator user.** The live-verify
  root must not share the orchestrator database with the real root: give it
  its own `owner` in `sandboxes.toml` (and the seed script sets it). 2.9
  refuses the take-over outright.
- **One supervisor build per fix cycle.** Fixes in the supervisor, relay or
  orchestrator need `scripts/build.sh` (Linux binaries), `scripts/deploy.sh`
  (orchestrator), and a node reinstall (the app does it at launch for idle
  nodes). The plan doc for each fix says which of the three it needs.
- **Stop old daemons.** Until 2.10 is deployed: `pkill -f
  'tod-agentd.*scratchpad'` kills only the scratch-root daemons.
- **Tests that pin the user-facing text** of every stop reason and every
  standing, so a reword cannot leave a state unnamed.
- **The flaky test** `tod-orchestrator broker::tests::a_machine_that_does_not_know_the_node_leaves_it_to_the_other`
  (times out under load) gets a deterministic clock.

## 5. Verification checklist

After deploying this pass (build, deploy the orchestrator, relaunch the app
so idle nodes reinstall), on the live root:

1. `remediate-stale-primary-lifecycle-stat-2` (planning, left stopped):
   the task panel says "stopped: you left it stopped after the budget
   question" with Keep going as the primary action; Keep going runs it.
2. `stop-writing-mount-style-and-placehold-2` (pr_draft, left stopped after
   a missing skill): Keep going runs the pr_draft agent with `ci-fix-step`
   read from `origin/main`; no `supervisor:failures` question.
3. The database is under 100 MB after the daemon restarts; the supervisor
   replica of a node run after that is under 100 MB.
4. No pending wait older than its node's current state (`select
   match_spec, state, datetime(due_at/1000,'unixepoch') from waits where
   state='pending'`).
5. Delete a node's sandbox, then Run in the cloud: the first beat is
   `starting` within 15 s of the fork; the checkout succeeds first time or
   the error names the probe that failed.
6. `tod-sandbox exec <node> -- curl -s 127.0.0.1:8080/process` shows one
   keepAlive process per hold episode, not twenty.
7. The quit dialog lists only nodes whose beat is live.
8. Only one `tod-agentd` per data root in use; none for a deleted root.
9. A `tod-cli db` from a node sandbox is answered by the live root's daemon
   even with the live-verify daemon running.
