# The unified view: implementation plan

How to build `doc/ui/unified-view.md`. Each work item below is sized for one
agent, names what it depends on, and names the files it owns so that items
running at the same time do not edit the same code.

## What the code has today

The facts that shape the plan:

- **The shell holds one instance of each panel** (`app/window.rs`, `open()`),
  and the right drawer (`app/right_drawer.rs`) shows one of them beside the
  tree. The column model replaces the drawer for the new view; the existing
  Tasks view keeps it. Every panel constructor needs little more than
  `Arc<FleetStore>`, so several instances are possible.
- **Reusable panels:** `ObligationsView` and `PlanStepsView` already run
  embedded (`set_embedded`, used by `conversation/context_panel.rs`).
  `TaskEditView` works but is 5,900 lines of details *and* settings.
  Findings have a row component (`views/rows/finding_row.rs`) but no panel.
  Transcripts are a window over every session, not a panel for one.
- **The node tree** (`views/task_list/`) can be embedded (it is the Tasks
  view's left side). It has search, tag filters, and a sort menu, but no
  right-click menu and no idea of "needs you". The item list's right-click row
  menu (`ui/item_list/row_menu.rs`) is the pattern to copy.
- **No record of a question with options and its answers exists** for
  lifecycle agents. Closest: a plan step's `HandoffReason::Decision { options }`
  (no answer), and `InterviewQuestion` (full question/options/answer, but tied
  to interview sessions). Gate reports, open review findings, and blocked plan
  steps are the other things a node can be waiting on.
- **"Agent running" lives in `ConversationView`'s memory**
  (`drivers: Vec<DriverSlot>`), not in the store, so no other view can show
  it, and the `state →` / `→ state` label has nothing to read.
- **Store changes** come as a coarse `FleetStore::subscribe_changes()`
  broadcast; views listen to it (no polling the DB on a timer).

## Work items

### Wave 1: four items, all in parallel

**W1. Skeleton and column model** (tod-ui)
- New `crates/tod-ui/src/unified/`: the view root, a `ShellView::Unified`
  variant, and a nav entry ("Workbench" is a placeholder name).
- `unified/columns.rs`: the column model as plain Rust with no GPUI, fully unit
  tested. It holds the columns, which are pinned, and which has focus; the
  "first unpinned column starting from the clicked one" rule; Ctrl+click;
  singleton panels; unpinning; closing; folding into strips when the columns
  do not fit.
- A `PanelKind` enum and a `ColumnPanel` trait (render, title, focus handle,
  "open this" requests up to the root) with placeholder panels, so later items
  only add implementations.
- Column 1 hosts `TaskListView`; selecting a node opens a placeholder details
  panel by the rule.
- Keys: Alt+W (pin the focused column), Ctrl+Left/Right between columns
  (`ui/pane_nav.rs`), Enter/Ctrl+Enter on links.
- Owns: `unified/`, and the few new arms in `app/window.rs` and
  `ui/app_nav.rs`.

**W2. Decisions store and CLI** (tod-store, tod-cli, tod-core mock)
- A `decisions` table: node, the conversation and protocol that asked, the
  question, its options, evidence references (obligation, plan step, test
  run, conversation), a status (pending/answered/withdrawn), created time.
- An append-only `decision_answers` table: decision, chosen option or free
  text, answered time. A change is a new row; nothing is updated or deleted.
- Both tables need `journey_changes` triggers. Schema bump and migration.
- Writes go through the fleet writer like every other mutation, attributed
  to an actor.
- A `tod-cli decisions` noun for agents: `ask` (with `--option`, repeatable,
  and evidence flags), `list`, `show`. A `cli/decisions.md` fragment kept in
  sync by `doc_sync`.
- The mock's `ask <text>` directive records a decision (options parsed from
  the text, e.g. `ask Round per line or per invoice? | per line | per invoice`)
  so `--agent mock` can exercise the whole loop.
- Owns: `tod-store/src/decisions.rs` (new), the schema/migration files,
  `tod-cli/src/decisions.rs` (new), `crates/tod/media/context/cli/decisions.md`,
  `tod-core/src/conversation/mock.rs`.

**W3. Shared agent-run registry** (tod-ui, tod-core)
- Move conversation driver hosting out of `ConversationView` into one
  app-level entity, shared like `LifecycleController`, so any view can ask
  "which agents are running on this node, under which protocol, entering or
  leaving which state".
- `ConversationView` keeps working unchanged from the user's point of view;
  it reads from and starts drivers through the registry.
- Owns: `conversation/driver_slot.rs`, the driver parts of
  `conversation/mod.rs`, a new `ui/agent_runs.rs` (or similar), and its
  construction in `app/window.rs`.
- This is the riskiest item. It must keep the conversation tests green
  (`cargo test -p tod-ui conversation`).

**W4. Node tree: right-click menu and status filters** (tod-ui task_list)
- A right-click menu on tree rows, following `ui/item_list/row_menu.rs`,
  carrying the actions the tree already has (open edit panel, obligations,
  plan, lifecycle, rename, delete…). It emits events; the host decides what
  "open" means, so the unified view can map them to columns later.
- Filter chips and a sort for "running" (from `live_run_count`) and a
  "needs you" count/filter/sort fed through a setter the host calls
  (`set_attention(map node → count, waiting since)`), so W6 can feed it
  without touching the tree again.
