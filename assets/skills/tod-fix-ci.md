# Fixing a pull request's checks and review feedback

How to clear what a node's pull request needs from you: failing checks, and
review comments from bots and people. Your phase's role doc says which of
these this state covers (in `pr`, checks and bots; in `pr_review`, everyone),
and how to decide scope; this skill is how to do the work.

## You never wait

A session does work and ends. It does not wait for anything outside it:

- **No waiting on CI or a review.** Not `gh pr checks --watch`, not a
  `sleep`, not a polling loop, not a background watcher you plan to come back
  to. When a check is still running, or a review has not landed, and there is
  nothing else to fix, end your turn.
- **No asking a bot to review.** Do not `@greptileai`, do not comment to
  trigger Macroscope, do not add a label. The app asks for reviews when they
  are due, waits for them and for CI, and sends you back with what landed.
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
blocks the merge. In the Cofactr workspace:

| Repo     | Required                                                                      | Advisory                              |
| -------- | ----------------------------------------------------------------------------- | ------------------------------------- |
| flagship | `test`, `Migrations / migration conflicts`, `Migrations / missing migrations` | `Lint / ruff`, `Type check / pyright` |
| ui       | `check`                                                                       | --                                    |
| kb       | none                                                                          | all of them                           |

The table drifts; the branch protection is the truth:

```
gh api repos/:owner/:repo/branches/main --jq '.protection.required_status_checks.contexts'
```

Run `gh` from inside the repository the PR belongs to (the submodule, in a
workspace of submodules). From the wrong one it says "Could not resolve to a
PullRequest", which reads like a broken PR.

## Flagship checks

`flagship-lint --diff` runs the lint and type checks exactly as CI does. Run
it before each push.

### `Lint / ruff`

- **`ruff check` and `ruff format --check`**: mechanical, always fix.
  `--format --fix` is safe. A bare `--fix` rewrites the whole file, including
  code the PR never touched, so read `git diff` after.
- **Quality ratchet** (`flagship-lint --ratchet`): fails when a function's
  complexity or statement count, or a file's length, grows past
  `.quality/baseline.json`. Simplify the code. Record growth you mean with
  `uv run python scripts/check_quality_ratchet.py --update --accept-regressions`,
  and say why in the PR.

### `Type check / pyright`

basedpyright over the whole tree against `.basedpyright/baseline.json`. Any
error the baseline does not record fails, including in files the PR never
touched (a changed signature breaking a caller). Fix them. Do not disable the
job, scatter `# pyright: ignore`, or fold unrelated type debt into the PR.

### Baselines

`.basedpyright/baseline.json` and `.quality/baseline.json` record existing
debt and should only shrink.

- **Stale** (the PR fixed something baselined: "Baseline is up to date"
  fails, or `stale:` lines): run `flagship-lint --pyright --diff --fix` and
  `flagship-lint --ratchet --fix`, and commit the result. Both only lower
  values.
- **Moved or renamed code**: both files are keyed by path, so moved code's
  existing debt reads as new. Confirm it is the same code (a rename that
  changed a type is a real finding), fix what the PR caused, re-record the
  rest (`uv run basedpyright --writebaseline`, and the ratchet `--update`
  above), and say in the PR what moved and how many entries.
- **Merge conflict in a baseline**: do not hand-merge the hunks. Take the
  base's version, then re-apply this PR's side with the two `--fix` commands,
  and record again any growth the PR deliberately recorded.

### `Migrations / migration conflicts` and `missing migrations`

- `migration conflicts` fails when merging would leave an app with two leaf
  migrations. It scans the live tip of `main`, so it can turn red with no
  push of yours; when you are woken for it, that is why.
- Fix: merge `origin/main` into the branch, then generate the merge migration
  with `python manage.py makemigrations <app> --merge`. Never hand-write it,
  and never renumber a migration already on `main`. If the PR's own migration
  has not merged anywhere, regenerating it onto the new leaf is an
  alternative (regenerate, do not edit `dependencies`).
- Check locally: `python3 scripts/check_migration_conflicts.py .` from
  `flagship/`.
- `missing migrations`: a model changed without its migration. Run
  `makemigrations` and commit the result.

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

### The bots

| Bot                      | Author (`user.login`) | Severity                          |
| ------------------------ | --------------------- | --------------------------------- |
| Greptile                 | `greptile-apps[bot]`  | `P1` (worst) / `P2` / `P3` / nit  |
| Macroscope               | `macroscopeapp[bot]`  | `🔴 High` / `🟡 Medium` / `🟢 Low` |
| Agent Code Review (ours) | `github-actions[bot]` | `Important` / `Nit` / `Pre-existing` |

- **Agent Code Review** is the Cofactr workspace's own CI (`pr-automation`,
  `.github/workflows/pr-automation-reusable.yml`), judging by
  `.claude/skills/code-review/REVIEW.md`. Its comments carry
  `<!-- pr-automation:code-review -->` (summary) or
  `<!-- pr-automation:inline-code-review:<fingerprint> -->` (inline). It runs
  once per `code-review` label, which only a person applies. **Never add the
  label.** If a finding is wrong in a way that will recur, say in the reply
  that `REVIEW.md` is where it should change.
- `🚫 **[pyright]**` comments from `github-actions[bot]` on older flagship PRs
  are output from a retired job, not findings. Skip them.
- **Greptile's "Comments Outside Diff."** Findings Greptile cannot anchor to a
  changed line go into one general PR comment marked
  `<!-- greptile_failed_comments -->` (a collapsed "Comments Outside Diff
  (N)"), not into threads, so `pr threads` does not show them:

  ```
  gh api repos/:owner/:repo/issues/$PR/comments --paginate \
    --jq '.[] | select(.user.login=="greptile-apps[bot]") | select(.body | test("greptile_failed_comments")) | .body'
  ```

  Each item names a path, a line range and a severity; triage them like any
  finding. They have no thread to answer, so answer them all in one top-level
  `gh pr comment`, item by item: what you fixed, or why not. Do not mention
  `@greptileai` in it; that asks for a review, which is the app's to do.

### Deciding each one

Read the code the comment is about before changing anything.

- Weigh by severity: look hard at `P1`/`P2`, `High`/`Medium`, `Important`.
  A nit that does not fit the codebase's conventions may be declined.
- Bots misread context, flag intended design, and flag what was already
  there. Do not apply a `suggestion` block without checking it; when it is
  right, apply it and run the checks. What a bot itself labels
  `Pre-existing` is usually out of scope.
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

A reply does not make a bot re-score. That is fine: the app decides when a
bot should look again, and asks it.
