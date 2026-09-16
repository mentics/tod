## `tod-cli memory`

Interview memory on a node. Notes are `m-<n>`, numbered per node.

```
tod-cli --data-root <DATA_ROOT> memory list   --node <UUID> [--kind context|handoff|parked|plan] [--status open|done]
tod-cli --data-root <DATA_ROOT> memory add    --node <UUID> --kind context|handoff|parked|plan --body <TEXT> [--phase requirements|design|planning] [--question <q-N>]
tod-cli --data-root <DATA_ROOT> memory update --node <UUID> <m-N> [--body <TEXT>] [--status open|done]
```

This is how the interview keeps what isn't an obligation, and how the two
interview agents pass context to each other. **It is the only memory
available** — do not use any memory feature of your own agent platform (memory
files, saved notes): nothing written there reaches the app, the other agent, or
a fresh session.

Keep every note short and specific: one fact or request per note. Update or
close an existing note rather than adding a near-duplicate.
