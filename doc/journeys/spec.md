# Journeys

A record of what happened, kept as it happens, so that when something goes
wrong, or when a node simply finishes, the whole story can be packaged up and
analyzed in development.

## 1. The problem

When the app does not do what the user expected (an agent stops short, a gate
check fails for no visible reason, the lifecycle stalls) there is nothing to
hand to development but a description from memory. `tod.log` carries little.
The conversation tables and agent transcripts carry a lot, but not the part
that usually explains the problem: **what the user did, and what the app was
showing them when they did it.**

A real case: a node in `verifying` failed its gate check with a finding. The
user clicked Fix, and the agent fixed it. The lifecycle panel then highlighted
"Rerun gate check", so the user clicked that. The right step was Verify:
after a Fix, verification has to run again to update the verdicts. The
sequence *Fix → gate check, with gate check highlighted* diagnoses it at a
glance. Without that sequence, nobody can tell whether the user made a mistake
or the app pointed the wrong way.

The same record serves a second purpose. Even when nothing goes wrong, a
node's finished journey shows where the user hesitated, backtracked, or
clicked the wrong thing, which is what we need to make the journey shorter.

## 2. The idea

Two kinds of journey, stored differently because they are used differently:

| | Node journey | App journey |
|---|---|---|
| Covers | Everything that touched one node, over its whole life | Second-by-second navigation of the whole app |
| Storage | Append-only file per node, on disk | In-memory ring buffer |
| Kept | For the life of the node | The last few minutes |
| Answers | "Why did this node's lifecycle go this way?" | "How could the user have got there faster?" |

A **bundle** is a self-contained snapshot built from those: one or more
journeys, with everything they reference pulled in from the database, plus a
settings snapshot. A bundle is queued when the user reports a problem, and
automatically at lifecycle milestones. It is built only to be sent, encrypted
so that only the receiver can read it, and deleted once delivered. Sending can
be automatic (opt-in).

## 3. Node journeys

### 3.1 What is recorded

Every event that concerns the node, in order:

- **User actions**: Implement, Verify, Review, Fix, gate check, Advance,
  Waive, Move back, force advance, revert, edits to obligations and plan
  steps, sends in a conversation focused on the node. Each records its
  *source* (key, click, menu) and the view it came from.
- **What the app presented** when the action was taken: the highlighted or
  primary step, which steps were enabled, whether the validity callout was
  showing and what it said. Every user action carries this snapshot. It is
  what separates a user mistake from the app pointing the wrong way.
