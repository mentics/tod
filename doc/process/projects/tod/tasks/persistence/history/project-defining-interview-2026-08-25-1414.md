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

## q-050

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

**Selected:** 1

**Answer:** 

## q-067

The constraint on **cross-OS storage portability** says users move between operating systems via export then import, and that copying the storage root across OS boundaries is not supported. **JSON import** is already out of scope for this task; export is not yet defined.

Must this persistence task also deliver fleet-state export?

**Recommend:** 2 — this task can rule out raw cross-OS copy without owning export; import is already a separate project capability.
Which do you want?

**Selected:** 2

**Answer:** 

## q-069

The requirement on **agent reconnect relaunch** says tod attempts to reattach to agent processes still running on the host.

After relaunch, for which agent environment types must tod attempt reattachment?

**Recommend:** 1 for v1 — “on the host” fits local processes; devcontainer and Micro-VM reattachment is a larger design surface.
Which do you want?

**Selected:** 2

**Answer:** 

## q-070

The requirements list is large and flat. For easier scanning, I propose grouping under these headings (requirement text stays the same; only headings change):

1. **Scope**
2. **Entity restore**
3. **Durability and writes**
4. **Quit and in-flight**
5. **Storage root and format**
6. **Agent relaunch**
7. **Integrity and recovery**
8. **Scale and retention**

Want to adopt this grouping?

**Recommend:** 1 — twenty-plus flat items are hard to scan; headings match how we have been talking about the set.
Which do you want?

**Selected:** 1

**Answer:** 

## q-071

The requirement on **invalid storage root** blocks launch when the configured path is missing, not a directory, or not writable, and says the user can fix the path in settings before continuing.

While launch is blocked for that reason, how does the user reach the path setting?

**Recommend:** 1 — otherwise “fix in settings” cannot work when the main app never opens.
Which do you want?

**Selected:** 1

**Answer:** 

## q-072

The requirement on **format upgrade migration** creates a backup copy of the pre-migration store before modifying on-disk files, and keeps that backup available if migration fails.

Where must that backup live for this task?

**Recommend:** 1 — a sibling under the storage root is findable for recovery without a separate settings surface.
Which do you want?

**Selected:** 1

**Answer:** 

## q-073

The requirement on **agent metadata relaunch** restores each agent’s last persisted runtime status. **Agent reconnect relaunch** also reattaches to still-reachable agent processes for every environment type.

After a successful reattach to a still-reachable agent process, which runtime status should the UI show?

**Recommend:** 1 — last-persisted can be stale (e.g. Processing vs Waiting) once the live process is reachable again.
Which do you want?

**Selected:** 1

**Answer:** 

## q-074

Proposed Goal wording for this task:

> Durable on-machine storage for tasks, agents, transcripts, open notifications, and open shell sessions; mutations persist without an explicit save and survive quit/relaunch.

**Recommend:** 1 — matches the gathered fleet-state scope and write-on-mutate / relaunch behavior.
Which do you want?

**Selected:** 1

**Answer:** 

## q-075

The requirement on **format upgrade migration** creates a pre-migration backup before modifying on-disk files and keeps that backup available when migration fails.

After a successful automatic format upgrade, what should happen to that backup?

**Recommend:** 1 — keeps a recovery copy if the new format later proves problematic; user controls disk use.
Which do you want?

**Selected:** 2

**Answer:** 

## q-076

The requirement on **format upgrade migration** creates a pre-migration backup before modifying on-disk files so recovery is possible if migration fails.

If creating that backup fails (for example the disk is full), what should tod do?

**Recommend:** 1 — without a backup, the recovery guarantee for a failed upgrade is gone.
Which do you want?

**Selected:** 1

**Answer:** 

## q-077

The requirement on **agent reconnect relaunch** leaves an agent in the fleet with a clear not-running status when reattachment fails because the process is gone.

The parent project runtime status set is Starting, Processing, Blocked, and Waiting (shutdown removes the agent from the fleet). How should not-running relate to that set after failed reattach?

**Recommend:** 1 — matches the task wording and distinguishes “in fleet, process gone” from Waiting or Blocked.
Which do you want?

**Selected:** 1

**Answer:** 

## q-078

The requirement on **shell session relaunch** restores reconnectable shell sessions after relaunch when the underlying process still exists.

For which agent environment types must shell reconnect apply after relaunch?

**Recommend:** 1 for v1 — local shells are the clear case; devcontainer and Micro-VM shell reconnect is a larger surface.
Which do you want?

**Selected:** 2

**Answer:** 

## q-079

For constraints on a persistence task like this, a few categories still look worth considering (not yet recorded):

1. **At-rest encryption** — fleet state (including agent transcripts) stored encrypted on disk vs plaintext under the storage root
2. **Storage file permissions** — restrict fleet-state files to the current OS user vs no extra permission rule beyond what the OS/default directory already provides

Want to pursue any of these (or a close variant)?

**Recommend:** 1 unless transcripts-with-secrets make encryption or user-only permissions clearly binding for this phase.
Which do you want?

**Selected:** 1

**Answer:** 

## q-080

The requirement on **format upgrade migration** blocks launch when automatic migration fails and keeps the pre-migration backup available for recovery under the storage root.

How should the user recover from that backup?

**Recommend:** 1 for v1 — keeps recovery simple; the error can point at the backup path without a restore UI.
Which do you want?

**Selected:** 1

**Answer:** 

## q-081

You chose that after a successful automatic format upgrade, the pre-migration backup is removed automatically (not kept until the user deletes it).

When should that removal happen?

**Recommend:** 1 — simplest durable rule; users who want a copy can duplicate the storage root before upgrading.
Which do you want?

**Selected:** 1

**Answer:** 

## q-083

The requirements on **agent reconnect relaunch** and **shell session relaunch** reattach to still-reachable processes after relaunch.

Must tod verify that a candidate process is the same agent or shell session (to guard against PID reuse), or is matching the recorded process identifier enough?

**Recommend:** 1 — after quit or host churn, a recycled PID could belong to an unrelated process.
Which do you want?

**Selected:** 1

**Answer:** 

## q-084

The requirement on **write timing** says that when quit proceeds, tod flushes any pending debounced fleet-state writes before the process exits.

If that flush fails (for example the disk is full), what should tod do?

**Recommend:** 1 — otherwise the flush-before-exit guarantee is hollow on the failure path.
Which do you want?

**Selected:** 1

**Answer:** 

## q-085

The requirement on **write timing** allows a crash or force-quit to lose at most a few seconds of recent edits.

What maximum loss window should this task require?

**Recommend:** 1 — small enough to feel safe and concrete enough to verify.
Which do you want?

**Selected:** 1

**Answer:** 

## q-086

An agent left in **not-running** after failed reattach can still be shut down to remove it from the managed fleet.

Does shutting down a not-running agent require confirmation?

**Recommend:** 1 — the process is already gone; matches Waiting (no confirmation) more than Processing/Blocked.
Which do you want?

**Selected:** 1

**Answer:** 

## q-082

I’ve reviewed the current requirements for this phase. These top-level areas look complete and coherent for a reasonable persistence task — I don’t see a compelling gap to propose next:

1. **Scope**
2. **Entity restore**
3. **Durability and writes**
4. **Quit and in-flight**
5. **Storage root and format**
6. **Agent relaunch**
7. **Integrity and recovery**
8. **Scale and retention**

Did we miss anything important?

**Recommend:** 1 unless you see a real gap.
Which do you want?

**Selected:** 1

**Answer:** 

## q-087

Proposed addition to **format upgrade migration** (you chose a sibling backup under the storage root):

> Before modifying on-disk files during automatic format upgrade, tod creates a backup copy of the pre-migration store as a sibling under the storage root. When automatic migration fails, the clear error names that backup path.

**Recommend:** 1 — matches the gathered location without adding a separate backup-path setting.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-088

Proposed update to **invalid storage root** (you chose that the user must still reach the path setting while launch is blocked):

> On launch, if the configured fleet-state storage root is missing, not a directory, or not writable, tod blocks launch with a clear error and still exposes a minimal settings UI so the user can fix the path before continuing.

**Recommend:** 1 — otherwise “fix in settings” cannot work when the main app never opens.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-089

Proposed addition to **agent reconnect relaunch** (you chose live status over last-persisted after successful reattach):

> After a successful reattach to a still-reachable agent process, tod shows that agent's live runtime status from the process and persists that status as the agent's last runtime status.

**Recommend:** 1 — last-persisted can be stale once the live process is reachable again.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-090

Proposed clarification on **agent reconnect relaunch** (you chose not-running as distinct from Waiting/Blocked after failed reattach):

> When reattachment fails because the underlying process is no longer reachable or identity verification fails, tod persists **not-running** as the agent's runtime status — an additional status beyond Starting, Processing, Blocked, and Waiting. The agent record remains in the managed fleet; the user can relaunch manually or shut it down.

**Recommend:** 1 — keeps “in fleet, process gone” distinct from Waiting or Blocked.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-091

Parent **agent runtime status** lets the user submit a new prompt without relaunching when an agent is Waiting or Blocked.

