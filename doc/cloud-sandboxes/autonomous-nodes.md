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

**Except `pr` and `secrets`, which run in the sandbox.** GitHub is reached
through the sandbox's proxy, which adds the user's token (see Credentials);
the orchestrator holds no GitHub token and could not. And `secrets run` must
start its command where the agent is. So the shim
(`cli_relay::HTTP_SHIM_SCRIPT`, `LOCAL_NOUNS`) runs those two nouns with the
real Linux `tod-cli`, installed beside it at `/opt/tod/tod-cli-local`
(`tod_sandbox::node::LOCAL_CLI_PATH`), against the supervisor's copy of the
database (`/var/lib/tod-supervisor/<node>`), whatever `--data-root` says.
What they write (`pr open`'s record, `mergeable`, `blocked`) goes into the
copy, through its mutation socket while the supervisor runs, and the
supervisor pushes it to the orchestrator after the step, as it does its own
writes; the gate checks that read it run in the supervisor against the same
copy. One mechanism, no GitHub logic anywhere but `tod_store::github`.
Before the supervisor has made its copy (the first seconds of a new
sandbox), those nouns fail and say so.

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

1. Creates a Blaxel schedule on its own sandbox for the soonest pending
   wait: the deadline for an event wait (give up or check directly if the
   webhook never comes), the next check for a poll, the time for a timer.
   It is a one-shot `at` schedule whose process is named `wait-<id>`
   (`tod_core::scheduler::BlaxelScheduler`). Its command pokes the relay on
   loopback, which starts the supervisor with the environment only it gets,
   or signals the one running, exactly as a poke from outside does; should
   the relay not be running, it starts the supervisor directly. The call
   goes to `api.blaxel.ai` with a placeholder token, which the sandbox's
   proxy replaces. The supervisor takes its own hold as soon as it
   starts, before it syncs its copy of the database. (With `scheduler = "orchestrator"` the orchestrator keeps
   the timer instead; see Development account.)
2. Releases its hold. The sandbox is in standby about 15 s later.

**A wake is a poke, never an instruction.** Whether it comes from a schedule,
a webhook, a user's answer, or a change to its context, the supervisor asks
the orchestrator what it is waiting on and whether that is satisfied. Then it
continues, or schedules the next check and goes back to sleep. Duplicate,
late, and spurious wakes are therefore harmless, and so is a restart in the
middle of a step.

Blaxel assigns a schedule's id (`schedule-0`, `schedule-1`, … reused once
free), so a wake is found by its process name: cancelling lists the
sandbox's schedules and deletes those named `wait-<id>`, and scheduling
deletes any of that name first, so rescheduling replaces it. The supervisor
keeps the wait it last scheduled in its state directory (`scheduled-wake`):
when a sooner wait takes over, or it wakes to work (the wait was satisfied
some other way, such as a webhook before the deadline), it cancels that
one. A wait the user cancels, satisfies, or reschedules in the app pokes
the node (the orchestrator's `wait_changes`), so it reconciles its wake
then, not at the old time. A deleted or already fired schedule is not an error (one-shot
schedules delete themselves once they have run); a stale one firing later
is a harmless poke.

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
- **The user's answer takes effect.** These questions, and the watchdog's
  flag, are decisions marked as stop questions (`tod_core::stop_questions`);
  an answer syncs to the orchestrator, which pokes the node. "Keep going"
  resets the failure count or grants another budget of the same size;
  "Leave it stopped" / "Leave it asleep" keeps the node stopped with no
  wake scheduled until the user answers again. See `orchestrator.md`.
- **The watchdog.** Hourly, it lists the workspace's node sandboxes through
  the control plane and asks each one's relay what holds it. It cannot
  skip the ones in standby: the control plane reports a sandbox held awake
  by a `keepAlive` process as `STANDBY` (its state follows proxied traffic,
  not the VM), so asking wakes a sleeping one for that one request. A
  sandbox that has been awake longer than its lease allows gets its holds
  cleared (the relay's `release-all`, which kills its `keepAlive`
  process), and the node is flagged through the orchestrator. The watchdog
  does nothing else. See `orchestrator.md`.
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
- **tod's own GitHub calls go through the proxy too.** A node's sandbox
  created with a GitHub rule has `TOD_GITHUB_AUTH=proxy` in its environment
  (`tod_sandbox::node::node_env`). With it, `tod_store::github::Github`
  sends no `Authorization` header of its own and makes its requests through
  `HTTPS_PROXY`, trusting the CA bundle `SSL_CERT_FILE` names
  (`tod_store::sandbox_http`, the same agent the supervisor reaches the
  orchestrator with); `CredentialStore` reports the GitHub token as
  available from the proxy (`CredentialBackend::Proxy`,
  `resolve_github_auth`) instead of "not configured", and `tod-cli secrets
  run` gives a command the same placeholder `gh` gets. So the supervisor's
  derived gate checks (`pr-mergeable`, `pr-merged`) and `tod-cli pr` (which
  the shim runs in the sandbox; see `tod-cli` in an agent sandbox) work with
  the token never in the sandbox. The orchestrator drops the flag from what
  a shim sends it: its own proxy has no GitHub rule.

  Measured (September 2026, `testspace-358401`, node sandbox
  `node-cloud-test` created by "Run in the cloud" with the user's GitHub
  token, mock agent, on the throwaway `octocat/Hello-World` checkout; all
  GitHub traffic read-only except the supervisor's own branch push):

  | Check | Result |
  |---|---|
  | the sandbox's environment | `TOD_GITHUB_AUTH=proxy`, `GH_TOKEN=<placeholder>`, `HTTPS_PROXY=http://localhost:49152`, `SSL_CERT_FILE=/etc/ssl/certs/sandbox-ca-bundle.crt`; no token |
  | `curl https://api.github.com/user` | the user's login |
  | `tod-cli pr status`, `pr list`, `pr list --all-open` (run by the shim in the sandbox) | the recorded PR's live state (merged, checks); the repository's open PRs |
  | `tod-cli secrets list` | `github_token`: set, added by the sandbox's proxy |
  | the supervisor's gate checks at `pr` and `approved` | `…/pull/6 is already merged.`, `…/pull/6 is merged.` (derived, pass); the node moved on to `learn` |
  | `git push --dry-run` to `octocat/Hello-World` | `Permission to octocat/Hello-World.git denied to <the user>`: authenticated, refused only for permission |
  | the same with `NO_PROXY=github.com` | no credentials at all |

  The image (`tod-baked-ubuntu-24-04`) has no `gh`; where an image has it,
  the placeholder `GH_TOKEN` works as measured above (`gh api user`, `gh
  repo view`). Not yet measured with a repository the user can write to:
  opening a PR (`tod-cli pr open`) and a push that is accepted.
- **Where the values come from.** The user already stores a GitHub token and
  a Linear API key in tod (`CredentialStore`: the OS keyring, else an encrypted
  file; set in the app or with `tod-cli secrets set`). When tod creates a
  node's sandbox, it reads them and passes them as the proxy rules' secrets;
  nothing is configured in Blaxel by hand. The sandbox therefore acts as the
  user who accepted the node: commits, PRs, and ticket updates are theirs. A
  token changed later reaches only sandboxes created after (Blaxel can
  replace a running sandbox's rules, but only by redeploying it; see To
  verify 4). **User tokens never leave the user's
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
- **The Claude subscription: proxy injection too.** A long-lived token from
  `claude setup-token`, which the user runs once on their own machine
  (Settings → Cloud sandboxes → Claude subscription → **Get a token** opens a
  terminal running it; the printed token is pasted into the row above it).
  tod keeps it in `CredentialStore` (`claude_oauth_token`, a kind agents
  cannot read through `tod-cli secrets`) and puts it in each node sandbox's
  proxy: a rule for `api.anthropic.com` sets `Authorization: Bearer
  {{SECRET:claude}}`, and the sandbox's environment gets
  `CLAUDE_CODE_OAUTH_TOKEN=<placeholder>`, so Claude Code starts signed in
  and never sees the token. No `claude /login` in any sandbox.
  `sandboxes.toml`'s `claude_token_via = "env"` (under `[blaxel]`) is the
  fallback: the token goes into the supervisor's environment alone (the
  relay is started with it as `TOD_SUPERVISOR_ENV_CLAUDE_CODE_OAUTH_TOKEN`,
  takes it out of its own environment, and hands it only to the supervisor
  it starts, so shells and other processes the relay starts do not get it),
  never into the sandbox-wide environment. Claude Code, the supervisor's
  child, then holds the real token, and so can anything it runs.
  "Run in the cloud" and the replacement of a lost sandbox fail before
  creating anything when no token is stored and the node would run Claude
  (anything but `TOD_CLOUD_AGENT=mock`), naming the Settings row.
  **A token changed later** reaches a node when its sandbox is next created
  (a replacement, or running it in the cloud again after deleting it): proxy
  rules cannot be changed after creation. With `env`, it is passed at every
  provisioning, so it also reaches the supervisor's next start after the
  node is run in the cloud again.

  Measured (September 2026, `testspace-358401`, a node sandbox created by
  `tod_sandbox::node::create_body` in proxy mode with the dummy token
  `test-not-a-real-token`, plus, for the test only, the same rule for
  `httpbin.org`):

  | Check | Result |
  |---|---|
  | `curl httpbin.org/headers` with `Authorization: Bearer <placeholder>` | echoed `Bearer test-not-a-real-token`: the proxy **replaces** the header, the placeholder is not sent |
  | `POST api.anthropic.com/v1/messages`, no auth header, through the proxy | `401 Invalid bearer token` (the dummy was added) |
  | the same bypassing the proxy (`--noproxy '*'`) | `401 x-api-key header is required` |
  | the placeholder (`sk-ant-oat01-…`) sent directly, bypassing the proxy | `401 OAuth access token is invalid.` |
  | `claude -p hi` (Claude Code 2.1.283) | no login prompt; `401 Invalid bearer token`: its request went through the proxy and got the dummy |
  | `claude -p hi` with `NO_PROXY='*'` | `401 OAuth access token is invalid.`: the placeholder, sent directly |
  | `claude-code-acp` 0.16.2: `initialize`, `session/new`, `session/prompt` | session created with no `~/.claude` credentials; the prompt failed with `401 Invalid bearer token`, again the injected dummy |
  | the dummy in the sandbox's environment or files (`/etc /root /opt /tmp`) | nowhere |
  | env mode: relay started with `TOD_SUPERVISOR_ENV_CLAUDE_CODE_OAUTH_TOKEN`, then poked | the supervisor's environment had `CLAUDE_CODE_OAUTH_TOKEN`; a command run through the relay (`tod-sandbox exec`) had neither variable; the process API's record of the relay shows no environment |

  So Claude Code's own traffic (Node, with `NODE_USE_ENV_PROXY=1` and the
  proxy's CA in `NODE_EXTRA_CA_CERTS`) goes through the proxy, and it does
  not check the token locally: the proxy is the default. Not measured, as it
  needs a real token: a successful, streamed turn through the proxy.
- **The watchdog** holds a Blaxel key as a job secret.

## Development account

Sandbox schedules and volumes are now enabled on the development workspace
(`testspace-358401`); forking still is not (`403 Sandbox snapshot/fork
feature is not enabled for this workspace`). Each feature has a stand-in,
chosen per account in `sandboxes.toml`, so the same code runs everywhere:

- **Schedules → the orchestrator's timer** (`scheduler = "orchestrator"`,
  the default when the file does not say; `"blaxel"` is the design above,
  verified on the development workspace). An agent sandbox still
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
  on a timer, which at development scale is a few dollars a month. The
  timer stays: the orchestrator's `wakes` table is also how the answer,
  webhook, and impact paths retry a poke that did not get through, and how
  a lost sandbox is noticed, whichever scheduler the nodes use.
