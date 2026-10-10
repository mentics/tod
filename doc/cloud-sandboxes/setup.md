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

Everything that runs inside a sandbox (`tod-relay`, `tod-supervisor`,
`tod-orchestrator`, the Linux `tod-cli`, and `tod-watchdog`) is a static
Linux binary for `x86_64-unknown-linux-musl`, built from whatever OS you
develop on, into `target/sandbox/`. A node's sandbox gets the Linux
`tod-cli` too: `tod-cli pr` and `secrets` run there, not on the
orchestrator, so rebuild after changing either. Provisioning looks there first
(`relay_path()` in `crates/tod-store/src/fleet/sandbox.rs`, and
`tod-sandbox orchestrator`).

```sh
scripts/build-sandbox-binaries.sh              # release build (default)
scripts/build-sandbox-binaries.sh --debug
scripts/build-sandbox-binaries.sh --docker-test -p tod-relay   # Linux-only tests
```

On Windows, run it under Git Bash (`bash scripts/build-sandbox-binaries.sh`);
there is no PowerShell twin.

Everything but the relay links C: bundled SQLite (through `tod-store`),
OpenSSL (reqwest's native-tls, through `tod-integration`), zstd, and ring.
So the script picks, in order:

- **Docker** (used whenever `docker` answers, unless `--no-docker`; Docker
  Desktop on Windows and macOS). One container run of `rust:1-alpine`
  (override with `TOD_SANDBOX_BUILD_IMAGE`), where musl is the native
  target and OpenSSL is linked statically, builds all five. The repository
  is mounted read-only; cargo's target directory, registry, and git
  checkouts live in Docker volumes (`tod-sandbox-target-<checkout>`, one
  per checkout, since cargo would otherwise reuse another worktree's build of
  a crate whose files here are older; `tod-sandbox-cargo`,
  `tod-sandbox-cargo-git`), so the host's `target/` is
  untouched but for the binaries copied to `target/sandbox/`, and later
  builds are incremental. The first build takes about 15 minutes (it
  fetches the workspace's git dependencies, gpui's included, to resolve it);
  a rebuild after a change, a minute or two. `--docker-test <args>` runs
  `cargo test <args>` in the same container, for code only Linux compiles
  (`tod-relay`'s tests).
- **`cargo zigbuild`**, when `zig` and `cargo-zigbuild` are installed and
  Docker is not (or `--no-docker`). Not verified here, and OpenSSL would
  still need a musl build of its own.
- Otherwise only `tod-relay` is built: it has no C dependencies, so a plain
  `cargo build --target x86_64-unknown-linux-musl` links it with `rust-lld`
  (`.cargo/config.toml`) on any host. The script says what it skipped.

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

`bake` can clone repositories into the image too, one `--repo` per repository
(`URL` or `URL=/dir`; the directory defaults to `/root/<name>`):

```sh
tod-sandbox bake ubuntu:24.04 --agents --name my-base \
  --repo https://github.com/acme/app --repo https://github.com/acme/lib=/root/lib
```

Only `https://` URLs without credentials are accepted: the clone command stays
in the image's layers, so a token in a URL would too. A private repository
cannot be built in this way; it is fetched when a node's sandbox is made, like
any other. One image can hold the repository a node works on and the source it
depends on, and be shared by every node. In the app, the task editor's Files
section does this under **Each node's sandbox starts from → An image → Image
with repositories**: the base image, a name, and one repository per line
(`<url> [<directory>]`); when the build finishes the new image is what each
node's sandbox starts from.

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
through *This machine*, *Dev container*, and *Cloud sandbox*. With *Cloud
sandbox*, the node and every node under it that does not set Files itself
each get **a sandbox of their own**, made the first time the node needs its
files (a turn, a shell, an editor). There is nothing to create by hand.

**Each node's sandbox starts from** chooses between *An image* (a cold
start; empty uses the default image from Settings) and *A fork of a sandbox*
(pick one from the list of the workspace's sandboxes; see **Forks** above).
Either way it must already hold the repository: give the workspace directory
as its path there (`/root/app`). A baked image takes seconds, any other image
a minute or more. Each sandbox is named after its node and checks out the
node's own branch (`task/<slug>` unless set). See
[../files-locations.md](../files-locations.md) for how these are kept and
removed. An agent can set the same thing:

```sh
tod-cli capabilities set <node> files --sandbox image --dir /root/app
tod-cli capabilities set <node> files --sandbox fork:<name> --dir /root/app
tod-cli capabilities set <node> files --sandbox ''     # back to this machine
```

**Claude must be signed in** in each sandbox before its first turn. tod
never copies credentials into a sandbox, so the simplest way is to sign in
once in a sandbox that holds the repository and have every node fork it:

```sh
tod-sandbox shell <name>
claude /login
```

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
runs in them. How long each waits before parking (Zed 10 s, terminals and
agents 3 s) is in Settings → Advanced.

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

## The orchestrator

The sandbox that holds each user's database for autonomous nodes and runs
their `tod-cli` commands: `tod-sandbox orchestrator` sets it up. See
[orchestrator.md](orchestrator.md).

Settings for autonomous nodes in `sandboxes.toml` (none is needed):

```toml
scheduler = "blaxel"                        # who wakes a waiting node: "orchestrator" (default) or "blaxel"

[blaxel]
orchestrator = "tod-orchestrator"           # the orchestrator sandbox's name (default)
orchestrator_volume = "tod-orchestrator-data" # keep its /data on a volume (see orchestrator.md)
node_base = "tod-node-base"                 # fork node sandboxes from this base (needs forking)
```

`scheduler = "blaxel"` has each node's supervisor schedule its own wake on
its sandbox (a Blaxel schedule; the workspace needs sandbox schedules, and
the node's proxy adds the token, so prefer an API key, whose token does not
expire); `"orchestrator"` has the orchestrator's timer poke it. It is read
when a node's sandbox is created. `node_base` makes a base sandbox with the
relay, supervisor, and bundles once (again when any of them, the
credentials, the image, or the orchestrator change) and forks each new node
from it; where forking is refused, nodes are created from the image as
without it, so leave it unset there, or the base sits unused. See
[autonomous-nodes.md](autonomous-nodes.md), Development account.

## Running a node in the cloud without the window

`cargo run -p tod-core --example cloud_dev -- <data_root> run <node>` does
what the app's "Run in the cloud" does (`cloud_sync::run_in_cloud`): seed the
orchestrator, create the node's sandbox with its proxy rules, install the
relay, the `tod-cli` shim, the supervisor, the process and media
bundles, and the Claude agent adapter when the image lacks it, check out the node's branch (its commits authored by your git
`user.name`/`user.email`: the node repository's own if it is on this
machine, else your global one), and poke it. `... sync` sends the
outbox and pulls the node's progress back; `... answer <decision> <option>`
answers one of the node's pending decisions (as the task panel would; `sync`
sends it); `... init` makes a list `cloud` in
a fresh data root, and `... node <title> <repo> <branch> <step>...` a node
in it (`active`, with Lifecycle, Agent, and Files on the repository, an
HTTPS URL or a checkout, and one plan step per argument), printing its
UUID; `... state <node> proposed` starts it from the beginning instead,
and `... stop <node>` takes it out of the cloud and deletes its sandbox. Build the sandbox
binaries first, and never point it at a data root the app has open. Each
run reports how long the sandbox took to come up and to be provisioned.

