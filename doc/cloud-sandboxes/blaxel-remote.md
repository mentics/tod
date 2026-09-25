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

Any image works, in one of two ways, both automated by `tod-sandbox` (setup
and use: [setup.md](setup.md)):

- **Install on first connect** (`tod-sandbox create <name> --image <image>`).
  Blaxel's own images (`blaxel/...`) and images built in the workspace
  (`sandbox/...`) run as they are. Any other image (Docker Hub, a private
  registry) is first wrapped by a three-line Dockerfile that adds Blaxel's
  `sandbox-api` as the entrypoint. Blaxel builds it remotely with `bl push`
  (about 16 s, no local Docker). Blaxel's "metamorph" import of a registry
  image alone is not enough: the image's own `CMD` runs and `sandbox-api`
  never starts. On the first connect, `tod-sandbox` uploads
  `assets/sandbox/bootstrap.sh` and the relay, runs the script (it installs
  what is missing with apk, apt-get, dnf, microdnf, or yum), and starts the
  relay.
- **Baked** (`tod-sandbox bake <base-image>`). The same script runs at build
  time and the relay is copied in, so a new sandbox only has to start the
  relay.

What a ready sandbox has:
- `tod-relay` at `/opt/tod/tod-relay`, started through the process API as
  `tod-relay`, restarted on failure. It runs with no `keepAlive`, so it does not
  block standby. Port 2222 is declared at creation, next to 8080
  (`sandbox-api`).
- `git`, `curl` (Zed downloads its remote server with it), `tar`/`gzip`,
  `bash`, `ps`, `scp`, and `sftp-server`, linked as `/opt/tod/bin/sftp-server`
  wherever the distribution keeps it. Zed uploads extensions with `scp`, which
  speaks SFTP.
- With `--agents`: Node.js 18+ and `@zed-industries/claude-code-acp`.
- `/opt/tod/manifest`: hashes of the bootstrap script and the relay, and
  whether agents were asked for. Connecting checks it in one relay round trip
  (0.3 s) and reprovisions only when it differs, for example after a tod
  update. A baked image carries the same manifest.

Measured end to end with `tod-sandbox create`:

| Image | Ready in |
|---|---|
| `blaxel/base-image` (Alpine; has everything already) | 7.5 s |
| `docker.io/library/ubuntu:24.04` with `--agents` (wrap 16 s, deploy 3.5 s, install 43 s) | 70 s |
| That Ubuntu, baked (bake: 57 s, once) | 10 s |

### 2. The relay (in the sandbox)

`crates/tod-relay`, a static Linux binary (musl, linked with `rust-lld`, so it
cross-builds from Windows or macOS). The wire protocol is in
[relay-protocol.md](relay-protocol.md). In short, it is one WebSocket server,
routed by path:

- **`/exec`** runs a command, or a terminal on a pty. With a session id, the
  process outlives the socket and its output is held (up to 32 MB) for the
  next attach. Zed's proxy and terminals use this.
- **`/agent/<name>`** attaches to one long-lived, line-oriented agent process
  (ACP). A new attach replaces the old one, and output is held while detached.
- **`/tunnel`** carries connections made to `127.0.0.1:2223` in the sandbox
  out to the client. `tod-cli` in the sandbox uses it to reach the app.

It sets `HOME` and the working directory itself, because the process API's
environment has `HOME=/blaxel`. The port proxy was verified to require the
Blaxel token for every request (401 without it), so the relay has no auth of
its own.

**Holding the sandbox awake.** A process started through the sandbox's local
API with `keepAlive` disables standby while it runs; this was verified, and
with no connection open. The relay runs one `sleep` that way while there is
work no client is watching:
- a terminal with a foreground job,
- a command started with `keep_awake`,
- an agent that owes its client an answer.

It kills that process when the last one ends. `--max-hold-secs` (default 4 h)
caps any one hold.

### 3. tod's transport (`tod-agent`)

