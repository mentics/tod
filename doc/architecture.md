# tod: architecture overview

The one-page map of how tod is put together. It names the parts, the
boundaries between them, and the rules that hold across all of them, and
points at the document that goes deeper on each. It describes the code as
built; where a design document under `doc/` still carries an older status
line, the code and this page are current.

## What tod is

tod is a desktop application for directing coding agents. The user keeps a
tree of **nodes** (projects, features, tickets, tasks). A node can carry
**obligations** (what must be true when it is done), **content** (its spec
or design), and a **plan** (steps). A node with the Lifecycle capability is a
**task node**: an agent takes it through a fixed sequence of states, from
`proposed` to `done`, doing the work of each state, and the app checks each
transition deterministically. The user is asked only for what only they can
supply: an intent, a decision, an access grant, a human review.

Every agent interaction is a **conversation** on a focus (the project, a
node, an obligation, a plan step). The agent edits the tree through
`tod-cli`, every edit is recorded, and every edit can be **reversed** from
the conversation's own change set.

The work a node needs (a shell, an editor, an agent, git) can run on this
machine, in a git worktree, in a running dev container, or in a cloud
sandbox. A node whose files are in a cloud sandbox is started on a supervisor
process in that sandbox, which takes it through the lifecycle with the app
closed; any other node runs under `tod-agentd`, which also survives the app.
The task panel's runner menu starts, pauses and resumes either, and
**Migrate to…** moves a node between places.

## The concepts

| Term | Meaning | Where it lives |
|---|---|---|
| Node | One item of the outline tree, addressed by a stable slug or UUID. | `tod_store::outline` |
| Capability | A feature a node turns on: Spec, Lifecycle, Agent, Generator, Tags, Files, Ticket, Environment, LifecycleConfig, Groups. Most settings inherit down the tree. | `tod_store::outline::types::Capability` |
| Obligation | A requirement on a node, verified at the end with a recorded verdict. | `tod_store::outline`, `tod_store::verification` |
| Plan step | One step of a node's plan, with its own status (open, blocked, verified, failed). | `tod_store::outline` |
| Lifecycle state | One of `proposed, design, planning, ready, active, verifying, review, pr, pr_draft, pr_review, approved, merged, released, learn, done`, in that order. | `tod_core::task::model::LIFECYCLE_STATES` |
| Gate | The app's deterministic check for leaving a state. No agent evaluates one. | `tod_core::phase::settle_gate`, `tod_core::gate::derived` |
| Phase agent | The agent that does one state's work and certifies it (or marks it ready for an independent evaluator). | `tod_core::conversation::phase`, `doc/lifecycle/phase-agents.md` |
| Conversation | One agent session on a focus, with its transcript, recorded actions, and reversal. | `tod_core::conversation`, `tod_store::conversation` |
| Protocol | What kind of conversation it is: chat, implement, verify, review, fix, pr, phase, evaluate, visual design. Decides context, working directory, looping, and side pane. | `tod_core::conversation::protocol` |
| Autopilot | The loop that runs a task node's lifecycle without the user: run the phase, check the gate, advance, repeat. | `tod_core::autopilot` |
| Wait | A recorded reason a node is not working now (CI, a review, a timer, the user). An agent never waits in process; it records a wait and exits. | `tod_store::waits`, `tod_core::scheduler` |
| Who is next | The derived answer for every node: Working, You, Others, or Done. Every view reads it from one function. | `tod_core::next_actor`, `doc/lifecycle/next-actor.md` |
| Files location | How a node gets its files: workspace directory, worktree, dev container, or cloud sandbox. Configured once, inherited, instantiated per node. | `tod_store::fleet`, `doc/files-locations.md` |
| Data root | The one directory holding everything durable for one install: the SQLite database, settings, logs, the daemon's files. | `tod_store::paths` |

The glossary of user-facing terms is `doc/glossary.md`.

## The processes

