# Why the runner did nothing on PR 2888 from 16:08 to 18:37

Status: investigated read-only, 2026-11-19. Cause known; no code change (see
"Fix or recommendation").

## Which day

The window is **2026-11-19**, and the times are **UTC**, not the user's local
time (PST, UTC-8). It is 08:08 to 10:37 local.

- GitHub's timeline for Cofactr/kb#2888: `ready_for_review` and
  `review_requested` at **16:08:10-12Z** (joel-cofactr marked it ready), a
  Greptile review at 16:11Z, a human review by sunnyp06 with three comments at
  **17:06:50Z**, and the next commit from tod's agent at **18:40:43Z**.
- The node (`digikey-productdetails-404s-50-of-look-2`, id `a690a4b5-...`)
  next acts at **10:37:03 PST = 18:37:03Z**: its timeline shows the checks
  round, then the `pr_draft -> pr_review` gate check (10:37:10), then a Fix
  conversation at 10:38:28. The autopilot state file's `started_at_ms` is
  1795113483974, 18:38:03Z.

So between 16:08Z and 18:37Z, 2h29m, a person had marked the PR ready and a
reviewer had left comments, and the node did nothing.

## What the autopilot was doing

Nothing, because **no run existed for the node and it was waiting on nothing**.

1. **Its last wait was swept on 11-18.** From 11-13 18:48 the node sat in
   `pr_draft` with an event wait `github:pr:2888:ready_for_review`, renewed
   every 5 minutes (`waits` table: 61 + 279 + 279 + 105 + 133 + 64 rows). The
   last one was cancelled at **2026-11-18 05:21:15 PST** with the detail
   `lifecycle: the node's sandbox is lost`. That is `tod_store::waits::sweep`:
   a pending wait of a cloud node whose `cloud_nodes.lost_at` is set is
   cancelled, since nothing serves it. The sandbox was already lost: it was
   replaced three times on 11-16 and 11-17 (agentd.log, `cloud_sync::lost`,
   2026-11-16T16:55Z, 11-17T15:04Z, 11-17T18:48Z), each time with "6
   implementation step(s) were done ... a stub cannot be told from work". At
   2026-11-17T19:14Z `node_problems` warns that the cloud record names a
   sandbox its Files location does not record, and adopting it failed
   (`node_problems`, kind `adopt`: Blaxel no longer has it). No wait row
   exists for the node between 11-18 05:21 PST and 11-19 11:08 PST.

2. **The node then moved from cloud to a dev container, with no run started.**
   At **2026-11-19T00:28:39Z** (11-18 16:28 PST) the daemon logged `no longer
   runs in a sandbox; dropping its cloud record` for this node and about 20
   others in the same millisecond range: the user changed the workspace's
   Files location from cloud sandbox to the `workspace-dev-app-1` dev
   container. The node's own location in the container was made at 05:32Z
   (`node_files_locations.created_at` 1795066325145), when the user's chat
   conversation (`outline`, 11-18 21:31 to 11-19 07:32 PST) committed to the
   branch and made a worktree there. Nothing in this chain is a Start: the
   runner's desired state for the node was never set to run
   (`tod_agentd::runners::decide` returns `None` for `Desired::Unset`), so
   the daemon had nothing to start, and no wait to wake.

3. **GitHub events could not wake it.** `node_events` has no row for this
   node at all after 11-16, and the only thing that turns a webhook or the
   5-minute poll into a run is a pending wait or a live run. The ready flip at
   16:08Z and sunnyp06's review at 17:06Z landed on a node that had neither.

4. **Something did start it.** At 18:36:22Z the app provisioned the node's
   files (`tod.log`, `files for a690a4b5...`), the local runner started at
   18:38:03Z (agentd.log 18:38:28Z onward, then `autopilot notice ... a fresh
   session will be started`), and it began with the checks round and the
   review/fix loop on the human's feedback. That is a user pressing Start
   (the journey record would show it, but the run state row was overwritten
   by the later stop at 13:06 PST).

