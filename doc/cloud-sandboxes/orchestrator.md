# The orchestrator

One sandbox per workspace (`tod-orchestrator`) holds a copy of each user's
tod database and runs every `tod-cli` command the agent sandboxes send. The
design is in [autonomous-nodes.md](autonomous-nodes.md); this is how it runs.

## Server (`crates/tod-orchestrator`)

`tod-orchestrator [--port 8090] [--bind 0.0.0.0] [--base /data] [--tod-cli PATH]`

- `--base` (or `TOD_ORCHESTRATOR_BASE`): each user's data root is
  `<base>/users/<user>/`, created and opened as a `FleetStore` on first use,
  with its mutation socket running, so every `tod-cli` run against it writes
  through that store's one writer.
- `--tod-cli` (or `TOD_ORCHESTRATOR_TOD_CLI`): the Linux `tod-cli`; defaults
  to the one beside the executable.
- A small std HTTP/1.1 server: one request per connection, bodies by
  `Content-Length` only (no chunked uploads).

Every request but `/health` names the user, in `X-Tod-User` or the path (if
both, they must agree). A user name is one path segment: letters, digits,
`-`, `_`, `.`, not starting with `.` or `-`, at most 64. Anything else is a
400 and nothing is created.

| Route | Body | Reply |
|---|---|---|
| `GET /health` | | `ok` |
| `POST /cli` | a `cli_relay` request frame | 200 with the reply frame, whatever the exit code |
| `POST /users/<u>/seed` | a database snapshot (`tod_store::sync::snapshot`) | `{"last_seq": n}` |
| `POST /users/<u>/changes` | JSON `[Change]`, the app's outbox | the `ApplyReport` plus `last_seq` |
| `GET /users/<u>/changes?after=<n>` | | `{"last_seq": n, "changes": [...]}` |

`/cli` runs the real `tod-cli` with `--data-root` and `TOD_DATA_ROOT` set to
the user's root, replacing any the sandbox sent; only `TOD_*` variables are
passed through. The token line of the frame is not checked: the orchestrator
is reached only through the sandboxes' proxies.

A seed replaces the user's database, so it closes the user's store first
(409 if a request still holds it; retry). The snapshot's `last_seq` is kept
in `<root>/orchestrator-seed-seq` and is where the feed starts: `GET
changes` never returns the app's own log back to it, whatever `after` says.
The app pulls again with `after` set to the `last_seq` it was given. Changes
from the app are applied without being logged again, so they are not echoed.

After an apply, three handlers look at what arrived. `impact_handler.rs` marks
the running cloud nodes whose context the changes affect and pokes them.
`answers.rs` pokes a cloud node when the changes answer one of its **stop
questions** (`tod_core::stop_questions`): the decisions the node's
supervisor asks after repeated agent failures (`supervisor:failures`) or a
spent budget (`supervisor:budget`), and the watchdog's flag (`watchdog`),
each marked by the decision's `protocol` column. Decisions are otherwise not
context (`tod_core::impact::IGNORED_TABLES`), so an answer to any other
decision pokes nothing. On waking, the supervisor reads the node's latest
stop question: "Keep going" / "Wake it again" carries on (after a spent
budget, with another budget of the same size); "Leave it stopped" / "Leave
it asleep" leaves it stopped, asking nothing and scheduling no wake, until
the user answers again (the last answer counts). While one is unanswered the
supervisor does not ask again. `wait_changes.rs` pokes a cloud node when a
client other than its own supervisor changes one of its waits (the user
cancels, satisfies, or reschedules it): waits are not context either, and
without the poke the node would sleep until the wake it scheduled for the
old time.

## Provisioning

```sh
scripts/build-sandbox-binaries.sh      # target/sandbox/tod-orchestrator and tod-cli (needs zigbuild)
tod-sandbox --data-root <root> orchestrator [--image IMAGE] [--bin PATH] [--tod-cli PATH] [--move-data]
```

This creates the orchestrator's sandbox (`tod-orchestrator`, or
`orchestrator = "<name>"` under `[blaxel]` in `sandboxes.toml`; the app and
the nodes find it by that name too) if it does not exist (labelled
`tod-role=orchestrator`, declaring the relay's port and 8090; not 8080, where the sandbox's own API listens), uploads the
two binaries to `/opt/tod-orchestrator/` in 4 MB parts, (re)starts the
server as a sandbox process that restarts on failure, asks for a public
preview on 8090 (named `webhooks`, for later; a failure there is only
reported), and waits for `/health`. The server's URL is
`<sandbox url>/port/8090`. Run it again to update the binaries.