tod is several executables that share one data root. The app is the user's
window onto the data; it is not the only thing that works on it.

```
                    ┌────────────────────────────────┐
   user ──────────▶ │ tod            (GPUI app)      │ reads SQLite directly (read-only)
                    └───────┬────────────────────────┘
                            │ loopback TCP, token in <data root>/daemon/
                            ▼
                    ┌────────────────────────────────┐
                    │ tod-agentd     (resident daemon)│ the ONE writer of the database;
                    │  one per data root, started by  │ timers; starts and stops runs;
                    │  tod or tod-cli, outlives both  │ pushes changes to subscribers
                    └───────┬────────────────────────┘
                            │ spawns, one per running node
                            ▼
                    ┌────────────────────────────────┐
                    │ tod-supervisor wake --node N   │ runs the autopilot for one node
                    │  exits at a wait, a question,  │ (same binary runs in a cloud
                    │  or done                       │ sandbox against a synced copy)
                    └───────┬────────────────────────┘
                            │ ACP (claude-code-acp, cursor-agent), or the in-process mock
                            ▼
                    ┌────────────────────────────────┐
                    │ the agent                      │ on the host, in a dev container,
                    │  shells out to tod-cli         │ or in a cloud sandbox
                    └───────┬────────────────────────┘
                            │ every mutation goes through the daemon
                            ▼
                        tod-cli  ──▶  tod-agentd
```

| Executable | Crate | Role |
|---|---|---|
| `tod` | `crates/tod` (launcher), `crates/tod-ui` (everything else) | The desktop app. Opens the database read-only and sends every write to the daemon. Installs `process/`, `media/`, `skills/` beside itself at build time. |
| `tod-agentd` | `crates/tod-agentd`, protocol and client in `crates/tod-agentd-client` | One per data root, started by `tod` or `tod-cli`, never by the OS. Holds the store's writer lock, serves mutations and subscriptions over loopback TCP with a per-process token, runs the local scheduler, and starts a `tod-supervisor` for each node it runs. A stale daemon is drained and restarted by a newer app, never the reverse. `doc/agentd.md`. |
| `tod-cli` | `crates/tod-cli` | The agent's only interface to the data. Each verb wraps an existing store mutation; the agent never sees SQL. Kept light so it starts fast. Syntax is documented for agents under `crates/tod/media/context/cli/`. |
| `tod-supervisor` | `crates/tod-supervisor` | Runs one node's autopilot for one wake. Locally it is spawned by the daemon; in the cloud it is woken in the node's sandbox, syncs a local copy of the user's database with the orchestrator, runs, pushes the branch, schedules the next wake, and exits. |
| `tod-orchestrator` | `crates/tod-orchestrator` | One server for the whole team in its own sandbox: a copy of each user's database, serving `tod-cli` to agent sandboxes, relaying changes between the app and supervisors, receiving webhooks. Deployed with `scripts/deploy.sh`. `doc/cloud-sandboxes/orchestrator.md`. |
| `tod-relay` | `crates/tod-relay` | The one server inside every cloud sandbox: commands, terminals, agents, and port forwards over WebSockets. Cross-built for Linux. `doc/cloud-sandboxes/relay-protocol.md`. |
| `tod-sandbox` | `crates/tod-sandbox-cli` over `crates/tod-sandbox` | The command line for sandboxes: create, exec, shell, agent, orchestrator deploy. |
| `tod-zed-shim` | `crates/tod-zed-shim` | The `ssh`/`scp`/`sftp` Zed runs, routing `<name>.tod` and `<container>.docker.tod` hosts to sandboxes and dev containers. |
| `tod-journeys` | `crates/tod-journeys` | The standalone receiver that decrypts and analyzes problem journeys users send. `doc/journeys/spec.md`. |

Every executable reports failures through `crates/tod-problems`: one JSON
line per failure in `<data root>/problems/<process>.jsonl`, tailed by the
app into its log panel, with errors raised as toasts. Failures of the
cloud's executables come back through the orchestrator. `doc/problems.md`.

