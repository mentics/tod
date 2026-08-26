# Persistence

Project: `doc/process/projects/tod/`

## Goal

Durable on-machine storage for tasks, agents, transcripts, open notifications, and open shell sessions; mutations persist without an explicit save and survive quit/relaunch.

## Requirements

1. Full parity — This task delivers all of project requirement 23 in one pass.

2. Agent transcript history — After quit and relaunch, each managed agent still has full prompt/response history for every agent session available to the user.
   - Success criteria:
     - After relaunch, the user can view the complete prompt/response history for all prior sessions of each managed agent

3. Shell session relaunch — After quit and relaunch, each open shell session restores as its own reconnectable identity on the agent detail page; the user reconnects to the live process when it still exists. When an agent had more than one open shell session before quit, each session restores separately. Restored shell sessions preserve reconnectable identity only; terminal scrollback (output history from before quit) is not restored.
   - Success criteria:
     - After relaunch, previously open shell sessions remain visible on the agent detail page
     - When an agent had multiple open shell sessions before quit, each restores as a separate reconnectable identity
     - User can reconnect to a shell session whose underlying process is still running
     - When a restored shell session's underlying process is no longer running after relaunch, the session remains listed with a clear not-running indicator
     - When a restored shell session's underlying process is no longer running after relaunch, no reconnect action is offered
     - After relaunch, restored shell sessions do not include pre-quit terminal scrollback

4. Write timing — Ordinary mutations are persisted via a short debounced batch; a crash or force-quit may lose at most a few seconds of recent edits. When quit proceeds (including after the in-flight count reaches zero), tod flushes any pending debounced fleet-state writes before the process exits.
   - Success criteria:
     - No explicit save step is required for ordinary mutations
     - Data loss window after crash is bounded to a few seconds of recent edits
     - When quit proceeds, pending debounced fleet-state writes are flushed before exit completes

5. Agent metadata relaunch — After quit and relaunch, each agent that was in the managed fleet before quit restores associated task, environment type, mode (autonomous vs interactive), and last persisted runtime status.
   - Success criteria:
     - After relaunch, user can see each restored agent's associated task, environment type, mode, and last persisted runtime status

6. First launch bootstrap — On first launch when no fleet-state store exists at the configured storage root, tod creates an empty store; the fleet starts with zero tasks and agents.
   - Success criteria:
     - On first launch with no existing store, tod initializes storage without blocking the user
     - Fleet begins with zero tasks and zero agents

7. Delete and shutdown durability — Permanent task deletes and agent shutdowns before quit are persisted; those entities remain absent from the fleet after relaunch.
   - Success criteria:
     - After relaunch, permanently deleted tasks do not reappear
     - After relaunch, shut-down agents do not reappear in the managed fleet

8. Quit with in-flight items — When the user attempts to quit while in-flight items exist, tod warns with the in-flight count and the queued-not-yet-sent count shown separately; the warning updates live as items complete; when the in-flight count reaches zero, the warning dismisses and quit proceeds. While the in-flight count is still above zero, the warning includes a "quit anyway" action that proceeds immediately, accepting possible loss of in-flight work. In-flight means prompts that have been sent to an agent and are awaiting a response; queued means prompts waiting to be sent; agents in Starting status do not block quit.
   - Success criteria:
     - Quit attempt while in-flight items exist shows a warning with the current in-flight count and the queued-not-yet-sent count displayed separately
     - The displayed in-flight count updates as sent prompts receive responses
     - When the in-flight count reaches zero, the warning dismisses and quit proceeds automatically
     - While the in-flight count is above zero, the warning includes a "quit anyway" action that quits immediately
     - Agents in Starting status do not increment the in-flight count or block quit
     - After force quit while prompts are in-flight and relaunch, each in-flight sent prompt remains visible in the agent transcript as interrupted/incomplete
     - After force quit while prompts are in-flight and relaunch, no response (partial or complete) is available for those in-flight prompts

