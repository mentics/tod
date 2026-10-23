# Port forwarding: sandboxes reaching hosts only this machine can reach

An agent in a cloud sandbox sometimes needs one connection that only the
user's machine can make: a database host behind the company VPN, an
allowlisted address. This lets a sandbox listen on a loopback port whose
connections are opened **from the user's machine** to a configured
`host:port`, as if the user had made them. It is plain TCP forwarding and
nothing else: no commands run on the user's machine, ever.

## Shape

```
agent in sandbox ──► 127.0.0.1:5432 (relay /tunnel listener)
                        │  relay frames: o<id>:5432, data, c<id>
                        ▼
              tod-sandbox on the user's machine ──► db.internal:5432 (over the VPN)
```

It is the relay's existing `/tunnel` (which carries `tod-cli` to the app),
generalised from one loopback port to several.

## Configuration

`forwards` in the account of `sandboxes.toml`:

```toml
[blaxel]
forwards = ["5432=db.internal:5432", "15432=replica.internal:5432"]
```

Each entry is `[SANDBOX_PORT=]HOST:PORT`. With one number, both ends use it.
Sandbox ports below 1024 are refused. A malformed or repeated entry is an
error when a node starts, not skipped. The forwards apply to every sandbox
the app attaches to.

## Protocol (additions to `/tunnel`; see `relay-protocol.md`)

- Client → relay, after the first frame: text `f<port>` per forward.
- Relay → client: `l<port>` (listening) or `e<port>:<reason>` (refused: below
  1024, or the port is already taken in the sandbox). The client prints a
  refusal on stderr, once. If no answer comes in 10 s the relay is too old
  (it ignores `f`), and the client says so: rebuild the sandbox image.
- A connection to a forwarded port opens its stream as `o<id>:<port>`. The
  default tunnel port stays plain `o<id>`, so old clients and relays work
  as before for `tod-cli`.
- Data and close frames are unchanged.

## Security

- **The sandbox never names a target.** The relay sends only the sandbox-side
  port. The client dials a target only if that port is in its own
  configuration; any other port is closed at once. A compromised sandbox can
  reach only the hosts the user listed.
- Listeners are on `127.0.0.1` in the sandbox, so only processes there
  reach them.
- Nothing is forwarded unless the user configured it.

## When it works

A forward is carried by the same tunnel as `tod-cli`: **only while the app
has the sandbox attached** (an agent turn, an attached terminal). It does
not work for autonomous nodes running with the app closed (the orchestrator
design in `autonomous-nodes.md`), which have no path to this machine; a
connection then fails with "connection refused" in the sandbox, as any
closed port does. Listeners the relay opened stay open after the app
detaches; connections to them are closed immediately while no client is
attached.

## Using it with IAM database authentication

This is only a TCP pipe, so IAM auth works if the token is made correctly.

- Token generation is local signing of the host and port plus the IAM
  credentials; it does not contact the database. The sandbox's credentials
  (reaching the cloud provider over the internet) can do it.
- The token is for the **real** host and port. Give the client the real
  host name for the token and for TLS verification, and the loopback
  address only for the connection. With libpq:
  `host=db.internal hostaddr=127.0.0.1 port=5432 sslmode=verify-full`.
  Using the same port number in the sandbox as the database's keeps the token
  valid for databases that sign the port.
- Only that one connection crosses the VPN. Everything else (the token,
  other cloud APIs) goes over the internet from the sandbox as before.

## Out of scope

Running commands on the user's machine; UDP; forwarding to the sandbox from
the user's machine (the other direction); per-node or per-location
forwards (the account-wide list is the first step); a UI to edit the list.