## The crates, and the direction of dependencies

Policy depends on transport, never the reverse. Each crate depends only on
the ones below it.

```
 tod ─────────────────────────────────────────────────────────────────┐
  └─ tod-ui          GPUI views, app shell, conversation view, unified  │
      │              workbench, dev-only agent control socket          │
      ├─ tod-agentd  the daemon (runs, scheduler, server)              │
      └─ tod-core    policy: conversation driver and protocols, phase  │
          │          and gate rules, context assembly, autopilot,      │
          │          next-actor, cloud sync, settings and paths        │
          ├─ tod-store         SQLite persistence: outline, fleet,     │
          │   │                conversations, waits, credentials       │
          │   ├─ tod-agent     agent transport (ACP, Claude, Cursor,   │  leaf
          │   ├─ tod-journey   mock); dev containers; sandboxes        │  leaf
          │   └─ tod-sandbox   Blaxel API, relay client, provisioning  │  leaf
          ├─ tod-agentd-client daemon protocol and client              │
          ├─ tod-orchestrator  server, and the types its clients share │
          ├─ tod-integration   external data sources (Linear, GitHub)  │  leaf
          └─ tod-problems      the failure channel                     │  leaf
 tod-cli ──▶ tod-core, tod-store, tod-sandbox, tod-journey, tod-problems
 tod-supervisor ──▶ tod-core, tod-store, tod-agent, tod-orchestrator
 tod-relay ──▶ tod-problems          tod-zed-shim ──▶ tod-sandbox
 tod-journeys ──▶ tod-journey, tod-integration
```

What each layer may know:

- **`tod-agent`** holds conversations and sessions with agents and knows
  nothing about paths, settings, process docs, or persistence. It is told
  what to say and reports back. Anything a provider needs is passed in.
- **`tod-store`** is durable state and the mutation queue every writer uses
  (the GUI, `tod-cli`, the daemon). Invariants live here, so no caller can
  bypass them.
- **`tod-core`** decides *when* and *what* to persist, and *what to say* to
  an agent. It is shared by the app, `tod-cli`, the daemon, and the
  supervisor, which is what lets one node run in four places with one code
  path.
- **`tod-ui`** renders and routes input. Nothing that runs git, Docker, or
  `tod-cli` runs on its main thread.

New code goes in the lowest layer that can hold it. The crate-by-crate
detail, with the module names, is in `.claude/CLAUDE.md`.

## Where the data is

Everything durable lives flat under one **data root**, chosen by
`--data-root`, then `TOD_DATA_ROOT`, then the user's `install.toml`
(never written by code or tests). Its SQLite database (`tod.db`) holds the
outline, fleet, conversations, waits, and the rest; `tod.yml` holds
settings; the daemon's lock, port, token, and log are in `daemon/`;
failures in `problems/`; journeys, credentials, worktrees, shells, and the
agent's own files each in a directory of their own.

The database has **one writer**: the daemon. The app and `tod-cli` read on
their own read-only connections (WAL) and send writes as requests. Changes
are pushed to subscribers, so views update without polling and idle costs
nothing.

Machine-local tables (files locations, worktrees, sandboxes) are never
synced. The user's database is primary; in the cloud the orchestrator holds
a copy, and each supervisor a copy of that, synced around every step.

Bundled, read-only assets resolve separately from the data root and hold no
user data: `assets/process/` (role docs, agent definitions),
`crates/tod/media/context/` (prompt fragments and `tod-cli` docs), and
`assets/skills/` (skills agents read with `tod-cli skills`). Each is copied
beside the executable at build time and found through an env override, the
executable's directory, or a walk up from the working directory.

## How an agent is driven

