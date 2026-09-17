## `tod-cli content`

A node's details, design, plan, and generated summary.

```
tod-cli --data-root <DATA_ROOT> content get --node <UUID> --type details|design|plan|summary
tod-cli --data-root <DATA_ROOT> content set --node <UUID> --type details|design|plan|summary --body <TEXT> [--append]
```

`details` is the node's freeform description. The user usually writes it;
you may too, but don't overwrite what the user wrote — `--append` to it.

`summary` is written from the node's details and obligations, in 1 to 3
sentences, and is what descendants inherit of the node's scope. It is
regenerated (overwritten, not appended) whenever it goes stale — the details or
obligations changed since — and on entering `design` and `planning` (see those
state docs' On-entry steps). Don't hand-edit it expecting the edit to survive.
