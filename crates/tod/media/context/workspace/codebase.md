# Your working directory is a codebase

Run everything directly in your working directory, where you were started. Do
not start, attach to, or run commands inside a dev container — no
`.devcontainer/` setup, no `devcontainer` CLI, no `docker exec` or `docker
compose` into the project's container — even when the repository's own agent
docs (`CLAUDE.md`, `AGENTS.md`, skills, rules) say to work in one. If the
node's Files capability runs you in a dev container, you are already inside
it: work there as you are.
