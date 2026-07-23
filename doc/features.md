# Features

## Data model

### Task

A task is a simple object with:

| Field | Type | Notes |
| --- | --- | --- |
| Title | string | Required |
| Branch | string? | Optional branch name |
| Issue ID | string? | Optional tracker issue ID (e.g. from Linear) |
| Modules | list | Required; may be empty. See [Modules](#modules) |
| Worktree | optional | See below |
| Last used | timestamp | Last interacted with / last used; list views sort by this descending (most recent first) |

### Modules

Each task has a **modules** list: which parts of the repo this task cares about when activating a worktree.

The set of possible module names is **dynamic per repository**:

- the name of the main repo itself
- plus the name of each git submodule

Stored values are those names. The list can be empty (none selected yet).

### Worktree

When a task has an associated worktree, store both:

| Field | Type | Notes |
| --- | --- | --- |
| Number | integer | Displayed as `1`, `2`, `3`, … |
| Path | path | Filesystem path to the worktree |

Worktree association is tracked automatically; the user does not set it by hand in the edit view.

## Persistence

Tasks (and all associated fields) are persisted as **one JSON file per task**.

| Setting | Behavior |
| --- | --- |
| Config directory | From an environment variable (e.g. `TASKSTUI_DATA_DIR`) if set; otherwise default to `$HOME/.config/taskstui/` (Linux XDG-style convention). This is the top-level config dir; task JSON files live under `{config}/tasks/` (i.e. `${TASKSTUI_DATA_DIR}/tasks/` when the env var is set). |
| Load | On startup, load all task JSON files from `{config}/tasks/` |
| Save | **Immediately** write to disk whenever any task data changes, so nothing is lost |
| File names | Normalized, truncated title + random characters (avoids collisions) |

## Integrations & credentials

Integrations (Linear now; others later) store credentials in the OS keyring via the [`keyring`](https://crates.io/crates/keyring) crate.

When code first needs to contact an integration:

1. Try to load the required credential (e.g. Linear API key) from the OS keyring.
2. If it is missing, prompt the user for it.
3. Store the entered credential in the keyring for later use.

## Views

There are several primary views.

### 1. Task list (main / initial view)

Shows active tasks, sorted by **last used descending** (most recent at the top). At the top of the list is a **Create new task** option.

Each list row displays:

- title
- branch (if any)
- worktree number (if any)

**Controls**

| Key | Action |
| --- | --- |
| ↑ / ↓ | Move selection |
| Enter | On a task: **switch** to it (see [Switching to a task](#switching-to-a-task)). On **Create new task**: open the new-task input (below). |
| E | Open the edit view for the selected task |
| A | Archive the selected task |
| Shift+A | Open the archive view |
| Q | Quit |

#### Create new task input

Selecting **Create new task** and pressing Enter prompts for a single-line input. The user types a value and presses Enter. Parsing rules, in order of specificity:

1. **Issue ID** — matches a few letters, a hyphen, and a few numbers (e.g. `ABC-123`). Look up that ID in Linear, pull the issue title, use it as the task title, and store the issue ID on the task. (May prompt for Linear credentials via the keyring flow above.)
2. **Branch name** — matches alphabetic characters, then `/`, then non-whitespace characters (no spaces). Create a task with that value as both **branch** and **title** (title is required).
3. **Title (default)** — otherwise treat the input as the new task’s title.

The new task appears in the list as usual (sorted by last used).

### 2. Archive view

Lists archived tasks, sorted by **last used descending**. Same display fields as the main list (title, branch, worktree number). Exact controls TBD beyond returning to the main view.

### 3. Edit view

Lets the user edit a task’s editable fields and shows read-only worktree info.

**Editable**

- title
- branch
- **modules** — multiselect list of the repo’s available modules (main repo name + each submodule name). The user can select or unselect any number of them.

**Display only (not editable)**

- worktree number and/or path (tracked automatically)
- issue ID (if any), as applicable

### 4. Switching to a task

Pressing Enter on a task in the list runs the **switch** action. The goal is to put the user in that task’s environment and open Cursor on its worktree.

Worktrees are managed with **[Treehouse](https://github.com/mentics/treehouse)** ([mentics/treehouse](https://github.com/mentics/treehouse) — our fork with submodule support). Treehouse maintains a pool of reusable git worktrees; we use it rather than managing worktrees by hand.

#### High-level flow

```
if task has no worktree:
    ensure modules + branch (prompt if missing; update the task)
    run New worktree steps
run Activate worktree steps   # always, including after New worktree
launch Cursor: cursor {worktree path}
```

Whether a Cursor instance already exists for that path does not matter; we always run `cursor {worktree path}` as the **last** step.

#### Prerequisites when there is no worktree yet

Before **New worktree**, if the user is switching to a task that is not yet associated with a worktree:

1. **Modules** — if the task has no modules associated (empty list), prompt with a multiselect of available modules (main repo + submodules). Persist the selection on the task.
2. **Branch** — if the task has no branch, prompt for a branch name. Persist it on the task.

#### New worktree steps

Used when the task is **not** yet associated with a worktree. After these succeed, continue with **Activate worktree**.

1. Call Treehouse **lease** (`treehouse get --lease`), always with **submodules** enabled (we always need submodule support).
2. Associate the leased worktree (number + path) with the task and persist.

#### Activate worktree steps

Used when the task **already** has a worktree, and also always after **New worktree**.

Check out a branch in the main repository and in every submodule. Worktrees cannot all share the same branch name, so:

| Target | Branch to check out |
| --- | --- |
| Modules associated with the task | The task’s **branch** (create the branch if it does not exist, then check it out) |
| Main repo and submodules **not** in the task’s modules list | `temp{N}` where `N` is the worktree number (e.g. `temp1`, `temp3`). Create if missing, then check out |

Then proceed to launch Cursor on the worktree path.
