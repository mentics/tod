# The orchestrator

One sandbox per workspace (`tod-orchestrator`) holds a copy of each user's
tod database and runs every `tod-cli` command the agent sandboxes send. The
design is in [autonomous-nodes.md](autonomous-nodes.md); this is how it runs.

## Server (`crates/tod-orchestrator`)

`tod-orchestrator [--port 8080] [--bind 0.0.0.0] [--base /data] [--tod-cli PATH]`

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

## Provisioning

```sh
scripts/build-sandbox-binaries.sh      # target/sandbox/tod-orchestrator and tod-cli (needs zigbuild)
tod-sandbox --data-root <root> orchestrator [--image IMAGE] [--bin PATH] [--tod-cli PATH]
```

This creates `tod-orchestrator` if it does not exist (labelled
`tod-role=orchestrator`, declaring the relay's port and 8080), uploads the
two binaries to `/opt/tod-orchestrator/` in 4 MB parts, (re)starts the
server as a sandbox process that restarts on failure, asks for a public
preview on 8080 (named `webhooks`, for later; a failure there is only
reported), and waits for `/health`. The server's URL is
`<sandbox url>/port/8080`. Run it again to update the binaries.

On the dev account its data lives on the sandbox's own disk, `/data`: it
goes with the sandbox. The image needs `curl` (for the health check).

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

1. Lists the workspace's sandboxes and keeps the node ones (`tod-kind=node`)
   Blaxel reports as running. One in standby, or with no state reported, is
   never contacted, since anything sent to its URL wakes it.
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
   decisions panel and the attention queue. It needs no schema of its own.

A failure on one sandbox is reported and the rest go on; the pass exits 1
if anything failed.

Running it:

- `tod-sandbox watchdog run-once [--orchestrator-url URL]` runs one pass
  from this machine with the account in `sandboxes.toml`. Without
  `--orchestrator-url` it uses the `tod-orchestrator` sandbox's
  `<url>/port/8080`.
- `tod-sandbox watchdog deploy --image IMAGE` creates (or replaces) the job
  `tod-watchdog`, cron `0 * * * *`. IMAGE must have the Linux `tod-watchdog`
  (`scripts/build-sandbox-binaries.sh` builds it into `target/sandbox/`) at
  `/opt/tod/tod-watchdog`. It needs an API-key sign-in (`tod-sandbox setup
  --auth api-key`), since a `bl login` token expires.
- The job runs `tod-watchdog`, which reads everything from its environment:
  `TOD_WATCHDOG_BLAXEL_WORKSPACE`, `TOD_WATCHDOG_BLAXEL_TOKEN` (a job secret,
  never a plain env value in the spec), `TOD_WATCHDOG_ORCHESTRATOR_URL`, and
  optionally `TOD_WATCHDOG_MAX_AWAKE_SECS` / `TOD_WATCHDOG_MAX_LEASE_SECS`.

The job's request body (`watchdog::job_body`) is a best reading of Blaxel's
jobs API, which is not documented here: verify it against a live workspace
and adjust only there and in `watchdog::deploy`.
