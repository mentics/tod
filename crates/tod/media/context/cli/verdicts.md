## `tod-cli verdicts`

Verification's verdict on each obligation of a node: whether it holds in the
running work, and the evidence. The app reads these, not your reply — a
requirement you checked but gave no verdict counts as unchecked.

```
tod-cli --data-root <DATA_ROOT> verdicts list    [--node <NODE_UUID>] [--unchecked]
tod-cli --data-root <DATA_ROOT> verdicts record  <OBLIGATION_ID> [--node <NODE_UUID>] --status verified|failed --evidence <TEXT>
tod-cli --data-root <DATA_ROOT> verdicts history [--node <NODE_UUID>]
```

Inside a verification conversation `--node` defaults to the node being
verified. Obligation ids may be given in full or as the 8-character prefix
shown in listings.

`list` shows the node's own obligations and where each stands: `unchecked`,
`verified`, `failed`, or `reopened` — verified once, but a plan step was
implemented again since, so it has to be checked again. `--unchecked` keeps
only the ones still owed a verdict.

`record` rules on one obligation — the node's own, or an inherited constraint
that applies to this work. `--evidence` is required for `verified` as much as
for `failed`: what you ran or drove, and what you saw. "The code does X" or
"the unit tests pass" is not evidence that a requirement holds; the behaviour
observed in the running work is. Use `--evidence -` and a heredoc for anything
long. Recording again replaces the verdict and keeps the old one as history.

`history` lists every verdict the node's obligations have been given, oldest
first — what failed along the way, and what it took to pass.
