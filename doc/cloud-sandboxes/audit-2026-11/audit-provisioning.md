# Audit: cloud sandbox provisioning, replacement, checkpoint, delete, credentials

Checkout: /Users/jshellman/dev/tools/tod, branch main (HEAD f6ca8ff0). Read-only; nothing edited.
Evidence marked LOG is from `~/.config/tod/daemon/agentd.log` and `~/.config/tod/log/tod.log`
(live data root). Evidence marked DB is a read-only `SELECT` on `~/.config/tod/tod.db`.
"Confirmed" = shown by logs/DB plus code. "Inferred" = best fit, with the measurement that settles it.

Paths are relative to the repo root.

---------------------------------------------------------------------------

## 1. A fork's proxy rules are replaced right after the fork call, with no wait and no check; the checkout then runs before the GitHub rule is in effect (and a retry never repairs it)
Severity: blocks-work
Where: `crates/tod-sandbox/src/node.rs:798-832` (`fork_golden`), `crates/tod-sandbox/src/blaxel.rs:314` (`update_proxy`) and `:506` (`wait_deployed`), `crates/tod-core/src/cloud_sync.rs:1349-1372` (made_now / fork), `:1394-1395` (`install_into_fork` + `start`), `crates/tod-sandbox/src/node.rs:593-618` (`checkout_script`).