For an agent left in **not-running** after failed reattach, can the user submit a new prompt without manually relaunching first?

**Recommend:** 1 — the process is gone; a prompt needs a running agent (unlike Waiting/Blocked).
Which do you want?

**Selected:** 1

**Answer:** 

## q-092 (action: reconsider)

When the user manually relaunches an agent left in **not-running** after failed reattach, **agent metadata relaunch** already restores environment type and mode.

Should that manual relaunch reuse the last persisted environment type and mode by default?

**Recommend:** 1 — matches restored metadata and avoids re-asking what was already configured.
Which do you want?



## q-093

Parent **destructive-action confirmation** warns before shutdown when the agent's working set has uncommitted changes.

When shutting down an agent in **not-running** (process already gone), should tod still warn if that agent's isolated worktree has uncommitted changes?

**Recommend:** 1 — process gone does not mean a clean worktree; reclaim still risks losing local edits.
Which do you want?

**Selected:** 1

**Answer:** 

## q-094

I’ve reviewed the current constraints for this phase. These top-level areas look complete and coherent for a reasonable persistence task — I don’t see a compelling gap to propose next:

1. **Fleet entities only**
2. **JSON import out of scope**
3. **Configurable storage root**
4. **Default storage root**
5. **Cross-OS storage portability**
6. **Fleet-state export out of scope**

Did we miss anything important?

**Recommend:** 1 unless you see a real gap.
Which do you want?

**Selected:** 1

**Answer:** 

## q-095

Proposed addition to **agent metadata relaunch** (dirty-worktree warning on not-running shutdown requires the worktree to still be findable):

> After quit and relaunch, each local or devcontainer agent that had an isolated git worktree also restores enough worktree identity (path or equivalent) to dirty-check, reclaim, and manually relaunch against that same worktree.

**Recommend:** 1 — without this, not-running dirty-worktree warnings and reclaim cannot work after relaunch.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-096

**Quit with in-flight items** allows "quit anyway" while prompts are still in flight. **Agent reconnect relaunch** reattaches to still-reachable processes and shows live status.

If the user quits anyway while an agent is Processing and that process is still running after relaunch, what should tod do?

**Recommend:** 1 — process still reachable; reconnect + live status already define that path (interrupted transcript applies to force-quit / lost process cases).
Which do you want?

**Selected:** 1

**Answer:** 

## q-097

Parent **human-in-the-loop** auto-adds a notification when an agent enters Blocked.

When relaunch leaves an agent in **not-running** after failed reattach, should tod also add a notification to the open queue?

**Recommend:** 2 for v1 — not-running is visible on the agent; auto-notify every failed reattach could flood the queue after a host reboot.
Which do you want?

**Selected:** 2

**Answer:** 

## q-098

Proposed update to constraint **Fleet entities only** (you accepted a minimal settings UI so **invalid storage root** can be fixed while launch is blocked):

> Fleet entities only — Application settings and preferences (project requirement 26) are out of scope except the fleet-state storage root path exposed by the minimal settings UI when launch is blocked for an invalid storage root; this task covers fleet state per project requirement 23.

**Recommend:** 1 — removes the tension between “settings out of scope” and the accepted recovery path.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-100

**Agent reconnect relaunch** adds **not-running** as a runtime status beyond the parent project's Starting / Processing / Blocked / Waiting set.

For this phase, is that addition task-scoped only, or must the parent project agent runtime status set also be updated to include not-running?

**Recommend:** 1 for this interview — keep the persistence task unblocked; park a parent `user.md` status-set update for a follow-up if you want fleet-wide wording aligned.
Which do you want?

**Selected:** 1

**Answer:** 

## q-103

The requirement on **invalid storage root** blocks launch and still exposes a minimal settings UI so the user can fix the path.

After the user sets a valid, writable storage root in that UI, what should happen?

**Recommend:** 1 — otherwise fixing the path still leaves the user stuck until they quit and relaunch manually.

Which do you want?

**Selected:** 2

**Answer:** 

## q-104

**Shell session relaunch** restores reconnectable shell sessions when the underlying process still exists. **Agent reconnect relaunch** can leave the agent itself in **not-running**.

If an agent is **not-running** but a restored shell session's process is still running, can the user reconnect to that shell?

**Recommend:** 1 — shell identity and agent runtime status are separate; a live shell should stay usable.
Which do you want?

**Selected:** 1

**Answer:** 

## q-105

**First launch bootstrap** creates an empty store when none exists at the configured root. **Invalid storage root** blocks launch when the root is missing, not a directory, or not writable.

If the configured storage root path does not exist yet as a directory, which rule applies?

**Recommend:** 1 — missing directory on an intended root should bootstrap; “not a directory” / not writable still block via invalid-root.
Which do you want?

**Selected:** 1

**Answer:** 

## q-106

When the user manually relaunches an agent left in **not-running** after failed reattach, **agent metadata relaunch** already restores environment type and mode.

Should that manual relaunch reuse the last persisted environment type and mode by default?

**Recommend:** 1 — matches restored metadata and avoids re-asking what was already configured.
Which do you want?

**Selected:** 1

**Answer:** 

## q-107

Constraint **Fleet entities only** keeps general preferences out of scope except the minimal invalid-root path UI. Requirement **storage root change** describes migration when the configured root changes.

Who delivers the normal (non-blocked-launch) UI control that changes the storage root path?

**Recommend:** 1 — parent **application settings** owns the control; this task owns migration and fleet behavior when that configured path changes.
Which do you want?

**Selected:** 1

**Answer:** 

## q-108

**Shell session relaunch** lets the user reconnect to a still-running restored shell even when the agent is **not-running**.

While an agent is **not-running**, can the user also launch a *new* shell into that agent's environment?

**Recommend:** 2 for v1 — environment may be gone with the agent process; reconnect covers live leftovers without implying a full environment is still up.
Which do you want?

**Selected:** 2

**Answer:** 

## q-109

**Invalid storage root** requires a full app restart after the user sets a valid path in the minimal settings UI.

When is that corrected storage root path persisted?

**Recommend:** 1 — otherwise the restart still boots against the old invalid path.
Which do you want?

**Selected:** 1

**Answer:** 

## q-111

**First launch bootstrap** creates the storage root directory when it does not exist yet.

If tod cannot create that directory (for example permissions deny it), what should happen?

**Recommend:** 1 — same recovery path as other root problems; do not pretend the fleet is durable.
Which do you want?

**Selected:** 1

**Answer:** 

## q-112

**Shell session relaunch** keeps a restored shell listed with a not-running indicator when its process is gone (or identity verification fails).

Can the user dismiss or remove that dead shell session entry from the agent detail page?

**Recommend:** 1 — otherwise not-running shell rows accumulate with no cleanup short of shutting down the agent.
Which do you want?

**Selected:** 1

**Answer:** 

## q-113

**Storage root change** lets the user point fleet state at a new path (copy, move, or create new). **First launch bootstrap** creates a missing storage root directory when needed.

If the user chooses a new storage root path that does not exist yet as a directory, should tod create that directory as part of the storage-root change?

**Recommend:** 1 — matches bootstrap creating missing roots; otherwise every new path must be pre-created by hand.
Which do you want?

**Selected:** 1

**Answer:** 

## q-114

**Storage root change** offers **create new**, which starts an empty store at the new root and leaves the previous root’s fleet state untouched.

If agents from the previous fleet still have live processes when the user chooses create new, what should tod do?

**Recommend:** 1 — create-new abandons that fleet in the UI; orphaning live agent processes is a footgun.
Which do you want?

**Selected:** 1

**Answer:** 

## q-115

Proposed addition to **invalid storage root** (you chose persist-on-confirm before the required restart):

> After the user sets a valid, writable storage root in that UI, tod persists the corrected path immediately on confirm (before the required full app restart).

**Recommend:** 1 — otherwise the restart still boots against the old invalid path.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-116

Proposed addition to **first launch bootstrap** (you chose block as invalid when directory create fails):

> If tod cannot create the storage root directory (for example permissions deny it), tod treats that as an invalid storage root: launch is blocked with a clear error and the minimal settings UI remains available so the user can fix the path.

**Recommend:** 1 — same recovery path as other root problems; do not pretend the fleet is durable.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-117

Proposed addition to **shell session relaunch** (you chose that the user can dismiss dead sessions):

> When a restored shell session is listed with a not-running indicator (process gone or identity verification failed), the user can dismiss or remove that session entry from the agent detail page.

**Recommend:** 1 — otherwise not-running shell rows accumulate with no cleanup short of shutting down the agent.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-118

Proposed addition to **storage root change** (you chose create the missing directory as part of the change):

> If the user chooses a new storage root path that does not exist yet as a directory, tod creates that directory as part of the storage-root change.

**Recommend:** 1 — matches bootstrap creating missing roots; otherwise every new path must be pre-created by hand.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-119

Proposed addition to **storage root change** (you chose block create-new while prior agents still have live processes):

> Create new is blocked while any agent from the previous fleet is still present with a runtime status other than not-running; the user must shut those agents down (or wait until they are not-running) before create new can proceed.

