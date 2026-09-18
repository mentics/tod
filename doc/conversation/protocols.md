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
└─ protocol: context recipe, turn envelope, reply parsing, done-check, loop policy
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
| Reply parsing | none — body is markdown | protocol |
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
    body            TEXT NOT NULL,      -- the parsed report, as JSON
    PRIMARY KEY (conversation_id, turn_seq)
);
```

A protocol that parses replies stores the parsed form here, so the side pane
and the done-check read structured data instead of re-parsing text. The raw
reply stays in `conversation_turns.body` either way.

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

- **Clean close.** `CursorAcpProvider`'s `Drop` kills the whole tree
  (`taskkill /T` on Windows). Nothing survives, provided the provider is
  actually dropped on the way out.
- **Crash.** The child is not in a job object and not tied to the parent's
  lifetime (no `KILL_ON_JOB_CLOSE`, no `PDEATHSIG`), so the OS leaves it
  running. It has lost both ends of its only channel: stdin reads EOF and its
  replies go nowhere. The Claude adapter does not stop on that EOF: after a
  force-killed tod, its `cmd` → `node` chain was still running with no parent.
  Subprocesses it had started (a build, a test run) keep running too. Nothing
  reconnects to it. Putting agents in a job object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` would have Windows kill the tree when
  tod dies for any reason.
- **Next launch.** The conversation's turn is left without a reply. Sending
  again resumes the recorded agent session id in a *new* process, which has the
  session's history but none of the orphan's unreported work.

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

### 4.2 The reply is a document

The stance mandates that every implementation reply is a single YAML document
and nothing else — no prose outside it, no code fence around it.

```yaml
status: working        # working | complete | blocked
summary: One line on what this turn did.
steps:                 # every plan step this turn touched
  - id: <plan step slug or uuid>
    status: in_progress | implemented | verified | blocked
    note: optional
tests:
  written: true        # tests were added or updated for this turn's work
  ran: true
  green: true
  detail: cargo test -p tod-store — 84 passed
remaining:             # what this turn did not finish
  - Wire the side pane to the report.
blockers:              # needs the user; empty unless status is blocked
  - Should the cap be configurable?
notes: |
  Free prose, optional. Rendered as the answer body.
```

`notes` is the only free-text field and it is optional; everything else is
structured.

Models asked for a bare document routinely wrap it anyway — a line of prose
and a fenced block (a real Claude run did exactly this). So the parser reads
the whole reply, then the last fenced block, then the reply from its first
`status:` line, and takes the first that parses; only a reply with no readable
report at all gets a correction turn (§4.5).

An accepted reply is stored in `conversation_turns.body` as readable markdown
(`Report::to_markdown`: the summary as the headline, then steps, tests,
what remains, and the notes) and in `conversation_reports` as the structured
report. The raw text of an accepted reply is not kept; a rejected one is, in
its error turn.

### 4.3 Done

A turn ends the exchange when **all** of:

1. `status: complete`.
2. No plan step on the node is `pending`, `ready`, `in_progress`, or
   `blocked` — every one is `implemented` or `verified`
   (`tod_store::outline::repos::plan_steps`).
3. `tests.written`, `tests.ran`, and `tests.green` are all true.

Condition 2 is read from the store, not the reply — the agent closes steps
through `tod-cli plan update --status` as it goes, and the app checks what
landed. Condition 3 is agent-reported and trusted.

`status: blocked`, or any plan step in `blocked`, stops the loop and hands
back to the user regardless of the other conditions.

### 4.4 The loop

Always on for this protocol. When a turn finishes and the done-check fails,
the driver appends a `continuation` turn and immediately starts another turn
carrying the remaining work — without the user.

Stops on any of:

- **Done** (§4.3).
- **Blocked** — `status: blocked` or a blocked plan step.
- **Cap** — 10 continuations per user message.
- **No progress** — a continuation that closes no plan step and changes no
  file in the worktree. Checked against the step statuses and `git status
  --porcelain` before and after the turn.
- **Stop** — the user. The transcript panel's existing Stop kills the loop,
  not just the turn in flight.

The header shows the continuation count while a loop runs.

A `continuation` turn is a one-line transcript marker, exactly as
`TurnRole::Rotation` already is — *not* a synthesized user turn. The user can
always see that the loop ran and how often.

### 4.5 Malformed replies

A reply that does not parse gets **one** correction turn quoting the schema
and asking again. If the second reply also fails to parse, the driver records
an error turn holding the raw text, marks it in the transcript as a protocol
error (distinct styling, so it is obvious while troubleshooting), and hands
back to the user. Malformed replies do not count against the continuation cap.

### 4.6 Side pane

Plan steps first, with live status, in plan order — the same data the
done-check reads, so the user sees what the gate sees. Below them, the files
changed in the worktree (`git status --porcelain`, refreshed when a turn
finishes). A header strip carries test status from the latest report and the
loop's continuation count.

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
3. **The implementation protocol.** *Done.* Reply schema, done-check, loop,
   side pane, `--agent mock` support. **Implement** opens the conversation
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
  its agent session id. Two consequences until then: the one-live-run-per-node
  lock (`live_implementation_session_for_node`) no longer covers
  implementation conversations, and an agent orphaned by a crash (§3.2) is not
  found again. §3.1 describes a fleet-run link that may not be the shape this
  ends up taking.

## 7. Out of scope

- The `verifying` lifecycle phase gets its own protocol and conversation
  later. Unit tests shipping green is an `active`-phase obligation and is
  covered by §4.3; end-to-end and manual verification are a separate phase.
- Capturing test output in the app. Test status is agent-reported (§4.3).
- Terminal sessions and prompt queuing for implementation runs (§3.1).
