# Journeys: implementation plan

How to build [spec.md](spec.md). Read the spec first; this document says
*where* and *in what order*, and does not repeat *why*.

Line numbers below were correct when this was written and will drift. Search
for the names, not the numbers.

## Ground rules

- Follow `.claude/CLAUDE.md`. In particular: run every `cargo` command with a
  ~120 s timeout, scope tests to the crate you touched, and never launch `tod`
  without `--data-root`.
- **Never block the UI thread.** Recording is a non-blocking channel send.
  Anything that touches a journey file, the database, or the network runs on
  a background thread.
- **Recording must never fail the operation being recorded.** A journey error
  is logged with `tracing::warn!` and dropped.
- Each step below ends in a commit with its tests passing, and leaves the app
  working. Steps 1–5 change nothing the user can see except the stored
  per-turn context.
- A schema bump (`CURRENT_USER_VERSION` in `crates/tod-store/src/fleet/schema.rs`)
  requires `cargo build -p tod-cli` as well, since `tod-cli` refuses a
  database of a different version.

## Where the code goes

| Piece | Crate | Why there |
|---|---|---|
| Record types, CBOR, writer (tail, compaction, index), reader, retention, sealing, relay code, bundle stream | **new `crates/tod-journey`** | A leaf with no `tod-*` dependencies, so the receiver can use it without GPUI or SQLite. |
| ntfy client (`put`, poll) | `crates/tod-integration` (new `ntfy.rs`) | That crate is where transport to external services lives. Uses its existing blocking `reqwest`. |
| `journey_changes` table and triggers, submission queue table, turn-range query, per-turn context column | `crates/tod-store` | Schema and repos. |
| Recorder (writer thread, global handle), change drain, validity tracking, bundle exporter, submission worker, settings snapshot | `crates/tod-core` (new `journey/` module) | Needs the store; shared by the app's threads. |
| Protocol stop reasons, driver and gate-check recording | `crates/tod-core` (existing modules) | Where the decisions are made. |
| App journey ring buffer, user-action recording, `ReportProblem`, dialog, screenshot, settings section | `crates/tod-ui` | GPUI. |
| Receiver (`init`, `pull`, `show`, `stats`) | **new `crates/tod-journeys`** (binary) | Runs on the receiving desktop. Depends on `tod-journey` and `tod-integration` only. |

Add both new crates to `members` in the root `Cargo.toml`. There is no
`[workspace.dependencies]`; pin versions per crate as the others do.

New dependencies: `ciborium`, `zstd`, `age` (in `tod-journey`); `uuid` with
`serde`, `serde`, `chrono` as elsewhere. `zstd` builds C code through `cc`,
which works on all three CI platforms; check that CI still passes after step 1.

---

## Step 0: Store the per-turn context sent to the agent

The spec requires every prompt to be stored (§3.2). The opening context is
(`conversations.opening_context`), but the per-turn delta is not.

- `crates/tod-core/src/conversation/driver.rs`, `ConversationDriver::send`
  (~L216–280): `changes` is computed with `protocol_delta` and sent as
  `join(&changes, text)`, but only `text` is stored as the `User` turn.
  Continuation turns (`land_reply`, `Next::Continue`) send a message too.
- Add a nullable `sent_context TEXT` column to `conversation_turns` (schema
  bump; follow the `if version < N` chain in `apply_migrations`). Store the
  part of what was sent that is not the user's own text: the delta for a user
  turn, and nothing extra for a continuation (its body is already what was
  sent). Thread it through `InterviewCommand` / `append_turn_with_parts` in
  `crates/tod-store/src/conversation/repo.rs` and
  `crates/tod-store/src/interview/command.rs`.
- Include it in `Turn` (`crates/tod-store/src/conversation/types.rs`).
- Also add `ConversationRepo::turns_range(conversation_id, from_seq, to_seq)`,
  which the exporter needs in step 6.

**Tests**: a driver test with the mock agent where the second send has a
delta, asserting the stored turn's `sent_context` equals what the mock
received. A repo test for `turns_range`.

## Step 1: `tod-journey` crate: format, writer, reader

