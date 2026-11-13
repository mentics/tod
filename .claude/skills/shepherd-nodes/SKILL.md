---
name: shepherd-nodes
description: Watch the nodes the user names (by ticket, slug, or a parent node for its subtree) on the live data root as tod moves them through every phase, from proposed to done. Wherever one gets stuck, find out why, make tod handle that case itself (a bug fix or an enhancement, done by subagents and merged to main), redeploy and restart, and get the node moving again. Use when asked to shepherd, babysit or unstick the live nodes, or to make the lifecycle run without help.
user_invocable: true
---

## The goal

tod is meant to take a node from `proposed` all the way to `done` with no
help, and to stop only for what only a person can do.
Up to `pr_draft` it should end waiting for the user to click **Open** on a
clean PR and choose a reviewer. After that it should keep the PR clean through
human review, wait for the merge and the release, then do the post-release
work and the retrospective on its own. It does not always get there yet. Some nodes stop on bugs.
Others stop because the app or its prompts are not smart enough for the case:
an unclear request, an error with no way forward, a loop that gives up early.

This skill closes those gaps. Shepherding a node by hand is a symptom, not the
job. Each time a node needs you, ask what tod should have done itself, and
change tod (code, prompts, context, error messages) so the next node never
needs that help. A fix can be a bug fix or an enhancement: a feature, a
fallback, a clearer message, or a smarter loop. Then get the node moving again
with the improved tod.

## Which nodes

The user names the nodes in the skill's argument. Shepherd those and nothing
else; other nodes with a lifecycle may be run by hand or left alone on purpose.

- Each item may be a Linear ticket (`ENG-2925`), a node slug, or a node UUID.
- A parent node (such as a project or `ENG work`) means every node under it
  that has Lifecycle.
- With no argument, don't guess: list the nodes with Lifecycle that are not
  `done` (ticket, title, state, run state) and ask which to shepherd.
- Resolve the items with `scripts/survey.py` (below) before starting, and say
  which nodes are in scope. An item that matches nothing is an error to
  report, not something to skip.
- A node in scope that was never started, or sits in `proposed`, is yours to
  start. Starting it is part of the job, not a step to ask about.
- The scope holds for the whole session. Add a node only when the user names
  it. A node outside the scope can still show a tod gap: note it in the
  improvement log, but don't resume, answer or reset it.

## Hard rules

- Use the **default live root**: the `data_root` in `install.toml`. Read it,
  never write it. Pass that root as `--data-root` on every launch.
- Read its database read-only (`sqlite3 'file:<root>/tod.db?mode=ro'`). Change
  node state only through the app (control socket) or `tod-cli`.
- Only the nodes in scope, and never move one to `pr_review`. Never mark a PR
  ready or request a reviewer: that is the user's step.
- Never approve, merge or release a PR, and never move a node forward past a
  gate with the lifecycle chip or `tod-cli lifecycle set`. A person's review,
  the merge and the release are the only stops after `pr_draft`; everything
  else is tod's to do, and yours to make tod do.
- Moving a node **back** to an earlier state (`tod-cli lifecycle set <node>
  <state> --reason …`), rerunning a phase, resuming, and resetting or
  deleting its environment are your calls. Don't ask the user for them.
- Never push to, comment on, or edit a PR or branch yourself. The
  node's agent does that, as the product should. Reading with `gh` is fine.
- Never label a node sandbox `keep-when-idle=true`. Letting idle node
  sandboxes be pruned is deliberate; a pruned node gets a new sandbox when it
  is resumed.
- A change to another repository goes on a branch
  and waits for the user's OK to push and open a PR.

## Fixing tod: subagents in worktrees

Hand each fix to a subagent (Sonnet, medium effort) with
`isolation: "worktree"`. Several can run in parallel, one per independent
problem. Each brief gives:
- the symptom, with the node, ids and evidence;
- what you have already ruled out;
- every code path that does the same thing (every caller of the delete, every
  push site, every way the state or the request can change): the fixer lists
  them and puts the guard at the lowest point they share, so a verifier
  doesn't find them one round at a time;
