# Autonomous nodes in cloud sandboxes

Status: design (September 2026). Nothing here is built. It builds on the
sandbox transport in [blaxel-remote.md](blaxel-remote.md) and the relay in
[relay-protocol.md](relay-protocol.md). The work items are in
[autonomous-nodes-plan.md](autonomous-nodes-plan.md).

## Goal

The user accepts a node (a ticket), and an agent takes it through the whole
lifecycle on its own: implement, verify, review, fix, done. A human is
involved only when the agent needs a clarification or an action it cannot
take itself.

Each node runs in its own Blaxel sandbox, and the work goes on with the tod
app closed. Accept ten tickets, close the laptop, and each one keeps going
until it is done or needs a human, whichever comes first.

Constraints:

- **Nothing on the user's machine keeps the work going.** The loop, its
  state, its timers, and its webhooks all live in Blaxel.
- **Subscription billing.** Agents run as Claude Code (through
  `claude-code-acp`) on a Claude subscription, not on per-token API billing.
  That rules out Claude Managed Agents, which would otherwise run the loop and
  keep the transcripts for us. Transcripts are therefore ours to store.
- **A sandbox is awake only while it has work.** Whenever an agent waits (on
  CI, a PR review, a timer, a human, a usage limit), its sandbox goes to
  standby, and something outside it wakes it when the wait is over. The
  orchestrator sleeps too, whenever no request is in flight: nights,
  weekends, quiet days.
- **A crash never leaves a sandbox awake indefinitely.** Every hold on it
  expires or is cleared from outside.
- **The app stays fast.** Every local action is reflected in tens of
  milliseconds; nothing the user does waits on the orchestrator.

## The parts

```
 ┌──────────────────────────┐  tod-cli, wakes: HTTPS  ┌──────────────────────────┐
 │ agent sandbox (per node) │────────────────────────▶│ orchestrator sandbox     │◀── webhooks
 │  supervisor: the loop    │◀────────────────────────│  one per team            │    (GitHub, Linear)
 │  lifecycle sessions      │  wake, "context changed"│  one SQLite DB per user, │
 │  interactive sessions    │                         │  on a volume             │
 │  tod-relay               │                         └──────▲────────┬──────────┘
 └──────────▲───────────────┘                  changes, pulls│        │ "something changed"
            │ Blaxel schedules fire commands in it            │        ▼
 ┌──────────┴───────────────┐                         ┌──────┴───────────────────┐
 │ watchdog (Blaxel job,    │                         │ tod app (per user)       │
 │ hourly cron, tiny)       │                         │  its own DB is primary   │
 └──────────────────────────┘                         └──────────────────────────┘
          Agent Drive: transcripts (written by the supervisors)
```

- **Agent sandbox**, one per node. It runs the **supervisor**: the loop that
  today is the app's `ConversationDriver` and protocol loop (start a lifecycle
  session, check progress, run the gate, advance, pick the next protocol). The
  supervisor holds the sandbox awake while it works, and schedules its own
  wake before it lets the sandbox sleep. The user's interactive sessions run
  in the same sandbox (see Interactive sessions).
- **Orchestrator sandbox**, one for the whole team. It holds one SQLite
  database per user on a Blaxel volume, answers `tod-cli` for every agent
  sandbox, receives each user's local changes, routes webhooks, and tells
  running nodes when a change affects them. It serves discrete HTTP requests
  and holds no connections, so it sleeps between them.
- **Agent Drive** holds transcripts, and nothing else for now.
- **Blaxel sandbox schedules** are every timer: a wait's deadline, a poll's
  next check, a usage limit's reset.
- **Watchdog**: a separate Blaxel job on an hourly cron that only watches.

## Calling between sandboxes

Every call between sandboxes is one plain HTTPS request to the other
sandbox's **direct URL** on a port declared when it was created:
`https://<sandbox-url>/port/<n>/<path>`, the same route the app already uses
to reach `tod-relay`. No preview URL, no tunnel, no stream: a request, a
response, done. The platform requires the workspace's Blaxel token on it; an
agent sandbox never holds that token, because its **proxy adds it** (see
Credentials). A request to a sandbox in standby wakes it and is answered.

Measured between two sandboxes in `us-was-1` (September 2026), a Node HTTP
server on a declared port 3000 in one, `curl` through the proxy in the other:

- It works with a proxy rule naming the target host exactly, and with
  `*.bl.run`. Without the proxy (`--noproxy '*'`) the platform answers `401`.
