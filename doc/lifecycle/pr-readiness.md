# PR readiness and the PR babysitter

Status: implemented, with the differences listed under "As built". Extends
the `pr` state in [phase-agents.md](phase-agents.md).

**Three states.** `pr` opens the draft; the babysitter runs in the other two:

- In `pr` the PR agent opens the draft pull request (or, when the node was
  moved back and one exists, reviews it and brings its description up to
  date) and certifies the `pr` phase (`tod-cli phase certify`). The gate,
  `pr → pr_draft`, asks that a PR is linked and open
  (`pr-pr_draft.pr-open`) and that the phase is certified in this stay
  (`pr-pr_draft.phase-certified`). A certificate counts only for the stay it
  was given in, so a move back always sends the agent round again.
- In `pr_draft` it gets the draft ready *before* a person looks: the PR
  agent takes at least one turn in every stay in `pr_draft` (so the skills
  configured for `pr_draft` apply, even when GitHub shows no work), then the
  same loop as `pr_review` clears checks, branch drift, threads, and bot scores. A
  wait for the human review ends the run's work there (it is the goal), and
  the gate asks that a person has been put on it. "Run with a new session"
  forces a fresh turn.
- In `pr_review` it answers the human reviewer's feedback.

The two states, as they were split:

- `pr`: the agent opens the pull request as a **draft** (`tod-cli pr open`;
  the project's own skill for it goes under the `pr` phase in Lifecycle
  config) and certifies it.
- `pr_draft`: the draft is worked on its automated feedback (the skill for
  that goes under the `pr_draft` phase). Its gate, `pr_draft → pr_review`
  (the one `pr` had before `pr_draft`), is app-checked: the PR is open and out
  of draft (`pr-pr_review.ready-for-review`) and a person is on it as
  reviewer, requested or already reviewed (`pr-pr_review.reviewer-assigned`).
  A person does both; the run waits at the gate until they have. These two
  are hardcoded. What "ready for human review" means will want to be
  scriptable per project; that is for later.
- `pr_review`: the babysitter. Its skills go under the `pr_review` phase,
  so responding to reviewers can use a skill of its own. Its gate,
  `pr_review → approved`, is the one `pr` had (the criteria below, which keep
  their `pr-approved.*` slugs so existing waivers still apply).
- Skill groups (`;` in a phase's skills, see `.claude/CLAUDE.md`, "Lifecycle
  config") work as in every phase: on entry the PR agent's session and then each
  later group run in order, once per stay, and the run then looks at the pull
  request. `pr_draft` always takes its entry turn; `pr_review` only when it has
  skills. Work on the pull request (a code review's threads, CI) sends the
  run round again from the agent's session and through every group; with no
  work the run waits for the person (woken by the webhook, else by polling) and
  nothing runs again on a restart in the same stay.
- A configured bot's findings it could not post inline (Greptile's
  "Comments Outside Diff" comment, whose lines are outside the diff) are work
  whatever its score: the comment's text goes to the agent as feedback, like a
  person's review comment, once per edit of it. Other comments by a bot are
  not feedback. Bots are set in Settings → Lifecycle, "Review bots to wait for".
- `approved` is unchanged: it waits for the merge.

Below, "the `pr` state" means `pr_review` unless it is about opening the PR.

## Problem

A node in `pr` has a linked pull request. Today its only gate check is
`pr-approved-mergeable`: GitHub's `mergeable_state == "clean"`. That says
nothing about review comments, and nothing acts on them. The user reads the
feedback, asks an agent to fix it, replies, resolves threads, and asks the
review bot to look again. This is slow and takes hours of wall-clock time,
because reviewers and bots answer late.

The goal: once a PR is open, tod babysits it without the user. It reads
feedback, fixes what needs fixing, replies, resolves threads, keeps the branch
current with its base, and waits for bot reviews and CI, until the PR is **fully
mergeable except for the human review**. That review is the one thing it
cannot do; it waits for it (see the loop). It stops for the user only at a
blocker it cannot solve.

## The rule, applied

Gates are app checks; no agent evaluates one (see phase-agents.md). So:

- **Readiness is a list of criteria the app derives** from GitHub; the
  user can move a node past a failing one with the lifecycle dropdown.
- **The babysitter runs the PR agent.** The agent changes the world (code,
  replies, thread resolution, and review requests where the project's own
  skill says to make them) so the criteria become true. Neither decides
  readiness.

## Criteria

Derived in `tod_core::gate::derived` and read from GitHub through the same
client `pr_mergeable_outcome` uses. All apply to **every linked PR**.

| Slug | Passes when |
|---|---|
| `pr-approved-mergeable` (existing) | GitHub `mergeable_state` is `clean`, or the PR is merged. |
| `pr-up-to-date` | The PR's head contains the base branch's tip (GitHub `mergeable_state` is not `behind`), with no conflict (`mergeable_state` `dirty`, or GitHub's `mergeable` false, which a draft's `draft` state would otherwise hide). |
| `pr-threads-resolved` | Every review thread, human or bot, is resolved or outdated. |
| `pr-review-current` | For each configured review bot, a review exists **for the current head commit** (so a push makes the old review stale). See Bots. |
| `pr-review-score` | For each configured bot with a `min_score`, its latest review for the current head has a score at or above it. |

Each fails with a detail line naming the thread, bot, or score, so the
runner line's Check now result shows why. There is no waiver: the user moves the node on with the task panel's
lifecycle dropdown.

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
- **A head that only merges the base in is covered.** Greptile does not
  re-review a head whose own change is unchanged, so when the summary names a
  commit other than the head, `Github::same_change` compares
  `base...reviewed` with `base...head` (merge-base diffs, so what the merge
  brought in drops out). The review covers the head when the head's diff is a
  **subset** of the reviewed one: every file of the head's is in the reviewed
  one's with the same added and removed lines (hunk positions and context
  ignored). A file only the reviewed diff has (the base absorbed it) is fine;
  a file only the head has, or a differing one, is a real change and the
  review stays stale. Then the review is current, with the reviewed commit's
  score, and no `BotReviewMissing` is produced. A reviewed commit no longer in
  the repository (a force-push) is a real change. If the comparison cannot be
  made (GitHub failing, an unknown base or head, a binary file with no patch,
  a compare of 300 or more files, which GitHub cuts off) the assessment fails
  with that error, never a guess either way.
