# Blaxel sandboxes: reaching them from tod and Zed

Status: design, backed by a working prototype (September 2026). Nothing here is
in tod yet. The prototype lives in the git-ignored
`.local/agent/scratchpad/blaxel-spike/` of the `journeys-functionality-12b84f`
worktree; see [Prototype](#prototype).

## Goal and constraints

tod runs on the user's machine, possibly for days. Each node's work runs in a
Blaxel sandbox: an agent (over ACP), commands tod runs there, and the user's
editor (Zed remote development). The sandbox must go to standby after about
15 seconds of idle, and anything tod or Zed does must wake it quickly and
without errors.

What Blaxel does, measured:

- **Standby starts about 13.5 s after the last connection through Blaxel's
  proxy closes.** The sandbox clock runs 3.9 s ahead of the laptop, so raw
  logs read about 17.5 s.
- **Any open connection through the proxy keeps the sandbox awake**, even
  with no traffic: an idle WebSocket and an idle SSH connection each held it
  awake for the full 90 s tested. Blaxel's docs say an idle connection keeps
  it awake for up to 15 minutes.
- **Nothing else keeps it awake.** Processes running in the sandbox (even a
  busy loop writing every second) are frozen with it; background processes
  started with the process API without `keepAlive` do not block standby.
- **Control-plane calls do not wake it** (`GET api.blaxel.ai/v0/sandboxes/<name>`).
- **Any request to the sandbox's URL wakes it.** Everything in it resumes where
  it was: processes, open files, a paused Zed server.
- **Only declared ports are reachable** (`https://<sandbox-url>/port/<n>/...`);
  an undeclared port returns 502. Ports are declared when the sandbox is
  created.

So the one rule that makes standby work: **when nothing is happening, tod holds
no connection to the sandbox.** Every design choice below follows from it.

## The approach

All traffic goes over WebSockets through Blaxel's own port proxy, to one small
server in the sandbox (the *relay*). Connections are opened when there is work
and closed when there is not. A process that has to survive a closed connection
(the agent, Zed's server) keeps running in the sandbox, and the relay holds its
output until the next connection.

```
laptop                                         sandbox
───────────────────────────────                ──────────────────────────────────
tod ── ACP over WebSocket ──────┐              relay (port 2222)
tod ── commands (exec) ─────────┤  wss://…     ├─ /agent  → the ACP adapter (one, persistent)
Zed ── ssh shim ── exec/proxy ──┼─ /port/2222 ─├─ /exec   → sh -c <cmd>; stdio over the socket
Zed ── scp/sftp shim ───────────┘              │           (sessions outlive the socket)
                                               └─ /ssh    → sshd (optional, see Terminals)
tod ── Blaxel API (create, status, process API) ── sandbox-api (port 8080)
```

The other options, and why not, are under [Alternatives considered](#alternatives-considered).

## The parts

### 1. Sandbox image

- `relay` listening on 0.0.0.0:2222, started at boot through the process API
  (no `keepAlive`, so it does not block standby). Port 2222 declared at
  creation, next to 8080 (`sandbox-api`).
- The agent adapter (today `@zed-industries/claude-code-acp`), started by the
  relay.
- `sftp-server` (`/usr/lib/ssh/sftp-server`, Alpine `openssh-sftp-server`): Zed
  uploads extensions with `scp`, which speaks SFTP.
- Recommended: Zed's remote server for the pinned Zed version, preinstalled in
  `~/.zed_server`, to skip a download on first connect (about 1.8 s measured).
- `sshd` only if tod offers interactive terminals over SSH (see Terminals).

The prototype used Alpine 3.21 with `openssh-server websocat git bash curl`,
Node, and `ws`. A production relay should be a single static binary (Rust),
so the image needs no Node for it.

### 2. The relay (in the sandbox)

One WebSocket server, routed by path. Every frame format is simple enough to
implement on both sides in a few hundred lines.

**`/exec`**: run one command.
- The first message is text JSON `{"cmd": "...", "session": "..."?}`.
- The relay spawns `sh -c cmd` with `HOME=/root`, cwd `/root`. It has to set
  these itself: the process API's environment has `HOME=/blaxel`, and a command
  started through it inherits that.
- Binary frames in are stdin; text `eof` closes stdin; text `bye` kills the
  process.
- Binary frames out are stdout, text frames out are stderr, and the last frame
  is text `exit:N`.
- **Without a session id**, closing the socket kills the process.
- **With a session id**, the process outlives the socket. Output while no
  socket is attached is buffered, and replayed when a socket attaches with the
  same id. Zed's proxy uses this.

**`/agent`** (the prototype's `/acp`): attach to the one persistent ACP adapter.
- One client at a time. A new attach replaces the old one.
- Messages are newline-delimited JSON-RPC, one per text frame.
- Output while detached is buffered by line and replayed on attach, so a turn's
  late notifications are never lost.

**`/ssh`** (optional): raw bytes to `sshd` on 127.0.0.1:22.

Production needs, not in the prototype:
- A bound on buffered output per session, and a way to discard a session nobody
  will come back for (Zed was killed, not closed).
- More than one agent session per sandbox, if a node ever runs two agents.
- Its own auth check if the port proxy turns out not to require the Blaxel
  token for every request (not verified; the prototype always sent it).

### 3. tod's transport (`tod-agent`)

- A WebSocket client (`tokio-tungstenite` with rustls; the ring provider must be
  installed explicitly, or rustls panics on first use).
- The Blaxel bearer token in the `Authorization` header of every connection.
  tod keeps it with its other credentials and refreshes it before it expires.
  A token taken before an hour of idle may already be stale at wake.
- **Agent turns**: open `/agent`, send the turn, keep the socket open until the
  turn ends, then close it. Starting a session (`session/new`) takes about 2 s,
  so sessions are created once and reused across wakes.
- **Commands** (git, tests, `tod-cli` work in the sandbox): `/exec` without a
  session, about 115–160 ms warm, versus 450–1100 ms for a new `ssh.exe`.
  Either this or the process API; `/exec` streams stdin/stdout, and the process
  API is 70–175 ms warm and needs no relay.
- **The idle rule**: no connection is left open once the work is done. This
  includes pooled HTTP keep-alive connections (the prototype's process-API
  calls send `Connection: close`) and child processes: a killed `ssh.exe` left
  its ProxyCommand running, which kept a connection, and the sandbox, open.

### 4. Zed

Zed's remote development runs a server of its own on the remote
(`~/.zed_server/zed-remote-server-<version>`): it scans the project, watches
files, runs git and language servers. The local Zed is the UI, and keeps a
synced copy of the state by applying the server's messages in order. It never
rescans on reconnect; a message it misses is simply missing. SSH is only a pipe
to that server: Zed runs `ssh host "zed-remote-server proxy --identifier X"`,
and the proxy relays stdin/stdout to a long-lived server process.

Left alone, Zed breaks the idle rule: its connection stays open, so the sandbox
never sleeps while Zed is open, and after standby it reconnects with about six
separate SSH connections (about 5 s). tod fixes that by launching Zed with a
directory of its own first on `PATH`, holding three small programs Zed calls by
name (there is no setting for a different `ssh` program):

**`ssh` shim.** For a host tod manages, it never runs SSH. It reads the
destination and command from Zed's arguments (options come both before and
after the destination) and handles four cases:

1. **Zed's Windows "master" connection.** Its command contains
   `ZED_SSH_CONNECTION_ESTABLISHED`. The shim prints that marker and stays
   alive, holding no connection. On Windows, Zed has no ControlMaster and
   uses this process only to know the host is reachable.
2. **One-off commands** (`uname`, `cat /etc/os-release`, the server version
   check, the server download): these become `/exec` without a session, with
   the remote exit code passed back.
3. **The proxy** (`... proxy --identifier X [--reconnect]`): this becomes
   `/exec` with a session id. The shim then *parks* it: after
   `ZSHIM_PARK_SECS` (20 s in testing) with no real message in either
   direction, it closes the WebSocket. Zed's pipe stays open, and Zed never
   learns it was disconnected. The first real message from Zed reattaches
   (about 90–130 ms warm), and the relay replays anything the server sent
   meanwhile.
4. **`-s sftp`**: this becomes `/exec` of `sftp-server`.

Hosts tod does not manage pass through to the real `ssh.exe`.

**Keepalive pings.** Zed pings after 5 s without traffic and gives up after 5
missed pings. The shim answers them itself while parked, with an `Ack` whose id
is the last real server message id: Zed sets its received-counter from the
incoming id rather than taking a maximum, so any other id corrupts its
bookkeeping. In practice Zed sent no pings at all while parked, over parks of
up to 3 minutes, and the code is there for safety.

Zed's messages are framed as a u32 little-endian length plus a protobuf
`Envelope`. The shim decodes only `id` (1), `responding_to` (2), and the payload
field number (`Ack` = 5, `Ping` = 7), skipping 3 and 266.

**`scp` and `sftp` shims.** These run the real `scp.exe`/`sftp.exe` with `-S`
pointing at the `ssh` shim, so an upload goes over `/exec` like everything else.
Without them, Zed's extension upload opened its own connection. That upload
hung for 2 minutes, retried, and held the sandbox awake throughout.

**The "sandbox busy" hold.** While the proxy is parked, messages the server
starts on its own (a file an agent changed, new diagnostics) wait in the relay
until Zed next sends something. So while tod is running an agent turn in the
sandbox, it tells the shim to stay attached; the sandbox is awake for the turn
anyway. The prototype uses a flag file (`awake.flag` beside the shim). tod
should use something per sandbox, such as a local socket or a file per host.
When the hold is cleared, the shim parks again after the idle time.

**Zed's settings.** The host entry needs no ProxyCommand or key options. With
none, nothing Zed runs can reach the sandbox except through the shims, and a
path that bypasses them fails at once instead of holding a connection open.

**Version.** The shims depend on how Zed talks to its server: the master
marker, the proxy command line, the envelope field numbers, the ping/ack
semantics. tod should pin the Zed version it supports and carry a test that
starts a real Zed through the shim and checks connect, park, and reattach.

### 5. Terminals

Not yet designed. `/exec` has no pty. The options are SSH over `/ssh` (the
prototype's `wsbridge` ProxyCommand) or a pty mode on `/exec`. An open terminal
is an open connection, so it keeps the sandbox awake; that is probably what a
user with a terminal open expects, but it counts against the idle rule.

## Expected performance

Laptop in the US; sandbox in `us-pdx-1`, 2 cores, 4 GB (the free-plan cap).
"Cold" means the sandbox was confirmed in standby: a loop in the sandbox writing
the time every second shows a gap covering the request.

### Waking from standby

Time from the request to its complete answer, sandbox asleep:

| Idle before | ACP: attach + one round trip | Process API: `echo ok` | New `ssh.exe` command over WebSocket |
|---|---|---|---|
| 20 s | 397, 422 ms | 217, 282 ms | 809, 744 ms |
| 60 s | 431, 440 ms | 270, 281 ms | 708, 781 ms |
| 5 min | 745 ms (open 686 ms) | 269 ms | 745, 748 ms |
| 20 min | 491 ms | 258 ms | — |
| 60 min | 475 ms (open 367 ms) | — | — |

Warm (just used), for comparison:

- ACP round trip: 170–350 ms, with the WebSocket open taking 120–300 ms of that.
- Process API: 70–175 ms.
- New `ssh.exe` command: 430–1100 ms.
- Shim `/exec`: 113–160 ms.

Waking adds roughly 150–450 ms to the first request, with no trend by sleep
length up to the 60 minutes tested.

### Zed

| | Measured |
|---|---|
| First connect, server download included | about 3.7 s |
| Connect, server already installed | about 2 s (six one-off commands at about 120 ms each, then the proxy) |
| Park to standby, Zed open | standby 14 s after parking; confirmed repeatedly |
| Open a file from the CLI with the sandbox asleep | request on its way 0.7 s later (the cold one-off command took 340 ms, the proxy reattach 91 ms) |
| Proxy reattach, sandbox awake | 90–130 ms |
| Upload through the `scp` shim | 1.3 s for a small file |
| Before the shims (plain SSH over WebSocket) | 9 s first connect; about 5 s to reconnect after standby; sandbox never slept while Zed was open |

### Other

- Round trip on an open SSH channel through the relay: median 57 ms, p90 126 ms.
- Throughput through the port proxy: 42 MB/s raw WebSocket. SSH through a
  ProxyCommand bridge is capped at about 5.7 MB/s down and 1 MB/s up by Windows
  `ssh.exe`. Zed's main connection does not use `ssh.exe` with the shims, so the
  cap only applies to `scp` uploads.
- ACP: attach and `initialize` in 190 ms; `session/new` about 2 s.

## Risks and open questions

- **Zed internals.** Covered under Version above. It is the biggest risk, and it
  is contained: if the shim breaks, Zed still works through plain SSH over
  `/ssh`; the sandbox just stays awake while Zed is open.
- **Only Windows tested.** On macOS and Linux, Zed uses SSH ControlMaster
  (`-M`, `-S`, `-O` options), a different path from the Windows master. The
  shim needs to handle it, and that is untested.
- **A simpler relay, untested.** Zed's own server already keeps messages the
  client has not acknowledged, and resends them when a proxy connects with
  `--reconnect`. So parking could end the proxy process, and reattaching could
  start a new one with `--reconnect`. The relay would then need no sessions or
  buffering for Zed. What needs checking is whether the resend fits Zed's
  acknowledgement bookkeeping when the client never saw a disconnect.
- **Long parks.** Zed stayed healthy through parks of 2–3 minutes. Hours are
  untested.
- **Language servers.** rust-analyzer on a repository this size needs more than
  the free plan's 4 GB and 3.1 GB of disk; untested. Larger sandboxes need a
  paid plan.
- **One unexplained reading.** An early 5-minute cycle found the sandbox awake
  at wake. A rerun slept normally. The leftover `ssh.exe` ProxyCommand, found
  later, is the likely cause but is not confirmed.
- **Several Zed windows, or several sandboxes per window.** Untested; each has
  its own proxy and session id, so it should work.

## Alternatives considered

- **Tailscale** (the "Blaxel wakeup time - tailscale" session). An SSH
  connection over the tailnet survives standby without keeping the sandbox
  awake, and a command on it takes about 25 ms. But:
  - Tailscale traffic cannot wake the sandbox. Waking needs a Blaxel API
    call, so Zed would still need a shim, one that wakes the sandbox before
    forwarding.
  - Recovery after a wake measured 0.3–5 s. Getting it that low took three
    workarounds that depend on Tailscale's internals, and in 2 of 4 cycles the
    connection never opened.
  - Since only proxied connections keep the sandbox awake, an agent working
    only over the tailnet would likely be frozen about 15 s into its turn
    (inferred, not tested). tod would need a Blaxel connection during turns
    anyway.
  - Every teammate would have to join the tailnet, and each sandbox would add
    a device.
- **Plain SSH over WebSocket for everything.** Simple, but Zed holds its
  connection open, so the sandbox never sleeps with Zed open. It is still
  useful for terminals and as the fallback if the shim breaks.
- **Process API only.** Fast (about 270 ms cold) and needs nothing in the
  image, but it cannot carry a long-lived bidirectional stream the way ACP and
  Zed's proxy need. Fine for one-off commands.

## Prototype

In `.local/agent/scratchpad/blaxel-spike/` (git-ignored) of the
`journeys-functionality-12b84f` worktree:

- `bx.py`: the Blaxel API helper (create with declared ports, status, run,
  start, delete).
- `setup.sh`: the sandbox's image setup.
- `relay.js`: the relay (Node; `/acp`, `/exec`, SSH passthrough, plus a `/blob`
  throughput test).
- `wsbridge/`: Rust SSH ProxyCommand over the WebSocket.
- `zedshim/`: Rust `ssh`, `scp`, and `sftp` shims (`sandboxes.txt` maps a host
  name to a relay URL; `bl_token.txt` holds the token).
- `timing.py`: the standby and wake measurements (results in `timing.jsonl`).
- `gapcheck.py`: reads the sandbox's once-a-second timestamp file to find
  standby gaps. It waits 2 s before reading. The request that wakes the sandbox
  runs before the timestamp loop writes again, so reading immediately misses
  the gap in progress.
- `zed-data/`: the test Zed profile (its `settings.json` holds the host entry).
- `acpclient.py`, `rtt.py`, `wsblob.py`: ACP, latency, and throughput probes.
