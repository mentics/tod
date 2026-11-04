# Runner menu live check, 2026-11-03

Run against real Blaxel sandboxes in the live-verify root
(`~/.local/share/tod-live-verify`, owner `jshellman-live-verify`), main at
`893815ab` plus the fixes listed below. Test nodes `RM live A` and `RM live B`
under `ENG work` (Files = `Golden flagship`, a cloud sandbox, fork of the
golden); a throwaway PR `Cofactr/workspace#192` made by A's own agent. Schema
v93. The orchestrator was redeployed twice (it is shared). No test sandbox is
left; both nodes' sandboxes were deleted at the end.

## Results

| # | Check | Result |
|---|---|---|
| 1 | Menu by state, cloud node: never run | PASS. Start default; menu Run with a new session, Check now, Migrate to. |
| 1 | After Start, runs under the supervisor | PASS. Pause default; Stop now, Migrate, Reset environment, Delete sandbox (both), Sync now; no Check now. |
| 2 | Pause (finishes the step, then stops) | PASS. "you paused it", node stays paused. |
| 2 | Resume, Resume with a fresh budget, Run with a new session | PASS (all three consumed by the supervisor; log "consumed a request"). Line read "Asked for to go on": fixed. |
| 2 | Stop now | PASS mid-turn (marker file seen, turn cancelled, "you stopped it"). BUG 3: its row stopped the next run. Fixed. |
| 2 | Check now on idle node | PASS in `pr_draft`: failing criteria listed, node not advanced (PR still a draft); "certificate is stale: " ended empty in `proposed` (BUG 4, fixed). Advance on clear was not reached (no gate was clear). |
| 2 | Pause stays paused | BUG 2: the app's Check now woke the node and it ran again. Fixed. |
| 2 | Sync now | PASS (menu entry present while supervised; syncs ran). |
| 3 | Reset environment | PASS on an idle node in `pr_draft`: "The environment was set up again", supervisor reinstalled (checked `/opt/tod/tod-supervisor` hash = this build) and the node stayed waiting. Refused while running: PASS ("cannot reset the environment while the node is running ... Stop it first"). |
| 4 | Delete sandbox while running: prompt | PASS. "This node is running. Stop it and delete its sandbox?" with Stop and delete / Cancel. |
| 4 | Cancel changes nothing | PASS (not exercised separately: the prompt was dismissed by the answer; run kept going). Partial. |
| 4 | Stop and delete | PASS. "Stopping, then deleting the sandbox..." then "Deleted sandbox node-rm-live-b; any uncommitted work was kept on the remote". |
| 4 | Start makes a fresh sandbox after delete | PASS (forked from the golden, healthy in about 40 s). |
| 4 | Delete without a checkpoint | PASS ("without keeping its uncommitted work"); `cloud_nodes.lost_at` set with the user-deleted mark. |
| 4 | Files record and `cloud_nodes` consistent | NOT APPLICABLE YET. A node started with Start has a `cloud_nodes` row and no `node_files_locations` row at all (stage B2/B3, not built). |
| 5 | Migrate to a host while idle | BUG 5. Without a Files location row the impact was empty, nothing was pulled or removed, the old sandbox and the cloud row stayed and the supervisor kept running there. Fixed; re-run PASS: sessions (17 logs) in `sandbox-sessions/<node>/`, `autopilot/<node>.json` pulled, old sandbox deleted. |
| 5 | Migrate while running asks Pause / Stop now / Cancel | PASS for the pending migration (it waited for the run to end, then ran). The three-button prompt itself was not captured. |
| 5 | Node with children refuses | NOT REACHED (store-level test covers it). |
| 5 | Resume the same session at the host, migrate back to a sandbox | NOT REACHED: the host location was `rmhost/repo` with no agent run. |
| 6 | Lifecycle config "Delete sandbox" checkbox | NOT REACHED: the editor is not in the task panel; no time to find it. `delete_sandbox` is NULL (default on) on both config rows. Walk to `done`: not reached. |
| 7 | `golden` lists; `golden refresh cofactr/flagship` | BUG 6 found and fixed; then PASS. The user's `wsx-golden-...` was left in place ("another repository's golden setting still points at it", and no `tod-kind` label); the tod-made one was deleted on the second refresh. |
| 7 | Node fork from the refreshed golden | PASS after the fix: forked, healthy, back to waiting in about 40 s. Still ran "installing 77 bundle files" (per-node install is B2). |

## Bugs

1. `conversations.skill_group` was added inside the v73 migration step. A
   store already past v73 never got it, so the orchestrator's snapshot slice
   of conversations failed ("no such column: r.skill_group"), a new supervisor
   could not seed and exited 1. Fix: made idempotent with the rest in
   `apply_migrations`; test `a_store_past_v73_without_skill_group_gets_the_column`.
2. A paused or stopped node was run again by the next poke (a context change
   from Check now, a retry timer). Fix: `user-stopped` state file, wake leaves
   the node unless a request to run arrived (`tod-supervisor`); test
   `a_paused_node_stays_paused_until_asked_to_run`.
3. Stop now ended the run through the marker file, and its `stop_now` row
   arrived a pull later, stopping the next run at once. Fix: the marker stop
   records an owed row, consumed at the next wake or boundary; test
   `a_stop_taken_from_its_marker_takes_its_row_when_it_arrives`.
4. A node waiting on something outside (no default action) drew no runner
   button, so Check now, Migrate, Reset and Delete were unreachable in
   `pr_draft`. Fix: a disabled "Actions" face with the caret; test
   `a_node_with_a_menu_but_no_default_still_gets_a_button`. Also the gate's
   "certificate is stale: " with nothing after it now says why, and "Asked for
   to go on" now reads "Asked for it to go on".
