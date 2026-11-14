# Hard rules (every shepherd, and the manager)

- Use the **default live root**: the `data_root` in `install.toml`. Read it,
  never write it. Pass that root as `--data-root` on every launch and every
  `tod-cli` call.
- Read its database read-only (`sqlite3 'file:<root>/tod.db?mode=ro'`). Change
  node state only through `tod-cli` (CLI shepherds) or the app's control
  socket (the UI shepherd). The one file you write in the root is the
  shepherd ledger, and only through `ledger.py`.
- Only the nodes you hold (a shepherd) or that are in scope (the manager).
  A node outside them can still show a tod gap: report it, but don't resume,
  answer or reset it.
- Never move a node to `pr_review`. Never mark a PR ready or request a
  reviewer: that is the user's step.
- Never approve, merge or release a PR, and never move a node forward past a
  gate with the lifecycle chip or `tod-cli lifecycle set`. A person's review,
  the merge and the release are the only stops after `pr_draft`; everything
  else is tod's to do, and yours to make tod do.
- Moving a node **back** to an earlier state (`tod-cli lifecycle set <node>
  <state> --reason …`), rerunning a phase, resuming, and resetting or
  deleting its environment are your calls. Don't ask the user for them.
- Never push to, comment on, or edit a PR or branch yourself. The node's
  agent does that, as the product should. Reading with `gh` is fine.
- Never label a node sandbox `keep-when-idle=true`. Letting idle node
  sandboxes be pruned is deliberate; a pruned node gets a new sandbox when it
  is resumed.
- A change to another repository goes on a branch and waits for the user's
  OK to push and open a PR.
- A node the user put on hold (`ledger.py hold`) is theirs: never act on it.
