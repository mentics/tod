## `tod-cli obligations`

Requirements and constraints on a node. Obligation ids may be given in full or
as the 8-character prefix shown in listings.

```
tod-cli --data-root <DATA_ROOT> obligations list       --node <UUID> [--kind requirement|constraint] [--inherited] [--search <TEXT>]
tod-cli --data-root <DATA_ROOT> obligations show       <ID>
tod-cli --data-root <DATA_ROOT> obligations add        --node <UUID> --kind requirement|constraint --body <TEXT> --phase requirements|design [--section <NAME>] [--after <ID>] [--before] [--attention low|medium|high --why <TEXT>]
tod-cli --data-root <DATA_ROOT> obligations update     <ID> [--body <TEXT>] [--section <NAME>] [--phase requirements|design|unknown] [--attention low|medium|high --why <TEXT>]
tod-cli --data-root <DATA_ROOT> obligations move       <ID> --node <UUID>
tod-cli --data-root <DATA_ROOT> obligations delete     <ID>
tod-cli --data-root <DATA_ROOT> obligations deleted    --node <UUID> [--by user|agent|<SESSION>]
tod-cli --data-root <DATA_ROOT> obligations history    <ID>
tod-cli --data-root <DATA_ROOT> obligations restore    <r-N>... | --node <UUID> --by user|agent|<SESSION>
tod-cli --data-root <DATA_ROOT> obligations check-refs [--node <UUID>]
```

`add` appends to the end of its kind group by default. Pass `--after` to place
it after a specific obligation, and add `--before` to place it before that one
instead. Obligation text with no words is refused. On `update`, `--section ""`
clears the section.

Inside an interview, an agent's `add` always writes its own session's phase —
`--phase` there only matters when running `add` outside an interview.

What you write through `tod-cli` gets `agent` provenance, and listings mark
obligations nobody has confirmed as `<agent, attention: reason>`. When you
write or change one, give `--attention` (how likely the user is to change it)
with a one-line `--why`. `move` keeps provenance.

`deleted` and `history` list changes as `r-<n>`; deletions and edits stay
restorable for 30 days. `restore r-<n>` puts back the obligation as it was
before that change — a deleted one with its id, position, and marks; an edited
one with its earlier wording. With `--node` and `--by` it restores every listed
deletion by that party, newest first, so they return to their original order. A
restore is recorded like any other edit, so it can itself be restored.

Obligation text can reference any node inline by slug: `[[dynamic-form]]`. A
write naming a slug no node has is refused; `check-refs` lists existing
obligations whose references are broken.
