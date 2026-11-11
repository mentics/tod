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

## 2026-11-10 — first pass over the 12 `pr_draft` nodes

Starting state (live root): 12 nodes in `pr_draft`; 8 stopped
`review_not_converging`, 1 stopped `pr_stuck`, 2 waiting on a request, 1
waiting for the PR to be marked ready. Every node's cloud sandbox was gone
from Blaxel.

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
- **Fix**: only consecutive unclean reviews count; never stop with nothing
  standing. (in progress)
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
- **Fix**: fetch the node's branch in each submodule before branching. (in
  progress)
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
- **Fix**: node sandboxes carry `keep-when-idle=true`; tod deletes them at
  `done` (`sandbox_retire`).
- **Measure**: replacements per node per day.

### Bot review keeps finding new things (PR stuck)

- **Seen**: 1 node (ENG-3055, 4 rounds).
- **Symptom**: stopped `pr_stuck`; Greptile posted a new P1 on kb#2859 after
  the last push.
- **Cause**: real feedback; each fix round draws new bot findings.
- **Fix**: none needed in the app yet; rerun.
- **Measure**: bot findings per round; whether they shrink.
