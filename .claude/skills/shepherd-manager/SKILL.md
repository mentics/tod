---
name: shepherd-manager
description: Shepherd many live tod nodes at once (by ticket, slug, or a parent node for its subtree) from proposed to done. Runs CLI shepherd agents in parallel and one UI shepherd over the app, assigns them nodes from a ledger kept in the data root, collects the tod gaps they report, fixes each once with subagents merged to main, deploys in drained batches, and keeps going as nodes are added. Use when asked to shepherd, babysit or unstick the live nodes, or to make the lifecycle run without help.
user_invocable: true
---

## The goal

tod is meant to take a node from `proposed` all the way to `done` with no
help, and to stop only for what only a person can do. Up to `pr_draft` it
should end waiting for the user to click **Open** on a clean PR and choose a
reviewer. After that it should keep the PR clean through human review, wait
for the merge and the release, then do the post-release work and the
retrospective on its own. It does not always get there yet.

Shepherding closes those gaps. Each time a node needs help, the question is
what tod should have done itself; tod is changed (code, prompts, context,
error messages, the GUI) so the next node never needs that help. Then the
node moves on with the improved tod.

You are the manager. Shepherd agents work the nodes and report gaps; you
assign nodes, fix each gap once, deploy, and keep the slots busy without
churning them. You don't work nodes yourself.

## Pieces

- **Ledger** (`<root>/shepherd/ledger.db`), owned by the shepherds, not tod,
  and the one place for what is current: the nodes being shepherded (scope,
  verdict, fingerprint, lease) and the open gaps, i.e. the tod bugs and
  improvements not yet deployed. What is finished goes to `history.jsonl`
  beside it (`L history`), which nothing reads to decide what to do. See
  [the ledger](../shepherd-common/ledger.md). Below, `L` is
  `python3 -I .claude/skills/shepherd-common/scripts/ledger.py <root>`.
- **`cli-shepherd`** agents (several): work nodes through `tod-cli`
  ([shepherd-cli](../shepherd-cli/SKILL.md)).
- **`ui-shepherd`** agent (at most one): works nodes through the app and
  finds UI bugs ([shepherd-ui](../shepherd-ui/SKILL.md)).
- **Config**: [config.toml](config.toml); `key=value` arguments override it
  for the session.
- Shared: [hard rules](../shepherd-common/hard-rules.md),
  [triage](../shepherd-common/triage.md),
  [gap reports](../shepherd-common/gap-report.md),
  [reference](../shepherd-common/reference.md).

## Start

1. The root is the `data_root` in `install.toml` (read it, never write it).
2. Read the config and apply the arguments' overrides.
3. **Scope.** Items in the arguments (a ticket, slug, UUID, or a parent for
   every node under it with Lifecycle) go in with `L scope add <item>…`. An
   item that matches nothing is an error to report. Scope rules persist: with
   no items, use the ledger's scope; if that is empty too, list the Lifecycle
   nodes not `done` (ticket, title, state, run state) and ask which.
4. `L sync`: resolves scope (nodes created under a parent in scope since the
   last pass come in by themselves), computes fingerprints, and makes due
   every node whose fingerprint changed, whose recheck time passed, whose gap
   was deployed, or whose verdict came from an older shepherd version. A
   leftover ledger from an earlier run means most nodes need nothing: that is
   intended.
5. If `ui_shepherd` is on, make sure the app runs at `ui_window` with the
   socket on `ui_socket_port` (see "Deploy", step 3, for the launch line).
6. Say which nodes are in scope and how many are due.

A node in scope that was never started, or sits in `proposed`, is the
shepherds' to start.

## Dispatch

After every sync:

1. `L status` counts due nodes and leases; `L owners` lists running
   shepherds, how many nodes each holds, their visits and empty visits, and
   their signal and acknowledgement.
2. Running shepherds with room absorb due nodes themselves (their `wait`
   claims them). Start a new shepherd only for what they can't absorb, and
   only when at least `min_batch` such nodes are due or one has waited
   `max_queue_delay_minutes`, and fewer than `max_parallel` shepherds run.
