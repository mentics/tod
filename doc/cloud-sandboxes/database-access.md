# Database access for cloud nodes

Status: built. Verified by hand from a sandbox to a production RDS instance
(October 2026); the broker path is verified as described under "Verified".

A node running in the cloud sometimes needs a database that only the user's
own machine can reach: an RDS instance with an IP allowlist, or behind a VPN,
logged in to with IAM authentication. The sandbox cannot reach it, and holds
no AWS credential. This lets the node's agent run

```
tod-cli db psql prod -- -c "select count(*) from parts"
tod-cli db run  prod -- python scripts/check_counts.py
```

and have the connection made **from the user's machine**, with a login signed
**on the user's machine**. It builds on [port-forwarding.md](port-forwarding.md).

## What the agent holds

Nothing long-lived. `db run` gives the command it starts a 15-minute RDS IAM
token in `PGPASSWORD`, good for one database user on one database, and masks
it in the command's output. The AWS session, and the SSO refresh token behind
it, never leave the user's machine. No credential is stored in a sandbox, so
nothing needs rotating, and a forked or copied sandbox carries none.

## Parts

```
sandbox: tod-cli db run prod
   │ 1. POST /users/<u>/broker/request {"op":"token","name":"prod"}
   ▼
orchestrator (broker.rs: a rendezvous, nothing stored)
   ▲ 2. GET  /users/<u>/broker/next   (long poll, held by the daemon)
   │ 5. POST /users/<u>/broker/reply/<id>
user's machine: tod-agentd (tod_core::db_broker)
   3. open the forward on the node's sandbox: relay /tunnel, port 15432 → host:5432
   4. sign the token:  aws rds generate-db-auth-token …  (the user's SSO profile)
   ▼
sandbox: map the database's name to 127.0.0.1 in /etc/hosts, then run the
         command with PGHOST=<name> PGPORT=15432 PGPASSWORD=<token> …
         psql → 127.0.0.1:15432 → relay → tunnel → user's machine → database
```

The database sees the connection come from the user's machine, so its
allowlist and the VPN apply as they do for the user.

### Configuration, on the user's machine

`[[database]]` in `sandboxes.toml`:

```toml
[[database]]
name = "prod"                      # what the agent asks for
host = "prod-monolith-rds.cv9iw2uvo9td.us-gov-east-1.rds.amazonaws.com"
port = 5432
user = "joel@cofactr.com"          # the database user the token logs in as
dbname = "monolith"
region = "us-gov-east-1"
profile = "cofactr-gov"            # AWS CLI profile: `aws sso login --profile …`
sandbox_port = 15432               # optional; 15432 by default
```

- **`host` must be the server's own name**, the one in its TLS certificate:
  for RDS, the instance endpoint, not a CNAME to it. A token signed for the
  alias is refused (`PAM authentication failed`), and the certificate does
  not match it. (Measured against `prod-monolith-db.cofactr.com`, a CNAME.)
- `sandbox_port` is not 5432 because sandboxes built from the workspace image
  already run a Postgres there. The token is signed for the real `host:port`,
  not the sandbox's port, so they need not match.
- A malformed or repeated entry is an error, not skipped.

### The broker (`tod-orchestrator`, `broker.rs`)

A rendezvous: three routes, in memory, nothing persisted. A sandbox's request
waits for the user's machine to take it and answer (up to 90 s). It fails at
once, saying why, when no machine has polled in the last 45 s (the user's
machine is off, or tod is not running). The orchestrator sees the reply, which
carries the token. It is already the system of record for the user's
database and is reached only through the sandboxes' proxy.

### The daemon (`tod_core::db_broker`, run by `tod-agentd`)

Holds a 25 s long poll on the broker while any `[[database]]` is declared.
For each request it:

1. answers only a **declared database** and only for a **node it knows runs in
   its own cloud sandbox** (`cloud_nodes`); anything else is an error reply,
2. opens the forward on that node's sandbox (`tod_sandbox::hold`), waiting
   until the relay is listening: the tunnel is held while the node keeps
   asking and closed 10 minutes after the last request, so an idle sandbox
   can sleep (an open tunnel keeps a sandbox awake),
3. signs the token with the AWS CLI and replies.

`op: list` returns the declared names (never hosts or users' secrets).
`op: touch` only keeps the forward open; `db run` sends one a minute while its
command runs.

### In the sandbox (`tod-cli db`)

Runs in the sandbox, like `pr` and `secrets` (`cli_relay::LOCAL_NOUNS`),
because it edits `/etc/hosts` and starts the command there. It:

- maps `host` to `127.0.0.1` in `/etc/hosts` (a line marked
  `# tod db <name>`, replaced, never duplicated; it needs root). With that,
  the client connects to the real name, so TLS verification
  (`sslmode=verify-full`) and the token's signed host both match, and any
  libpq client works with no special settings;
- fetches the RDS CA bundle for the region once, through the sandbox's proxy;
- validates what came back over the network before using it (host and name
  are plain words, the CA URL is an RDS trust store).

## Safety

- **The sandbox picks a name, nothing else.** It cannot supply a host, port,
  user, profile or command. The daemon connects only to what the user
  declared, as the user declared.
- **A token is narrow and short.** Fifteen minutes, one database user, one
  database. Without the forward it is useless, and the forward goes only to the
  declared host.
- **Production is production.** The token logs in as the declared database
  user with that user's privileges. For an agent, declare a user whose
  privileges suit it (read-only is the safe choice); `db` cannot narrow them.
- **Trust is the orchestrator's.** Any sandbox that can reach the orchestrator
  can ask for any declared database under any node id the daemon knows; the
  orchestrator does not authenticate beyond the workspace proxy (see
  autonomous-nodes.md, "Calling between sandboxes"). A node's agent already
  has everything the node does. This does not widen that to other users: the
  broker is keyed by user, and the daemon is the user's.
- **Nothing is hidden.** A failure is the reply the agent reads: the AWS
  session expired (and the profile to sign in with), the machine cannot sign,
  the database is not declared, the node is unknown, the relay is too old to
  forward. The agent is told to record a request for the user instead of
  retrying.

## When it works

Only while the user's machine is on, with tod (its `tod-agentd`) running and
the AWS session signed in. That is inherent: the database only admits that
machine. The orchestrator and the node run without it; a `db` command
without it fails at once with the reason.

The AWS SSO session lasts as long as the user's identity provider allows
(here the access token is 1 hour, refreshed from a refresh token valid for the
session). `aws sso login --profile <profile>` renews it, and nothing else
changes.

## Verified

By hand (October 2026), from a forked sandbox to the production RDS instance,
with a token signed on the user's machine, `sslmode=verify-full`, a hosts
entry for the RDS endpoint and the forward on port 15432: `select
current_user` returned the IAM user and the server saw the connection from the
user's own (allowlisted) address.

## Out of scope

Credentials for anything but RDS IAM login (another database, an AWS API); a
per-node allowlist of which databases a node may use; approval prompts per
login; a UI for `[[database]]`; databases for a node that runs on this
machine with its files in a sandbox (use the database directly there); a
Windows sandbox.