`crates/tod-journey/src/`:

- `record.rs`: `Record`, `Actor`, `Event`, and the payload types from spec
  §4.1 (`Presented`, `PresentedAction`, `CriterionResult`, `GateReport`,
  `Regression`, `Decision`, `TurnPhase`, `RowRef`, `Reference`,
  `Resolution`, `NavEvent`, `Manifest`, `Blob`). Plain data with serde,
  using strings and `Uuid`, not `tod-*` types. Use `#[serde(default)]` on
  every field added after the first release, and never `deny_unknown_fields`.
- `key.rs`: `JourneyKey { Node(Uuid) | Project }` and its file stem.
- `writer.rs`: `JourneyWriter` (synchronous; the thread is in tod-core):
  - `open(dir, key)`: reads the `.idx` and the tail, recovers (§4.3), and
    returns the next seq.
  - `append(&mut self, actor, event) -> seq`: stamps seq and time, encodes
    one CBOR item, appends it to the tail and flushes (no fsync per record).
  - `compact(&mut self)`: tail becomes one zstd frame appended to `.zst`
    (fsync), then the index is rewritten atomically (write temp, rename),
    then the tail is truncated. Called when the tail passes 64 KB, on
    milestones, and on open if the tail is non-empty.
- `reader.rs`: `JourneyReader::open(dir, key)` and `from_reader(impl Read)`
  (for bundles). It is an iterator of `Record` that chains the `.zst` stream
  (`zstd::stream::read::Decoder` handles concatenated frames) and then the
  tail, dropping non-increasing seqs and a torn final record, and stopping at
  an optional `up_to` seq.
- `retention.rs`: `enforce_cap(dir, cap_bytes, keep: JourneyKey)`: sum the
  sizes of every journey's files, and while over the cap delete the journey
  (all three files) with the oldest modification time, never `keep`.
- `seal.rs`: `seal(recipient: &str, plaintext) -> Vec<u8>` and
  `open(identity, ciphertext)` with the `age` crate (X25519), plus
  `split(bytes, max) -> Vec<Vec<u8>>` and `join`.
- `relay_code.rs`: `RelayCode { recipient, server, inbox, ack }`, encoded as
  `todj1:` + base64url of its CBOR. `parse` checks the recipient is a valid
  age X25519 recipient.
- `bundle.rs`: `BundleWriter` (writes a CBOR sequence into a zstd encoder
  over a `Vec<u8>`) and reading a bundle with `JourneyReader::from_reader`.

**Tests** (in the crate, using `tempfile`):
- Append and read back; compaction across several frames; reopening
  continues the seq.
- Crash cases: a tail left after a compaction whose index was written; a
  frame written but index not; a torn final record (truncate the tail
  mid-item). Each reads back with no duplicates and no error.
- Unknown fields and variants from a newer writer are skipped (encode a
  record with an extra field by hand).
- Retention evicts oldest-modified first and never the kept key.
- Seal and open round-trip with a generated identity; split and join.
- Relay code round-trip; a bad recipient is rejected.

## Step 2: Recorder and data-change feed

### 2a. Paths and settings

- `crates/tod-store/src/paths.rs`: `journeys_dir()` (`<data_root>/journeys`),
  following `visual_design_dir`.
- `crates/tod-store/src/settings.rs`: add `journeys: JourneySettings` to
  `TodSettings` with `#[serde(default)]`, in `Default`, and in `validate()`:

  ```rust
  pub struct JourneySettings {
      pub send: bool,                     // default false
      pub include_transcripts: bool,      // default false
      pub relay_code: Option<String>,     // default None
      pub milestone_states: Vec<String>,  // default: active, verifying, review, approved, done
      pub storage_cap_mb: u64,            // default 1024
  }
  ```

  `validate()` rejects `send` without a relay code that parses, and unknown
  lifecycle states.

### 2b. `journey_changes`

In `crates/tod-store` (a new module, e.g. `journey_changes.rs`, with its DDL
const; register it in the next schema bump, following how v55 registers
`learn::CREATE_LEARN_TABLES`):

