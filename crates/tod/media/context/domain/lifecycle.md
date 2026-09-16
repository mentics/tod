# Lifecycle states

Each node sits in one **lifecycle state**, and moves forward through them in
order:

`proposed` → `design` → `planning` → `ready` → `active` → `verifying` →
`review` → `approved` → `merged` → `released` → `done`

A node does not advance on its own. Each state has a **forward gate** — a set
of criteria that must hold before the node may move to the next state — and
advancing means either passing that gate or a human waiving it explicitly.

Most states have a **state agent** responsible both for that state's own work
and for evaluating its forward gate; there is no separate orchestrator.
`ready` and `done` have no agent.

Two kinds of turn are distinct and must not be conflated:

- **On entry** — the work this state exists to do, run once the node has
  actually arrived in it.
- **Gate check** — a read-only evaluation of whether the node may leave.
