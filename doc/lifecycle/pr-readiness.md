# PR readiness and the PR babysitter

Status: implemented, with the differences listed under "As built". Extends
the `pr` state in [phase-agents.md](phase-agents.md).

## Problem

A node in `pr` has a linked pull request. Today its only gate check is
`pr-approved-mergeable`: GitHub's `mergeable_state == "clean"`. That says
nothing about review comments, and nothing acts on them. The user reads the
feedback, asks an agent to fix it, replies, resolves threads, and asks the
review bot to look again. This is slow and takes hours of wall-clock time,
because reviewers and bots answer late.

The goal: once a PR is open, tod babysits it without the user. It reads
feedback, fixes what needs fixing, replies, resolves threads, keeps the branch
current with its base, and gets bot reviews rerun, until the PR is **fully
mergeable except for the human review**. That review is the one thing it
cannot do; it waits for it (see the loop). It stops for the user only at a
blocker it cannot solve.

## The rule, applied

Gates are app checks; no agent evaluates one (see phase-agents.md). So:

- **Readiness is a list of criteria the app derives** from GitHub, each
  waivable like any gate criterion.
- **The babysitter is the `pr` phase agent.** It changes the world (code,
  replies, thread resolution, re-review requests) so the criteria become
  true. It never decides readiness.

## Criteria

Derived in `tod_core::gate::derived` and read from GitHub through the same
client `pr_mergeable_outcome` uses. All apply to **every linked PR**.

| Slug | Passes when |
|---|---|
| `pr-approved-mergeable` (existing) | GitHub `mergeable_state` is `clean`, or the PR is merged. |
| `pr-up-to-date` | The PR's head contains the base branch's tip (GitHub `mergeable_state` is not `behind`), with no conflict. |
| `pr-threads-resolved` | Every review thread, human or bot, is resolved or outdated. |
| `pr-review-current` | For each configured review bot, a review exists **for the current head commit** (so a push makes the old review stale). See Bots. |
| `pr-review-score` | For each configured bot with a `min_score`, its latest review for the current head has a score at or above it. |

Each fails with a detail line naming the thread, bot, or score, so the
side pane shows why and offers Waive. A waiver is per criterion and is
cleared when the PR head changes, as other certificates go stale.

### Bots

A **review bot** is a configured GitHub author whose PR comment carries a
score. First supported: Greptile.

- Author: `greptile-apps`.
- Score: the comment starts with `Confidence Score: N/5`, N an integer 1–5.
  The parser reads only the first line of the newest such comment; a comment
  without it is not a review.
- Ownership of the review: Greptile edits or re-posts its summary per run,
  so "for the current head" is decided by comment `updated_at` being after
  the head commit's push time and, where the comment names a commit,
  matching it. If GitHub does not let us tie it to the head, treat as not
  current.
- Threshold: `min_score = 4` for this project (4 or 5 passes).
- Re-review: a bot may need to be asked. Greptile is asked by commenting
  `@greptileai review this` on the PR, or `@greptileai review this draft`
  when the PR is a draft. The app does this after pushing, when
  `pr-review-current` fails because the bot has not reviewed the new head
  and has had `rerun_after` to do it on its own (default 10 minutes).
  Requests are not repeated for the same head more than once per
  `rerun_after`.

The bot's specifics (author, score pattern, re-review comment, thresholds)
are data on a `ReviewBot` definition in code (Greptile is the only built-in);
the project config below only selects and tunes bots.

## Project configuration

Per project, kept in the project's settings (no raw JSON in the UI; a
Settings section, autosaved). Nothing is required: with no configuration the
criteria are `pr-approved-mergeable` and `pr-threads-resolved`.

```
pr_readiness:
  bots:
    - name: greptile
      min_score: 4
      rerun_after_minutes: 10
```

Only selecting and tuning built-in bots is supported at first. Arbitrary
user-defined checks (required label absent, named check green, specific
approver) are a later extension; GitHub branch protection already covers
most of them through `mergeable_state`, so they are not needed to ship this.

## The babysitter

The `pr` phase agent (`ProtocolKind::Phase`, state `pr`). Its role doc's
**Done when**: every review thread is answered and resolved, every bot review
is current and at or above its threshold, and the PR is mergeable. It
certifies nothing: the gate derives all of this.

### Loop

The autopilot's `next_step` for a node in `pr` (after the PR exists):

1. Gate passes → advance to `approved`.
2. Unanswered decision → stop for the user.
3. **Actionable feedback exists** (open threads, a failed check, a bot score
   under threshold, the branch behind its base, or a conflict) → run a
   babysitter turn.
4. **Nothing actionable, but the gate waits on something outside** (a bot
   has not reviewed the current head, a required human review, checks still
   running) → **wait**. No agent turn is used while waiting.

