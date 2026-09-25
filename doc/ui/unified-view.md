# The unified view

A new view, separate from the existing ones, built for one person keeping many
agents busy at once. Status: design in progress. Nothing here is implemented
yet.

## The goal

A user should be able to run on the order of 100 nodes at once, each with an
agent walking it through the lifecycle, and spend their time only on the
decisions no agent can make.

In practice the user:

1. Selects many nodes at once (accept 20 tickets) and launches agents on all of
   them.
2. Lets the agents work. Each one walks its node through the lifecycle on its
   own and stops only when it needs a human decision.
3. Sees which nodes are waiting on them, presses one key to jump to the next
   one, finds everything needed to decide already on screen, answers, and
   presses the same key again.

Today the lifecycle is walked by hand, and doing anything with a node means
going back and forth between the node tree and the conversation view. This
view is meant to end both.

## Vocabulary

| Term | Meaning |
|---|---|
| **Region** / **slot** | A positional container. The two are used interchangeably; region is technically the higher level, but there are no regions made of slots yet. |
| **Column** | A vertical slot, numbered from the left: "the second column". |
| **Top region** | The header across the whole application. |
| **Bottom region** | The status bar. |
| **Panel** | One piece of content placed in a slot: the node tree panel, the details panel, the decisions panel, the obligations panel, the settings panel. |
| **Pane** | A subsection of a panel. |
| **Drawer** | A panel that slides out from an edge and collapses back on its own. It pushes what is above it up rather than covering it. The chat drawer is the only one so far. |

## Layout

```
┌─────────────────────────── top region: header ───────────────────────────┐
├───────────┬──────────────┬──────────────┬──────────────┬─────────────────┤
│ column 1  │ column 2     │ column 3     │ column 4     │ …               │
│ node tree │ (any panel)  │ (any panel)  │ (any panel)  │                 │
│           │              │              │              │                 │
├───────────┤              │              │              │                 │
│ chat      │              │              │              │                 │
│ drawer    │              │              │              │                 │
├───────────┴──────────────┴──────────────┴──────────────┴─────────────────┤
│                        bottom region: status bar                           │
└────────────────────────────────────────────────────────────────────────────┘
```

- **Column 1 is always the node tree panel.** It never leaves the screen: to
  answer one node's question the user often has to go and look at another.
  It starts wide enough for about 80 characters of a node's title, unless
  that would leave no room for a panel beside it, and keeps its width
  whatever opens or closes beside it.
- **Columns 2 onward hold whatever the user opened**, and share the rest of
  the width equally until the user drags them. The first takes all of it; a
  second halves it; a third takes a third. There is no limit and nothing
  folds away: many columns are just narrow ones.
- **Every divider between two columns can be dragged** to resize them. A drag
  moves width between the two columns beside the divider and leaves the rest
  where they are; the last column always takes whatever is left. Widths are
  kept across restarts: the tree's, and each column position's (the next
  panel opened in the third column takes the third column's width).
- The focused column's header is tinted (`column-header` in
  `doc/ui-style-guide.yaml`). Column 1's header is the node tree's own
  toolbar, on one fixed-height row.
- When the app opens, only the tree is shown. Selecting a node opens its
  details panel in column 2.

### Where a panel opens

> A panel opens in the **first unpinned column, starting with the column the
> user clicked in**. If there is none, a new column is added on the right.

- **Click** in an unpinned column: the new panel replaces that column's panel.
- **Click** in a pinned column: the new panel opens in the first unpinned
  column to its right.
- **Ctrl+click**: the new panel opens in the first unpinned column *after* the
  clicked one, even if the clicked column is unpinned. This is how to keep what
  you are looking at and open something beside it.
- The node tree counts as pinned, so selecting a node opens its details panel
  in the first unpinned column from column 2.
- Replacing a column's panel leaves the columns to its right alone, even ones
  that were opened from the panel it replaced.
- From the keyboard, **Enter** on a focused link is a click and **Ctrl+Enter**
  is a Ctrl+click. On an item (a node, an obligation, a plan step), Enter
  edits it, as it does in every list and in the node tree; **E** opens the
  item's panel instead, as it already opens the node tree's edit panel, and
  **Ctrl+E** opens it as a Ctrl+click would.

### Singleton panels

Some panels exist at most once. Opening one that is already shown does not
open a second copy: focus moves to the existing one, which switches to what
was asked for. The column rule applies only when it is not shown anywhere.

The decisions panel is a singleton. That is what makes **Alt+Q** repeatable:
each press moves the one decisions panel to the next waiting node, wherever the
panel is and whether or not it is pinned. Other panels may turn out to want
the same treatment.

### Pinning

Any column can be pinned (**Alt+W** toggles the focused column, or the pin in
its header). A pinned column keeps its panel whatever else the user opens.

That one mechanism covers several layouts no one has to configure:

- Keep a node's details open in column 2 while opening its obligations, plan,
  and transcripts to the right.
- Compare several nodes: open the obligations of three nodes and pin each one.
- Unpin a column to the left of a pinned one to free it up. The pinned column
  stays where it is, and the next panel opens in the freed column. This is
  deliberate: pinning a panel on the right and unpinning column 2 is how a user
  changes what appears in the middle.

Pinning and dragging dividers are the only layout controls. A fully
user-configurable layout (any panel in any slot, docking, dragging panels
around) was considered and rejected as not worth building.

### The chat drawer

Freeform conversation with the agent lives in a drawer at the bottom of
column 1, under the node tree.

- Collapsed, it is a small **Chat** tab at the bottom. Clicking the tab expands
  it upward. Clicking its header collapses it again.