9. Agent reconnect relaunch — After quit and relaunch, tod attempts to reattach to agent processes still running on the host for agents that were in the managed fleet before quit. When reattachment fails because the underlying process is no longer running, the agent record remains in the managed fleet with a clear not-running status; the user can relaunch manually.
   - Success criteria:
     - After relaunch, tod attempts to reattach to agent processes still running on the host
     - When reattachment fails because the process is no longer running, the agent remains in the fleet with a clear not-running status
     - User can manually relaunch an agent left in not-running status after failed reattachment

10. Task record relaunch — After quit and relaunch, each task restores the full task record: title, slug, lifecycle state, git repository, branch, tags, notes, and linked issues/PRs.
   - Success criteria:
     - After relaunch, user can see each restored task's title, slug, lifecycle state, git repository, branch, tags, notes, and linked issues/PRs

11. Notification relaunch — After quit and relaunch, each unresolved notification restores full queue entry: message text, links to related task and agents, and unresolved status.
   - Success criteria:
     - After relaunch, user can see message text for each unresolved notification
     - After relaunch, user can see related task and agent links for each unresolved notification
     - After relaunch, unresolved notifications remain in unresolved status

12. Storage root change — When the user changes the fleet-state storage root to a new location, tod asks whether to migrate existing fleet state and offers copy, move, or create new. Migration runs immediately after the user confirms the choice; tod blocks further fleet mutations until migration finishes or the user cancels. When in-flight or queued items exist at migration time, tod shows a modal dialog (same pattern as the quit warning) with the in-flight count and queued-not-yet-sent count shown separately; the dialog blocks the whole application and migration proceeds only when the in-flight count reaches zero. Post-migration behavior follows the user's choice: copy leaves fleet state at the previous root (a duplicate exists at the new root); move relocates fleet state to the new root (nothing remains at the previous root); create new starts with an empty store at the new root without migrating from the previous root and leaves fleet state at the previous root untouched (user cleans up manually if desired).
   - Success criteria:
     - Changing storage root prompts the user whether to migrate existing fleet state
     - User can choose copy, move, or create new at the new root
     - Storage root migration runs immediately after the user confirms the migration choice
     - While migration is pending or in progress, further fleet mutations are blocked
     - When in-flight or queued items exist at migration time, a modal dialog blocks the application
     - Migration dialog shows in-flight count and queued-not-yet-sent count separately
     - Migration proceeds only when the in-flight count reaches zero
     - User can cancel a pending or in-progress storage root migration
     - When the user cancels a pending or in-progress storage root migration, tod automatically rolls back to the pre-migration state
     - After cancel mid-migration, the configured storage root remains the pre-migration root
     - After cancel mid-migration, partial fleet state written to the new root during the interrupted migration is removed
     - After a copy migration, fleet state remains at the previous root
     - After a move migration, fleet state is no longer at the previous root
     - After create new, the new root starts empty without data migrated from the previous root
     - After create new, fleet state at the previous root remains untouched
     - When a copy or move migration fails partway, tod blocks completion and leaves both roots as-is
     - When a copy or move migration fails partway, the user sees a clear error and can retry or fix manually
     - If the user force-quits while a storage root copy or move migration is in progress, the next launch automatically rolls back to the pre-migration root
     - After force-quit mid-migration and rollback on next launch, partial fleet state written to the new root during the interrupted migration is removed

13. Format upgrade migration — When a new tod version ships with an incompatible on-disk fleet-state format, tod auto-migrates existing fleet state in place on first launch after update, creating a backup copy of the pre-migration store before modifying on-disk files.
   - Success criteria:
     - After upgrading tod when the on-disk format is incompatible, existing fleet state is preserved without manual export or import
     - Migration runs automatically on first launch after update
     - Before modifying on-disk files during automatic format upgrade migration, tod creates a backup copy of the pre-migration store
     - When automatic migration fails, launch is blocked with a clear error
     - When automatic migration fails, pre-migration fleet state at the storage root is left untouched
     - When automatic migration fails, the pre-migration backup remains available for recovery

14. Invalid storage root — On launch, if the configured fleet-state storage root is missing, not a directory, or not writable, tod blocks launch with a clear error; the user fixes the path in settings before continuing.
   - Success criteria:
     - Launch is blocked when the storage root is missing, not a directory, or not writable
     - User sees a clear error explaining the problem
     - User can fix the path in settings before continuing

