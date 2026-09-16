## `tod-cli obligations`

```
tod-cli --data-root <DATA_ROOT> obligations list       --node <NODE_UUID> [--kind requirement|constraint] [--inherited]
tod-cli --data-root <DATA_ROOT> obligations show       <OBLIGATION_UUID>
tod-cli --data-root <DATA_ROOT> obligations add        --node <NODE_UUID> --kind requirement|constraint --body <TEXT> [--after <OBLIGATION_UUID>] [--before] [--attention low|medium|high --why <TEXT>]
tod-cli --data-root <DATA_ROOT> obligations update     <OBLIGATION_UUID> [--body <TEXT>] [--attention low|medium|high --why <TEXT>]
tod-cli --data-root <DATA_ROOT> obligations move       <OBLIGATION_UUID> --node <NODE_UUID>
tod-cli --data-root <DATA_ROOT> obligations delete     <OBLIGATION_UUID>
tod-cli --data-root <DATA_ROOT> obligations deleted    --node <NODE_UUID> [--by user|agent|<SESSION_ID>]
tod-cli --data-root <DATA_ROOT> obligations history    <OBLIGATION_UUID>
tod-cli --data-root <DATA_ROOT> obligations restore    <r-N>... | --node <NODE_UUID> --by user|agent|<SESSION_ID>
tod-cli --data-root <DATA_ROOT> obligations check-refs [--node <NODE_UUID>]
```

`add` appends to the end of its kind group by default. Pass `--after` to place
it after a specific obligation, and add `--before` to place it before that one
instead. Obligation text with no words is refused.

What you write through `tod-cli` gets `agent` provenance. When you write or
change one, give `--attention` (how likely the user is to change it) with a
one-line `--why`. `move` keeps provenance.

Deleting or rewording an obligation keeps the version it replaced for 30 days.
`deleted` lists a node's obligations that are gone but restorable, and
`history` lists one obligation's earlier versions (it also accepts the id of a
deleted one). Both list changes as `r-<n>`. `restore r-<n>` puts back the
obligation as it was before that change: a deleted one returns with its id,
position, section, phase and marks, and an edited one gets its earlier wording
back. Pass several `r-<n>`, or `--node` with `--by`, to restore a batch;
deletions come back newest first, so they return to their original order. A
restore can itself be restored: it is recorded like any other edit.

Obligation text can reference any node inline by slug: `[[dynamic-form]]`. A
write naming a slug no node has is refused; `check-refs` lists existing
obligations whose references are broken.
