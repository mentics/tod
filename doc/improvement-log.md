# Improvement log

What went wrong while driving real nodes through the lifecycle, how often, and
what was done about it. One entry per kind of problem; bump its count and add
the node when it happens again. Newest kinds first. The point is to see which
problems recur and fix those at the root (app, prompts, context), not one node
at a time.

Fields: **Seen** (count, nodes, dates) · **Symptom** (what the user saw) ·
**Cause** · **Fix** (commit, or "open") · **Measure** (what statistic would
have shown it sooner).

## Statistics worth collecting

Ideas, not yet built. Each would come from data tod already has (conversations,
turns, findings, run_state, node_timeline, journey records):

- Per node and phase: agent sessions, turns, tool calls, tokens, wall time.
- Review loop: rounds per stay, findings per round, how many rounds came back
  clean, how many findings were reopened (fixer said fixed, review found it
  again), rejected/declined ratio.
- PR loop (pr_draft): bot review rounds, bot findings per round, threads
  answered vs. code changed, CI failures per push.
- Requests to the user: how many, how long they waited, how many were
  answered "carry on" or with an option the agent offered (a sign the
  question was not needed), how many were about tod itself (broken setup)
  rather than the work.
- Stops (`run_state` stopped): count by `reason_kind`, and how many were
  resumed without any change by the user (a sign the stop was wrong).
- Sandbox churn: replacements per node, time spent setting up, losses.

---

## 2026-11-12: slow replacement

### Replacing a sandbox spends ten minutes copying session logs

- **Seen**: 1 node (ENG-2925, F05EF005), 3 session logs, about 10 minutes, 2026-11-12. The node did nothing meanwhile.
- **Symptom**: "sending session log n of 3" for minutes.
- **Cause**: `session_log::sh_write` sent a log in 48 KB base64 chunks, each its own `tod-sandbox exec` (a relay connection plus a `sh` per chunk): about 21 calls per MB, and the bytes uncompressed. A 50 MB log is about 1,000 calls.
- **Fix**: `SandboxRemote::write` makes the project directory with one command, then uploads the whole file gzipped through Blaxel's filesystem API (`upload_large_compressed`, 4 MB parts, unpacked and renamed in place), the path the supervisor binary already takes. JSON-lines logs compress about 10 to 1. That is one `mkdir` command, two free-space checks, one PUT per 4 MB of the gzip, and one unpack command per log (about 4 calls plus 1 per 4 MB of gzip, so ~0.25 per raw MB at 10 to 1, against ~21), and about a tenth of the bytes. Failures name the log. Total bytes and seconds are logged and shown when done. Dev containers keep the chunked path (local `docker exec`, cheap). Nothing runs differently inside the sandbox (no relay or supervisor change).
- **Measure**: seconds and bytes per replacement's session-log copy (now logged as "restored session logs").

## 2026-11-11: overnight stops

### PR babysitter counts housekeeping as rounds ("PR stuck")

- **Seen**: 3 nodes (ENG-2228, ENG-2927, ENG-3055), on the night of 2026-11-10/11. CI was green on all of them, and Greptile scored them 4/5 to 5/5.
- **Symptom**: the node stopped with `pr_stuck` after hitting `max_rounds` (4).
- **Cause**: rounds were every PR agent turn since the run started. Merging main, resolving a conflict, and asking a bot to review a new head all used up the cap.
- **Fix**: b23d8034. Only a turn on feedback is a round (`Work::is_feedback`: threads, failing checks, low score, changes requested, review comments). The count resets when the PR is clear or waiting for a person.
- **Measure**: PR turns per stay, split by the kind of work.

### Review of a PR change counts clean reviews ("not converging")

