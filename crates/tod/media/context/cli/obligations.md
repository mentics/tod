## `tod-cli obligations`

Requirements and constraints on a node. Obligation ids may be given in full or
as the 8-character prefix shown in listings.

```
tod-cli --data-root <DATA_ROOT> obligations list       [--node <UUID>] [--search <TEXT>] [--kind requirement|constraint] [--inherited]
tod-cli --data-root <DATA_ROOT> obligations show       <ID>
tod-cli --data-root <DATA_ROOT> obligations add        --node <UUID> --kind requirement|constraint --body <TEXT> --phase requirements|design [--acts-in design|active|verifying|merged|released] [--code-only <REASON>] [--section <NAME>] [--after <ID>] [--before]
tod-cli --data-root <DATA_ROOT> obligations update     <ID> [--body <TEXT>] [--section <NAME>] [--phase requirements|design|unknown] [--acts-in design|active|verifying|merged|released] [--code-only <REASON> | --live] [--reason <TEXT>]
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
is `active`. Listings show it as `acts-in=<state>` when it is not.

Every obligation is **live**: verification has to show it in the running work.
`--code-only <REASON>` marks one that only the code can show (a pure function,
a refactor, a rename), so a unit test or reading the code verifies it;
`--live` clears the mark. Listings show it as `code-only (<reason>)`. Only the
design phase agent (in `design`) and the user may set or clear it; every other
agent is refused.

Who may change an obligation is set by who stands behind it (its
**authority**) and by the phase that wrote it. Listings show `(user)` for one
the user wrote or edited and `(approved)` for one the user confirmed; an
agent's own show nothing. You may never change, move or delete a user or
approved obligation, in any state. An agent's own may be changed by an agent
only until the phase that wrote it is done: a requirement (`--phase
requirements`) through `proposed`, a design obligation through `design`. From
`planning` on the app refuses every agent change to it, and the refusal says to
send the node back with `lifecycle send-back --obligation <ID> --reason <WHY>`
to dispute it (to `proposed` for a requirement, `design` for a design
obligation), so the phase that wrote it settles it. There is one exception: a verification session may `update <id> --acts-in merged|released
--reason <TEXT>` on an obligation of its own node (in `verifying`) that acts in
an earlier state, to hand it to the phase that can show it true. The `--reason`
(at least 15 characters) is required and is kept with the change: say why nothing in the checkout can
show it true. Refused when the obligation has a `failed` verdict recorded in this stay in
`verifying` or a `failed` plan step carries it: a recorded failure stays a
failure (a `failed` verdict from before the task went back to implementation
does not count). Nothing
else, and never back.

`deleted` and `history` list changes as `r-<n>`; deletions and edits stay
restorable for 30 days. `restore r-<n>` puts back the obligation as it was
before that change — a deleted one with its id and position; an edited one
with its earlier wording. With `--node` and `--by` it restores every listed
deletion by that party, newest first, so they return to their original order. A
restore is recorded like any other edit, so it can itself be restored.

Once the phase that wrote an obligation is done, or when the user stands
behind it, an agent's `add`, `update`, `move`, `delete` and `restore` on it are
refused (the user's own edits, and a freeform conversation the user directs,
are not). For an agent's own obligation, dispute it with a send-back; for a
user or approved one, propose the change with `decisions ask` instead,
quoting the obligation's id, its current text, the text you propose, and why;
if the user approves, they make it.

Obligation text can reference any node inline by slug: `[[dynamic-form]]`. A
write naming a slug no node has is refused; `check-refs` lists existing
obligations whose references are broken.