- Owns: `views/task_list/` (new `context_menu.rs`, delegate chips).

### Wave 2: after wave 1, in parallel

**W5. Details panel** (needs W1)
- Title, lifecycle status label, the details field (one content block,
  editable with the existing content editor), and links: obligations (count),
  plan (done/total), decisions waiting (count), settings.
- Owns: `unified/panels/details.rs`.

**W6. Attention: what a node is waiting on** (needs W2)
- `tod_core::attention`: for a node, and for a whole list at once, return
  what it is waiting on and since when: pending decisions (W2), plus today's
  sources adapted into the same shape: blocked/partial plan steps with a
  reason, open review findings, a gate report that needs a human. Each item
  carries the actions that answer it.
- Unit tested in tod-core against a temp store.
- Owns: `tod-core/src/attention.rs` (new).

**W7. List panels** (needs W1)
- Column panels wrapping `ObligationsView` (embedded), `PlanStepsView`
  (embedded), a new findings panel built on `item_list` and `finding_row`,
  a settings panel that hosts `TaskEditView` for now (its redesign is out of
  scope), and a transcript panel for one conversation (reusing
  `conversation/transcript.rs`).
- E on an item opens its panel.
- Owns: `unified/panels/{obligations,plan,findings,settings,transcript}.rs`.

**W8. Chat drawer** (needs W1, W3)
- The bottom drawer: a Chat tab, expands up, header collapses, Ctrl+J
  toggles while in the unified view. It is about the current selection,
  shows the latest freeform conversation about it, continue or Ctrl+N new.
- Reuse the conversation view's transcript and composer pieces and the W3
  registry; freeform protocol only.
- Owns: `unified/chat_drawer.rs`.

### Wave 3: after wave 2

**W9. Decisions panel** (needs W1, W2, W6)
- The singleton panel: pending decisions oldest first, number keys answer
  the top one, evidence links open by the column rule, the append-only
  answer log with Change and a transcript link per entry.
- Every answer records a journey `UserAction` with its `Presented` options.
- Owns: `unified/panels/decisions.rs`.

**W10. Answers reach the agent; structured-only lifecycle agents**
(needs W2, W3)
- Answering a decision resumes the asking conversation with the answer (and,
  for a change, what the earlier answer was), through the W3 registry.
- Protocols hand back when their node has pending decisions, and resume on
  an answer.
- The lifecycle protocols' surface fragments tell the agent to ask through
  `tod-cli decisions ask` and never in reply text. Add `cli/decisions` to
  their recipes.
- Owns: `tod-core/src/conversation/protocol*.rs` and `implement/verify/review/
  fix.rs`, `media/context/surface/*`, `context_recipes.rs`, the answer hook
  in the registry.

**W11. Status label** (needs W3, W5)
- `state`, `state →` (gate running), `→ state` (on-entry running), read from
  the registry; shown in the details panel and on tree rows in this view.
- Owns: `unified/status_label.rs`, a small hook in the tree delegate.

### Wave 4: after wave 3

**W12. Alt+Q loop and integration** (needs W4, W6, W9)
- Alt+Q / Alt+Shift+Q: next/previous node waiting, in the order the tree's
  "needs you" sort gives; shows it in the singleton decisions panel, opening
  and pinning it if it is not shown.
- Feed W6 into the tree (`set_attention`), and map the tree's right-click
  menu to columns (open details, decisions (n), obligations…).
- An end-to-end mock smoke: launch with `--agent mock`, have the mock ask
  on two nodes, answer both with Alt+Q and number keys, change one answer,
  check the log.
- Update `.claude/CLAUDE.md` with a short section on the new view.

## Dependencies at a glance

```
W1 ──┬── W5 ──┐
     ├── W7   ├── W11
     ├── W8 ◄─┼── W3
     └────────┼──────────── W9 ◄── W6 ◄── W2
W3 ───────────┴── W10 ◄── W2
W4, W6, W9 ──────────────── W12
```

| Item | Needs | Can run beside |
|---|---|---|
| W1 skeleton + columns | — | W2, W3, W4 |
| W2 decisions store + CLI | — | W1, W3, W4 |
| W3 agent-run registry | — | W1, W2, W4 |
| W4 tree menu + filters | — | W1, W2, W3 |
| W5 details panel | W1 | W6, W7, W8 |
| W6 attention | W2 | W5, W7, W8 |
| W7 list panels | W1 | W5, W6, W8 |
| W8 chat drawer | W1, W3 | W5, W6, W7 |
| W9 decisions panel | W1, W2, W6 | W10, W11 |
| W10 answers + structured agents | W2, W3 | W9, W11 |
| W11 status label | W3, W5 | W9, W10 |
| W12 Alt+Q + integration | W4, W6, W9 | — |

## How agents run it

- Each item runs in its own git worktree and ends with one commit on its own
  branch. Items in a wave touch different files; the few shared lines (a new
  arm in `window.rs`, a module line in `lib.rs`) merge by hand.
- After each wave the branches merge into the feature branch, then
  `cargo check --workspace --all-targets` and the touched crates' tests run
  before the next wave starts.
- Every agent follows `.claude/CLAUDE.md`: timeouts on every cargo command,
  tests scoped to the crates touched, `--data-root` on every launch, UI work
  checked in the real app with `--agent mock --no-focus`, dynamic text
  selectable, nothing slow on the UI thread, views updated from store events.