After a push the sequence is: the turn's fixes are pushed and its replies
posted **at once** (nothing is held back for the re-review), the bot is asked
to review if it does not do so itself, and the node waits on `bot review`.
When the review lands, the loop starts again from step 1. That may repeat
several times; the limits are under "Guards against loops".

Waiting is the point of the design. It is not an agent sitting in a turn:

- The runner records a `WaitingOn` for the node (`bot review`, `checks`,
  `human review`), with the time it began, so the task panel says what the
  node waits on and for how long, and it counts as neither "blocked" nor
  "running" for Alt+Q.
- A **PR poller** in the app re-reads each waiting node's PRs on an interval
  (default 60 s while anything waits; 5 min after 1 h). It is off the UI
  thread, like `views::incoming_check`, and pushes a change event only when
  the PR's observed state changed (new comment, new review, check result,
  head moved), which wakes the runner. Views react to the event, not to the
  timer.
- The wait has no time limit while the node is otherwise healthy. It is
  shown, not treated as a failure. The user may pause the runner.

### A turn

The agent is given, by the app (a `Phase`-protocol dynamic block, never
fetched by the agent itself), the PR's:

- unresolved threads: id, file and line, every comment, diff hunk;
- top-level PR comments and reviews since its last turn, bot summaries
  included with their scores;
- failing checks with the tail of their logs;
- whether the branch is behind the base, and any conflicts.

**Keeping current with the base.** When the branch is behind, the agent merges
the base branch into it (a merge, never a rebase or force-push, so review
history stays intact), resolves conflicts, and leaves the recorded test run
green before the app pushes. This is ordinary babysitter work, not a stop.
Conflicts it cannot resolve confidently are `needs-user`.

**Scope.** Feedback is judged against the node's intended scope (its
obligations and plan), and the agent must not grow the PR to satisfy it. For
each piece of feedback it asks:

- *Does this change introduce the problem?* Then it is in scope: fix it. No
  exceptions.
- *Is the problem pre-existing and not something this node is meant to fix?*
  Then it is normally out of scope: answer `rejected`, say it predates the
  change, and, where it is worth keeping, record it as a new item outside
  this node (a task under the project, via `tod-cli node create`) so it is
  not lost.
- *Would fixing it enlarge what the node set out to do* (new behaviour, a
  refactor beyond what the change touches)? Out of scope, same handling.
- A legitimate concern (correctness, security, data loss) is never rejected
  for scope alone; if a pre-existing problem makes this change unsafe, fixing
  it is in scope.

The reply always gives the reason, so the reviewer can disagree. When the
agent cannot tell whether something is in scope, that is `needs-user`.

It records how it answered each thread through `tod-cli` (`pr threads`,
in the manner of `review` findings; `tod-cli` refuses a status only the
user may set):

- `fixed`: it changed code; the reply says what.
- `rejected`: it disagrees or the suggestion is out of scope; the reply says
  why. A comment that is only praise or a question already answered counts
  as `rejected` with a short reply.
- `needs-user`: it cannot decide (product intent, a demand that conflicts
  with an obligation, access it lacks). This raises a decision
  (`tod-cli decisions ask`) and stops the runner. This is the only outcome
  that stops for the user.

The app then acts, not the agent: it commits and pushes the agent's changes
(the same path Implement uses), posts each reply, resolves the threads it
answered, and requests bot re-reviews (the Greptile comment above). The
agent may fix and answer many threads in one turn; the app posts them
together after the push so replies can cite the pushed commit.

**Replies and resolving are autonomous.** This is what the user asked for and
it is outward-facing, so it is bounded:

- Replies are posted as the user's GitHub identity, by the same credentials
  the app already uses for PR reads. They carry no marker.
- Human reviewers' threads are handled exactly like a bot's. When the agent
  judges it has addressed a thread, or has a justification for not acting,
  it posts an appropriate reply and resolves the thread, autonomously. A
  reviewer who disagrees can reopen it, which counts as a new round.
- Code changes go through the node's ordinary review-state tests: a turn that
  changes code must leave the recorded test run green, as Fix does, or the
  fix is not pushed and the turn is reopened.
- Every posted reply, push, resolve, and re-review request is recorded as a
  `UserAction`-style journey event with what it did, so a wrong autonomous
  action is traceable.

### Guards against loops

- A thread answered `fixed` that the reviewer reopens or answers again is
  handled as new feedback. After **3 rounds on one thread** the agent
  must answer `needs-user`.
- A **round** overall is one cycle of: a review (bot or human) finds
  something, the babysitter fixes and pushes. After **4 rounds** on the PR
  the run stops for the user (`NeedsHuman::PrStuck`, saying what keeps
  coming back), since something unusual is going on. Both limits are
  settings (`pr_readiness.max_thread_rounds`, `max_rounds`); merging the base
  branch and fixing a failed check do not count as rounds.
