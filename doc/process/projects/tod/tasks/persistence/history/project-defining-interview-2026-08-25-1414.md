# Project defining interview — persistence — 2026-08-25

## Session

**Entity:** `doc/process/projects/tod/tasks/persistence`
**Phase:** project-defining (Initial / defining)
**SQLite session id:** 2
**Mode:** interactive

**Prior context:**

- Task `persistence` is in lifecycle state `proposed` with a draft Goal and empty Requirements / Constraints in `user.md`.
- Parent project `tod` is active; fleet-state persistence is project requirement **23** in `doc/process/projects/tod/user.md`.
- Task was generated as `fleet-persistence` in `task-generation-2026-08-24-1642`.
- Interview UI kickoff started this session; researcher owns scaffolding (transcript, session scratchpad, queue, config).

---
## q-001

Project requirement **23** covers tasks, agents, agent transcripts, open notifications, and open shell sessions surviving relaunch with write-on-mutate semantics.

For this task, what delivery scope do you want?

**Recommend:** 1 — the Goal already names the full entity set; splitting unless you have a deliberate milestone in mind.

Which do you want?

**Selected:** 1

**Answer:** 

## q-004

Agent transcripts are named in the task Goal and project requirement **23**.

When the user relaunches tod, what transcript history must still be available per managed agent?

**Recommend:** 1 — project wording says “agent transcripts” survive restart without a truncation carve-out.

Which do you want?

**Selected:** 1

**Answer:** 

## q-002

Project **26** covers application settings separately from fleet state (**23**).

Should this persistence task also own settings/preferences storage, or only fleet entities (tasks, agents, transcripts, notifications, shell sessions)?

**Recommend:** 1 — keeps the task aligned with the Goal and project split unless you want one storage milestone for everything.

Which do you want?

**Selected:** 1

**Answer:** 

## q-003

Project **24** defines JSON data import as its own capability.

Is JSON import in scope for this persistence task, or explicitly excluded?

**Recommend:** 1 — import is a distinct user-facing flow; this task can focus on the live store and relaunch survival.

Which do you want?

**Selected:** 1

**Answer:** 

## q-005

Project requirement **15** says open shell sessions survive application restarts and remain visible on the agent detail page.

What minimum shell-session state must this task restore after relaunch?

**Recommend:** 1 unless you need scrollback durability in v1; reconnecting to a still-running process matches “open sessions survive restart.”

Which do you want?

**Selected:** 1

**Answer:** 

## q-006

Project requirement **23** says durable state is written when mutated with no separate explicit save.

For this task, how strict must write timing be?

**Recommend:** 1 — matches “no explicit save” and avoids surprise data loss on crash or force-quit.

Which do you want?

**Selected:** 2

**Answer:** 

## q-007

Project constraint **4** requires durable state on the user’s machine under a known local location the user can copy or back up.

Where should the fleet-state storage root live for this task?

**Recommend:** 1 for v1 — satisfies known location + backup with ordinary filesystem tools; configurable path can follow in settings work if needed.

Which do you want?

**Selected:** 2

**Answer:** 

## q-008 (action: defer)

For constraints on a persistence task like this, a few categories still look worth considering (not yet recorded in task `user.md`):

1. Crash and partial-write safety — torn writes or corrupted store must not silently destroy the whole fleet
2. Backup and portability — user can copy the storage root with ordinary filesystem tools without a proprietary export step
3. Concurrency — concurrent UI mutations and background agent updates must not corrupt stored fleet state
4. Platform paths — storage layout works on Windows, macOS, and Linux (project constraint 2)

Want to pursue any of these (or a close variant)?

**Recommend:** 1 unless one is clearly binding before design; project constraint 4 already covers backup at a high level.

Which do you want?



## q-012

The project requirement on **fleet-state persistence** says agents survive relaunch. **Agents and environments** defines environment type (local, devcontainer, Micro-VM), autonomous vs interactive mode, and runtime statuses (Starting, Processing, Blocked, Waiting).

After quit and relaunch, which agent metadata must this task restore for each agent that was in the managed fleet?

**Recommend:** 1 — matches full fleet parity and lets the UI show where each agent ran and how it was configured before quit.

