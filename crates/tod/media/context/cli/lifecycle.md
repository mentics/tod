## `tod-cli lifecycle`

```
tod-cli --data-root <DATA_ROOT> lifecycle send-back --to <STATE> --reason <TEXT> [--evidence <KIND>:<ID>]... [--obligation <ID>]...
```

`send-back` sends the node this session is about back to an earlier state,
to the phase whose work was wrong. Only a phase agent, an evaluator, or an
implementation, verification, review or fix session may use it. The target
must be earlier than the node's state and before `pr_review`.

- `--reason`: what is wrong and why that phase must fix it. That phase's
  agent starts from it, with the evidence, the state it came from and who
  sent it.
- `--evidence`: what shows it (`obligation`, `plan_step`, `test_run`,
  `conversation`, `finding` or `node`, then `:` and its id); repeatable.
- `--obligation`: an obligation the reason is about; repeatable. Required
  for `--to verifying`, which reopens only those obligations' verdicts. At
  `active` or earlier the implementation steps and every verdict reopen.
  For any other target it makes the send-back a **dispute** of those
  obligations: they must be agent-written (never the user's or an approved
  one) and the target must be the phase that wrote them (`design` for a
  design obligation, `proposed` for a requirement). An implementation
  session may send back only this way.

Then end your turn: the node is in the other phase. The same move again
while the obligations, plan and design are unchanged, or a third send-back
to the same state since the user last acted, is refused and stops the run
for the user.

```
tod-cli --data-root <DATA_ROOT> lifecycle resolve-dispute --as <fixed|upheld|passed-up> --note <TEXT> [--send-back <ID>]
```

`resolve-dispute` is for the phase agent a dispute was sent to. It settles
the open dispute for the node's current state: `fixed` (a disputed
obligation was changed or removed; refused when none was), `upheld` (the
obligation stands, and `--note` says how it is meant), or `passed-up`
(`design` only: the dispute goes on to `proposed`, carrying the chain).
`--note` is required. You cannot record the phase ready or certify it while
a dispute sent to it is open.

`lifecycle set <NODE> <STATE> --reason <TEXT>` is the operator's own move
and is refused in an agent session.

`lifecycle workflow <COMMAND>` (list, publish, assign, status, resume,
cancel and the rest; `lifecycle workflow help` lists them) authors, assigns
and runs saved lifecycle workflows. It is the operator's own and is refused
in an agent session, so never run it.
