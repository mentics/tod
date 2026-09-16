## `tod-cli questions`

Interview questions on a node. Questions are `q-<n>`, numbered per node.

```
tod-cli --data-root <DATA_ROOT> questions list      --node <UUID> [--status open|answered|deferred|withdrawn]
tod-cli --data-root <DATA_ROOT> questions show      --node <UUID> <q-N>
tod-cli --data-root <DATA_ROOT> questions add       --node <UUID> [--session <UUID>] [--phase requirements|design|planning]
tod-cli --data-root <DATA_ROOT> questions withdraw  --node <UUID> <q-N> --reason <TEXT>
tod-cli --data-root <DATA_ROOT> questions processed --node <UUID> <q-N> --summary <TEXT>
```

`add` takes the question as YAML on stdin, with the fields `question`,
`context`, `options`, `recommend`, `proposal`, `intent`, and `covers`.
