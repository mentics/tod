# Journeys

A record of what happened, kept as it happens, so that when something goes
wrong, or when a node simply reaches a milestone, the story can be packaged up
and analyzed in development.

The step-by-step build plan is [implementation-plan.md](implementation-plan.md).

## 1. The problem

When the app does not do what the user expected (an agent stops short, a gate
check fails for no visible reason, the lifecycle stalls) there is nothing to
hand to development but a description from memory. `tod.log` carries little.
The conversation tables and agent transcripts carry a lot, but not the part
that usually explains the problem: **what the user did, and what the app was
showing them when they did it.**

A real case: a node in `verifying` failed its gate check with a finding. The
user clicked Fix, and the agent fixed it. The lifecycle panel then highlighted
"Run gate check", so the user clicked that. The right step was Verify: after a
Fix, verification has to run again to update the verdicts. The sequence
*Fix → gate check, with gate check highlighted* diagnoses it at a glance.
Without that sequence, nobody can tell whether the user made a mistake or the
app pointed the wrong way.

The same record serves a second purpose. Even when nothing goes wrong, a
node's journey shows where the user hesitated, backtracked, or clicked the
wrong thing, which is what we need to make the journey shorter. This is not
only about fixing problems but about improving the experience.

## 2. The idea

Two kinds of journey, stored differently because they are used differently:

| | Node journey | App journey |
|---|---|---|
| Covers | Everything that touched one node, over its whole life | Second-by-second navigation of the whole app |
| Storage | Append-only file per node, on disk | In-memory ring buffer |
| Kept | Until the storage cap evicts it (§4.5) | The last 30 minutes |
| Answers | "Why did this node's lifecycle go this way?" | "How could the user have got there faster?" |

There is also one **project journey** per data root, for events with no node
(project-focus conversations). A project is not a node and has no id, so it
has one fixed file (§3.3).

A **bundle** is a self-contained snapshot of one node's journey (or the
project journey) up to a given record, with everything the journey references
pulled in from the database, plus a settings snapshot. A bundle is queued when
the user reports a problem, and at lifecycle milestones. It is built only to
be sent, encrypted so that only the receiver can read it, and never kept.
Sending is opt-in.

## 3. Node journeys

### 3.1 What is recorded

Every event that concerns the node, in order:

- **User actions**: Implement, Verify, Review, Fix, Fix failed, gate check,
  Advance, Waive, Move back, Back, force advance, revert, open interview,
  check incoming, and sends in a conversation focused on the node. Each
  records its *source* (click, keyboard activation, shortcut) and the surface
  it came from (lifecycle panel, conversation view).
