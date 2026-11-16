## `tod-cli access`

How you read a production system from a cloud node's sandbox when it is not a
database: the user's Hatchet (workflow runs), a Redis (claim keys and the
like), a web API such as Flagship's. This is the way to get that access. Do
not ask the user for a token, a VPN, or to run a command for you: the user's
machine makes the call with the credential it holds, and the result comes
back. You never hold a credential, and nothing is stored in the sandbox.

```
tod-cli --data-root <DATA_ROOT> access list
tod-cli --data-root <DATA_ROOT> access call <SERVICE> <ACTION> [NAME=VALUE...]
```

`list` shows the services the user has set up for you by name, each with its
actions and their arguments (`name` is required, `name?` is optional). `call`
makes one action and prints its result as JSON. Every action only reads;
nothing you can name changes data. For example:

```
tod-cli --data-root <DATA_ROOT> access call hatchet-prod runs-list since=6h statuses=FAILED limit=20
tod-cli --data-root <DATA_ROOT> access call hatchet-prod run-get id=<RUN_UUID>
tod-cli --data-root <DATA_ROOT> access call redis-prod scan pattern='claim:*'
tod-cli --data-root <DATA_ROOT> access call redis-prod get key=claim:42
tod-cli --data-root <DATA_ROOT> access call flagship-prod get path=/api/v1/orgs/<ORG> query=include=sync
```

What the kinds offer:

- **hatchet**: `runs-list` (a time span such as `since=24h` or an RFC3339 time,
  `until`, `limit`, `statuses`, `workflow`, `metadata=key:value`), `run-get`,
  `workflows-list`.
- **redis**: `get`, `type`, `ttl`, `exists`, `hget`, `hgetall`, `llen`,
  `lrange`, `scard`, `smembers`, `zrange`, `scan`, and similar lookups. No
  command that writes exists. `scan` walks the keyspace, so use a specific
  pattern and a small `max`.
- **api**: `get` of a path the user allowed (`list` names them). A path
  outside them is refused.

A result over 256 KB comes back cut, saying so: ask for less. A failure names
the service, the action and what went wrong (a refused or expired credential,
a machine that cannot reach the server, an unknown argument): read it and fix
the call. Triggering something (a resync, a retry, a deploy) is a change and is
not available here; when the work needs one, record it as a request for the
user, naming the exact call.

It needs the user's machine to be on, with the app running. When it is not
connected for a moment, `access` waits and asks again by itself, for a few
minutes: do not retry it yourself. If it still fails with "has not been
connected", that is a failure of the app's own connection, not something to ask
the user about: stop and report it as a failure with the error. If `list`
shows nothing, or not the service you need, record a request that names the
kind of service, its URL and what you need to read, so the user adds it once.
Treat the data as production: read what you need and do not copy secrets from
it into files or messages.