Its data is under `/data`. By default that is the sandbox's own disk, and
goes with the sandbox. With `orchestrator_volume = "<volume>"` under
`[blaxel]`, the volume (4 GB, in the account's region, labelled
`tod-role=orchestrator-data`) is created if missing and mounted at `/data`,
so the data outlives the sandbox: delete it, run `tod-sandbox orchestrator`
again, and the new one has everything. A volume can only be given to a
sandbox when it is created, so for an orchestrator that already exists
without it the command stops and asks for `--move-data`, which stops the
server, packs `/data`, keeps a copy in the data root
(`<orchestrator>-data.tar.gz`), deletes and recreates the sandbox with the
volume, and unpacks it there; the copy is removed once it is on the volume.
If the move stops after the old sandbox was deleted (deleting one in standby
can take minutes), run it again with `--move-data`: it restores from that
copy. Nodes reach the orchestrator by name, so they are unaffected. The
image needs `curl` (for the health check).

## Telling the app (`notify.rs`)

The orchestrator announces changes through **ntfy** (`https://ntfy.sh`, or
the server in `TOD_NTFY_URL`; `off` turns it off). Messages carry no data.

- Each user has a random secret, made once in `<root>/notify.json`; their
  topic is `tod-<secret>`, and `tod-<secret>-alerts` is for the phone.
  `GET /users/<u>/notify` returns `{"server", "topic", "alerts_topic"}`.
- After any POST that succeeded, the request thread pokes the notifier; its
  own thread checks the user's `last_seq` and publishes `changed` to the
  topic if it moved, at most once a second per user, trailing edge included
  (the last change is always announced).
- When a new pending decision or blocked plan step appears, it also
  publishes "A node needs you" (priority high) to the alerts topic. Subscribe
  the ntfy phone app to that topic to get pushes.
- The app (`tod_core::cloud_notify`), once its data root is seeded, fetches
  the topics into `cloud-sync.json` (`notify`), holds `GET <server>/<topic>/json`
  on a background thread (renewed every 10 minutes, resuming with `since=`;
  reconnects with backoff), and runs one cloud sync per message, never two
  at once.

## The watchdog (`tod_sandbox::watchdog`, `tod-watchdog`)

A Blaxel job that runs hourly, so a node's sandbox is never kept awake (and
billed) longer than its lease allows because something kept renewing a hold.
Each pass:

1. Lists the workspace's sandboxes and keeps the deployed node ones
   (`tod-kind=node`, or a fork of a base known by its `TOD_USER` and
   `TOD_NODE`), whatever state Blaxel reports. The control plane's `state`
   cannot be used to skip sleeping ones: a sandbox held awake by the relay's
   `keepAlive` process read `STANDBY` in all 30 samples over 10 minutes
   while its VM ran without a pause (2026-09-28), and the first deployed
   job skipped it for that. Asking one that is really asleep wakes it for
   that one request.
2. Asks each one's relay what holds it (`GET /holds`, see
   `relay-protocol.md`).
3. Judges it over its lease when it has been held continuously for more
   than `max_awake_secs` (default 6 h; the relay's own `--max-hold-secs` is
   4 h, so past that something keeps renewing), or holds a lease with more
   than `max_lease_secs` left (default 1 h; the supervisor renews 120 s at a
   time).
4. For each one over: `POST /release-all` on its relay, then
   `POST /users/<u>/nodes/<n>/flags` on the orchestrator (`flags.rs`), which
   records a pending decision on the node (`tod-cli decisions ask`, options
   "Wake it again" / "Leave it asleep"), so the user sees it in the
   decisions panel and the attention queue. It needs no schema of its own:
   it is filed as a `watchdog` stop question (the orchestrator sets
   `TOD_DECISION_KIND` on that `tod-cli` run, and strips it from `/cli`
   requests), so its answer pokes the node (see Server).

A failure on one sandbox is reported and the rest go on; the pass exits 1
if anything failed.

Running it:

- `tod-sandbox watchdog run-once [--orchestrator-url URL]` runs one pass
  from this machine with the account in `sandboxes.toml`. Without
  `--orchestrator-url` it uses the `tod-orchestrator` sandbox's
  `<url>/port/8090`.
- `tod-sandbox watchdog deploy` creates the job `tod-watchdog` (cron
  `0 * * * *`, UTC), or updates it in place (a new revision) when it
  exists. It first builds the job's image, `job/tod-watchdog:latest`: the
  Linux `tod-watchdog` from `target/sandbox/`
  (`scripts/build-sandbox-binaries.sh`) on Alpine, as the entrypoint,
  built by Blaxel with `bl push` (about 16 s). `--image IMAGE` skips the
  build and uses an image whose entrypoint is `tod-watchdog`. It needs an
  API-key sign-in (`tod-sandbox setup --auth api-key`), since a `bl login`
  token expires; the key is the job's secret.
- The job runs `tod-watchdog`, which reads everything from its environment:
  `TOD_WATCHDOG_BLAXEL_WORKSPACE`, `TOD_WATCHDOG_BLAXEL_TOKEN` (a secret
  env, `secret: true`), `TOD_WATCHDOG_ORCHESTRATOR_URL`, and
  optionally `TOD_WATCHDOG_MAX_AWAKE_SECS` / `TOD_WATCHDOG_MAX_LEASE_SECS`.