**Recommend:** 1 — create-new abandons that fleet in the UI; orphaning live agent processes is a footgun.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-121

**Agent reconnect relaunch** reuses the last persisted environment type and mode by default when manually relaunching a not-running agent.

Can the user change environment type or mode for that relaunch before it starts?

**Recommend:** 1 — defaults remove friction; override covers intentional switches.
Which do you want?

**Answer:** So there's a possibility I may have answered incorrectly previously, but I want to make one thing really clear: a given agent can never change its environment type. If the user wants an agent on a different environment type, then it needs to just create a new one on the new environment type, and then it can shut down the old one if it doesn't need it. A single agent cannot migrate environments or anything like that. The environment type is inherent to the nature of a given agent.

## q-123

**Agent metadata relaunch** restores enough worktree identity for local/devcontainer agents to manually relaunch against that same worktree. **Agent reconnect relaunch** already reuses last environment type and mode by default on manual relaunch.

When manually relaunching a **not-running** local or devcontainer agent that has restored worktree identity, should tod relaunch into that same worktree by default?

**Recommend:** 1 — that is why worktree identity is restored.
Which do you want?

**Selected:** 1

**Answer:** 

## q-124

**Storage root change** for copy or move proceeds when the in-flight count reaches zero. Agents may still be Waiting with live processes.

When the user chooses **copy** or **move**, must those still-running Waiting agents be shut down (or reach not-running) before migration proceeds?

**Recommend:** 1 — copy/move keep fleet continuity; live processes can reattach against the migrated store.
Which do you want?

**Selected:** 1

**Answer:** 

## q-125

**Shell session relaunch** now lets the user dismiss a restored shell session listed as not-running.

After that dismissal, must it stay gone across quit and relaunch?

**Recommend:** 1 — otherwise dismiss is only a temporary UI hide.
Which do you want?

**Selected:** 1

**Answer:** 

## q-126

**Storage root change** creates a missing new-root directory when needed.

If creating that directory fails (for example permissions deny it), what should tod do?

**Recommend:** 1 — do not leave the configured root pointing at an unusable path.
Which do you want?

**Selected:** 1

**Answer:** 

## q-127

**Storage root change** copy leaves fleet state at the previous root and also creates a duplicate at the new root.

After a successful **copy** migration, which path is the configured/active storage root tod uses?

**Recommend:** 1 — the user changed the root to the new location; the previous root is the left-behind duplicate.
Which do you want?

**Selected:** 1

**Answer:** 

## q-128

**Storage root change** copy/move migrates fleet state (tasks, agents, transcripts, notifications). Open shell sessions are fleet state too.

When the user chooses **copy** or **move**, are open shell session records included in the migrated fleet state?

**Recommend:** 1 — matches full fleet-state parity; create-new starts empty so it does not apply.
Which do you want?

**Selected:** 1

**Answer:** 

## q-129

**Shell session relaunch** lets the user dismiss a restored shell session listed as not-running.

Does that dismiss require confirmation?

**Recommend:** 1 — the session is already dead; confirmation would only slow cleanup.
Which do you want?

**Selected:** 1

**Answer:** 

## q-130

You clarified that an agent's **environment type** never changes. **Agent reconnect relaunch** still reuses the last persisted **mode** (autonomous vs interactive) by default on manual relaunch of a not-running agent.

Can the user change mode for that relaunch before it starts?

**Recommend:** 1 — parent product already allows switching mode on live agents; relaunch should not be stricter.
Which do you want?

**Selected:** 1

**Answer:** 

## q-131

You chose that manually relaunching a **not-running** local or devcontainer agent reuses the restored worktree **by default**.

Can the user choose a different path or a fresh worktree for that relaunch instead?

**Recommend:** 1 — default preserves continuity; override covers a missing or unwanted worktree without inventing a new agent.
Which do you want?

**Selected:** 1

**Answer:** 

## q-133

Proposed addition to **shell session relaunch** (you chose that dismissing a not-running restored shell must survive relaunch):

> After the user dismisses or removes a restored shell session listed with a not-running indicator, that session entry remains absent after quit and relaunch.

**Recommend:** 1 — otherwise dismiss is only a temporary UI hide.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-134

Proposed addition to **storage root change** (you chose block the change and keep the previous root when creating the new-root directory fails):

> If tod cannot create the new storage root directory as part of a storage-root change (for example permissions deny it), tod blocks the change with a clear error and keeps the previous root configured.

**Recommend:** 1 — do not leave the configured root pointing at an unusable path.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-135

Proposed clarification to **agent reconnect relaunch** (you said environment type is inherent to an agent and never changes):

> An agent's environment type is fixed for that agent's lifetime and cannot be changed on relaunch or otherwise. When the user manually relaunches an agent left in not-running after failed reattachment, tod always uses that agent's persisted environment type. To run on a different environment type, the user launches a new agent (and may shut down the old one).

**Recommend:** 1 — matches your clarification; mode stays out of this wording until that gather lands.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-136

Proposed clarification to **storage root change** (you chose that after a successful **copy**, the new path is the configured/active root and the previous root keeps the left-behind duplicate):

> After a successful copy migration, the new path becomes the configured storage root tod uses; fleet state also remains at the previous root as a duplicate.

**Recommend:** 1 — the user changed the root to the new location; the previous root is the left-behind copy.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-137

Proposed clarification to **storage root change** (you chose that **copy** and **move** proceed while Waiting agents with live processes may remain running once the in-flight count is zero):

> Copy and move are not blocked by agents that remain Waiting with live processes after the in-flight count reaches zero; those agents may stay running and reattach against the migrated store.

**Recommend:** 1 — copy/move keep fleet continuity; create-new already blocks on live prior-fleet agents separately.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-138

Proposed clarification to **storage root change** (you chose that open shell session records are included when copy/move migrates fleet state):

> When the user chooses copy or move, open shell session records are included in the migrated fleet state along with tasks, agents, transcripts, and notifications.

**Recommend:** 1 — matches full fleet-state parity under **fleet-state persistence**.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-139

Proposed clarification to **shell session relaunch** (you chose that dismissing a not-running restored shell does not require confirmation):

> When a restored shell session is listed with a not-running indicator, dismissing or removing that session entry does not require confirmation.

**Recommend:** 1 — the session is already dead; confirmation would only slow cleanup.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-140

Proposed clarification to **agent reconnect relaunch** (you chose that manual relaunch of a not-running agent defaults to the last persisted mode and the user can change mode before it starts):

> When the user manually relaunches an agent left in not-running after failed reattachment, tod reuses the last persisted mode (autonomous vs interactive) by default, and the user can change mode for that relaunch before it starts.

**Recommend:** 1 — defaults remove friction; override matches live mode switching elsewhere.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-141

Proposed clarification to **agent metadata relaunch** (you chose reuse of the restored worktree by default on manual relaunch, and that the user can pick a different path or a fresh worktree instead):

> When the user manually relaunches a not-running local or devcontainer agent that has restored worktree identity, tod relaunches into that same worktree by default, and the user can choose a different path or a fresh worktree for that relaunch instead.

**Recommend:** 1 — default preserves continuity; override covers a missing or unwanted worktree.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-143

**Agent metadata relaunch** restores worktree identity for local/devcontainer agents. Manual relaunch reuses that worktree by default.

If that restored path no longer exists on disk when the user manually relaunches, what should tod do?

**Recommend:** 1 — do not silently invent a new worktree; the override path already covers choosing fresh.
Which do you want?

**Selected:** 1

**Answer:** 

## q-144

I’ve reviewed the current requirements for this phase. These top-level areas look complete and coherent for a reasonable persistence task — I don’t see a compelling gap to propose next:

1. **Scope**
2. **Entity restore**
3. **Durability and writes**
4. **Quit and in-flight**
5. **Storage root and format**
6. **Agent relaunch**
7. **Integrity and recovery**
8. **Scale and retention**

Did we miss anything important?

**Recommend:** 1 unless you see a real gap.
Which do you want?

**Selected:** 1

**Answer:** 

## q-145

I’ve reviewed the current constraints for this phase. These top-level areas look complete and coherent for a reasonable persistence task — I don’t see a compelling gap to propose next:

1. **Fleet entities only**
2. **JSON import out of scope**
3. **Configurable storage root**
4. **Default storage root**
5. **Cross-OS storage portability**
6. **Fleet-state export out of scope**

Did we miss anything important?

**Recommend:** 1 unless you see a real gap.
Which do you want?

**Selected:** 1

**Answer:** 

## q-146

You chose that manual relaunch of a **not-running** agent defaults to the last persisted mode and the user can change mode before it starts.

When the user changes mode for that relaunch, is the chosen mode what tod persists as the agent's last mode going forward?

**Recommend:** 1 — relaunch is a real launch; the mode in effect should be what survives next quit.
Which do you want?

**Selected:** 1

**Answer:** 

## q-147

You chose that manual relaunch of a **not-running** local or devcontainer agent can use a different path or a fresh worktree instead of the restored one.

When the user chooses that override and relaunch succeeds, is the chosen worktree what tod persists as the agent's worktree identity going forward (dirty-check, reclaim, later relaunch)?