- **Waking takes nothing extra:** a request to the target in standby
  (confirmed by the control plane) was answered in 0.18 s, end to end.
  Warm requests take about 0.1 s. The server's in-memory state survived the
  standby.

The orchestrator is not authenticated beyond that. A request carries the
user id and the node's id (`X-Tod-User`, `X-Tod-Node`), which pick the
database and the node; an agent sandbox only acts on its own node, by
convention, not enforcement. Only holders of the workspace token (through
their proxy) can reach the orchestrator at all.

## Where data lives

**Each user's app database is primary.** The orchestrator keeps a copy of it
per user, `/data/users/<user>/tod.db` on its volume, which the agents work
against. Volumes are block storage that outlive their sandbox, so SQLite on
one is safe and fast.

**Nothing that matters lives only in an agent sandbox.** A sandbox can be
lost at any time: it expires, is deleted, or breaks. (New sandboxes report a
seven-day `expiresIn`, a limit of our current tier that may go away on a
higher one.) Replacing it means creating a new sandbox, checking out the
node's branch, and starting the supervisor. The user's app does this, since
only it holds the user's tokens (see Credentials); a node whose sandbox is
lost while the app is closed waits for it. For that to lose nothing:

- **Code** is committed and pushed as the agent goes, not only at the end.
  The supervisor pushes at the end of every lifecycle session, before it
  sleeps.
- **The node's working state** (obligations, plan, verdicts, findings,
  conversation log, waits, questions) is in the user's database on the
  orchestrator, because every `tod-cli` write goes there.
- **The agent's session** is in the transcript mirror (see Transcripts). A
  new sandbox copies it back under `~/.claude/projects/` and resumes the
  session by id. If that fails, it starts a fresh session from a snapshot, as
  rotation already does.

