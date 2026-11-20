## `tod-cli lifecycle`

```
tod-cli --data-root <DATA_ROOT> lifecycle send-back --to <STATE> --reason <TEXT> [--evidence <KIND>:<ID>]... [--obligation <ID>]...
```

`send-back` sends the node this session is about back to an earlier state,
to the phase whose work was wrong. Only a phase agent, an evaluator, or a
verification, review or fix session may use it. The target must be earlier
than the node's state and before `pr_review`.

- `--reason`: what is wrong and why that phase must fix it. That phase's
  agent starts from it, with the evidence, the state it came from and who
  sent it.
- `--evidence`: what shows it (`obligation`, `plan_step`, `test_run`,
  `conversation`, `finding` or `node`, then `:` and its id); repeatable.
- `--obligation`: an obligation the reason is about; repeatable. Required
  for `--to verifying`, which reopens only those obligations' verdicts. At
  `active` or earlier the implementation steps and every verdict reopen.

Then end your turn: the node is in the other phase. The same move again
while the obligations, plan and design are unchanged, or a third send-back
to the same state since the user last acted, is refused and stops the run
for the user.

`lifecycle set <NODE> <STATE> --reason <TEXT>` is the operator's own move
and is refused in an agent session.