```sql
CREATE TABLE journey_changes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  node_id BLOB NOT NULL,
  tbl TEXT NOT NULL,
  row_id TEXT NOT NULL,
  op TEXT NOT NULL,            -- insert | update | delete
  old_state TEXT, new_state TEXT,  -- node_lifecycle only
  at TEXT NOT NULL
);
```

- Add `AFTER INSERT / UPDATE / DELETE` triggers on every table with a node id
  column: at least `nodes`, `node_lifecycle`, obligations, plan steps,
  verdicts (`tod_store::verification`), review findings
  (`tod_store::review`), test runs, `node_gate_evaluations`, capability
  tables (`node_files` and the others), and content. List every table in the
  schema that has a `node_id` and decide per table. The rule is to include
  it, so a table left out needs a reason in a comment. Model the triggers on
  the `trg_ic_*` ones in `fleet/schema.rs`.
- Add a repo: `changes_after(conn, after_id) -> Vec<ChangeRow>` (read
  connection) and `prune_through(id)` as a writer command.
- `conversation_*` tables are not included: conversations are recorded
  through the driver (step 3).

**Tests**: each trigger fires for insert, update, and delete, and the
`node_lifecycle` trigger records old and new state.

### 2c. The recorder

`crates/tod-core/src/journey/`:

- `recorder.rs`:
  - `Recorder` is a cloneable handle holding a `std::sync::mpsc::Sender<Msg>`.
  - A process-wide `OnceLock<Recorder>` has `install(recorder)` and
    `pub fn record(key: JourneyKey, actor: Actor, event: Event)`, which does
    nothing when none is installed. Everything else in the app records
    through this function. (`tod_core::logging` already uses process
    globals the same way.)
  - `spawn(paths, store: Arc<FleetStore>, settings) -> Recorder` starts the
    writer thread. It keeps a map of open `JourneyWriter`s (close a writer
    idle for 10 minutes), compacts every journey with a non-empty tail on
    startup, and calls `retention::enforce_cap` after each compaction.
- `changes.rs`: a second thread subscribes with `store.subscribe_changes()`
  (`tokio::sync::broadcast::Receiver<()>`; use `blocking_recv`, and treat
  `Lagged` as "drain now"). On each signal:
  - Read `changes_after(high_water)`, group by node, and record one
    `DataChanged` per node.
  - For each `node_lifecycle` row, record `Transition { from, to }`. If `to`
    is in `milestone_states`, record `Milestone` and ask the writer to
    compact; the queueing half comes in step 7.
  - For each node touched, compute
    `tod_core::lifecycle_validity::regression(conn, node)` and record
    `Validity` if it differs from the last value recorded for that node (an
    in-memory map).
  - Keep `high_water` in memory and in `journeys/changes.mark`. Every few
    minutes, and at startup, prune through it with the writer command.
- `crates/tod-ui/src/app/window.rs`: start the recorder where the store and
  mutation socket are started (~L1518) and `install` it. The recorder must
  not start for a data root that fails to open, and it stops with the app.

**Tests** (tod-core, with a temp data root and store, which other tod-core
tests already set up):
- Enqueueing an obligation mutation produces one `DataChanged` in that node's
  journey.
- `SetLifecycle` into `verifying` produces `Transition` and `Milestone`, and
  compacts the journey.
- `record` with no recorder installed is a no-op.

## Step 3: Decisions from tod-core

### 3a. Why a protocol loop stopped

`crates/tod-core/src/conversation/protocol.rs`: `Next::Done` becomes
`Next::Done(Stop)`:

```rust
pub enum Stop {
    Complete,                 // the protocol's definition of done holds
    HandBack(String),         // e.g. "step 5 is blocked"
    ContinuationCap,
    NoProgress,
}
```

Update `next` in `implement.rs`, `verify.rs`, `review.rs`, `fix.rs`,
`gate_check.rs` and any other implementors to return the specific reason
they already compute. The cap and no-progress paths are shared, so return
those from there. Also give `Next::Continue` a short `reason: String`
("3 plan steps open").

### 3b. Driver events

In `crates/tod-core/src/conversation/driver.rs`, key every record by
`self.focus.node_id()` (or `Project`), with `Actor::Agent { conversation }`
or `Actor::App`:

