# Messages sent while the agent is working

Status: **design, not implemented.** Findings from 2026-10-01 were measured
against the real agents (below); the design is not built. Written for
`tod-agentd` (`doc/agentd.md`), where the daemon owns each node's one agent
session and one driver per conversation.

## The problem

Today `ConversationDriver::send_with_images` bails with "the agent is still
working on the previous message" when a run is in flight
(`driver.rs:337`), and the views repeat the refusal (`conversation/mod.rs:1515`,
`unified/chat_drawer.rs:401`). The user must wait for the whole turn, which on
a loop protocol (Implement, Verify, Review, Fix, Phase, Pr) can be many turns.

Wanted, as in the Claude desktop app and Claude Code: the message is **not**
held until the turn ends, and it is **not** a full interrupt: it is picked up
at the next opportunity, and a tool that is running is not cancelled.

## Findings

Versions: Claude Code 2.1.283, `@agentclientprotocol/claude-agent-acp` 0.81.2
(Agent SDK 0.3.280), `cursor-agent` 2026.09.02. Every observation is from a
run with a three-step "run `Start-Sleep -Seconds 12` three times" task and a
second message sent 6 to 7 seconds in; the scripts were throwaway
(`.local/agent/scratchpad/steer/`, not committed).

### Claude Code (CLI, `--input-format stream-json`)

A user message written to stdin mid-turn takes an optional `priority`
(`'now' | 'next' | 'later'`, `SDKUserMessage` in the SDK types):

| Priority | What happened to the running tool | When the model saw the message | Results |
|---|---|---|---|
| none (default) and `next` | **Not cancelled**; it ran to its end (15.9s) | Right after that tool finished; no further tool call was made | One turn, one `result` |
| `now` | **Aborted** (`Command was aborted`, 6.2s, `stop_reason: tool_use`) | At once, as a new turn | Two `result`s |
| `later` | Not cancelled; all three ran | After the whole turn (44s) | Two turns, two `result`s |

So Claude Code's "queue and inject at the next tool-call boundary" is the
default priority: a message arriving while a tool runs waits for the tool and
is delivered with its result, in the same turn. A reply with no tool calls in
flight is a boundary too, so a message at that moment simply starts the next
generation.

### Claude via ACP (`claude-agent-acp`, what we run)

- **A second `session/prompt` while one is in flight is accepted.** The agent
  advertises it: `agentCapabilities._meta.claudeCode.promptQueueing: true`.
  The adapter pushes the message onto the SDK's input stream with the default
  priority, so it behaves as above (delivered at the tool boundary, tool not
  cancelled).
- **Responses are per prompt, and the first one is a hand-off, not the end.**
  With prompts #1 and #2 outstanding, #1's response arrived at the boundary
  where #2 was taken: `stopReason: end_turn`, zero usage, no reply text, while
  the agent went on working (in the "also do X" run: two more tool calls, then
  the reply, 23 seconds later). #2's response arrived at the real end, with
  the usage and the whole reply streamed as `agent_message_chunk`s. If
  the injected message made the agent stop (my "change of plan" run), #1 came
  at the boundary and #2 a second later.
- **`_session/steering`** (advertised by `_meta.steering.supported`) injects
  into the running turn, but only with priority `now` (or `later` while a
  permission or elicitation is open). Measured: `{"outcome":"injected"}`, the
  running tool failed at once (`tool_call_update failed`), one response for
  the original prompt. So steering over ACP is an interrupt-and-redirect, not
  a boundary delivery. If no turn is running it starts one (or returns
  `promptRequired` when asked).
- **`session/cancel`** (a notification): the in-flight prompt resolved
  `cancelled` within 0.1s, the running tool was stopped, queued prompts
  resolve `cancelled` too, and **the session stays usable**: the next prompt
  worked and read the cached context (`cachedReadTokens` 50,749).

### Cursor ACP (`cursor-agent acp`)

- `initialize` advertises `loadSession`, `promptCapabilities` (image yes) and
  `sessionCapabilities.list`. No queueing or steering capability.
