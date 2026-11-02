# Fixing a pull request's checks and review feedback

How to clear what a node's pull requests need from you: failing checks, and
review comments from bots and people. It is meant for both `pr_draft` (before
a person reviews: checks and automated reviewers) and `pr_review` (after: the
human review too, and whatever the bots say about the changes it brings).
Your phase's role doc says which feedback the state covers and how to decide
scope; this skill is how to do the work.

This skill is the procedure for one turn. A project skill configured after it
for the same phase is its reference: which checks are required, how to run
and fix the project's own checks, and its own review bots. Where it says
something more specific, follow it.

The loop is the app's, not yours: it watches the pull requests, waits for CI
and reviews, and wakes you with what changed. Each turn you clear what is
there and stop.

## You never wait

A session does work and ends. It does not wait for anything outside it:

- **No waiting on CI or a review.** Not `gh pr checks --watch`, not a
  `sleep`, not a polling loop, not a background watcher you plan to come back
  to. When a check is still running, or a review has not landed, and there is
  nothing else to fix, end your turn.
- The node's sandbox sleeps while it waits, and you are woken with what
  changed. Assume nothing from your last session is still running.

Work that is yours to finish is different: a long test suite you started is
part of the turn. Run it in the background if it may outlast the tool's time
limit, and take its result before you finish.

If something you need will not reach you on its own (not checks, reviews or
comments on the linked PR, which the app watches), record it with
`tod-cli wait` and end the turn; `tod-cli help wait` has the forms.

## Each time you are woken

1. `tod-cli pr status` for the checks, and `tod-cli pr threads` for the open
   review threads. Read what the message that woke you says needs doing.
2. For each failing check, read the failure: `gh run view <run-id>
   --log-failed` (the run is in `gh pr checks <pr>`). Lint and type-check
   failures post no PR comments; they are in the log and the check's
   annotations.
3. Read every review comment in your scope (see below), and the code it is
   about.
4. Fix the failures and the valid findings. Run the tests and the checks you
   can run locally.
5. Commit and push to the PR's branch. Never rebase or force-push: when the
   base has moved on, merge it in.
6. Answer every thread, now that the reply can name the commit.
7. End the turn.

## Which checks matter

Every red check is yours to fix if the change caused it; a required one also
blocks the merge. The project skill lists them per repository; the branch
protection is the truth:

```
gh api repos/:owner/:repo/branches/main --jq '.protection.required_status_checks.contexts'
```

Run `gh` from inside the repository the PR belongs to (the submodule, in a
workspace of submodules). From the wrong one it says "Could not resolve to a
PullRequest", which reads like a broken PR.

## Fixing a check

Read the failure before changing anything, and reproduce it locally with the
command the check runs (the project skill names the local equivalents). Fix
the cause, not the check: do not disable a job, skip a test, or add
suppressions to make it pass. A check can turn red with no push of yours
(it compares against the base branch's moving tip); when you are woken for
one, that is a likely reason, and merging the base in is the usual start.

## Review comments

`tod-cli pr threads` lists every open review thread, human and bot, with its
code and comments. Comments that are not in a thread need fetching from
GitHub; check them too.

```
PR=<number>
# Review summary bodies ("2 nits", "requesting changes")
gh api repos/:owner/:repo/pulls/$PR/reviews --paginate \
  --jq ".[] | select(.body != \"\") | {id, user: .user.login, state, body}"
# General PR comments
gh api repos/:owner/:repo/issues/$PR/comments --paginate \
  --jq '.[] | {id, user: .user.login, body}'
```

People come first: their comments are the ones that most need an answer, and
when a person and a bot disagree, the person wins. An approval with nits
still wants each nit addressed or acknowledged.

Which review bots the project uses, how each marks severity, and where each
posts findings that are not in a thread are in the project skill.

### Deciding each one

Read the code the comment is about before changing anything.

- Weigh by severity: look hard at each bot's highest levels.
  A nit that does not fit the codebase's conventions may be declined.
- Bots misread context, flag intended design, and flag what was already
  there. Do not apply a `suggestion` block without checking it; when it is
  right, apply it and run the checks. What a bot itself says was already
  there is usually out of scope.
- Scope is the role doc's rule: what this change introduced is fixed; what
  predates it and would grow the PR is declined, with the reason.

### Answering

Answer every thread, fixed or declined, with `tod-cli pr threads answer`:
it posts the reply and resolves the thread. Push first, so a `--fixed` reply
can name the commit. A comment outside a thread (a review summary, a general
comment) gets a reply with `gh pr comment`.

- **Fixed**: what you changed, with the commit.
- **Valid but out of scope**: say so, and why.
- **Not a problem**: why the code is right as it is.

Keep replies short and factual. Do not mention AI, Claude, or an assistant.