- **What the app presented** when the action was taken: every button that
  was on offer (id, label, primary or not, disabled or not), which one had
  the keyboard highlight, and the notices and callouts showing (the validity
  callout's text, the gate status or error). Every user action carries this
  snapshot. It is what separates a user mistake from the app pointing the
  wrong way.
- **App decisions, with reasons**:
  - lifecycle transitions (from, to), whoever caused them;
  - gate check results per criterion (outcome, detail, source: derived,
    agent, or human) and the gate report (result, summary, blockers);
  - the validity ruling when it changes (`lifecycle_validity::regression`:
    target and reasons);
  - protocol loop decisions: continue (and why), or stop (and why: complete,
    handed back, continuation cap, no progress).
- **Agent activity**: turn started (conversation, user turn seq), turn ended
  (reply seq, or error), stopped by the user, and session rotation (reason).
  The agent's `tod-cli` calls are not recorded separately: they are in its
  transcript, which the journey references.
- **Data changes as the app sees them**: every change to a node-scoped row,
  whoever wrote it (the user, the agent through `tod-cli`, the app itself).
  See §3.4.
- **Milestones, reports, and submissions** (§6, §5, §9).

Conversations have a focus. Events from a conversation focused on a node, or
on one of its obligations or plan steps, go into that node's journey
(`Focus::node_id`). Project-focus conversations go into the project journey.

### 3.2 References, not copies

Large data that already lives in the database is **referenced**, not copied:
conversation turns, the opening context and per-turn context sent to the
agent, the change set, verdict history, review findings, gate evaluations. An
agent session's own transcript (Claude/Cursor) is referenced by platform and
session id. A reference names what it points to precisely enough to extract
later (e.g. conversation id plus turn seq range).

This keeps journey files small, and it means a bundle never needs the database
itself: the exporter resolves the references a journey actually makes (§5.3).

Full transcripts, including every prompt exactly as sent, must be stored in
the database. Today the opening context is (`conversations.opening_context`)
but the per-turn context (the delta of changes prepended to each user message)
is computed at send time and not kept. That is a bug, and storing it is part
of this work.

### 3.3 Files

```
<data_root>/journeys/nodes/<node-uuid>.journey.zst    compacted frames
<data_root>/journeys/nodes/<node-uuid>.journey.tail   recent, uncompressed
<data_root>/journeys/nodes/<node-uuid>.journey.idx    small index (§4.3)
<data_root>/journeys/project.journey.zst / .tail / .idx
```

The UUID is the node's stable UUID (`nodes.id`), not its slug, which can
change.

### 3.4 Data changes

The store's change signal carries no payload: it says only that something
committed. So data changes get their own feed, built the way the incoming-
changes tracking already is (triggers in `fleet/schema.rs`):

- SQLite triggers on the node-scoped tables insert a row into a
  `journey_changes` table: node id, table, row id, operation (insert, update,
  delete), time, and for `node_lifecycle` the old and new state.
- After each commit, the recorder drains `journey_changes`, groups the rows by
  node, and writes **one `DataChanged` record per node per drain**, listing
  every row touched. A burst of agent writes is one record, not dozens.
- Every node-scoped table is covered, not a chosen subset: a reference costs a
  few dozen bytes, and which changes matter is exactly what is not known in
  advance (the Fix → gate check case turned on a finding's status).
- Text is recorded when it is saved, never per keystroke. This follows from
  recording committed rows.

Because the `node_lifecycle` trigger sees every write, transitions and
milestones are caught whoever made them, including writers added later.

## 4. File format

### 4.1 Records

A journey is a sequence of records. Each is encoded in **CBOR** (RFC 8949)
through serde (`ciborium`), and a file is a **CBOR sequence** (RFC 8742):
items written back to back, each one self-delimiting, so there is no framing
of our own.

Why CBOR over postcard/bincode: it is a standard, it is self-describing (a
reader can skip fields it does not know, so the schema can grow without
versioning every change), and it can be read outside Rust by an analysis
script. It is binary and compact, and it has native byte strings, so a
screenshot is stored as bytes, not base64. Repeated field names cost almost
nothing once compressed.

JSON appears only where JSON *is* the data (e.g. an agent's raw payload),
carried as an opaque byte string with its media type.

```rust
struct Record {
    seq: u64,              // per journey, strictly increasing
    at: i64,               // unix microseconds
    actor: Actor,          // User | App | Agent { conversation }
    event: Event,
}

enum Event {
    // node and project journeys
    UserAction { action, source, surface, presented: Presented },
    Transition { from, to },
    GateResult { from, to, criteria: Vec<CriterionResult>, report: Option<GateReport> },
    Validity { regression: Option<Regression> },
    ProtocolDecision { conversation, protocol, decision: Decision },
    AgentTurn { conversation, phase: TurnPhase },  // Started { user_seq } | Replied { seq } | Failed { seq, error } | Stopped
    SessionRotated { conversation, reason },
    DataChanged { rows: Vec<RowRef> },
    Milestone { state },
    Report { note, app_journey: Vec<Record>, screenshot: Option<Blob> },
    Submission { bundle: Uuid, status },           // queued | sent | acknowledged | abandoned
    // app journey only
    Nav { what: NavEvent },
    SettingsChanged { key, value },
    // bundles only
    Manifest { .. },
    Settings { snapshot },
    Resolved { reference: Reference, content: Resolution },  // found data, or Missing, or Withheld (transcripts excluded)
}

struct Blob { mime: String, bytes: Vec<u8> }
```

This is the shape, not the final type. Adding a variant or field is always
allowed. Renaming, removing, or reusing one is not: old journeys must stay
readable.

### 4.2 Tail plus compaction

Compressing each event on its own gains almost nothing, and holding events in
memory to compress in batches loses them in a crash, which is exactly when
they are wanted. So:

1. Each record is appended **uncompressed** to `<id>.journey.tail` as it
   happens.
2. When the tail passes 64 KB, when a milestone is recorded, and at startup
   (recovering a previous run), the tail is compressed into **one zstd
   frame** and appended to `<id>.journey.zst`.
3. The index is updated (§4.3), and then the tail is truncated.
4. zstd decodes concatenated frames as one stream, so the `.zst` file is just
   frames appended over time.

### 4.3 Sequence numbers and the index

`<id>.journey.idx` is a small CBOR file: creation time, the last seq
compacted, and the number of records compacted. It lets the writer continue
the sequence without decompressing the journey, and gives retention its
creation time.

- On opening a journey, the next seq is one past the larger of the index's
  last compacted seq and the last seq in the tail.
- At startup recovery, tail records whose seq is not greater than the index's
  last compacted seq were already compacted (a crash came after step 3's
  index update but before the truncate) and are dropped.
- Readers drop any record whose seq is not greater than the last one seen.
  That covers a crash between steps 2 and 3, where the frame was written but
  the index was not.
- A torn final record in the tail fails to decode and is dropped.

### 4.4 Writer

Only the app writes journeys, through one journey-writer thread fed by a
channel. Nothing on the UI thread waits on a journey; recording is a
non-blocking send. `tod-cli` never writes one: its calls are in the agent's
transcript, and the changes it makes reach the journey through
`journey_changes` (§3.4).

Recording is a no-op when no writer is running (tests, `tod-cli`), so code
that records never has to check.

### 4.5 Retention

Journeys are kept for a long time. A setting caps their total size on disk
(default 1 GB). When the cap is exceeded, whole journeys are deleted, the one
whose **last update** (file modification time) is oldest first, so a
long-running node that is still active survives a finished one that is newer.
The check runs after each compaction. The journey being written is never the
oldest, so it is never the one deleted.

## 5. Reporting a problem

### 5.1 Entry points

- A `ReportProblem` action bound app-wide to **Ctrl+Shift+R** with no key
  context, so it fires from anywhere, including a focused text field. The
  title bar shows a button and shortcut pill beside "Talk about the
  selection". It is scoped the way `OpenAgentChat` is: a view in the focus
  path that knows its selection handles it and supplies the node or
  conversation. The shell is the fallback (the task list's selection, else
  the project).
- Inline where things go wrong:
  - in the conversation view's header actions, beside "Copy context";
  - beside the gate status or error in the lifecycle panel;
  - in the validity ("Move back") callout;
  - on error turns in the transcript.

### 5.2 The dialog

A small modal with one multi-line field, "What did you expect?", which may be
left empty, and the node it is about as its title. Ctrl+Enter submits and
Escape cancels. It follows the navigation/edit-mode convention: the field
starts in edit mode because typing is the only thing to do.

On submit, a `Report` record is appended to the journey. It carries the note,
a copy of the app journey ring buffer, and a screenshot of the window, all
captured at that moment because none of them can be rebuilt later. If
sending is on (§9.1), the report is queued. A toast confirms either way,
saying whether it was queued or only recorded.

Screenshots are Windows-only today (the capture code uses Win32). On other
platforms the report has no screenshot, and says so.

### 5.3 The bundle

A bundle is a copy of data, so it exists only to be sent: it is built at
submission time, sealed, delivered, and dropped. What is kept is the journey
and the queue entry that says "send this journey up to record *seq*".

A bundle is itself a journey stream (same format, same reader), holding in
order:

1. A `Manifest`: bundle id, reason (report or milestone and its state), node
   id, slug, and title, the queued seq, build identity (`CLI_BUILD_STAMP`, git
   commit and whether the tree was dirty, crate version), schema version, OS
   and architecture, and whether transcripts are included.
2. A `Settings` snapshot (§7).
3. The journey's records up to and including the queued seq. For a report,
   that includes its ring buffer and screenshot; the screenshot is dropped if
   transcripts are excluded.
4. A `Resolved` record for each distinct reference the journey makes, in the
   order first referenced: the data itself, or `Missing` if it no longer
   exists, or `Withheld` if it is a transcript and transcripts are excluded.

## 6. Milestones

A `Milestone` record is appended, the journey compacted, and a bundle queued
(if sending is on) when a node enters one of a configured list of lifecycle
states. During the current phase of rapid development the list is broad, to
get feedback early:

`active`, `verifying`, `review`, `approved`, `done`

A later bundle contains everything an earlier one did, so the early ones are
redundant by design. They exist for early feedback. The list is a setting and
will be trimmed once the lifecycle settles.

Milestones are detected from `node_lifecycle` changes (§3.4), so they fire
however the node got there.

A milestone bundle has no screenshot and no app ring buffer. Those describe
the moment of a report, not the node's progress.

A queued milestone that has not been sent yet (the relay was unreachable) and
has been overtaken by a later milestone for the same node is dropped from the
queue: the later bundle contains it. Reports are never dropped this way.

## 7. Settings snapshot

Every bundle carries a snapshot of the settings: all of `TodSettings`, plus
what was resolved from them: agent platform, model, and effort per role; data
root, process root, and media root; the `tod-cli` path and build stamp; the
dev container configuration of the node. Settings drive a large share of
errors (wrong path, wrong model), so the snapshot is complete.

Settings hold no secrets today: credentials are requested lazily and kept in
the OS keyring, outside settings. If a secret is ever added to settings, it
must be marked as such and left out of the snapshot.

Settings changes are recorded as events in the app journey.

## 8. App journey

An in-memory ring buffer of the last 30 minutes, capped at 5,000 events:

- view switches, drawer opens and closes, and conversations opened;
- actions dispatched from the keyboard (the action name and the keystroke);
- the user actions of §3.1, whichever node they concern;
- settings changes.

Each carries its time. The time between events matters: a long gap before a
click is someone looking for something. The buffer is never written to disk
on its own. A report copies it into the journey (§5.2).

## 9. Submission

Bundles are only useful if they reach development without effort, and only
acceptable if the user decides what leaves the machine. Submission runs on a
background thread: build the bundle, seal it, deliver it, and record in the
journey what happened.

### 9.1 Settings

A **Journeys** section in the settings view:

- **Send journeys to development**: off by default. While it is off, reports
  and milestones are still recorded in the journey, but nothing is queued and
  nothing leaves the machine. Turning it on does not send anything recorded
  before. While it is on, each report and milestone is sent as soon as it is
  queued.
- **Include transcripts**: off by default, and only enabled while sending is
  on. Transcripts are where sensitive data is most likely to be: what the
  user typed, what the agent read and wrote, and the prompts it was given.
  With this off, a bundle still carries the journey itself (actions, what
  was presented, transitions, gate results, protocol decisions), the
  settings, and the user's report note, and every transcript reference is
  resolved as `Withheld` (its id, size, and time only). The screenshot is
  left out too, since it can show a transcript. Outline text (obligations,
  plan steps, review findings, verdict evidence) is included either way:
  agents write some of it, but it is unlikely to carry sensitive data. A
  separate switch can be added if that proves wrong.
- **Relay code**: one pasted string that holds the recipient's public key,
  the relay server, and the two topics (§9.5). Neither the key nor the topics
  are secrets: the key only encrypts, and the topics expose only ciphertext.
  Sending cannot be turned on until a valid code is entered.
- **Send a test**: sends a tiny sealed test bundle and reports whether the
  relay accepted it (§9.7).
- **Milestone states** and **Journey storage cap** (default 1 GB).

Next to **Send journeys to development**, a prominent warning (a warning
callout, not a caption):

> **Journeys leave this computer.** They are encrypted so that only the
> receiving computer can read them, but they are delivered to a computer
> that is not managed by your employer. If this is a work computer, make sure
> your employer's policy allows it before turning this on. Transcripts are
> left out unless you include them below.

A shorter form is repeated beside **Include transcripts** when it is on:

> Transcripts can contain anything you or the agent typed or read, including
> code and data from your work.

### 9.2 Seal: the app can write, but not read

A submitted bundle must be readable only by whoever receives it, and not by
the app that sent it. Any credential shipped with the app is public, so read
access cannot rest on one.

Each bundle is therefore **encrypted to a public key** before it leaves the
machine, in the [age](https://age-encryption.org) format (the `age` crate,
X25519 recipients). The private key never leaves the receiving machine.

- The app is write-only by construction. It cannot decrypt what it sent,
  and nothing it ships could.
- Where the bundle travels no longer matters for confidentiality. The relay
  only ever holds ciphertext.
- The receiving side decrypts with the standard `age` tool, or with the
  receiver command.

The bundle stream is zstd-compressed before sealing (encrypted data does not
compress).

### 9.3 Deliver: ntfy.sh as a relay

The app often runs on work computers, where only ordinary outbound HTTPS can
be counted on: no mesh networks, no inbound connections. So delivery is a
**relay**: the app pushes a sealed bundle to a holding place, and the
receiving desktop is notified, pulls it down, and acknowledges it.

The relay is [ntfy.sh](https://ntfy.sh), a free publish/subscribe service:

- **Push**: the app sends each sealed bundle as a file attachment to the
  inbox topic with one HTTP PUT, naming it with the `Filename` header
  (`<bundle-id>.journey.age`).
- **Topics**: two long random names, an inbox and an acknowledgement topic.
  Without an account, knowing a topic's name is what grants access to it, and
  all it grants is ciphertext.
- **Limits** of the public server: 15 MB per attachment, attachments expire
  after 3 hours, messages are cached for 12 hours, and there is a per-visitor
  storage cap. A sealed bundle larger than 14 MB is split into parts
  (`<bundle-id>.<n>-of-<m>.journey.age`) that the receiver joins.
- **Self-hosting**: ntfy is open source and a single binary. The server is
  part of the relay code, so moving to a self-hosted one changes the code and
  nothing else.

Delivery sits behind one trait (`put(name, bytes)` plus polling for
acknowledgements), with a folder implementation alongside ntfy for tests and
for the app running on the receiving machine.

### 9.4 Acknowledge and resend

Attachments expire after 3 hours, and the receiving desktop may be off for
longer. Since a bundle can always be rebuilt from its journey, the app keeps
no copy of what it sent, only the queue entry:

1. The app sends the bundle and marks the entry *sent*.
2. The receiver, on fetching it, publishes `got <bundle-id>` to the
   acknowledgement topic.
3. While running, the app polls the acknowledgement topic every 15 minutes
   and marks acknowledged entries done.
4. An entry still unacknowledged 4 hours after sending is rebuilt and sent
   again under the same bundle id. After 7 days of this it is abandoned, and
   the journey records that it never arrived.

The receiver ignores a bundle id it already has, so a resend after an
acknowledgement was missed (the app was off for more than the 12-hour message
cache) costs only bandwidth.

### 9.5 Receive

A small command-line program on the receiving desktop, `tod-journeys`:

- `tod-journeys init` generates an age identity (kept in its own directory,
  never shown again), two random topic names, and prints the **relay code**
  to paste into the app's settings.
- `tod-journeys pull` fetches whatever the inbox topic still holds, then keeps
  listening. For each attachment it downloads it, joins parts, decrypts it,
  decompresses it, files it as `<received-dir>/<bundle-id>.journey`, and
  publishes the acknowledgement. `--once` fetches and exits.
- `tod-journeys show <file>` prints a readable timeline (§10).

It depends on no GPUI and no database.

### 9.6 Queue

The queue is a table in the app's database, so it survives restarts: bundle
id, node (or project), the seq to send up to, reason, status (queued, sent,
acknowledged, abandoned), attempts, first queued, last sent. Each status
change is also recorded in the journey as a `Submission` record.

### 9.7 Before relying on it

Company web filters and data-loss-prevention tools often block or flag
anonymous file-sharing and notification services. **Send a test** from each
work computer, before turning sending on, is part of setup.

## 10. Analysis

A reader streams a journey or bundle, decompresses frame by frame, and decodes
records one at a time. From there an analysis tool loads what it needs into
SQLite or DuckDB, or hands it to an agent. The first tool answers the
questions that started this:

- For one bundle, a readable timeline: actions and what was presented,
  decisions, agent turns, data changes, with the time between them.
- Across bundles, recurring patterns: an action commonly followed by its
  reversal, the step users take right after Fix, time spent in each state,
  gate failures per criterion, how often a highlighted button was not the one
  clicked.

`node_context::render_work_history` and the `learn` state may later be
replaced by journey analysis done outside the app. That is out of scope here.

## 11. Out of scope

- Replacing `render_work_history` or the `learn` state.
- Screenshots on macOS and Linux.
- Any relay other than ntfy and the folder sink.
- Replaying a journey against the app.
