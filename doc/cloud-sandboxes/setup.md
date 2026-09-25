# Cloud sandboxes: setup and use

A cloud sandbox is a Linux machine on Blaxel that sleeps when nothing uses it
and wakes in under a second. tod works in it through `tod-sandbox`, and Zed
edits in it. How it works: [blaxel-remote.md](blaxel-remote.md).

## Install

From a clone of the repository:

```powershell
scripts\install.ps1 -TargetDir C:\tools\tod -SandboxWorkspace <workspace>
```

```sh
scripts/install.sh ~/tools/tod --sandbox-workspace <workspace>
```

Besides `tod` and `tod-cli`, this installs:
- `tod-sandbox`
- `tod-zed-shim`
- `sandbox/tod-relay`, a Linux binary. The script cross-builds it after
  `rustup target add x86_64-unknown-linux-musl`, so no Linux toolchain is
  needed.

If building the relay fails, tod is still installed, without cloud sandboxes.

`-SandboxWorkspace` / `--sandbox-workspace` also runs `tod-sandbox setup`. That
needs a data root: if tod has not been run yet, run it once, then run
`tod-sandbox setup --workspace <workspace>` yourself. `tod-sandbox` reads the
data root the same way tod does (`--data-root`, `TOD_DATA_ROOT`, then
install.toml) and never writes install.toml. `-NoSandbox` / `--no-sandbox`
skips all of this.

## The Blaxel account

`tod-sandbox setup` records the workspace, and how to sign in to it, in
`<data root>/sandboxes.toml`. There are two ways to sign in:

- **`bl login` (the default when the Blaxel CLI is installed).** Each person
  signs in as themselves. tod asks `bl` for a token and caches it until
  shortly before it expires. If you are not signed in, `setup` runs
  `bl login <workspace>` for you.
- **An API key** (`--auth api-key`), for machines without a person at them.
  The key goes into tod's credential store (the OS keyring, else an encrypted
  file) as a kind that agents cannot read: `tod-cli secrets` never lists or
  returns it.

**For a team**, use one Blaxel workspace and invite each teammate to it. Each
person runs `tod-sandbox setup --workspace <team-workspace>` and signs in with
their own `bl login`. There is no shared secret to hand out or rotate, access
is removed by removing the person from the workspace, and the workspace's logs
show who did what. Every sandbox is labeled with its creator (`tod-owner`,
from `--owner`, which defaults to your OS user name), so `tod-sandbox list` shows
whose sandbox is whose. Keep API keys for automation.

Building images (a wrapped or baked image, below) also needs the Blaxel CLI.
With an API key, `tod-sandbox` passes the key to `bl`.

**In the app**, Settings → **Cloud sandboxes** sets the same two things
`sandboxes.toml` holds for everyone on this data root: the **Blaxel
workspace** and the **default image** new sandboxes start from. Setting a
workspace there signs in with `bl login` (run it once yourself); a workspace
already set up with `tod-sandbox setup` keeps how it signs in. An empty
workspace turns cloud sandboxes off, and an empty image means Blaxel's base
image.

## Sandboxes

```sh
tod-sandbox create dev                                   # Blaxel's base image (Alpine)
tod-sandbox create dev2 --image ubuntu:24.04 --agents    # any image, plus Node and the agent adapter
tod-sandbox fork dev2 dev2-copy                          # a copy of dev2 as it is now
tod-sandbox zed dev /root/project                        # open it in Zed
tod-sandbox shell dev                                    # a terminal
tod-sandbox exec dev -- git -C /root/project status      # one command
tod-sandbox list | status dev | delete dev | doctor
```

**Any image works (install on first connect).** Blaxel's own images
(`blaxel/...`) and ones built in the workspace (`sandbox/...`) are used as
they are. Any other image, from Docker Hub or a private registry, is wrapped
first: Blaxel builds a copy with its sandbox API added, in about 15 s and with
no local Docker. The first connect then installs what tod needs (git, curl,
scp/sftp-server; with `--agents`, Node.js 20.10+, the Claude Code ACP adapter,
and Claude Code itself), using whichever of apk, apt-get, dnf, microdnf, or
yum the image has. A distribution's Node.js that is older than 20.10 is left
alone, and the official Node.js 22 build goes in `/opt/tod/node` ahead of it
on `PATH`. On Alpine, only Alpine's own Node.js runs, so an old one there is an
error.

