## `tod-cli journey`

Read a recorded journey: what happened on a node, or in the user's own recent
activity (`project`). Read-only. For what to look for, read the `journeys`
skill (`tod-cli skills show journeys`).

```
tod-cli --data-root <DATA_ROOT> journey show <TARGET> [--since <DURATION>] [--last <N>] [--full]
```

`<TARGET>` is a node slug or UUID, or `project`; `journey:node/<SLUG>` and
`journey:project` name the same. A journey can be long: use `--last` or
`--since` (`2h`, `1d`) first.