- **Volumes → the orchestrator's own disk.** Without `orchestrator_volume`,
  the per-user databases are on the sandbox's ordinary filesystem: they
  survive standby but not the sandbox. With it, `tod-sandbox orchestrator`
  creates the volume (4 GB) and mounts it at `/data`; `--move-data` moves an
  existing orchestrator's data onto it (see `orchestrator.md`).
- **Forking → creating from the image.** Without `node_base`, a node's
  sandbox (and a replacement for one) is created from the image, given the
  relay, supervisor, and bundles, and checks out the node's branch. With
  `node_base = "<name>"`, a base sandbox with everything but the checkout is
  made once (and again when the binaries, bundles, credentials, image, or
  orchestrator change: its fingerprint is kept in the data root), and each
  node is forked from it with its own environment, then only checks out
  and starts. A fork that is refused or fails falls back to creating from
  the image. A fork has the base's labels (`tod-kind=node-base`, no node),
  so the watchdog knows it by its `TOD_USER` and `TOD_NODE`.

## To verify

1. **Sandbox schedules.** Verified on the development workspace
   (2026-09-27), with a probe sandbox and then a real node run with the
   mock agent (`scheduler = "blaxel"`):
   - A schedule is `POST /v0/sandboxes/<sb>/schedules` with `{type: at |
     cron | sleep, value, input: {command, name, env, workingDir, keepAlive,
     timeout}}`; `GET` lists them, `DELETE /v0/sandboxes/<sb>/schedules/<id>`
     removes one, and `GET /v0/sandboxes/<sb>/schedule-executions` shows
     each run. The id is Blaxel's (`schedule-N`, reused), not ours; at most
     100 per sandbox. A one-shot `at` schedule deletes itself after it runs.
   - It fires 10–50 s after its time (12–40 s on the probe, 10 s and 50 s
     on two node runs), and wakes a sandbox in standby. A wake is never
     early, and good to about a minute.
   - A sandbox can schedule itself through its proxy, which adds the token
     for `api.blaxel.ai` (the supervisor sends a placeholder).
   - End to end: the mock node recorded `wait --until 3m`, the supervisor
     created `wait-<id>` on its own sandbox and slept; the schedule fired,
     the relay started the supervisor, it settled the wait, cancelled the
     (already gone) schedule, and carried the node on to its review gate. A
     10-minute wait satisfied from the app instead had its live schedule
     deleted on the next poke.
   - With an API key (`auth = "api-key"`, 2026-09-28): the node's proxy
     rule for `api.blaxel.ai` carries the key (`Sandboxes::token`; the `bl`
     token cache was not touched by the runs, and the user endpoint
     `/v0/profile` answered 401 through the proxy, while schedules were
     created, listed, and deleted through it). A mock node on `mentics/test-repo` recorded `wait 3m`; its
     supervisor created `wait-<id>` (`at` 03:59:00Z, the wait due at
     03:58:59.3Z) and exited; the sandbox went to standby; the schedule
     fired at 03:59:42 (**42 s late**), its command poked the relay, which
     started the supervisor; it settled the wait within a second, found the
     schedule already gone, and carried the node on to its PR gate. The
     schedule list was empty afterwards. `tod-sandbox status` (the control
     plane) still said STANDBY for about three minutes after the wake.
   - Cancelling: a second node's 10-minute wait, cancelled in the app
     (`tod-cli wait cancel`, then sync), did **not** wake it: waits were not
     context, so the orchestrator poked nobody, and the node would have slept
     until the old time. Its next poke (an obligation added) deleted the
     live schedule, which then never fired. Fixed: a wait changed by any
     client but the node's own supervisor now pokes the node
     (`tod-orchestrator`'s `wait_changes`). Deployed and verified
     (2026-09-28): a mock node asleep in standby on a 10-minute wait (its
     `wait-<id>` schedule listed); the wait cancelled in the app and synced
     (the sync returned at 05:50:26.7); the orchestrator logged "a wait
     changed", the relay started the supervisor, which deleted the schedule
     at 05:50:27.1 (**0.4 s** after the sync returned) and carried the node
     on to its PR gate; the schedule list was empty at the next look (5 s).
     An event wait *added* in the app to a sleeping node woke it the same
     way (0.65 s), and it went back to sleep on both waits.
   - The first start of each node's supervisor spent 74–87 s seeding its
     copy of the database: `sync::snapshot` (on the orchestrator, under the
     user's sync lock) and `sync::restore` (in the sandbox) used the paced
     backup, 5 pages then 100 ms, about 36 s each for the 7 MB database.
     Both now copy in one step: on two fresh nodes (2026-09-28, same data
     root) the supervisor's first start went from "seeding the local copy"
     to its first step in **0.68 s** and **0.85 s**, and had scheduled its
     wake 3.4 s and 4 s after it started. Creating and provisioning each
     sandbox (baked image) took 29.5 s and 30.3 s.
2. **Agent Drive.** Private preview, and only in `us-was-1`, which is also
   the region the team will use (tod's default region is now `us-was-1`).
   Check that a mount survives standby, and how it behaves with a line
   appended per transcript write. Not testable yet (2026-09-28): the
   development workspace does not have it (`GET /v0/drives` answers 403
   "Drives feature is not enabled for this workspace"); it has to be
   requested from Blaxel for the workspace first.
3. **Volumes.** Verified on the development workspace (2026-09-27): an
   orchestrator's `/data` moved onto a volume with `--move-data`, then the
   sandbox deleted and redeployed with `tod-sandbox orchestrator`: the
   volume reattached at `/data` (virtiofs) with every database and marker
   intact, and the app's sync carried on from its cursor. A volume attaches
   to one sandbox at a time and hides what the image had at its mount
   point; deleting a sandbox in standby can take minutes before its name
   (and volume) are free.
4. **The proxy.** Claude Code's own traffic goes through it and gets the
   subscription token injected (see Credentials; measured with a dummy
   token). A real token works through it (2026-09-28): a Claude node
   (`cloud-claude-pr-3`, from `proposed`, one obligation: add
   `tod-claude-test-3.md` with one sentence) ran 11 Claude sessions (gate
   checks, on-entry turns, Implement, Verify, Review, PR) with the token only
   in the proxy, and opened https://github.com/mentics/test-repo/pull/4
   (one file, one line), stopping at `approved` on "PR merged?". Polled
   every ~16 s (58 polls, each stamped with the sandbox's clock): never more
   than one `claude-agent-acp` and one `claude` alive (once, a `claude auth
   status` beside them, from the same adapter); a session's `claude`,
   orphaned when its adapter exits, showed as `<defunct>` under the relay in
   3 polls and was gone by the next (the relay reaps orphans every 5 s; 11
   reaped, one per session); afterwards nothing but the sandbox API and the
   relay, no zombies. Existing sandboxes, created without the proxy, cannot
   get it (Blaxel: the proxy can be neither enabled nor fully disabled after
   creation).

   **Rotating a running sandbox's secrets** (2026-09-28). Blaxel documents
   it (SDK `updateNetwork`; REST `PUT /v0/sandboxes/<name>`): the update
   **replaces the whole network configuration**, so every rule and every
   secret must be sent again (secrets are write-only: `GET` returns the
   header templates, never the values, and shows every env value as
   `****`). Tried on a mock node's running sandbox, adding a dummy rule
   (`httpbin.org`, `X-Tod-Probe: {{SECRET:probe}}`) with a body of only
   `metadata.name` and `spec.network.proxy` (the node's own rules and
   credentials unchanged): accepted in 0.2 s and stored, but it is a **full
   spec replace and a redeploy**, not a hot update. The events show "Update
   deployment", then "Deployment has failed" 7 s later: the partial spec
   had cleared `runtime.image` (to `""`), reset `memory` to 1024, and
   dropped the relay's port (the env names were kept). A request through
   the proxy in the second before the redeploy did not yet carry the new
   header. So rotating needs the complete create body (`node::create_body`,
   with every credential) and redeploys the sandbox. Whether a full-body
   update keeps the sandbox's disk and processes (the checkout, `/opt/tod`,
   the supervisor's replica) is untested: it means sending the real
   credentials again, which was not approved for this run. Until then tod
   does not rotate: a token replaced in Settings reaches a node when its
   sandbox is next created ("Stop running in the cloud", then "Run in the
   cloud").
5. **The push service.** ntfy (`ntfy.sh`; see `orchestrator.md`, "Telling
   the app"). Measured 2026-09-28 from this machine (US West) against the
   development orchestrator (`us-was-1`), with a subscriber held the way
   `cloud_notify` holds it (`GET <server>/<topic>/json`, the topic from `GET
   /users/<u>/notify`). 12 samples, each a `tod-cli node rename` POSTed to
   the orchestrator's `/cli` as a node's sandbox would: `/cli` answered in
   124–273 ms, and the ntfy message arrived **47–61 ms (median 50 ms) after
   that reply**, 180–329 ms (median 211 ms) after the request was sent. The
   app then pulls the feed (one more round trip). The once-a-second limit
   delayed no sample (they were 2.5 s apart).
6. **Subscription use.** One subscription driving many unattended agents: the
   limits, and that it is within the subscription's terms.
7. **The watchdog job.** Verified on the development workspace
   (2026-09-28), with the API key (`auth = "api-key"`): `tod-sandbox
   watchdog deploy` built `job/tod-watchdog:latest` and created the job
   (cron `0 * * * *`, UTC; the token a `secret: true` env). A test sandbox
   labelled as a node of a scratch orchestrator user (`watchdog-test`, one
   node) held itself with `hold?reason=wdtest&secs=7200`. The first job
   skipped it (the control plane said `STANDBY`; see Crash guards). A tick
   written every 5 s inside it had no gap for as long as it was held (13
   minutes in all), and one 12 s gap, just after the watchdog released
   it: held, the VM runs whatever `state` says. Fixed,
   the next manual execution logged `released wd-test-node: hold
   "ext:wdtest" leased for another 1h45m`, the relay's `/holds` was empty
   afterwards, and the orchestrator had filed a pending `watchdog`
   decision on the node ("Wake it again" / "Leave it asleep"). The
   held-too-long path (`TOD_WATCHDOG_MAX_AWAKE_SECS=60` as an execution
   env override): runs at 20 s and 44 s held left the hold alone; the run
   after 60 s released it. `tod-sandbox watchdog run-once` from this
   machine did the same. The cron fired on its own at 07:00:08 UTC (8 s
   late) and released a hold left for it (`released wd-test-node: hold
   "ext:wdcron" leased for another 1h17m`), filing its decision. Each run
   took 1.5–3 s. The job stays deployed on the development workspace.
   Blaxel's jobs API as found is in `orchestrator.md`.