- `start` (~L585): `AgentTurn { Started { user_seq } }`.
- `tick` / `poll` (~L306–412): `Replied { seq }` on success, and
  `Failed { seq, error }` on the error paths.
- `cancel` (~L200): `Stopped`.
- `rotate_and_start` (~L548): `SessionRotated { reason }`, where the reason
  is "over budget", "not resumable", or "cold resume failed", as known at
  each call site.
- `land_reply` (~L421): after `next`, `ProtocolDecision` with the protocol
  kind and `Continue { reason }` or `Stop`.

### 3c. Gate results

In `crates/tod-core/src/conversation/gate_check.rs`:

- `agent_criteria` / `settle_derived_criteria`: record `GateResult` with the
  derived criteria (source `derived`).
- `GateCheckProtocol::apply` (~L270): record `GateResult` with the agent's
  `gate_results`, the `GateReportRecord` summary and blockers, and the
  forward state if it advanced.

Waivers and human advances are recorded as user actions in step 4. The
resulting row changes and transitions come from step 2 either way.

**Tests**: extend the existing protocol tests to assert the `Stop` each
returns in its done, handed-back, cap, and no-progress cases. Add a driver
test with the mock agent and an installed recorder over a temp dir, asserting
the `Started` / `Replied` / `ProtocolDecision` sequence.

## Step 4: User actions, what was presented, and the app journey

### 4a. The presented snapshot

Build `Presented` from what each surface already computes:

- **Conversation view**: `crates/tod-ui/src/conversation/lifecycle.rs`.
  `lifecycle_action` (~L382) is the single handler for every lifecycle button
  there, from both the transcript footer and the side pane. At its top,
  build `Presented` from `lifecycle_controls(cx)` (every `PanelAction`: id,
  label, primary, disabled), the panel's current keyboard highlight, and the
  notices (gate notices and report notices, as text), then record
  `UserAction { action: id, source, surface: "conversation" }` to the node's
  journey. The source (click or keyboard) has to be passed in from the two
  callers (`transcript.rs` ~L83, `side_pane.rs` ~L967).
- **Lifecycle panel**: `crates/tod-ui/src/views/lifecycle_panel.rs`. Clicks
  today are separate `cx.listener` closures, and only the keyboard goes
  through `activate_focused` (~L350). Refactor so that every button's
  `on_click` and `activate_focused` both call one
  `fn perform(&mut self, stop: LifecyclePanelStop, source: Source, window, cx)`,
  which records and then does what the closure did. Add stops for the
  actions that have none (Waive per criterion, Advance after criteria, Back
  to active, per-criterion Open interview), keyed by criterion where
  needed. Build `Presented` from `stops()`, each button's primary and
  disabled state as rendered (Verify primary when `unchecked > 0`, Review
  when not reviewed, and so on), `focused_stop()`, `regression` (the callout
  text), `gate_status`, and `gate_error`. To keep the snapshot and the render
  from drifting apart, compute each button's primary/disabled flags in one
  function that both `render` and `perform` use.
- **Sends**: in `ConversationView::send` (`crates/tod-ui/src/conversation/mod.rs`
  ~L1325), record `UserAction { action: "send" }`. Record the turn seq and the
  text length, not the text; the text is in the transcript.

### 4b. The app journey ring buffer

`crates/tod-ui/src/ui/journey.rs`: a GPUI `Global` modelled on the status hub
(`ui/status.rs`, `HubGlobal`): a `VecDeque<Record>` trimmed to 30 minutes and
5,000 entries, plus `record_nav`, `record_action`, and `snapshot()`. Feed it
from:

- `Shell::select_view` (`app/window.rs` ~L209), `apply_drawer_request`
  (~L476), `open_conversation_with` (~L581), and `queue_open_interview`
  (~L261);
- `cx.observe_keystrokes` registered once at startup, recording the action
  name and keystroke when a keystroke resolved to an action (skip plain text
  input);
- every `UserAction` from 4a (write it to both the ring and the node
  journey through one helper);
- settings saves in `SettingsView` (`interview/views/settings.rs` ~L774),
  recording the keys that changed.

