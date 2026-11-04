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
