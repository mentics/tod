# Brokered read-only access for cloud nodes

Status: built; the adapters are tested against local fakes. Not yet run
against a real Hatchet, Redis or Flagship (needs the user's credentials: see
"What the user sets up").

A node in a cloud sandbox sometimes has to look at a production system that
only the user's machine can reach or hold a credential for: which Hatchet runs
failed, whether a Redis claim key is set, what a Flagship endpoint says about
an organization. Before this, such a node stopped and asked the user for
access (shepherd-ledger gap 18: a Hatchet `runs.list`, Redis claim keys, a
Flagship API call). `tod-cli db` already solved this for databases; this
generalizes it.

```
tod-cli access list
tod-cli access call hatchet-prod runs-list since=24h statuses=FAILED
tod-cli access call redis-prod scan pattern='claim:*'
tod-cli access call flagship-prod get path=/api/v1/orgs/acme
```

## The same path as `db`, one more op

```
sandbox: tod-cli access call S A k=v
   │ POST /users/<u>/broker/request {"op":"service","name":S,"action":A,"args":{k:v}}
   ▼
orchestrator broker (a rendezvous; it does not look inside the request)
   ▲ long poll                               │ reply {"ok":true,"result":…}
user's machine: tod-agentd (tod_core::db_broker)
   knows the node?  declared service?  credential (saved or token_command)
   → tod_sandbox::service adapter makes the call from this machine
```

- **Nothing is stored in the sandbox**, and no credential ever goes there: the
  daemon holds it and sends only the result. `op: services` lists names, kinds
  and actions, never a URL, tenant or token (`access_context` writes the same
  names into the node's state so its agent context says which exist).
- **The orchestrator did not change**: it passes the request through, so
  nothing needs redeploying for the broker. The sandbox's `tod-cli` and the
  user's app/daemon are the two ends that must be new.
- No port forward is needed: the daemon makes the call itself. The service sees
  the call come from the user's machine, so its allowlist and VPN apply as for
  the user.

## Adding a target

Configuration plus a small adapter. In `tod_sandbox::service`:

1. add a `Kind` variant and the fields it needs to `Service`,
2. add a module with `actions()` (what an agent may do, with arguments) and
   `call(service, token, action, args)`,
3. dispatch both from `Service::actions` / `Service::call`.

The broker, the daemon, `tod-cli access`, the agent text and `tod-sandbox
service` need no change.

## Read-only by construction

An allowlist of actions, not a denylist of verbs. No adapter forwards what the
agent typed:

- **Hatchet**: three actions, three fixed GET paths on Hatchet's REST API
  (`runs-list`, `run-get`, `workflows-list`). Arguments are validated values
  (a UUID, a time span, a status from a fixed set, a limit 1 to 200) put in
  query positions.
- **Redis**: each action is one command from a fixed enum of reads (`GET`,
  `TYPE`, `TTL`, `HGETALL`, `SCAN`, …). The agent supplies keys and numbers
  only, sent as binary-safe bulk strings, so a key named `FLUSHALL` is only a
  key. The only other commands sent are `AUTH` and `SELECT` from the
  declaration. Big structures are refused (hash, set above 500), `scan` is
  bounded. A test asserts no write or admin command is in the enum.
- **api** (Flagship): one action, `get`, of a path matching the declaration's
  `allow` list (exact, or a prefix ending in `*`; `/*` and `..` are refused;
  a service with no `allow` does not load). There is no method or body
  argument. Anything that triggers work (a resync) is a change and is not
  reachable.

Every failure is the reply, naming the service, the action and the cause (an
unknown argument, a refused credential, a machine that cannot reach the
server, a failing `token_command`). A result over 256 KB is cut and says so.
Nothing falls back to anything else.

## What the user sets up, on their machine

`[[service]]` in `sandboxes.toml`, or `tod-sandbox service add|list|remove|
test|token`:

```
tod-sandbox service add hatchet-prod --kind hatchet --url https://<hatchet host> \
    --tenant <tenant uuid> --check
echo "$HATCHET_TOKEN" | tod-sandbox service token hatchet-prod     # kept encrypted here
tod-sandbox service add redis-prod --kind redis --url rediss://<host>:6379 \
    --token-command 'aws secretsmanager get-secret-value … --query SecretString --output text'
tod-sandbox service add flagship-prod --kind api --url https://<flagship host> \
    --allow '/api/v1/orgs/*' --token-command 'op read op://…' --check
```

A Redis URL often names a DNS alias of a managed cache (ElastiCache) whose
certificate is issued to the cache's own endpoint, so verifying against the URL's
host fails ("certificate not valid for name ..."). Set `tls_name` (`--tls-name`
on `service add`, or `tod-sandbox service set <name> --tls-name <name>` to change
only that on a saved service; `--clear-tls-name` removes it) to the name the
certificate is issued to. The connection still goes to the URL's host and the
certificate is still fully verified against the system roots, only against that
name. It needs a `rediss://` URL, and `service list` shows it.

The credential is the one saved with `service token` (OS keyring, else the
encrypted file, as other tod secrets), else what `token_command` prints. A
credential that is missing for a Hatchet service is an error telling the agent
the user has not set it up, not an empty call. Use read-only credentials (a
Hatchet token limited to reads; a Redis ACL user with `+@read`): the adapter
only reads, but the credential is the second line of defence.

## What is not verified

The Hatchet paths follow its v1 REST API (`/api/v1/stable/tenants/{tenant}/
workflow-runs`, `/api/v1/stable/workflow-runs/{id}`, `/api/v1/tenants/{tenant}/
workflows`) and were not run against a real server: `service test` or
`service add --check` shows within seconds whether the deployment's version
matches. Redis is RESP over TCP or TLS and was tested against a fake server.
The Flagship paths are whatever the user allows: tod does not know them.

## Writes are never brokered (gap 31)

A node's sandbox has no write access to the Flagship API or any other
production system, directly or through the broker. A write the sandbox cannot
make (for example the per-org resync) is a request to the user
(`tod-cli decisions ask`, reason `access`): one call is given as the exact
command to paste; anything longer is a script the agent writes in the
sandbox, which the user copies to their checkout's untracked `.local/` and
runs with one line: `tod-sandbox cp "$TOD_SANDBOX":/tmp/<script> .local/<script>`
(`cp` copies one file from the sandbox byte for byte). The agent's side of
this is the last section of `media/context/cli/decisions.md`.
