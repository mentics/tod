## `tod-cli lifecycle`

Moves a node's lifecycle state. **For the operator only: it is refused in an
agent session** (whenever `TOD_INTERVIEW_ACTOR`, `TOD_IMPLEMENT_NODE` or
`TOD_IMPLEMENT_CONVERSATION` is set). A node's agent never moves its own node;
the lifecycle process does.

```
tod-cli --data-root <DATA_ROOT> lifecycle set <NODE> <STATE> --reason <TEXT>
```

`<NODE>` is the node's slug or full UUID; `<STATE>` is one of the lifecycle
states.

`set` does what the task panel's state chip does: the node moves at once,
bypassing the gates, and the change shows on its activity as the user's with
the reason. A move back clears what certified the states being redone and
reopens verified verdicts. `--reason` is required. Moving into `pr_review` or
any later state is refused: marking the pull request ready and choosing a
reviewer are the user's step.