**Recommend:** 1 — same as a real launch; the worktree in effect should be what survives next quit.
Which do you want?

**Selected:** 1

**Answer:** 

## q-148

When manually relaunching a **not-running** local or devcontainer agent, the user can choose a different existing path instead of the restored worktree.

If that path is already the restored worktree of another agent in the fleet, what should tod do?

**Recommend:** 1 — isolated worktrees are per-agent; sharing invites cross-agent dirty/reclaim surprises.
Which do you want?

**Selected:** 1

**Answer:** 

## q-149

**Storage root change** already rolls back on the next launch after a **force-quit** mid copy/move migration.

If the user attempts a normal quit while a copy or move migration is still in progress, what should tod do?

**Recommend:** 1 — ordinary quit should not leave migration half-done; cancel remains available if they want out.
Which do you want?

**Selected:** 1

**Answer:** 

## q-150

**Storage root change** blocks further fleet mutations while copy or move migration is pending or in progress. **Copy** and **move** may leave Waiting agents running so they can reattach afterward.

If a still-running agent produces transcript or status updates during that migration, what should tod do with those agent-triggered fleet-state writes?

**Recommend:** 1 — same mutation freeze as the UI; writing mid-copy/move risks a torn store.
Which do you want?

**Selected:** 1

**Answer:** 

## q-151

You chose that a successful worktree override on manual relaunch of a **not-running** local or devcontainer agent persists the chosen worktree as the agent's identity going forward.

What should tod do with the **previous** restored worktree path after that override succeeds?

**Recommend:** 2 — same dirty-worktree safety as reclaim elsewhere; do not silently delete.
Which do you want?

**Answer:** You've asked a lot of questions about this not-running state, and it's kind of wasting time. Not running is kind of the equivalent of "it's time to start over," so I want to keep that simple. We need to make that more simple. We don't need a huge amount of error handling or recovery stuff around it, so let's just keep it really simple.

I was only even saying that you could restart it just for the convenience of it. I'm not sure if it even makes any sense. All it does is restart the config. It's literally a new agent. It has nothing to do with the old agent at all.

That being said, I probably said the wrong thing before. If somebody tries to relaunch a not-running agent and there is an error about "the work tree doesn't exist" or something like that, then we should probably just delete that agent. We need to keep this simpler, because if the work tree is gone, then that agent doesn't really count anymore. It doesn't need to be there anymore.

## q-152

You chose that agent-triggered fleet-state writes during a **copy** or **move** storage-root migration are queued/blocked until migration finishes or cancels.

When migration **succeeds**, what should tod do with those held agent-triggered writes?

**Recommend:** 1 — agents may still be Running/Waiting; their updates belong on the active store after cutover.
Which do you want?

**Selected:** 1

**Answer:** 

## q-153

Proposed clarification to **agent metadata relaunch** (you chose fail relaunch when the restored worktree path is missing until the user picks another or fresh):

> When the user manually relaunches a not-running local or devcontainer agent whose restored worktree path no longer exists on disk, tod fails that relaunch until the user chooses a different path or a fresh worktree.

**Recommend:** 1 — matches your choice; override already covers picking fresh.
Accept, modify, or reject this wording?

**Answer:** No, I just clarified that in a previous one, but no, they can't choose a different path. We just need to delete that agent because if that work tree is gone, then that agent doesn't really exist anymore either.

## q-154

Proposed clarification to **agent reconnect relaunch** (you chose that a mode change at manual relaunch becomes the persisted last mode):

> When the user changes mode for a manual relaunch of a not-running agent, the mode chosen for that relaunch becomes the agent's persisted last mode going forward.

**Recommend:** 1 — relaunch is a real launch; the mode in effect should survive the next quit.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-155 (action: reconsider)

Proposed clarification to **agent metadata relaunch** (you chose that a successful worktree override becomes the persisted identity):

> When the user chooses a different path or a fresh worktree for a manual relaunch of a not-running local or devcontainer agent and that relaunch succeeds, tod persists the chosen worktree as that agent's worktree identity going forward.

**Recommend:** 1 — same as a real launch; the worktree in effect should survive the next quit.
Accept, modify, or reject this wording?



## q-156 (action: reconsider)

