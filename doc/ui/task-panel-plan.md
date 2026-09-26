# The task panel: implementation plan

How to build `doc/ui/task-panel.md`. Each work item below is sized for one
agent, names what it depends on, and names the files it owns so that items
running at the same time do not edit the same code.

## What the code has today

The facts that shape the plan:

- **Selecting a node always opens `PanelKind::Details`**
  (`unified/mod.rs`, `on_task_list_event`, `SelectionChanged`, and the
  `OpenTaskEdit` / `OpenActionPanel` arms). There is no per-kind default.
  Generator and managed nodes have their own UI only in the old task editor
  (`views/task_edit/`), not in the unified view.
- **The decisions panel already does most of the request work**
  (`unified/panels/decisions.rs`): it lists `tod_core::attention::for_node`
  (decisions, plan steps handed back, open findings in `review`, a gate
  check needing a human), answers each through its existing code path
  (`AgentRuns::answer_decision`, `answer_plan_step_handoff`,
  `respond_review_finding`, `LifecycleController::waive`), renders evidence
  links (`evidence_target`), handles the number keys and keyboard stops,
  records journey `UserAction`s with `Presented`, and shows the
  `decision_answers` log with **Change**. The task panel reuses all of this;
  it moves, it is not rewritten.
- **The decisions panel is the only singleton** (`PanelKind::Decisions`,
  `is_singleton`, `sync_decisions_node`), and Alt+Q opens it
  (`UnifiedView::advance_waiting`). The tree's attention badge and right-click
  menu emit `TaskListEvent::OpenDecisions`.
- **Reasons partly exist.** A plan-step handoff records
  `HandoffReason::{Conflict, Decision, Access}`. A decision
  (`tod_store::decisions`, `tod-cli decisions ask`) records question,
  options, evidence, conversation, and protocol, but no reason. Gate reports
  and findings carry their own text.
- **There is no runner.** What exists per conversation is
  `tod_core::conversation::ConversationStatus` (running, activity, last
  error, live token usage), held for the UI in `AgentRuns` driver slots. The
  runner itself is being designed separately; until it lands, the runner line
  is derived from these, the node's lifecycle state, and its attention.
- **Nothing counts changed files.** `tod_store::fleet::Workdir` runs git on
  the host, in a container, or in a sandbox, and must never be called on the
  UI thread. There is no changes panel.
- **There is no journey viewer** yet.
- **Capabilities:** "task node" is Lifecycle on the node itself plus Agent on
  the node or inherited (`node_actions::nearest_with_capability` already
  finds the nearest Agent).

## Work items

### T1. Task-node routing and the panel shell

Depends on: nothing.

- `tod_store`: an `is_task_node(conn, node_id)` predicate (Lifecycle on the
  node, Agent on it or an ancestor), next to `nearest_with_capability`, with
  tests for self, inherited, and neither.
- `PanelKind::Task(Uuid)` and a `TaskPanel` in `unified/panels/task.rs`.
- One function decides a node's default panel (task → `Task`, else
  `Details`; generator and managed arms are left for later). Every place that
  opens `Details` for a selection or the tree's "open" events goes through it.
  The decision must not block the UI thread: prefer data the tree rows already
  hold; otherwise a short `fleet.read`, as the details panel's load does.
- The fixed header: **identity** (title and Linear ticket id) and the
  **artifact strip**: Obligations (count, and failed when any) and Plan
  (done / total), each opening its panel by the column rule (Ctrl+click as
  usual). Reload on store change, as `DetailsPanel` does.

Owns: `unified/panels/task.rs`, `unified/columns.rs`, the selection arms of
`unified/mod.rs`, the predicate in `tod-store/src/fleet/node_actions.rs`.

Done when: selecting a task opens its task panel with identity and the two
artifact links, every other node opens details, and the column-model tests
cover `PanelKind::Task`.

### T2. The runner line

Depends on: T1.

- A `RunnerStatus` in `tod_core` (a new `runner_status.rs`): `Running {
  activity, since, tokens }`, `Waiting { since }`, `Failed { error }`,
  `Idle`, `Done`. Built from the node's lifecycle state, its attention
  (`waiting_since`), and the `ConversationStatus` of any conversation running
  on the node. The runner, when it exists, becomes this type's source; the
  panel does not change.
- The line renders it: lifecycle state, then what the status carries (see
  the table in `task-panel.md`). **Stop** at the end while an agent runs
  (the existing conversation stop). Pause and resume wait for the runner.
- Elapsed time ticks from the status's `since`, not by polling the store.

Owns: `tod-core/src/runner_status.rs`, the runner line in
`unified/panels/task.rs`.

Done when: with `--agent mock`, starting Implement on a task shows the line
running with the agent's activity and tokens, a pending decision shows
waiting and for how long, and a failed turn shows its error.

### T3. Request reasons

Depends on: nothing (store and CLI only; parallel with T1–T2).

- A `RequestReason` in `tod_core::attention`: missing rule, conflict,
  access, risk, capability, other. `AttentionItem` gains `reason`.
