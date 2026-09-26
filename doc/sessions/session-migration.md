# Moving a conversation's agent session to another machine

Status: design, not implemented.

A conversation's agent session can run in one of three places: on this
machine, in the node's dev container, or in its cloud sandbox (the Files
capability's "Runs in"). This is how a stopped session moves from one of
those places to another and picks up where it left off. The next Send, or the
terminal handoff, resumes the same session id in the new place.

Scope: Claude sessions. Cursor keeps its sessions somewhere else, which has
not been looked into (see [Open questions](#open-questions)).

## What moves and what doesn't

| Thing | Moves? | How |
|---|---|---|
| The node's code | Yes | Through git. The source commits and pushes the node's branch, and the target fetches and checks it out. No files are copied. |
| The agent's session (its transcript) | Yes | The app copies the session's files from the source's Claude config directory to the target's. |
| tod's own data (outline, conversation, change set, verdicts, findings) | No | It stays in the app's data root. Agents reach it through `tod-cli` wherever they run: on this machine directly, and from containers and sandboxes through `fleet::cli_relay`. |
| The conversation row | No | `conversations.agent_session_id` keeps the same id. Only the node's "Runs in" changes. |
| Running processes, background shells, the agent process | No | The session must be stopped first. |
| Claude's sign-in, MCP servers, settings | No | Each place has its own. The target must already be signed in. |

**Assumption: the session owns its branch.** Nothing else commits to the
node's branch while the session is running. Because of that, pushing from the
source and fast-forwarding on the target is enough. If the branch diverged,
something broke that assumption, and the migration stops and asks the user
rather than merging.

## Where a session lives

Claude stores a session as:

```
<config>/projects/<encoded-cwd>/<session-id>.jsonl   the transcript; this is the session
<config>/projects/<encoded-cwd>/<session-id>/        subagent transcripts (subagents/) and large
                                                     tool results (tool-results/); may not exist
<config>/file-history/<session-id>/                  /rewind checkpoints; tod doesn't use these, not copied
```

- `<config>` is `$CLAUDE_CONFIG_DIR`, or else `~/.claude` for the user the
  agent runs as. `tod_agent::run_state::claude_config_dir` resolves it the
  same way for this machine.
- `<encoded-cwd>` is the directory the agent was started in, **as the agent
  sees it**, with every character that isn't a letter or digit replaced by
  `-`. For example, `C:\data\git\tod` becomes `C--data-git-tod` and
  `/workspaces/tod/.worktrees/feat-x` becomes
  `-workspaces-tod--worktrees-feat-x`.
- `claude --resume <id>` (and the ACP adapter's resume) looks only in the
  directory for its current cwd. So on the target the file has to go under
  the target cwd's encoded name, not the source's.

The cwd is the conversation protocol's `Protocol::cwd` (a `Workdir`), which is
also the directory `TerminalHandoff` uses. Where the agent runs is
`launch_environment` → `AgentEnvironment`.

| Runs in | Agent's cwd (what gets encoded) | `<config>` | Read files with | Write files with |
|---|---|---|---|---|
| This machine | Host path of the node's Files directory | `claude_config_dir()` | `std::fs` | `std::fs` |
| Dev container, repository in the container | Container path (`<repo>/.worktrees/<branch>`) | The `remoteUser`'s `$CLAUDE_CONFIG_DIR` or `~/.claude`, inside the container | `docker exec -u <user> cat` (`ContainerExec`) | `devcontainer::write_file` (writes as root), then `chown` to the `remoteUser` |
| Dev container, mounted | The host directory mapped through the container's mounts (the agent still runs **in the container**, so the session lives there, not on the host) | Same as above | Same as above | Same as above |
| Cloud sandbox | Sandbox path of the repository | The sandbox user's real home (`/root/.claude` by default; the relay's `home_dir`, not the process API's `HOME=/blaxel`) | `tod-sandbox exec … cat` | Blaxel file upload (`blaxel::upload`), or `exec` with stdin |

Get `<config>` in a container or sandbox by asking it rather than
assuming: `sh -c 'echo "${CLAUDE_CONFIG_DIR:-$HOME/.claude}"'`, run as the
agent's user.

A conversation whose protocol cwd is the data root (not inside the node's
Files directory) always runs on this machine (`launch_for`), so there's
nothing to move. Migration is offered only when the cwd is in a Files
directory.

## The process

The app runs this. None of it runs on the UI thread, because every step calls
git, Docker, or `tod-sandbox`. The UI shows progress per step and can show
the step that failed. The app is the hub: it reads from the source and writes
to the target, and the two places never talk to each other directly.

### 0. Preconditions (checked before anything changes)

- The conversation has an `agent_session_id`.
- The session isn't in use. The app closes its own session
  (`handoff::release_session`), and if a terminal handoff is still open, the
  user is asked to close it. Two writers would fork the transcript.
- The source can be reached (the container is running, or the sandbox is up)
  and the session file is there.
- The target can be reached, has `claude` (and the ACP adapter) installed and
  signed in, and can reach the git remote.
- The node's branch is known and has an `origin` remote.

### 1. Push the code on the source

In the source's Files directory, going depth-first through submodules so
each submodule is pushed before the superproject records it:

1. Every submodule with changes must be on the node's branch
   (`ensure_worktree` put it there). A detached HEAD stops the migration.
2. In each submodule, then in the superproject:
   - `git add -A` (respects `.gitignore`)
   - `git commit -m "Move session to <target>"` if anything is staged. Hooks
     run: if one fails, the migration stops with its output, and nothing is
     skipped.
   - `git push -u origin <branch>`. If the push is rejected as a
     non-fast-forward, the migration stops (the branch diverged; see the
     ownership assumption above).
3. Record the pushed commit of the superproject and each submodule. The
   target checks them in step 4.

### 2. Copy the session out

Read `<session-id>.jsonl` and, if it exists, the whole `<session-id>/`
directory from the source's `<config>/projects/<encoded source cwd>/`. Find
it by id across `projects/*` the way `find_claude_session_log` does, not by
recomputing the encoded name, so a Claude version that encodes paths
differently doesn't break the read side. Hold the files in memory or in a
temp directory under the data root.

### 3. Switch the node

Change the node's Files setting to the target ("Runs in"): on this machine,
the container (and whether it's mounted), or the sandbox. This is the same
write the task editor or `tod-cli capabilities set <node> files …` makes.
Nothing else in the node changes.

### 4. Bring the code up on the target

In the superproject, then in each submodule:

- `git fetch origin <branch>`. This matters because `ensure_worktree` creates
  the branch from `origin/<branch>` as of the **last fetch**.
- If there's no Files directory on the target yet: `ensure_worktree`. It
  creates the branch from `origin/<branch>`, initializes submodules, and puts
  them on the branch.
- If one already exists (the node ran here before), it must be clean. Then
  run `git checkout <branch>` and `git pull --ff-only`, and the same in each
  submodule after `git submodule update --init --recursive`. A dirty tree or
  a non-fast-forward stops the migration.
- Check that HEAD is the commit recorded in step 1, in the superproject and in
  every submodule.

### 5. Copy the session in

Compute the target cwd (`Protocol::cwd` again, now that the node points at
the target), encode it, and write the files to
`<target config>/projects/<encoded target cwd>/`, creating the directory. In
containers, `chown -R` the result to the agent's user. If the target already
has a file with this id, it's overwritten only when it's a prefix of the
incoming one (a stale earlier copy). Otherwise the migration stops.

Records inside the transcript keep the source's `cwd` and absolute paths.
They aren't rewritten (see [Open questions](#open-questions)).

### 6. Tell the agent, and the user

- The transcript gets a marker, like the rotation marker: "Moved to the dev
  container `<name>`". This is either a new `conversation_turns` kind
  (`migration`) or a rotation-style row with a different label.
- The next turn's delta (`conversation::context`) starts with one line saying
  the working directory is now `<target cwd>` on `<environment>` and that
  paths in earlier turns referred to `<source cwd>`.
- Record a journey `UserAction` for the move, as for other node actions.

### 7. Clean up the source

After the first turn on the target succeeds, delete the source's copy of the
session files, so a later handoff there doesn't resume a stale fork. The
source worktree is **not** removed: that stays the user's call, as it is now.

## Failure and retry

- Steps 0–2 change nothing but git history (a commit and a push), so a
  failure there leaves the node running where it was. The commit and push are
  harmless on retry.
- Step 3 is the switch. A failure in step 4 or 5 offers **Retry** (every step
  is idempotent: fetching, fast-forwarding, and writing the same files again)
  or **Move back** (switch "Runs in" back; the source still has its session
  files and its worktree at the pushed commit).
- If the session can't be resumed on the target anyway (an unsupported
  Claude version, a file in the wrong place), nothing is lost: the driver
  already rotates to a fresh session seeded with a snapshot when a resume
  fails, and says so in the transcript. Migration can rely on that as the
  last resort.

## What this depends on

These are the places that could break it. Each should have a test or a
check in `doctor`:

1. **Claude's session layout**: `projects/<encoded-cwd>/<id>.jsonl` plus the
   `<id>/` directory, and resume finding the session by the current cwd's
   encoded name. The encoding rule (non-alphanumeric → `-`) is observed, not
   documented. Very long paths may be shortened by newer versions, which
   hasn't been checked. Pin the encoder with a test against real directory
   names, and after the copy check that `claude` on the target can see the
   session (for example, that it's listed for that cwd) before switching
   over.
2. **The ACP adapter resumes from the same files** that `claude --resume`
   does. The app's turns go through `claude-code-acp`, and the terminal
   handoff goes through `claude`.
3. **`Protocol::cwd` is the same directory on both sides of a turn**: the
   cwd the transcript was written under on the source, and the one used to
   encode on the target.
4. **The agent's user and home in the container or sandbox**: the
   `remoteUser` for dev containers, and the relay's real home for sandboxes,
   not `HOME=/blaxel`.
5. **Git**: an `origin` both places can reach with credentials, the node's
   branch name being the same in both places, and `ensure_worktree`'s "create
   from `origin/<branch>` as of the last fetch" behavior.
6. **The session owns its branch** (above).
7. **tod data never moving**: `cli_relay` works from containers (Docker
   Desktop's `host.docker.internal`) and from sandboxes (the relay's
   `/tunnel`), so an agent in the target writes to the same data root.

## Open questions

- **Rewrite `cwd` inside the transcript?** It's more faithful, but it edits
  Claude's own file format. Start without it and rely on the one-line note in
  the next delta, then check whether the agent gets confused by the old
  paths.
- **Cursor**: where `cursor-agent` keeps its sessions, and whether they can
  be resumed after a move at all. Until that's known, offer migration only for
  Claude sessions.
- **Autonomous sandbox runs**: once nodes run to completion in a sandbox
  without the app, the session will be written there with no app holding it.
  Migrating *into* a sandbox works as above. Migrating *out* needs the run
  stopped first, which is something to design with that work.
- **Several conversations on one node**: each has its own session. Moving
  the node's "Runs in" affects all of them, so the migration should copy every
  conversation on the node that has a session, not just the open one.
