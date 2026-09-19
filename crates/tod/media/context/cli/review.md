## `tod-cli review`

Code review findings on a node, and the response each one gets. The app shows
them to the user as a list; a finding that is only in a reply is lost.

```
tod-cli --data-root <DATA_ROOT> review list    [--node <NODE_UUID>] [--open]
tod-cli --data-root <DATA_ROOT> review show    <FINDING_ID>
tod-cli --data-root <DATA_ROOT> review add     [--node <NODE_UUID>] --severity high|medium|low --summary <TEXT> [--file <PATH>] [--line <N>] [--detail <TEXT>]
tod-cli --data-root <DATA_ROOT> review respond <FINDING_ID> --status open|fixed|out_of_scope|declined [--response <TEXT>]
tod-cli --data-root <DATA_ROOT> review done
```

Inside a review conversation `--node` defaults to the node under review, and
each finding is filed under the conversation. Finding ids may be given in full
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

`respond` answers a finding: `fixed` (the response points at the change or
commit), `out_of_scope` (real, but not this node's), or `declined` (not
critical, beyond the requirements, or not worth the cost). Every status but
`open` needs `--response`; `open` clears it.

`done` records that the review is finished. It only works inside a review
conversation; until a turn records it, the app sends the review back to you.