- A bot score under threshold after a pass with no code change possible (the
  agent judges the remaining findings wrong) is `needs-user`, not a
  rerun: the user decides whether to waive `pr-review-score`.
- A turn that changes nothing, replies to nothing, and leaves the gate
  unchanged twice in a row stops the run (`NeedsHuman::PrStuck`), like
  `EvaluationStuck`.

## What is built where

- `tod-store`: PR thread, review, check and comment reads (GraphQL for
  threads: REST does not expose resolved state) with an observed-state
  digest for the poller; post reply, resolve thread, post comment; the
  per-thread answer table (`pr_thread_answers`, node-scoped, so with a
  `journey_changes` trigger).
- `tod-core`: the criteria above in `gate::derived`; `ReviewBot`; the
  babysitter protocol and its dynamic block; the `pr` step of the autopilot,
  `WaitingOn`, `NeedsHuman::PrStuck`.
- `tod-cli`: noun `pr threads` (list, answer), documented in
  `media/context/cli/`, with `doc_sync` coverage.
- `assets/process/`: the `pr` role doc's **Done when**; a
  `surface/` fragment and `ContextRecipe` for the babysitter.
- `tod-ui`: the poller feeding the runner (`unified/`), the wait line in the
  task panel runner line, failing criteria with Waive in the side pane, and
  the Settings section for bots.

## Build order

1. Read side: PR threads/reviews/comments client, `pr-threads-resolved`
   criterion, Greptile parser with tests on real comment text.
2. `pr-review-current` and `pr-review-score`; project settings for bots.
3. `tod-cli pr threads` and thread-answer storage.
4. Babysitter protocol with post/resolve/push by the app, against a fake
   GitHub client (`--agent mock` directive support included).
5. Poller, `WaitingOn`, autopilot wiring, task panel line.
6. Re-review requests and the loop guards.

## Decided

- Posting, resolving and requesting re-reviews use the same GitHub
  credentials the app uses to read the PR (the engineer's own).
- Human and bot threads are treated alike; the agent replies and resolves.
- Fixes are pushed and replies posted immediately; only the bot re-review is
  waited on, by polling.

## Open questions

- Whether the stored credentials' scopes allow posting and resolving; a
  check at the first post should fail with a clear message, not silently.

## As built

Where the code differs from the design above:

- **Answering a thread is one CLI call, not a stored answer.** `tod-cli pr
  threads answer` posts the reply, resolves the
  thread, and returns; there is no `pr_thread_answers` table and the app does
  not post after the push. The agent pushes first, as its instructions say.
  A thread it cannot decide is a decision (`tod-cli decisions ask`); there is
  no `needs-user` status.
- **Rounds are approximate.** Replies carry no marker, so rounds on a thread
  are estimated as its comments divided by two (`pr_readiness::rounds`); the
  aim is only to stop a loop that never settles. Rounds on
  the PR are the agent turns of its conversation since the run began counting
  (`AutopilotState::pr_turns_base`, reset by renewing the budget). Both limits
  are `pr_readiness.max_thread_rounds` / `max_rounds`.
- **A round that changes no code still gets a fresh review.** A bot reviews
  on a push, not on a reply, so when replies (declining a finding, say) have been
  posted since the bot's latest review of the current head, its review counts as
  out of date and the app asks for another at once (`Assessment::of`, from the
  PR's reviews and its thread comments), asking again for each later reply.
- **The pull request protocol keeps looping while there is work**
  (`conversation::pr`): after each turn it reads the PR and continues with
  the work, so the conversation view's Pr step also babysits. The autopilot's
  `babysit` (`autopilot/mod.rs`) wraps it: it opens the PR, reads it, runs
  the agent while there is work, asks an overdue bot to review, and polls
  GitHub while it waits. The poller is that loop; it runs on the run's own
  thread and sleeps in half-second slices so Pause is heard, and time asleep
  is not time worked. It does not push a store event; the task panel's runner
  line shows "waiting for greptile review" through `StepHook::waiting`.
- **The poll is fixed at 60 s, then 5 min after an hour**, not configurable.
- **Not built:** the Settings UI for bots (edit `pr_readiness` in `tod.yml`);
  `WaitingOn` in the task panel beyond the runner line; the `--agent mock`
  directive support for a fake babysat PR (autopilot tests use a fake
  `PrFeed`).
- The gate criteria are `pr-approved.up-to-date`, `.threads-resolved`,
  `.review-current`, `.review-score`, beside `.mergeable`. The two review
  criteria pass when no bot is configured. A waiver is the existing per-node
  waiver; it is not cleared when the head changes.