- **Seen**: 4 nodes (ENG-3067 10 rounds, ENG-3095 10, ENG-3193 10, ENG-2923 7). ENG-3193's last review was clean.
- **Cause**: `pr_change_reviewed` counted every review since the change was made (`reviews_since(..).len()`). The fix of 2026-11-10 covered only the own-review path.
- **Fix**: b23d8034. Both paths now share `unconverged_rounds_since`, which counts only consecutive unclean reviews.
- **Measure**: a stop whose standing list is empty, or whose last review was clean.

### Another build refreshes the shared golden, which blocks Start

- **Seen**: 2026-11-11. All 7 stopped nodes are blocked.
- **Symptom**: "The golden sandbox for cofactr/workspace was refreshed by a newer tod (install cc0ea2cf…); this tod is older".
- **Cause**: another session's build, which is not on main, refreshed the one golden every root shares. Main's build then refuses to fork from it.
- **Fix**: open. The install stamp says which is newer, not which build is on main.
- **Measure**: golden refreshes by install stamp; Start refusals for this reason.

### Waiting for the user to mark ready misses a new conflict

- **Seen**: 1 node (ENG-3093, 2026-11-11). It waited on "flagship#6641 to be marked ready" while GitHub showed the PR as `CONFLICTING`.
- **Cause**: found 2026-11-12. Each scheduled look did re-read the PR, but a conflict on a draft was invisible: GitHub reports a draft's `mergeable_state` as `draft` whatever the branch is, and `Assessment::conflicted` read only `mergeable_state == dirty`, so `Work::Conflict` never fired while the draft waited to be marked ready. (Behind was caught: it comes from the compare count.) Whether ENG-3093 was also stopped cannot be proved now, its sandbox being pruned.
- **Fix**: `conflicted()` also reads `mergeable == false` (which only ever means a conflict); `mergeable` null stays "GitHub still computing", left to the run's own poll, never clean and never a conflict. Test: `autopilot::tests::a_draft_waiting_to_be_marked_ready_gets_work_when_it_falls_behind_or_conflicts`.
- **Measure**: nodes waiting for a person whose PR is not mergeable.

## 2026-11-10 — first pass over the 12 `pr_draft` nodes

Starting state (live root): 12 nodes in `pr_draft`; 8 stopped
`review_not_converging`, 1 stopped `pr_stuck`, 2 waiting on a request, 1
waiting for the PR to be marked ready. Every node's cloud sandbox was gone
from Blaxel.

### basedpyright check killed for memory in node sandboxes

- **Seen**: 7 nodes (ENG-2680, ENG-3055, ENG-2927, ENG-2923, ENG-3067,
  ENG-3193, ENG-3095), 2026-11-10/11, five of them as requests to the user; any node whose
  pr_draft or review checks include flagship's basedpyright.
- **Symptom**: the check fails with no output (exit 247 or nothing after
  `uv sync`); the agent files "give the sandbox more memory".
- **Cause**: a fork has its golden's 8 GB, and its root filesystem is in RAM
  (about 2.7 GB for the checkout, venvs and node_modules). The workspace's
  Blaxel boot starts the dev servers (UI vite about 1.5 GB, two flagship
  runservers and the KB API, about 1.4 GB). basedpyright reaches 2.5–3.2 GB
  RSS, and the check gives it an 8 GB heap cap, so the kernel kills it before
  V8 collects. The checks themselves run one at a time.
- **Fix**: the check's heap is 2 GB (`--max-old-space-size=2048`) in the
  pr_draft and review checks of "All tasks" (2026-11-11, by the user's
  choice; a check setting, not a commit). Measured in ENG-2680's sandbox with
  the dev servers running: 108 s, peak RSS 2.2 GB, 0 errors and the baseline
  unchanged. The five OOM requests were answered "fixed, try again". A bigger
  golden was not needed: Blaxel takes any memory size in MB (CPU follows at
  1 per 2 GB, about $0.041 per GB-hour active), but a fork cannot choose its
  own, so it would mean changing the golden.
