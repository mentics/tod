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

## Asking the user

Reduce a decision to options wherever you can before you ask; the user is
answering a queue of these across many nodes, and each one you settle
yourself is one they never see. When you do ask, say why in one word — it is
how the user learns which questions are worth asking and which ones a
better rule would have avoided:

- **missing_rule** — no obligation, plan step, or process doc settles this;
  the gap itself is worth fixing.
- **conflict** — obligations or other requirements cannot all hold as
  written.
- **access** — a secret, account, or permission you do not have.
- **risk** — a judgment call worth a human's sign-off, not a missing rule
  or an outright conflict (a review finding or a gate blocker you are
  raising rather than deciding).
- **capability** — about a capability's own configuration.
- **other** — none of the above; use sparingly, since it cannot be counted
  toward a specific fix.
