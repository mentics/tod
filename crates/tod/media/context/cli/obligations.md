## `tod-cli obligations`

Requirements and constraints on a node. Obligation ids may be given in full or
as the 8-character prefix shown in listings.

```
tod-cli --data-root <DATA_ROOT> obligations list       [--node <UUID>] [--search <TEXT>] [--kind requirement|constraint] [--inherited]
tod-cli --data-root <DATA_ROOT> obligations show       <ID>
tod-cli --data-root <DATA_ROOT> obligations add        --node <UUID> --kind requirement|constraint --body <TEXT> --phase requirements|design [--acts-in design|active|verifying|merged|released] [--section <NAME>] [--after <ID>] [--before]
tod-cli --data-root <DATA_ROOT> obligations update     <ID> [--body <TEXT>] [--section <NAME>] [--phase requirements|design|unknown] [--acts-in design|active|verifying|merged|released]
tod-cli --data-root <DATA_ROOT> obligations move       <ID> --node <UUID>
tod-cli --data-root <DATA_ROOT> obligations delete     <ID>
tod-cli --data-root <DATA_ROOT> obligations deleted    --node <UUID> [--by user|agent|<SESSION>]
tod-cli --data-root <DATA_ROOT> obligations history    <ID>
tod-cli --data-root <DATA_ROOT> obligations restore    <r-N>... | --node <UUID> --by user|agent|<SESSION>
tod-cli --data-root <DATA_ROOT> obligations check-refs [--node <UUID>]
```

`list` needs `--node` or `--search`. Without `--node` it searches every node's
obligations, best match first, and each line names the obligation's node as
`on <slug>` — use this to find duplicates, contradictions, and references
anywhere in the project before or after a change. `--inherited` needs `--node`.

`add` appends to the end of its kind group by default. Pass `--after` to place
it after a specific obligation, and add `--before` to place it before that one
instead. Obligation text with no words is refused. On `update`, `--section ""`
clears the section.

An obligation has two phases, set by two flags: `--phase` is the state it was
**introduced** in and `--acts-in` is the state whose agent **acts on** it.
The obligations concepts say how to set each. `--acts-in` is left off when it
is `active`. Listings show it as `acts-in=<state>` when it is not. Inside an interview, an agent's `add` always writes its own session's phase —
`--phase` there only matters when running `add` outside an interview.

Past `planning` the app refuses every obligation change from an agent, with one
exception: a verification session may `update <id> --acts-in merged|released`
on an obligation of its node (in `verifying`) that acts in an earlier state, to
hand it to the phase that can show it true. Nothing else, and never back.

`deleted` and `history` list changes as `r-<n>`; deletions and edits stay
restorable for 30 days. `restore r-<n>` puts back the obligation as it was
before that change — a deleted one with its id and position; an edited one
with its earlier wording. With `--node` and `--by` it restores every listed
deletion by that party, newest first, so they return to their original order. A
restore is recorded like any other edit, so it can itself be restored.

Past `planning` the obligations are settled: on a node whose lifecycle
state is later than `planning`, an agent's `add`, `update`, `move`, `delete`
and `restore` are refused (the user's own edits, and a freeform conversation
the user directs, are not). Propose the change with `decisions ask` instead,
quoting the obligation's id, its current text, the text you propose, and why;
if the user approves, they make it.

Obligation text can reference any node inline by slug: `[[dynamic-form]]`. A
write naming a slug no node has is refused; `check-refs` lists existing
obligations whose references are broken.
