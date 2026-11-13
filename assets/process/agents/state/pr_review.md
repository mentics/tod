# State: `pr_review`

**Gate:** `pr_review` → `approved`. The app checks it against GitHub; no agent evaluates it: the PR is mergeable (approved, checks green), its branch is up to date with its base, every review thread is resolved, and every review bot the project names has reviewed the current head and scored it high enough.

## First

The PR is driven as the task's `pr` conversation (the protocol is named for the pull request; this state is its review). The lifecycle runner sends you into it when there is something to do, and tells you what: the open threads, the failing check, the branch behind its base.

1. Read lifecycle state, obligations, plan, and the review's findings and their answers.
2. Check the pull request's status before doing anything else (`tod-cli pr status`): the `pr` state opened it as a draft, `pr_draft` cleared the automated feedback on it, and a person has since marked it ready and asked for a review.

## Responsibilities

The pull request already exists: the `pr` state opened it, and the gate out of `pr_draft` held until it was open for review with a reviewer on it. Do not open another. **Babysit** it: get it to a state that is 100% mergeable except for the human review, which you cannot do and do not wait on.

- **Keep the branch current.** When the base branch has moved on, merge it into the branch (a merge, never a rebase or a force-push, so review history stays intact), resolve conflicts, and run the tests.
- **Answer every review thread**, human or bot, with the same care. Read the thread with its code. Then either fix it, or decline with a reason, and answer the thread so the reply is posted and the thread resolved. Push before you answer, so the reply can name the commit.
- **Fix failing checks** the change caused.
- **Fix what a review bot found**, whatever its score: a score at or above the threshold does not make a real problem acceptable. Its threads and its summary say what; apply the scope rule below to each.

You never wait for a review or for CI: the app does, and calls you back when there is something new.

### Scope

Do not grow the PR. For each piece of feedback ask:

- **Does this change introduce the problem?** Then it is in scope. Fix it. No exceptions.
- **Was the problem already there, and is fixing it not what this task is for?** Then it is out of scope: decline, say it predates the change, and where it is worth keeping, record it as a new task under the project so it is not lost.
- **Would fixing it enlarge what the task set out to do** (new behaviour, a refactor past what the change touches)? Out of scope, same handling.
- A real concern (correctness, security, data loss) is never declined for scope alone. If a problem that was already there makes this change unsafe, fixing it is in scope.

Always give the reason in the reply, so the reviewer can disagree. If you cannot tell whether something is in scope, that is a question for the user, not a guess.

### Rounds

A thread you have answered three times that is still open, or four rounds of fixing and reviewing on the PR as a whole, mean something unusual is going on. Stop and hand back with what keeps coming back; the app stops the run on the same limits.

### Before you say nothing is left

Do not rely on what the runner sent you or on GitHub's merge state alone: the merge state reads `blocked` while a review is missing, and that hides a branch that is behind its base. Check each of these yourself, and fix what fails:

- **The branch is current.** Compare it with its base (`git fetch`, then see whether the base has commits the branch lacks); if it does, merge the base in.
- **Every review bot the project names** (there may be several, or none) **has reviewed the current head, and its latest score for the head meets its threshold.** A score, or a review, left on an earlier commit does not count: you never ask a bot yourself, and the project's skills must not either (a bot's credits are spent once per pull request, on the final head): tod asks it, only after its own review of that head is clean, waits for it, and sends you what it finds, which you fix where this change introduced it.
- **Every open review thread, from a person or a bot, is answered.**
- **This phase's configured skills have been applied**, as the context lists them.

Only when all of these hold, say nothing is left.

## Done when

The app checks the gate itself from GitHub; you are done when nothing on the PR needs you:

- Every review thread is answered and resolved.
- The branch contains its base and does not conflict with it.
- Checks that the change affects are passing.
- Any review bot's score for the current head meets its threshold.

## Blockers

A conflict you cannot resolve, a request you cannot judge, a failing check you cannot fix, a question of scope you cannot settle → ask the user, or record blocked with why, and stop.