*As built:* the app does not open WebSockets itself. A node whose Files
directory is in a sandbox has `Workdir::Sandbox` (git and worktrees run
through `tod-sandbox exec`) and launches with `AgentEnvironment::Sandbox`
(`tod_agent::sandbox::SandboxLaunch`). The ACP host spawns `tod-sandbox agent
<sandbox> --name acp-<pid>-<n> -- <adapter>` as the agent's process, and
that command bridges its stdio to `/agent/<name>`:

- It opens `/tunnel` first and waits for it, so the agent's first `tod-cli`
  finds it. The tunnel carries `tod-cli` to the app's `fleet::cli_relay`
  listener. The relay token and port are set on the local process and passed
  into the sandbox by name (`--env NAME`), so they never appear on a command
  line.
- While attached, it keeps a file fresh in `<data root>/zed-shim/awake/<sandbox>/`,
  which keeps a Zed on the sandbox attached for the turn. See below.
- It detaches after `--idle` seconds (default 30) with nothing owed in either
  direction. The agent keeps running in the sandbox, and the next turn
  reattaches.

`tod-store` builds the launch (`fleet::dev_container::sandbox_launch_for`),
next to the dev-container one. The notes below are the original design.

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
   `TOD_ZED_PARK_SECS` (default 30 s; 20 s in testing) with no real message in either
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
anyway. The flag is a directory per sandbox, `<data root>/zed-shim/awake/<name>/`:
each agent bridge keeps a file of its own there fresh (every 30 s) while it
is attached. The shim counts the sandbox as held while any file there changed
in the last 120 s, so a bridge that crashed stops counting on its own. Once
none are fresh, the shim parks again after the idle time.

**Zed's settings.** The host entry needs no ProxyCommand or key options. With
none, nothing Zed runs can reach the sandbox except through the shims, and a
path that bypasses them fails at once instead of holding a connection open.

**Version.** The shims depend on how Zed talks to its server: the master
marker, the proxy command line, the envelope field numbers, the ping/ack
semantics. tod should pin the Zed version it supports and carry a test that
starts a real Zed through the shim and checks connect, park, and reattach.

### 5. Terminals

A terminal is an `/exec` session on a pty. That covers Zed's terminals (`ssh
-t` through the shim) and `tod-sandbox shell`, which share
`tod_sandbox::terminal`. After 60 s with no input, no output, and no foreground
job, the client closes the socket and the sandbox can sleep. The shell stays
where it was, and the next keypress reattaches (a wake plus a reattach: about
0.5–0.8 s cold). While a foreground job runs, such as a build, a test run, or a
dev server, the terminal stays attached and the relay holds the sandbox
awake, so the job never freezes mid-way. `TOD_TERMINAL_PARK_SECS` sets the
idle time; 0 never parks.

tod's own shells for a sandbox node run `tod-sandbox shell <sandbox> --cwd
<dir> --cli-relay-file <file>`. The app writes the relay environment to a
file under `<data root>/sandbox-relay/`, which the command reads and deletes,
so the token never goes on a command line. The shell then carries the tunnel
too, and `tod-cli` works in it.

The catch: a dev server left running in a terminal holds the sandbox awake
until it is stopped or the hold cap (4 h) ends. That is what a running server
needs, but it costs until then.

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

- **Zed internals.** Covered under Version above. It is the biggest risk. The
  relay has no SSH fallback, so a Zed release that changes these internals
  breaks remote editing until the shim is updated.
- **Only Windows tested.** On macOS and Linux, Zed uses SSH ControlMaster
  (`-M`, `-S`, `-O` options), a different path from the Windows master. The
  shim answers it locally: it creates a placeholder at the control path and
  answers `-O` requests itself. That path is untested.
- **A Zed not started by tod.** tod assumes it is the only thing that starts
  Zed, because the shim reaches Zed only through `PATH`. Windows Zed is
  single-instance, so if Zed is already open, a launch from tod is handed to
  that instance, which has no shim. `tod-sandbox zed` notices that no
  connection came through the shim within 20 s and says to quit Zed first.
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