- Threshold: `min_score = 4` for this project (4 or 5 passes).
- Asking for a review: the app never asks a bot. How and when to ask (on a
  draft, after a round of fixes) is project-specific, so it is in the
  project's own skill for `pr_draft` and `pr_review`, and the agent does it.
  When the bot has not reviewed the current head `rerun_after_minutes`
  (default 10) after the head commit, and the agent has had no turn in that
  time, the app sends the agent `Work::BotReviewMissing`; it may need
  asking. That repeats at most once per `rerun_after_minutes`, and a bot
  that never comes ends in the round limit.

The bot's specifics (author, score pattern) are data on a `ReviewBot`
definition in code (Greptile is the only built-in); the project config below
only selects and tunes bots.

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

The autopilot's loop for `pr_draft` and `pr_review` (`autopilot::babysit`,
run from the gate step of those states, which have no phase agent). It is
the only loop there: the PR agent (`ProtocolKind::Pr`) takes one turn each
time it is sent, and hands back. The role docs' **Done when**: every review
thread is answered and resolved, every bot review is current and at or above
its threshold, and the PR is mergeable. Nothing is certified: the gate
derives all of this.

### Loop

Each round reads every linked PR (`pr_readiness::Assessment`):

1. Unanswered decision → stop for the user.
2. **Work** (open threads, a failed check, a bot score under threshold not
   yet seen by the agent, a bot review overdue, the branch behind its base,
   a conflict, a person's review or comment since the agent's last turn) →
   one agent turn with the work, then read again.
3. **Waiting** (a bot has not reviewed the current head, checks still
   running, GitHub has not computed mergeability) → poll. No agent turn is
   used while waiting.
4. **Waiting on a person** → in `pr_draft`, tod's own review first (below),
   then the gate (a person on the draft is what it asks); in `pr_review`,
   end the run with a wait the review's webhook or the next scheduled look
   wakes.
5. **Clear** → in `pr_draft`, tod's own review first; then the gate.

### tod's own code review in `pr_draft`

The bots may be satisfied while the project's own review would not be. A
node whose lifecycle config has **Include review steps** on for `pr_draft`
(`lifecycle_config::resolve_pr_draft_review`, inherited like the rest of the
config, off by default; `tod-cli capabilities set <node> lifecycle-config
--pr-draft-review on`) runs it once nothing but a person is left on the pull
request (`tod_core::pr_draft_review`, `Autopilot::own_review`), and it
reviews every change the PR agent makes for feedback before that change is
pushed (`Autopilot::pr_change_reviewed`), so a person is troubled again only
with a change tod's review has cleared:

- **Change, review, then push.** A round of work is two turns of the PR
  agent in the stay's conversation. First it brings the branch up to date,
  fixes what the feedback needs, runs the tests and commits, without pushing
  or answering (`pr::change_message`). If the branch's change against its
  base then differs from before (`pr_draft_review::branch_change`: the
  `-U0` diff from the merge base of `origin/<base>` and `HEAD`, without hunk
  positions, so merging the base in alone does not count), the Review → Fix
  → Review loop of `review` runs on the local head (`Autopilot::review_change`,
  same `lifecycle.review_round_limit` and `ReviewNotConverging`), and the
  clean review records the local `HEAD` as the head it reviewed. A merge
  alone carries a clean review of the previous head over. Then the agent
  pushes and answers each thread (`pr::push_message`); `owed` finds the
  pushed head already reviewed. Where the round is is saved
  (`AutopilotState::pr_change`), and a git failure stops the run.

The steps below are for a head that reaches the pull request without that
loop (the stay's first turn, or a push from outside):

- **Review.** The Review protocol, with the `review` phase's skills in every
  group, on the current head. Its opening (`REVIEW_ON_PR_SESSION`,
  `surface/review-on-pr`) says it is reviewing the branch's local head; it records
  findings through `tod-cli review` as in `review` and never posts itself.
  The app records the heads it reviewed on the review's report (`pr_heads`).
- **Post.** The app posts the findings as one review: on a changed line,
  else on the file (a path from a parent folder, such as a submodule's
  `flagship/flagship/settings.py`, is matched to the changed `flagship/settings.py`;
  a file the change does not touch goes on the nearest changed file, its body
  naming the real location), and a finding with no file goes on the file with the most changed lines. Only a
  pull request with no changed files gets the review's text, since only a thread
  can be replied to and resolved. Each finding records its
  thread (`review_findings.pr_thread`), found by a marker in the comment.
  A failure to post stops the run (`PrBlocked`).
- **Fix.** An open thread is work for the PR agent, whose opening adds the
  `fix` phase's skills; `tod-cli pr threads answer` answers the thread's
  finding too. A finding no open thread carries is sent to the agent by id,
  answered through the `review` noun (fixed or rejected); the same ones left
  unanswered twice stop the run (`PrStuck`). A thread a person resolves
  without an answer declines its finding.
- **Again.** As in `review`, only a review clears the change: the new head
  is reviewed again until a review records nothing that stands. More than
  `REVIEW_ROUND_LIMIT` reviews in one stay stops the run
  (`ReviewNotConverging`).

The `pr_draft → pr_review` gate checks the same thing
(`pr-pr_review.own-review`): it passes when the steps are off, else needs
the latest own review to cover every linked PR's current head, a clean round,
and no open finding.

After a push the sequence is: the agent's fixes are pushed and its replies
posted **at once** in its turn (and, where the project's skill says so, it
asks the bot to review), and the node waits on `bot review`. When the review
lands, the loop starts again. That may repeat several times; the limits are
under "Guards against loops".

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

The agent is given, by the app (`conversation::pr::work_message`; it
fetches the details with `tod-cli pr`), what needs doing on the PR:

- unresolved threads: id, file and line, every comment, diff hunk;
- top-level PR comments and reviews since its last turn, bot summaries
  included with their scores;
- failing checks with the tail of their logs;
- whether the branch is behind the base, and any conflicts.

**Keeping current with the base.** When the branch is behind, the agent merges
the base branch into it (a merge, never a rebase or force-push, so review
history stays intact), resolves conflicts, and leaves the recorded test run
green before it pushes. This is ordinary babysitter work, not a stop.
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

The agent commits and pushes, then answers each thread with `tod-cli pr
threads answer`, which posts the reply and resolves the thread, so replies
can cite the pushed commit. Asking a bot for a review, where it is needed, is
the agent's too, as the project's skill says.

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
- Every posted reply, push, and resolve is recorded as a `UserAction`-style
  journey event with what it did, so a wrong autonomous action is
  traceable.

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
- A bot score under threshold is work once: the agent's next turn sees it.
  If that turn pushes nothing (the agent judges the remaining findings
  wrong), the score is left to the gate, which stops for the user, who
  decides whether to move the node on with the lifecycle dropdown.
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
  task panel runner line (its Check now result lists the failing criteria),
  and the Settings section for bots.

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

- Posting and resolving use the same GitHub credentials the app uses to
  read the PR (the engineer's own).
- Human and bot threads are treated alike; the agent replies and resolves.
- Fixes are pushed and replies posted immediately; only the bot review is
  waited on, by polling.
- The app never asks a bot for a review: that is project-specific, and in
  the project's skill. A tod skill (`tod-fix-ci`) names no bot, and says
  nothing about asking one, so it cannot contradict the project's skill run
  beside it.

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
  the PR are consecutive agent turns on feedback about the change itself (open
  threads, failing checks, a low score, requested changes, review comments:
  `Work::is_feedback`); merging the base in or asking a bot to review the new
  head is housekeeping and not a round. The count
  (`AutopilotState::loops.pr_rounds`) starts over when the PR is clear or
  waits for a person's review, and when the budget is renewed. Both limits
  are `pr_readiness.max_thread_rounds` / `max_rounds`.
- **One loop.** The pull request protocol (`conversation::pr`) loops only
  in `pr`, until the draft is certified; in `pr_draft` and `pr_review` it
  hands back after every turn. The autopilot's `babysit`
  (`autopilot/mod.rs`) is the loop there: it reads the PR, runs the agent
  while there is work, and polls GitHub while it waits. It runs on the run's
  own thread and sleeps in half-second slices so Pause is heard, and time
  asleep is not time worked. It does not push a store event; the task
  panel's runner line shows "waiting for greptile review" through
  `StepHook::waiting`.
- **The poll is fixed at 60 s, then 5 min after an hour**, not configurable.
- **Not built:** the Settings UI for bots (edit `pr_readiness` in `tod.yml`);
  `WaitingOn` in the task panel beyond the runner line; the `--agent mock`
  directive support for a fake babysat PR (autopilot tests use a fake
  `PrFeed`).
- The gate criteria are `pr-approved.up-to-date`, `.threads-resolved`,
  `.review-current`, `.review-score`, beside `.mergeable`. The two review
  criteria pass when no bot is configured. A criterion cannot be waived; the user moves the node on with the lifecycle
  dropdown.

## Waiting on GitHub after the review: the merge, then the release

`autopilot/github_wait.rs`. Once the gate lets a node into `approved`, a
person merges the pull request, and some time after that a release carries
it. Neither is agent work, so the run waits, the same way it waits for a
person's review:

- **`approved`** — until every linked pull request is merged. The run ends as
  `Outcome::Waiting` with an `event` wait on `github:pr:<n>:merged`.
- **`merged`** — before the phase agent runs, until a published release's
  notes name each linked pull request. The wait is on
  `github:release:published`. The next release is not assumed to include the
  change (a cherry-pick or another release branch breaks that), so the notes
  are searched (`release_watch::release_carrying`): the pull request's title
  (case and spacing ignored), its link, or its `#number`, in a release
  published after the merge. Drafts and pre-releases do not count. The phase
  agent then runs with the release already out and records its evidence. A
  node with no linked pull request waits for nothing: its phase agent drives
  the release.

When a node has several pull requests the wait lists where each stands
(`acme/app#42 released in v1.4.2; acme/infra#7 merged, no release names it
yet`; in `pr`, `approved` / `awaiting review` / `changes requested`), so the
person can see what holds the node up. A repository whose profile is `merge`
(`doc/lifecycle/repo-profiles.md`) publishes no releases and is never waited
on for one.

The deadline of each wait is the next scheduled look (`review_schedule`), at
which the scheduler wakes the node; a GitHub webhook for the merge, or for a
published release (subscribe the webhook to *Releases*), satisfies the wait
sooner. Waking runs the autopilot again, which reads GitHub afresh, so a late
or duplicate wake is harmless. A pull request that cannot be read (no
credentials, GitHub down) is not waited on: the run goes to the gate, which
says why it fails.

## Looking again while waiting on a person

A pull request waiting for a person (a draft to be marked ready, a reviewer)
is read again every `pr_readiness.review_schedule.interval_minutes`
(default 5 in the working day; `off_hours_interval_minutes`, default 240,
caps the gap at night and at weekends). Each look, in the app's `Check now`
and in the orchestrator's gate (which absorbs the `pr_review` wait too, so a
pull request waiting for a person does not wake the sandbox), has three
outcomes:

1. Nothing changed: the next look is recorded and the sandbox stays asleep.
2. Ready for review and reviewed: the wait ends and the gate can pass.
3. Feedback: a person asked for changes, review threads are open, a check
   failed, the branch is behind, a bot scored under its threshold, a bot's
   review of the head is overdue, or a person
   left a review with text and no verdict (`Work::ReviewComments`) since the
   head commit and since the pull request agent's last turn. The wait is
   cancelled, the node's supervisor is woken, and the `pr` agent runs with
   the feedback text. A bot's report above its threshold, and reviews that
   say nothing, are not feedback.

The agent's own turn answers the reviews before it, so one review is acted on
once; a new push or a newer review reopens it.