**Context.** Every surface's prompt is assembled by one function,
`tod_core::context_recipes::build_message`, from an explicit ordered list of
static fragments (`stance/` exactly one, then `domain/`, `cli/`,
`surface/`) and dynamic blocks (the data root, the focus with ids *and*
text, the configured skills, the usual actions). There is no implicit
ancestor chain and no per-surface builder. Tests enforce that `tod-cli`
syntax appears only under `cli/`, and that every fragment a recipe names
exists. `doc/agent-context-map.md` maps every surface to what it needs.

**Sessions.** A conversation holds one agent session at a time. The first
message sends the opening context and the message as one turn; every later
message sends only the delta (the user's own edits and reversals, and items
changed elsewhere) plus the message. When the session outgrows its budget
or cannot be resumed, the driver rotates to a fresh one seeded with a
snapshot. Replies are never parsed: anything the app needs, the agent
records through `tod-cli` as it works.

**Recording and reversal.** An agent writes as an actor
(`conversation:<uuid>`), and each node, obligation, plan step, and
capability mutation is recorded with its before and after state in the same
transaction. The change set is net per item, reversal applies inverse
mutations, and conflicts and dependents are reported for confirmation.
Lifecycle state is never set by an agent; only the lifecycle process moves a
node.

**Protocols.** The conversation's protocol decides the recipe, the working
directory, what "done" means, whether the app loops it without the user,
and which side pane the view shows. Implement loops over open plan steps
until the plan is done and a recorded test run is green; Verify exercises
every obligation and records verdicts; Review records findings and Fix
resolves them; the PR protocol opens the draft and the autopilot's
babysitter drives it until only the human review is missing.

**Phases and gates.** In the judgment states (`proposed`, `design`,
`planning`, `merged`, `released`, `learn`) a phase agent does the work and
certifies the phase; the certificate holds a digest of what it covered, so
any later change makes it stale. With independent evaluation on (the
default) a separate evaluator session, which may not edit the node,
certifies or rejects with fixes. The gate is then an app check over derived
criteria and the certificate. A node whose state stops holding after the
fact (its obligations or plan changed since it entered `ready`) is named by
`tod_core::lifecycle_validity`, and the user confirms the move back.

**Running unattended.** The autopilot takes a node through the whole
lifecycle with no built-in stopping points. A permission an agent asks for
becomes a pending decision on the node; answering it records a standing
grant and reopens the step. Anything that waits (CI, a bot review, a human
approval) is a recorded wait, not a held process: the supervisor exits and
a scheduler (the daemon locally, Blaxel's schedule or the orchestrator's
timer in the cloud) pokes it when the wait is over. A late, duplicate, or
stale wake is harmless because a woken supervisor re-reads the waits and
decides for itself.

## Where the work runs

A node's **Files** capability picks a location, inherited down the tree,
and the node gets its own instance of it the first time something needs its
files: a worktree on its own branch, a directory in a running dev
container, or a cloud sandbox of its own. `tod_store::fleet::Workdir` (host,
container, or sandbox) is the one directory type every git operation and
launch takes, so the rest of the code does not branch on where it runs.

| Place | Git runs | Agent runs | `tod-cli` reaches the daemon by |
|---|---|---|---|
| This machine, a worktree | here | here | loopback TCP |
| Dev container, repository inside | `docker exec` | `docker exec` | a bash shim relaying over TCP to the app's listener |
| Dev container, repository mounted | here | `docker exec` | the same relay |
| Cloud sandbox | `tod-sandbox exec` through the relay | `tod-sandbox agent` bridged to the relay | the relay's tunnel back to this machine, or the orchestrator when it runs under a supervisor |

Nothing starts or builds a container; tod only uses a running one.

