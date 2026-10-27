# Checkpoints: keeping a sandbox's uncommitted work

A cloud sandbox expires on its own (Blaxel), and anything uncommitted in it
goes with it. A **checkpoint** keeps that work on the remote, under a hidden
ref, so a node whose sandbox is made again gets it back. Code:
`tod_store::fleet::checkpoint` (git), `fleet::node_checkpoint` (per node),
`tod_core::checkpoint` (when).

## What is saved

- A commit built from a temporary index, so the node's branch, index, and
  working tree are never touched. Its parent is `HEAD`, so commits not yet
  pushed travel with it.
- Edits, new files, and deletions. **Never** what `.gitignore` covers (the
  agent scratchpad, build output).
- Pushed, forced, to `refs/tod/checkpoints/<node>/root` (a submodule:
  `…/<node>/sub/<path>`, on the submodule's own remote). Not a branch or tag:
  no pull request, branch list, or default clone shows it, and the node's
  branch history is unchanged.

## The size gate

Checked before anything is staged; a refused checkpoint pushes nothing and
fails naming the files (shown in the task list's status line):

| Limit | Default |
|---|---|
| any one file | 5 MiB |
| any one binary file (a NUL byte in it: archives, images, artifacts) | 1 MiB |
| all changed files together | 50 MiB |

Remove or ignore the files and the next checkpoint goes through. The limits
are `checkpoint::Limits::default()`, not yet a setting.

## When

`tod_core::checkpoint::Tracker`, fed by the unified view's standings pass:

- a node **turns to `You`** (request, failure, stop, pause): its agent has
  just finished, so the sandbox is awake and the push costs no wake-up;
- a node that stays **`Working`**: every 15 minutes, against a crash;
- never for `Others` or `Done`, and not at app start for nodes already
  waiting (waking every parked sandbox would cost more than it saves).

Only nodes with a sandbox are touched; a push is skipped when the work is
the same as the last checkpoint.

## Restore

When a node's sandbox is made (including again, after it expired), the
checkpoint is fetched, the branch is moved up to the checkpoint's parent if
that is ahead (unpushed commits), and the changes are applied as
**uncommitted** edits. A checkpoint that does not apply (the branch moved on)
leaves the checkout clean, says so in the files warnings, and stays on the
remote.

## Cleanup

- **Superseded**: the branch push at a step boundary deletes the node's
  refs when the checkout is clean afterwards.
- **Discard** (the Files dialog) and **removing the location** delete them.
- **Sweep**: every 12 hours (first one 5 minutes after start), refs of nodes
  that no longer exist or are past `merged` are deleted, found through one
  checkout per distinct Files settings. A node waiting for its sandbox to be
  made again is neither, so its checkpoint stays.

Objects stay on GitHub until the ref is deleted and GitHub collects them;
push protection and secret scanning apply to the push.