- Decisions: a `reason` column (schema bump, migration defaulting existing
  rows to `other`), `tod-cli decisions ask --reason <kind>`, and its
  `cli/decisions.md` entry (`doc_sync` holds the two together).
- Mapping for the other kinds: handoff `Decision` → missing rule, `Conflict`
  → conflict, `Access` → access; a finding and a gate blocker → risk unless
  their record says otherwise.
- Tell the lifecycle agents to give a reason: the relevant `surface/` or
  `domain/` fragment says when each applies, in prose (no syntax outside
  `cli/`).

Owns: `tod-store/src/decisions.rs`, the schema migration, `tod-cli`'s
decisions noun, `media/context/cli/decisions.md`, `tod-core/src/attention.rs`.

Done when: `attention::for_node` returns a reason for every kind, and the
store, CLI, and doc-sync tests pass.

### T4. Requests in the task panel

Depends on: T1, T3.

- Move the request rendering and answering out of `decisions.rs` into a
  shared `unified/requests.rs`: per-kind answer controls, evidence links, the
  number keys, keyboard stops, and the journey recording, unchanged in
  behaviour.
- The task panel shows them below the runner line, oldest first, scrolling
  when there are several; no heading.
- The one footer line: evidence links (each item's name), the reason, and
  room at the right for T6's control.

Owns: `unified/requests.rs`, the requests section of
`unified/panels/task.rs`, `unified/panels/decisions.rs` (to switch it onto
the shared module until T7 deletes it).

Done when: every kind of request can be answered from the task panel, with
the same journey records as from the decisions panel, and the decisions
panel's tests still pass against the shared module.

### T5. The Answered drawer

Depends on: T4.

- A drawer anchored to the bottom of the task panel, collapsed by default,
  "Answered (n)". Opened, it lists the node's `decision_answers` newest
  first, each with a link to the conversation that asked and **Change**,
  moved from the decisions panel's log.
- Its open state is kept per panel for the session; it does not need to
  survive a restart.

Owns: the drawer in `unified/panels/task.rs` and its piece of
`unified/requests.rs`.

Done when: changing an answer from the drawer adds a new entry and sends it,
as the decisions panel does today.

### T6. Request feedback

Depends on: T3, T4.

- `tod_store::request_feedback`: node, request kind and id, reason,
  conversation, protocol, verdict (`bad_question` / `should_not_ask`),
  optional note, created at. Schema bump, and a `journey_changes` trigger so
  the writes also appear in journeys; the table itself is written whether or
  not journeys are on.
- **Shouldn't have asked** on each request's footer line: one click records
  it, then offers a note field and a switch to "bad question". It does not
  dismiss or answer the request.
- The context recipe version is not recorded yet; add it here once turns
  record it.

Owns: `tod-store/src/request_feedback.rs` and its migration, the control in
`unified/requests.rs`.

Done when: feedback is stored with its request's reason and protocol, and a
store test reads it back.

### T7. Alt+Q to the task panel; remove the decisions panel

Depends on: T4, T5.

- `advance_waiting` selects the next / previous waiting task in the tree
  (as it does now) and lets selection open its task panel; keyboard focus
  lands on the panel so the number keys work at once. The tree badge and the
  right-click menu's `OpenDecisions` open the task panel instead (rename the
  event).
- Delete `PanelKind::Decisions`, `HostedPanel::Decisions`,
  `sync_decisions_node`, `unified/panels/decisions.rs`, and the singleton
  machinery if nothing else uses it.
- Update `.claude/CLAUDE.md` ("Unified view") and the "decisions panel"
  wording in `cli/decisions.md` and elsewhere in `media/context/`.

Owns: `unified/mod.rs` (waiting and events), `unified/columns.rs`,
`views/task_list/` (the event rename), the docs above.

Done when: Alt+Q steps through waiting tasks showing each one's task panel
with focus on its top request, and no code refers to the decisions panel.

### T8. Changes

Depends on: T1.

- A background count of files changed on the node's branch against its base,
  through `Workdir` (host, container, or sandbox), never on the UI thread.
  Recomputed when the panel opens and when a turn on the node ends; not on a
  timer.
- **Changes** joins the artifact strip, and `PanelKind::Changes(Uuid)`
  lists the changed files with lines added and removed; a file opens in the
  code editor the node already uses.
- No count while nothing is known yet (no Files directory, or the count has
  not come back); never a stale number shown as current.

Owns: `unified/panels/changes.rs`, the count's background job, the Changes
link in the strip.

Done when: a task with commits on its branch shows the right file count, and
the panel lists them, on the host and in a dev container.

## Order

```
T1 ──► T2
 │
 ├──► T8
 │
T3 ──► T4 ──► T5 ──► T7
        │
        └──► T6
```

T1 and T3 can start together; T2 and T8 follow T1 in parallel with T4.

## Left for later

- **Journey** on the artifact strip: shown once there is a journey viewer to
  open. A link that goes nowhere is not shown.
- **Pause and resume, and stuck requests:** they belong to the runner.
- **Generator and managed nodes' default panels** in the unified view; until
  then they open details.
- **The context recipe version on request feedback** (T6), once turns
  record it.