- the bar: a regression test, scoped tests and `cargo check --workspace
  --all-targets` pass, and the improvement log is updated.

When done, the subagent rebases on `origin/main` and pushes straight to main
(`git push origin HEAD:main`; tod has no PRs). That needs no input from the
user.

This session does not verify fixes itself, and never reports a fixer's test
results as checked. When a subagent reports, confirm its commit is on
`origin/main` and start its verifier at once; don't hold it to batch with
another fix. The verifier is a Haiku subagent (medium effort) given the
symptom, not the fixer's conclusion. It checks that the diff addresses the
cause and covers every path, runs the regression test and the scoped tests on
`origin/main`, and, after the deploy, checks that the node got past the point
where it was stuck. If it disagrees, send the finding back to a fixer.

Deploy only commits whose verifier passed. If you must deploy one that has
none yet, say so in your report.

## Deploy and restart after every merged fix

A fix does nothing until the running pieces have it. Right before deploying,
save a survey (`survey.py … --save <state dir>/pre-deploy.json`). From a
checkout that is current with `origin/main`:

1. `scripts/build.sh --data-root <root>`
   - Builds the host binaries (dev-fast) and the Linux sandbox binaries, each
     only if stale.
   - Reports whether the orchestrator and the watchdog are out of date.
2. `scripts/deploy.sh` (no arguments; it uses the default root)
   - Deploys the shared orchestrator and the watchdog when they are stale.
   - Refreshes tod's golden for each repository.
   - The orchestrator is shared by every root and user, so say in your report
     when you redeployed it.
3. **Restart tod.** This is not optional: the running app keeps the old code.
   - Send `quit` on the control socket.
   - Launch: `SKIP_BUILD=1 scripts/run.sh --data-root <root> --no-focus --width 1800 --height 1000 --agent-socket-port 47621`, in the background.
4. **The daemon** (`tod-agentd`) is a separate process and survives the app.
   - On launch, the app asks a daemon from an older build to drain and restart
     with the new copy.
   - It leaves a daemon from a newer build alone and says so. That means
     another build (another session) owns it: ask the user, and don't kill it.
   - Check `<root>/daemon/` and `ps` to confirm the daemon runs the new build.
     A daemon-only change still needs this restart.
5. **Node sandboxes** keep the binaries they were made with.
   - At launch the app reinstalls idle node sandboxes whose install is older.
   - A node with a live heartbeat is skipped. If the fix runs in its
     supervisor or relay, use the runner menu's **Reset environment** on that
     node.
   - A sandbox broken beyond that: **Delete sandbox**, then Resume.
6. **Goldens** come in two layers.
   - The repository's own golden (`tod-golden-…` forks it) is rebuilt from
     the workspace repo's `origin/main` by `scripts/refresh-workspace-golden.sh`
     (a scheduled launchd job, or by hand).
   - tod's golden is a fork of it with this build's files. It follows within
     10 minutes, or at once with `tod-sandbox --data-root <root> golden refresh <repo>`.
   - Node sandboxes are forked from tod's golden. A change to the workspace
     repo reaches a node only through a new golden and then a new node sandbox.
   - Start refuses a golden made by a newer build than its own. If another
     session's build refreshed it, rebuild from main and refresh it again.
7. **Check the deploy.** A few minutes after the restart, run
   `survey.py … --compare <state dir>/pre-deploy.json`. Every node it lists as
   worse is a regression: fix it before anything else.

## The loop

Every node in scope that is not `done`, in any phase:

1. **Survey** with the script, which reads the database read-only:

   ```bash
   python3 -I .claude/skills/shepherd-nodes/scripts/survey.py <root> <item>…
   ```

   One line per node: its state and how long, its run state (`phase`,
   `reason_kind`, `reason_text`), its pending requests and waits, and its
   sandbox. Decide each node's state from this, never from a glyph, a chip
   or a screenshot: a red triangle and an empty run line can hide a stop
   from days ago.
