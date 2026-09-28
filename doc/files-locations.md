# Files: inherited settings, per-node locations

## The problem

The Files capability used to be one place: a workspace directory, maybe a
worktree set up by a button, maybe a cloud sandbox chosen or created by
name. Descendants inherited that one place. With a sandbox, every node under
the capability shared a single machine, and a sandbox that was never created
("Create and use it") left every descendant with "Choose the cloud sandbox".

## The model

A capability that **inherits** holds *settings* on the node that enables it
(the source), and every node below it that does not enable the capability
itself uses them. When the settings describe something each node needs its
own copy of, the copy is **derived per-node state**: stored on the node,
marked with where it came from, and never mistaken for an override.

For Files:

- **Settings** (`node_files` on the source node): the workspace directory,
  where it runs (this machine, a dev container, cloud sandboxes), whether
  each node gets a worktree, and for sandboxes what each starts from (an
  image, or a fork of a workspace sandbox).
- **Location** (`node_files_locations`, one row per node): the worktree or
  sandbox made for that node, with `source_node_id` (whose settings made
  it) and `recipe` (a fingerprint of those settings,
  `files_location::recipe_key`). It is machine-local and never synced.
- A node's location is **current** when its source and fingerprint match
  the settings the node resolves to now, and **stale** otherwise. A stale
  location is never used; a launch refuses it until it is removed.
  Reverting the settings makes it current again.
- A node's **branch** is its own (`node_fields.branch`), `task/<slug>` by
  default, recorded when its location is made. Every node with a location is
  on a unique branch.
- A node with no worktree or sandbox shares the workspace directory, as
  before.

## Made on first use

Nothing asks the user to set up a worktree or create a sandbox. The first
time a node needs its files — an agent turn, a shell, a code editor, a
background run — `provision::resolve_launch_cwd` makes its location:

- a worktree (`ensure_worktree`, on this machine or in the dev container),
  on the node's branch; nodes on the same branch share it;
- a sandbox from the image or as a fork (`Sandboxes::create`), named after
  the node, then the node's branch checked out in the repository there.

A per-node lock keeps two launches from making two. Making one runs git,
Docker, or the network, so it only happens off the UI thread; the UI asks
`launch_cwd_if_made`, which never makes anything.

## Confirming a change with its impact

Any significant change is confirmed first, with what it will do shown. For
Files, a change that would leave locations stale lists them
(`provision::locations_affected_by`) in a dialog (`ui::files_impact`):

- changing the source's workspace directory, where it runs, whether
  repositories are mounted, worktrees on or off, or what sandboxes start
  from: the locations made from its settings;
- enabling Files on a descendant: the locations at or below it made from an
  ancestor's settings;
- disabling Files: every location made from it;
- "Remove" on one node: its own location.

Each row shows the node, its worktree or sandbox, and its state. Confirm
removes every one — pushing its branch to `origin` first, then deleting the
sandbox, returning the Treehouse lease, or removing the git worktree — and
only then makes the change. A push that fails keeps the location and stops.

A row with uncommitted work blocks Confirm and offers:

- **Commit** — a work-in-progress commit of everything there;
- **Retry** — check again;
- **Shell** — open a shell there; the dialog stays open, so the user can
  settle the files, close the shell, and Retry;
- **Discard** — throw the uncommitted work away.

A row whose node has a shell or agent running is busy until it stops.

`tod-cli capabilities set … files` refuses a change that would leave
locations stale: removing them needs the user.

## Known gaps

- Deleting a node archives its location row but leaves its worktree or
  sandbox; restoring the node puts it back in it.
- The removal check sees shells and agents recorded for the node, not a
  conversation turn or autopilot run that is starting.
- The cloud runner makes its sandbox on its own path
  (`cloud_sync::ensure_node_sandbox`).
- A new sandbox checks out the node's branch without fetching first.