Which do you want?

**Selected:** 1

**Answer:** 

## q-013

**Manual task create, edit, and delete** says permanently deleted tasks are removed; **Agent runtime status** says shut down removes an agent from the managed fleet.

Before quit, if the user permanently deletes a task or shuts down an agent, should that entity remain absent from the fleet after relaunch?

**Recommend:** 1 — durable mutations should include deletes and shutdowns, not only creates and edits.

Which do you want?

**Selected:** 1

**Answer:** 

## q-014

On first launch when no fleet-state store exists yet at the configured storage root, what should tod do?

**Recommend:** 1 — matches an empty managed fleet on fresh install; the configurable root can still default to a standard path.

Which do you want?

**Selected:** 1

**Answer:** 

## q-015

**Agent runtime status** lets the user queue a new prompt while an agent is Processing instead of interrupting.

After quit and relaunch, must queued (not yet delivered) prompts for an agent still in the managed fleet be restored?

**Recommend:** 1 — otherwise relaunch silently drops user-queued work.

Which do you want?

**Answer:** I think we should warn users if they try to quit when there are items in flight. We should warn them and tell them how many. In fact, it would be good to keep that number updated so that, as they do finish, that warning box could update the number. Once it gets down to zero, it could dismiss and quit. I think that's the best thing to do.

## q-016

The project requirement on **fleet-state persistence** says agents survive application restarts. **Agent runtime status** defines Starting, Processing, Blocked, and Waiting.

After quit and relaunch, what should happen to agents that were in the managed fleet before quit?

**Recommend:** 1 for v1 — durable fleet records plus manual relaunch is simpler than cross-process reattachment across local, devcontainer, and Micro-VM environments.

Which do you want?

**Selected:** 2

**Answer:** 

## q-017

**Human-in-the-loop** says notifications persist until acted upon and resolved. **Fleet-state persistence** includes open notifications in what must survive relaunch.

After quit and relaunch, what minimum must each unresolved notification restore?

**Recommend:** 1 — matches “from a notification, the user can see the related task, involved agents, and respond.”

Which do you want?

**Selected:** 1

**Answer:** 

## q-018

**Manual task create, edit, and delete** defines task fields (title, slug, lifecycle state, git repository, branch, tags, notes, linked issues/PRs). **Fleet-state persistence** says tasks survive relaunch.

After quit and relaunch, which task data must this task restore for each task?

**Recommend:** 1 — full parity with fleet-state persistence implies the complete task record the UI already manages.

Which do you want?

**Selected:** 1

**Answer:** 

## q-019

The task constraint on **configurable storage root** lets the user point fleet-state storage at a different path in application settings.

When the user changes the storage root to a new location, what should happen to fleet state already stored at the previous root?

**Recommend:** 1 — changing location should not silently discard an existing managed fleet unless the user explicitly opts into a fresh start.

Which do you want?

**Answer:** We should ask the user if they want to migrate and give them a choice of copy, move, or create new.

## q-020

When tod ships a new version whose on-disk fleet-state format is incompatible with the previous version, what must happen to fleet state already stored at the configured root?

**Recommend:** 1 — users expect upgrades to preserve their managed fleet without manual export/import steps.

Which do you want?

**Selected:** 1

**Answer:** 

## q-021

The task constraint on **configurable storage root** lets the user point fleet-state storage at a path on their machine.

On launch, if the configured storage root is missing, not a directory, or not writable, what should tod do?

**Recommend:** 1 — avoids silently writing fleet state to an unexpected location or losing data without the user noticing.

Which do you want?

**Selected:** 1

**Answer:** 

## q-022

**Human-in-the-loop** says notifications persist until acted upon and resolved.

Before quit, if the user resolves a notification, should that notification remain absent from the open notification queue after relaunch?

**Recommend:** 1 — durable mutations should include resolution, not only creates and edits.

Which do you want?

**Selected:** 1

**Answer:** 

## q-023

You described a quit warning that shows how many items are still in flight and waits until the count reaches zero before quitting.

What should count toward that in-flight number?

**Recommend:** 2 — queued prompts are user work that has not been delivered yet; Starting agents have not finished launching.

Which do you want?

