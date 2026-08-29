# Persistence

Project: `doc/process/projects/tod/`

## Goal

Durable on-machine storage for tasks, agents, transcripts, open notifications, and open shell sessions; mutations persist without an explicit save and survive quit/relaunch.

## Requirements

### Scope

1. Full parity — This task delivers all of project requirement 23 in one pass.

### Entity restore after relaunch

2. Task records — After quit and relaunch, each task restores title, slug, lifecycle state, git repository, branch, tags, notes, and linked issues/PRs.

3. Notifications — After quit and relaunch, each unresolved notification restores message text, links to related task and agents, and unresolved status. Resolving a notification before quit keeps it absent after relaunch.

4. Agent fleet — After quit and relaunch, each managed agent restores task association, environment type, mode, last persisted runtime status, full prompt/response transcript history, and (for local/devcontainer agents) worktree identity sufficient to dirty-check, reclaim, and manually relaunch. Tod attempts reattach to still-reachable agent processes for every environment type, verifying identity beyond recorded PID (PID-reuse guard). Successful reattach shows and persists live runtime status. Failed reattach persists **not-running**; the agent stays in the fleet until manual relaunch or shutdown. Manual relaunch reuses the same agent record and persisted environment type (fixed for the agent's lifetime); mode defaults to last persisted with optional change before start. Successful manual relaunch preserves transcript history and reconnectable shell sessions on that record.

5. Shell sessions — After quit and relaunch, each open shell session restores as its own reconnectable identity on the agent detail page (per environment type); reconnect verifies identity beyond recorded PID. Multiple open sessions restore separately. Restored sessions do not include pre-quit terminal scrollback. Reconnect is independent of agent runtime status when the shell process is still running and passes verification. While an agent is not-running, the user cannot launch a new shell into that environment — only reconnect to existing still-running restored shells. When a restored shell's process is gone or verification fails, the session stays listed with a clear not-running indicator and no reconnect action; the user may dismiss/remove that entry without confirmation, and dismissed entries stay absent after relaunch.

6. Queued prompts — Undelivered queued prompts are not restored after relaunch.

### Durability and writes

7. Write timing — Ordinary mutations use a short debounced batch (~2s); crash or force-quit may lose at most ~5 seconds of recent edits. When quit proceeds (including after in-flight count reaches zero or on "quit anyway"), tod flushes pending debounced fleet-state writes before exit. If that flush fails, quit is blocked with a clear error and retry until flush succeeds or the user force-quits.

8. Immediate durability — Permanent task deletes, agent shutdowns, notification resolves, not-running shell dismissals, sent in-flight prompts, reattach outcomes, and other irreversible or relaunch-critical mutations flush immediately (not via the debounce).

9. Delete durability — Permanent task deletes and agent shutdowns before quit remain absent after relaunch.

10. Write path — UI- and agent-triggered fleet-state writes serialize through one queue; no concurrent writers. Write failures (disk full, unwritable root) block the mutation with a clear error; the last good on-disk store stays authoritative. External edits under the storage root while tod runs are picked up on the next read or write (stale in-memory state discarded).

### Quit and in-flight

11. Quit with in-flight items — When quit is attempted while in-flight items exist, tod warns with in-flight count and queued-not-yet-sent count shown separately; counts update live; when in-flight reaches zero, quit proceeds. While in-flight is above zero, "quit anyway" abandons in-flight work and proceeds (still subject to write flush). **In-flight** = prompts sent and awaiting response; **queued** = not yet sent. Agents in Starting do not block quit. After quit-anyway while Processing, if the agent process is still reachable after relaunch, tod reattaches with live status; when the process is gone, in-flight sent prompts remain in the transcript as interrupted/incomplete with no response.

### Storage root and lifecycle

12. First launch — When the configured storage root does not exist or has no fleet-state store, tod creates the directory (if needed) and an empty store; fleet starts empty. Unwritable or non-directory paths block launch per invalid storage root (below). First-launch store init failure blocks launch with clear error, leaves partial state as-is, and exposes minimal settings to fix the path. A later relaunch against an unchanged path with a partial store runs corrupted-store recovery.

13. Invalid / newer format — Launch is blocked with clear error and minimal settings when the root exists but is not a writable directory, the store is corrupted and auto-recovery fails, or on-disk format is newer than this build (store left untouched). A path that does not exist yet is not invalid (bootstrap creates it). After correcting the storage root in settings, persist the path on confirm and require full app restart before launch proceeds.

14. Single instance — A second tod instance against an in-use storage root is blocked with clear error. Stale locks from crashed processes are treated as abandoned. Destination roots during copy/move migration and roots undergoing format upgrade are also treated as in-use. After successful copy migration, a second instance against the previous-root duplicate path is allowed (locking is per root).

15. Format upgrade — On first launch after an incompatible on-disk format, tod auto-migrates in place after creating a pre-migration backup sibling under the storage root. Backup creation failure blocks launch. Failed upgrade blocks launch with error naming the backup path; recovery is manual filesystem restore (no in-app restore action). Force-quit mid-backup retries on next launch; force-quit mid-upgrade auto-restores from backup on next launch and defers upgrade. Successful upgrade removes the backup.

16. Storage root change — When the fleet-state storage root changes (via parent application settings), tod offers copy, move, or create new at the new location. Tod creates the destination directory if needed; blocks the change if the destination already contains a store or is not creatable. Migration runs after user confirmation: flush pending writes first; block fleet mutations (and queue agent-triggered writes to a held sidecar during copy/move) until finish or cancel. In-flight/queued items at migration time use the same modal pattern as quit (proceed when in-flight is zero). Copy/move include open shell session records. **Copy** — new root becomes active; previous root keeps a point-in-time duplicate. **Move** — fleet relocates; previous root cleaned of tod-owned files. **Create new** — empty store at new root; previous root untouched; blocked while any prior-fleet agent is not **not-running**. Cancel mid copy/move rolls back without confirmation. Failed partway copy/move leaves both roots as-is with retry. Force-quit mid copy/move rolls back to pre-migration root on next launch. Create-new switch flushes previous root first; does not tear down still-alive shell processes from the previous fleet.

17. Scale and retention — Fleet-state storage supports at least ~100 agents and ~500 tasks. Agent transcript history on disk has no automatic pruning cap in this task.

## Constraints

1. Fleet entities only — Application settings (project requirement 26) are out of scope except the fleet-state storage root path in minimal settings when launch is blocked for storage problems, and the normal storage-root control owned by parent settings. JSON import (requirement 24) and fleet export are separate tasks.

2. Configurable storage root — User-configurable path on the user's machine; copy/backup with ordinary filesystem tools (project constraint 4). Where the user views or changes the storage root, tod shows brief guidance that a consistent backup requires copying the root only while no tod instance is running against it. Default on first launch is the OS-specific application data directory. On-disk layout need not be portable across operating systems.

3. Agent removal cascade — Removing an agent from the managed fleet (shutdown, auto-delete on missing worktree, or explicit remove) tears down still-alive restored shell processes, deletes that agent's shell and transcript rows, and hard-deletes open notifications linked to that agent — without confirmation. Auto-delete on missing worktree shows a clear notice (no confirmation on delete).

4. Worktree missing — When manual relaunch of a not-running local/devcontainer agent finds the restored worktree path missing, tod deletes that agent from the fleet (cascade above applies).

5. Not-running agent UX — Inherits project requirement 3 (**not-running** status) and requirement 31 exceptions: shutdown and dismiss of stale shells need no confirmation; dirty-worktree warning still applies on shutdown; no new prompt without manual relaunch; failed reattachment does not add a notification.

6. Persistent notices — Follow [`doc/process/shared/constraints/persistent-notice-constraints.md`](../../../../shared/constraints/persistent-notice-constraints.md).

7. Operation failure feedback — User-visible errors for blocked operations inherit project requirement 11 (toast/banner).