- **Seen again**: ENG-2157 (2026-11-11), this time the ui `tsc` check, which runs with
  `--max-old-space-size=6144`. It was killed at 3.3 GB RSS, with about 3.4 GB free
  next to the dev servers. It is the same kind of problem and is not fixed yet.
  Measured: at 2048 MB tsc runs out of V8 heap after 46 s, and at 3072 MB the kernel kills it
  (available memory fell to 21 MB). So a smaller heap cannot fix it. It needs memory freed
  (the dev servers stopped while it runs) or a smaller tsc footprint.
- **Fix (tod, 2026-11-12)**: a check killed for memory (exit 137/SIGKILL, "Killed", V8 heap OOM,
  `MemoryError`) is now *broken*, not failed: it is not sent to the Fix agent, the run stops with
  a request to the user (lower the heap, scope or remove the check), and the output starts
  "Killed for memory (...)" with the machine's total and free MB when `/proc/meminfo` exists
  (`checks::run_command`; tests `a_sigkilled_check_is_killed_for_memory_not_a_finding`,
  `a_v8_heap_exhaustion_is_killed_for_memory`). Commit: see `git log -S"KILLED_FOR_MEMORY"`.
- **Measure**: check failures with no output, or exit 137/247, per check.

### Agent-made git worktrees hide the work

- **Seen**: 1 node (ENG-2927).
- **Symptom**: after Reset environment the checkout stayed on a detached HEAD;
  the PR head was in `/workspace/.worktrees/{flagship,kb}-eng-2927`.
- **Cause**: the workspace's CLAUDE.md tells agents to work in a worktree.
  tod reviews, checks and pushes the checkout it set up, and the worktree held
  the branch, so the checkout could not be put back on it.
- **Fix**: `workspace/codebase.md` (c3db91b2): work in the checkout you were
  started in; never make a worktree or switch branches.
- **Measure**: a check after each turn that the checkout is on the node's
  branch and `git worktree list` has one entry.

### Sandbox setup fails on concurrent forks; the kept sandbox cannot be recovered

- **Seen**: 3 nodes (ENG-3067, ENG-3193, ENG-2228), 2026-11-11 00:2x UTC.
- **Symptom**: "sandbox health check 'manifest' failed: ... agents=false,
  expected node ..."; Adopt existing sandbox then refused the kept sandbox.
- **Cause**: `fleet::sandbox::connect` ran `Sandboxes::ensure` on a half-made
  node sandbox and installed a plain manifest. A fork whose proxy update wiped
  tod's files was not reinstalled. A failed sandbox was kept for inspection
  with nothing in the app able to replace it.
- **Fix**: 240ab386 (merged 74292b77): `ensure` refuses node sandboxes that
  are not ready; `make_sandbox` reinstalls tod's files into a fork that lost
  them; Start deletes a kept failed sandbox of the same node and makes a fresh
  one (`NameDecision::ReplaceFailed`). Seen working live for ENG-3067 and
  ENG-3193.
- **Wording**: every health failure now says what is wrong, the likely cause
  and the next step in plain words, then a `Technical detail:` line with the
  check name and fields (`HealthFailure` Display, a3b0a37e; next steps made
  true per new/existing sandbox in the follow-up commit). No automatic retry
  added.
- **Measure**: setup failures per fork, by health check.

### Small UI problems seen

- Stale "the cloud record names sandbox … not adopted" notes stay on the task
  panel after Start has made a new sandbox.
- The task panel's upper scroll area is short: the runner buttons (Defer,
  Watch, Resume) are below its fold on a 1000 px window when a node has a long
  failure text.
- Stop texts stored before the rewording keep the old wording.
- `tod-sandbox exec ""` (a node with no sandbox) answers "not authorized
  (401)" instead of naming the missing sandbox.
- The task panel's layout shifts when "Reading GitHub…" clears, so a click
  aimed at a request can land on another control (here it opened Findings).
- Enter on a request that carries evidence (a finding link) opens the
  finding instead of the free-text answer field.
