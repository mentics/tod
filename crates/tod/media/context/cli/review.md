## `tod-cli review`

Code review findings on a node, the response each one gets, and what each
review checked. The app shows them to the user as a list; a finding that is
only in a reply is lost.

```
tod-cli --data-root <DATA_ROOT> review list     [--node <NODE_UUID>] [--open]
tod-cli --data-root <DATA_ROOT> review show     <FINDING_ID>
tod-cli --data-root <DATA_ROOT> review add      [--node <NODE_UUID>] --severity high|medium|low --summary <TEXT> [--file <PATH>] [--line <N>] [--detail <TEXT>] [--root-cause <TEXT>]
tod-cli --data-root <DATA_ROOT> review respond  <FINDING_ID> --status open|fixed|out_of_scope|declined|rejected [--response <TEXT>]
tod-cli --data-root <DATA_ROOT> review coverage --lens <LENS> --checked <TEXT> --result none|finding:<FINDING_ID> [--result finding:<FINDING_ID>]... [--file <PATH>]... [--read <PATH>]...
tod-cli --data-root <DATA_ROOT> review coverage --lens <LENS> --result n/a --reason <TEXT> [--file <PATH>]...
tod-cli --data-root <DATA_ROOT> review coverage --pass saturation --checked <TEXT> --result none|finding:<FINDING_ID> [--result finding:<FINDING_ID>]...
tod-cli --data-root <DATA_ROOT> review done
```

Inside a review or fix conversation `--node` defaults to the node under
review, and each finding is filed under the conversation. Finding ids may be given in full
or as the 8-character prefix shown in listings. `list --open` shows only the
findings nobody has responded to.

`add` records one open finding:

- `--severity`: `high` (wrong behavior, data loss, a security hole, a broken
  build), `medium` (a real defect in a narrower case, or a maintenance hazard
  that will cause one), `low` (worth fixing, harmless as it stands).
- `--summary`: the defect in one sentence — the claim, not the fix.
- `--detail`: the concrete inputs or state that go wrong, and what happens;
  add the fix you would make if it is not obvious. Use `--detail -` and a
  heredoc for anything long.
- `--file` / `--line`: the repo-relative path and 1-based line the finding is
  anchored to, when it is about one place.
- `--root-cause`: the assumption or pattern that made the defect possible
  ("the org's supplier id is treated as unique"). Give every finding that
  shares a cause the same words: the fixer is shown them together, as one
  class to fix. `list` and `show` print it under the finding.

`respond` answers a finding: `fixed` (the response points at the change or
commit), `out_of_scope` (real, but not this node's), `declined` (not
critical, beyond the requirements, or not worth the cost), or `rejected` (not
a problem after all — the response says why, for the user to read). Every
status but `open` needs `--response`; `open` clears it. Inside a fix or pull
request conversation only `fixed` and `rejected` are accepted: the others are
the user's answers.

`coverage` records one entry of this review's coverage record, which the user
sees beside the findings. Record one per lens as you sweep it, and more when
it helps to say what you checked file by file; entries add up, none replaces
another.

- `--lens`: one of `obligations`, `data-assumptions`, `external-systems`,
  `persisted-results`, `interactions`, `reach`, `data-writes`, `concurrency`,
  `error-paths`, `tests`, `verification-evidence`.
- `--pass saturation`: the pass after the sweep, made assuming you missed at
  least three defects. Takes `--checked` and `--result` like a lens; it
  always applies, so never `n/a`.
- `--checked`: what you checked and how ("org id lookup: queried prod, 113
  orgs with more than one id"). `--checked -` reads it from stdin.
- `--result`: `none` (checked, nothing wrong), `finding:<FINDING_ID>` for each
  finding the check produced (repeat it; record the finding first), or `n/a`
  with `--reason` saying why the lens does not apply to this change.
- `--file`: a changed file the entry covers. `--read`: a file you read for it
  (the project's review guidance, a skill it routes to, code a lens needed).
  Both repeat. A selected skill or file whose text is not in your opening
  must be named under `--read` (a skill by its name, a file by its path)
  before `done` is accepted.

`done` records that the review is finished. It only works inside a review
conversation; until a turn records it, the app sends the review back to you.
It is refused, with exactly what is missing, until every lens has a coverage
entry in this review and a saturation pass with `--result none` was recorded
after the last finding this review added. A saturation pass that finds
something means recording those findings and making another pass.