- **App decisions, with reasons**: lifecycle transitions (from, to, by whom),
  gate check results per criterion (pass/fail/waived, detail),
  `lifecycle_validity` rulings, and protocol loop decisions ("continuing: step
  3 open", "stopping: no green test run", "handing back: step 5 blocked").
- **Agent activity**: turn started and ended (with outcome), and session
  rotation. The agent's `tod-cli` calls are not recorded separately: they are
  already in its transcript, which the journey references.
- **Data changes as the app sees them**: when the store reports a change
  concerning the node (an obligation, plan step, verdict, finding, or test
  run, whether written by the agent through `tod-cli` or by the user), a
  reference to the changed row. The rows themselves already carry their
  history (`conversation_actions`, verdict history). The journey adds only
  *when* the change happened relative to the user's clicks, which the
  transcript cannot show.

  Every kind of node-scoped change is recorded, not a chosen subset: a
  reference costs a few dozen bytes, and which changes matter is exactly what
  is not known in advance (the Fix → gate check case turned on a finding's
  status). One `DataChanged` record is written per store change event, listing
  every row it touched, so a burst of agent writes is one record, not dozens.
  Text content is recorded when it is saved, never per keystroke.
- **Milestones and reports** (sections 6 and 5).

Conversations already have a focus. Events from a conversation focused on a
node, or on one of its obligations or plan steps, go into that node's
journey. Project-focus conversations go into the project's journey.

### 3.2 References, not copies

Large data that already lives in the database is **referenced**, not copied:
conversation turns, the change set, the prompts as sent, verdict history,
review findings. An agent session's own transcript (Claude/Cursor JSONL) is
referenced by session id and path. A reference names what it points to
precisely enough to extract later (e.g. conversation id plus turn range).

This keeps journey files small, and it means a bundle never needs the database
itself: the exporter resolves the references a journey actually makes (§5.3).

Full transcripts, including the prompts exactly as sent, are stored in the
database. If any part of a transcript turns out not to be, that is a bug to
fix, not something the journey works around.

### 3.3 Files

```
<data_root>/journeys/nodes/<node-uuid>.journey.zst    compacted frames
<data_root>/journeys/nodes/<node-uuid>.journey.tail   recent, uncompressed
<data_root>/journeys/projects/<project-id>.journey.zst / .tail
```

The UUID is the node's stable UUID (not its slug, which can change).

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

JSON appears only where JSON *is* the data (e.g. an agent's raw tool-call
payload), carried as an opaque payload with its media type.

```rust
struct Record {
    seq: u64,              // per file, monotonic; see compaction
    at: i64,               // unix microseconds
    actor: Actor,          // User | App | Agent { session }
    node: Option<Uuid>,
    event: Event,
}

enum Event {
    UserAction { action, source, view, presented: Presented },
    Transition { from, to },
    GateResult { criteria: Vec<CriterionResult> },
    Validity { holds_through, reasons },
    ProtocolDecision { protocol, decision, reason },
    AgentTurn { phase, conversation, turn, outcome },
    SessionRotated { conversation, reason },
    DataChanged { what: Reference },
    Milestone { state },
    Report { note, app_journey, screenshot },
    Settings { snapshot },
    Nav { from_view, to_view, focus },          // app journey
    Blob { mime, bytes },                        // screenshot, raw payloads
}
```

This is the shape, not the final type. Adding a variant or field is always
allowed. Renaming or reusing one is not.

### 4.2 Tail plus compaction

Compressing each event on its own gains almost nothing, and holding events in
memory to compress in batches loses them in a crash, which is exactly when
they are wanted. So:

1. Each event is appended **uncompressed** to `<id>.journey.tail` as it
   happens.
2. When the tail passes ~64 KB, at a milestone, or at startup (recovering a
   previous run), the tail is compressed into **one zstd frame** and appended
   to `<id>.journey.zst`. The tail is then truncated.
3. zstd decodes concatenated frames as one stream, so the `.zst` file is just
   frames appended over time.

A crash between steps 2 and 3 duplicates records. Readers drop any record
whose `seq` is not greater than the last one seen. A torn final record in the
tail fails to decode and is dropped.

### 4.3 Writer

Only the app writes journeys, through one journey-writer thread fed by a
channel. Nothing on the UI thread waits on a journey. `tod-cli` never writes
one: its calls are in the agent's transcript, and the changes it makes reach
the journey as `DataChanged` when the app sees them (§3.1).

### 4.4 Retention

Journeys are kept for a long time. A setting caps their total size on disk
(default 1 GB). When the cap is reached, the journey whose **last update** is oldest is
deleted first, so a long-running node that is still active survives a
finished one that is newer. Creation and last-update times are kept for each
journey.

## 5. Reporting a problem

### 5.1 Entry points

- A `ReportProblem` action bound app-wide (key to be chosen, shown in the
  title bar next to the Ctrl+J badge). It is scoped the way `OpenAgentChat`
  is: a view that knows its selection handles it and supplies the node or
  conversation. The shell root is the fallback (the task tree's selection,
  else the project).
- Inline where things go wrong: error turns, a failed gate check, the "Move
  back" callout, and the row menu of an agent turn ("this turn was wrong").

### 5.2 The dialog

One field, "What did you expect?", which may be left empty. Ctrl+Enter
submits. A `Report` event is appended to the node's journey, carrying the
note, a copy of the app journey ring buffer, and a screenshot of the window,
all captured at that moment because none of them can be rebuilt later. The
report is queued for submission if sending is on (§9.1), and a toast
confirms it.

### 5.3 The bundle

A bundle is a copy of data, so it exists only to be sent: it is built at
submission time, delivered, and deleted. What is kept is the journey and the
queue entry that says "send this node's journey up to record *seq*". Nothing
under `journeys/` holds bundles at rest.

A bundle is itself a journey file (same format, same reader), holding in
order:

1. A manifest: app version, git sha, `CLI_BUILD_STAMP`, schema version, OS,
   the reason (report or milestone), and the user's note.
2. A settings snapshot (§7).
3. The node's journey up to the queued record (for a report, that includes
   its ring buffer and screenshot).