- "journey: prune failed: tod-agentd exited at start" in the log: the prune
  starts a second daemon while one is already running.

### Review "not converging" with nothing standing

- **Seen**: 8 nodes (ENG-2927 12 rounds, ENG-2157 10, ENG-2228 8, ENG-3095 8,
  ENG-3067 7, ENG-3193 7, ENG-2680 7, ENG-3093 5). ENG-2680 recorded one
  finding in total (fixed); ENG-2927 three (all fixed).
- **Symptom**: node stopped, "the code review ran N rounds of review and fix
  and the last review still found problems", with no finding listed.
- **Cause**: `Autopilot::own_review` stopped once
  `pr_draft_review::reviews_this_stay` reached 5. That counts every review in
  the stay, clean ones included, and a review is owed on every new PR head
  (merge of main, CI fix, answering a bot), so ordinary progress used up the
  rounds. The message claimed problems the stop list did not have.
- **Fix**: only consecutive unclean reviews count
  (`pr_draft_review::unconverged_rounds`); never stop with nothing standing.
- **Measure**: rounds per stay vs. findings per round; a stop whose standing
  list is empty.

### Replacement sandbox puts submodules on a stale branch

- **Seen**: 1 node (ENG-2923, 2026-11-10 16:09); latent for every replaced
  sandbox.
- **Symptom**: the reviewer asked the user to "fix the working directory": the
  flagship branch was at origin/main, the PR's commits missing.
- **Cause**: `prepare_sandbox_checkout` fetched only the superproject; the
  submodules (flagship, kb, ui) never learned `origin/<branch>`, so the branch
  was created at the pinned commit.
- **Fix**: each submodule fetches `origin/<branch>` before branching; a
  local branch behind it is fast-forwarded, a diverged one is an error
  (`worktree::sync_branch_with_origin`).
- **Measure**: a check after setup that each submodule's branch contains
  `origin/<branch>`.

### Node sandboxes vanish

- **Seen**: 11 sandboxes 2026-11-10 21:01–23:01 UTC; 32 earlier the same
  day (14:42 "forgot 32 sandboxes").
- **Symptom**: every waiting node's sandbox gone; each is rebuilt on next
  start (minutes each), losing anything not pushed.
- **Cause**: Cofactr's `Blaxel Prune` workflow (Cofactr/workspace,
  every 30 min) deletes any sandbox asleep over `SANDBOX_IDLE_TTL` (1h)
  unless labelled `keep-when-idle=true`; tod labelled node sandboxes
  `false` on purpose.
- **Fix** (2026-11-10): node sandboxes carry `keep-when-idle=true`; tod
  deletes them at `done` (`sandbox_retire`).
