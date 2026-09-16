# Capabilities

A node can have **capabilities** enabled on it — optional behaviours that are
off by default and turned on per node:

- **Agent** — allows agents to be configured and run against this node. A node
  without it has no agent surface at all.
- **Files** — binds the node to a repository and working directory (a worktree,
  when one is set up), so work on it has somewhere to land.
- **Spec** — gives the node a stated `goal`, which descendants inherit as
  purpose.

(Others exist — `Lifecycle`, `Tags` — but these three are the ones that change
what an agent can do.)

Gate criteria frequently refer to capabilities: a node cannot reach `active`
without Agent and Files, for instance. Treat an absent capability as a real
blocker, not something to work around.
