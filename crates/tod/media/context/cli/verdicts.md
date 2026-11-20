## `tod-cli verdicts`

Verification's verdict on each obligation of a node: whether it holds in the
running work, and the evidence. The app reads these, not your reply — a
requirement you checked but gave no verdict counts as unchecked.

```
tod-cli --data-root <DATA_ROOT> verdicts list    [--node <NODE_UUID>] [--unchecked]
tod-cli --data-root <DATA_ROOT> verdicts record  <OBLIGATION_ID> [--node <NODE_UUID>] --status verified|failed --evidence <TEXT> [--method live|local-data|static|unit] [--ran <TEXT>]
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

`--method` says how you showed it, and is required for `verified`: `live`
(the running work against the real system), `local-data` (a local database
seeded with crafted data), `static` (reading the code, a type check), or
`unit` (tests with mocks). `--ran` names what you ran: the command, request,
or query. The gate accepts `static` and `unit` only for an obligation marked
code-only; a live one needs `live` or `local-data`. `list` and `history` show
each verdict's method and what ran.

`history` lists every verdict the node's obligations have been given, oldest
first — what failed along the way, and what it took to pass.
