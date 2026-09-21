## `tod-cli learn`

A node's `learn` retrospective: what its pass through the lifecycle taught.
It is stored once per pass and never changes after. The app reads it here, not
from your reply.

```
tod-cli --data-root <DATA_ROOT> learn record  [<NODE>] --content <TEXT>
tod-cli --data-root <DATA_ROOT> learn list    [<NODE>]
```

`<NODE>` is the node's slug or full UUID. Inside a gate check it defaults to
the node being checked.

`record` records the retrospective of the pass the node is finishing. It works
only while the node is in `learn`; recording again replaces what you recorded
before. It is stored for good when the node moves to `done`. Use
`--content -` and a heredoc for anything longer than a line.

`list` shows the retrospectives of the node's earlier passes, and the one
recorded for this pass so far. An earlier pass's findings are already in its
retrospective, so the work history you are given covers only this pass.
