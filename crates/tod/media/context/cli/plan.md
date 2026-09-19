## `tod-cli plan`

```
tod-cli --data-root <DATA_ROOT> plan list      [--node <NODE_UUID>] [--search <TEXT>]
tod-cli --data-root <DATA_ROOT> plan show      <STEP_ID>
tod-cli --data-root <DATA_ROOT> plan add       --node <NODE_UUID> --body <TEXT> [--after <STEP_ID>] [--before] [--depends-on <STEP_ID>] [--satisfies <OBLIGATION_ID>]
tod-cli --data-root <DATA_ROOT> plan update    <STEP_ID> [--body <TEXT>] [--status pending|ready|in_progress|implemented|verified|failed|partial|blocked] [--reason conflict|decision|access] [--why <TEXT>] [--did <TEXT>] [--cites <OBLIGATION_ID>]... [--option <TEXT>]... [--needs <TEXT>] [--tried <TEXT>] [--note <TEXT>]
tod-cli --data-root <DATA_ROOT> plan delete    <STEP_ID>
tod-cli --data-root <DATA_ROOT> plan depend    <STEP_ID> --on <STEP_ID>
tod-cli --data-root <DATA_ROOT> plan undepend  <STEP_ID> --on <STEP_ID>
tod-cli --data-root <DATA_ROOT> plan satisfy   <STEP_ID> --obligation <OBLIGATION_UUID>
tod-cli --data-root <DATA_ROOT> plan unsatisfy <STEP_ID> --obligation <OBLIGATION_UUID>
tod-cli --data-root <DATA_ROOT> plan ready     --node <NODE_UUID>
```

`list` needs `--node` or `--search`. Without `--node` it searches every node's
plan steps, best match first, and each line names the step's node as
`on <slug>`.

Step and obligation ids may be given in full or as the 8-character prefix
shown in listings. `satisfy`/`unsatisfy` link a step to the requirement or
constraint it fulfills — use `obligations list --node <NODE_UUID>` to find the
obligation id if you weren't given it. `ready` lists the steps eligible to
start now (status `ready`, or `pending` with every dependency
`implemented`/`verified`).

`--status partial` and `--status blocked` hand the step to the user, so each
requires `--reason` and `--why`, and the reason names what the user does.
`--why` is why you cannot go on until they do (use `--why -` and a heredoc
for anything long). The reason is one of:

- `conflict` — obligations that cannot all hold. Give `--cites` for each of
  them, two or more (repeat the flag, or separate ids with commas).
- `decision` — a choice the obligations leave open that is not yours to make.
  Give `--option` for each choice you see, two or more; the user picks one.
- `access` — a secret, account, or permission you do not have. Give `--needs`,
  what the user must supply, and `--tried`, the command you ran and the error
  it gave.

`partial` also requires `--did`, what you changed or built; a step with
nothing done is `blocked`, and takes no `--did`. If there is nothing for the
user to do, there is nothing to hand back.

For example:

```
tod-cli --data-root <DATA_ROOT> plan update <STEP_ID> --status blocked --reason decision --option "Keep the generic form" --option "Build the Linear-specific form" --why -
```

`--status failed` is verification's verdict that a step is not done. It
requires `--note` — what failed and the evidence (the command run, what it
printed, what was expected) — and takes no `--reason`:

```
tod-cli --data-root <DATA_ROOT> plan update <STEP_ID> --status failed --note -
```

Setting any other status clears the reason and note. `list` and `show` print
a step's current reason and note on the lines below it. Every note a step has
been given is kept, and `show` lists them all, oldest first, each with its
time and the status it came with — a step with several `failed` notes has
failed verification several times.

If you change an obligation and existing plan steps depend on it or claim to
satisfy it, check `plan list --node <NODE_UUID>` for steps whose `satisfies`
links point at it — they may need their body updated, a new step added, or
their status reset to `pending` so the change gets picked up downstream.
