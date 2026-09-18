# Plan steps

A **plan** is the structured breakdown of how one node's obligations get
implemented. Plan steps are created during that node's `planning` state, one
node at a time.

- A step's **ordinal** is display order only. Real execution order comes from
  its **depends-on** links, which form a dependency graph.
- A step may **satisfy** one or more obligations. That link is what connects
  the plan back to what the node committed to, and it is how coverage is
  judged — an obligation no step satisfies is an obligation nothing is being
  done about.
- A step has a status: `pending`, `ready`, `in_progress`, `implemented`,
  `verified`, `partial`, or `blocked`. `partial` means the step was done as
  far as it could go and the rest needs the user; `blocked` means it could not
  be started at all. Both carry a **reason** — `conflict` (obligations that
  cannot all hold, cited by id), `decision` (a choice the obligations leave
  open, with the options), `access` (a secret, account, or permission), or
  `external` (waiting on something outside the node) — and a **note**: what
  is left, and how the user can unblock it. The user answers through the
  reason: they settle the conflict, pick an option, or supply the access.

Plans and obligations move together. If an obligation changes, the steps
linked to it may need their body updated, their status reset, or a new step
added; if an obligation is deleted, its links need removing rather than being
left dangling.
