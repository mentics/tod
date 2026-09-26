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