**Tests**: GPUI tests (see existing `#[gpui::test]` tests in tod-ui) that
drive `lifecycle_action` and `perform` and assert the recorded `Presented`
names the primary button. A ring buffer test for trimming.

**Check by hand**: run with `--agent mock --no-focus` and a scratch data
root, click through Implement → Verify → gate check, then read the node's
journey with a small test helper or `tod-journeys show` (step 9) and check
that the sequence and highlighted buttons are right.

## Step 5: Build identity and settings snapshot

- `crates/tod-core/build.rs`: also emit `TOD_GIT_COMMIT` (`git rev-parse HEAD`)
  and `TOD_GIT_DIRTY` (`git status --porcelain` is non-empty), falling back
  to `unknown` when git or the repository is missing (installed builds from
  a tarball). Rerun when `.git/HEAD` or the ref it points to changes. Expose
  them next to `CLI_BUILD_STAMP` in `crates/tod-core/src/lib.rs`.
- `crates/tod-core/src/journey/snapshot.rs`: `settings_snapshot(paths,
  settings, node) -> SettingsSnapshot`: the whole `TodSettings`, serialized,
  plus resolved agent platform, model, and effort per `AgentRole`
  (`launch_options_for`), data, process, and media roots (`TodInstallPaths`,
  `MediaPaths`), the `tod-cli` path and its build stamp, and the node's dev
  container settings.

**Tests**: the snapshot of a default settings file contains the resolved
agent settings for each role.

## Step 6: Reporting a problem and the bundle exporter

### 6a. Screenshot

Move `capture_client_rgba`, `main_hwnd` / `find_main_hwnd`, and
`write_png_fast` out of `crates/tod-ui/src/agent_socket/capture.rs` into an
ungated `crates/tod-ui/src/ui/screenshot.rs` (`#[cfg(windows)]`; other
platforms return `None`). `agent_socket` calls the new module. Its
dependencies (`image`, `windows`) are already unconditional. Capture happens
off the UI thread.

### 6b. The action and entry points

- `crates/tod-ui/src/ui/report_problem.rs`: `actions!(report_problem,
  [ReportProblem])` and a struct action `OpenReportDialog { key: JourneyKey,
  conversation: Option<Uuid> }`, following `ui/agent_chat.rs`
  (`OpenAgentChat` / `OpenConversation`). Bind `ctrl-shift-r` with no
  context, registered next to `register_agent_chat_keyboard_bindings` in
  `app/mod.rs`.
- Handlers, following the `OpenAgentChat` pattern: the conversation view
  (its conversation and focus), the task list and plan steps and obligations
  views (their selection), the lifecycle panel (its node). The shell fallback
  in `app/window.rs` (next to `on_open_agent_chat`, ~L667) uses the task
  list's selection, else the project.
- Title bar (`render_title_bar`, ~L938): a ghost compact button "Report a
  problem" with a shortcut pill, beside "Talk about the selection".
- Inline entries, each dispatching `OpenReportDialog`:
  - conversation header actions (`set_header_actions`, beside "Copy context",
    `conversation/transcript.rs`);
  - beside the gate status or error in the lifecycle panel (~L1433);
  - in the validity callout (~L1189);
  - on error entries in `ui/transcript_list.rs` (`render_entry`, error
    branch).

### 6c. The dialog

A modal through `window.open_dialog` (as `ui/agent_permission.rs` does), with
the node's title, one multi-line `Textarea` in edit mode from the start,
Ctrl+Enter to submit through `key_context::including_input` (as
`AgentConversationSubmit` does in `ui/agent_conversation.rs`), and Escape to
cancel. On submit, on a background task:

1. Snapshot the ring buffer (on the UI thread; it is in memory) and take the
   screenshot.
2. Record `Report { note, app_journey, screenshot }` to the journey.
3. If sending is on, insert a queue entry (6d) for the journey's current seq.
4. Toast: "Report recorded" or "Report recorded and queued to send", using
   `ui/toast.rs`.

### 6d. Queue table

