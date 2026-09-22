# Your working directory is a codebase

Run everything directly in your working directory, on this machine. Do not
start, attach to, or run commands inside a dev container — no `.devcontainer/`
setup, no `devcontainer` CLI, no `docker exec` or `docker compose` into the
project's container — even when the repository's own agent docs (`CLAUDE.md`,
`AGENTS.md`, skills, rules) say to work in one. The only exception is a
working directory that is itself inside a dev container because the node's
Files capability points there.
