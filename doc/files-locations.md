# Files: global locations, inherited choice, per-node worktrees

## The problem

The Files capability used to be one place: a workspace directory, maybe a
worktree set up by a button, maybe a cloud sandbox chosen or created by
name. Descendants inherited that one place. With a sandbox, every node under
the capability shared a single machine, and a sandbox that was never created
("Create and use it") left every descendant with "Choose the cloud sandbox".

## The model

**Locations** (`files_places`, `tod_store::fleet::FilesPlace`) are configured
once for the whole app, from the **Locations** button on the status bar
(`views::locations`): a name, the workspace directory, where it runs (this
machine, a dev container, cloud sandboxes), whether each node gets a
worktree, and for sandboxes what each starts from (an image, or a fork of a
workspace sandbox). They are machine-local and never synced.

A node's **Files capability** only *picks* one (`node_files.place_id`) and
every node below it that does not enable Files itself inherits the choice.
The node keeps its own **branch** (`node_fields.branch`), so an inheriting
node still has one. Enabling Files on a descendant starts from the location
it already worked from. A node whose location was deleted, or never chosen,
says "Choose a location" and cannot launch.

The worktree or sandbox a node gets from its location is **derived
per-node state**:

- **Location** (`node_files_locations`, one row per node): the worktree or
  sandbox made for that node, with `source_node_id` (the node whose pick made
  it) and `recipe` (a fingerprint of the location's settings,
  `files_location::recipe_key`). It is machine-local and never synced.
- A node's location is **current** when its source and fingerprint match
  what the node resolves to now, and **stale** otherwise. A stale location is
  never used; a launch refuses it until it is removed. Reverting makes it
  current again.
- A node's **branch** is its own, `task/<slug>` by default, recorded when its
  location is made. Every node with a location is on a unique branch.
- A node with no worktree or sandbox shares the workspace directory.

## Made on first use

Nothing asks the user to set up a worktree or create a sandbox. The first
time a node needs its files — an agent turn, a shell, a code editor, a
background run — `provision::resolve_launch_cwd` makes its location:

- a worktree (`ensure_worktree`, on this machine or in the dev container),
  on the node's branch; nodes on the same branch share it;
- a sandbox from the image or as a fork (`Sandboxes::create`), named after
  the node, then the node's branch checked out in the repository there.

Either way `origin` is fetched first (`worktree::fetch_origin`), so the work
starts from the latest: a branch `origin` already has (work pushed before)
is continued from there, and a new one starts from `origin`'s default branch
(`origin/HEAD`, else `origin/main`), never from whatever the checkout had.
A fetch that fails stops the launch with the reason. A repository without an
`origin` starts new branches at HEAD.

A per-node lock keeps two launches from making two. Making one runs git,
Docker, or the network, so it only happens off the UI thread; the UI asks
`launch_cwd_if_made`, which never makes anything.

## Confirming a change with its impact

Any significant change is confirmed first, with what it will do shown. A
change that would leave locations stale lists them
(`provision::locations_affected_by`) in a dialog (`ui::files_impact`):

- **editing a location** that nodes use: the nodes that picked it, and the
  worktrees and sandboxes made from it whose fingerprint the edit changes
  (`fleet::places::impact`). A rename, or any edit that leaves the settings a
  worktree was made from alone, removes nothing;
- **deleting a location**: the nodes that picked it (they have none
  afterwards) and every worktree or sandbox made from it;
- a node **picking another location**: its own locations made from the old
  one;
- enabling Files on a descendant: the locations at or below it made from an
  ancestor's pick;
- disabling Files: every location made from it;
- "Remove" on one node: its own location;
- deleting a node: every location at or below it
  (`provision::locations_in_subtree`); the node is deleted once they are
  gone.

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

`tod-cli capabilities set … files --location` picks an existing location and
refuses a pick that would leave locations stale; agents cannot create, edit
or delete locations. `tod-cli node delete` refuses a node with locations at
or below it: removing them needs the user.

## Known gaps

- The Locations dialog does not build images with repositories baked in (the
  old per-node "Image with repositories" form). `tod-sandbox bake` does it.
- The removal check sees shells and agents recorded for the node, not a
  conversation turn or autopilot run that is starting.
- "Run in the cloud" (a node's autonomous runner, `doc/cloud-sandboxes/
  autonomous-nodes.md`) makes its sandbox on its own path
  (`cloud_sync::ensure_node_sandbox`): it clones the repository by URL and
  starts the node's branch from `origin/HEAD` there, but it is not recorded
  as the node's location and none of the above applies to it.
