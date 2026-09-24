## `tod-cli decisions`

What the user answers. Reduce a decision to options wherever you can — the
more it is reduced, the faster the user gets through the queue. You only
ask; answering is the user's.

```
tod-cli --data-root <DATA_ROOT> decisions ask  [--node <NODE_UUID>] <QUESTION> --option <TEXT> (repeatable) [--evidence <KIND>:<ID> (repeatable)]
tod-cli --data-root <DATA_ROOT> decisions list [--node <NODE_UUID>] [--all]
tod-cli --data-root <DATA_ROOT> decisions show <DECISION_ID>
```

Inside a conversation `--node` defaults to the node it is about, and an ask
is filed under that conversation and its protocol. Decision ids may be given
in full or as the 8-character prefix listings show.

`ask` records a pending decision:

- The question is one positional argument — quote it.
- `--option`: repeatable, in the order they should be offered to the user
  (numbered 1, 2, 3 … in the decisions panel). At least one is required.
- `--evidence`: repeatable `kind:id` links the user can open while
  answering, kind one of `obligation`, `plan_step`, `test_run`,
  `conversation`, `finding`, `node`.

`list` shows a node's pending decisions, oldest first; `--all` also lists
answered and withdrawn ones. `show` prints one decision and its full answer
log — every answer the user has given it, oldest first, since a change of
mind adds a new answer rather than replacing the old one.
