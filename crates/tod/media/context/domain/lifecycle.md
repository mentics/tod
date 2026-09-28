# Lifecycle states

Each node sits in one **lifecycle state**, and moves forward through them in
order:

`proposed` → `design` → `planning` → `ready` → `active` → `verifying` →
`review` → `pr` → `approved` → `merged` → `released` → `learn` → `done`

Once its runner is started, a node moves through the whole lifecycle without
the user. Each state has a **gate**: criteria the app checks itself, from the
node's data, before the node moves on. No agent evaluates a gate. A state's
agent does the state's work and records it in a form the gate can check.

Where the work is a judgment (`proposed`, `design`, `planning`, `merged`,
`released`), the agent **certifies** the phase is done. A certificate records
a digest of what it covers (the node's obligations, its plan, its content), so
any later change to those makes it stale and the phase runs again. When
independent evaluation is on, the phase agent marks the work ready and a
separate, fresh session judges it: it certifies, or sends it back with fixes
for the phase agent to make.

The user is involved only for what only they can supply, and for a pull
request that needs a human review before it is mergeable.

## Asking the user

Reduce a decision to options wherever you can before you ask; the user is
answering a queue of these across many nodes, and each one you settle
yourself is one they never see. When you do ask, say why in one word — it is
how the user learns which questions are worth asking and which ones a
better rule would have avoided:

- **intent** — what the user wants is not stated anywhere: the node's
  purpose or scope, or a preference only they can give. Usually free text.
- **missing_rule** — no obligation, plan step, or process doc settles this;
  the gap itself is worth fixing.
- **conflict** — obligations or other requirements cannot all hold as
  written.
- **access** — a secret, account, or permission you do not have.
- **risk** — a judgment call worth a human's sign-off, not a missing rule
  or an outright conflict (a review finding or a blocker you are
  raising rather than deciding).
- **capability** — about a capability's own configuration.
- **other** — none of the above; use sparingly, since it cannot be counted
  toward a specific fix.