**Baked images, for fast creation.**

```sh
tod-sandbox bake ubuntu:24.04 --agents          # builds sandbox/tod-baked-ubuntu-24-04
tod-sandbox create dev3 --image sandbox/tod-baked-ubuntu-24-04:latest --agents
```

A baked image already has everything installed, so a sandbox created from it
is ready in about 10 s. `tod-sandbox setup --image <image>` makes an image the
default for `create`.

**Forks.** `tod-sandbox fork <source> <name>` makes a new sandbox from
another one's current state, files and installed tools included, even while
the source is in standby. Not every Blaxel workspace has forking; one without
it refuses with "fork feature is not enabled for this workspace".

**Updates.** Whenever `tod-sandbox` connects, it checks the sandbox's manifest
of what was installed (one round trip, 0.3 s). After a tod update it
reinstalls what changed in place, so a rebake is optional.

## A node's work in a sandbox

In the task editor, a node's **Files** section has **Runs in**, which cycles
through *This machine*, *Dev container*, and *Cloud sandbox*. Choose *Cloud
sandbox*, pick a sandbox from the list (every sandbox in the workspace, with
its status, image, and owner), and give the workspace directory as a path in
the sandbox (`/root/app`).

To make a new one there, use **New sandbox** below the list. **Start from**
chooses between *An image* (a cold start; empty uses the default image from
Settings) and *A fork of a sandbox* (pick which one; see **Forks** above).
The name defaults to the node's slug. **Create and use it** makes the
sandbox, installs what agents need, and selects it for the node; a baked
image takes seconds, any other image a minute or more. tod checks the
directory there, which wakes the sandbox. An agent can set the same thing:

```sh
tod-cli capabilities set <node> files --sandbox <name> --dir /root/app
tod-cli capabilities set <node> files --sandbox ''     # back to this machine
```

**Sign in to Claude in the sandbox once**, before the first turn:

```sh
tod-sandbox shell <name>
claude /login
```

tod never copies credentials into a sandbox.

From then on, the node runs in the sandbox:
- **Git and worktrees** run in the sandbox.
- **Agent turns** (conversations, Implement, Verify, Review, Fix) start the
  agent there through `tod-sandbox agent`. The sandbox stays awake while a
  turn is running, and so does any Zed open on it. Between turns, the agent
  process stays in the sandbox and the sandbox sleeps.
- **`tod-cli`** in the sandbox (`/opt/tod/bin/tod-cli`) reaches the app's data
  through the relay's tunnel. It works only while the app has the sandbox
  attached, which is always the case during a turn.
- **Shells** (the action panel's *New shell*) open `tod-sandbox shell` in the
  node's directory, with `tod-cli` on `PATH`.
- **Open in Zed** opens `ssh://root@<name>.tod/<dir>`. Only Zed can open a
  sandbox directory.

If an agent fails to start, `tod-sandbox` names its log in the sandbox
(`/opt/tod/logs/agent-<name>.log`).

## Zed

`tod-sandbox zed <name> [path]` opens `ssh://root@<name>.tod/<path>` in Zed,
started with the shim first on its `PATH`. tod does the same whenever it
starts Zed. Hosts ending in `.tod` go to sandboxes; every other host goes to
the real `ssh`, so Zed's ordinary SSH remotes are unaffected.

While Zed is open and idle, its connection is parked and the sandbox sleeps.
The first action after a sleep wakes it in about 0.7 s. Zed's terminals are
parked the same way when they are idle, and stay attached while a command
runs in them.

**tod assumes it is the only thing that starts Zed.** Windows Zed is
single-instance, so a Zed that was already open (started from the Start menu,
say) receives the request without the shim. `tod-sandbox zed` notices and
says to quit Zed and try again.

## Where things live

| What | Where |
|---|---|
| Account and sandboxes | `<data root>/sandboxes.toml` |
| API key, if used | tod's credential store (`blaxel_api_key`) |
| Cached `bl` token | `<data root>/sandbox-token.json` |
| Zed's `ssh`/`scp`/`sftp`, and their log | `<data root>/zed-shim/` (`shim.log`) |
| Which sandboxes a turn is holding awake | `<data root>/zed-shim/awake/<sandbox>/` |
| In the sandbox | `/opt/tod/` (relay, bootstrap script, manifest, `bin/sftp-server`, `bin/tod-cli`, `node/`, `logs/`) |
