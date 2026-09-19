# Conversation protocols

One conversation view for every agent surface in the app, with a **protocol**
deciding what kind of conversation it is.

## 1. The problem

There are two chat UIs. The conversation view
(`crates/tod-ui/src/conversation/`) is the good one: a transcript of
collapsible chunks, a focus-scoped picker, a database-backed log, a side pane
showing the change set. The interactive agent window
(`crates/tod-ui/src/views/interactive_agent.rs` +
`crates/tod-ui/src/app/interactive_agent_window.rs`) is the other one, and it
is where the **Implement** button, the visual-design chat, and the action
panel's node chats all end up.

The second one is worse in ways that are not cosmetic:

- Its transcript is `Vec<(String, String)>` — in memory only. `TranscriptHistory`
  exists to re-fetch prior turns over ACP resume/load, recovering what was
  never persisted.
- It polls the agent every 300 ms, against this repo's own "events, not
  polling" rule.
- It renders replies with its own bespoke panels, so the collapsible
  narration/thought/tool chunking never reaches it.
- Its sessions do not appear in the conversation picker, so there is no one
  place to review everything that happened on a node.

And there is a behavioral gap neither UI covers: **agents stop early.** An
implementation run reports "remaining work" and hands back, and the user has
to notice and say "keep going".

## 2. The idea

A conversation is a transcript plus a side pane. The transcript is generic.
Everything else — where the agent runs, what context it gets, how its reply is
read, what "done" means, whether the app loops — is a **protocol**.

```
ConversationView
├─ picker + focus history + header   (generic)
├─ transcript                        (generic; protocol supplies answer rendering)
└─ side pane                         (protocol)

ConversationDriver
└─ protocol: context recipe, turn envelope, done-check, loop policy
```

Swapping the protocol turns the same view into a different tool. That is the
whole design.

### 2.1 What a protocol owns