## Was the wait correct?

Each step followed its own rule, and the outcome was still wrong for this PR:

- Cancelling the `ready_for_review` wait when the sandbox is lost is correct:
  the supervisor that served it was gone.
- Dropping the cloud record when the Files location left the sandbox is
  correct.
- Not starting a run on a bare location change is what the design says
  ("Start" is the user's), and the node's actor was `You`: `NotStarted` or
  `Stopped` (`tod_core::next_actor::run_end`: "its sandbox is gone; a new one is
  made when the node next has work, or Start makes one now"). It was on the
  user's list (Alt+Q), not silent.

What was missing is that **nothing watched the pull request of a node that was
`You`/stopped**. Tod knew the PR (it is linked, and `pr_readiness` can read
it), so a PR going from draft to ready, or a human review landing, is a fact
that changes what the node needs. The node stayed a "not started" item with
the same wording while its PR had moved on. It is not a bug in the autopilot
loop, the wait, or the sweep, so there is no contained fix to make here.

## Recommendation

1. **Wake a stopped node when its PR changes.** The daemon's minute pass can
   read the linked PR of every `pr_draft`/`pr_review` node whose actor is
   `You` for "not running" reasons (not a request, not a user's own Stop or
   Pause), and, when `pr_readiness` finds work (feedback, a human review, a
   ready flip) newer than the node's last run, either start it, or at least
   raise the node's standing text to say that its PR has feedback and nothing
   is running. Starting it unasked is a policy choice: a user's explicit Stop
   must stay stopped, a lost sandbox should not be remade silently (cost).
   Needs a decision; not done here.
2. **A bulk Files location change should say what it leaves stopped.** Changing
   a location that about twenty nodes use dropped their cloud records in one
   pass and left each as `NotStarted` with no wait. Per
   `.claude/CLAUDE.md`, "confirm a significant change with its impact shown":
   the impact dialog (`ui::files_impact`) could list the nodes in a waiting
   state (a PR wait, a review wait) and offer to start them on the new
   place.
3. **The sweep's cancellation is silent on the node.** When `sweep` cancels
   the PR wait of a lost-sandbox node, a node problem or timeline note saying
   "its PR is no longer watched" would make the gap visible in Activity (the
   timeline has no entry between 11-17 10:51 and 11-19 10:37).

## Evidence and where it came from

All read-only (`sqlite3 'file:/Users/jshellman/.config/tod/tod.db?mode=ro'`,
`daemon/agentd.log`, `log/tod.log`, `gh api repos/Cofactr/kb/issues/2888/timeline`).

| When (PST) | What | Source |
|---|---|---|
| 11-13 18:48 | `ready_for_review` wait begins, polled every 5 min | `waits` |
| 11-16 08:55, 11-17 07:04, 11-17 10:48 | the node's sandbox replaced (lost, not pushed) | agentd.log `cloud_sync::lost`, `node_timeline` notes |
| 11-17 11:14 | cloud record names a sandbox the location does not | agentd.log `node_problems` |
| 11-17 11:46 to 11-18 04:10 | adopt attempts, all fail | agentd.log `cloud_sync::adopt`, `node_problems` kind `adopt` |
| 11-18 05:21 | last wait cancelled: sandbox lost | `waits.detail` |
| 11-18 16:28 | cloud record dropped (bulk, ~20 nodes) | agentd.log |
| 11-18 21:32 | node's dev-container location made, by a chat conversation | `node_files_locations`, `conversations` |
| 11-19 08:08 | PR marked ready, review requested | GitHub timeline (16:08Z) |
| 11-19 09:06 | sunnyp06 reviews with three comments | GitHub timeline (17:06Z) |
| 11-19 10:36-10:38 | files provisioned, run starts, checks run, gate check, Fix | tod.log, agentd.log, `node_timeline`, autopilot state |
