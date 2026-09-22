## `tod-cli incoming`

Changes a node inherits and has not been checked against yet: an ancestor's
constraint added, reworded, or deleted since the node committed to its design
and plan. The app reads your verdict here, not your reply.

```
tod-cli --data-root <DATA_ROOT> incoming list     [<NODE>]
tod-cli --data-root <DATA_ROOT> incoming resolve  [<NODE>] --affects none|plan|obligations --note <TEXT>
```

`<NODE>` is the node's slug or full UUID. Inside an incoming-changes check it
defaults to the node being checked.

`list` shows the node's pending changes, netted per item: each with where it
was made, how it reached this node (through an ancestor or a reference), and
its text before and after.

`resolve` records one verdict on all of them together:

- `none`: nothing of this node's own obligations or plan is affected.
- `plan`: the obligations still hold, but some plan steps no longer do. The
  node will be sent back to `planning`.
- `obligations`: an obligation of this node must be added, changed, or
  removed. The node will be sent back to `design`.

`--note` says why, in a sentence or two, naming the obligation or plan step
concerned. It opens the node's next pass when it goes back, so write it for
the person who will rework the node. Resolving clears the changes you were
shown; one that arrived since stays pending for the next check.
