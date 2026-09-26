# The task panel

The panel shown by default when a **task node** is selected (see
`doc/glossary.md`: lifecycle on itself, agent on itself or inherited). Other
node types have their own default panel (generator nodes and managed nodes
already do); every node with none of its own gets the details panel. Status:
design. Nothing here is implemented yet.

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

Findings, verdicts, and each phase's change sets are still stored. They are
steps toward the goal, not the goal, so they are not artifacts and are seen
through the journey, not here. The exception is one the user has to answer,
which appears as a request.

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

While it waits, the line says only that and for how long; the request below
says why.

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
  | gate criterion needing a human | **Waive**, through the shared `LifecycleController` |
  | stuck, raised by the runner | the actions that fit what it detected |

- One small footer line, never more, holding:
  - its **evidence**: links to what it is about, each shown as that item's
    name (an obligation's title, "Plan step 5"). They open in the next column
    by the column rule, so the request stays put while the user checks. A
    request with none shows none.
  - its **reason**: why the agent or runner could not handle it itself.
    Reasons are a fixed set (missing rule, conflict, access, risk,
    capability, other) so they can be counted.
  - **Shouldn't have asked**, at the right end: feedback on the request
    itself (see below).

  ```
  Totals match the ledger · Plan step 5 · Missing rule     Shouldn't have asked
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
