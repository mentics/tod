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

The `forwards` of `sandboxes.toml` are carried by the same tunnel as
`tod-cli`: **only while a local `tod-sandbox agent` or terminal has the
sandbox attached**. A node running in the cloud has no such process; for it,
the daemon opens a forward on demand, tied to a database login. See
[database-access.md](database-access.md). A connection with no tunnel fails
with "connection refused" in the sandbox, as any closed port does. Listeners the relay opened stay open after the app
detaches; connections to them are closed immediately while no client is
attached.

## Using it with IAM database authentication

This is only a TCP pipe, so IAM auth works if the token is made correctly,
and [database-access.md](database-access.md) does that for a node: it signs
the token on this machine and maps the database's name to loopback in the
sandbox. Verified against RDS Postgres. What matters, if you do it by hand:

- Token generation is local signing; it contacts nothing. Sign it for the
  server's **own name** (for RDS, the instance endpoint): a token signed for a
  CNAME alias is refused, and the TLS certificate is made for the endpoint
  name too.
- The client connects to that same name, which must resolve to `127.0.0.1`
  in the sandbox (`/etc/hosts`), at the forwarded port. The port need not
  match the database's: the token is signed for the real `host:port`, not
  the client's.
- Only that one connection crosses to the database from this machine.

## Out of scope

Running commands on the user's machine; UDP; forwarding to the sandbox from
the user's machine (the other direction); per-node or per-location
forwards (the account-wide list is the first step); a UI to edit the list.