- Expanding it pushes the node tree up; nothing is hidden behind it. For now
  it sits under column 1 only, and collapses independently of everything
  else.
- It is about the **most recent selection that can have an agent session**
  (a node, an obligation, a plan step), in whichever panel that selection was
  made, and says so ("About: Totals match ledger"). Selecting something that
  cannot have one (a finding, a decision) leaves it where it was. When its
  subject changes, it shows the most recent conversation about it, which the user can continue or replace with a new
  conversation (**Ctrl+N**).
- **Ctrl+J** toggles it: expands it when collapsed, collapses it when
  expanded.
- It holds only **freeform** conversations, where the agent may reply in plain
  text. Structured lifecycle agents never appear here (see below).

## Panels

### Node tree

- Filters, sorting, and view controls on the tree itself are how the user
  finds what needs attention (e.g. "needs you" and "running" chips, sort by
  time waiting). There is no separate attention column.
- Each node shows whether an agent is running on it or waiting on the user,
  with a count badge for pending decisions. Clicking the badge opens the
  node's decisions panel.
- **Right-click** on a node opens a menu of everything valid on it. Mostly
  navigation: open details, open decisions (with the count, only when some are
  pending), open obligations, plan, settings. Then node actions such as rename,
  move, and launch agent.

### Details

What a user expects to see when they click a node. It holds:

- The node's title and its lifecycle status label (below).
- The details field: one block of content, not a list.
- Links to the node's other panels (obligations, plan, decisions waiting,
  settings).

Obligations and plan steps are *not* laid out inside the details panel. A node
can have 20 or 30 obligations, so they get panels of their own.

### Decisions

What the user answers. Its own panel, not a pane of the details panel.

- **Pending decisions** are at the top, one pane each, oldest first. More than
  one agent may be working on a node, so there can be several; the user
  answers them one after another.
- Each decision comes from a structured agent reply: the question, its
  options with quick keys (**1**, **2**, **3** …), and links to its evidence
  (the obligation, the plan step, the test run, the transcript). Evidence opens
  by the column rule above, so the decision stays put while the user checks.
- Below them, **the answer log**: every answer the user gave on this node, and
  which agent asked. The panel scrolls when it gets long.

The answer log is **append-only**. **Change** on an entry asks the question
again, and the new answer is sent to the agent as a new action and logged as a
new entry. The old entry stays, so it is plain that the user changed their
mind. Changing an answer does not reverse anything: the agent may have done
other work based on the first answer, so the agent is told about the change
and decides how to adjust. Each entry links to the agent transcript for that
action.

### Obligations, plan, findings, transcripts

Each is a panel of its own, opened from a link or from the node's menu, and
built on the item list (`doc/ui/item-list.md`). Not everything is a list,
though: settings, for one, is not.

### Settings

A panel of its own. The current settings are too big and need their own
redesign, which is out of scope here.

## Lifecycle

### Status label

There is no lifecycle panel and no lifecycle header, just a small status
label wherever the node is shown:

| Label | Meaning |
|---|---|
| `verifying` | In this state. |
| `verifying →` | Checking the gate to leave this state. |
| `→ verifying` | Running the on-entry agent for this state. |

Decisions the lifecycle panel used to hold (gate verdicts, waives, what to do
next) move to the decisions panel.

### Structured agents

Lifecycle agents (implement, verify, review, fix, gate checks, on-entry) reply
**only** in structure. They have no free-text reply. The app renders what they
record: decisions with options, changes to items, findings, verdicts. The
more a decision is reduced to options, the faster the user gets through the
queue.

Freeform conversation stays available in the chat drawer, and is always
visibly distinct from structured work.

## Keys

| Key | Action |
|---|---|
| **Alt+Q** | Show the next node waiting on the user in the decisions panel (a singleton), opening and pinning it if it is not shown. |
| **Alt+Shift+Q** | The same, going back. |
| **Alt+W** | Pin or unpin the focused column. |
| **Enter** / **Ctrl+Enter** | Click / Ctrl+click the focused link. |
| **Enter** on an item | Edit it (lists and the node tree alike). |
| **E** on an item | Open its panel. |
| **Ctrl+E** on an item | Open its panel as a Ctrl+click would: after the current column. |
| **Ctrl+W** | Close the focused column (never column 1). |
| **1**, **2**, **3** … | Answer the top pending decision with that option. |
| **Ctrl+J** | Expand or collapse the chat drawer. |
| **Ctrl+N** | Start a new conversation in the chat drawer. |
| **Ctrl+Left / Ctrl+Right** | Move focus between columns (`ui/pane_nav.rs`). |

The next-waiting key has to be easy to reach with the left hand in the usual
typing position, and has to work while typing in the chat drawer. Nothing in
the app binds Alt+letter yet.

## Open questions

None at the moment. Settled: **Ctrl+E** is the Ctrl+click of E, **Ctrl+W**
closes the focused column, the chat drawer pushes content up (under column 1
for now), and it follows the most recent selection that can have an agent
session.

## Deferred

- **Bulk launch** (multi-select nodes, launch agents on all of them) and
  **notifications** when a node starts waiting are separate features. The
  goal above explains why this view exists; it does not put them in scope.

- **Lifecycle history.** A view by lifecycle phase: each phase's conversations
  and what each changed, found, and resolved (verification is the clearest
  case). It may help clean up what the conversation view shows today, but
  whether users need it is unproven.
- **Relations between nodes.** Beyond parent and children, which relations
  matter will be worked out as needed. Evidence links from a decision are the
  first real use.
- **Dockable chat**, under a particular column or dragged between them.