4. The data it references, resolved from the database: conversation turns,
   prompts, change sets, verdicts, findings, agent session transcripts. A
   reference that no longer resolves is recorded as missing, not skipped.

## 6. Milestone bundles

A `Milestone` event is recorded, and a submission queued, when a node enters
certain lifecycle states. During the current phase of rapid development the
list is broad, to get feedback early:

`active`, `verifying`, `review`, `approved`, `done`

A later bundle contains everything an earlier one did, so the early ones are
redundant by design. They exist for early feedback. The list is a setting and
will be trimmed once the lifecycle settles.

A milestone bundle has no screenshot and no app ring buffer. Those describe
the moment of a report, not the node's progress.

A queued milestone that has not yet been sent (the relay was unreachable)
and has since been overtaken by a later one for the same node is dropped from
the queue: the later bundle contains it.

## 7. Settings

Every bundle carries a snapshot of the settings: all of them, plus what was
resolved from them: agent platform, model, and effort; data root, process
root, and media root; the `tod-cli` path and build stamp; the dev container
configuration. Settings drive a large share of errors (wrong path, wrong
model), so the snapshot is complete.

Settings hold no secrets today: credentials are requested lazily and kept in
the OS keyring, outside settings. If a secret is ever added to settings, it
must be marked as such and left out of the snapshot.

Settings *changes* are recorded as events in the app journey.

## 8. App journey

An in-memory ring buffer (in the order of the last 5,000 events or 30
minutes) of view and focus changes and dispatched actions, each with its
source and timestamp. The time between events matters: a long gap before a
click is someone looking for something. It is never written to disk on its
own. A report copies it into the node's journey (§5.2).

## 9. Submission

Bundles are only useful if they reach development without effort, and only
acceptable if the user decides what leaves the machine. Submission runs in the
background: build the bundle, seal it, deliver it, and record in the journey
that it was sent.

### 9.1 Settings

Two settings, in their own section of the settings view:

- **Send journeys to development**: off by default. While it is off, reports
  and milestones are still recorded in the node's journey, but nothing is
  queued and nothing leaves the machine. While it is on, each report and
  milestone is sent as soon as it is queued.
- **Include transcripts**: off by default, and only enabled while sending is
  on. Transcripts are where sensitive data is most likely to be: what the
  user typed, what the agent read and wrote, and the prompts it was given.
  With this off, a bundle still carries the journey itself (actions, what
  was presented, transitions, gate results, protocol decisions), the
  settings, and the user's report note, and every reference into a
  transcript is kept as its id, size, and time only. The screenshot is left
  out too, since it can show a transcript.

Next to the first setting, a prominent warning (the style guide's warning
callout, not a caption):

> **Journeys leave this computer.** They are encrypted so that only the
> receiving computer can read them, but they are delivered to a computer
> that is not managed by your employer. If this is a work computer, make sure
> your employer's policy allows it before turning this on. Transcripts are
> left out unless you include them below.

The same warning is repeated in shorter form beside **Include transcripts**
when it is turned on.