- **Not established.** The account hit its plan limit ("Upgrade your plan to
  continue"), so no turn did real work. The one signal: a second
  `session/prompt` sent while the first was outstanding was followed 0.7s later
  by the first resolving `cancelled`, which is what a new prompt superseding
  the running one would look like, but a `session/cancel` control run also
  cancelled in 0.6s, so a refusal-driven cancel cannot be excluded. `session/cancel`
  itself works (`cancelled`, session reusable). Treat Cursor as **unable to
  take a mid-turn message** until re-run with a working account.

### Our provider and driver today

- `tod-agent/src/cursor_acp.rs` (it hosts both agents): one worker thread per
  conversation. `ConversationWorker::turn` calls `prompt_blocks`, which
  `clear_reply()`s, sends `session/prompt`, and blocks in `await_response`
  until a response arrives. The worker reads its command channel **only
  between turns** (`run`, `recv_timeout`), so a second message cannot even be
  sent mid-turn.
- `await_response` returns the **first** `Response` and ignores its id
  (`AcpRequest::Response { _id: _, .. }`). With two prompts outstanding it
  would take #1's hand-off as the end of #2's turn.
- **`session/cancel` is never sent.** `cancel_run` sets the flag and
  `kill_tree`s the agent process; the session is resumable by id, but the
  process and its tool die, and the next message pays a respawn.
- The driver: `run: Option<Run>` is the in-flight turn; `send` refuses while
  it is set; `tick`/`poll` collect the outcome and ask `protocol.next(..)`,
  which either ends the exchange or `resend`s a `Continuation` turn.
  `send` resets `continuations` ("a user message ends whatever loop the
  previous one started") but a loop protocol still continues after the
  user's turn, since `next` derives from state.
- `context::take` (tod-supervisor) runs only at a stopping point, closes the
  conversation's session, clears `agent_session_id`, and appends a `Rotation`
  turn; the next turn starts fresh from a snapshot.

## The design

### What the user sees

A message sent to a node is **never refused for being busy**. It appears in
the transcript at once, with a delivery state under it:

| State | Meaning | Shown as |
|---|---|---|
| `queued` | Stored; not yet given to the agent | "Queued" (dimmed bubble; **Edit**, **Delete**, **Send now**) |
| `sent` | Given to the agent; it has not yet taken it (it is finishing a tool) | "Queued" (tooltip: waiting for the running tool); no longer editable |
| `delivered` | The agent took it | "Delivered" |
| `seen` | The agent produced output after taking it | "Seen" (the check shown for the reply that follows) |

`sent` is internal: three labels, four states, because the user cannot act on
the difference but a restart can (below). `seen` is an honest "the agent
reacted", not "the model read it": the first agent message chunk or tool call
after delivery.

**Send now** (on a queued message) is the interrupt: cancel the turn
(`session/cancel`), then send it. **Stop** stays a plain stop; queued
messages stay queued, listed with Send and Delete, and are not sent by
themselves.

### Where the queue lives

In the store, on the transcript row itself. `conversation_turns` gains
`delivery` (`queued | sent | delivered | seen`; NULL on every existing and
non-user row, which reads as delivered), `delivered_at`, and the user turn is
**appended when the user sends**, in the same transaction as today (images
already saved under `conversation-attachments/`, schema v73). No second table:
a queue entry is a user turn that has not been delivered, so ordering, the
journey rows, `net_changes` and the cloud sync need nothing new. The new
column needs the usual `journey_changes` coverage (it is on an existing
table, so the trigger already fires).

A client command `SendMessage { conversation, text, images }` goes to the
daemon, the one writer. It appends the row (`queued`) and returns the turn's
seq at once, whether or not a run is in flight, then wakes the process that
owns the conversation's agent session. That is the daemon's driver, or the
supervisor when one holds the node (the doc leaves open which, see below);
this design only needs "the owner of the session".

### Delivering at the earliest boundary the agent allows

The provider reports a capability per session, read from `initialize`:

```rust
enum MidTurn {
    /// A second `session/prompt` is taken at the next tool boundary and the
    /// running tool is left alone (claudeCode.promptQueueing).
    Queue,
    /// Cannot take one: deliver at the next turn boundary.
    None,
}
```

`Steer` is deliberately not a variant: over ACP it aborts the running tool
(measured), which is the interrupt, and we expose that only as **Send now**.
Claude with `promptQueueing` is `Queue`; Cursor (until proven), the mock agent
(unless a test turns it on), and a Claude adapter without the flag are `None`.

**`Queue`.** The owner marks the row `sent` and calls
`AgentProvider::queue_session_message(key, turn)`. In the worker this is a
new `ConversationCommand::Queue`, handled **inside** `await_response`'s loop
(it already wakes every 100ms): it sends `session/prompt` with a fresh id and
keeps waiting. Changes this forces in `cursor_acp.rs`:

1. `PersistentAcpSession` tracks the **set of outstanding prompt ids**, and
   `await_response` resolves a prompt by its own id instead of taking the first
   response.
2. A run's outcome is `Success` only when **all** its prompts have resolved;
   the **last** one's response is the turn's end and carries the usage, and
   the reply is the whole text since the first prompt. `prompt_blocks`'s
   `clear_reply()` runs only for the first prompt of a run.
3. A response to an *earlier* prompt while a later one is outstanding is the
   **hand-off**: it means the later message was taken. That is the event
   that moves it `sent` → `delivered` (the driver is told through the run's
   state, `AgentRunState::InFlight` gaining the delivered turn seqs). If the
   earlier prompt resolves `cancelled`, the adapter has also cancelled the
   later ones: they go back to `queued`.
4. Usage from the hand-off (zero) is added like any other; the idle timeout
   already restarts on any inbound update.

The context delta (`protocol_delta`: the user's own edits and reversals since
the previous turn) is computed **when the row goes `sent`**, as `send` does
now, and prepended to that message, not when it was queued; it is then fresh.

**`None`, and every failure of `Queue`.** The row stays `queued`. At the next
turn boundary (the run ends, `poll` has recorded the reply) the driver sends
**all** queued messages as **one** turn, in order, joined as the user wrote
them, and marks them `delivered` together. One turn, not one per message:
there is no benefit to answering the first of three queued corrections before
the second.

### Seen and the transcript

`delivered` → `seen` on the first agent update after the hand-off. The
transcript should show the user's message where it was delivered, not after
the whole reply. But its turn was appended (with its `seq`) when the user sent
it, before the reply it interrupts is saved. Splitting the reply at the
hand-off (the part so far, then the user's turn, then the rest) means the
driver saving the first part as an agent turn with a seq after the user's, and
is the one place seq order is not the order of events. It is listed as open;
the cheaper alternative is one agent turn with a marker where the message
landed.

### Loop protocols

The autopilot's next turn (`Next::Continue`, `resend`) and the user's message
race at every turn boundary. The rule: **a user message always goes first.**

- At the boundary, before `protocol.next(..)` is asked, the driver drains the
  queue. If anything is queued it sends that (the `None` path above) *instead
  of* the continuation message, then evaluates `next` after that turn as it
  does after any turn. A loop protocol therefore carries on after the user's
  turn from state (open plan steps, unverified obligations), with nothing
  replayed; the continuation it would have sent is simply not sent.
- A user message in either path (injected or at the boundary) **resets
  `continuations`** (as `send` does) and **clears `progress_before`**, since
  the fingerprint the turn started with was taken before the user changed
  direction; `progressed` is then "nothing to compare", which never stops a
  loop (`driver.rs` already does that for a missing baseline). Without this a
  steered turn that made no progress would end the run as `NoProgress`.
- **`Queue` mid-turn is the same on a loop**: the user's message joins the
  turn in flight; the loop's own continuation is unaffected.
- **Paused.** A message to a node whose autopilot is paused is delivered as one
  turn, and the run stays paused afterwards (a message is a conversation, not
  a resume). Open question below.
- **`Evaluate`.** An evaluation's session may not edit the node and is judging
  a fixed digest. It does not take mid-turn messages: they stay `queued` on the
  node's phase conversation and are delivered when the phase agent next runs
  (after the verdict). Open question below.
- **Phase certificates, gates and lifecycle validity** need nothing: a message
  that changes the outline makes the certificate stale by digest as before.
- Journeys: the user's send is already a `UserAction`; add `Presented`
  buttons for **Send now**, **Edit**, **Delete** per the conventions in
  `CLAUDE.md`, and record `delivery` transitions as `DataChanged` through the
  trigger.

### `context::take` and the poke

Two different wake-ups, which `signal::take_poke` must not conflate:

- The **context poke** (SIGUSR1, `context_changed_at`) means "look at the next
  stopping point" (`take` rotates the session, never mid-turn). It stays
  exactly as it is.
- The **queue wake-up** means "act now". Mid-turn it must not wait for a
  stopping point. It is a command to the session's owner, not a signal.

Ordering at a boundary: `take` first (it ends the session and starts a fresh
one from a snapshot), **then** the queue is drained onto the new session, as
the first user message of a rotation (`rotate_and_start` already takes the
user's text). A queued message therefore survives a rotation, and arrives with
the new context. Because `take` runs only when no run is in flight, no
injected (`sent`) prompt can be outstanding when it closes the session; the one
race, a message injected in the instant the run ends, resolves in the adapter
as a new turn, and the context poke is then taken at the end of *that* turn.

A context poke while a `Queue` message is extending a turn is simply deferred
to that turn's end, as any poke during a turn is.

On Windows nothing sends SIGUSR1 (`signal.rs`), and the daemon needs a channel
to the supervisors it spawns anyway; the queue wake-up should use it (see open
questions) rather than a second signal.

### Restart and durability

Rows are the queue, so a restart loses nothing queued. What is lost is the
agent process and whatever it had taken but not finished:

- `queued` rows stay and are delivered on the first turn after the restart (the
  session resumes by id, or rotates).
- `sent` rows (given to the agent, no hand-off seen) are ambiguous: the agent
  may have taken the message before dying. On restart they go back to `queued`
  and are **re-delivered**, which risks the agent seeing one twice. The
  session transcript can settle it: the adapter stamps each prompt with a
  uuid and the CLI records user messages in the session `.jsonl`, so the
  daemon may look for it first (see open questions); the default is to
  re-deliver, since a repeated instruction is cheaper than a lost one, and the
  re-delivery is marked in the transcript.
- `delivered`/`seen` are history.

## What changes where

| Where | Change |
|---|---|
| `tod-agent` | `MidTurn` capability from `initialize`; `AgentProvider::queue_session_message`; worker handles `Queue` inside `await_response`; per-id prompt tracking; `session/cancel` for Stop (kill the process only after a grace period), which **Send now** needs; mock agent can opt into `Queue`. |
| `tod-store` | `conversation_turns.delivery`, `delivered_at` (schema bump, migration); `SendMessage`, `MarkDelivery`, `EditQueued`, `DeleteQueued` commands; `net_changes` etc. unchanged. |
| `tod-core::conversation` | `send_with_images` no longer bails: append, then deliver or leave queued; `poll` drains the queue before `protocol.next`; reset `continuations`/`progress_before`; reply split at the hand-off. |
| `tod-supervisor` | Queue wake-up handled in the turn-wait loop (not at stopping points); `take` before drain. |
| `tod-ui` | Delivery state under a user bubble; Edit/Delete/Send now on `queued`; the input never disabled while a run is in flight; the "still working" errors removed. |
| Docs | `doc/conversation/spec.md` (the transcript), `doc/agentd.md` decision 8 now points here. |

## Tests

- Provider, against the scripted fake ACP agent (`cursor_acp.rs` has one):
  a second prompt mid-turn resolves in order; the hand-off response does not
  end the run; `cancelled` on the first returns the rest to `queued`; an
  agent without the capability is left `queued`.
- Driver, with `--agent mock` and a mock that takes injected messages: the
  states `queued → sent → delivered → seen`; three queued messages deliver as
  one turn at the boundary; a loop protocol sends the queue **instead of** its
  continuation and then continues; `NoProgress` is not raised by a steered
  turn; a `take` between queueing and the boundary delivers onto the fresh
  session.
- Restart: kill after `sent`, restart, the message is re-delivered once and
  marked.
- Live (gated on a real adapter, as the other `TOD_TEST_*` smoke tests): the
  three-sleep run above, asserting the tool is not cancelled and the answer
  follows the first tool.

## Open questions

1. **Which process owns the session**, and how the daemon reaches it. The
   daemon owning the driver is simplest (one process, no channel); if a
   supervisor holds the node's agent (as in `doc/agentd.md`), the daemon needs
   a channel to it that works on Windows. A line protocol on the supervisor's
   stdin is the likely answer; the cloud has only the orchestrator's poke
   (SIGUSR1 through the relay), which is read at stopping points, so **a cloud
   node gets the turn-boundary fallback** until the supervisor's turn-wait
   loop also watches it.
2. **Cursor**: re-run the experiment with an account that has quota. If a
   second prompt supersedes the first (a cancel), it must stay `None`; if it
   queues, make it `Queue`. Also check whether Cursor has an extension
   method for steering.
3. **Split the reply at the hand-off** (above) or keep one agent turn and mark
   where the user's message landed. Splitting reads right but touches
   `conversation_turns` ordering and the `net_changes` projection.
4. **A message to a paused node**, and to a node whose active conversation is
   an `Evaluate`: held for the phase agent (recommended), or refused with a
   reason.
5. **Re-delivery after a restart**: check the session `.jsonl` for the prompt
   uuid (precise, Claude only, not in a container the app cannot read) or
   re-deliver and mark it (simple).
6. **Several injected messages**: each goes as its own prompt (what was
   measured: one) or batched while one is already `sent`. Batching a second
   message that arrives while the first is still `sent` would avoid a stack of
   hand-offs; it was not measured.
7. **`Send now` while a permission is open**: the adapter uses `later` for a
   steer then, to avoid stranding the permission request. A plain
   `session/cancel` does not have that hazard, which is another reason to
   build Send now on cancel and never on steering.
8. **Hand-off usage**: the first prompt's response carries zero usage, and the
   last carries the whole turn's. The per-turn usage the driver records
   (`session_token_usage`) is therefore right as a sum, but the cost of a
   steered turn is attributed to the last message only.