**Answer:** I mentioned two things:
- Queued prompts: the prompt has been sent to an agent, and we're waiting to receive its response. That would be the in-flight count.
- Prompts that are sitting in the queue waiting to be sent would be the queued count.
For agents that are in starting status, I don't think that needs to block at all because we already have the metadata for it.

## q-024

You described a quit flow that updates the in-flight count and quits automatically once it reaches zero.

While the count is still above zero, should the user be able to quit anyway (accepting possible loss of in-flight work)?

**Recommend:** 1 — matches waiting for the count to drain before quitting.

Which do you want?

**Selected:** 2

**Answer:** 

## q-025

**Agent runtime status** lets the user queue a new prompt while an agent is Processing instead of interrupting.

After quit and relaunch, must queued (not yet delivered) prompts for an agent still in the managed fleet be restored?

**Recommend:** 1 — otherwise relaunch silently drops user-queued work.

Which do you want?

**Selected:** 2

**Answer:** 

## q-028

The task requirement on **agent reconnect relaunch** says tod attempts to reattach to agent processes still running on the host after relaunch.

When reattachment fails because the underlying process is no longer running, what should happen to that agent in the managed fleet?

**Recommend:** 1 — preserves the durable fleet record and metadata the user expects after relaunch; they can relaunch when ready.

Which do you want?

**Selected:** 1

**Answer:** 

## q-029

The task constraint on **configurable storage root** lets the user change the path, but does not define the default before the user configures one.

On first launch before the user sets a custom root, where should the default fleet-state storage root point?

**Recommend:** 1 — matches a known local location users can find and back up; OS app-data dirs are the usual desktop convention.

Which do you want?

**Selected:** 1

**Answer:** 

## q-030

**Launch shell into agent environment** supports multiple concurrent shell sessions per agent, visible on the agent detail page.

When an agent had more than one open shell session before quit, after relaunch must each of those sessions restore as a separate reconnectable identity?

**Recommend:** 1 — matches multiple concurrent sessions surviving restart without silently collapsing to one.

Which do you want?

**Selected:** 1

**Answer:** 

## q-031

The task requirement on **storage root change** offers copy, move, or create new when the user points fleet-state storage at a new path.

After a successful copy or move migration completes, what should happen to fleet state still at the previous root?

**Recommend:** 1 for copy (keeps a backup until the user deletes it); 2 for move (move implies the old location should not retain a second live copy).

Which do you want?

**Answer:** I thought I specifically said they have three choices:
- copy
- move
- create a new one
This behavior will be defined by what they choose. If they copy it, the stuff would stay there, and if they move it, then it would move, so it wouldn't be in the previous location anymore.

## q-032

You clarified that **in-flight** means prompts sent and awaiting a response, and **queued** means prompts waiting to be sent.

The task requirement on **quit with in-flight items** describes a warning with the in-flight count that updates until quit proceeds.

Should the quit warning also show the queued-not-yet-sent count (separate from in-flight)?

**Recommend:** 2 — both counts were distinct in your answer; showing only in-flight may hide undelivered queued work.

Which do you want?

**Selected:** 2

**Answer:** 

## q-033

The task requirement on **write timing** persists ordinary mutations via a short debounced batch; a crash may lose at most a few seconds of recent edits.

When quit proceeds (including after the in-flight count reaches zero), should tod flush any pending debounced fleet-state writes before the process exits?

**Recommend:** 1 — intentional quit should not lose edits that a debounce timer has not yet written.

Which do you want?

**Selected:** 1

**Answer:** 

## q-034

The task requirement on **format upgrade migration** says tod auto-migrates incompatible on-disk fleet state on first launch after update.

When that automatic migration fails, what should tod do?

**Recommend:** 1 — preserves the user's existing fleet data and avoids silent loss or a partial corrupt store.

Which do you want?

**Selected:** 1

**Answer:** 

## q-035

The task requirement on **quit with in-flight items** lets the user choose quit anyway while prompts are still in-flight (sent, awaiting a response).

After that force quit and relaunch, what should happen to an in-flight prompt and any partial agent response received before quit?

**Recommend:** 1 — the user still sees what was in progress instead of silently losing context.