- Run it now: `POST /v0/jobs/tod-watchdog/executions` with
  `{"tasks":[{}]}` (an execution's `env` overrides the job's, e.g. a low
  `TOD_WATCHDOG_MAX_AWAKE_SECS` to test); its output is in `bl logs job
  tod-watchdog <execution-id>`.

Blaxel's jobs API, as checked against the development workspace
(2026-09-28; `watchdog::job_body`): a job's runtime has no command, only an
image (built by Blaxel: `bl push` of a `type = "job"` project files it as
`job/<name>`), whose entrypoint runs once per task; fields the API does not
know (`command`, a `secrets` list, `maxConcurrentTasks`) are dropped without
an error, and memory is raised to at least 1024 MB. A secret is an env entry
with `secret: true`; every stored env value reads back as `****`. The
trigger is `{"type": "cron", "configuration": {"schedule": ..., "tasks":
[{}]}}`.
- The `tod-orchestrator` binary publishes by default. The library's
  `Server::new` / `with_poker` publish only when `TOD_NTFY_URL` is set, so
  tests and embedders never reach ntfy.sh unasked.

## Webhooks (`webhooks.rs`)

GitHub and Linear deliveries go to the orchestrator's public preview URL:
`POST /webhooks/github` and `POST /webhooks/linear`. They carry no
`X-Tod-User`; the signature authenticates them and routing picks the user.

**Registering.** One secret per source for the whole orchestrator, made once
in `<base>/webhooks.json` (`{"github": "...", "linear": "..."}`), or set by
`TOD_ORCHESTRATOR_GITHUB_WEBHOOK_SECRET` / `TOD_ORCHESTRATOR_LINEAR_WEBHOOK_SECRET`.
Read it from that file on the orchestrator sandbox, then:

- GitHub (repository or organization → Settings → Webhooks): payload URL
  `<preview-url>/webhooks/github`, content type `application/json`, that
  secret; events: pull requests, pull request reviews and review comments,
  issue comments, check suites, check runs, workflow runs, pushes.
- Linear (Settings → API → Webhooks): URL `<preview-url>/webhooks/linear`;
  Linear makes the signing secret, so put it in `linear` in `webhooks.json`
  (or the env var) and restart.

A request whose HMAC-SHA256 over the raw body (`X-Hub-Signature-256:
sha256=<hex>`, `Linear-Signature: <hex>`) does not match, compared in
constant time, is refused with 401 before its body is parsed.

**Keys.** Each delivery becomes match keys:

| Event | Keys |
|---|---|
| `pull_request` | `github:pr:<n>:<action>` (`merged` for a merged close), `github:branch:<b>:pr:<action>` |
| `pull_request_review(_comment)` | `github:pr:<n>:review:<state>` (`approved`, `changes_requested`, `commented`; `comment` for review comments), same under `github:branch:<b>:` |
| `issue_comment` | `github:pr:<n>:comment` or `github:issue:<n>:comment` |
| `check_suite`, `check_run`, `workflow_run` (only `completed`) | `github:pr:<n>:checks:<conclusion>` per PR, `github:branch:<b>:checks:<conclusion>` |
| `push` | `github:branch:<b>:push` |
| other GitHub events | `github:<event>:<action>` (routed by wait only) |
| Linear | `linear:<type>:<identifier>:<action>` and `linear:<type>:<id>:<action>` (e.g. `linear:issue:ENG-12:update`) |

An `event` wait's `match_spec` matches a key when, with `:` and spaces both
taken as separators, it is the key or a prefix of it ending at a separator:
`github:pr 12 checks` matches `github:pr:12:checks:failure`. The agents'
version is in `media/context/cli/wait.md`.

**Routing**, across every user under `<base>/users/`: first the cloud nodes
(`cloud_nodes`) whose Files branch (`node_fields.branch`) is the event's
branch (a Linear issue's `branchName`); if none, every node with a pending
event wait that matches. For each node:

1. The event is recorded in `node_events` (schema v69, synced: keys, a
   summary, the raw payload), under the user's sync lock.
2. Its matching pending waits are set `satisfied` through
   `InterviewCommand::SetWaitState` (actor `webhook`).
3. Their orchestrator wakes (id = wait id, in `wakes.json`) are dropped.
4. Its sandbox is poked (a failed poke is retried by the wake timer), and
   the user's change notice is sent.

A wait scheduled with Blaxel (`BlaxelScheduler`) keeps its schedule (its
process is named `wait-<id>`) here: the orchestrator has no Blaxel
credentials for the user's sandboxes. The poke wakes the supervisor, which
cancels the schedule it no longer needs (`waits::reconcile_wake`); one that
fires first is a harmless poke.

Deliveries are not deduplicated (`X-GitHub-Delivery` is ignored); a repeat
records the event again and pokes again, which is harmless. Webhooks can be
lost; every event wait also has its deadline, so a lost one only delays the
node.
