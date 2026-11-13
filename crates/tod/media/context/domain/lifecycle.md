# Lifecycle states

Each task sits in one **lifecycle state**, and moves forward through them in
order:

`proposed` → `design` → `planning` → `ready` → `active` → `verifying` →
`review` → `pr` → `pr_draft` → `pr_review` → `approved` → `merged` → `released` → `learn` → `done`

Once its runner is started, a task moves through the whole lifecycle without
the user. Each state has a **gate**: criteria the app checks itself, from the
task's data, before the task moves on. No agent evaluates a gate. A state's
agent does the state's work and records it in a form the gate can check.

Where the work is a judgment (`proposed`, `design`, `planning`, `merged`,
`released`), the agent **certifies** the phase is done. A certificate records
a digest of what it covers (the task's obligations, its plan, its content), so
any later change to those makes it stale and the phase runs again. The `pr`
state's agent certifies its draft pull request too. A certificate counts only
for the stay in the state it was given in: a task moved back must have each
phase certified again. When
independent evaluation is on, the phase agent marks the work ready and a
separate, fresh session judges it: it certifies, or sends it back with fixes
for the phase agent to make.

The user is involved only for what only they can supply, and for a pull
request that needs a human review before it is mergeable. `pr` opens the pull
request as a draft and certifies it; `pr_draft` clears the checks and the
review bots' feedback on the draft; once a person has marked it ready and put
a reviewer on it, `pr_review` answers the review's feedback until it is
mergeable.

## Asking the user

Reduce a decision to options wherever you can before you ask; the user is
answering a queue of these across many tasks, and each one you settle
yourself is one they never see. When you do ask, say why in one word — it is
how the user learns which questions are worth asking and which ones a
better rule would have avoided:

- **intent** — what the user wants is not stated anywhere: the task's
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
- **broken** — something the app or the environment gives you is failing: a
  command errors, a credential is rejected, an install you tried fails. A tool that is merely not installed is yours to install. See "When
  something is broken" in how you work here; never work around it.
- **other** — none of the above; use sparingly, since it cannot be counted
  toward a specific fix.