In `crates/tod-store` (schema bump), a `journey_submissions` table with the
columns in spec §9.6, a repo, and writer commands to insert entries and
change their status. Each status change is also recorded in the journey as a
`Submission` record.

### 6e. The exporter

`crates/tod-core/src/journey/bundle.rs`:
`build_bundle(store, paths, settings, entry) -> Vec<u8>` (compressed, not yet
sealed):

1. `Manifest` (spec §5.3) and `Settings` (step 5).
2. The journey's records up to `entry.seq`. For `Report`, drop the
   screenshot when transcripts are excluded.
3. Collect every `Reference` the records make, and for each distinct one,
   in order, a `Resolved` record:
   - conversation turns: `turns_range`, including `sent_context` (step 0);
   - opening context: `ConversationRepo::opening_context`;
   - conversation actions: `ConversationRepo::actions` filtered to the
     referenced ids;
   - rows from `DataChanged`: the row's current state from its repo (and for
     verdicts, the verdict history), or `Missing` if deleted;
   - gate evaluations and reports;
   - agent session transcripts: `tod_agent::transcript::read_transcript(platform,
     session_id)`, using the session id from `conversations.agent_session_id`.

   Transcript references (turns, contexts, agent transcripts) resolve to
   `Withheld { size, at }` when `include_transcripts` is off.

**Tests**: with a temp store, a conversation, and a journey referencing
turns, build a bundle with transcripts on and off. Read it back with
`JourneyReader::from_reader`, and check the manifest, the `Resolved`
contents, `Withheld` when off, `Missing` for a deleted row, and that nothing
after `entry.seq` is included.

## Step 7: Milestones

In the change drain (step 2c), when a milestone is recorded and sending is
on, insert a `journey_submissions` entry with reason `milestone:<state>` for
the seq of the `Milestone` record. When inserting, mark as abandoned
("superseded") any still-`queued` (never sent) milestone entry for the same
node. Reports are never superseded.

**Tests**: two milestones before any send leave one queued entry; a report
entry is untouched.

## Step 8: Submission

### 8a. ntfy client

`crates/tod-integration/src/ntfy.rs`, blocking `reqwest`:

- `publish_file(server, topic, filename, bytes)`: `PUT {server}/{topic}` with
  header `Filename: <name>` and the bytes as the body.
- `publish_text(server, topic, text)`.
- `poll(server, topic, since) -> Vec<Message>`:
  `GET {server}/{topic}/json?poll=1&since={since}` (`since` is a message id,
  or `all`); each line is a JSON message. Keep `id`, `time`, `message`, and
  `attachment { name, url, size, expires }`.
- `download(url) -> Vec<u8>`.

Test against a local HTTP stub (no network in tests). A manual smoke test
against ntfy.sh is in the checklist below.

### 8b. Sink trait

In `tod-core::journey::submit`: `trait Relay { fn put(&self, name, bytes);
fn acknowledgements(&self, since) -> (Vec<Uuid>, cursor); }`, with
`NtfyRelay` (from the relay code) and `FolderRelay` (writes files; reads
`got <id>` lines from an `acks` file). Tests use `FolderRelay`.

### 8c. Worker

A background thread started with the recorder, and only while sending is on:

- Wake when an entry is queued (a channel from the queue insert) and every
  15 minutes.