A node has **one cloud sandbox**, and its Files location row is the one
record of it (`node_files_locations.sandbox`; `cloud_nodes` keeps only the
supervisor's and orchestrator's state and a copy of the name for the
orchestrator). `provision::make_sandbox` is the only creator, whether the
sandbox serves a chat, a shell, or the supervisor: it is named
`tod-<slug>-<6 hex>`, forked from the repository's **golden** (one per
repository, carrying the toolchain, the checkout and tod's files;
`tod-sandbox golden`, refreshed when this build is newer) or created from an
image, and then given the node's environment, proxy rules, checkout and a copy
of tod's files (a fork's proxy update wipes what the golden had installed). A
sandbox an older build recorded only in `cloud_nodes` is adopted. At `done`
the daemon deletes the node's sandbox (with a checkpoint) unless the lifecycle
config's **Delete sandbox** is off, and the runner menu's Delete sandbox does
it on demand. `doc/runner-menu/design.md`.

A sandbox is awake only while it has work, and every hold on it expires or is
cleared by the hourly watchdog, so a crash never leaves one awake.
`doc/files-locations.md`, `doc/cloud-sandboxes/`.

## The user interface

`tod-ui` is GPUI with `gpui-component`. The **unified view**
(`unified/`) is the workbench: column one is always the node tree, and
panels (task, details, obligations, plan, findings, settings, transcript)
open in columns by one placement rule. The **conversation view**
(`conversation/`) is where every agent chat runs, with the transcript, the
change set, and a side pane chosen by the protocol; Ctrl+J opens it on the
current selection. The **lifecycle panel** and the conversation view share
one `LifecycleController`, so a gate check started in either shows in both.
`views/` holds the rest: transcripts, locations, log panel, editors.

Conventions that every view follows: styles come from
`doc/ui-style-guide.yaml` and are implemented once each; dynamic text is
selectable; list-shaped views converge on one item-list component
(`doc/ui/item-list.md`); text fields never trap keyboard navigation
(navigation mode and edit mode); multi-column surfaces move focus with
Left/Right or Ctrl+Left/Right. Every user-facing lifecycle action records a
journey event with a snapshot of the buttons that were on screen.

## Rules that hold everywhere

- **Errors are never hidden.** No silent fallback, no `let _ =`, no empty
  default. If the configured action cannot be taken, the error names what
  failed and why. A fallback exists only when the user configured it.
- **One writer.** The daemon owns the database; every other process reads
  directly and writes through it.
- **Gates are app checks.** No agent evaluates a gate. Judgment happens
  before, in a phase, and is certified over a digest.
- **An agent never waits.** It records a wait and the process exits; a wake
  is only a poke.
- **Replies are never parsed.** Structured state the agent produces, it
  records through `tod-cli`.
- **Every change is reversible from its conversation.** Reversal is built
  from the recorded action log, never from an undo history.
- **Significant changes show their impact first.** Editing or removing a
  files location, disabling a capability, deleting a node: the app shows
  what would be affected and confirms; `tod-cli` refuses instead.
- **Nothing on the UI thread runs git, Docker, or `tod-cli`.**
- **Every executable reports its failures to the app.**

## Where to read next

| Topic | Document |
|---|---|
| Crate-by-crate detail, commands, conventions for contributors and agents | `.claude/CLAUDE.md` |
| Build, install, control socket, project layout | `README.md` |
| The daemon: one writer, scheduler, runs | `doc/agentd.md` |
| Phase agents, certificates, gates | `doc/lifecycle/phase-agents.md` |
| Who is next | `doc/lifecycle/next-actor.md` |
| PR babysitting | `doc/lifecycle/pr-readiness.md` |
| Plan step phases, obligations, repo profiles | `doc/lifecycle/` |
| Agent prompts, surface by surface | `doc/agent-context-map.md` |
| Files locations and worktrees | `doc/files-locations.md` |
| Cloud sandboxes, the orchestrator, autonomous nodes, the relay | `doc/cloud-sandboxes/` |
| Unified view, task panel, item list, visual design browser | `doc/ui/` |
| Journeys (problem reports) | `doc/journeys/spec.md` |
| Problems channel | `doc/problems.md` |
| Live verification of the whole process | `doc/live-verification.md` |
| User-facing terms | `doc/glossary.md` |
