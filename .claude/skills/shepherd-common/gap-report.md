# Reporting a gap

A gap is anything tod should have done itself: a bug, a missing feature, a
misleading status, a prompt that asked the user what tod could decide, a
`tod-cli` verb a shepherd needed and didn't have, a UI element the UI
shepherd couldn't reach. Shepherds report gaps; only the manager starts
fixers. Many nodes hit the same gap, so one gap must collect them all.

Before reporting, list the open gaps (`ledger.py gap list`) and attach the
node to one that matches (`gap report` with the same signature does this).

## Signature

`<kind>:<area>:<short-slug>`, lowercase, stable across nodes. The same cause
on another node must produce the same signature.

- `kind`: one of `bug`, `enhancement`, `prompt`, `status`, `setup`,
  `cli-parity`, `ui`, `ui-target`.
- `area`: the phase, protocol or component (`pr_draft`, `review`, `evaluator`,
  `runner-menu`, `sandbox`, `gate`, `babysitter`, `task-panel`, …).
- `short-slug`: the cause, not the symptom or the node:
  `bug:babysitter:bot-ask-not-counted`, not `eng-2925-stuck`.

`cli-parity` is a GUI action with no shepherd `tod-cli` verb. `ui-target` is
an element the UI shepherd could not find in the accessibility tree.

## Summary

One paragraph the manager can brief a fixer from without asking you:
- the symptom, with node, ids and evidence (log lines, table rows, a PR URL);
- the cause as far as you found it, and what you ruled out;
- every code path you know of that does the same thing;
- what tod should have done instead.

Then set the node's verdict to `blocked_on_gap` with the gap id, unless
another action of yours can still move it.
