# The task panel

The panel shown by default when a **task node** is selected (see
`doc/glossary.md`: lifecycle on itself, agent on itself or inherited). Other
node types have their own default panel (generator nodes and managed nodes
already do); every node with none of its own gets the details panel. Status:
built (`crates/tod-ui/src/unified/panels/task.rs`).

The task panel replaces the decisions panel. **Alt+Q** / **Alt+Shift+Q** jump
the tree selection to the next / previous task with a request, longest
waiting first, which shows its task panel.

## The goal

The user picks a task in the node tree because they want to move it along.
The task panel is where they land, so it answers only what they need for that:

1. Am I where I think I am?
2. What is this task made of, and how far along is it?
3. What is its runner doing right now?
4. Is anything waiting on me?

## The rule

**Show only what the user acts on or decides with.** A line that restates
what another line already implies is removed, not shortened. Anything
important enough to show is kept where it cannot scroll out of reach;
anything not important enough is not shown at all.

## Layout

Ordered by permanence: what never changes at the top, what changes by the
minute below it.

```
┌──────────────────────────────────────────────────────────┐
│ Export totals match the ledger                  ENG-412  │  identity
│ Obligations 7 · 1 failed   Plan 5/6   Changes 8   Journey│  artifacts
│ verifying · checking obligation 6 of 7 · 6m · 23k tok  ⏸ │  runner
├──────────────────────────────────────────────────────────┤
│ Rounding: the ledger rounds per line, the export rounds  │
│ the total. Which is correct?                             │  requests
│  1 Round per line   2 Round the total   3 Answer…        │
│  Not in the obligations or the repo · Shouldn't have asked│
├──────────────────────────────────────────────────────────┤
│ ▸ Answered (4)                                           │  drawer
└──────────────────────────────────────────────────────────┘
```

The first three lines are fixed. Only the requests scroll, and the drawer is
anchored to the bottom of the column.

### Identity

The title and the external ticket id (Linear), so the user can confirm they
are on the task they meant.

### Pull-request buttons

