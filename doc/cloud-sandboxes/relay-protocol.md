# tod-relay wire protocol

`tod-relay` (`crates/tod-relay`) is the one server tod runs inside a cloud
sandbox. It listens for WebSockets on port 2222, which the sandbox declares, so
clients reach it at `wss://<sandbox-url>/port/2222/<path>`. The provider's port
proxy authenticates every request (`Authorization: Bearer <token>`), so the
relay has no authentication of its own. The client side is
`tod_sandbox::relay` (`crates/tod-sandbox`).

The relay routes by the path's segments, since the proxy may or may not keep
its `/port/2222` prefix: a path with an `agent` segment goes to
[`/agent/<name>`](#agentname), one with an `exec` segment goes to
[`/exec`](#exec), and one with a `tunnel` segment goes to
[`/tunnel`](#tunnel).

## `/exec`

This runs a command or a terminal with its stdio carried over the socket.

**The first frame** is text: a JSON request.

| Field | Meaning |
|---|---|
| `cmd` | Run with `/bin/sh -c`. With `pty` and no `cmd`, run a login shell (`bash -l`, else `sh -l`). |
| `session` | An id. A session with an id outlives the socket (see below). Sending only the id reattaches to it. |
| `pty` | `[cols, rows]`: run the command on a pseudo-terminal (`TERM=xterm-256color`). |
| `env` | Extra environment. `HOME` is always the user's real home (the provider starts processes with `HOME=/blaxel`). |
| `cwd` | Working directory; defaults to the home directory. |
| `keep_awake` | Hold the sandbox awake while the command runs. |

**From the client:**

| Frame | Meaning |
|---|---|
| binary | stdin bytes |
| text `eof` | Close stdin. On a pty this sends ^D. |
| text `bye` | Kill the process group. |
| text `r<cols>x<rows>` | Resize the pty. |

**From the relay:**

| Frame | Meaning |
|---|---|
| binary | stdout. On a pty, this is all output. |
| text `e<text>` | stderr. The relay's own errors also arrive this way. |
| text `b1` / `b0` | A pty's foreground job started or ended. |
| text `x<code>` | The process exited. A Close follows. |

**Sessions.** Without an id, closing the socket kills the process. With one,
the process keeps running after the socket closes, and its output is held for
the next attach. A new attach replaces the old one, which gets Close code
4001. If more than 32 MB is held, the relay ends the process, because the
client could not recover a stream with a gap in it. Once a session's exit has
been delivered, the session is gone. Reattaching after that gets `e` with "no
such session" and then `x255`.

A non-pty process runs in its own process group, and `bye` kills the group.

## `/agent/<name>`

This attaches to one long-lived, line-oriented agent process (JSON-RPC, such as
ACP).

- The first frame is text, `{"cmd": ..., "env"?: ..., "cwd"?: ...,
  "attach_only"?: bool, "replace"?: bool}`. It starts the agent if none by
  that name is running; otherwise it attaches and the rest is ignored.
  - `attach_only` never starts one. With no agent by that name (or an empty
    `cmd`), the socket gets Close code 4004 "no such agent".
  - `replace` ends an agent already running under the name (its client gets
    Close code 4001) and starts afresh.
- After that, each text frame is one line, in either direction. From the
  client, the text `bye` kills the agent's process group (a JSON line cannot
  be `bye`).
- Lines the agent writes while no client is attached are held and replayed on
  the next attach, with a 32 MB cap after which the agent is ended.
- A new attach replaces the old one (Close code 4001).
- The agent's stderr goes to `/opt/tod/logs/agent-<name>.log`.
- When the agent exits, the attached client gets Close code 4000 with the
  reason `exit:<code>`.

## `/tunnel`

This carries connections made in the sandbox to `127.0.0.1:2223`
(`--tunnel-port`) out to the client, which connects each one to a port on its
own machine. It is how `tod-cli` in a sandbox reaches the tod app: the shim at
`/opt/tod/bin/tod-cli` connects to the tunnel port, and the client carries the
connection to the app's `tod-cli` relay, which checks the token and runs the
command against the app's data root.

- The client's first frame is text (`{}`), as on every socket.
- The relay sends text `o<id>` when a connection arrives. `id` is a number.
- Binary frames carry data both ways: a 4-byte big-endian `id`, then the
  bytes.
- Text `c<id>`, either way, says the sender has nothing more to write on that
  stream.
- New connections go to the most recently attached client. A connection that
  arrives while no client is attached is closed at once, and a client's
  streams end when it detaches.

## Holding the sandbox awake

The provider freezes a sandbox about 15 s after its last proxied connection
closes, whatever is running inside it. The relay starts one process through
the provider's local API (`127.0.0.1:8080/process` with `keepAlive`), which
disables standby. It keeps that process while any of these reasons hold:

- **`busy:<session>`:** a pty session has a foreground job (a build, a test
  run, a dev server). An idle prompt does not count.
- **`awake:<session>`:** an `/exec` command was started with `keep_awake`.
- **`agent:<name>`:** the agent owes the client an answer (a request it has
  not responded to), unless it is itself waiting on a client request while no
  client is attached. In that case it cannot make progress anyway.

When the last reason ends, the relay kills that process and the sandbox can
sleep. `--max-hold-secs` (default 4 h) bounds any one hold, so a forgotten dev
server cannot keep a sandbox running forever.

## Running it

```sh
tod-relay --port 2222 --tunnel-port 2223 --max-hold-secs 14400
tod-relay --version
```

`tod-sandbox` starts it through the provider's process API (as the process
`tod-relay`, restarted on failure) and installs it at `/opt/tod/tod-relay`.
`/opt/tod/manifest` records the relay and bootstrap-script hashes that the
sandbox was provisioned with (see `tod_sandbox::provision`).