2. **Triage every pending request before anything reaches the user** (see
   "Requests to the user"). Most are tod's gaps, not the user's questions.
3. **For each node not making progress, find out why** before touching it:
   - its latest agent session;
   - the gate criteria that fail (task panel, Check now);
   - its findings and plan;
   - its PR on GitHub (`gh pr view … --json mergeable,mergeStateStatus,statusCheckRollup,reviewDecision`);
   - the sandbox's processes, memory and logs.
4. **Decide what tod should have done**, write it in the improvement log, and
   send a fixer. Typical gaps:
   - a stop that was a counting or state bug;
   - a phase agent or evaluator looping or giving up;
   - a gate that cannot be met;
   - a status that says something untrue;
   - an error with no next step;
   - setup that fails;
   - a request that only asks the user because tod lacked a capability;
   - after `pr_draft`: review feedback the babysitter does not take up, an
     approved PR left behind its base, a merge or release that tod did not
     notice (wait never satisfied, webhook missed), a post-release check or
     retrospective that fails or loops.
5. **Get the node moving** with the improved tod: Resume, "Resume with a fresh
   budget", Check now, Rerun this phase, Run with a new session, Reset
   environment, or a move back, whichever the cause calls for. Then run the
   survey for that node: an action counts only once the database shows it took
   (the state moved, the request is no longer pending, the run is working).