In `pr`, `pr_draft` and `pr_review`, one row per linked pull request sits
under the identity line (`unified/panels/pr_controls.rs`, logic in
`tod_core::pr_actions`, memory in `tod_core::pr_choices`): **Labels** (the
standard searchable multi-select over the repository's own labels),
**Open** (a toggle: open for review, or back to a draft), **Reviewer** (a
searchable pick, GitHub's suggestions first) and **Update PR**.

Nothing waits on GitHub. The menus' lists are remembered in the data root
(`pr-controls.json`) and read again only from the last entry of each menu,
"Refresh this list" (the first time, when nothing is remembered yet). A choice
is saved as it is made, so it is still there after a restart, and "Not sent to
GitHub yet" shows until **Update PR** sends it (labels, then open or draft,
then the reviewer; it stops at the first failure and says why on the row).
Opening a draft, and asking a reviewer of a draft, are refused at Update PR
until the code review has passed clean (`Assessment::review_clean`); the error
says what is left. Every call to GitHub runs off the UI thread.

### Artifacts

A task's **artifacts** are its lasting outputs: **obligations**, the
**plan**, and the **changes** (the code). They are what the user ultimately
cares about getting done, so they sit just under the identity.

Each shows the one number worth knowing (failed obligations, steps done,
files changed). Each is a link: it opens that artifact's panel in the next
column by the column rule (`doc/ui/unified-view.md`). The list is never shown
inside the task panel itself; it would be squeezed, and the columns exist for
exactly this.

**Journey** sits at the right end of the same line. It is secondary (for
troubleshooting and process improvement), but putting it here makes the line
read as navigation: everything on it goes somewhere.

Findings appear as a link on the artifact strip only when the review has
recorded some (`Findings 3 · 2 open`); it opens the findings panel in the next
column. Verdicts and each phase's change sets are still stored. They are
steps toward the goal, not the goal, so they are not artifacts and are seen
through the journey, not here. A finding the user has to answer also appears
as a request.

### Runner

One line for the task's **runner** (its lifecycle processor, one per task):
the lifecycle state, what the agent is doing, how long, and tokens, with
pause and resume at the end.

| Runner is | Line shows |
|---|---|
| running an agent | state · what it is doing · elapsed · tokens |
| waiting on a request | state · waiting and for how long |
| paused by the user | state · paused · resume |
| failed | state · the error · retry |
| done | done |

Who the node is waiting on (the user, someone or something else, or nobody
because it is being worked) is `tod_core::next_actor`'s answer, rendered by
`views::task_list::standing::runner_parts`; the table above says what the
line shows in each case, and the extra parts are the actor's reason, who or
what is waited on, how long, when it is next checked, a retry count and the
error. The runner (`tod_core::runner_status::LiveTurn`) supplies only what a
live turn is doing and the split button's actions. While the node waits, the
line says only that and for how long; the request below says why.

The line ends with a split button: its default action moves the runner
along, and its menu holds the rest. The entries are a pure function of one
value (`unified::panels::runner_menu::menu`, tested without a window); design
and reasons are in `doc/runner-menu/design.md`.

| Runner is | Default action | Menu |
|---|---|---|
| never started, or done and moved back | **Start** | Run with a new session, Check now, Migrate to…, Reset environment, Update workspace to latest main, Delete sandbox |
| running | **Pause** (once the agent's turn ends) | Stop now (cancels the turn) |
| paused, stopped, failed, or waiting on a request | **Resume** (none for a run that ended only to wait: the button reads "Actions", disabled, and its caret opens the menu) | Resume with a fresh budget, Run with a new session, Check now, and the rest as above |
| stopped because its budget ran out | **Resume with a fresh budget** | Run with a new session, Check now, and the rest as above |
| a conversation the user started by hand is running | **Stop now** | |
| done | | Delete sandbox, Update workspace to latest main, Sync now (where they apply) |

Further entries show when they apply: **Adopt existing sandbox** (a node
whose older records name a sandbox its Files location does not; it records it
as the node's, after checking it is the node's and answers), **Migrate to…** (a node with no
children), **Reset environment** (a dev container or cloud sandbox; sets the
environment up again, deleting nothing), **Delete sandbox** and **Delete
sandbox without a checkpoint** (a node with a cloud sandbox, even while it
runs, in which case it asks first), and **Sync now** (a node whose runner is a
supervisor in a sandbox). While a job of the panel's own is running the
default shows what it is doing, disabled, and the menu is empty.

**Check now** runs the current state's gate check at once with no agent
(`tod_core::lifecycle::check_now`): the same `phase::settle_gate` the
autopilot's gate step makes, then, when the gate is clear, the advance; in
`pr_draft`, `approved` and `merged` it also looks at GitHub. It never starts
or wakes the runner and is offered only while the runner is idle.

The line says where the node runs: `in the cloud (sandbox …)` for a node
supervised in a sandbox (`in the cloud (its Files location records no
sandbox)` when only an older record names one; Adopt existing sandbox fixes
that), `this machine` (with `dev container …` when its work is in one).

**Where it runs.** Start looks at where the node's Files capability puts its
work (`tod_core::runner_place`). Files on this machine or in a dev container:
the autopilot runs under `tod-agentd`, the resident daemon, and the app
(`tod_ui::unified::runners::NodeRunners`) only sends it commands and shows the
state it pushes. Files in a cloud sandbox: a supervisor in the node's own
sandbox runs it (`doc/cloud-sandboxes/autonomous-nodes.md`), and the daemon
refuses to run that node itself. A node's conversation shows in `AgentRuns` as
hosted elsewhere, so the tree's status labels and the close-window warning see
it and nothing else sends to it. **Migrate to…** is the way to move a node:
pick one of the app's locations, and its sessions go with it
(`doc/agentd.md`, "Moving a node").

**Closing the app** does not stop a run: local runs are hosted by
`tod-agentd`, which keeps them going (and starts a run it was stopped on
again after a restart), and a node in a sandbox continues under its
supervisor. The close warning lists both as continuing, in yellow; a paused or
stopped run is left as it was.

**Delete sandbox when done.** The lifecycle config editor's Done block has a
**Delete sandbox** checkbox, on by default, inherited from the nearest node
that sets it like the skills are: when the node reaches `done` its sandbox is
deleted, by the daemon with no click and no sync (`tod_core::sandbox_retire`,
`doc/runner-menu/design.md`, "Deleting the sandbox at `done`"). A failed delete
is reported on the node. If it looks temporary (Blaxel unavailable, a
timeout) the panel says "Will retry automatically" and the daemon's next pass
(every minute) tries again; otherwise it says "Needs you", is not retried, and
points at the menu's Delete sandbox (which tries again) and Delete sandbox
without a checkpoint. The menu's Delete sandbox stays the manual way.

**Requests.** A run that stopped for a request (a decision, a plan step
handed back, a gate criterion no agent can fix) continues by itself once the user has
answered it and no conversation of the app's own is still working on the
node. Any other stop (budget, a step that changed nothing, a failed turn)
waits for Resume.

### Requests

A **request** is something the runner cannot continue without the user
deciding or doing. An agent raises one when it asks; the runner raises one
when it detects the work is stuck (it bounced between the same phases,
went over its token budget, or stopped making progress). To the user both are
the same: something to act on. The list has no heading, since a question with
numbered options needs no label.

Each request shows:

- The question and how to answer it, which depends on its kind:

  | Kind | Answered with |
  |---|---|
  | decision | its options, with quick keys (**1**, **2**, **3** …), or in words |
  | plan step handed back (`blocked` / `partial`) | words, or retry |
  | review finding the user must answer | its status (fixed / rejected) and a note |
  | stuck, raised by the runner | the actions that fit what it detected |

- One small footer line, never more, holding:
  - its **evidence**: links to what it is about, each shown as that item's
    name (an obligation's title, "Plan step 5"). They open in the next column
    by the column rule, so the request stays put while the user checks. A
    request with none shows none.
  - its **reason**: why the agent or runner could not handle it itself.
    Reasons are a fixed set (missing rule, conflict, access, risk,
    capability, broken, other) so they can be counted.
  - **when** it was raised: the time of day if today, else the date and
    time (`Nov 5 14:32`), in local time, so the user can tell a question
    from this morning from one that has waited a week.
  - a **terminal** icon, when the lifecycle conversation that asked (the
    decision's or finding's own, or the implement/verify conversation that
    handed the plan step back) has an agent session:
    it continues that session in a terminal, in the agent's own CLI, for
    anything too involved to answer here. It is disabled while that
    session's turn is running.
  - **Answered elsewhere**, for a decision or a handed-back plan step: the
    user settled it outside the app (typically in that terminal), so it is
    dismissed without an answer going to the agent. A decision logs an
    answer saying so; a plan step goes back to `in_progress`. A finding
    needs none: its own status buttons already settle it without a turn.
    Once nothing else is waiting, the runner continues as it would after an
    answer (see "Runner" above), resuming the same agent session, so the
    terminal should be closed first.
  - **Shouldn't have asked**, at the right end: feedback on the request
    itself (see below).

  ```
  Totals match the ledger · Plan step 5 · Missing rule · 14:32     Shouldn't have asked
  ```

Oldest first. There is rarely more than one; the list scrolls when there is.
When there are none, the space is empty.

In navigation mode, Up/Down move among a request's stops (evidence links,
options, the answer field); Enter activates a stop and Ctrl+Enter opens a
link as a Ctrl+click would. The number keys answer the top request whenever
it has options. Every answer records a journey `UserAction` with a
`Presented` snapshot of the choices shown.

### Answered drawer

Collapsed at the bottom of the column: "Answered (n)". Opening it lists the
task's past **decisions** and the answers given, newest first, for "didn't I
already answer this?" (the existing `decision_answers` log). Other kinds of
request are not listed: their answer is the item's own status. Each entry
links to the conversation that asked. The log is append-only: **Change** asks
again, and the new answer is sent to the runner as new input and logged as a
new entry, leaving the old one in place. Nothing is reversed; the runner is
told about the change and decides how to adjust.

## Request feedback

Every request can be marked as a bad question or one that should not have
been asked, with an optional note. This is recorded whether or not journeys
are on: it is how agents are taught to need the user less. Each entry keeps
the request, its reason, the phase, and the context recipe version that asked
it, so every "shouldn't have asked" becomes a test case for improving that
phase's prompts.

## Not in the task panel

- **Next / just happened.** What happens next is implied by the runner line
  and any request; history belongs to the journey.
- **Transcripts.** For troubleshooting; reached through the journey.
- **Workspace actions** (shell, agent). The agent panel beside it holds them.
