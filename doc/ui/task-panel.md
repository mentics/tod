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
│  Not in the obligations or the repo · Discuss            │
│ Suggestions: Add retry to webhook delivery   (ENG-3557?) │  suggestions
│  [Accept] [Dismiss]                                      │
├──────────────────────────────────────────────────────────┤
│ ▸ Answered (4)                                           │  drawer
└──────────────────────────────────────────────────────────┘
```

The first three lines are fixed. Only the requests and suggestions scroll, and the drawer is
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
says what is left. Beside the buttons an indicator says where the review
stands without waiting for a click: "✓ Ready to open and assign", or
"⏳ Review feedback to address / Waiting for <bot> / Checks running", with the
full list of what is left in its tooltip (also on Open while it is a draft). Every call to GitHub runs off the UI thread.

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
through the journey, not here. A finding is never a request: tod and the agents settle findings (review
scoring, `doc/lifecycle/review-scoring.md`), and the user reopens or
rescores them in the findings panel.

### Guidance

The Guidance section is one line, `▸ Guidance (N)` with Add, until the user
opens it (the rows are one per selected item or lens and can run to dozens,
which would push the runner line out of view). It starts closed each session.

### Runner

One line for the task's **runner** (its lifecycle processor, one per task):
the lifecycle state, what the agent is doing, how long, and tokens, with
pause and resume at the end. The state is a chip in every state, `done`
included; picking another phase from it moves the node there at once,
bypassing the gates (to correct a mistake).

| Runner is | Line shows |
|---|---|
| running an agent | state · what it is doing · elapsed · tokens |
| waiting on a request | state · waiting and for how long |
| paused by the user | state · paused · resume |
| failed | state · the error · retry |
| done | done (the state chip still lists every other phase, to move it back) |

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
The stopped reason the panel shows is the node's run state row, so a check
that makes it untrue updates it (`check_now::refresh_stopped_reason`): when the
gate is clear, or the check moved the node on, a stopped reason about the
phase's work or gate (`run_state::kind::depends_on_gate`: no plan, evaluation
stuck, no progress, looping, review or checks not converging, a broken check,
lost work, a send-back loop) is replaced by "checked now: the gate … is clear"
(kind `checked`, still stopped and the user's to resume), instead of staying
until the next Resume. A reason a gate check says nothing about (the user's
pause or stop, a failed agent, a spent budget, a permission, a pull request's
trouble) is left alone, and so is a gate that still fails.

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

- The question and how to answer it, which depends on its kind. An access,
  broken or capability request is written by the app from the structured fields
  the agent filed (`tod_core::request_brief`, the one template): the need on
  the first line, the command in a copy block with a Copy button, the error,
  what was tried, and what happens after. A request without those fields (any
  other reason, or one asked before they existed) shows its question as the
  agent wrote it.

  | Kind | Answered with |
  |---|---|
  | decision | its options, with quick keys (**1**, **2**, **3** …), or in words |
  | plan step handed back (`blocked` / `partial`) | words, or retry |
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
    decision's own, or the implement/verify conversation that
    handed the plan step back) has an agent session:
    it continues that session in a terminal, in the agent's own CLI, for
    anything too involved to answer here. It is disabled while that
    session's turn is running.
  - **Answered elsewhere**, for a decision or a handed-back plan step: the
    user settled it outside the app (typically in that terminal), so it is
    dismissed without an answer going to the agent. A decision logs an
    answer saying so; a plan step goes back to `in_progress`.
    Once nothing else is waiting, the runner continues as it would after an
    answer (see "Runner" above), resuming the same agent session, so the
    terminal should be closed first.
  - **Discuss**, on a request triage escalated (`doc/lifecycle/triage.md`):
    opens the triage session that read it in the chat drawer, where the user
    can ask what it means, push back, or settle it in words.

  ```
  Totals match the ledger · Plan step 5 · Missing rule · 14:32            Discuss
  ```

  A request triage escalated shows triage's brief, then the options. Picking
  an option answers it directly (a short reflection by its session may
  follow); the number keys never answer one, since the brief is read first.
  The agent's own question and options are kept with it (`decisions.asked`)
  and can be read from the session.

Oldest first. There is rarely more than one; the list scrolls when there is.
When there are none, the space is empty.

In navigation mode, Up/Down move among a request's stops (evidence links,
options, the answer field); Enter activates a stop and Ctrl+Enter opens a
link as a Ctrl+click would. The number keys answer the top request whenever
it has options. Every answer records a journey `UserAction` with a
`Presented` snapshot of the choices shown.

### Suggestions

Below the requests, in the same scrolling area, the task's **suggestions**
(`tod_store::suggestions`, `doc/requests/design.md` section 9): out-of-scope
work an agent (`tod-cli suggest ticket`), triage or the app itself filed,
most often a follow-up ticket. It is not a request. It never makes the task
`You` (`tod_core::next_actor` does not read it), never stops the runner, and
stays at `done`, so nothing here ever needs an answer to go on. Nothing shows
when there are none.

Each open suggestion is a block: its title, the ticket text exactly as it will
be created (description and acceptance criteria), the tickets already in the
tracker that look like it ("ENG-3557 looks like this: ..."; or, in muted text,
why they were not checked: no Linear key for the task, a failed search), and
two buttons. **Accept** creates the ticket exactly as written in Linear, in the
team of the task's own ticket (or the nearest ancestor's), off the UI thread,
and records its id on the suggestion; if that fails (no ticket to take the team
from, no key, Linear refuses), the error is said under the suggestion and it
stays open. **Dismiss** records that it was declined. Both record a journey
`UserAction` (`suggestion_accept:<id>`, `suggestion_dismiss:<id>`) with the
buttons as presented. A decided suggestion shows as one muted line ("Accepted
as ENG-4001: ...", "Dismissed: ...").

The text, duplicates and errors are selectable.

### Answered drawer

Collapsed at the bottom of the column: "Answered (n)". Opening it lists the
task's past **decisions** and the answers given, newest first, for "didn't I
already answer this?" (the existing `decision_answers` log). Other kinds of
request are not listed: their answer is the item's own status. Each entry
links to the conversation that asked. The log is append-only: **Change** asks
again, and the new answer is sent to the runner as new input and logged as a
new entry, leaving the old one in place. Nothing is reversed; the runner is
told about the change and decides how to adjust.

An entry tod answered itself (`decision_answers.decided_by = 'tod'`: a silent
review bot gone on without, a pre-existing failure waived) reads "decided by
tod" where a user's answer shows the actor, with the rationale (what was tried
or compared) in a "Why:" line below the answer. A triage agent's answer reads
"decided by triage" the same way, and its entry has a **Triage session**
button that opens the session in the chat drawer. **Change** works as for any
answer; when the task has already moved past the phase that asked (the
request's protocol says which: implementation or fix, verification, review,
pr), it also offers **Also send the task back to <phase>**, the user's own
move with the change as its reason.

## Request feedback

There is no "shouldn't have asked" control. Feedback happens in the
conversation (`doc/lifecycle/triage.md`):

- **Highlight to flag.** Highlight words in a request's text and pick **This
  is confusing** from the right-click menu (`ui::selectable_text`'s
  `selectable_markdown_flaggable`). The highlighted words go into the
  request's triage session as a chat message, which opens in the drawer, and
  triage explains them and fixes its brief. A request with no triage
  session (an access, broken or capability request, one the app asks itself,
  or any with triage turned off) has no one to explain it: the flag records an
  improvement lesson (source `user`, linked to the request and the conversation
  that asked it, the highlighted words as the remark) and the panel says it was
  noted.
- **Say it** in the session: "you could have worked that out from X".

When a triaged request closes its session reflects once and records one
lesson (`improvement_lessons`, via `tod_store::lessons::record_triage_lesson`).
The old `request_feedback` table is kept and still readable; nothing writes
to it any more.

## Not in the task panel

- **Next / just happened.** What happens next is implied by the runner line
  and any request; history belongs to the journey.
- **Transcripts.** For troubleshooting; reached through the journey.
- **Workspace actions** (shell, agent). The agent panel beside it holds them.