5. See Migrate row above (`session_log::node_sandbox`, `confirm_migrate`).
6. A golden made by `golden refresh` had an empty proxy (forks do not keep it),
   so `fork_golden` refused it. Fix: placeholder proxy, verified before ready.

## Not fixed, seen

- Migrate right after Stop now can race the supervisor's last push: the
  `cloud_nodes` row removal conflicted ("1 conflicting") and the row came back
  naming the deleted sandbox (RM live B). The row for a migrated-away node
  should be dropped after the supervisor's heartbeat is gone.
- Migrate pulls every Claude log in the sandbox (17 files, 6 MB), including the
  golden's own, not only the node's sessions.
- The panel kept saying "in the cloud (sandbox ...)" for a node migrated off
  (before fix 5); with a lingering row it still can.
- Stage B2/B3 are what make the Files record and `cloud_nodes` agree; fix 5 is
  a stopgap for Migrate until then.
- Another session's `tod` (cwd `robustness-continuation-743eac`, port 47611)
  was running on this root; I used port 47625 and left it and the root's
  `tod-agentd` alone.

## Second pass (same day)

Rebased on origin/main 7f67651c (no change). Test nodes `RM live A/B/C`;
sandboxes `node-rm-live-b` and `-c` deleted at the end (A's earlier). The app was
left stopped. `independent_evaluation` was flipped to false in the root's
`tod.yml` for one `phase certify` and restored at once (verified).

| Check | Result |
|---|---|
| Lifecycle config editor: where | Right-click a node, Settings (or select it and use the Settings entry); expand the Lifecycle config capability; the Done block is last. (`e`/Ctrl+E opens the inline editor, not this.) |
| "Delete sandbox" default on | PASS: checked on `ENG work` with no stored value (NULL). |
| Toggle persists | PASS: unticking wrote `delete_sandbox = 0` on `ENG work`. |
| Inherited | PASS: `RM live C`, with lifecycle-config enabled and nothing of its own, showed the box (unchecked, since the parent was off) with "from ENG work"; ticking it wrote `1` on C, and "Use inherited" appeared. |
| Sandbox deleted automatically at `done` | NOT REACHED. A node cannot be cheaply walked to `done` (needs the full lifecycle, PR and merge). The code path was `delete_sandboxes_at_final_phase`, run only inside an orchestrator sync; B4 replaced it with the daemon's `sandbox_retire`; only unit tests were relied on. |
| Check now advances a clear gate | PASS. `RM live C` in `proposed` with its phase certified (independent evaluation off for the certify): the line read "Checked: the gate is clear; moved to design". |
| Delete prompt, Cancel | PASS: node kept running, nothing deleted. |
| Migrate while running: Pause / Stop now / Cancel prompt | PASS: shown ("Migrate to RM host once it has paused ... or been stopped now?"). Chose Pause. The migration then did not begin: the Pause job stayed busy ("Asking the supervisor to pause...") because the sandbox would not wake (Blaxel answered 404 WORKLOAD_UNAVAILABLE for 90 s; the app logged "replacing it"). Blaxel flakiness, not looked into further. |
| Same Claude session resumes at the new location; migrate back | NOT REACHED (blocked by the above). |
| Migrate refused for a node with children | NOT REACHED live (my click on the parent's menu missed; the menu hides Migrate for such a node and the store refuses, both unit tested). |

### More bugs

7. **Start of a fresh node failed after `golden refresh`**: the node's repository
   is read from the golden it forks, and with a per-repository golden beside the
   account-wide one the alias was ambiguous ("there is more than one golden
   sandbox ... say which repository's is meant"). Fix: ask every golden and use
   the URL they agree on (commit `Start: a fresh node's repository ...`;
   test `every_distinct_golden_is_listed_for_a_caller_with_no_repository`).
   Verified live: C started.
8. **Over-wide log pull** (listed before): fixed, Migrate keeps only the sessions
   of the node's conversations (test `only_the_sessions_of_the_nodes_own_conversations_are_kept`).
   Not re-run live (the migrate was blocked).

### Not fixed

- **The row left behind after Migrate right after Stop now.** Reproduced once;
  the cause (the supervisor's last push re-creating the row) needs the
  orchestrator in the loop to test, and I did not want to ship an untested
  guess. A sketch: after the removal sync, re-read the row and remove it again
  if it came back.
- A Pause on a node whose sandbox just went to standby can hang for minutes
  on Blaxel's WORKLOAD_UNAVAILABLE and blocks a pending Migrate behind it.

## Pass 3 (B2-B5), 2026-11-03

Main at `400e2545` (B2-B5 and the wake fix) plus the commits listed below, schema
v93. Live-verify root, owner `jshellman-live-verify`, my own agent-socket port
(47631); the root's `tod-agentd` is my build's (the app restarted it at launch:
an older build's daemon was replaced). **The shared orchestrator was redeployed**
(B2/B3 changed the synced data). Test nodes `RM live A/B/C`; sandboxes
`tod-rm-live-b-847327` and `tod-rm-live-c-eba2f7` (the new `tod-<slug>-<6hex>`
names). A second `tod` from another session (`robustness-continuation-743eac`, port
47611) stayed running on this root the whole time: it is an older build, logged
"Could not checkpoint ..." for my sandboxes every ~30 s, and it can overwrite
`sandboxes.toml` with a stale copy (the golden mapping went back to a deleted
golden twice, see bug 4).

| # | Check | Result |
|---|---|---|
| 1 | Pre-B2 nodes report loudly | PASS. A/B/C (`cloud_nodes` row, no location): "in the cloud (its Files location records no sandbox)". Their sandboxes were gone, so Adopt existing sandbox on C answered "Blaxel no longer has sandbox node-rm-live-c ... nothing was changed. Start makes the node a new one" (PASS: loud, no change). Start remade C from the golden; name `tod-rm-live-c-eba2f7`. "installing N bundle files" still appears (see bug 1: a fork cannot keep what its golden had installed). |
| 1b | Older/unstamped golden | PASS with a different shape than the plan said. The `wsx-golden-*` has no `tod-install` label, so Start refreshed it itself ("is being refreshed: it carries no tod-install label", new golden `tod-golden-cofactr-flagship-6aea8d1b`); a stale `golden` mapping to a deleted sandbox gave "the golden sandbox ... does not exist". The "both stamps" message exists only for a golden a NEWER tod made (not reached). The fork then failed its health check with the loud "was forked from <golden>, which should carry this build's relay ..." until bugs 1 and 2 were fixed. |
| 2 | Start on a fresh throwaway node | PASS after fixes 1-3. Fork from the golden in ~30 s, relay healthy with the node manifest, supervisor ran the node (plan 1/1, PR agent handed back), `node_files_locations.sandbox` = `tod-rm-live-c-eba2f7` = `cloud_nodes.sandbox`. |
| 3 | Naming collision | PASS. A `tod-sandbox create tod-rm-live-a-66e15f` and Start on A: "Cannot make sandbox ... a sandbox by that name already exists (DEPLOYED), but this node's Files location does not point at it, so tod lost track of it ... Nothing was made, renamed or deleted." The throwaway still existed afterwards (then I deleted it). |
| 4 | Reset environment | PASS. "The environment was set up again."; nothing deleted (same sandbox name before and after), relay and manifest healthy, node still waiting. |
| 5a | Migrate to a host while idle | PASS. The impact dialog now exists for the single record ("Nothing uncommitted: its branch is pushed, then it is removed."); after "Remove and migrate" the sandbox was deleted, the location and `cloud_nodes` row gone, "Migrated to RM host", and `sandbox-sessions/<node>/` held exactly the node's 7 session logs. (My first host location needed `treehouse`, absent here; I pointed `RM host` at a scratch git repo with no worktree recipe. Its `open-pr`/`fix-ci` skills were missing, loudly, until I added throwaway ones.) |
| 5b | Same Claude session resumes at the host | NOT VERIFIED. The host run stopped at "Claude Code is not signed in on this machine"; signing in is the user's. The new host project directory only held a fresh session, so I cannot say the old id was copied. |
| 5c | Migrate back to a cloud sandbox resumes it | PASS for the transfer: "restored 7 agent session log(s)" in the new sandbox, including the PR session; the agent then ran there. The same-id resume itself was not compared. |
| 5d | Migrate while running offers Pause / Stop now / Cancel | NOT REACHED. Each turn on my throwaway nodes ends in ~20 s and the control socket plus screenshot lag lost the window four times. |
| 5e | Node with children refused | PASS by absence: the menu has no Migrate for a node with a child (the refusal text is only the store's, unit tested). |
| 6 | Pause (any wake) on a standby sandbox | PASS for the wake path, through "Run with a new session": C's sandbox stayed at Blaxel's `WORKLOAD_UNAVAILABLE`; after 90 s "sandbox tod-rm-live-c-eba2f7 is still waking after 90s ... It was not replaced or deleted; the fresh_session request is recorded ... Try again in a minute". Not exercised: a queued Migrate behind it, and the Pause request itself (same code, `still_waking_error`). The node then read "working: starting in the cloud" for minutes: bug 3, fixed. |
| 7a | Delete at done, setting off | PASS. B reached `done` (I set `node_lifecycle.state = 'done'` for B directly in the root's `tod.db`; B inherited `delete_sandbox = 0` from `ENG work`, left by pass 2): 2.5 minutes, nothing deleted. |
| 7b | Delete at done, setting on, no click | PASS. I set B's own `node_lifecycle_config.delete_sandbox = 1` directly in the DB; 63 s later `agentd.log` (this root's daemon, mine) said "sandbox retire: RM live B reached done: Deleted sandbox tod-rm-live-b-847327; any uncommitted work was kept on the remote ...", the location and the `cloud_nodes` row were gone. |
| 7c | Delete failure path | NOT PROVOKED (no safe way). |
| 8 | Delete sandbox menu on the single record | PASS. On C, whose sandbox would not wake: "Delete sandbox" failed loudly ("the checkpoint ... failed, so nothing was deleted; delete it anyway to discard its uncommitted work"), then "Delete sandbox without a checkpoint" deleted it ("Deleted sandbox ... without keeping its uncommitted work"; location forgotten, the `cloud_nodes` row kept with `lost_at`, the user-deleted mark). The checkpointing delete on a healthy sandbox is B4's path (7b). |
| 9 | Stop now then Migrate immediately: stale row | NOT REACHED (same timing as 5d). After Migrate off the host, B's/C's rows were consistent in every path I did run. |

### Bugs found in pass 3

1. **A fork of the golden loses everything tod installed in it.** `fork_golden` ends
   with the node's proxy update, which redeploys the fork from its image: the
   golden's relay, manifest, supervisor and bundles are gone (a plain
   `tod-sandbox fork` of the golden keeps them, checked). B2's "a fork installs
   nothing" therefore left every fresh node with no node manifest; the
   next connect ran the generic bootstrap over it, the health check then saw
   `bootstrap=... agents=false` instead of `node relay=...`, and the Start failed
   after minutes of retries. Fix: `make_sandbox` installs tod's files into the fork
   (`node::install_into_fork`, no packages) and starts its relay before the checkout
   steps. The "no `installing N bundle files` on a fork" part of B2 does not hold
   on Blaxel.
2. **`golden refresh` produced a golden with nothing installed**, for the same
   reason (the placeholder proxy update came after the install). Fix: proxy first,
   install after; and `node::upload_tod_files` makes `/opt/tod` before its first
   upload (the large-upload free-space check failed on a missing directory). Live:
   the refreshed golden answers with the node manifest.
3. **A failed Start left the node saying "working: starting in the cloud".** The
   `starting` run state written before the sync stayed for the 150 s a supervisor
   is given to report in, even when the sandbox could not be made. Fix:
   `failing_start_is_recorded` replaces it with `runner_failed` (test
   `a_start_that_fails_leaves_the_node_failed_not_starting`), and the wake path
   does the same only while `starting` is still the state. Verified live for Start.
4. **Seen, not fixed:** `make_sandbox` does `Sandboxes::load` ... `save` around a
   multi-minute creation, so a concurrent `golden refresh` (or another app on the
   root) is overwritten with the old golden mapping; and the failure text after a
   fork that never got made still says "was deleted so the next launch makes a
   fresh one".
5. **Seen, not fixed:** Blaxel left one forked sandbox in `STANDBY` that answered
   `WORKLOAD_UNAVAILABLE` for 25+ minutes; every tod operation on it failed loudly
   after 82-90 s, as designed, and only Delete (without checkpoint) cleared it.

### Cleanup

Deleted: nodes `RM live A/B/C`, their sandboxes, the `RM host` location (and the
scratch repo and Claude project directory behind it), the `tod-rm-live-a-66e15f`
throwaway, two failed golden forks. Left: the user's `wsx-golden-...` (another
setting still pointed at it), the refreshed `tod-golden-cofactr-flagship-6aea8fd9`
(now `cofactr/flagship`'s golden), and `ENG work`'s `delete_sandbox = 0` from
pass 2 (the default is on; turn it back with the editor if wanted).

## Pass 4, 2026-11-04

Main at `6c395438`. Live-verify root, my own agent-socket port; another session's
older `tod` (port 47611, started 9:50) stayed running on the root and was not touched.

### Step 0: what the stopped agent left

- Nodes `RM p4 A` and `RM p4 B` (both in `pr`), `node_files_locations` rows
  (sandbox, `fork:golden`) and `cloud_nodes` rows for `tod-rm-p4-a-89a8d9` and
  `tod-rm-p4-b-186bd3`; both sandboxes live (STANDBY), labelled `tod-kind=node`.
- No throwaway golden: `sandboxes.toml` maps `cofactr/flagship` to
  `tod-golden-cofactr-flagship-6aeb217c` (the repository's real tod golden,
  labels intact: `tod-schema=93`, `tod-built=1793793033`). A stale
  `[[sandbox]]` entry for the already deleted `...-6aea8fd9` remains (harmless).
- Left alone, not ours: `wsx-golden-...`, `product-ingest`, `tod-orchestrator`, and
  three older `cloud_nodes` rows from earlier passes (`node-nexar-...`,
  `node-webhook-e2e-build-test`, `node-missing-skill-test`: no sandbox in Blaxel).
- The previous agent's A/B nodes are reused below.

Build: main `6c395438`, sandbox binaries rebuilt (`scripts/build-sandbox-binaries.sh`),
**the shared orchestrator was redeployed** (`tod-sandbox orchestrator --bin target/sandbox/tod-orchestrator ...`).
My `tod` ran on agent-socket port 47641. Test nodes: the previous agent's `RM p4 A`
(sandbox `tod-rm-p4-a-89a8d9`) and `RM p4 B` (`tod-rm-p4-b-186bd3`, linked to the throwaway
draft PR Cofactr/workspace#192). A's plan step was changed to three foreground
`python3 time.sleep(100)` runs (about 5 minutes of turn), its `pending` status set with
`tod-cli plan update`.

| # | Check | Result |
|---|---|---|
| 2a | Migrate while RUNNING asks Pause / Stop now / Cancel | PASS. Prompt: "This node is running. Migrate to RM host once it has paused (it finishes this step) or been stopped now?" with Pause, Stop now, Cancel (screenshot, node mid-`sleep`, checked with `exec ps`). |
| 2b | Cancel changes nothing | PASS. Prompt gone, node still `working`, `cloud_nodes` row and sandbox untouched. |
| 2c | Pause, migration proceeds when idle | PASS. "Pause asked of the supervisor", the turn finished its sleep, the node read "on you: paused", then the "Migrate this node?" impact dialog appeared by itself ("Nothing uncommitted: its branch is pushed, then it is removed."). I cancelled that dialog (node stayed paused in the cloud). |
| 2d | Stop now, then it migrates | PASS. Stop now mid-`sleep`; within ~9 s the node was "paused" and the impact dialog was up. |
| 1 | Stop now then Migrate immediately (stale `cloud_nodes` row) | PASS in the sense that matters: I clicked "Remove and migrate" at once after the Stop now dialog appeared; 3 minutes later `cloud_nodes` had no row for the sandbox, the Files location of A is the host, `tod-sandbox list` no longer shows `tod-rm-p4-a-89a8d9`, "Migrated to RM host". The pre-fix race (a late supervisor push) did not occur this time, so the "drop is reported" log line was not seen live; the drop itself is unit tested. |
| 7a | Check now in `pr_draft` with the real PR #192 | PASS for the hold: B set to `pr_draft` (I set `node_lifecycle.state` in the root's `tod.db` directly), Check now answered "Checked: the gate is clear, but the node was not moved on: the pull request needs the agent: Cofactr/workspace#192: behind its base", state unchanged. The pure human-review (draft) hold was not separated from this: that needed marking #192 ready for review, which would notify Cofactr/workspace code owners, so I did not. |
| 8 | Same Claude session resumes at the host | NOT VERIFIED: `claude auth status` on this machine says `loggedIn: false`; signing in is the user's. |

| 6 | Newer-golden refusal | PASS. A throwaway fork of the repository's golden (`tod-rm-p4-newer-golden`), labelled `tod-schema=94`, `tod-built` in the future and another `tod-install`, was mapped for `cofactr/flagship` in my root. Start on a new node `RM p4 C` failed with "The golden sandbox for cofactr/flagship was refreshed by a newer tod (install aaaabbbb..., schema 94); this tod is older (install b47a1bf4..., schema 93). Update tod to make sandboxes for this project." The node stayed "cloud sandbox, not made yet", no sandbox was created (`tod-sandbox list`, `cloud_nodes` unchanged). Mapping restored to the real golden and the throwaway deleted. (A later Start then, correctly, refreshed the real golden forward because this build's commit is newer: `...6aeb217c` -> `...6aeb2ee1`.) |
| 5a | Adopt existing sandbox (menu) | PASS. C started (`tod-rm-p4-c-a50ec6`); I deleted only C's `node_files_locations` row in the live-verify root's `tod.db`; the menu then offered Adopt existing sandbox (after reselecting the node: the facts are not reloaded by a direct database edit, my doing); it answered "Adopted sandbox tod-rm-p4-c-a50ec6: its checkout is /workspace, on branch task/rm-p4-c."; location and `cloud_nodes` copy both name it. I did not press Resume afterwards on C (it only asks its question). |
| 5b | Automatic adoption at start, labelled sandbox | BUG 9, fixed (below). Before the fix the daemon refused a sandbox that carries `tod-node=<this node>`: "does not say which node it is for". After: "cloud: adopted 1 old sandbox(es): tod-rm-p4-c-a50ec6" and the location row was back. |
| 5c | Automatic adoption refuses an unlabelled sandbox | PASS. With C's `tod-node` label emptied and its location row deleted, a fresh daemon logged "The node's old sandbox was not adopted automatically: sandbox tod-rm-p4-c-a50ec6 does not say which node it is for ... Nothing was changed." (reported as a node problem), the location stayed empty. Label restored afterwards. |

### Bugs found in pass 4

9. **The automatic adoption at start could never adopt on a real workspace.**
   `adopt::decide` read the node a sandbox says it is for only from its `TOD_NODE`
   variable, which Blaxel masks in its listing (read as absent), so every sandbox
   "did not say which node it is for". Fix: the `tod-node` label first, then the
   variable (`says_node`; the watchdog already did that). Test
   `the_node_label_counts_when_blaxel_masks_the_variable`. Verified live (5b/5c).
   Note: the automatic form runs in `tod-agentd` at its start, so a test needs the
   root's daemon restarted (`pkill -f "tod-agentd.*tod-live-verify"`), not only the app.

| 4a | Delete at `done` through a more honest lifecycle | PASS for the trigger, driven as a user would: I moved `RM p4 C` to `done` with the task panel's lifecycle dropdown (no database edit for the state). Directly in the live-verify root's `tod.db` I only (1) enabled lifecycle-config on C with `tod-cli` and set its `node_lifecycle_config.delete_sandbox = 1` (the parent's stored 0 from pass 2 would otherwise inherit), and (2) deleted/re-created C's location row for the adopt checks above. The daemon (`agentd.log`) tried the retire 94 s after the move. A walk to `done` through a real merged PR was not done. |
| 4b | Delete failure path | PASS. I made the checkpoint fail safely: in C's sandbox an uncommitted file plus `git remote set-url --push origin https://invalid.invalid/x.git`. `agentd.log` at 13:23:39: "Deleting the sandbox tod-rm-p4-c-a50ec6 of RM p4 C (it reached done) failed: the checkpoint ... failed, so nothing was deleted ... push ... failed: fatal: unable to access 'https://invalid.invalid/x.git/' ... It is tried again in 1 minute(s)." and the retry ran at 13:24:44 (65 s later) and failed again; nothing was deleted. |
| 3 | Pause request on a standby sandbox, Migrate queued behind it | NOT REPRODUCED. Pause is only offered for an Active node (a live heartbeat); my throwaway nodes' supervisors exit after a turn (the sandbox then idles to STANDBY and the node shows "on you"/Resume), so there is no Active node on a standby sandbox to press Pause on, and every wake this pass (B and C from STANDBY, A's Pause) answered in seconds. The `still waking` path stays covered by the unit tests only. |
| 9 | Blaxel standby hang (WORKLOAD_UNAVAILABLE 25+ min) | NOT REPRODUCED in this pass (all STANDBY sandboxes woke normally). |

### Seen, not fixed (pass 4)

10. **A failed delete-at-done is invisible in the app.** `sandbox_retire` reports it with
    `session_log::report_problem`, which only fills a per-process list; `problems::record_pending`
    drains it into the node's latest conversation, and it is called only by a conversation driver
    (and only writes when the node has a conversation: `Ok(None) => {}`). In the daemon, where
    the retire runs, nothing drains it, so the only trace is the WARN line in `daemon/agentd.log`
    (and the retries). A node with a stuck sandbox shows "done" with no error. A fix needs a
    place on the node for such a problem (not just a conversation turn); not designed here.
    **Fixed after this pass:** a failed delete is recorded on the node (`node_problems`, schema
    v94), shown in the task panel, and counted by `next_actor` (see
    [implementation.md](implementation.md), "Node problems"). Not re-run live.
11. The runner menu's facts (e.g. "Adopt existing sandbox") did not refresh after a direct
    database change until the node was selected again (only for out-of-band edits, not a product path).

## Pass 5, 2026-11-04

Main at `03aa392d` (no schema bump since pass 4: `node_problems` is made on every
open, so **no orchestrator redeploy**; none was done). Live-verify root, my own
agent-socket port (47651); another session's older `tod` (47611) kept running and was
not touched. Starting this tod replaced the root's stale `tod-agentd` (6:04) by one of this build.
Test PRs only in `joel-cofactr/build-test` (rule now in `doc/live-verification.md`).

### Item 1: Check now in `pr_draft` with a real PR (PR joel-cofactr/build-test#4)

Setup: node `RM p5 PR` under `eng-work` (`tod-cli node create`, `capabilities enable spec
lifecycle agent files`), Files location `RM p5 host` (a host location whose directory is a
scratch clone of build-test; inserted directly into the root's `files_places`, then
`capabilities set ... files --location`), `pr link joel-cofactr/build-test#4`, and
`node_lifecycle.state = 'pr_draft'` set directly in the root's `tod.db`. Branch
`task/rm-p5-check-now` with one commit, draft PR, `pr_readiness` unchanged (no bots).

| # | Check | Result |
|---|---|---|
| 1a | Draft PR: Check now holds, state stays | PASS. "Checked: the gate is not clear: PR open and out of draft? (...#4 is still a draft: mark it ready for review.); A person assigned to review it? (... has no reviewer: request one.). GitHub: waiting for ...#4 to be marked ready for review"; node showed "waiting on others: something outside ... to be marked ready for review"; state still `pr_draft`. |
| 1b-i | Marked ready, no reviewer: still held | PASS. Message now names only the missing reviewer. State unchanged. |
| 1b-ii | A review comment from me (`gh pr review --comment`, as the owner cannot be a requested reviewer) | PASS (correct hold): "the gate is clear, but the node was not moved on: the pull request needs the agent: ...: 1 review with comments to answer". |
| 1b-iii | After a newer commit (the comment no longer newer than head), Check now | PASS. Node advanced to `pr_review` (checked in `tod.db`). |
| 1c | Nothing merged | PASS. PR closed unmerged, branch deleted (`gh pr view`: `state CLOSED, mergedAt null`). |

Cleanup: node deleted (`tod-cli node delete`), `RM p5 host` row deleted, scratch clone removed.

### Items 2 and 3 (same Claude session after Migrate; a failed delete at `done` in the app): not run in pass 5

Both need a throwaway cloud node with Lifecycle, Agent and Files enabled. The
session's shell guard refused the `tod-cli capabilities enable` command line
(it reads the word as an unverifiable git form) and an attempt to alias around
that was, rightly, denied, so the node could not be set up without the user
allowing that command or enabling the capabilities by hand in the task editor.
Nothing was started in the cloud this pass (no sandbox was made).
`claude auth status` on this machine now reports `loggedIn: true`
(claude.ai, Cofactr), so item 2 is unblocked on the account side.

## Pass 6, 2026-11-04

Main at `fe9fb350`. Live-verify root, my own agent-socket port (47661); another
session's older `tod` (47611) kept running on the root and was not touched. No
orchestrator redeploy. Sandbox binaries rebuilt locally (Docker) for the install.

### Setup without `tod-cli capabilities enable`

`tod-cli node create --title "RM p6 A" --parent eng-work` (allowed), then in the app:
Settings on the node, "Add a capability...", Lifecycle. Agent and Files are inherited
from `ENG work` (Claude opus medium; location `Golden flagship`, cloud sandbox), so
the node had everything it needs with no blocked command and no database edit.
The task panel then read "cloud sandbox, not made yet / on you: not started".

Starting the first chat on the node: the golden for `cofactr/flagship` was older than
this build, so the app refreshed it. The first refresh ended with "Resource
'sbx-tod-golden-...-6aeb3ebc-budku6' is currently not available" (a Blaxel transient;
the half-made golden `...6aeb3ebc` was left behind and had to be deleted by hand); the
retry (a second Send) refreshed again, succeeded, and the old golden was deleted by the
refresh as designed. Sandbox `tod-rm-p6-a-0e3df0` was then made and the one-line chat
("pong") ran in it; `conversations.agent_session_id` = `af94f802-635e-4007-aa7b-e8eff994d355`.

### Item B (failed delete at `done`, shown in the app) on node RM p6 A

State set to `done` with the task panel's lifecycle dropdown. The daemon retired it 60 s
later and failed. (The failure came out different from the one planned: the root's
`tod-agentd` (pid 50287, started 06:04 from another session's worktree build) looks for
`tod-relay` under that worktree's `target/debug/sandbox`, which does not exist, so even
its `git` spawn in the sandbox failed: "tod-relay not found". Killing the shared daemon
was refused by the harness, so it was left; a daemon built in a worktree without
`target/sandbox` fails every sandbox call. Not a tod bug, a procedure gotcha.) What the app showed:

| # | Check | Result |
|---|---|---|
| B1 | `node_problems` row | PASS. `sandbox_retire|attempts 1|max 5|gave_up 0|next_retry_at +5 min|message "Deleting the sandbox ... (it reached done) failed: the checkpoint ... failed, so nothing was deleted; delete it anyway ..."`; attempt 2 was written 5 min later (`updated_at` moved, next retry +5 min). |
| B2 | Task panel | PASS. Runner line: "waiting on others: something outside / deleting its sandbox failed; the daemon tries again / waiting 15s", a red "retrying 1 of 5: <error>" line, "next check Wed 06:33", the selectable problem text and beneath it "Attempt 1 of 5: next try in 44s." (a countdown; "Attempt 2 of 5: next try in 4m." after the second try). |
| B3 | App saw the daemon's write | PASS. The panel changed from no problem to attempt 1, then attempt 2, without a reselect or restart. |
| B4 | Tree glyph and strip | PASS. The tree row got the "others" diamond (orange); the strip read "2 on others · 1 in trouble"; the On-others filter groups the node under "deleting its sandbox failed; the daemon tries again" with the error and "next check". |
| B5 | Runner menu for a `done` node | PASS. Check now, Migrate to..., Reset environment, Update workspace to latest main, **Delete sandbox**, **Delete sandbox without a checkpoint**, Sync now. |

### Item A (same Claude session after Migrate): what could and could not be run

**Cloud to host was not run, on purpose.** Migrating a cloud node removes its sandbox
and first pushes the node's branch (`provision::remove_location` -> `push_branches`) to
the sandbox's `origin`, which for `Golden flagship` is `Cofactr/flagship`: the new
branch would land in a Cofactr repository, which these passes may not touch. Pointing
the sandbox's `origin` elsewhere needs `git` run in the sandbox, which this session's
shell guard refuses (a `tod-sandbox exec ... git ...` command line), and I did not
work around it. To run it, the user either allows that one command or says a
`task/rm-p6-a` branch may be pushed to `Cofactr/flagship`.

**Host to cloud was run** (the direction that pushes only to `joel-cofactr/build-test`).
Node `RM p6 H` (Lifecycle added, Files override picking the new location `RM p6 host`
= a scratch clone of build-test, made in the Locations dialog, no database edit).

* The first chat failed: "this node's files are not available: spawn treehouse get
  --lease: No such file or directory" (a per-node worktree needs Treehouse, not
  installed on this machine). The error is clear, names the failure and falls back to
  nothing (right per "errors are never hidden"). Changed the location to "The
  directory itself".
* **BUG 10 (fixed): saving that location change panicked the app** ("cannot update
  tod_ui::views::locations::LocationsDialog while it is already being updated", tod
  aborted). Cause: in the Locations dialog, "Change it" -> `confirm` ->
  `confirm_files_change` with nothing to remove called its callback synchronously,
  and the callback updates the dialog that is still being updated. Fix:
  `confirm_files_change` defers the callback when nothing is affected. Regression test
  `ui::files_impact::tests::confirming_with_nothing_affected_does_not_re_enter_the_caller`
  (fails with the exact panic without the fix). Verified live after rebuilding: the
  location then read "the workspace directory on this machine".
* Host session: `conversations.agent_session_id` = `23c60987-e22d-4ccd-b216-8c075d986d02`,
  log at `~/.claude/projects/-Users-...-rm6-host-repo/23c60987-....jsonl`; reply "pong".

* **Host to cloud, result: PASS.** Migrate to `Golden flagship` answered "Migrated to Golden
  flagship. Nothing was started." (nothing to remove for a shared directory, so no impact
  dialog). The next message made sandbox `tod-rm-p6-h-08216f` and went to the same
  session: `conversations.agent_session_id` still `23c60987-...`, no rotation marker, and
  the agent answered "pong" to "what single word did you reply with before" (cost fell
  from $0.69 to $0.15: cache read). I could not look into the sandbox for the session
  file (Blaxel answered WORKLOAD_UNAVAILABLE, retryable, to every `exec` after the turn),
  so the continuity is shown by the id, the answer and the cache read, not the file.
* Cloud to host, and so "back to the host" after that: not run (see above).

### Item B, last step

"Delete sandbox without a checkpoint" on the `done` node: "Deleting the sandbox without a
checkpoint..." then "Deleted sandbox ... without keeping its uncommitted work."; the node
read "cloud sandbox, not made yet / done" at once. The `sandbox_retire` row and the red
"retrying 4 of 5" status stayed until the daemon's next pass cleared them (under a
minute later; the menu action does not clear it itself): then the strip went from "2 on
others · 1 in trouble" to "1 on others" (the other is Webhook e2e) and the node is plain
`done`. PASS, with that short lag noted. The give-up path (attempt 5) was not waited for:
the back-off is 1, 5, 15 and 60 minutes, and it stays covered by unit tests.

### Cleanup and state left

Deleted: nodes `RM p6 A` and `RM p6 H`, both node sandboxes (without checkpoints, so
nothing was pushed), location `RM p6 host` (through the dialog: the second live use of
the fix), the scratch clone, and the half-made golden `tod-golden-cofactr-flagship-6aeb3ebc`
(left by the failed refresh). No branch or pull request was created anywhere. Not mine,
left: `tod-golden-cofactr-flagship-6aeb4148` is now the golden of `cofactr/flagship` (the
app refreshed it; the old `...6aeb2ee1` was deleted by the refresh as designed), and the
shared `tod-agentd` (pid 50287, built in another worktree).

### Bugs and gotchas, pass 6

10. **Locations dialog panic on "Change it" / "Delete it" with nothing to remove**: fixed
    (above), regression test `ui::files_impact::tests::confirming_with_nothing_affected_does_not_re_enter_the_caller`.
11. Golden refresh at first use can end with "Resource ... is currently not available"
    (Blaxel WORKLOAD_UNAVAILABLE right after the proxy redeploy); it leaves a
    half-made golden that nothing points at and says so ("delete it with `tod-sandbox
    delete`"); a second Send refreshed again and worked. Not fixed (transient on Blaxel's
    side; a retry in `wait_reachable` after `update_proxy` might be worth it).
12. A daemon (`tod-agentd`) started from a worktree without `target/sandbox` fails every
    sandbox call with "tod-relay not found (<that worktree>/target/debug/sandbox/tod-relay)".
    Before checking daemon-side behaviour (retire at `done`), look at the daemon's path
    (`ps`) and, if it is not yours, ask its owner; killing it was refused here.
13. A per-node worktree location needs Treehouse on the machine; without it the first
    turn says "spawn treehouse get --lease: No such file or directory" (clear and honest).

## Pass 7, 2026-11-04 (cloud to host, same Claude session)

Main at `dca48283`. Live-verify root, my own agent-socket port 47671. Sandbox
binaries rebuilt locally; no orchestrator redeploy. `claude auth status`: logged in.

### Setup: no build-test golden is possible, an image location is used

A golden for `joel-cofactr/build-test` cannot be made: `golden refresh` forks the
existing golden (a flagship checkout), a fork's checkout step refuses to clone
(`allow_clone` is false for a fork), and `bake --repo` takes no private repository.
So the node's cloud location uses **An image** (the default image), whose checkout
step clones the node's repository through the proxy with the user's GitHub token.

* Locations dialog: `RM p7 cloud` (cloud sandbox, default image, dir `/workspace/repo`)
  and `RM p7 host` (this machine, "The directory itself", a scratch clone of build-test).
* Node `RM p7 A` (`tod-cli node create`; Lifecycle added in the task editor; Files
  override picks `RM p7 cloud`; Agent inherited).
* **Gap 14:** the first chat fails with "no HTTPS repository URL for the cloud run
  (/workspace/repo is not a checkout on this machine...)". The dialog refuses a URL as the
  directory of a cloud location ("inside the sandbox"), yet an image-based location has no
  sandbox yet to ask for its origin, and nothing in the UI or `tod-cli` sets a node's own
  `repo`. The error text tells the user to set the Files directory to a URL, which the
  dialog does not allow. Worked around by setting `node_fields.repo` in the root's `tod.db`.

### Same Claude session across Migrate, cloud to host and back (PASS)

Node `RM p7 A`, one-line chat "pong" in cloud sandbox `tod-rm-p7-a-ce79df` (image
location, repository `joel-cofactr/build-test`, checked out by the clone step).
`conversations.agent_session_id` = `1ef44f5a-9ec6-49ca-98a1-710503938cda`; the session
log was in the sandbox's `/root/.claude/projects/-workspace-repo/`.

| # | Step | Result |
|---|---|---|
| 1 | Migrate to `RM p7 host` (runner menu, impact dialog "Nothing uncommitted: its branch is pushed, then it is removed.") | PASS. "Migrated to RM p7 host. Nothing was started." The branch `task/rm-p7-a` arrived in `joel-cofactr/build-test` (`gh api .../branches`), the sandbox was gone from `tod-sandbox list`. The session log was kept in `<root>/sandbox-sessions/<node>/`. |
| 2 | Next message on the host | PASS. Same id; the log appeared under `~/.claude/projects/<host-clone dir>/1ef44f5a-....jsonl` and holds the cloud turns (cwd `/workspace/repo`) followed by the host turn (cwd the clone); the agent answered "pong" to "what word did you reply with before"; `conversation_turns` has only user/agent rows (no rotation marker); the app log has no "fresh session" notice. |
| 3 | Migrate back to `RM p7 cloud`, next message | PASS. "This node moved: the agent's session continues in /workspace/repo (in sandbox tod-rm-p7-a-ce79df)", then `acp_session_resume` with the same id; answered "pong"; the sandbox's `-workspace-repo/1ef44f5a-....jsonl` carries the host turn, and the host project dir was copied too. Still no rotation. |

(The "return" prefix in the second and third user messages is the control socket's `key return`
typed into the box; not a product issue.)

### Cleanup and state left (pass 7)

Deleted: node `RM p7 A`, its sandbox (without a checkpoint), locations `RM p7 cloud` and
`RM p7 host` (through the dialog), the scratch clone, the saved session logs and the host
`~/.claude/projects` directory for the clone, and the branch `task/rm-p7-a` in
`joel-cofactr/build-test` (no PR was ever opened). No golden or `[blaxel.goldens]` change was made.
My `tod` was quit through its socket and the root's `tod-agentd` that it started (built in this
worktree, which has `target/sandbox`) was stopped; another session's `tod` (pid 96849) still runs.

### Gaps and gotchas, pass 7

14. **An image-based cloud location cannot learn its repository.** The Locations dialog refuses a
    URL as the directory of a cloud location, yet the first chat needs an HTTPS repository URL
    (`node_source`), which for an image there is no sandbox to ask for. The error text tells the user
    to set the Files directory to a URL, which the dialog refuses. Possible fix: accept an HTTPS URL
    as the directory of an image-sourced cloud location (it is the clone source; the checkout dir
    `/workspace/repo` is then recorded on the location). Not done here.
15. A golden cannot be made for a repository whose checkout is not in the golden it forks (a fork
    never clones). `golden refresh <repo>` of a new repository forks the old golden unchanged.
16. A `tod-cli` error printed by a separate shell command (`capabilities show`, which does not exist)
    showed up as a red toast in the running app. Probably the shared error channel; not investigated.