| Concern | Today (hardcoded) | Under a protocol |
|---|---|---|
| Opening context + per-turn delta | `conversation::context` | protocol's context recipe |
| Working directory | `scratch_dir()` | protocol (scratch, or the node's worktree) |
| `SessionPurpose` | `Conversation` | protocol |
| Actor env (`TOD_INTERVIEW_ACTOR`) | always set | protocol (only outline-mutating protocols set it) |
| Answer rendering | markdown | protocol |
| Done-check | none; every turn ends the exchange | protocol |
| Loop | none | protocol |
| Side pane | change set | protocol |

Registered the way `tod_core::context_recipes::ALL_RECIPES` already registers
context recipes — one list, one place, tests over it.

### 2.2 The four protocols in scope

| Protocol | Launched from | cwd | Mutates outline | Side pane | Loops |
|---|---|---|---|---|---|
| `outline` | Ctrl+J, app nav, `proposed`/`design` nodes | scratch | yes, recorded | change set | no |
| `implementation` | Active-phase **Implement** | node worktree | no | plan steps + changed files | yes |
| `chat` | action panel's **Chat**, the picker | node's directory, else scratch | yes, recorded | change set | no |
| `visual_design` | design panel | scratch | yes (1 command) | designer | no |

`outline` is today's behavior, unchanged. The other three replace the
interactive agent window, which is deleted.

## 3. Data model

Schema **v39**. Two column additions, one new turn role, one new table.

```sql
ALTER TABLE conversations ADD COLUMN protocol TEXT NOT NULL DEFAULT 'outline';
ALTER TABLE conversations ADD COLUMN agent_run_id TEXT REFERENCES agent_runs(id);
```

`conversation_turns.role` gains `continuation`. Because the column has a
`CHECK` constraint, this is a table rebuild, following the
`migrate_v32_to_v33` rename-dance pattern.

```sql
CREATE TABLE conversation_reports (
    conversation_id BLOB NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    turn_seq        INTEGER NOT NULL,
    body            TEXT NOT NULL,      -- the report, as JSON
    PRIMARY KEY (conversation_id, turn_seq)
);
```

A report is structured data the agent records through `tod-cli` while it
works — never parsed out of its reply — so the side pane and the done-check
read it directly. It is filed against the turn in progress (the latest turn
in the transcript: the agent's own turn is appended only when it ends), and a
second record in the same turn replaces the first.

### 3.1 Conversations and fleet runs

`agent_runs` is the **process** record: pid, birth token, reattach,
liveness, worktree, platform/model/effort, and the one-at-a-time lock
(`live_implementation_session_for_node`). `conversations` is the **dialogue**
record: turns, protocol, report, loop state.

An implementation conversation references its fleet run through
`agent_run_id`. It keeps reattach and the live-run lock without rebuilding
them. `agent_runs.cached_transcript` is not used for conversation-backed runs;
the turns are authoritative.

Implementation runs get **no terminal session and no prompt queue**. The
conversation is the only way to talk to them.

### 3.2 What happens to the agent when the app goes away

Each conversation's agent is an ACP child process spawned with piped stdio
(`tod_agent::acp_host::spawn_acp_process`) — for Claude, the
`claude-code-acp` adapter, reached on Windows through its npm `.cmd` shim, so
the tree is `tod` → `cmd` → `node` → whatever the agent is running.

Every agent is spawned into a container the OS tears down with tod
(`tod_agent::process_tree`):

- **Windows**: a job object per agent with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. tod holds its only handle. The child is
  created suspended and resumed once it is in the job, so everything it starts
  is in the job too.
- **macOS and Linux**: the agent leads its own process group, and a watchdog
  `sh` blocks reading a pipe only tod writes to. When tod exits the read hits
  EOF and the watchdog kills the group.

- **Clean close.** `CursorAcpProvider`'s `Drop` kills each agent's tree
  (`AgentProcess::kill_tree`: `TerminateJobObject`, or `killpg`).
- **Crash.** The OS closes tod's handles (or the watchdog's pipe), and the tree
  dies with it — including whatever the agent was running. Verified on
  Windows by force-killing tod while Claude ran a shell command: `cmd`, both
  `node`s, `bash`, and the command were all gone within two seconds. Before
  this the adapter kept running with no parent, since it does not stop on
  stdin EOF. The Unix side is type-checked and its watchdog script was run
  under a Linux kernel, but the Rust has not been run on Linux or macOS.
  If the container cannot be set up, the agent runs without it and a warning
  is logged.
- **Next launch.** The conversation's turn is left without a reply. Sending
  again resumes the recorded agent session id in a *new* process, which has the
  session's history but none of the killed agent's unreported work.

Agents do not inherit `CLAUDECODE`: `spawn_acp_process` removes it, because
Claude refuses to start where it finds it set, and tod launched from a Claude
Code session would otherwise pass it to every agent.

## 4. The implementation protocol

### 4.1 Launch

**Implement** on an `active` node opens the node's most recent
implementation conversation, found by
`(focus = Node(id), protocol = 'implementation')`, or an unsaved new one, and
sends the starter message (§5). A node may have several, one after another;
the picker starts another.

Blocks with a message when the node has no plan steps. The lifecycle gate is
not changed: "plan steps exist and the graph is actionable" stays an
agent-judged rule on `planning` → `ready`
(`assets/process/agents/state/planning.md`), and the app's only derived
criterion on `ready` → `active` stays the action-config check in
`tod_core::gate::derived`.

### 4.1a Back from verification

Verification does not fix what it finds. The `verifying` → `review` gate has
a criterion the app answers itself (`verifying-review.plan-steps-verified`,
`tod_core::gate::derived`): every plan step `verified`, none `failed` or
unchecked. While any step is `failed`, the lifecycle panel's Verification
section says so and offers **Back to active**, which reverts the node to
`active`; **Implement** then sends the agent back to the failed steps, and
advancing to `verifying` again runs the on-entry verification over the
result. **Verify again** reruns that on-entry turn without leaving
`verifying`.

### 4.2 What the agent records, and what it says

Nothing in an implementation reply is parsed. The app reads what the agent
wrote as it worked, through `tod-cli`:

- **Plan steps.** It closes each finished step (`plan update --status
  implemented`). A step that needs the user it marks `partial` (done as far
  as it could go) or `blocked` (nothing could be done), with a `--reason`
  and a `--note` saying what is left and how the user unblocks it. The
  reason is a closed set (`HandoffReason`, stored as JSON in
  `node_plan_steps.reason`, schema v42), each with what the user needs to
  answer it:
  - `conflict` — obligations that cannot all hold, cited by id (two or more);
  - `decision` — a choice the obligations leave open, with the options (two
    or more);
  - `access` — a secret, account, or permission the agent lacks;
  - `external` — waiting on something outside the node.

  A step's size, not knowing how, or existing code that does not fit an
  obligation are deliberately not reasons: the context tells the agent that
  existing code is never a requirement, and a conflict is between
  obligations. `tod-cli` refuses a conflict without citations or a decision
  without options.
- **Steps that failed verification.** The `verifying` state's on-entry turn
  checks every plan step and records its verdict on the step: `verified`, or
  `failed` with a `--note` saying what was checked, how, and what happened
  (`tod-cli` refuses `failed` without one). A `failed` step is open work for
  this protocol, like any step not yet `implemented`. The agent is shown its
  latest note — as the step's own note while it is `failed`, and as
  `failed verification: …` once it has moved the step on, since a status
  change clears the step's note — and the continuation message repeats it.
  Every note a step is given is kept in `node_plan_step_notes` (schema v43),
  oldest first, and `plan show` lists them: several `failed` notes on one
  step are several attempts that did not hold up.
- **The test run.** After its last change in a turn it runs the tests and
  records the counts: `tests record --command <cmd> --passed N [--failed N]
  [--errors N]`, stored in `conversation_reports` as a `TestRun`. The latest
  record in a turn is that turn's.

The reply is only what the user needs that those do not already show —
usually nothing when the plan is done, and, when steps are left for the
user, a sentence or two leading with what blocks them and how to unblock it. The context says so as a scoped exception
to the autonomous stance (`surface/implement.md`): no summary of the work, no
list of steps, no test results. One reason covers every step it blocks.

A missing credential is not by itself a reason to block. `tod-cli secrets run`
starts a command (usually a script the agent writes) with a secret from tod's
credential store in its environment and masks the value in its output, so the
agent uses the user's stored keys without seeing them. Automated tests use
mocks or recorded fixtures; live calls are for exploring the service,
capturing fixtures, and checks a step or obligation asks to run live.

This replaced a YAML report the whole reply had to be. It restated what the
plan steps already said, models wrapped it in prose anyway, and the user got
the whole document back as the reply.

### 4.3 Done

A turn ends the exchange when **both**:

1. No plan step on the node is `pending`, `ready`, `in_progress`, `failed`,
   `partial`, or `blocked` — every one is `implemented` or `verified`
   (`tod_store::outline::repos::plan_steps`).
2. The turn recorded a test run with at least one pass and no failures or
   errors. A run from an earlier turn does not count: the code may have
   changed since.

Both are read from the store. Test counts are agent-recorded and trusted.

Once no step is left open — every one is done, `partial`, or `blocked`, and
at least one is `partial` or `blocked` — the loop hands back to the user,
whatever the tests say. A `partial` or `blocked` step never stops work on the
open ones: the loop keeps going while any step is still open.

### 4.4 The loop

Always on for this protocol. When a turn finishes and the done-check fails,
the driver appends a `continuation` turn and immediately starts another turn
carrying the remaining work — without the user.

Stops on any of:

- **Done** (§4.3).
- **Needs the user** — every step not done is `partial` or `blocked`.
- **Cap** — 10 continuations per user message.
- **No progress** — a continuation that closes no plan step and changes no
  file in the worktree. Checked against the step statuses and `git status
  --porcelain` before and after the turn.
- **Stop** — the user. The transcript panel's existing Stop kills the loop,
  not just the turn in flight.

The header shows the continuation count while a loop runs.

A `continuation` turn records the message the loop sent, verbatim — *not* a
synthesized user turn: it is left out of the history a fresh session is
seeded with, and the transcript shows it as "Sent automatically", so the user
sees exactly what the agent was told, that the loop ran, and how often. The
message opens with what is left ("4 plan steps are still open…"), lists the
open steps, says what the tests still need, and restates the reply rule.

### 4.5 Side pane

Plan steps first, with live status, in plan order — the same data the
done-check reads, so the user sees what the gate sees. Below them, the files
changed in the worktree (`git status --porcelain`, refreshed when a turn
finishes). A header strip carries the latest recorded test run's counts
("24 passed", "22 passed, 2 failed" in the error color) — nothing until the
agent records one — and the loop's continuation count, and says how many
steps need the user.

Under each `failed` step: "Failed verification" and its note. The header
says how many steps failed.

Under each `partial` or `blocked` step: its reason, its note, and a way to
answer by reason — for a conflict, each cited obligation's text with
**Keep**; for a decision, each option with **Choose**; for access or
something external, **Retry**. Answering sends the agent a message saying
what the user decided (`implement::handoff_answer_message`, which carries the
note along) and, once it has gone out, sets the step back to `in_progress`
as the user's edit, which starts the loop again. A step handed back before
reasons existed shows its note alone; the message input answers it.

## 5. View changes

- `Pane::ChangeSet` becomes `Pane::Side`; the pane's content comes from the
  protocol. Keyboard navigation and the `Pane`/`Stop` model are unchanged.
- `AgentConversationPanel` gains a hook for protocol-supplied answer
  rendering. Its `parts` chunking (narration, thoughts, tool calls) is
  unchanged; only the answer body's rendering is delegated.
- The header gains the platform/model/effort pickers and the permission
  prompt, both previously owned by the interactive agent window.
- The picker lists all of a focus's conversations regardless of protocol,
  badged by kind, followed by one "New …" entry per kind the focus can start:
  a conversation and a chat anywhere, an implementation only on an `active`
  node with plan steps. Ctrl+N starts another of the kind that is open.
  *Done.* So a node can have several implementation conversations; Implement
  reopens the most recent.
- A protocol may name a **starter** message (`Protocol::starter`;
  implementation's is "Implement the plan."). A new conversation from the
  picker or Ctrl+N puts it in the input, unsent, to edit or send as is. The
  lifecycle panel's Implement sends it on arrival (`OpenConversation::start`),
  since the click already says what the user wants — unless the agent is
  still working on that conversation.

## 6. Staging

1. **The seam.** *Done.* `Protocol` trait and registry
   (`tod_core::conversation::protocol`), driver parameterization,
   protocol-chosen side pane (`tod_ui::conversation::side_pane`). `outline`
   keeps today's behavior and its tests pass untouched.
2. **Schema v39** and the report table. *Done.*
3. **The implementation protocol.** *Done.* Recorded test runs, done-check,
   loop, side pane, `--agent mock` support. **Implement** opens the conversation
   view instead of the interactive agent window.
4. **The `chat` protocol.** *Done.* `ChatProtocol` with its own
   `surface/chat.md` and `CHAT` recipe. A chat has no fixed job — the user sets
   it — so the agent may do whatever it is asked: write files (in the focus
   node's directory when it has one), research, change the outline. Its
   outline writes are attributed to the conversation, so they land in its
   change set and can be reversed, same as an outline conversation's. The
   action panel's list of chat sessions is gone: one **Chat** button opens the
   node's chat in the conversation view, whose picker is the session list.
5. **The `visual_design` protocol.** *Stubbed; the real one is a separate
   piece of work.* `ProtocolKind::VisualDesign` exists, resolves to
   `ChatProtocol`, and shows a placeholder side pane; nothing launches it. The
   working designer is still `views::visual_design_panel`, which embeds its
   chat beside the canvas (`InteractiveAgentView::with_embedded`). The rebuild
   inverts that — the conversation view hosts, the designer is the side pane —
   and that design has not been done.
6. **Deletion.** *Blocked on 5.* `visual_design_panel.rs`,
   `interactive_agent.rs`, and `interactive_agent_window.rs` are marked
   slated-for-deletion in their module docs. One thing outlives the window:
   `InteractiveAgentWindowControl::engagement`, the registry the action panel
   reads for its background runs' status labels, needs another home first.

### What stages 3-4 left for later

- `conversations.agent_run_id` and `ConversationRepo::set_agent_run` exist, but
  nothing writes them, and that is deliberate for now: reattaching to a
  still-running agent only earns its keep once agents run in dev containers
  and cloud VMs. Locally a conversation already survives a restart by resuming
  its agent session id. Until then the one-live-run-per-node lock
  (`live_implementation_session_for_node`) no longer covers implementation
  conversations, and an agent does not survive tod (§3.2), so there is
  nothing to reattach to. A remote agent will need to outlive tod on purpose,
  which is where reattach comes back. §3.1 describes a fleet-run link that may
  not be the shape this ends up taking.

## 7. Out of scope

- The `verifying` lifecycle phase gets its own protocol and conversation
  later. Unit tests shipping green is an `active`-phase obligation and is
  covered by §4.3; end-to-end and manual verification are a separate phase.
- Capturing test output in the app. Test counts are agent-recorded (§4.2).
- Terminal sessions and prompt queuing for implementation runs (§3.1).
