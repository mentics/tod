# Capabilities

A task can have **capabilities** enabled on it — optional behaviours that are
off by default and turned on per task:

- **Agent** — allows agents to be configured and run against this task. A task
  without it has no agent surface at all.
- **Files** — binds the task to a repository and working directory (a worktree,
  when one is set up), so work on it has somewhere to land.
- **Spec** — gives the task obligations and a generated `summary` of its
  details and obligations, which is what descendants inherit of its scope
  (along with its constraints).

(Others exist — `Lifecycle`, `Tags` — but these three are the ones that change
what an agent can do. `Lifecycle config` lists the skills the user chose for each
phase, inherited from the nearest task that sets them; when it applies to you,
your message has a Skills section. `Groups` only changes how the tree shows a
task's children — under a header per lifecycle state — and requires them to have
Lifecycle.)

Gate criteria frequently refer to capabilities: a task cannot reach `active`
without Agent and Files, for instance. Treat an absent capability as a real
blocker, not something to work around.