- **Reverted** (2026-11-11): only goldens and the orchestrator are kept;
  node sandboxes are `false` again, left to the prune and made again on the
  next start. Sandboxes made between the two keep `true` (relabeling resets
  a sandbox's container) until `sandbox_retire` deletes them.
- **Measure**: replacements per node per day.

### Agents in a sandbox cannot query the database

- **Seen**: 5+ requests to the user (remediate-stale-primary ×2,
  editing-a-linked-org-part, nightly-auto-dedup, retract-manufacturer), all
  `db-query: aws CLI not found`.
- **Cause**: the workspace repo tells agents to use its `db-query`, which
  needs the aws CLI and an SSO session; a sandbox has neither. tod's brokered
  `tod-cli db` was not in the phase agents' or evaluator's context.
- **Fix**: a `db-query` shim in every node sandbox routes `prod-platform` /
  `prod-kb` through `tod-cli db run` (read only, same limits); `cli/db` is in
  the phase and evaluate recipes.
- **Measure**: requests whose text contains a tool error, by error.

### Requests to the user are hard to read

- **Seen**: most requests (user's report).
- **Symptom**: dense run-on text, bare ids, unclear what to do, long options.
- **Fix**: "Wording a request" in `stance/autonomous-session.md` (imperative
  first line, background after, ids explained, short options with
  consequences, recommended first); app stop texts reworded.
- **Measure**: request length; answers that pick the first option.

### Bot review keeps finding new things (PR stuck)

- **Seen**: 1 node (ENG-3055, 4 rounds).
- **Symptom**: stopped `pr_stuck`; Greptile posted a new P1 on kb#2859 after
  the last push.
- **Cause**: real feedback; each fix round draws new bot findings.
- **Fix**: none needed in the app yet; rerun.
- **Measure**: bot findings per round; whether they shrink.

### Pruned sandbox stops a waiting node

- **Seen**: several live nodes (ENG-3093, ENG-2925).
- **Symptom**: a node waiting on GitHub (CI, a bot, a person) showed stopped,
  "its sandbox was deleted; Start makes a new one", until someone clicked Resume.
- **Cause**: the Blaxel Prune workflow deletes node sandboxes asleep over an
  hour (they are never kept awake on purpose). The orchestrator marks the node
  lost when a wake finds the sandbox gone and the app or daemon replaced it,
  but with no regard for whether the node was meant to run (a paused node got
  one too), and a replacement that failed was only logged and retried every
  ten minutes forever while the node read "a new one is made at the next sync".
- **Fix**: `cloud_sync::lost` replaces the sandbox lazily (only when a wake or
  feedback finds it gone) for a node whose run row is not `Stopped`/`Done`;
  a user-deleted sandbox is still left for Start. Three failed tries, ten
  minutes apart, record the node failed with the real error
  (`next_actor` shows that error instead of the loss). The status note says
  "pruned or deleted outside the app; made a new one". Commit: see `git log`
  for "Pruned sandbox".
- **Measure**: nodes showing "sandbox is gone/deleted" for longer than one sync;
  replacements per node; nodes failed by `could not be made after 3 tries`.

- **Follow-up (pruned sandbox, second pass)**: the first fix did not help the
  live nodes because `cloud-sync.json`'s `deleted_by_user` flag was set on 13
  non-done nodes, which kept them out of replacement. Cause: the flag was set by
  every `delete_with`, including the daemon's retirement of a done node's
  sandbox (`sandbox_retire`), and outlived the node's next run. Fix: only the
  user's explicit Delete sandbox (menu, delete anyway, stop and delete) sets
  it, bound to the sandbox it removed (`deleted_sandboxes`); retirement leaves
  none and a done node is never replaced. `lost::heal_deleted_flags` (every
  sync, in the app and the daemon) clears a flag whose row now names another
  sandbox, and a legacy flag (no sandbox name) on a node whose run row is
  starting/working/waiting. The replacement tries are persisted
  (`replace_tries`), kept until a sandbox is made so a failed record write is
  recorded again without another try, and the conversation driver's resend
  after a launch finds the sandbox gone shares the same bound
  (`allow_remake`).

- **Follow-up (pruned sandbox, third pass)**: after the flags healed, ten
  replacements went quiet for minutes after "forked and set up" and the lost
  mark stayed. The silence is the step after the fork: a replaced node's agent
  session logs (58 for one node) are copied in chunk by chunk through the relay,
  which logged nothing, so a slow copy looked like a hang. Fix: that copy says
  "sending session log n of N (KB)"; a replacement is done only when the lost
  mark is verified cleared in the database (`verify_recorded`, else an error),
  and says so ("replaced ... with ..."); a Start also resets the persisted
  replacement tries; the driver's remake counts a try only once it begins one
  (`can_remake` / `note_remake`); stale flags heal before the sync, which may
  fail; and the one-call submodule branching handles a submodule that already
  has the branch locally (every golden fork), moving it to origin's when
  behind, instead of declining on every fork. A5898BE2's row (sandbox
  `node-bom-upload-...`, lost since October, no Files location) is a legacy
  record no replacement is attempted for.
