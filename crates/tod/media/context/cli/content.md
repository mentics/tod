## `tod-cli content`

A node's goal, design, plan, and generated summary.

```
tod-cli --data-root <DATA_ROOT> content get --node <UUID> --type goal|design|plan|summary
tod-cli --data-root <DATA_ROOT> content set --node <UUID> --type goal|design|plan|summary --body <TEXT> [--append]
```

`summary` is regenerated (overwritten, not appended) once on entering `design`
and once on entering `planning` — see the design and planning state docs'
On-entry steps. Don't hand-edit it expecting the edit to survive.
