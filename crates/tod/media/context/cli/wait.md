## `tod-cli wait`

What a node waits on between sessions. Never wait inside a session (no
`sleep`, no polling loop): record the wait and end your turn. The node is
woken when the wait is due, and its next session is told what it was waiting
on. To ask a person, use `decisions ask` instead.

```
tod-cli --data-root <DATA_ROOT> wait add        [--node <NODE_UUID>] --until <TIME>
tod-cli --data-root <DATA_ROOT> wait add        [--node <NODE_UUID>] --event <SOURCE>:<MATCH> [--deadline <TIME>]
tod-cli --data-root <DATA_ROOT> wait add        [--node <NODE_UUID>] --check <COMMAND> --every <DURATION>
tod-cli --data-root <DATA_ROOT> wait list       [--node <NODE_UUID>] [--all]
tod-cli --data-root <DATA_ROOT> wait show       <WAIT_ID>
tod-cli --data-root <DATA_ROOT> wait satisfy    <WAIT_ID>
tod-cli --data-root <DATA_ROOT> wait cancel     <WAIT_ID>
tod-cli --data-root <DATA_ROOT> wait reschedule <WAIT_ID> --at <TIME>
```

`add` may be left out: `tod-cli wait --until 2h`. Inside a conversation
`--node` defaults to the node it is about. Wait ids may be given in full or
as the 8-character prefix listings show.

- `<TIME>` is RFC 3339 (`2026-09-27T09:00:00Z`) or a duration from now
  (`+2h` or `2h`); `<DURATION>` is `30s`, `5m`, `2h`, `1d`, or plain seconds.
- `--until`: a timer, e.g. until a usage limit resets.
- `--event`: a webhook, e.g. `github:pr 123 checks`. `--deadline` (default
  24h) is when to give up on the webhook and check directly.
- `--check`: a shell command polled every `<DURATION>` until it exits 0, for
  something that sends no webhook. Quote it.

`list` shows the node's pending waits, soonest first; `--all` also lists
satisfied, cancelled, and expired ones. `satisfy` and `cancel` close a wait
you no longer need; `reschedule` moves a pending wait's next time.