Which do you want?

**Answer:** So I'm gonna guess that in that situation we probably would have already stored the prompt that we sent or something, or at least the metadata about it. I don't know, it depends on how that's done, but we would have no way of accessing the response, I don't think.

## q-036

The task requirement on **storage root change** offers copy, move, or create new when the user points fleet-state storage at a new path.

When the user chooses **create new** at the new root, what should happen to fleet state still at the previous root?

**Recommend:** 1 — create new implies a fresh fleet at the new location without silently discarding the old store.

Which do you want?

**Selected:** 1

**Answer:** 

## q-037

The task requirement on **shell session relaunch** restores open shell sessions as reconnectable identities on the agent detail page.

When a restored shell session's underlying process is no longer running after relaunch, what should the user see for that session?

**Recommend:** 1 — keeps the session visible so the user knows it existed; matches how failed agent reattachment leaves a clear not-running status.

Which do you want?

**Selected:** 1

**Answer:** 

## q-039

The task requirement on **storage root change** offers copy or move when the user points fleet-state storage at a new path.

When that copy or move migration fails partway, what should tod do?

**Recommend:** 1 — avoids guessing which side is authoritative after a partial transfer; the user can inspect both roots and retry.

Which do you want?

**Selected:** 1

**Answer:** 

## q-040

The task requirement on **shell session relaunch** restores open shell sessions as reconnectable identities on the agent detail page.

After relaunch, must a restored shell session also preserve terminal scrollback (output history from before quit)?

**Recommend:** 1 — project wording emphasizes open sessions surviving restart and reconnecting to live processes; scrollback durability is extra surface area.

Which do you want?

**Selected:** 1

**Answer:** 

## q-041

The task requirement on **storage root change** prompts for copy, move, or create new when the user points fleet-state storage at a new path.

When should that migration run relative to the settings change?

**Recommend:** 1 — avoids writing new mutations to the wrong root or leaving two divergent stores mid-session.

Which do you want?

**Answer:** Yeah, we should probably block. In fact, if there are queued or in-flight items, we should probably behave as if they were trying to quit. We would probably show a warning. It can be almost the same as the quit box, but with just a different message. I'll show you: have this mini cute, this many in-flight items. Require that to go down to zero before it does the migration, and it would block. That would be a modal message box or dialog, so it would block the whole application.

## q-042

The parent project constraint on **logging practices** applies to tod generally.

Should this persistence task record a constraint that fleet-state read, write, and migration failures are logged per those shared logging constraints?

**Recommend:** 1 — operators need traceability when data fails to load or save; aligns with parent constraint without prescribing format here.

Which do you want?

**Selected:** 2

**Answer:** 

## q-043 (action: defer)

I've reviewed the current requirements for this phase. These top-level areas look complete and coherent for a reasonable persistence task — I don't see a compelling gap to propose next:

1. **Scope** — full fleet parity, fleet entities only, JSON import out of scope
2. **Entity restore** — transcripts, shell sessions, agent metadata, task records, notifications
3. **Durability semantics** — write timing, deletes/shutdowns, notification resolution, queued non-restore
4. **Quit and in-flight** — quit warning with live counts, force-quit transcript behavior
5. **Storage root** — configurable/default root, root change migration, invalid root, format upgrade
6. **Agent relaunch** — reconnect to running processes, not-running fallback

Did we miss anything important?

**Recommend:** 1 unless you see a real gap.
Which do you want?



## q-044

On launch, if the fleet-state store at the configured root appears corrupted or partially written (for example after a crash mid-write), what should tod do?

**Recommend:** 1 — preserves existing data and avoids silent loss or guessing after torn writes.
Which do you want?

**Selected:** 2

**Answer:** 

## q-045

The UI can mutate fleet state while background agent activity may also trigger writes. What consistency guarantee must this task provide when those writes overlap?

**Recommend:** 1 — matches write-on-mutate without requiring a global write lock that stalls the UI.
Which do you want?

**Selected:** 2

**Answer:** 

## q-046 (action: defer)

For constraints on a persistence task like this, a few categories still look worth considering (not yet recorded in task `user.md`):

