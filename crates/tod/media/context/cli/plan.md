## `tod-cli plan`

```
tod-cli --data-root <DATA_ROOT> plan list      --node <NODE_UUID>
tod-cli --data-root <DATA_ROOT> plan show      <STEP_ID>
tod-cli --data-root <DATA_ROOT> plan add       --node <NODE_UUID> --body <TEXT> [--after <STEP_ID>] [--before] [--depends-on <STEP_ID>] [--satisfies <OBLIGATION_ID>]
tod-cli --data-root <DATA_ROOT> plan update    <STEP_ID> [--body <TEXT>] [--status pending|ready|in_progress|implemented|verified|blocked]
tod-cli --data-root <DATA_ROOT> plan delete    <STEP_ID>
tod-cli --data-root <DATA_ROOT> plan depend    <STEP_ID> --on <STEP_ID>
tod-cli --data-root <DATA_ROOT> plan undepend  <STEP_ID> --on <STEP_ID>
tod-cli --data-root <DATA_ROOT> plan satisfy   <STEP_ID> --obligation <OBLIGATION_UUID>
tod-cli --data-root <DATA_ROOT> plan unsatisfy <STEP_ID> --obligation <OBLIGATION_UUID>
tod-cli --data-root <DATA_ROOT> plan ready     --node <NODE_UUID>
```

Step and obligation ids may be given in full or as the 8-character prefix
shown in listings. `satisfy`/`unsatisfy` link a step to the requirement or
constraint it fulfills — use `obligations list --node <NODE_UUID>` to find the
obligation id if you weren't given it. `ready` lists the steps eligible to
start now (status `ready`, or `pending` with every dependency
`implemented`/`verified`).

If you change an obligation and existing plan steps depend on it or claim to
satisfy it, check `plan list --node <NODE_UUID>` for steps whose `satisfies`
links point at it — they may need their body updated, a new step added, or
their status reset to `pending` so the change gets picked up downstream.