Answers to your four questions:
- Is the fork made with a proxy spec? Yes: a fork carries its SOURCE's proxy rules (`blaxel.rs:337-340`: "Its labels, proxy rules, ports, and memory are the source's"); `fork_golden` refuses a golden with zero routes (`node.rs:801-808`). So the fork has a proxy, but with the golden's rules, not the node's. The node's rules (GitHub Basic on `github.com`, Bearer on `api.github.com`, Claude, Linear, Blaxel) are only put there by a second call, `update_proxy` (GET the record, PUT it back with new network.proxy), `node.rs:826-831`. The create-from-image path sends all rules inside the POST (`create_body`, `node.rs:463-490`), so it has no such window. That is the fork-vs-create difference you asked about.
- Is the checkout run before the proxy is up? Yes, in the sense that matters. Nothing waits for the PUT to take effect or tests it. Between `update_proxy` returning and `git fetch` there is only: second `wait_deployed` (returns at once), `wait_reachable`, `require_fork_prerequisites`, ~35 MB of uploads, ~77 bundle uploads (~15-20 s in the logs). That is not enough in the failing cases.
- Is `NO_PROXY` set? tod never sets or clears `NO_PROXY`/`no_proxy` anywhere in the repo (grep). A fork inherits the golden's env, and `fork_golden` overrides only HTTPS_PROXY/HTTP_PROXY (+ lowercase), `provision.rs:190-197`. But env is not the cause here, see evidence: the same sandbox later succeeded with no env change.
- Is the github.com rule missing in the fork path? In the window after the fork it is (golden's rules apply); afterwards it is present. 

Evidence (LOG):
- Timing of one failure, node 8f80f1e0, 2026-11-03: `05:32:26.53 forking ... from wsx-golden-20261102-221712...` -> `05:32:27.27 waiting for ...` (0.74 s: so the first `wait_deployed` inside `fork_golden` returned on its first poll; a fork record is already `DEPLOYED` with a URL about a second after the POST, long before it is really materialised) -> `05:32:29.4 is up after 2.9s` (the second wait took one 2 s sleep, so the status moved after the PUT: the PUT lands on a fork that is still being created/reconciled) -> `05:32:45.8 checking out` -> `05:32:46.07 failed ... could not read Username for 'https://github.com'` (0.27 s: git got an unauthenticated 401 straight back, i.e. no Authorization header was injected).
- The SAME sandbox is then reused (see item 4): 05:33:09 "Run in the cloud" -> `node-stop-writing-... is up after 0.1s` -> checkout -> same error at 05:33:26. Then at 05:43:05 the lost check reused it again ("is up after 0.2s", no "forking" line) and the checkout succeeded in 1.5 s (`05:43:21.95 checking out` -> `05:43:23.47 starting the supervisor`). Nothing was changed on the sandbox between 05:33 and 05:43: only time passed. Same pattern 2026-10-30: node 66003e5b failed at 21:22:42 and its refresh at 21:24:58 on the same sandbox worked.
- Same code, same golden, same minute, different outcome: 10-30 21:22:42 failed for 66003e5b while 21:23:04 and 21:24:09 succeeded for two siblings; 11-02 13:47 and 20:29 each had 1 of 3 parallel replacements fail. Failures are always the fork path (all 9 `could not read Username` lines are from `cloud_sync::lost` or Run in the cloud on a fork).
- The GitHub token is in the store and was passed (no "no GitHub token stored" warning since 2026-10-16; credentials dir has github_token.enc).

Conclusion (confirmed that it is a propagation lag on that sandbox, inferred that the cause is the PUT-after-fork): the node's rules reach the fork's proxy tens of seconds to minutes after the PUT is accepted, and the checkout beat it. The doc's "in effect within a second" (`blaxel.rs:307-310`) was measured on a long-running sandbox, not a fresh fork.

Why it does not self-heal: `ensure_node_sandbox` only deletes a failed fork if `fork_golden` itself errors (`cloud_sync.rs:1354-1360`). A failure in `install_into_fork`/`start` leaves the fork; the next call sees it `DEPLOYED`, so `made_now == false` (`:1349`), no fork, no proxy update, no probe, and it repeats the same checkout (item 4).

Fix:
1. Add `node::wait_proxy_ready(bx, url, creds, progress)` called at the end of `fork_golden`'s caller (in `ensure_node_sandbox`, after `wait_reachable`, before `install_into_fork`) and in the already-existing path when the sandbox is not known good. It runs, through `bx.run`, the exact thing the checkout needs: `GIT_TERMINAL_PROMPT=0 git ls-remote --exit-code <repo_url> HEAD` (with the same `git config --system http.sslCAInfo` step), retries with backoff 2 s -> 15 s for up to 3 minutes, and re-sends `update_proxy` once if it still fails at 60 s. Only then proceed. A final failure names "GitHub auth through the proxy still not working after N s" and deletes the fork (see item 4).
2. Before the PUT, do a real readiness wait on the fork: `wait_reachable` (fixed per item 2) and a `get_json` check that `state` is RUNNING/STANDBY, not just `status == DEPLOYED`; after the PUT wait until the record's `updatedAt`/status settles. Ask Blaxel (or test) whether the fork API accepts `spec.network.proxy` in the POST body: that removes the PUT altogether.
3. Make the checkout script itself retry auth failures (`exit 128` + "could not read Username"/"Authentication failed") 3 times with 5/10/20 s sleeps and set `GIT_TERMINAL_PROMPT=0` so the message is "Authentication failed", and print `env | grep -i proxy` (names only) plus the proxy's response status on failure into the error, so the next occurrence is self-explaining.

Regression test: partly. `checkout_script` retry/`GIT_TERMINAL_PROMPT` is unit-testable as a string/`sh` fixture in `crates/tod-sandbox/src/node.rs` tests (a fake `git` first returning 128). The propagation lag needs a stub Blaxel: `API` is a `const` (`blaxel.rs:12`), so add `Blaxel::with_base_url` and test `wait_proxy_ready` against a stub HTTP server in `crates/tod-sandbox/tests/`. The live case stays an `#[ignore]`d e2e like the existing one in `node_checkpoint.rs`.

---------------------------------------------------------------------------

## 2. Blaxel's own "workload not ready" 404 is treated as reachable by `wait_reachable` and is never retried by `run`/`start`/`kill`/`get_url`; `upload` retries only 502/503/504
Severity: blocks-work
Where: `crates/tod-sandbox/src/blaxel.rs:533-560` (`wait_reachable`), `:562-575` (`run`), `:582-615` (`start_with_env`), `:699-737` (`upload`, `ATTEMPTS`), `:155-168` (`check`).

Scenario: a sandbox that was woken, just created, or is in standby/expired answers the sandbox API with
`404 {"error":{"code":"WORKLOAD_UNAVAILABLE","origin":"platform","retryable":true,"safe_to_retry_request":true,"dispatch_state":"not_dispatched","action":"Retry with exponential backoff: 500ms -> 30s, giving up after ~60s"}}`.
- `wait_reachable` probes `GET /process/tod-wake-probe` and returns OK on ANY status except 502/503/504, with the comment "a 404 included, means the workload is there". That is true for the sandbox API's own 404 ("process not found") but false for the platform's WORKLOAD_UNAVAILABLE 404, so the wake wait returns immediately on a sandbox that is not up.
- `run` has no retry at all. `check()` turns the 404 into an error, logs the raw body at WARN (that is your 8 warnings), and the caller fails.
- `upload` retries 502/503/504 only, for 2+4+8 = 14 s in total; a 404 returns at once.
- `start_with_env` and `kill` do not retry either; `kill` maps a sandbox-API 404 to Ok (fine) but would also map the platform's 404 to Ok (a bug: it would then "start" into a sandbox that is not there).

Evidence (LOG): agentd.log 2026-10-31 04:01-04:54, four replacements of node 8f80f1e0: each shows `waking ...` then exactly 30 s later `run command: 404 ... WORKLOAD_UNAVAILABLE` (the platform holds the request ~30 s, then answers 404) and `cloud: replacing node ...: run command: 404 Not Found`; the sandbox record stayed `DEPLOYED` ("is up after 0.1s") for about an hour. Also 2026-10-23 22:11 (Files path): same 404 then `NeedsHuman AgentFailed`.

Fix: in `blaxel.rs` add `fn is_workload_unavailable(status, body) -> bool` (status 404 and `error.code == "WORKLOAD_UNAVAILABLE"` or `origin == "platform"`), use it (a) in `wait_reachable` to keep waiting, (b) in a shared `send_with_backoff` used by `run`, `start_with_env`, `kill`, `process_status`, `get_url`, `download`, and `upload`: retry 404-WORKLOAD_UNAVAILABLE and 502/503/504/429 with 0.5 s doubling to 30 s for up to 60-90 s total, honouring `Retry-After`; only requests with `"safe_to_retry_request": true` or idempotent verbs (`GET`, `PUT` of the same file) are replayed, as the platform's own advice says ("Do not retry faster than 500ms, send concurrent retries"). For `run` (a POST that may have started) retry only when the body says `dispatch_state: not_dispatched`. After the budget, return a typed error `SandboxUnavailable` so callers (item 3) can treat it as "gone".
Regression test: yes. `wait_reachable`/`run`/`upload` against a stub server (needs the base-URL hook from item 1) returning 404-WORKLOAD_UNAVAILABLE N times then 200; also a pure test of `is_workload_unavailable` on the logged body, in `blaxel.rs` tests (parsing is pure and does not need the stub).

---------------------------------------------------------------------------

## 3. Replacing a lost sandbox reuses the unreachable record, and first spends ~31 s asking the dead sandbox for the repository URL
Severity: blocks-work
Where: `crates/tod-core/src/cloud_sync.rs:1205-1225` (`existing_sandbox_name`), `:1337-1349` (dead/`made_now`), `:1045-1115` (`origin_url_of`, `node_source`); `crates/tod-core/src/cloud_sync/lost.rs:34-45` (`lost_rows`), `:184-222` (`replace`), `:62-72` (`RETRY_AFTER`/`FAILED`); `crates/tod-core/src/cloud_sync.rs:196-199` (`sandbox_is_dead`).

Scenario: the orchestrator marks the node `lost_at` because the poke found the workload unavailable. `lost::replace` calls `ensure_node_sandbox`, which does not know the node is marked lost. `sandbox_is_dead` looks only at `status` (FAILED/TERMINATED/DELETING/DELETED), never `state`, and an expired sandbox stays `status=DEPLOYED` for a long while (about an hour in the log). So `made_now == false`: no fork, wake wait passes (item 2), the first real command 404s after 30 s, replacement fails, the node is backed off for 10 minutes in memory (`FAILED` static: lost on every daemon restart, and there were ~50 daemon binaries since Oct 20) and the lost mark stays set, so it repeats. Only when Blaxel finally reports the record gone (05:05 in the log) does a fork happen.

A second cost on every replacement: `node_source` -> `origin_url_of` first runs `git config --get-regexp` in the node's OWN recorded sandbox through `SandboxExec::output` -> `connect` -> `Sandboxes::ensure_as` -> `provision::ensure`, which, on an unreachable sandbox, waits the 30 s platform hold (LOG: `gone; replacing it` 04:31:06.8 -> first 404 at 04:31:37.6 -> "waiting for" 04:31:38.4: the first 404 happens BEFORE any fork or wake). The error is swallowed into `tried` and the next candidate (the golden) works. It also invokes the "install" path of `provision::ensure` against a node sandbox (see item 5). 

Fix:
- Add a parameter `replace_unreachable: bool` (true from `lost::replace`, `wake_node_with`'s dead branch, Run in the cloud when the row has `lost_at`) to `ensure_node_sandbox`. When true and the existing record is not dead, probe with the fixed `wait_reachable` for at most 60 s; if it is WORKLOAD_UNAVAILABLE for the whole budget, delete it (after a checkpoint attempt that is allowed to fail, item 6) and fork. Also extend `sandbox_is_dead` to treat `state` of TERMINATED/ERROR/FAILED as dead.
- `origin_url_of`: look at the golden/fork source first; or better, store the resolved HTTPS URL on the `cloud_nodes` row (or `node_fields`) at first run so a replacement never asks a sandbox. Never ask a sandbox that `lost_rows` already flagged.
- Persist the failure backoff (`FAILED`) in `cloud_nodes` (e.g. `replace_failed_at`, `replace_error`) instead of a process-global map, and show the last error on the node.
Regression test: yes. `lost_rows`/`sandbox_is_dead` are already unit-tested in `lost.rs` tests; add cases for `state == "TERMINATED"` and a `replace_unreachable` decision function `decide_sandbox_action(existing, lost_mark, reachable) -> {Reuse, Replace, Fork}` extracted from `ensure_node_sandbox` (pure, testable in `cloud_sync/tests.rs`).

---------------------------------------------------------------------------

## 4. A sandbox that fails halfway is left behind, is reused as "good" on the next attempt, and nothing excludes two provisioners of one node
Severity: blocks-work
Where: `crates/tod-core/src/cloud_sync.rs:1227-1450` (`ensure_node_sandbox`), `:1354-1362` (only fork_golden failure cleans up), `:1488-1516` (`record_cloud_node` only after success), `crates/tod-sandbox/src/node.rs:690-720` (`start`: `kill` then `start_with_env`), `crates/tod-store/src/activity.rs:231` (`with_lease`).

Scenario and evidence:
- After the fork succeeds, any failure in `install_into_fork`, `start`, `push_supervisor_settings` leaves a forked sandbox in Blaxel with no `cloud_nodes` row (first run) or a stale row (replacement), nothing in `node_files_locations`, and nothing in `sandboxes.toml` (node sandboxes are not upserted there). Cost accrues, and the next attempt adopts it as `made_now=false` (item 1: its proxy may be wrong, nothing is re-checked; item 5: its disk may be full). LOG 05:32:46 -> 05:33:26 is exactly this.
- `with_lease` (`activity.rs:231`) is an observability heartbeat, not a lock. `ensure_node_sandbox` has no per-node mutex (the `node_lock` in `fleet/provision.rs` guards only the Files path), and the app and the daemon are different processes that both call it (UI "Run in the cloud", the lost check, refresh at launch). Two have collided in the log: `fork: a sandbox named tod-scale-paradedb-... already exists` (2026-10-16 19:07) and `start process: 400 {"error":"process with name 'tod-relay' already exists and is running"}` (19:13).
- `node::start` does `bx.kill(RELAY)` then `start_with_env(RELAY, restart_on_failure=true)`: the relay was started with `restartOnFailure`, so the platform can restart it between the two calls; the 400 above is that race, and it is not handled.
- A crash mid-provision leaves an orphan fork nobody lists. The sandbox name is deterministic (`node-<slug>`), so a retry of the same node reuses it, but a node deleted or re-slugged leaves it forever (cost).

Fix:
1. Wrap `ensure_node_sandbox` in a cross-process per-node lock (a `flock` on `<data root>/locks/node-<id>.lock`, like `sync_lock`, `cloud_sync.rs:362`), held for the whole call.
2. Track provisioning state in the store: write `cloud_nodes.provision_state` (`forking`, `installing`, `ready`) and the sandbox name BEFORE the fork call (so a crash leaves a row naming it), and set `ready` only after the probe in item 1 succeeds. `ensure_node_sandbox` treats any non-`ready` existing sandbox as made-now (re-run proxy check + install, or delete and refork after one failure).
3. On any error after the fork succeeded, delete the sandbox (unless `TOD_KEEP_FAILED_SANDBOX`, as the Files path already does, `fleet/provision.rs:676-696`), and report if the delete fails instead of `let _ = bx.delete(&name)` (`cloud_sync.rs:1358`).
4. In `node::start`, after `kill`, poll `process_status` until it is not `running` (max 10 s), and treat 400 "already exists and is running" as "kill again then start".
5. A sweeper: list sandboxes with label `tod-kind=node` whose `tod-node` has no `cloud_nodes` row or Files location for > 1 h and delete them (the labels exist: `node.rs:471-477`).
Regression test: yes for 1, 4 and the state machine in `cloud_sync/tests.rs` (extract the decision function; fake `Blaxel` through the base-URL stub); the lock with two threads in a unit test.

---------------------------------------------------------------------------

## 5. No disk-space check anywhere; a full node disk corrupts the manifest and then triggers an image-style reinstall over a node sandbox
Severity: blocks-work
Where: `crates/tod-sandbox/src/provision.rs:227-300` (`ensure`, the `current.output().trim() != manifest` branch at ~:272), `crates/tod-sandbox/src/blaxel.rs:642-697` (`upload_large`), `crates/tod-store/src/fleet/sandbox.rs:946-961` (`connect` -> `ensure_as`), `crates/tod-sandbox/src/node.rs:636-661` (`upload_tod_files`), `assets/sandbox/bootstrap.sh`. A grep for `df`/disk/ENOSPC in `crates/tod-sandbox`, `crates/tod-core/src/cloud_sync*`, `crates/tod-supervisor`, `assets/sandbox` finds nothing.

Scenario/evidence (LOG tod.log 2026-11-02 17:10-17:12): checkpointing node 66003e5b logs `installing tod's dependencies in the sandbox (first connect only)...` then `upload /opt/tod/tod-relay.new: 422 ... no space left on device`, 7 times, then `Delete sandbox failed: could not find the checkout ... exit 1` (item 6). The relay is 1.7 MB, so the disk was completely full. Mechanism: `SandboxExec` -> `connect` -> `ensure_as` -> `provision::ensure`. With the disk full the relay is unreachable and `cat /opt/tod/manifest` came back empty (a `printf > manifest` on a full disk truncates it). An empty manifest is neither the image manifest nor a node manifest, so `ensure` goes down the "Install" branch meant for a plain image: it uploads bootstrap.sh and a new relay, then (had there been space) would run bootstrap, kill the node's relay and overwrite the node manifest with the image one, destroying what `is_node_manifest` exists to protect.

What fills 6.3 GB over a week (code-derived; I could not look inside a sandbox, so confirm with the commands at the end):
- The golden's own occupancy is the starting point (the fork copies it), so headroom is 6.3 GB minus the golden; the agent's builds then add to it: `cargo target/`, `~/.cargo/registry`, `node_modules`, pnpm/npm caches, `~/.claude` session logs.
- The supervisor's local replica of the user's database: 338 MB + WAL under `/var/lib/tod-supervisor/<node>`; on a re-seed (`replica.rs:114-125`) it writes `seed-incoming.db` (338 MB) AND restores into `tod.db` (338 MB) after deleting the old one: ~680 MB transient, and a crash leaves `seed-incoming.db`. See item 7.
- `upload_large` leaks parts on failure: it `?`-returns from the part loop (`blaxel.rs:655-659`), so parts already uploaded (`<path>.<nonce>.partNNNN`, 4 MiB each) are never removed; only a failed `cat` cleans up. A failed install of `tod-cli` (15.8 MB: 4 parts) and the supervisor (15.3 MB: 4 parts) leaks up to ~36 MB per attempt, repeated by every retry. These are your `tod-cli-local.*.part*` files.
- Node `/tmp`, process-API logs, `restartOnFailure` relay logs under `/opt/tod/logs`.

Fix: add `node::ensure_disk(bx, url, need_mb, progress)` run first in `upload_tod_files`, `provision::ensure`, `wake_node_with` and `checkpoint` entry: `df -Pm /` (and `/var/lib/tod-supervisor`); if free < max(500, 3 x bytes to upload), run a fixed cleanup script, re-check, and if still low fail with "sandbox disk full: X MB free of Y; largest: <du top 5>". Cleanup (safe, known paths only): `rm -f /opt/tod/*.part* /opt/tod/*.new /opt/tod/tod-cli-local.*.part* /var/lib/tod-supervisor/*/seed-incoming.db`, `npm cache clean --force`, `pnpm store prune`, `rm -rf ~/.cache/pip /var/cache/apt/archives/*`, `cargo clean`-equivalent only for `target/debug/incremental` and `target/*/deps` older than 2 days inside `/workspace`, `journalctl --vacuum-size=50M`. In `upload_large` remove already-uploaded parts on any error. In `provision::ensure`, refuse the Install branch when `reported` is `None` AND the `cat` output is empty AND `/opt/tod/tod-relay`/`tod-supervisor` exist (an empty manifest on a node is corruption, not a fresh image): fail with "manifest missing; disk full?". Write the node manifest with write-to-temp + `mv`, so a full disk cannot truncate it. Make `df` part of the node status the user sees.
Regression test: yes for the cleanup/decision function (pure parse of `df` output; `ensure` empty-manifest case with the stub), and `upload_large` part cleanup against a stub returning 422 on the 3rd part, in `blaxel.rs` tests.

Measure (run in a live node sandbox): `df -h / /var/lib/tod-supervisor /tmp; du -xh --max-depth=2 / 2>/dev/null | sort -h | tail -25; ls -la /opt/tod /var/lib/tod-supervisor/*`.

---------------------------------------------------------------------------

## 6. Deleting a broken sandbox from the app is refused when the checkpoint step fails, and there is no override
Severity: blocks-work
Where: `crates/tod-core/src/cloud_sync/lost.rs:495-525` (`delete_node_sandbox`), `:459-492` (`stop_running_in_cloud`), `crates/tod-core/src/cloud_sync.rs:145-182` (`sandbox_checkout`, `checkpoint_before_delete`).

Scenario: `delete_node_sandbox` calls `checkpoint_before_delete` first; `sandbox_checkout` runs `echo $(...)` through `bx.run` and bails `could not find the checkout in <name>: exit 1` (LOG 17:11:53, 17:12:02, both on the full-disk sandbox: the process API could not even spawn/log); `?` aborts before `sandboxes.delete`. The user cannot remove the sandbox from the app (and it keeps costing). `stop_running_in_cloud(.., delete_sandbox=true)` has the same ordering. The error text "could not keep ...'s uncommitted work, so the sandbox was not deleted" is right to block when the work is real, but it also blocks when the sandbox is unreachable and there is nothing to keep.

Fix: add `force: bool` to both functions and a UI "Delete anyway" shown after the refusal. Classify the checkpoint error: `CheckpointRefused` (size gate, push refused: data at risk -> require force, list what would be lost) versus `SandboxUnusable` (probe failed, `SandboxUnavailable`, disk full, exit code from the probe: nothing retrievable -> allow delete after a single confirmation that names the sandbox and says unsaved work in it cannot be recovered). In `sandbox_checkout`, report the probe's stderr/stdout in the error. Do the Blaxel delete before clearing local state and record `deleted_by_user` only after it succeeded (it is set after in `delete_node_sandbox`, fine).
Regression test: yes, `checkpoint_before_delete` error classification and the force path in `cloud_sync/tests.rs` with a stub.

---------------------------------------------------------------------------

## 7. `sync_changes` is 77% of the database and is copied whole to the orchestrator and every supervisor; the cause is `conversations.opening_context` in every update's before-image
Severity: blocks-work over time (disk, seed time), wastes-time now
Where: `crates/tod-store/src/sync/mod.rs:299-330` (`install`: trigger body writes `old_row = json_of("OLD", all columns)` on every UPDATE/DELETE), `:751` (`snapshot`), `crates/tod-core/src/cloud_sync.rs:753-770` (seed: snapshot -> `std::fs::read` -> POST), `crates/tod-orchestrator/src/sync_backend.rs:56-80` (seed) and `:156-168` (snapshot GET), `crates/tod-supervisor/src/replica.rs:114-125`.

Evidence (DB, user's tod.db, 337.9 MB; SELECT on dbstat):
- `sync_changes` 260,083,712 bytes (77%), plus `idx_sync_changes_row` 3.7 MB. 44,588 rows, seq 1..44588: never pruned (no `DELETE FROM sync_changes` exists in the repo).
- By table, `sum(length(old_row))`: `conversations` 230,503,334 bytes in 5,665 rows (avg 43 KB, max 96 KB); everything else combined is < 20 MB.
- Cause: `conversations.opening_context` (TEXT, ~50 KB each, 33.7 MB across 682 rows) is part of the row, and every update to a conversation row (agent_session_id, updated_at, ...: 5,303 updates, ~7.8 per conversation) logs the whole old row including that blob. The same blob also crosses the wire in each `Change.before`/`after` (`export_changes_for`) and is logged again on the orchestrator by `apply_changes_from`, so the orchestrator database (1.9 GB root disk) bloats the same way.
- What is sent: `sync::snapshot` is the whole database (`copy_database` = SQLite online backup): all nodes of the user, all conversations, turns, actions, and the 260 MB log. Seeding does `std::fs::read` of 338 MB into memory, uploads it, and the orchestrator writes `seed-incoming.db` (338 MB) then restores to `tod.db` (338 MB) while the old `tod.db` still exists: >1 GB transient on a 1.9 GB disk. Every supervisor start does it again via `GET /snapshot` (another `snapshot-N.db` 338 MB temp + RAM). `agent_runs.cached_transcript` is not involved: `agent_runs` is a fleet run-tracking table, not in `SYNCED_TABLES` (`sync/mod.rs:83-125`), and it is not among the large tables (DB: top are `sync_changes`, `conversations` 34 MB, `conversation_turns` 14 MB, `conversation_actions` 10 MB, `phase_events` 4.5 MB).

Fix (in order of payoff):
1. In `install`, build `old_row` for UPDATE with only the columns that changed (or exclude named large columns such as `conversations.opening_context` from the before-image and sync it only on insert and when it actually changes). The conflict logic only needs `before` of columns that the change touched; check `Change::before` consumers in `apply_changes_from`.
2. Prune: after the app's seed and each successful push, `DELETE FROM sync_changes WHERE seq <= min(sent_after, feed cursors known)` on the app, and the orchestrator deletes entries older than the oldest client cursor it has served (track per-client `after` in `sync_state`). Add `PRAGMA incremental_vacuum`/periodic `VACUUM` so the file shrinks. Beware `last_seq` is `MAX(seq)` (`sync/mod.rs:430`): if the table is emptied the cursor goes backwards. Keep one tombstone row or store `last_seq` in `sync_state`.
3. `snapshot_for_node(db, node)`: for supervisors, copy then delete in the copy everything outside the node's subtree and ancestors (`nodes` and every node-scoped synced table), all `conversation_*` of other nodes, and `sync_changes`; set `sync_state.last_seq` explicitly. A supervisor currently receives every other node's conversations.
4. Stream the seed (chunked upload / `Transfer-Encoding`) instead of `std::fs::read` + one POST, compress with zstd (text JSON compresses ~10x), and delete `seed-incoming.db` on the failure path in `replica.rs:114-125`.
Regression test: yes. In `crates/tod-store/src/sync/tests.rs`: update a row with a 50 KB column and assert `sync_changes` size; prune keeps `last_seq` monotonic; `snapshot_for_node` keeps exactly the subtree.

---------------------------------------------------------------------------

## 8. Checkpoint restore warnings repeat; a ref that can never apply is never cleaned; the "dirty" and "does not apply" cases have different causes
Severity: wastes-time (but it hides real work behind noise)
Where: `crates/tod-store/src/fleet/checkpoint.rs:330-370` (`restore_repo`), `:225-258` (`save_repo`), `crates/tod-store/src/fleet/node_checkpoint.rs:91-110` (`restore_into`), `:135-190` (`sweep`, `FINISHED_STATES`), `crates/tod-core/src/cloud_sync.rs:1428-1440` (`if made_now`, commit 6f8f768f).

State after 6f8f768f: restore runs only for a sandbox made now (cloud_sync.rs:1431). That stops the refresh path re-applying and warning (LOG shows refresh warnings every 15-60 min through 11-03 06:00: 26 warning lines total, on the daemon built before the fix). What remains:
- Every replacement (made_now) warns again: LOG `...refs/tod/checkpoints/8f80f1e0.../sub/flagship does not apply to this branch (error: could not apply 4a9b9e37...); it is still on origin` at 10-28, 10-29, 10-30 x2, 10-31, 11-02, 11-03 05:43. Nothing records that this exact ref commit was already tried and failed.
- `flagship` conflict cause: the node path never moves submodules onto the node's branch. `checkout_script` checks out the superproject branch only (`--no-recurse-submodules` fetch, no `submodule update`), so a fork's `flagship`/`kb` stay at the golden's pinned commits, while `restore_repo` only fast-forwards if `HEAD` is an ancestor of the checkpoint parent (`checkpoint.rs:347-352`). The Files path does `branch_submodules` (`fleet/provision.rs:564`), the cloud-node path does not. The cherry-pick then conflicts.
- `kb` "has uncommitted changes" cause: the check is `git status --porcelain --ignore-submodules=all` inside the submodule itself (`checkpoint.rs:333`). A fork of a golden built from a dirty tree (`wsx-golden-...-dirty-...` names in the log) has untracked/modified files in `kb` from the start, so a checkpoint for `kb` can never apply to any fork of that golden; and the checkpoint itself was probably built from that same golden dirt (it saves everything not ignored, `save_repo`), so noise is being preserved as "work".
- A stale ref is never deleted: cleanup is (a) a branch push at a step boundary when the checkout is clean, (b) Discard/remove location, (c) `sweep` only for nodes gone or in merged/released/learn/done. `save_repo` returns `Saved::Clean` early when nothing changed (`:228-231`), so a clean tree never touches an old ref either. Deleting an un-restorable ref automatically would lose work, so the real defect is that the user has no way to resolve it and the app has no memory.

Fix:
1. Record restore outcome per ref: `(ref, remote_sha) -> restored | failed(reason)` in a local table (machine-local like `node_files_locations`); skip the attempt and the warning when the sha is unchanged and was already reported; show one persistent item on the node ("Saved work from <date> could not be applied") with actions: Apply as a patch file, Push to branch `tod/checkpoint/<node>/<rel>` and drop the hidden ref, Discard. Only that action deletes the ref.
2. Make the cloud-node checkout put submodules on the node's branch exactly as the Files path does (move `worktree::branch_submodules` into the shared checkout step), so `restore_repo`'s fast-forward applies.
3. Compute a baseline at sandbox creation (`git status --porcelain=v1 -z` per repo, stored in the sandbox as `/opt/tod/baseline-<rel>`); `save_repo` excludes baseline paths whose content is unchanged, and `restore_repo` treats baseline-only changes as clean. Also ask for the golden to be built from a clean tree (its name says `dirty`).
Regression test: yes, `fleet/checkpoint.rs` already has git-fixture tests (`Fixture`); add cases: submodule HEAD behind; baseline untracked files; `restore_into` twice with the same failing sha warns once.

---------------------------------------------------------------------------

## 9. The golden is validated per node, after the fork and proxy update, instead of once when it is picked; `resolve_golden` can pick an unready or failed golden
Severity: wastes-time
Where: `crates/tod-sandbox/src/provision.rs:134-170` (`requirements_script`), `:319-338` (`require_fork_prerequisites`), `crates/tod-sandbox/src/node.rs:680-687` (`install_into_fork`), `crates/tod-store/src/fleet/sandbox.rs:263-300` (`resolve_golden`).

What tod checks and when: `install_into_fork` runs the requirements script (sh git curl tar gzip bash ps scp sftp-server, node>=20.10, npm, claude-agent-acp; `claude` optional) inside EVERY forked node, on every `ensure_node_sandbox` including refresh, after the fork, the proxy update and the wake. LOG: 2026-10-28 17:02:10-17:02:40 five replacements failed back to back with `node>=20.10`, plus Run in the cloud at 17:02, 17:24, 17:38; each had already forked and started its proxy update. A golden's content is immutable (its name carries a timestamp), so this is a property of the golden, not of the node. Also `resolve_golden` (`sandbox.rs:272-296`): excludes only TERMINATED/DELETING (not FAILED), prefers `ready=true` but if no golden is marked ready it takes the lexicographically newest, including half-made ones, silently.

Fix: `Sandboxes::validate_golden(bx, name)`: run `requirements_script(true)` once against the golden (wake it), cache the result keyed by `(name, created_at)` in `<data root>/golden-checks.json`, and call it from `resolve_golden` (so the Files dialog's pick, the first fork after a new golden appears, and the daemon start all see it). A failing golden is an error naming the missing piece and, if an older golden validated OK, say that it will keep using that one instead of failing: pick the newest golden that passed. Keep a cheap per-fork sanity (`command -v git node claude-agent-acp`, ~50 ms) so a drifted environment is still caught. Never accept a golden with no `ready` label or `FAILED` status; require `ready=true` and state it in the error.
Regression test: yes. `resolve_golden` selection with fake listings (pure filter/sort, extract it), and validation caching, in `fleet/sandbox.rs` tests (`the_golden_alias_is_resolved_not_looked_up_as_a_sandbox_name` already exists there).

---------------------------------------------------------------------------

## 10. `sandboxes.toml` is read-modify-written from several threads/processes with a fixed temp name and a stale full save at the end of every provisioning
Severity: wastes-time (silent config loss, and hidden failure)
Where: `crates/tod-core/src/cloud_sync.rs:1424` (`let _ = sandboxes.save();`), `crates/tod-sandbox/src/config.rs:314-322` (`Config::save`), `crates/tod-store/src/fleet/sandbox.rs:51`, `:141-163` (`url` saves), `crates/tod-core/src/cloud_sync/lost.rs:73-100`.

Scenario: `ensure_node_sandbox` loads `Sandboxes` at its start (`:1236`), runs 20-45 s, then writes the whole stale struct back, ignoring errors. Several run at once (lost.rs spawns one thread per node, plus refresh, plus the app). Anything saved in between by another thread (a `forget_gone_sandboxes`, `set_database`, a golden/account edit in Settings, another sandbox's URL) is overwritten by the old copy. `Config::save` writes `sandboxes.toml.tmp`, a fixed name, so two simultaneous saves interleave or one `rename` fails with ENOENT (and that failure is dropped by `let _`). Nothing in `ensure_node_sandbox` changes the config except possibly `sandboxes.url(...)` at the top, so the final save has no purpose except to clobber.
Fix: delete the `let _ = sandboxes.save()` line (or save only if `url()` changed it, with the error surfaced); make `Config::save` take an advisory lock (`fs2` flock on `sandboxes.toml.lock`), re-read the file under the lock, apply a closure (`Sandboxes::update(|cfg| ...)`) and write via a unique temp name. 
Regression test: yes, `config.rs` tests: two threads updating different fields keep both.

---------------------------------------------------------------------------

## 11. A rotated GitHub, Claude, Linear or Blaxel credential never reaches a node's sandbox; `update_proxy` on a node is never used
Severity: wastes-time now, blocks-work on expiry
Where: `crates/tod-core/src/cloud_sync/lost.rs:329-331` (`proxy_is_current`), `:341-420` (`refresh_credentials`), `crates/tod-store/src/fleet/sandbox.rs:557-572` (`interactive_proxy_is_current`, `proxy_is_current`), `crates/tod-sandbox/src/node.rs:463-490` (labels), `:826-831` (fork_golden labels), `crates/tod-store/src/fleet/provision.rs:440-510` (`refresh_sandbox_proxy`, Files only).

Scenario: a node sandbox's labels carry `tod-env` (custom Environment credentials only, `node.rs:471-477`), not `tod-creds`. `refresh_credentials` compares only that label (`proxy_is_current(&info, &custom)`), so a changed GitHub token (fine-grained tokens expire in 30-90 days), Claude subscription token, Linear key or the Blaxel key is invisible; the Files path gets `refresh_sandbox_proxy` with `CREDS_LABEL` and `update_proxy`, the autonomous path does not (the doc says so: "tod does not yet update those in place"). The old token keeps working until it expires, then every `git push`, `tod-cli pr`, Claude call in that node returns 401 with no hint, and the only repair is deleting the sandbox (which is item 6's hazard).
Also note: `update_proxy` rewrites `spec.runtime.envs` from a GET that masks values as `****` (`blaxel.rs:141-155`); it relies on Blaxel keeping masked values (measured once, comment says so). If that ever changes it would blank every env var in the sandbox, and a fork's overridden proxy env goes through this same path. Cheap guard: after any `update_proxy`, read back one non-secret env (`TOD_NODE`) via `bx.run` and fail loudly if it changed.

Fix: label node sandboxes with `CREDS_LABEL = credentials_fingerprint(creds, via)` plus the Blaxel and orchestrator-host rule (use `rules_fingerprint(proxy_rules(...))`) at create and in `fork_golden`; add `lost::refresh_node_credentials(fleet, node)` that, when the label differs, calls `update_proxy` in place (no deletion) and then runs the readiness probe from item 1 (propagation lag applies here too); run it from the same launch pass that does `refresh::stale`. Rotation notes: a changed Claude token with `claude_via = env` needs the supervisor restarted (it is in the relay's env at start): reuse `start()`.
Regression test: yes, extend `lost.rs`'s `a_proxy_is_current_when_its_label_matches_the_credentials` for the new label; `update_proxy_body` is already pure (`blaxel.rs:141`) and can get a test that masked envs round-trip.

---------------------------------------------------------------------------

## 12. Install stamps and deploy hashes: when they falsely say "up to date" or "out of date"
Severity: wastes-time
Where: `crates/tod-core/src/cloud_sync/refresh.rs:28-77` (`install_stamp`, `record`, `STAMPS_FILE`), `crates/tod-core/src/cloud_sync.rs:1427` (`refresh::record(.. &refresh::install_stamp()?)`), `:1451` onward, `scripts/build.sh`, `scripts/deploy.sh`, `scripts/lib/stamp.sh`, `crates/tod-orchestrator/src/lib.rs:175` (`/health` returns just "ok").

False "out of date" (cost):
- `install_stamp` hashes the actual bytes of relay/supervisor/tod-cli and all bundles, so every dev rebuild of any of them makes every cloud node stale. `lost::check(Check::Full)` at each start then reinstalls every idle node in parallel (wake from standby, ~35 MB upload, relay kill/restart, supervisor poke): LOG shows refreshes at 11-02 13:47, 20:29, 21:51, 22:29, 22:42, 23:36, 11-03 00:08, 06:00, each 18-44 s per node x 5 nodes. That also contradicts `checkpoints.md`'s own reasoning that waking every parked sandbox at start costs more than it saves.
False "up to date":
- `ensure_node_sandbox` records `install_stamp()` freshly computed at the END of the call (`:1427`), not the stamp of the payload it uploaded. If a rebuild lands during the 20-45 s the sandbox has the old bytes but is recorded as current. (`reinstall` then overwrites it with the `current` it computed at the start, `refresh.rs:130`: the two disagree; pick the payload's.)
- The stamp does not include what else defines a sandbox: proxy rules/credentials (item 11), the golden/image it came from, the orchestrator URL/host baked into its env (`TOD_ORCHESTRATOR_CLI_URL`), `claude_token_via`, scheduler, workspace, agent. A change to any of those leaves nodes "current". `ensure_base`'s `base_fingerprint` (`node.rs:724`) does include the create body; the stamp used for live nodes does not.
- `record` rewrites the whole JSON file with `std::fs::write` (non-atomic) under an in-process mutex only; two processes (app + daemon) can interleave and a crash mid-write yields invalid JSON, which `load` turns into an empty map via `.ok()...unwrap_or_default()` (`refresh.rs:59-62`), so every node counts as stale and is reinstalled, silently.
- `install_stamp` skips `tod-supervisor`/`tod-cli` when not built (`refresh.rs:36-41`), as `upload_tod_files` does with only a progress "warning:" (`node.rs:641-651`), so a node can be provisioned without a supervisor, stamped current, and later "fixed" when the binary appears.
- `orchestrator-deployed.hash` and `watchdog-deployed.hash` are per data root and record what THIS machine last deployed, never what the live orchestrator runs: a redeploy by `deploy::ensure_orchestrator` at launch (which deploys whatever binaries are on disk, `deploy.rs:63-103`) or from another machine/user leaves the hash saying up to date or out of date incorrectly. `/health` has no build id. `.source-hash` hashes sources not outputs, so a hand-copied or partial binary set passes it.

Fix: (a) record the stamp of the payload actually sent (compute once at the top, pass to `record`). (b) Add credentials fingerprint, golden name, orchestrator host, scheduler, via, agent to the stamp, stored as separate fields so the reason for reinstall can be shown ("proxy credentials changed" vs "new build"). (c) Make `record` atomic (temp + rename, file lock) and a corrupt file an error shown to the user, not an empty map. (d) Do not reinstall idle nodes at launch just because bytes changed in development: reinstall lazily, at the node's next wake/turn (`wake_node_with` already holds the sandbox awake), and only eagerly when the protocol/version number the supervisor and app exchange is incompatible. (e) Give the orchestrator `GET /health` a `{"build": "<source hash>"}` and have `deploy.rs`/`build.sh` compare against the live one instead of a local file.
Regression test: yes for (a)-(c) in `refresh.rs` tests (extend `a_record_is_kept_per_node`; corrupt file case); (e) in `crates/tod-orchestrator` tests.

---------------------------------------------------------------------------

## 13. Errors that are warned about and carried past (violates "errors are never hidden")
Severity: mixed; each is a place a node is "accepted" while broken.
- `crates/tod-core/src/cloud_sync.rs:1415-1423`: after the final poke, a non-2xx is only `progress("warning: the poke got ...")`; the node is recorded as running and nothing wakes it. (`wake_node_with` at least bails, `:1783`.) The poke is a bare `ureq::post` with no retry, so it also fails on the item-2 404/502s.
- `crates/tod-sandbox/src/node.rs:641-651`: a missing `tod-supervisor` or Linux `tod-cli` is a progress warning and provisioning goes on, "the node will not run on its own" (LOG 2026-10-21/22, 8 times). The accepted node never starts. This should be an error before any sandbox is made (read both binaries at the top of `ensure_node_sandbox`).
- `node.rs:868-885` (`ensure_claude_adapter`, image path only): install failure is a warning, though a Claude node cannot run without it ("the Claude ACP adapter is not installed" at every step later). The fork path correctly fails in `require_fork_prerequisites`.
- `cloud_sync.rs:1269`: a failure reading the node's Environment is a warning and the node gets no custom credentials; `:1272` no GitHub token is only a warning (the node "cannot push or open a pull request": that is a node that cannot do its job). `fleet/provision.rs:392-409` has the same for the Files path.
- `cloud_sync.rs:1358` `let _ = bx.delete(&name)` after a failed fork (orphan, silently); `:1424` `let _ = sandboxes.save()`; `:1342-1346` and `lost.rs:399` wait 120 s for a deleted sandbox to vanish and carry on when it did not (the later create then fails with "already exists" and no mention of the real cause).
- `cloud_sync.rs:208-211` `cloud_node()` `.ok()?` turns a database read error into "this node does not run in the cloud".
- `cloud_sync.rs:193-205`: a failed read of `deleted_by_user` is logged and counts as `false`, so the app may recreate a sandbox the user deliberately deleted.
- `provision.rs` `relay_output` (`:415-436`): any failure (timeout, no runtime, transport) is `None`, i.e. "relay not reachable", which then selects the Install branch (item 5).
- `fleet/provision.rs:168` `forget_lost_sandbox(...).unwrap_or(false)` swallows a store error that would have said why the retry did not happen.
- `refresh.rs:59-62` a corrupt stamps file is an empty map (item 12).
- `lost.rs` `reinstall` failure: logged, "tried again at the next launch": a node stays on an old supervisor for days with only a note.
Fix: make each of the above `bail!`/`Err` with the cause, or, where a degrade is truly wanted (no Claude adapter on a mock-agent node) make it an explicit configured option. Add a clippy-style grep test (`#[test]` scanning `cloud_sync*.rs` and `node.rs` for `"warning:"` and `let _ = ` against an allow-list) so new ones are noticed.
Regression test: yes per item, mostly through the existing `cloud_sync/tests.rs` patterns.

---------------------------------------------------------------------------

## 14. What each Blaxel call does on standby / terminated / deleting / not found / 429 / 5xx
Severity: wastes-time overall; feeds items 1-5. Based on `crates/tod-sandbox/src/blaxel.rs`; global agent timeout 60 s, connect 15 s (`:194-203`). No call except `upload` and `wait_reachable` retries anything; 429 is never mentioned.

| Call | 404 | 401/403 | 5xx/429/transport | Retried? | Timeout |
|---|---|---|---|---|---|
| `get`/`get_json` (`:208-230`) | `None` | "not authorized" | error, `?` | no | 60 s |
| `list` | n/a | same | error | no | 60 s |
| `create`/`create_from_body` | error | same | error | no | 60 s |
| `fork_with_envs` (`:343`) | "no sandbox named" | 403 special text | error | no | 60 s |
| `delete` | Ok | same | error | no; callers poll `get` up to 120 s and ignore expiry | 60 s |
| `update_proxy` = `get_json` + PUT | "no sandbox named" | same | error | no; no propagation wait (item 1) | 60 s |
| `wait_deployed` (`:506`) | `None` for >30 s -> error | `?` on `get` | the first failed `get` aborts the whole 300 s wait | polls every 2 s | caller's |
| `wait_reachable` (`:533`) | counted as reachable (item 2) | not examined | 502/503/504 and transport errors retried every 3 s | yes | 30 s per probe, 180 s total |
| `run` (`:562`) | error (WORKLOAD_UNAVAILABLE not retried) | error | error | no | `timeout_secs + 60` |
| `start`/`start_with_env` | error; 400 "already exists and is running" not handled | error | error | no | 60 s |
| `kill` | Ok (also for platform 404: wrong) | error | error | no | 60 s |
| `upload` (`:699`) | error | error | 502/503/504 retried 3x (2/4/8 s); 422 `no space left` surfaced raw | partial | 300 s |
| `upload_large` | as upload; parts leaked on error (item 5) | | | per part | |
| schedules (`create_schedule`, `delete_schedule(s)_named`) | delete: Ok | error | error | no | 60 s |
- `wait_deployed` and `status`: it returns when `status == DEPLOYED` and a URL exists; it never looks at `state` (RUNNING/STANDBY/ERROR) or at readiness. For a fork, `DEPLOYED` appears in <1 s (item 1); for an expired sandbox it stays `DEPLOYED` for about an hour (item 3).
- Standby: control-plane reads do not wake; any sandbox-URL call wakes it and returns 502/503/504 or the 30 s-held 404 until it is up. Only `upload` (14 s) and `wait_reachable` cover that, and `wake_node_with` (`cloud_sync.rs:1737-1795`) calls neither: `push_supervisor_settings` (`bx.upload`) is the first call to a possibly parked sandbox.
- Terminated/deleting: `get` returns the record with that status for a while after delete (`TERMINATED` lingers); `sandbox_is_dead` handles `status`, and `create_with_proxy` (`fleet/sandbox.rs:317`) allows reuse of a name when TERMINATED, but `ensure_node_sandbox` waits for `get().is_some()` to go away (up to 120 s), which TERMINATED never does (LOG shows it did not stall in practice because the next call forks anyway; the wait is dead code for that case).
Fix: one `http` helper in `blaxel.rs` with: classification (`Retryable { after }`, `NotReady`, `Gone`, `Auth`, `Fatal`), retry budgets per verb as in item 2, `Retry-After`, jitter, and a 429 policy; every public method goes through it; error type carries `kind`. Remove the 120 s wait for TERMINATED (treat `TERMINATED/DELETING` as gone). Make `wake_node_with` call `wait_reachable` (fixed) first.
Regression test: yes; the classifier is pure, the helper is testable with the stub server.

---------------------------------------------------------------------------

## 15. Provisioning order, what is recorded when, and crash behavior (summary of the current state)
Severity: informational, supports items 4 and 5.

`ensure_node_sandbox` (`cloud_sync.rs:1227`): read creds/source (network: orchestrator URL, `origin_url_of`, possibly the dead sandbox) -> `bx.get` -> (dead: delete, wait) -> fork or create (+ `update_proxy` for forks) -> `wait_deployed` -> `wait_reachable` -> `install_into_fork`/`provision` (prereq check, uploads, bundles) -> `start` (checkout, kill+start relay, poke supervisor) -> `push_supervisor_settings` (uploads) -> poke -> `sandboxes.save()` -> `refresh::record` -> restore checkpoint. Only AFTER it returns: `record_cloud_node` (`cloud_nodes` row via the writer, then sync) and the clearing of `deleted_by_user`.
- Recorded before the sandbox exists: nothing. Recorded after it works: `cloud_nodes`, stamp file. Never recorded: node sandboxes in `sandboxes.toml`; `node_files_locations` (the Files path records its row before the checkout "so the sandbox is never lost track of", `fleet/provision.rs:653`, but the cloud path does not).
- A crash between fork and `record_cloud_node` leaves an orphan with no record (item 4). A crash between `record_cloud_node` and the sync leaves the orchestrator not knowing the node is active until the next sync (`run_in_cloud_reporting` syncs after; a warning only, `:1556`).
- Re-entrancy: the call is idempotent by NAME only. It is safe to repeat only if the previous attempt left a sandbox that is truly good; the checks that would tell (proxy effective, disk free, relay manifest sane) do not exist (items 1, 4, 5).
- Wait budget: `wait_deployed` 300 s + `wait_reachable` 180 s + uploads without a total budget + 150 s supervisor wait (`start_in_cloud`), and no overall deadline or cancellation, so a stuck call holds the node's "starting" lease (shown as working) indefinitely if the HTTP call never returns (60 s global agent timeout bounds each call, not the sequence).
Fix: see item 4 (provision_state + lock + record-first) and add an overall deadline with a typed `Timeout` error per phase.

---------------------------------------------------------------------------

## Design changes that remove whole classes of failure
1. One Blaxel HTTP layer with classification and retry/backoff, honoring `safe_to_retry_request` and `Retry-After`, plus `Blaxel::with_base_url` so everything above can be tested with a stub server. (Items 2, 14, parts of 1, 3, 5.)
2. Replace "status says DEPLOYED, so it is fine" with a single `SandboxHealth` probe, run after create/fork/PUT/wake and before reuse: reachable (not WORKLOAD_UNAVAILABLE), disk free, relay manifest sane, `git ls-remote` through the proxy works, `TOD_NODE` env intact. `ensure_node_sandbox` becomes: decide (Reuse / Repair / Fork) from health and the lost mark, instead of from `status` and the made_now guess. (Items 1, 3, 4, 5.)
3. A node-sandbox state machine persisted in `cloud_nodes` (`forking -> installing -> verifying -> ready`, `last_error`, `attempts`) with a cross-process per-node lock; the sandbox name is written before the first Blaxel call; failures after creation delete the sandbox; a label-based sweeper removes orphans. Removes half-made sandboxes and duplicate provisioners. (Items 4, 15.)
4. Put the node's proxy rules in the fork request (or verify propagation before use); until Blaxel supports that, treat "proxy ready" as an explicit probed state, never an assumption. Add `tod-creds` fingerprint labels to node sandboxes and rotate in place with the same probe. (Items 1, 11.)
5. Validate a golden once when it is picked/resolved (cached by name and creation time), pick the newest golden that passed, and keep only a cheap sanity check per fork. (Item 9.)
6. Shrink what is synced: exclude large before-images from `sync_changes`, prune it, send node-scoped snapshots to supervisors, stream and compress seeds. That fixes the 338/370 MB copies, the orchestrator's disk, and cold-start time of every node. (Item 7.)
7. A disk budget: check-and-clean before every install/wake/checkpoint, atomic writes for the manifest and stamps, cleanup of leaked parts on error. A full disk must be a named error, never a reinstall loop. (Item 5.)
8. Persist outcomes the user must resolve (unrestorable checkpoint, failed replacement, last error) as node items instead of logs and in-memory maps, and give every destructive action an explicit "anyway" path with a named loss. (Items 3, 6, 8.)
9. Make launch-time reinstall lazy and reason-carrying, and stamp what was actually sent, including credentials/golden/orchestrator. (Items 11, 12.)
10. Enforce "errors are never hidden" mechanically: allow-list test that fails on new `warning:` progress lines and `let _ =`/`.ok()` in the provisioning modules. (Item 13.)

## Commands to confirm the two inferred points before changing code
- Item 1 (propagation lag): in a fresh fork, right after `update_proxy`, loop `bx.run(url, "GIT_TERMINAL_PROMPT=0 git ls-remote origin HEAD; echo $?")` every 2 s and log the time until it flips to success, together with `env | grep -i proxy | sed 's/=.*//'`, `git config --system -l`, and `bx.get_json(name)["spec"]["network"]["proxy"]["routing"]` destinations.
- Item 5 (disk): `df -h / /var/lib/tod-supervisor /tmp; du -xh --max-depth=2 / | sort -h | tail -25; ls -la /opt/tod /var/lib/tod-supervisor/*` in a node that has run for a few days, and on the golden itself right after it is built.