Proposed clarification to **agent metadata relaunch** (you chose block when the override path is another agent's restored worktree):

> When the user chooses a different existing path for a manual relaunch of a not-running local or devcontainer agent, if that path is already the restored worktree of another agent in the fleet, tod blocks the relaunch until the user picks a free path or a fresh worktree.

**Recommend:** 1 — isolated worktrees are per-agent; sharing invites cross-agent dirty/reclaim surprises.
Accept, modify, or reject this wording?



## q-157

Proposed clarification to **storage root change** (you chose block normal quit while copy/move migration is in progress until it finishes or the user cancels):

> If the user attempts a normal quit while a copy or move migration is still in progress, tod blocks quit until the migration finishes or the user cancels the migration.

**Recommend:** 1 — ordinary quit should not leave migration half-done; cancel remains available if they want out.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-158

You chose that agent-triggered fleet-state writes during a **copy** or **move** storage-root migration are queued/blocked until migration finishes or cancels.

When the user **cancels** that migration, what should tod do with those held agent-triggered writes?

**Recommend:** 1 — cancel rolls back to the pre-migration root; do not replay mid-migration updates onto the restored prior store.
Which do you want?

**Selected:** 1

**Answer:** 

## q-159

Proposed clarification to **storage root change** (you chose that agent-triggered fleet-state writes during copy/move migration are queued/blocked until finish or cancel):

> While a copy or move storage-root migration is pending or in progress, tod queues or blocks agent-triggered fleet-state writes until the migration finishes or the user cancels.

**Recommend:** 1 — same mutation freeze as the UI; writing mid-copy/move risks a torn store.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-153

Proposed clarification to **agent metadata relaunch** / **agent reconnect relaunch** (you corrected earlier direction — keep not-running simple; if the worktree is gone the agent no longer counts):

> When the user manually relaunches a not-running local or devcontainer agent and the restored worktree path no longer exists on disk (or relaunch fails because that worktree is missing), tod deletes that agent from the managed fleet.

**Recommend:** 1 — matches your correction; missing worktree means start-over, not recovery UX.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-161

Proposed clarification to **storage root change** (you chose that held agent-triggered writes during copy/move migration are applied when migration succeeds):

> After a successful copy or move storage-root migration, tod applies any agent-triggered fleet-state writes that were queued or blocked during the migration to the active store at the new root.

**Recommend:** 1 — agents may still be running/Waiting; their updates belong on the active store after cutover.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-162

Proposed clarification to **storage root change** (you chose that held agent-triggered writes are discarded when the user cancels copy/move migration):

> When the user cancels a pending or in-progress copy or move storage-root migration, tod discards any agent-triggered fleet-state writes that were queued or blocked during the migration (does not apply them to the rolled-back pre-migration store).

**Recommend:** 1 — cancel rolls back; do not replay mid-migration updates onto the restored prior store.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-165

You chose that agent-triggered fleet-state writes during a **copy** or **move** storage-root migration are queued/blocked until finish or cancel. Cancel discards them; success applies them at the new root.

If that migration **fails partway** (not user cancel) and tod leaves both roots as-is with the pre-migration root still active, what should tod do with those held agent-triggered writes?

**Recommend:** 1 — cutover did not happen; still-running agents belong on the prior root that remains active.
Which do you want?

**Selected:** 1

**Answer:** 

## q-168

**Write timing** flushes pending debounced fleet-state writes before quit. **Storage root change** starts copy or move migration immediately after the user confirms.

Before tod begins a **copy** or **move** storage-root migration, should it flush any pending debounced fleet-state writes first?

**Recommend:** 1 — same durability idea as quit; do not leave recent edits only in memory when the store is about to be copied or moved.
Which do you want?

**Selected:** 1

**Answer:** 

## q-170

**Storage root change** offers **create new**, which starts an empty store at the new root and leaves fleet state at the previous root untouched.

Before tod switches to that new empty root, should it flush any pending debounced fleet-state writes to the **previous** root first?

**Recommend:** 1 — the previous root remains the user’s leftover fleet; recent edits should land there before the app points at the empty new root.
Which do you want?

**Selected:** 1

**Answer:** 

## q-171

**Storage root change** lets the user cancel a pending or in-progress **copy** or **move** migration, which rolls back to the pre-migration root.

Does that cancel require confirmation?

**Recommend:** 1 — cancel is already an explicit escape from a blocking migration; a second confirm mainly slows recovery.
Which do you want?

**Selected:** 1

**Answer:** 

## q-172

You said **not-running** should stay simple ("time to start over"; relaunch is basically restarting config / like a new agent).

When the user uses **manual relaunch** on a not-running agent (and the restored worktree still exists when required), what should tod do?

**Recommend:** 1 — keep one fleet entry and transcript history; still a simple restart of the same config with no override recovery UX.
Which do you want?

**Selected:** 1

**Answer:** 

## q-173

**Shell session relaunch** allows reconnect to still-running restored shells even when the agent is **not-running**. Shutting down a not-running agent tears down the agent and removes it from the fleet.

If that not-running agent still has restored shell sessions whose processes are alive, what should shutdown do with those shell processes?

**Recommend:** 1 — shutdown means tear down the environment; leaving orphan shells is surprising.
Which do you want?

**Selected:** 1

**Answer:** 

## q-174

Proposed clarification to **storage root change** (you chose that on copy/move migration fail partway, held agent-triggered writes are applied to the still-active pre-migration store):

> When a copy or move storage-root migration fails partway and the pre-migration root remains active, tod applies any agent-triggered fleet-state writes that were queued or blocked during the migration to that still-active pre-migration store.

**Recommend:** 1 — cutover did not happen; still-running agents belong on the prior root that remains active.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-175

Proposed clarification to **storage root change** (you chose flush pending debounced fleet-state writes before copy or move migration begins):

> Before tod begins a copy or move storage-root migration, tod flushes any pending debounced fleet-state writes.

**Recommend:** 1 — same durability idea as quit; do not leave recent edits only in memory when the store is about to be copied or moved.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-176

Proposed clarification to **storage root change** (you chose flush pending debounced fleet-state writes to the previous root before switching on create new):

> Before tod switches to a create-new empty storage root, tod flushes any pending debounced fleet-state writes to the previous root.

**Recommend:** 1 — the previous root remains the leftover fleet; recent edits should land there before the app points at the empty new root.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-177

Proposed clarification to **storage root change** (you chose that cancel of a pending or in-progress copy/move migration does not require confirmation):

> Canceling a pending or in-progress copy or move storage-root migration does not require confirmation.

**Recommend:** 1 — cancel is already an explicit escape from a blocking migration; a second confirm mainly slows recovery.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-178

Proposed clarification to **shell session relaunch** / **agent reconnect relaunch** (you chose that shutting down a not-running agent tears down any still-alive restored shell processes with the agent/environment):

> When the user shuts down a not-running agent that still has restored shell sessions whose processes are alive, tod tears down those shell processes along with the agent and its environment.

**Recommend:** 1 — shutdown means tear down the environment; leaving orphan shells is surprising.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-175

Proposed clarification to **storage root change** / **write timing** (you chose flush pending debounced writes to the previous root before create new switches):

> Before tod switches to a new empty storage root for create new, tod flushes any pending debounced fleet-state writes to the previous root.

**Recommend:** 1 — matches your choice; the leftover prior-root fleet should get recent edits first.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-176

Proposed clarification to **storage root change** (you chose that cancel does not require confirmation):

> Canceling a pending or in-progress copy or move storage-root migration does not require confirmation.

**Recommend:** 1 — matches your choice; cancel is already the explicit escape from a blocking migration.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-177

Proposed clarification to **storage root change** (you chose that on copy/move failure partway, held agent-triggered writes go to the still-active pre-migration root):

> When a copy or move storage-root migration fails partway (not user cancel) and tod leaves both roots as-is with the pre-migration root still active, tod applies any agent-triggered fleet-state writes that were queued or blocked during the migration to the active store at the pre-migration root.

**Recommend:** 1 — matches your choice; cutover did not happen, so still-running agents belong on the prior root.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-178

Proposed clarification to **agent reconnect relaunch** (you chose keep the same agent record and restart the process with restored config):

> When the user manually relaunches a not-running agent and relaunch proceeds (including when a local or devcontainer restored worktree still exists), tod keeps that same agent record in the managed fleet and restarts the agent process using its restored configuration; it does not remove the agent and create a new fleet entry.

**Recommend:** 1 — matches your choice; one fleet entry and existing transcript history, with a simple process restart.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-179

**Agent metadata relaunch** / **agent reconnect relaunch** delete a not-running local or devcontainer agent when manual relaunch finds the restored worktree missing.

Does that delete require confirmation?

**Recommend:** 1 — you asked to keep not-running simple; the agent no longer counts, but the user should still see why it disappeared.
Which do you want?

**Selected:** 1

**Answer:** 

## q-180

After a **successful** copy or move storage-root migration, tod applies agent-triggered fleet-state writes that were held during the migration to the new root.

If that post-cutover apply fails (for example disk full), what should tod do?

**Recommend:** 1 — cutover already happened; match ordinary write-failure handling rather than undoing a finished migration.
Which do you want?

**Selected:** 1

**Answer:** 

## q-181

You chose that **manual relaunch** of a not-running agent keeps the same agent record and restarts with restored config.

After that relaunch succeeds, what happens to the agent's prior prompt/response transcript history?

**Recommend:** 1 — same fleet entry should keep continuity; blanking the transcript would fight **agent transcript history**.
Which do you want?

**Selected:** 1

**Answer:** 

## q-182

Proposed clarification to **agent metadata relaunch** / **agent reconnect relaunch** (you chose no confirmation, with a clear notice, when missing-worktree delete happens):

> When tod deletes a not-running local or devcontainer agent because manual relaunch found the restored worktree missing, that delete does not require confirmation; tod shows a clear notice explaining that the agent was removed because the worktree was missing.

**Recommend:** 1 — matches your choice; keep not-running simple, but do not disappear the agent silently.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-183

Proposed clarification to **storage root change** (you chose that post-cutover apply of held writes failing does not undo migration):

> After a successful copy or move storage-root migration, if applying agent-triggered fleet-state writes that were queued or blocked during the migration fails, the migration remains successful; tod shows a clear error and leaves those unapplied held writes unapplied (does not roll back to the pre-migration root).

**Recommend:** 1 — matches your choice; cutover already happened; ordinary write-failure handling applies.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-184

Proposed clarification to **agent reconnect relaunch** / **agent transcript history** (you chose prior transcript remains on the same agent after manual relaunch):

> After a successful manual relaunch of a not-running agent, that agent's prior prompt/response transcript history remains available on the same agent record.

**Recommend:** 1 — matches your choice; same fleet entry keeps continuity with **agent transcript history**.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-185

**Storage root change** flushes pending debounced fleet-state writes before a **copy** or **move** begins. **Write timing** blocks quit when the quit-time flush fails.

If that pre-migration flush fails (for example disk full), what should tod do?

**Recommend:** 1 — same durability idea as quit; do not copy/move a store that is missing recent edits.
Which do you want?

**Selected:** 1

**Answer:** 

## q-186

**Storage root change** flushes pending debounced fleet-state writes to the **previous** root before switching on **create new**.

If that flush to the previous root fails, what should tod do?

**Recommend:** 1 — the previous root is the leftover fleet; do not abandon unflushed edits there.
Which do you want?

**Selected:** 1

**Answer:** 

## q-187

When tod deletes a not-running local or devcontainer agent because the restored worktree is missing, that agent may still have restored shell sessions whose processes are alive.

What should tod do with those shell processes?

**Recommend:** 1 — same as not-running shutdown; deleting the agent should not leave orphan shells.
Which do you want?

**Selected:** 1

**Answer:** 

## q-188

When a **copy** or **move** migration fails partway, tod applies held agent-triggered writes to the still-active pre-migration store.

If that apply fails (for example disk full), what should tod do?

**Recommend:** 1 — migration already failed; match ordinary write-failure handling without inventing a second rollback path.
Which do you want?

**Selected:** 1

**Answer:** 

## q-189

After a successful **manual relaunch** of a not-running agent, that agent may still have restored shell session entries from before the agent was not-running.

Should those restored shell sessions stay reconnectable (when their processes are still running and identity verifies)?

**Recommend:** 1 — shell reconnect is already independent of agent runtime status; relaunch should not wipe live shells.
Which do you want?

**Selected:** 1

**Answer:** 

## q-190

Proposed clarification to **storage root change** (you chose block migration start when the pre-copy/move flush fails):

> If the flush of pending debounced fleet-state writes before a copy or move storage-root migration begins fails, tod blocks migration start with a clear error until the flush succeeds or the user cancels the storage-root change.

**Recommend:** 1 — matches your choice; same durability idea as quit-time flush failure.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-191

Proposed clarification to **storage root change** (you chose block the create-new switch when flush to the previous root fails):

> If the flush of pending debounced fleet-state writes to the previous root before switching to a create-new empty storage root fails, tod blocks the switch with a clear error until the flush succeeds or the user cancels the storage-root change.

**Recommend:** 1 — matches your choice; do not abandon unflushed edits on the leftover prior-root fleet.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-192

Proposed clarification to **agent metadata relaunch** / **agent reconnect relaunch** / **shell session relaunch** (you chose tear down still-alive restored shells when missing-worktree delete removes the agent):

> When tod deletes a not-running local or devcontainer agent because the restored worktree is missing, tod tears down any still-alive restored shell processes for that agent along with the agent and its environment.

**Recommend:** 1 — matches your choice; same as not-running shutdown — do not leave orphan shells.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-193

Proposed clarification to **storage root change** (you chose keep migration-failed state and leave held writes unapplied when fail-partway apply fails):

> When a copy or move storage-root migration fails partway and applying agent-triggered fleet-state writes that were queued or blocked during the migration to the still-active pre-migration store fails, tod keeps the migration-failed state, shows a clear error, and leaves those held writes unapplied.

**Recommend:** 1 — matches your choice; ordinary write-failure handling without a second rollback path.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-194

Proposed clarification to **shell session relaunch** / **agent reconnect relaunch** (you chose restored shells stay reconnectable after successful manual relaunch):

> After a successful manual relaunch of a not-running agent, previously restored shell sessions for that agent remain reconnectable when their processes are still running and pass identity verification.

**Recommend:** 1 — matches your choice; shell reconnect stays independent of the agent having been relaunched.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-195

**Storage root change** discards agent-triggered fleet-state writes held during a copy or move when the user cancels. Force-quit mid-migration rolls back to the pre-migration root on next launch.

What should tod do with those held writes after force-quit mid-migration and rollback?

**Recommend:** 1 — force-quit interrupted the migration like cancel; do not surprise-apply stale held writes on next launch.
Which do you want?

**Selected:** 2

**Answer:** 

## q-196

**Storage root change** flushes pending writes to the previous root before switching on **create new**, then starts an empty store at the new root.

If creating or initializing that empty store at the new root fails after the flush succeeded, what should tod do?

**Recommend:** 1 — previous root is still the good fleet; do not strand the app on a broken new root.
Which do you want?

**Selected:** 1

**Answer:** 

## q-197

When tod deletes a not-running agent because the restored worktree is missing (or the user otherwise removes an agent from the managed fleet), open notifications may still link to that agent.

What should happen to those agent links on open notifications?

**Recommend:** 1 — keep the notification text/queue entry; drop the dead agent link so relaunch does not show a dangling reference.
Which do you want?

**Selected:** 2

**Answer:** 

## q-198

Proposed clarification to **storage root change** (you chose apply held agent-triggered writes to the rolled-back pre-migration store after force-quit mid-migration):

> When the user force-quits while a copy or move storage-root migration is in progress, and the next launch rolls back to the pre-migration root, tod applies any agent-triggered fleet-state writes that were queued or blocked during the migration to that rolled-back pre-migration store.

**Recommend:** 1 — matches your choice; still-running agents belong on the prior root that remains active after rollback.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-199

Proposed clarification to **storage root change** (you chose keep the previous root when create-new empty-store init fails after a successful prior-root flush):

> If creating or initializing the empty store at the new root fails after a successful flush of pending debounced fleet-state writes to the previous root during a create-new storage-root change, tod blocks the switch with a clear error, keeps the previous root configured, and leaves fleet state at the previous root intact.

**Recommend:** 1 — matches your choice; do not strand the app on a broken new root.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-200

You chose that when tod deletes a not-running agent because the restored worktree is missing (or the user otherwise removes an agent from the managed fleet), tod dismisses open notifications that linked to that agent.

Does that dismiss require confirmation?

**Recommend:** 1 — the agent is already gone; a second confirm mainly slows cleanup of dead links.
Which do you want?

**Selected:** 1

**Answer:** 

## q-201

**Corrupted store recovery** blocks launch with a clear error when automatic repair or recovery fails. **Invalid storage root** still exposes a minimal settings UI so the user can fix the path.

When launch is blocked because corrupted-store recovery failed, should that same minimal settings UI be available?

**Recommend:** 1 — same escape hatch as invalid root; the user can point at a different writable root without editing files by hand.
Which do you want?

**Selected:** 1

**Answer:** 

## q-202

**Newer format downgrade** blocks launch with a clear error and leaves the store untouched. **Invalid storage root** still exposes a minimal settings UI so the user can fix the path.

When launch is blocked because the on-disk format is newer than this build supports, should that same minimal settings UI be available?

**Recommend:** 1 — same escape hatch; the user can point at a different root without hand-editing config while launch is blocked.
Which do you want?

**Selected:** 1

**Answer:** 

## q-203

**Storage root change** lets the user pick copy, move, or create new at a new path.

If that chosen new path already contains an existing fleet-state store, what should tod do?

**Recommend:** 1 — avoid silent overwrite or accidental dual-fleet confusion; make the user clear or pick another path.
Which do you want?

**Selected:** 1

**Answer:** 

## q-204

**Storage root change** allows **create new** once every prior-fleet agent is not-running, and leaves fleet state at the previous root untouched.

Those not-running agents may still have restored shell sessions whose processes are alive. What should tod do with those shell processes when create new switches to the new empty root?

**Recommend:** 2 — create new does not manage the leftover prior fleet; "previous root untouched" includes leaving those processes for manual cleanup if desired.
Which do you want?

**Selected:** 2

**Answer:** 

## q-205

**First launch bootstrap** creates an empty store when the storage root is missing or has no fleet-state store yet. Create-new init failure keeps the previous root.

If empty-store creation/init fails on first launch at an existing writable storage root (for example disk fills mid-init), what should tod do?

**Recommend:** 1 — no prior fleet to fall back to; block with a clear error and settings escape rather than running without durability.
Which do you want?

**Selected:** 1

**Answer:** 

## q-206

**Storage root change** keeps the previous root when create-new empty-store init fails after the previous-root flush.

If partial files were already written under the new root during that failed init, what should tod do with them?

**Recommend:** 1 — same cleanup idea as cancel mid-migration; do not leave a half-initialized store beside the still-active previous root.
Which do you want?

**Selected:** 1

**Answer:** 

## q-207

**Storage root change** flushes the previous root before switching on **create new**, then initializes an empty store at the new root. Copy/move force-quit rolls back to the pre-migration root.

If the user force-quits during that create-new switch (after the previous-root flush, before empty-store init finishes), what should next launch do?

**Recommend:** 1 — create new never cut over; keep the known-good previous root and clean up partial new-root work.
Which do you want?

**Selected:** 1

**Answer:** 

## q-208

**Format upgrade migration** creates a pre-migration backup sibling before modifying on-disk files. Storage-root copy/move force-quit rolls back automatically on next launch.

If the user force-quits (or the process dies) during automatic format upgrade after that backup exists and while on-disk files are being modified, what should next launch do?

**Recommend:** 1 — you already paid for a backup; auto-restore matches storage-root force-quit rollback rather than leaving a half-upgraded store.
Which do you want?

**Selected:** 1

**Answer:** 

## q-209

Proposed clarification to **agent metadata relaunch** / **notification relaunch** (you chose dismiss linked open notifications without confirmation when that agent is removed):

> When tod deletes a not-running local or devcontainer agent because the restored worktree is missing (or the user otherwise removes an agent from the managed fleet), tod dismisses open notifications that linked to that agent. That dismiss does not require confirmation.

**Recommend:** 1 — matches your choice; dead agent links should not linger in the open queue.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-210

Proposed clarification to **corrupted store recovery** (you chose the same minimal settings escape as invalid root when recovery fails):

> When automatic repair or recovery fails, launch is blocked with a clear error and the minimal settings UI remains available so the user can fix the storage root path before continuing.

**Recommend:** 1 — matches your choice; same escape hatch as **invalid storage root**.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-211

Proposed clarification to **newer format downgrade** (you chose the same minimal settings escape as invalid root when the on-disk format is newer than this build):

> When launch is blocked because the on-disk format version is newer than this build supports, tod still exposes a minimal settings UI so the user can fix the storage root path before continuing.

**Recommend:** 1 — matches your choice; same escape hatch without hand-editing config.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-212

Proposed clarification to **storage root change** (you chose block when the new path already has a fleet-state store):

> If the user chooses a new storage root path that already contains an existing fleet-state store, tod blocks the storage-root change with a clear error until the user chooses a different path or clears that location.

**Recommend:** 1 — matches your choice; avoid silent overwrite or dual-fleet confusion.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-213

Proposed clarification to **storage root change** (you chose leave prior-root restored shells running on create new):

> When the user switches to a create-new empty storage root, tod does not tear down still-alive restored shell processes from the previous-root fleet as part of that switch; those processes may remain running, and fleet state at the previous root remains untouched.

**Recommend:** 1 — matches your choice; create new does not manage the leftover prior fleet.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-214

Proposed clarification to **first launch bootstrap** (you chose block launch with clear error, leave any partial store as-is, and keep the minimal settings UI available when empty-store init fails):

> If creating or initializing the empty store fails on first launch at an existing writable storage root, tod blocks launch with a clear error, leaves any partial store as-is, and still exposes a minimal settings UI so the user can fix the storage root path before continuing.

**Recommend:** 1 — matches your choice; no prior fleet to fall back to, so block with an escape hatch rather than run without durability.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-215

Proposed clarification to **storage root change** (you chose remove partial new-root contents when create-new empty-store init fails after the previous-root flush):

> If creating or initializing the empty store at the new root fails after a successful flush of pending debounced fleet-state writes to the previous root during a create-new storage-root change, and partial files were already written under the new root, tod removes those partial new-root contents (same idea as cancel mid-migration cleanup).

**Recommend:** 1 — matches your choice; do not leave a half-initialized store beside the still-active previous root.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-216

Proposed clarification to **storage root change** (you chose keep the previous root and clear partial new-root create-new work after force-quit mid switch):

> If the user force-quits during a create-new storage-root switch after the previous-root flush and before empty-store init finishes, the next launch keeps the previous root configured and clears any partial new-root create-new work.

**Recommend:** 1 — matches your choice; create new never cut over, so restore the known-good previous root.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-217

Proposed clarification to **format upgrade migration** (you chose auto-restore from the pre-migration backup after force-quit mid-upgrade):

> If the user force-quits (or the process dies) during automatic format upgrade after the pre-migration backup exists and while on-disk files are being modified, the next launch restores from that pre-migration backup automatically and leaves the format upgrade for a later launch (does not retry the upgrade from the partially migrated store).

**Recommend:** 1 — matches your choice; same rollback idea as storage-root force-quit mid-migration.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-219

**Storage root change** applies agent-triggered fleet-state writes that were held during a copy or move migration when the next launch rolls back after a force-quit mid-migration. Ordinary write-failure handling keeps the last good store when an apply fails after successful cutover or fail-partway.

If that post-rollback apply of held writes fails (for example disk full), what should tod do?

**Recommend:** 1 — matches the other held-write apply failure paths; do not invent a second rollback.
Which do you want?

**Selected:** 1

**Answer:** 

## q-220

Proposed clarification to **storage root change** (you chose keep the rollback, show a clear error, and leave held writes unapplied when post-rollback apply fails):

> When the user force-quits while a copy or move storage-root migration is in progress, and the next launch rolls back to the pre-migration root, tod applies any agent-triggered fleet-state writes that were queued or blocked during the migration to that rolled-back pre-migration store. If that apply fails, tod keeps the rolled-back pre-migration root, shows a clear error, and leaves those held writes unapplied.

**Recommend:** 1 — matches your choice and the other held-write apply failure paths.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** 

## q-221

**Format upgrade migration** auto-restores from the pre-migration backup on next launch after a force-quit mid-upgrade.

If that automatic restore from the pre-migration backup fails, what should tod do?

**Recommend:** 2 — same blocked-launch escape hatch as corrupted recovery and newer-format; do not loop forever on a failing restore.
Which do you want?

**Selected:** 2

**Answer:** 

## q-222

**Format upgrade migration** auto-restores from the pre-migration backup after a force-quit mid-upgrade, then leaves the upgrade for a later launch. After a successful upgrade, the backup is removed immediately.

After a successful auto-restore from that backup, what should tod do with the backup?

**Recommend:** 2 — restore already recovered the pre-migration store; next upgrade attempt can create a fresh backup.
Which do you want?

**Selected:** 2

**Answer:** 

## q-223

**Invalid storage root** persists a corrected path immediately on confirm in the minimal settings UI and requires a full app restart before launch proceeds. The same minimal settings UI is also available when launch is blocked for **corrupted store recovery** failure, **newer format downgrade**, or **first launch** empty-store init failure.

For those other blocked-launch cases, should path-fix in that UI follow the same persist-immediately + full-restart rule?

**Recommend:** 1 — one escape-hatch behavior; continuing in-process after a launch block is risky.
Which do you want?

**Selected:** 1

**Answer:** 

## q-224

**Single storage root instance** blocks a second tod against the same in-use fleet-state storage root.

While a copy or move storage-root migration is in progress, fleet state may also exist at the destination new root.

Should a second tod instance that targets that destination new root also be blocked while the migration is still in progress?

**Recommend:** 1 — avoid a second process touching partial migration output mid-copy/move.
Which do you want?

**Selected:** 1

**Answer:** Yes — treat the destination new root as in-use; block the second instance

## q-225

After a successful **copy** storage-root migration, fleet state remains at the previous root as a duplicate, and the new path becomes the configured root.

May the user launch a second tod instance against that previous-root duplicate (a different storage root path than the running instance)?

**Recommend:** 1 — matches per-root single-instance and “duplicate left in place”; warnings/cleanup can stay manual.
Which do you want?

**Selected:** 1

**Answer:** Allowed — each storage root is independent; single-instance is per root only

## q-226

**First launch bootstrap** blocks launch, leaves any partial store as-is, and keeps the minimal settings UI available when empty-store init fails at an existing writable storage root.

If the user quits and relaunches **without** changing the storage root path (partial store still there), what should tod do?

**Recommend:** 3 — reuse the corrupted/partial recovery path rather than inventing a third bootstrap mode.
Which do you want?

**Selected:** 3

**Answer:** Treat the partial store as corrupted and run the corrupted-store recovery path

## q-227

**Write timing** flushes pending debounced fleet-state writes when quit proceeds, and blocks quit if that flush fails (unless the user force-quits). **Quit with in-flight items** offers "quit anyway," which proceeds immediately and accepts possible loss of in-flight work.

When the user chooses **quit anyway** while the in-flight count is still above zero, should tod still flush pending debounced fleet-state writes before exit?

**Recommend:** 1 — quit-anyway is about abandoning in-flight agent work, not skipping durability of already-accepted UI mutations.
Which do you want?

**Selected:** 1

**Answer:** Still flush pending debounced writes; block on flush failure like normal quit

## q-228

Proposed clarification to **format upgrade migration** (you chose block with a clear error and keep the minimal settings UI when auto-restore from the pre-migration backup fails after force-quit mid-upgrade):

> If automatic restore from the pre-migration backup fails on next launch after force-quit (or process death) mid format upgrade, tod blocks launch with a clear error, leaves the storage-root contents as-is, and still exposes a minimal settings UI so the user can fix the storage root path before continuing.

**Recommend:** 1 — matches your choice and the other blocked-launch escape hatches.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-229

Proposed clarification to **format upgrade migration** (you chose remove the pre-migration backup immediately after a successful auto-restore):

> After a successful automatic restore from the pre-migration backup on next launch after force-quit (or process death) mid format upgrade, tod removes that pre-migration backup immediately.

**Recommend:** 1 — matches your choice; next upgrade attempt can create a fresh backup.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-230

Proposed clarification for path-fix in the minimal settings UI when launch is blocked for **corrupted store recovery** failure, **newer format downgrade**, or **first launch bootstrap** empty-store init failure (you chose the same persist-immediately + full-restart rule as **invalid storage root**):

> After the user sets a valid, writable storage root in that UI, tod persists the corrected path immediately on confirm (before the required full app restart) and requires a full app restart before launch proceeds (does not continue launch in the same process).

**Recommend:** 1 — one escape-hatch behavior across blocked-launch cases.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-231

Proposed clarification to **single storage root instance** / **storage root change** (you chose block a second instance that targets the destination new root while copy or move migration is in progress):

> While a copy or move storage-root migration is in progress, a second tod instance that targets the destination new root is also blocked with a clear error (that destination is treated as in-use alongside the pre-migration root).

**Recommend:** 1 — matches your choice; avoid a second process touching partial migration output.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-232

Proposed clarification to **single storage root instance** / **storage root change** (you chose allow a second instance against the previous-root duplicate after a successful copy):

> After a successful copy storage-root migration, launching a second tod instance against the previous-root duplicate (a different storage root path than the running instance) is allowed; single-instance locking is per storage root only.

**Recommend:** 1 — matches your choice; each root is independent.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-233

Proposed clarification to **first launch bootstrap** / **corrupted store recovery** (you chose treat a leftover partial store as corrupted on relaunch of the same path):

> If creating or initializing the empty store fails on first launch at an existing writable storage root, and the user later relaunches without changing the storage root path while a partial store remains, tod treats that partial store as corrupted and runs the corrupted-store recovery path.

**Recommend:** 1 — matches your choice; reuse corrupted recovery rather than a third bootstrap mode.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-234

Proposed clarification to **write timing** / **quit with in-flight items** (you chose quit-anyway still flushes pending debounced writes and blocks on flush failure like a normal quit):

> When the user chooses "quit anyway" while the in-flight count is still above zero, tod still flushes any pending debounced fleet-state writes before the process exits. If that flush fails, tod blocks quit with a clear error until the flush succeeds or the user force-quits.

**Recommend:** 1 — matches your choice; quit-anyway abandons in-flight agent work, not durability of accepted mutations.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-235

**Single storage root instance** blocks a second tod against the same in-use storage root. **Format upgrade migration** can modify on-disk files for a stretch after launch starts.

While automatic format upgrade migration is in progress (backup created, on-disk files being modified), should a second tod instance that targets that same storage root be blocked?

**Recommend:** 1 — same per-root lock; do not let another process race a mid-upgrade store.
Which do you want?

**Selected:** 1

**Answer:** Yes — block; upgrade holds the root like normal single-instance


## q-236

Proposed clarification to **single storage root instance** (you chose block a second instance while format upgrade is mid-flight):

> While automatic format upgrade migration is in progress (after the pre-migration backup exists and while on-disk files are being modified), a second tod instance that targets that same storage root is blocked with a clear error (the upgrade holds the root like normal single-instance locking).

**Recommend:** 1 — matches your choice; do not let another process race a mid-upgrade store.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-237

Proposed clarification to **format upgrade migration** when auto-restore from the pre-migration backup fails (that path already exposes the minimal settings UI; other blocked-launch cases use persist-immediately + full-restart for path-fix there):

> After the user sets a valid, writable storage root in that UI, tod persists the corrected path immediately on confirm (before the required full app restart) and requires a full app restart before launch proceeds (does not continue launch in the same process).

**Recommend:** 1 — same escape-hatch behavior as **invalid storage root**, **corrupted store recovery**, **newer format downgrade**, and **first launch** init failure.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-238

**Format upgrade migration** restores from the pre-migration backup only after that backup exists and on-disk store files are being modified. Force-quit mid-upgrade in that window auto-restores.

If the user force-quits (or the process dies) **while the pre-migration backup is still being created** (store files not yet modified), what should next launch do?

**Recommend:** 1 — primary store is still intact; drop any incomplete backup sibling and retry a full upgrade on next launch.
Which do you want?

**Selected:** 1

**Answer:** Clear incomplete backup; retry upgrade next launch

## q-239

**Format upgrade migration** blocks launch with a clear error when creating the pre-migration backup fails, and does not proceed until a backup can be created. Other launch blocks (**invalid storage root**, restore-fail, and similar) still expose the minimal settings UI.

When backup creation fails, should that same minimal settings UI be available so the user can change the storage root path before continuing?

**Recommend:** 1 — same escape hatch; disk-full or path problems are often fixed by changing root or freeing space then retrying.
Which do you want?

**Selected:** 1

**Answer:** Yes — block + minimal settings UI

## q-240

**Format upgrade migration** says that when automatic migration fails, launch is blocked with a clear error naming the pre-migration backup path, recovery is by manual filesystem restore (no in-app restore), and pre-migration fleet state at the storage root is left untouched.

On a later relaunch (user has not manually restored or changed the path), what should tod do?

**Recommend:** 1 — store is still the pre-migration original; retrying upgrade is safer than permanent block when nothing was committed.
Which do you want?

**Selected:** 1

**Answer:** Retry automatic format upgrade on next launch

## q-241

**Format upgrade migration** blocks launch when automatic migration fails and names the pre-migration backup path for manual filesystem restore. Other launch blocks still expose the minimal settings UI.

When automatic migration fails that way, should the minimal settings UI also be available so the user can change the storage root path before continuing?

**Recommend:** 1 — path escape remains useful even when primary recovery is manual restore from the named backup.
Which do you want?

**Selected:** 1

**Answer:** Yes — block + minimal settings UI

## q-242

**Single storage root instance** blocks a second tod against an in-use storage root.

If a previous tod process crashed (or was killed) without releasing that single-instance lock, and no live process still holds it, what should the next launch do?

**Recommend:** 1 — otherwise a crash permanently bricks the root until manual lock cleanup.
Which do you want?

**Selected:** 1

**Answer:** Treat lock stale if no live holder; proceed

## q-243

**Format upgrade migration** removes the pre-migration backup immediately after a successful automatic format upgrade (and after a successful auto-restore).

If removing that backup fails (for example permissions or the file is locked), what should tod do?

**Recommend:** 1 — upgrade/restore already succeeded; a leftover backup is harmless and must not block the fleet.
Which do you want?

**Selected:** 1

**Answer:** Continue; leave leftover backup in place


## q-244

Proposed clarification to **format upgrade migration** (you chose clear any incomplete backup and retry upgrade after force-quit during backup creation):

> If the user force-quits (or the process dies) while the pre-migration backup is still being created (before on-disk store files are modified), the next launch clears any incomplete backup sibling and retries a full automatic format upgrade.

**Recommend:** 1 — matches your choice; primary store is still intact.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-245

Proposed clarification to **format upgrade migration** when creating the pre-migration backup fails (you chose keep the block and expose the same minimal settings UI; other blocked-launch escapes persist the corrected path immediately and require a full app restart):

> If creating that backup fails, tod blocks launch with a clear error and does not proceed with the upgrade until a backup can be created. When launch is blocked because creating the pre-migration backup failed, tod still exposes a minimal settings UI so the user can fix the storage root path before continuing. After the user sets a valid, writable storage root in that UI, tod persists the corrected path immediately on confirm (before the required full app restart) and requires a full app restart before launch proceeds (does not continue launch in the same process).

**Recommend:** 1 — matches your choice; same escape hatch as other blocked-launch path fixes.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-246

Proposed clarification to **format upgrade migration** after automatic migration fails with the store left untouched (you chose retry automatic format upgrade on next launch):

> When automatic migration fails, launch is blocked with a clear error that names that backup path; recovery is by manual filesystem restore from that backup (no in-app restore-from-backup action). On a later relaunch without the user having manually restored from that backup or changed the storage root path, tod retries automatic format upgrade.

**Recommend:** 1 — matches your choice; pre-migration store is still the live root.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-247

Proposed clarification to **format upgrade migration** when automatic migration fails (you chose keep the block that names the backup and also expose the minimal settings UI; other blocked-launch escapes persist the corrected path immediately and require a full app restart):

> When automatic migration fails, launch is blocked with a clear error that names that backup path; recovery is by manual filesystem restore from that backup (no in-app restore-from-backup action). When launch is blocked because automatic format upgrade failed, tod still exposes a minimal settings UI so the user can fix the storage root path before continuing. After the user sets a valid, writable storage root in that UI, tod persists the corrected path immediately on confirm (before the required full app restart) and requires a full app restart before launch proceeds (does not continue launch in the same process).

**Recommend:** 1 — matches your choice; path escape alongside manual restore from the named backup.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-248

Proposed clarification to **single storage root instance** (you chose treat a leftover lock as stale when no live holder remains):

> If a previous tod process crashed or was killed without releasing the single-instance lock and no live process still holds it, the next launch treats that lock as stale and proceeds.

**Recommend:** 1 — matches your choice; a crash must not permanently brick the root.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-249

Proposed clarification to **format upgrade migration** when removing the pre-migration backup fails after success (you chose continue and leave the leftover backup):

> If removing the pre-migration backup fails after a successful automatic format upgrade (or after a successful automatic restore from that backup), tod continues; the leftover backup remains in place (does not block launch or the fleet).

**Recommend:** 1 — matches your choice; leftover backup is harmless after success.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-250

**Single storage root instance** already blocks a second tod against the same root while automatic format upgrade is in progress after the pre-migration backup exists and while on-disk files are being modified.

While the pre-migration backup is still being created (store files not yet modified), should a second tod instance that targets that same storage root also be blocked?

**Recommend:** 1 — same per-root lock for the whole upgrade attempt; avoid racing backup creation.
Which do you want?

**Selected:** 1

**Answer:** Yes — block during backup create too

## q-251

**Format upgrade migration** creates a pre-migration backup sibling before modifying the store, and may leave a leftover backup in place if removal fails after a successful upgrade or restore.

When a later automatic format upgrade needs to create its pre-migration backup and a leftover backup sibling from an earlier success is still present, what should tod do?

**Recommend:** 1 — treat leftover as disposable after a prior success; replace it with a fresh pre-migration backup for this upgrade.
Which do you want?

**Selected:** 1

**Answer:** Replace leftover; then create fresh backup


## q-252

Proposed clarification to **single storage root instance** (you chose block a second instance while the pre-migration backup is still being created, same as mid-upgrade):

> While automatic format upgrade migration is in progress — including while the pre-migration backup is being created and after that backup exists while on-disk files are being modified — a second tod instance that targets that same storage root is blocked with a clear error (the upgrade holds the root like normal single-instance locking).

**Recommend:** 1 — matches your choice; one lock for the whole upgrade attempt.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-253

Proposed clarification to **format upgrade migration** (you chose replace a leftover backup from an earlier success, then create a fresh pre-migration backup for the later upgrade):

> When automatic format upgrade needs to create a pre-migration backup and a leftover backup sibling from an earlier successful upgrade or restore is still present, tod replaces that leftover and then creates a fresh pre-migration backup for this upgrade.

**Recommend:** 1 — matches your choice; leftover after success is disposable; this upgrade needs its own backup of the current store.
Accept, modify, or reject this wording?

**Selected:** 1

**Answer:** Accept

## q-254

I’ve reviewed the current requirements for this phase. These top-level areas look complete and coherent for a reasonable persistence task of this kind — I don’t see a compelling gap to propose next:

1. Scope
2. Entity restore
3. Durability and writes
4. Quit and in-flight
5. Storage root and format
6. Agent relaunch
7. Integrity and recovery
8. Scale and retention

Did we miss anything important?

**Recommend:** 1 unless you see a real gap.
Which do you want?

**Selected:** 1

**Answer:** No — this is enough for now

## q-255

I’ve reviewed the current constraints for this phase. These look complete and coherent for a reasonable persistence task of this kind — I don’t see a compelling further constraint category to propose:

1. Fleet entities only
2. JSON import out of scope
3. Configurable storage root
4. Default storage root
5. Cross-OS storage portability
6. Fleet-state export out of scope

Did we miss anything important?

**Recommend:** 1 unless you see a real gap.
Which do you want?

**Selected:** 1

**Answer:** No — this is enough for now

