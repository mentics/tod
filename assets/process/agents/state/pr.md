# State: `pr`

**Gate:** `pr` → `approved`. The app checks it against GitHub; no agent evaluates it: the PR is mergeable (approved, checks green), its branch is up to date with its base, every review thread is resolved, and every review bot the project names has reviewed the current head and scored it high enough.

## First

The PR is driven as the node's `pr` conversation. The lifecycle runner sends you into it when there is something to do, and tells you what: the open threads, the failing check, the branch behind its base.

1. Read lifecycle state, obligations, plan, and the review's findings and their answers.
2. Check the pull request's status before doing anything else — an earlier session may already have opened the PR.

## Responsibilities

Open the PR if none exists yet, then **babysit** it: get it to a state that is 100% mergeable except for the human review, which you cannot do and do not wait on.

- **Keep the branch current.** When the base branch has moved on, merge it into the branch (a merge, never a rebase or a force-push, so review history stays intact), resolve conflicts, and run the tests.
- **Answer every review thread**, human or bot, with the same care. Read the thread with its code. Then either fix it, or decline with a reason, and answer the thread so the reply is posted and the thread resolved. Push before you answer, so the reply can name the commit.
- **Fix failing checks** the change caused.
- **Fix what a review bot found** when its score is under the threshold: its summary comment says what.

You never ask a bot to review and you never wait for one: the app does both, and calls you back when there is something new.

### Scope

Do not grow the PR. For each piece of feedback ask:

- **Does this change introduce the problem?** Then it is in scope. Fix it. No exceptions.
- **Was the problem already there, and is fixing it not what this node is for?** Then it is out of scope: decline, say it predates the change, and where it is worth keeping, record it as a new task under the project so it is not lost.
- **Would fixing it enlarge what the node set out to do** (new behaviour, a refactor past what the change touches)? Out of scope, same handling.
- A real concern (correctness, security, data loss) is never declined for scope alone. If a problem that was already there makes this change unsafe, fixing it is in scope.

Always give the reason in the reply, so the reviewer can disagree. If you cannot tell whether something is in scope, that is a question for the user, not a guess.

### Rounds

A thread you have answered three times that is still open, or four rounds of fixing and reviewing on the PR as a whole, mean something unusual is going on. Stop and hand back with what keeps coming back; the app stops the run on the same limits.

## Done when

The app checks the gate itself from GitHub; you are done when nothing on the PR needs you:

- Every review thread is answered and resolved.
- The branch contains its base and does not conflict with it.
- Checks that the change affects are passing.
- Any review bot's score for the current head meets its threshold.

## Blockers

A conflict you cannot resolve, a request you cannot judge, a failing check you cannot fix, a question of scope you cannot settle → ask the user, or record blocked with why, and stop.