6. **A node is parked** (nothing for you to do until a person acts) only in
   these cases, and only while every condition holds:
   - `pr_draft`, waiting for the user to mark the PR ready and pick a
     reviewer: checks green, the review bot's verdict current for the head, no
     thread open, the PR mergeable, tod's own review clean.
   - `pr_review`, waiting only for a person's review: the same conditions. A
     person's comment or "changes requested" is work for tod's babysitter,
     not a stop.
   - `approved`, waiting only for a person to merge: the PR still clean and
     mergeable (a base that moved on or a new conflict is tod's to fix).
   - `merged`, waiting only for a release whose notes name the PR.
   - A request that only the user can answer (see below).

   Anything else is progress to be made. In `merged` (after the release),
   `released` and `learn` the phase agent does the work and the gate checks
   it; a stop there is a gap like any other.
7. **A node is finished** when it reaches `done` (and its sandbox is retired).
8. **Repeat until every node is finished.** Parked nodes still need a look on
   each pass: a person may have acted (reviewed, merged, released), and tod
   must then move on by itself. If it does not, that is a gap.

   Never end your turn while a node in scope is not finished without
   something scheduled to bring you back: a subagent you are waiting on, or
   `ScheduleWakeup` (about 10 minutes while nodes work or wait on CI or a
   bot, about an hour while every node is parked). Don't poll tightly.

Keep a state file at `.local/agent/scratchpad/shepherd/state.md` and update it
at each step: the scope, each node's last state and what you did, fixers and
verifiers in flight, commits not yet deployed, requests answered. Read it
first after a compaction.

## Requests to the user

A node waiting on the user is a case to triage, not just to relay.

These are tod's bugs, not questions, whatever the request says:
- an obligation that can only hold after rollout or a production run, asked
  in an earlier phase: its `acts_in` is wrong;
- "unreachable", "not connected", "not found", or a missing tool, while tod is
  running: the plumbing failed.

For each request, decide:

- **Can this session resolve it?** That covers access, connections, network
  and credentials plumbing, configuration, a missing tool in the sandbox, or a
  broken setup. Resolve it, and treat the request itself as a gap: tod should
  have handled it, or never needed it. Send a fixer, then answer the request
  ("Fixed, try again") so the node continues.
- **Is the request unclear, or does it ask for something tod could have
  decided?** Requests must follow "Wording a request" in
  `crates/tod/media/context/stance/autonomous-session.md`. Fix the prompt or
  context that produced the request.
- **Only the user can answer** (a product decision, an obligation that is
  wrong, a priority): make a brief with the `node-request-brief` skill and give
  the user its link.

## Driving the app

The control socket is `127.0.0.1:47621`. Commands: `click x y`, `shot2 <png>`,
`text`, `key enter`, `key ctrl-enter`, `scroll x y dx dy`, `sync`, `quit`.

- Wait about 1.5 s between clicks.
- Read every screenshot yourself.
- Take a new screenshot before clicking, because positions move.
- **Task panel:**
  - The top part shows the status line, "waiting on / working", and the
    runner split button.
  - That part scrolls: `scroll 1400 400 0 300` brings the runner buttons into
    view.
  - Lines saying "Reading GitHub…" shift the layout when they clear.
- **Runner menu:** the chevron is at about (1775, 280) for a node with one PR
  and (1775, 348) for a node with two. "Resume with a fresh budget" is about
  33 px below it.
- **To answer a request's free-text field:** click it, `key enter` to edit,
  `text …`, then `key ctrl-enter`.
- Confirm every GUI action in the database (the survey). When clicks keep
  landing in the wrong place, ask whether the user is using the app before
  debugging the layout.

## Reference

Tables to read (read-only):
- `run_state` (`phase`, `reason_kind`, `reason_text`, `updated_at`);
- `node_files_locations` (the sandbox name);
- `waits` (`match_spec`, `state`, `due_at`).

Logs:
- app log: `<root>/log/tod.log`;
- a sandbox: `tod-sandbox --data-root <root> exec <sandbox> -- sh -c '…'`.

Facts:
- **Node sandboxes are pruned on purpose.** A periodic
  workflow deletes node sandboxes that have been asleep for more than an
  hour. The status then says "its sandbox was deleted; Start makes a new one".
  Resume the node.
- **Sandbox memory:** 8 GB, no swap, and the root filesystem lives in RAM.
  The dev servers take most of it.
- **Checks** (per-phase commands) come only from the user's Lifecycle config,
  never from tod itself. A check that cannot work in a sandbox is the user's
  to change: tell them why.
- **Bot findings:** a PR whose bot posts real new findings each round is
  doing real work. It is not stuck.
- **After `pr_draft`** (`doc/lifecycle/pr-readiness.md`, `phase-agents.md`):
  `pr_review` is run by the babysitter until `pr-approved.*` passes and a
  person approves; `approved` waits on `github:pr:<n>:merged`; `merged` waits
  on `github:release:published` for a release naming the PR, then its phase
  agent certifies with evidence; `released` certifies a post-release smoke;
  `learn` records the retrospective (`learn-recorded`). Waits in `waits` are
  woken by webhook or the next scheduled look; a wait past its `due_at` with
  the event already on GitHub is a gap.

- **Production reads** are the node's agent's job (`tod-cli db` in its
  sandbox). This session's auto-mode classifier refuses them; don't retry.
- A duplicate subagent hand-back needs no reply.

Shell:
- In zsh, `$B:refs` inside double quotes is a modifier. Write `${B}:refs`.
- zsh's `[ a == b ]` fails with `= not found`. Use `=` or `[[ ]]`.
- macOS has no `timeout` command.
- Probe a sandbox once, in one command:
  `tod-sandbox --data-root <root> exec <sandbox> -- sh -c 'uptime; free -m; ps -eo pid,etime,rss,args --sort=-rss | head -15'`.

## Improvement log

`doc/improvement-log.md` gets one entry per kind of problem, with these
fields:
- **Seen**: count, nodes, dates;
- **Symptom**;
- **Cause**;
- **Fix**: the commit, or "open";
- **Measure**: the statistic that would have shown it sooner.

Bump the count when a kind comes back. Commit log updates to main as you go.

When you learn something this skill should have told you, don't edit this
file mid-run: add it under "Lessons from runs" in
`.claude/skills/review-shepherd-runs/reviews.md`, where the next review
picks it up.
At the end, report:
- what you fixed and deployed (the orchestrator included);
- each node's state;
- what is still open.