3. To start one: pick an owner id (`cli-<n>`, or `ui` for the UI shepherd),
   claim its batch with `L next --owner <id> --n <nodes_per_shepherd>`
   (group by phase or repository when the queue allows, so one shepherd's
   context serves all its nodes), then launch it in the background:
   `Agent(subagent_type: "cli-shepherd" | "ui-shepherd", run_in_background: true, …)`.
   Pass `model`/`effort` only when the config differs from the agent's own
   defaults (Sonnet, medium). The brief gives: owner id, root, claimed nodes,
   `nodes_per_shepherd`, `linger_minutes`, `max_visits_per_shepherd`, and for
   the UI shepherd the socket port and window size.
4. The UI shepherd gets nodes where the GUI gets real use first: a pending
   request, a stop, a runner action to take.

**No churn.** A node is dispatched only when the ledger says it is due;
never hand a shepherd nodes "to have a look". If `L owners` shows a shepherd
with many empty visits, or shepherds retiring after a visit or two, the
settings are wrong for this load: raise `linger_minutes` or `min_batch`, and
note it for the next review.

When a shepherd returns:
- confirm it holds no nodes (`L owners`); release any it left with
  `L release <node>… --owner <id>` (an abandoned lease also expires on its
  own after 20 minutes);
- a node it marked `parked` on a request only the user can answer: make a
  brief with the `node-request-brief` skill and give the user its link
  (once per request);
- add what it says the skill should have told it to "Lessons from runs" in
  `.claude/skills/review-shepherd-runs/reviews.md`;
- sync and dispatch again.

## Gaps and fixes

Only you start fixers. `L gap list` shows each gap with every node and
shepherd that reported it.

