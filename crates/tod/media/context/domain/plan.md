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
  `verified`, `failed`, `partial`, or `blocked`. `implemented` is
  implementation's claim; `verified` and `failed` are the verification
  phase's verdict on it. A `failed` step carries a **note** saying what
  failed, and goes back to implementation as open work. `partial` means the
  step was done as far as it could go and the rest needs the user; `blocked`
  means it could not be started at all. Both carry a **reason** — `conflict`
  (obligations that cannot all hold, cited by id), `decision` (a choice the
  obligations leave open, with the options), or `access` (a secret, account,
  or permission: what is needed, and the attempt that failed for want of it)
  — and a **note** saying why the agent cannot go on until the user acts.
  The user answers through the reason: they settle the conflict, pick an
  option, or supply the access. A step with nothing for the user to do is
  not handed back: the agent does it.
- Every note a step is given is kept, oldest first; the latest supersedes the
  rest. Several notes on one step mean several attempts at it.

Plans and obligations move together. If an obligation changes, the steps
linked to it may need their body updated, their status reset, or a new step
added; if an obligation is deleted, its links need removing rather than being
left dangling.
