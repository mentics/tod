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

## Building sandbox binaries (`scripts/build-sandbox-binaries.sh`)

Everything that runs inside a sandbox — `tod-relay` today, `tod-supervisor`
and `tod-orchestrator` once those crates exist (autonomous nodes, waves 2–3)
— is a Linux binary for `x86_64-unknown-linux-musl`, built from whatever OS
you develop on (Windows, macOS, or Linux) rather than requiring a Linux
machine for day-to-day work.

```sh
scripts/build-sandbox-binaries.sh              # release build (default)
scripts/build-sandbox-binaries.sh --debug
```

On Windows, run it under Git Bash (`sh scripts/build-sandbox-binaries.sh` or
`bash scripts/build-sandbox-binaries.sh`) — there is no PowerShell twin; the
script is POSIX shell like `scripts/dev.sh` and `scripts/install.sh`, and
Git Bash is already assumed to be present for those.

It builds each sandbox binary that exists in the workspace today into
`target/sandbox/` (skipping the ones later waves add, so it keeps working as
`tod-supervisor` and `tod-orchestrator` land), and `tod-store`'s sandbox
provisioning (`relay_path()` in `crates/tod-store/src/fleet/sandbox.rs`)
looks there first, ahead of the raw per-target cargo output and the
installed `sandbox/tod-relay`.

Two cases:

- **No C dependencies** (`tod-relay` today): a plain `cargo build --target
  x86_64-unknown-linux-musl`. `rust-lld` (`.cargo/config.toml`) links it with
  no Linux toolchain needed, on any host.
- **Depends on `tod-store`** (`tod-supervisor`, `tod-orchestrator`, once they
  exist): pulls in bundled SQLite, which is C, so cross-linking needs a
  Linux C toolchain for the musl target. The script tries
  [`cargo zigbuild`](https://github.com/rust-cross/cargo-zigbuild), which
  uses `zig` as that cross C compiler/linker from any host. Neither is
  installed by this script — install them yourself once:

  ```sh
  cargo install cargo-zigbuild
  # zig: https://ziglang.org/download/, or a package manager:
  #   brew install zig            (macOS)
  #   choco install zig           (Windows)
  #   apt/dnf/pacman install zig  (Linux)
  #   pip install ziglang         (any OS, via PyPI)
  ```

  Without them, the script leaves that binary out and prints where to build
  it instead: inside a Linux sandbox or container (`tod-sandbox exec <name>
  -- cargo build --release -p <crate>`), copying the result into
  `target/sandbox/<name>` by hand. `tod-relay` itself never needs this path.

  As of this writing, `zig` and `cargo-zigbuild` are not installed in this
  repository's dev environment, so the `tod-store`-dependent path above is
  documented but not yet verified end to end here; `tod-relay`'s plain
  cross-build is verified by this script (it is what `scripts/install.sh`
  already does for the installed relay).

## The Blaxel account

The workspace, and how to sign in to it, are recorded in
`<data root>/sandboxes.toml`. Set them in the app under Settings →
**Cloud sandboxes** (the workspace, **Sign in with**, the **API key**, and the
**default image** new sandboxes start from), or with `tod-sandbox setup`.
Saving checks the sign-in and says how many sandboxes the workspace has. An
empty workspace turns cloud sandboxes off, and an empty image means Blaxel's
base image. There are two ways to sign in:

- **An API key (the default).** Create one in the Blaxel console and paste it
  into Settings (or give it to `tod-sandbox setup`, which prompts for it or
  reads it with `--api-key-stdin`). Nothing else needs installing. The key
  goes into tod's credential store (the OS keyring, else an encrypted file)
  as a kind agents cannot read: `tod-cli secrets` never lists or returns it,
  and it is never copied into a sandbox. It is long-lived, so it works until
  it is revoked in the console.
- **`bl login`** (`--auth bl`). You sign in through the Blaxel CLI in a
  browser. tod asks `bl` for a short-lived token and caches it until shortly
  before it expires, and no key is stored. If you are not signed in, `setup`
  runs `bl login <workspace>` for you.

**For a team**, use one Blaxel workspace and invite each teammate to it. Each
person signs in as themselves, with their own key or `bl login`, rather than
sharing one key: access is removed by removing the person from the workspace,
and the workspace's logs show who did what. Every sandbox is labeled with its
creator (`tod-owner`, from `--owner`, which defaults to your OS user name), so
the sandbox list shows whose sandbox is whose.

Building images (a wrapped or baked image, below) needs the Blaxel CLI either
way. With an API key, tod passes the key to `bl`, so it does not need
`bl login`.

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