1. **Concurrency** — concurrent UI mutations and background agent updates must not corrupt stored fleet state
2. **Backup and portability** — user can copy the storage root with ordinary filesystem tools without a proprietary export step
3. **Platform paths** — storage layout works on Windows, macOS, and Linux

Want to pursue any of these (or a close variant)?

**Recommend:** 1 unless one is clearly binding before design; configurable storage root already covers backup at a high level.
Which do you want?



## q-047

The task requirement on **storage root change** lets the user cancel a pending or in-progress migration.

When the user cancels mid-migration, what should the state of the previous and new roots be?

**Recommend:** 1 — matches partial-failure behavior already chosen; avoids guessing which side is authoritative after an interrupted transfer.
Which do you want?

**Selected:** 2

**Answer:** 

## q-048

When a fleet-state write fails because the disk is full or the storage root is no longer writable, what should tod do?

**Recommend:** 1 — avoids silent data loss and keeps the last good on-disk state authoritative.
Which do you want?

**Selected:** 1

**Answer:** 

## q-049

If the user force-quits tod while a storage root copy or move migration is in progress, what should happen on the next launch?

**Recommend:** 1 — same principle as partial migration failure and cancel; do not guess which root is authoritative.
Which do you want?

**Selected:** 2

**Answer:** 

## q-051

The parent project constraint on **single-user local desktop** implies one user on one machine, but does not say whether two tod processes may use the same fleet-state storage root at once.

If the user launches a second tod instance while another is already running against the same storage root, what should happen?

**Recommend:** 1 — avoids concurrent writers corrupting the store without adding cross-process locking complexity.
Which do you want?

**Selected:** 1

**Answer:** 

## q-052

The task requirement on **format upgrade migration** covers launching after a tod upgrade when the on-disk format is older than this build. The reverse case is not yet defined.

When the fleet-state store on disk was written by a newer tod version than the one launching (format version newer than this build understands), what should tod do?

**Recommend:** 1 — avoids corrupting or misinterpreting data from a newer format.
Which do you want?

**Selected:** 1

**Answer:** 

## q-053

When tod runs automatic format upgrade migration on first launch after update, should it create a backup copy of the pre-migration store before modifying on-disk files?

**Recommend:** 1 — if migration fails partway, the user still has an untouched pre-migration copy to recover from.
Which do you want?

**Selected:** 1

**Answer:** 

## q-055

The parent project constraint on **cross-platform desktop** requires Windows, macOS, and Linux support. Task constraints do not yet say whether fleet-state storage layout must work on all three.

Should this task record a constraint that the on-disk fleet-state layout is compatible across Windows, macOS, and Linux?

**Recommend:** 1 — persistence owns the store; explicit constraint avoids platform-specific layout surprises during design.
Which do you want?

**Answer:** The actual storage does not have to be portable between operating systems. If someone wants to do that, we can implement import-export, and so if they wanted to migrate to a different operating system, they would export the data and then go import it in the destination.

## q-056

The parent project requirement on **concurrent tasks and agents** expects the UI to manage at least ~100 agents and ~500 tasks. Task requirements do not yet say whether the persistence layer must guarantee that scale.

Must this task explicitly require fleet-state storage and load to support at least ~100 agents and ~500 tasks?

**Recommend:** 1 — parent scale targets are binding for a fleet app; persistence should not become the bottleneck.
Which do you want?

**Selected:** 1

**Answer:** 

## q-057

The task constraint on **configurable storage root** puts fleet state where the user can browse files with ordinary tools. That opens the possibility of manual edits outside tod.

If the user manually modifies or deletes files under the fleet-state storage root while tod is running, what should tod do on the next fleet-state read or write?

**Recommend:** 2 — external edits should be visible rather than silently overwritten or ignored indefinitely.
Which do you want?

**Selected:** 2

**Answer:** 

## q-058

The task requirement on **agent transcript history** restores full prompt/response history after relaunch. There is no bound yet on how much transcript data the store retains over time.

Should agent transcript history on disk grow without limit, or should this task impose a retention cap or pruning policy?

**Recommend:** 1 for v1 — matches full history restore; pruning can be a later task if disk use becomes a problem.
Which do you want?

**Selected:** 1

**Answer:** 