The recipient's public key and the relay topic (§9.3) are settings too,
filled in once. Neither is a secret: the key only encrypts, and the topic
exposes only ciphertext.

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
  analysis tool given the private key.

### 9.3 Deliver: ntfy.sh as a relay

The app often runs on work computers, where only ordinary outbound HTTPS can
be counted on: no mesh networks, no inbound connections. So delivery is a
**relay**: the app pushes a sealed bundle to a holding place, and the
receiving desktop is notified, pulls it down, and acknowledges it.

The relay is [ntfy.sh](https://ntfy.sh), a free publish/subscribe service:

- **Push**: the app sends each sealed bundle as a file attachment to the
  inbox topic, with one HTTP PUT (`X-Filename: <bundle-id>.journey.age`).
- **Topics**: two long random names generated when the relay is set up, an
  inbox and an acknowledgement topic. Without an account, knowing a topic's
  name is what grants access to it, and all it grants is ciphertext.
- **Limits** of the public server: 15 MB per attachment, attachments expire
  after 3 hours, and a per-visitor storage cap. A sealed bundle larger than
  the limit is split into parts (`<bundle-id>.<n>-of-<m>.journey.age`) that
  the receiver joins.
- **Self-hosting**: ntfy is open source and a single binary. If the public
  server stops being suitable, the topic URL's host changes and nothing else
  does.

Delivery sits behind one trait (`put(name, bytes)`), with a folder sink
alongside ntfy for tests and for the app running on the receiving machine.

### 9.4 Acknowledge and resend

Attachments expire after 3 hours, and the receiving desktop may be off for
longer. Since a bundle can always be rebuilt from its journey, the app keeps
no copy of what it sent, only the queue entry:

1. The app sends the bundle and marks the entry *sent*.
2. The receiver, on fetching it, publishes `got <bundle-id>` to the
   acknowledgement topic.
3. The app polls the acknowledgement topic now and then (`?poll=1&since=…`)
   and drops acknowledged entries.
4. An entry still unacknowledged a few hours after sending is rebuilt and
   sent again, up to a limit of days, after which it is dropped and the
   journey records that it never arrived.

### 9.5 Receive

A small command on the receiving desktop (a `tod-journeys pull` binary, or a
mode of the analysis tool) subscribes to the inbox topic, downloads each
attachment as it arrives, joins parts, decrypts with the private key, files
the bundle locally for analysis, and publishes the acknowledgement. When it
starts after being off, it first fetches whatever the topic still holds.

### 9.6 Before relying on it

Company web filters and data-loss-prevention tools often block or flag
anonymous file-sharing and notification services. A test send from each work
computer, before turning sending on, is part of setup.

## 10. Analysis

A reader streams a journey or bundle, decompresses frame by frame, and decodes
records one at a time. From there an analysis tool loads what it needs into
SQLite or DuckDB, or hands it to an agent. The first tool should answer the
questions that started this:

- For one bundle, a readable timeline: actions, what was presented,
  decisions, agent turns.
- Across bundles, recurring patterns: an action commonly followed by its
  reversal, the step users take right after Fix, time spent in each state,
  gate failures per criterion.

`node_context::render_work_history` and the `learn` state may later be
replaced by journey analysis done outside the app. That is out of scope here.

## 11. Open questions

- **Whether store change events already say which rows they touched**, or
  need to, for `DataChanged` (§3.1).

## 12. Order of work

1. The journey writer (tail, compaction, CBOR records, retention cap) and a
   streaming reader, in `tod-store`, with tests.
2. Node journey events: lifecycle, gate checks, protocol decisions, agent
   turns, data changes, and the *presented* snapshot on each user action.
3. The app journey ring buffer and the settings snapshot.
4. `ReportProblem`: action, dialog, the submission queue, and the exporter
   that resolves references into a bundle.
5. Milestone bundles.
6. Submission: the two settings and the warning, sealing with age, the ntfy
   relay with acknowledge-and-resend, and the receiver's pull command.
7. The first analysis tool.