The node's supervisor runs Claude unless `TOD_CLOUD_AGENT=mock` is set when
the sandbox is created (it becomes the sandbox's `TOD_SUPERVISOR_AGENT`). In a
mock run, a plan step whose body starts `wait 3m: …` records a wait of that
long, so the node sleeps until its scheduler wakes it. A line `write <path>:
<text>` in a step makes the mock write that file in the checkout when it
closes the step (so the pushed branch differs from its base), and a line
`open pr: <title>` in any step makes its `pr` turn open a real pull request
with `tod-cli pr open` (from the branch to `origin`'s default branch)
instead of recording a fake one. The relay's log
(`GET <sandbox-url>/process/tod-relay/logs`) holds the supervisor's too.

For Claude, the node needs your Claude subscription token, set **once per
machine** (not per sandbox): Settings → Cloud sandboxes → **Claude
subscription** → **Get a token** opens a terminal running `claude
setup-token` (Claude Code must be installed on this machine); sign in in the
browser it opens, copy the token it prints (`sk-ant-oat01-…`), and paste it
into the **Claude subscription** field above the button (Enter saves it). tod
keeps it in its credential store (the OS keyring, else an encrypted file) and
gives it to each node sandbox it creates through the sandbox's proxy, as the
header for `api.anthropic.com`; Claude Code in the sandbox only sees a
placeholder, so nothing needs signing in there. Without a token, running a
Claude node in the cloud fails at once with a message naming that row. A
token changed later reaches a node when its sandbox is next created.
`claude_token_via = "env"` under `[blaxel]` in `sandboxes.toml` puts the
token in the supervisor's environment instead (see
[autonomous-nodes.md](autonomous-nodes.md), Credentials).

**Interactive nodes** (Files "Cloud sandbox", where the app's own agent runs
in the sandbox through `tod-sandbox agent`) use the same token and the same
rules: their proxy holds GitHub (`api.github.com` Bearer, `github.com`
Basic), Linear, and, with `claude_token_via = "proxy"`, `api.anthropic.com`,
beside the node's Environment secrets (no Blaxel rules; no supervisor runs
there). The agent process is launched with `CLAUDE_CODE_OAUTH_TOKEN` set to
the placeholder, `GH_TOKEN` a placeholder, `TOD_GITHUB_AUTH=proxy`, and
`NODE_USE_ENV_PROXY=1`; with `"env"` it gets the real token on the agent
process alone (never stored in the sandbox). A missing token just leaves its
rule out, with a warning. The sandbox carries a `tod-creds` label, a hash of
every rule and secret, so a credential changed or added later replaces the
proxy's rules in place (Blaxel's network update: in effect within a second,
no restart, nothing lost). Only a sandbox made without any proxy, which can
never get one, is recreated instead (after pushing its branch, asking first
when work would be lost).
