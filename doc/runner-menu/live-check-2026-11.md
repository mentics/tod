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
| Sandbox deleted automatically at `done` | NOT REACHED. A node cannot be cheaply walked to `done` (needs the full lifecycle, PR and merge). The code path is `delete_sandboxes_at_final_phase`, run only inside an orchestrator sync (B4 will add the daemon trigger); only its unit tests were relied on. |
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