The orchestrator's sandbox can expire too. Its replacement mounts the same
volume and carries on; nothing else about it is state. It keeps its name, so
its URL (and the agents' proxy rules, which name it) stay the same.

## `tod-cli` in an agent sandbox

`tod-cli` in an agent sandbox sends each command to the orchestrator as one
request: `POST /port/<n>/cli` with the arguments, the `TOD_*` environment
(the actor, the node, the user), and stdin; the response is stdout, stderr,
and the exit code. The orchestrator runs the real command against that
user's database. Every mutation still goes through `OutlineMutation`, so the
change set and reversal work unchanged. A failed request is retried with
backoff; a command that cannot reach the orchestrator fails like any other.

This replaces, in the cloud, the `cli_relay` route back to the app (a TCP
listener reached through the relay's `/tunnel`). The app is not involved.

## Sync with the app

The app writes to its own database first, as it does today; the user never
waits on the network. Changes then flow both ways as discrete requests.

**Up (the app's changes).** Each local change goes into an outbox in the
app's database. The app sends the outbox to the orchestrator in the
background (`POST /users/<user>/changes`), and deletes what was accepted. A
closed laptop just sends it later.

**Down (the agents' changes).** The orchestrator numbers every change it
commits to a user's database. The app keeps the last number it applied and
asks for everything after it (`GET /users/<user>/changes?after=<n>`) when it
starts and whenever it is told something changed (see Telling the app). The
app's own changes come back too, and applying them is a no-op.

What a change is: the row-level record the store already keeps for every
node-scoped table (`journey_changes`: node, table, row, operation), plus the
row's new contents. Every node-scoped table already needs that trigger, so
the feed is complete by construction, and applying it is an upsert or delete
by id. Mutation objects would be richer, but not every write is one.

**Conflicts** need the same row changed on both sides between two syncs.
Almost everything is scoped to a node, and a node in the cloud is almost
only written by its agent, so this is rare. When the orchestrator applies a
row from the app whose previous contents no longer match its copy, it
applies it (the app is primary) and records the conflict on the node, for the
user and for that node's supervisor.

## Changes that affect a running node

When the orchestrator applies a user's change, it checks it against that
user's active nodes. A change affects a node when it touches:

- the node itself: its obligations, plan steps, capabilities, content;
- something the node inherits: an ancestor's obligations or constraints;
- something its obligations reference, such as a reusable component.

Most changes affect nothing running; they are applied and that is all. For
an affected node, the orchestrator writes a "context changed" record on the
node and wakes its sandbox (`POST /port/<n>/poke` on the supervisor). What
the supervisor then does is a policy still to design:

- **At the next stopping point** (the default): let the current session end,
  then rebuild the context and continue.
- **Interrupt**: stop the session now and restart it with the new context,
  for changes that make the current work wrong.
- **Move back**: when the change means the node's state no longer holds
  (`tod_core::lifecycle_validity` already decides this for obligation and
  plan changes), the node returns to an earlier lifecycle state.

## Telling the app

The orchestrator must not hold a connection open to the app: that would keep
it awake whenever an app is open. Instead, when it commits something a user
should see (a question, a node done or blocked, any change to their data), it
sends that user **one "something changed" message** through a hosted
publish/subscribe service, and goes back to sleep. The message carries no
data, only the user and the latest change number.

- **App open:** the app holds its subscription to that service, not to the
  orchestrator. It gets the message within a second or two, then pulls the
  changes from the orchestrator (one request, waking it briefly).
- **App closed:** nobody receives it, which is fine: the app pulls on start.
  The same service can also notify the user's phone for questions and
  blocked nodes.

Candidates: ntfy (HTTP publish, SSE or WebSocket subscribe; hosted or
self-hosted), or a managed service such as Ably or Pusher. The topic per
user must not be guessable (or the service must authenticate subscribers),
since it reveals when a user's data changes.

## Interactive sessions

The user's interactive work in a node's sandbox (chat, the terminal
handoff, Zed) and the supervisor's lifecycle sessions share the sandbox and
nothing else. Each is its own Claude process with its own session: the
supervisor starts sessions per lifecycle step or transition, and an
interactive chat is a new session the user started. They cannot drive each
other's sessions.

Running in the same sandbox is deliberate: it costs no second sandbox, and
the interactive session sees the same worktree, including uncommitted
changes. The existing transport stays as it is for interactive use:
`tod-relay`, the agent bridge, and the relay's holds (an interactive session
is one more reason to hold the sandbox awake). The supervisor needs none of
the relay's streaming: it runs inside the sandbox, and reaches the
orchestrator and Blaxel with plain HTTPS.

Both work in one checkout, and when the supervisor commits and pushes, it
takes everything, the user's uncommitted edits included. The two are rarely
active at once, so this is kept simple on purpose.

## The supervisor and waiting

**An agent never waits inside a session.** When it needs to wait, it records
the wait through `tod-cli` and ends its turn, following the existing rule that
nothing is parsed from replies:

```
tod-cli wait --until <time>                  # a timer
tod-cli wait --event <source> <match>        # a webhook, e.g. github pr 123 checks
tod-cli wait --check <condition> --every 2m  # poll something that has no webhook
tod-cli ask <question>                       # a human (the existing question path)
```

The wait is a row in the user's database on the orchestrator. The supervisor
then:

1. Creates a Blaxel schedule on its own sandbox with the id `wait-<id>`: the
   deadline for an event wait (give up or check directly if the webhook never
   comes), the next check for a poll, the time for a timer. Its command is
   `tod-supervisor wake`, with `keepAlive: false`: the supervisor takes its own
   hold once it has decided there is work. (On the development account the
   orchestrator keeps the timer instead; see Development account.)
2. Releases its hold. The sandbox is in standby about 15 s later.

**A wake is a poke, never an instruction.** Whether it comes from a schedule,
a webhook, a user's answer, or a change to its context, the supervisor asks
the orchestrator what it is waiting on and whether that is satisfied. Then it
continues, or schedules the next check and goes back to sleep. Duplicate,
late, and spurious wakes are therefore harmless, and so is a restart in the
middle of a step.

When a wait is satisfied some other way (the webhook arrived before the
deadline), the supervisor deletes its schedule:
`DELETE /v0/sandboxes/<sandbox>/schedules/wait-<id>`. Choosing the id when
creating the schedule is what makes that a single call. A deleted or already
fired schedule is not an error; a stale one firing later is a harmless poke.

Polling backs off (for example 1, 2, 5, 15 minutes). Each check costs one
wake (about 0.2–0.7 s) plus the ~15 s before standby: at 4 GB that is
about $0.0007, so a node waiting all day costs cents.

## Usage limits

A Claude subscription has usage limits. With many nodes running, they will be
hit. When the agent reports that the limit is reached, the supervisor reads
the reset time from the message, records a wait `--until <reset>`, and
sleeps. Every node resumes on its own once the limit resets, with no user
involvement.

## Webhooks

Webhooks (GitHub, Linear) go to the orchestrator, not the agent sandboxes:
one public endpoint to register and verify signatures on, instead of a
listener in every sandbox. They arrive at a public preview URL on the
orchestrator, the one route into it that does not carry the workspace token;
the webhooks' signatures authenticate them instead.

Routing an event to its node:

- **By branch.** Each node works on its own branch. Pull requests, check
  suites, and workflow runs carry the branch name or head SHA, which
  identifies the user and the node.
- **By open waits.** Otherwise the orchestrator matches the event against the
  open waits in its databases.

It records the event on the node and pokes the node's sandbox. Waking a
sandbox and telling it to look is the same operation whether the message is
an event, a user's answer, or a changed context.

Webhooks can be lost. Every event wait also has a schedule (its deadline or
next direct check), so a lost webhook only delays the node.

## Holding the sandbox awake

**The hold is a `keepAlive` process** started through the sandbox's own
process API: while a process started with `keepAlive: true` runs, Blaxel does
not put the sandbox in standby, even with no connection open. This is what
`tod-relay` already does ([hold.rs](../../crates/tod-relay/src/hold.rs)): it
runs one `sleep` that way while any reason to stay awake holds, and kills it
when the last one ends. It is documented, it needs **no credentials** (the
process API at `127.0.0.1:8080` is unauthenticated from inside the sandbox;
nothing goes through the control plane), and the process has its own
`timeout`, so a hold whose owner dies ends by itself.

The rules around it:

- **The hold is short and renewed.** Its `timeout` is a lease, for example
  10 minutes, renewed (a new `keepAlive` process replacing the old one) only
  while the agent is making progress (output, tool calls). A supervisor or
  relay that dies or hangs stops renewing, and the sandbox can sleep within
  the lease. This replaces the relay's current 4-hour cap.
- **One owner.** Only `tod-relay` starts and kills hold processes; the
  supervisor, an interactive session, a terminal's foreground job, and an
  agent that owes an answer are reasons the relay holds for.
- **The watchdog is the backstop** (see Crash guards).

The orchestrator takes no hold: it is awake exactly while requests are in
flight.

### The alternative: Unikraft's counter file

Blaxel sandboxes run on Unikraft Cloud, which also lets the software inside
control standby through a counter file, `/uk/libukp/scale_to_zero_disable`
([Unikraft docs](https://unikraft.com/docs/guides/features/scaletozero/)):
while the count is above zero the sandbox does not go to standby. Writing
`+` or `-` changes it by one, `=N` sets it, and reading it returns `=N`.
Blaxel sets `BLAXEL_SCALE_FILE` to that path in every sandbox, but does not
document it. We do not use it; it is recorded here because it was measured.

Measured on a Blaxel sandbox (September 2026):

- With the count at 1 and no connection open, the sandbox stayed `RUNNING`
  for over three minutes, and a once-a-second clock inside it had no gap
  (92 ticks in 92 s). Without the hold it is in standby 15–40 s after the last
  request (the control plane's state lags a little).
- Setting `=0` put it in standby about 30 s later, by the control plane's state.
- A process that wrote `+` and was then killed left the count raised. The
  counter belongs to nobody: an increment whose owner crashes holds the
  sandbox awake indefinitely. Nothing resets it.
- No credentials are needed; it is a local file write that takes effect in
  milliseconds.

Nothing expires the count, which is why the `keepAlive` process, whose
`timeout` does, is the one we use.

## Transcripts

Claude Code writes each session to a JSONL file under `~/.claude/projects/`.
That directory stays on the sandbox's disk; it is not mounted from the drive,
and neither is `CLAUDE_CONFIG_DIR`, which also holds the subscription's
credentials.

The supervisor mirrors each session file to Agent Drive
(`users/<user>/nodes/<node>/transcripts/`) **as it is written**: it follows
the file and appends each new line to the copy. A user looking at a node sees
what the agent is doing now, not as of the last turn. If the network
filesystem cannot keep up with that, the fallback is batching (every few
seconds, and at the end of each turn); nothing else changes.

The app reads transcripts from the drive's S3 endpoint, which does not wake
anything. Finished sessions are compressed. Agent Drive is free during its
beta; when it is not, transcripts can move to cheaper object storage (or the
orchestrator's volume) without changing anything but where the supervisor
writes them.

## Crash guards

- **The hold expires.** See above; the supervisor's hold is a lease it must
  renew, so a supervisor that dies or hangs stops holding the sandbox.
- **A hung agent** (no output or tool activity for N minutes): the supervisor
  ends the session and retries. After K failures it asks the user.
- **A runaway loop**: each node has a budget (sessions, awake hours).
  Reaching it asks the user; it never just keeps going.
- **The watchdog.** Hourly, it lists the workspace's sandboxes through the
  control plane (which does not wake them). A sandbox that has been awake
  longer than its lease allows gets its hold cleared (the process API: kill
  any `keepAlive` process), and the node is flagged through the
  orchestrator. The watchdog does nothing else.
- **A lost sandbox is replaced, not repaired.** See Where data lives: code is
  pushed, state is on the orchestrator, and a new sandbox continues from them.

## Credentials

| Who | Needs | For |
|---|---|---|
| Agent sandbox | Blaxel token | its own schedules; calling the orchestrator |
| Orchestrator | Blaxel token | poking agent sandboxes |
| Watchdog | Blaxel API key | reading sandbox state, clearing holds |
| Agents | GitHub, Linear, … | pushing branches, opening PRs, updating tickets |
| Agents | Claude subscription | the agent itself |

Blaxel's options:

- **Service-account API keys** can be valid indefinitely, but give the
  service account's full workspace permissions; the docs describe no scoping
  to one sandbox or to read-only.
- **Environment variables** (`envs` at creation, or updated on a running
  sandbox for new processes) are visible to anything in the sandbox,
  including the agent.
- **Proxy routing with secrets injection** (public preview; set when the
  sandbox is created, in `spec.network.proxy`, and cannot be added later):
  all outbound traffic goes through Blaxel's proxy, which intercepts TLS and
  adds headers (or JSON body fields) per destination domain. Secrets are given
  with the rule and referenced as `{{SECRET:name}}`; the stored spec omits
  them, and they never enter the sandbox.

Measured with the proxy (September 2026):

- The sandbox gets `HTTP_PROXY`/`HTTPS_PROXY` (a local port),
  `SSL_CERT_FILE`, `CURL_CA_BUNDLE`, `REQUESTS_CA_BUNDLE`, and
  `NODE_EXTRA_CA_CERTS`. The secret values appear in no environment variable
  and no file.
- **The Blaxel API works through it.** With a rule adding the token for
  `api.blaxel.ai`, a sandbox holding no credentials read its own sandbox
  (200) and was authorized to create a schedule (refused only for the plan,
  not for the credentials).
- **Another sandbox's declared port works through it** (see Calling between
  sandboxes).
- curl got the injected header. **Node's `fetch` only goes through the proxy
  with `NODE_USE_ENV_PROXY=1`**; without it, it goes direct and gets nothing.
- **git needs the proxy's CA**: `http.sslCAInfo=$SSL_CERT_FILE` (bootstrap can
  set it system-wide). With it, `git ls-remote` over HTTPS works.
- `api.anthropic.com` is reachable through it.
- The first requests in the first second or so after creation got `407` from
  the proxy; it needs a moment to come up.
- **A destination that echoes request headers hands the secret back to the
  sandbox** (httpbin.org did). The rule is only as private as its
  destinations: name exact API hosts, never `*`.
- **GitHub and Linear work through it**, with the user's own GitHub token and
  Linear API key from tod's credential store, in a sandbox holding neither
  (neither value in its environment). All read-only; pushing was not tried,
  but uses the same header as the clone.

  | Rule (destination: header) | Check | Result |
  |---|---|---|
  | `api.github.com`: `Bearer <token>` | `curl /user`, a private repo | the user; `private: true`, 200 |
  | same | `gh api user`, `gh repo view` of a private repo, with `GH_TOKEN=placeholder` | both answered: the proxy replaces `gh`'s header |
  | `github.com`: `Basic base64(x-access-token:<token>)` | `git ls-remote`, `git clone` of a private repo | both worked |
  | none (git with `NO_PROXY=github.com`) | the same `ls-remote` | refused: no credentials |
  | `api.linear.app`: `<key>` | GraphQL `viewer`, `organization` | the user and the organization |

The plan:

- **GitHub, Linear, and similar APIs: proxy injection**, one routing rule per
  API host that adds the `Authorization` header, as measured above. `gh`
  gets a placeholder `GH_TOKEN` that the proxy overwrites.
- **Where the values come from.** The user already stores a GitHub token and
  a Linear API key in tod (`CredentialStore`: the OS keyring, else an encrypted
  file; set in the app or with `tod-cli secrets set`). When tod creates a
  node's sandbox, it reads them and passes them as the proxy rules' secrets;
  nothing is configured in Blaxel by hand. The sandbox therefore acts as the
  user who accepted the node: commits, PRs, and ticket updates are theirs. A
  token changed later reaches only sandboxes created after (until rotating a
  running sandbox's rule is verified). **User tokens never leave the user's
  machine except into a sandbox's proxy rules:** the orchestrator does not
  keep them. So a sandbox that expires while the app is closed is replaced
  only when the user's app next runs; until then its node waits (the
  orchestrator marks it, and the app replaces it on start). A team bot identity (a GitHub App, a Linear service
  account) would be a later change to where the secrets come from, nothing
  else.
- **Blaxel from an agent sandbox: proxy injection too**, a rule for
  `api.blaxel.ai` (its schedules) and one for the orchestrator's host by
  name (not `*.bl.run`, which would reach every sandbox in the workspace). A
  service-account key cannot be scoped to one sandbox, so this is a workspace
  credential the agent can exercise but never see.
- **The orchestrator** gets a rule for `*.bl.run` (it pokes every agent
  sandbox) the same way; it runs no agent, so an environment variable would
  also do.
- Holding a sandbox awake needs no credentials at all (see above).
- **The Claude subscription**: a long-lived token from `claude setup-token`,
  given to Claude Code as `CLAUDE_CODE_OAUTH_TOKEN`. Whether the proxy can
  inject it instead (a placeholder in the environment, the real token added
  for `api.anthropic.com`) is untested. Otherwise it is an environment variable
  of the agent's process.
- **The watchdog** holds a Blaxel key as a job secret.

## Development account

The account tod is developed on has no sandbox schedules, volumes, or
forking; the account it is deployed on has all three. Each gap has a
stand-in, chosen per account in `sandboxes.toml`, so the same code runs on
both and only the stand-ins are dev-only.

- **Schedules → the orchestrator's timer** (`scheduler = "orchestrator"`;
  the default, `"blaxel"`, is the design above). An agent sandbox still
  schedules its own wakes; the one call that creates or deletes a wake goes
  to the orchestrator (`POST /wakes` with the id, sandbox, and time;
  `DELETE /wakes/<id>`) instead of to Blaxel. The orchestrator keeps each
  wake as a row in the user's database, runs a timer in its own process, and
  pokes the sandbox when one is due, the same poke a webhook sends. A
  sandbox's timers stop in standby, so while any wake is pending the
  orchestrator holds itself awake with the same `keepAlive` lease the agent
  sandboxes use, and lets itself sleep when none is left; after a restart it
  reloads the pending wakes. Everything else is real: agent sandboxes sleep
  while they wait, waits and deadlines behave the same, and it runs with the
  app closed. The cost is an orchestrator that is awake while anything waits
  on a timer, which at development scale is a few dollars a month. The timer
  is deleted once development moves to an account with schedules.
- **Volumes → the orchestrator's own disk.** The per-user databases go in
  the same directory on the sandbox's ordinary filesystem. They survive
  standby but not the sandbox, so on this account a lost orchestrator loses
  them; that is acceptable for development data.
- **Forking → creating from the image.** A node's sandbox (and a replacement
  for one) is always created from the image and checks out the node's
  branch, which the design needs anyway. Forking would only make that
  faster.

## To verify

1. **Sandbox schedules.** Not available on our development account (403).
   On the deployment account, verify that a schedule wakes a sandbox in
   standby, that a sandbox can schedule itself, and that deleting a schedule
   by id works. Script: `.local/agent/scratchpad/sched-spike/spike.py`
   (git-ignored).
2. **Agent Drive.** Private preview, and only in `us-was-1`, which is also
   the region the team will use (tod's default region is now `us-was-1`).
   Check that a mount survives standby, and how it behaves with a line
   appended per transcript write.
3. **Volumes.** Available on the deployment account (not the development
   one). Check reattaching one to a replacement orchestrator sandbox.
4. **The proxy.** Claude Code's own traffic through it (subscription auth,
   streaming); whether a rule's secrets can be rotated on a running sandbox
   (the proxy cannot be added after creation); and existing sandboxes, which
   were created without it.
5. **The push service.** Pick one; measure publish-to-app latency.
6. **Subscription use.** One subscription driving many unattended agents: the
   limits, and that it is within the subscription's terms.