15. Notification resolution durability — Before quit, if the user resolves a notification, that notification remains absent from the open notification queue after relaunch.
   - Success criteria:
     - After relaunch, notifications resolved before quit do not reappear in the open notification queue

16. Queued prompt non-restore — After quit and relaunch, prompts that were queued but not yet delivered to an agent are not restored; relaunch clears undelivered queued prompts.
   - Success criteria:
     - After relaunch, undelivered queued prompts do not reappear on any agent's prompt queue

17. Corrupted store recovery — On launch, if the fleet-state store at the configured storage root appears corrupted or partially written (for example after a crash mid-write), tod attempts automatic repair or recovery before blocking launch.
   - Success criteria:
     - When the store appears corrupted or partially written on launch, tod attempts automatic repair or recovery
     - Launch proceeds normally when automatic repair or recovery succeeds
     - When automatic repair or recovery fails, launch is blocked with a clear error
     - When automatic repair or recovery fails, fleet state at the storage root is left as-is (not replaced with an empty store)

18. Concurrent write consistency — When the UI and background agent activity both mutate fleet state, all fleet-state writes are serialized through one queue; no concurrent writers touch the store.
   - Success criteria:
     - UI-triggered and agent-triggered fleet-state mutations share a single serialized write path
     - No two fleet-state writes execute concurrently against the store

19. Write failure handling — When a fleet-state write fails because the disk is full or the storage root is no longer writable, tod blocks the mutation with a clear error; the prior on-disk store remains intact and authoritative.
   - Success criteria:
     - When a fleet-state write fails due to disk full or an unwritable storage root, the user sees a clear error
     - The failed mutation does not persist; the last good on-disk fleet state remains intact
     - tod does not silently accept in-memory mutations that cannot be written to disk

20. Single storage root instance — If the user launches a second tod instance while another is already running against the same fleet-state storage root, the second instance is blocked with a clear error.
   - Success criteria:
     - Launch of a second tod instance against an in-use storage root is blocked
     - User sees a clear error explaining that another tod instance is already using the storage root

21. Newer format downgrade — When the fleet-state store on disk was written by a newer tod version than the one launching (on-disk format version is newer than this build understands), tod blocks launch with a clear error and leaves the store untouched.
   - Success criteria:
     - Launch is blocked when the on-disk format version is newer than this build supports
     - User sees a clear error explaining the store format is from a newer tod version
     - Fleet state at the storage root is left untouched (not modified or overwritten)

22. Fleet scale — Fleet-state storage and load explicitly support at least ~100 agents and ~500 tasks.
   - Success criteria:
     - Fleet-state store loads and persists mutations for a fleet with at least ~100 agents
     - Fleet-state store loads and persists mutations for a fleet with at least ~500 tasks

23. External storage edits — When the user manually modifies or deletes files under the fleet-state storage root while tod is running, tod reloads from disk on the next fleet-state read or write and discards stale in-memory state.
   - Success criteria:
     - After manual edits under the storage root while tod is running, the next fleet-state read or write reflects on-disk state
     - Stale in-memory fleet state is discarded rather than persisted over external changes

24. Agent transcript retention — Agent transcript history on disk is retained without limit; this task does not impose a retention cap or pruning policy.
   - Success criteria:
     - Agent transcript data on disk is not automatically pruned or evicted by size or age

## Constraints

1. Fleet entities only — Application settings and preferences (project requirement 26) are out of scope; this task covers fleet state per project requirement 23.

2. JSON import out of scope — JSON data import (project requirement 24) is a separate task; this task does not include import capability.

3. Configurable storage root — Fleet-state storage root is user-configurable in application settings; the chosen path is on the user's machine and can be copied or backed up with ordinary filesystem tools (project constraint 4).

4. Default storage root — On first launch before the user sets a custom root, the default fleet-state storage root is the OS-specific standard application data directory (AppData on Windows, Application Support on macOS, XDG data dir on Linux).

5. Cross-OS storage portability — On-disk fleet-state layout need not be portable between operating systems. When a user moves to a different operating system, they export fleet state and import it on the destination; copying the storage root across OS boundaries is not a supported migration path for this task.