- For each `queued` entry, or `sent` entry whose last send was 4+ hours ago:
  build (6e), seal (`tod_journey::seal` with the relay code's recipient),
  split at 14 MB, `put` each part, mark `sent`, increment attempts, and
  record `Submission`.
- Poll acknowledgements and mark `acknowledged`.
- Abandon entries first queued 7+ days ago that are unacknowledged.
- Network errors leave the entry as it is, to retry on the next wake. Log
  them, and show the last error in the settings section.

**Tests** with `FolderRelay`: queue, send, and acknowledge; resend after the
window (inject a clock); abandon after 7 days; a bundle over the split size
becomes several parts that `join` restores.

### 8d. Settings section

In `crates/tod-ui/src/interview/views/settings.rs`, add a `Journeys` variant
to `SettingsSection` (~L100) and to `SECTIONS`, and render it in
`render_active_section`:

- There is no boolean row today. Add a `toggle_row` (On/Off, activated with
  Enter or Space and by click, following the section's navigation rules)
  rather than using `cycle_row`.
- **Warning callout**: the style guide has no warning callout (the orange
  `callout-stale` means something else). Add a `callout-warning` entry to
  `doc/ui-style-guide.yaml` and implement it as `style::callout_warning` /
  `callout_warning_title` in `crates/tod-ui/src/ui/style.rs`, next to
  `callout_stale`. Use it for both warnings in spec §9.1, with the exact text
  given there.
- Rows: Send journeys (toggle, disabled until the relay code is valid),
  Include transcripts (toggle, disabled while sending is off), Relay code
  (`text_input_row`, validated with `RelayCode::parse`, error shown under
  it), Send a test (button; runs on a background task and shows the result
  inline), Milestone states, Storage cap (`stepper_row`, in MB), and the last
  delivery error if there is one.

**Check by hand**: toggles are unreachable in the wrong states, the warning
renders, and a bad relay code is refused.

## Step 9: Receiver `tod-journeys`

`crates/tod-journeys/src/main.rs` (use `clap` if other binaries in the
workspace do, or match `tod-cli`'s own argument parsing):

- `init [--server URL] [--home DIR]`: generate an age identity with
  `age::x25519::Identity::generate()` and write it to
  `<home>/identity.txt` (default: the OS data dir, `tod-journeys/`). Create
  two random topic names (32 characters from a CSPRNG), save
  `<home>/config.toml`, and print the relay code. Refuse to overwrite an
  existing identity without `--force`.
- `pull [--once]`: `poll(inbox, since=last)` and download each attachment.
  Buffer parts until all have arrived, then join, open with the identity,
  and decompress. Write `<home>/received/<bundle-id>.journey`, publish
  `got <bundle-id>` to the ack topic, and save the last message id. Without
  `--once`, repeat every 60 seconds (ntfy's streaming endpoint is an
  optional improvement). Skip bundle ids already received, but still
  acknowledge them.
- `show <file>`: see step 10.

**Tests**: `init` produces a relay code that `RelayCode::parse` accepts and
whose recipient opens with the saved identity. `pull` against `FolderRelay`,
or a local HTTP stub, assembles a split bundle and acknowledges it.

## Step 10: First analysis tool

In `tod-journeys`:

- `show <file>`: a readable timeline: time (and the gap since the previous
  record), actor, and a one-line rendering of each event. For `UserAction`,
  show the action, and in brackets the primary button presented if it was a
  different one. Then the resolved references, abbreviated (`--full` prints
  them in full).
- `stats <dir>`: across all received bundles:
  - how often the clicked action was not the primary one, by surface and
    lifecycle state;
  - the action most often following each action (e.g. what comes after
    Fix);
  - time spent in each lifecycle state;
  - gate failures per criterion;
  - protocol stop reasons per protocol.

**Tests**: a hand-built bundle with a Fix → gate check sequence where gate
check was primary renders that line in `show`, and is counted by `stats`.

---

## Done checklist

- [ ] `cargo check --workspace --all-targets` passes; CI green on all three
      platforms.
- [ ] `cargo test -p tod-journey -p tod-store -p tod-core -p tod-ui -p tod-journeys`
      passes.
- [ ] A release build (`--no-default-features`) still captures a screenshot
      for a report on Windows.
- [ ] End to end by hand: `tod-journeys init` on one machine, paste the code
      into a scratch app (`--data-root .local/agent/scratchpad/tod/root-journeys
      --agent mock --no-focus`), turn sending on, move a node into `active`,
      and file a report. `tod-journeys pull --once` receives both,
      acknowledges them, and `show` renders them. The app marks both entries
      acknowledged.
- [ ] With transcripts off, the received bundle has `Withheld` for every
      transcript reference and no screenshot.
- [ ] `.claude/CLAUDE.md` gains a short "Journeys" section: where recording
      happens (`tod_core::journey::record`), that new user-facing lifecycle
      actions must record a `UserAction` with `Presented`, and that new
      node-scoped tables need a `journey_changes` trigger.
