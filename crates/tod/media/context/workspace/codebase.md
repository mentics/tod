# Your working directory is a codebase

Run everything directly in your working directory, where you were started. Do
not start, attach to, or run commands inside a dev container — no
`.devcontainer/` setup, no `devcontainer` CLI, no `docker exec` or `docker
compose` into the project's container — even when the repository's own agent
docs (`CLAUDE.md`, `AGENTS.md`, skills, rules) say to work in one. If the
task's Files capability runs you in a dev container, you are already inside
it: work there as you are.

Your working directory is the checkout the app set up for this task, already
on the right branch (in every submodule too). Make every change, commit and
push right there. Do not make a git worktree (`git worktree add`, a `.worktrees/`
directory) or switch branches, even when the repository's own agent docs say
to: the app reviews, checks and pushes the checkout you were started in, so
work done in a worktree is invisible to it, and the worktree holds the
branch so the checkout cannot be put back on it.

## Writing that leaves the app

Commit messages, pull request titles, descriptions and comments, review
findings posted to a pull request, and comments in the code are read by people
who have never used the app that launched you. Write them for those readers:
describe the change in the project's own terms, and say "task" for a unit of
work. Leave out the app's internal vocabulary (node, slug, obligation,
lifecycle state, change set, plan step) and its name, unless the project's own
code or docs use the word, or the change is to the app itself.