- **One fixer per gap**, however many nodes reported it: a subagent (config
  `fixer`, Sonnet medium by default) with `isolation: "worktree"`, then
  `L gap set <id> --status fixing`. Several run in parallel, one per independent gap.
  The brief gives:
  - the symptom, with every node, ids and evidence from the gap's reports;
  - what the shepherds already ruled out;
  - every code path that does the same thing: the fixer lists them and puts
    the guard at the lowest point they share;
  - the bar: a regression test, scoped tests and `cargo check --workspace
    --all-targets` pass, and a commit message that says the symptom, the
    cause, the fix, and the statistic that would have shown it sooner (that
    statistic, if tod doesn't keep it, is a `measure` gap: report it).
  When done, the fixer rebases on `origin/main` and pushes straight to main
  (`git push origin HEAD:main`; tod has no PRs).
- A `cli-parity` gap's fix is a new verb in the shepherd tier of `tod-cli`
  (hidden from agents, refused in agent sessions) plus its line in
  `shepherd-common/operator-cli.md`. A `ui-target` gap's fix is at the
  lowest shared point when it can be (the default roles, labels and
  synthetic nodes in the patched gpui under `vendor/gpui-pre`, `TOD PATCH`
  markers), and otherwise gives that element a stable `.id()` and a label.
- You don't verify fixes yourself and never report a fixer's test results as
  checked. When a fixer reports, confirm its commit is on `origin/main`
  (`L gap set <id> --status fixing --commit <sha> --note "verifying"`) and start its verifier at once: a
  subagent (config `verifier`, Haiku medium by default) given the symptom,
  not the fixer's conclusion. It checks the diff addresses the cause and
  covers every path, and runs the regression test and scoped tests on
  `origin/main`. It passes (`L gap set <id> --status verified`) or goes back to a
  fixer.
- A change to another repository goes on a branch and waits for the user's
  OK to push and open a PR.
- A gap you learn of yourself (from a fixer, a deploy, the user) goes in with
  `L gap report --owner manager --signature … --summary …` (`--node` when
  one shows it). A new gap whose reply has `closed_before` means an earlier
  fix didn't hold: give its fixer that commit.

## Deploy, with the shepherds drained

A fix does nothing until the running pieces have it, and a deploy restarts
the app and the daemon under every shepherd. So deploy in batches: when
verified fixes wait and no other fixer is expected within about 15 minutes,
or the oldest verified fix has waited 30. Deploy only verified commits; if
you must deploy one without a pass, say so in your report.

1. **Drain**: `L signal pause --all`. Each shepherd stops at its next action
   boundary and acknowledges; wait until `L signal status` reports
   `all_paused` (`awaiting_pause` names who hasn't) (a shepherd that never acknowledges within ~10 minutes is dead:
   release its nodes and `L signal retire --owner <id>`).
2. Save a survey: `survey.py <root> <the items in L scope list> --save <state dir>/pre-deploy.json`.
3. From a checkout current with `origin/main`:
   1. `scripts/build.sh --data-root <root>`: builds the host binaries
      (dev-fast) and the Linux sandbox binaries, each only if stale, and
      reports whether the orchestrator and the watchdog are out of date.
   2. `scripts/deploy.sh` (no arguments; it uses the default root): deploys
      the shared orchestrator and the watchdog when stale and refreshes tod's
      golden for each repository. The orchestrator is shared by every root
      and user: say when you redeployed it.
   3. **Restart tod.** Send `quit` on the control socket, then launch
      `SKIP_BUILD=1 scripts/run.sh --data-root <root> --no-focus --width <w> --height <h> --agent-socket-port <port>`
      (from the config's `ui_window` and `ui_socket_port`) in the background.
   4. **The daemon** (`tod-agentd`) survives the app. On launch the app asks
      a daemon from an older build to drain and restart with the new copy.
      It leaves a daemon from a newer build alone and says so: another
      session owns it; ask the user, and don't kill it. Check `<root>/daemon/`
      and `ps` to confirm the daemon runs the new build.
   5. **Node sandboxes** keep the binaries they were made with. At launch the
      app reinstalls idle node sandboxes whose install is older; a node with
      a live heartbeat is skipped. If the fix runs in its supervisor or relay,
      the node needs **Reset environment**; a sandbox broken beyond that,
      **Delete sandbox** then Resume. Tell the shepherd holding it (via the
      gap's note) rather than doing it yourself.
   6. **Goldens**: the repository's own golden (`tod-golden-…`) is rebuilt
      from the workspace repo's `origin/main` by
      `scripts/refresh-workspace-golden.sh` (scheduled, or by hand). tod's
      golden forks it with this build's files and follows within 10 minutes,
      or at once with `tod-sandbox --data-root <root> golden refresh <repo>`.
      Start refuses a golden made by a newer build: if another session's
      build refreshed it, rebuild from main and refresh it again.
4. `L gap set <id> --status deployed` for each gap in the batch: its
   `blocked_on_gap` nodes come due, with the gap named, and running
   shepherds claim them from `wait`.
5. **Resume**: `L signal resume --all`.
6. **Check the deploy** a few minutes later:
   `survey.py <root> <the items in L scope list> --compare <state dir>/pre-deploy.json`.
   Every node it lists as worse is a regression: fix it before anything else.

Running shepherds carry on after a deploy; their context is still good, since
the change was in tod. **Retire and relaunch only when the shepherds
themselves changed** (a shepherd skill, a shared file, or an agent
definition): `L signal retire --all`, then sync (verdicts from the old
version become due) and dispatch fresh shepherds, each filled to a full batch
from due nodes.

## The user's controls

The user can change the work while it runs, from any terminal or session:
- `L scope add <item>` / `L scope remove <item>`, or create the node under a
  parent in scope in the app;
- `L hold <node>` (shepherds leave it alone; they're working it themselves)
  and `L unhold <node>`;
- `L clear --all | --node <node> | --verdict <v> | --reason-like <text>` to
  make nodes due again.

They may also answer requests and act in the app at any time. The UI
shepherd yields to them by itself, and every shepherd checks a node right
before acting on it, so nothing needs coordinating. Pick these changes up on
your next sync; mention them in your next report.

## Keep going

Never end your turn while a node in scope is not finished without something
to bring you back: a shepherd or fixer running in the background (its return
wakes you), or `ScheduleWakeup` set to the next due time from the ledger
(at least 5 minutes, at most an hour; an hour while every node is parked).
Don't poll tightly.

Keep `.local/agent/scratchpad/shepherd/state.md` for what the ledger doesn't
hold: fixer and verifier agent ids, commits not yet deployed, briefs given to
the user. Read it and `L owners` first after a compaction.

## Lessons for the skills

When you learn something these skills should have told you, don't edit them
mid-run (that would retire every shepherd): add it under "Lessons from runs"
in `.claude/skills/review-shepherd-runs/reviews.md`.

## Report

When the user asks, and at the end:
- what was fixed and deployed (the orchestrator included);
- each node's state and verdict (`L status`);
- open gaps and what is still open (`L gap list`), and gaps that came back
  (`recurrence`);
- churn: shepherds started, visits, empty visits (`L owners` for running
  ones, `L history --event owner_gone --since-hours <run length>` for the
  rest).
