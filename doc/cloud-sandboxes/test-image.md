# The test image

A small Ubuntu image that holds what tod needs where there is no desktop: a
dev container or a cloud sandbox we can start, test against, and throw away.
It is what the moves, the session logs, and a real Claude are tested in
(`doc/agentd.md`), and with the `gui` tag it runs the tod app itself.

Source: `assets/sandbox/image/Dockerfile`. It starts from `ubuntu:24.04` and
adds nothing by hand: `assets/sandbox/bootstrap.sh --agents` is the one place
that says what a sandbox needs (sh, git, curl, scp/sftp-server, Node.js 22,
the Claude Code ACP adapter, Claude Code), so `base` is the same as
`tod-sandbox bake ubuntu:24.04 --agents`.

| Tag | Adds | Size |
|---|---|---|
| `tod-test:base` | the above | ~870 MB (Claude's native binary is 240 MB, Node 120 MB) |
| `tod-test:gui` | Xvfb, Mesa's software Vulkan (lavapipe, every other driver removed), the X11/xkb libraries, one font, and WebKitGTK/GTK (tod links them for its embedded web views) | ~1.6 GB |

```sh
docker build -f assets/sandbox/image/Dockerfile -t tod-test:base assets/sandbox
docker build -f assets/sandbox/image/Dockerfile -t tod-test:gui --build-arg GUI=1 assets/sandbox
```

Kept small on purpose: the adapter and Claude Code both ship the same native
`claude`, so one is hard-linked to the other when they are identical, and
Node's headers and docs are removed. Nothing else is installed: no compiler,
no editor, no desktop.

## Running the app with no display

GPUI draws with Vulkan on an X11 or Wayland window. With no monitor and no
GPU:

- **Xvfb** is a virtual X11 framebuffer: an X server whose screen is memory.
  `xvfb-run -a -s "-screen 0 1280x800x24" <command>` starts one, sets
  `DISPLAY`, and stops it when the command ends.
- **lavapipe** (`mesa-vulkan-drivers`) is a Vulkan driver that runs on the
  CPU, so the same renderer works with no GPU. It is slow, which is fine for
  tests. `VK_ICD_FILENAMES` in the image points at it.

## Building tod for it

`scripts/build-linux-tod.sh` builds the Linux `tod`, `tod-agentd`, and
`tod-cli` (glibc, with GPUI's X11 and Wayland) in a Debian bookworm container
into `target/linux/`. The first build takes a long time; later ones are
incremental (the target is in a Docker volume).

## Testing the app in it

Verified: the Linux build runs in `tod-test:gui` under Xvfb with the mock
agent, starts its own `tod-agentd`, and draws its window.

```sh
docker run -d --name tod-gui tod-test:gui
docker cp target/linux/. tod-gui:/opt/tod/app
docker exec tod-gui chmod +x /opt/tod/app/tod /opt/tod/app/tod-agentd /opt/tod/app/tod-cli
docker exec -d tod-gui sh -c 'xvfb-run -a -s "-screen 0 1280x800x24 -fbdir /tmp/fb"   /opt/tod/app/tod --data-root /root/data --agent mock --no-focus --agent-socket-port 47400'
```

The agent control socket (`README.md`) listens on the container's loopback, so
drive it with `docker exec tod-gui node -e '...'` (Node is in the image). Its
own `shot` is not supported on Linux; instead Xvfb writes the screen to
`/tmp/fb/Xvfb_screen0` (`-fbdir`), and `scripts/xwd2png.py` turns that into a
PNG: `docker cp tod-gui:/tmp/fb/Xvfb_screen0 . && python scripts/xwd2png.py
Xvfb_screen0 shot.png`. Quit it with `quit` on the socket, as on any machine.

## A real Claude

Claude needs a credential in each place. Use a token made for tests
(`claude setup-token`), never your own sign-in. Put `TOD_TEST_CLAUDE_TOKEN=...`
in the git-ignored `.env` at the repository root and load it into the
environment that runs the tests (`set -a; . ./.env; set +a`); the tests give
it to Claude only as `CLAUDE_CODE_OAUTH_TOKEN`. Note that in a sandbox the
token travels in the command sent to the sandbox, so use a token you can
revoke.

The gated tests in `crates/tod-store/src/fleet/session_log.rs`
(`a_real_claude_resumes_a_session_moved_into_a_dev_container` and `..._cloud_sandbox`)
start a real session on the host, copy its log into the place under that
place's own working-directory name, resume it there with `claude --resume`
(it must know what it was told on the host), then copy the log back and
resume on the host (it must know what it was told in the place). A run costs
about three turns. For the container: `TOD_TEST_DEV_CONTAINER=<a container
from tod-test:base>`. For the sandbox: `TOD_TEST_SANDBOX=<one made with
--agents>` and `TOD_TEST_SANDBOX_ROOT`, and `TOD_RELAY_BIN` if the relay is
not beside the build.

Both pass (`ubuntu:24.04` with `--agents` for the sandbox, `tod-test:base`
for the container).

## Moving a node through the app (3 legs)

`crates/tod-core/examples/e2e_move_node.rs` drives one conversation about one
node with a real Claude through `ConversationDriver`, changing the node's
Files settings between turns: host -> dev container -> cloud sandbox -> host.
Between legs it removes the old location the way the Files impact dialog does
(`provision::remove_location`, which pushes the branch first). Each turn asks
about what was said in the earlier ones and where the agent is running; it
fails if a reply forgets, the working directory is wrong, or a fresh session
was started.

`scripts/e2e-move.sh <data root with sandboxes.toml>` makes all of the setup
below, runs it, and removes it (the baked sandbox image stays in Blaxel).

Setup (all throwaway): a fresh data root holding only a `sandboxes.toml`
(Blaxel account, no stored token), a host git repo with an `origin`, a running
`tod-test:base` container started with `-e CLAUDE_CODE_OAUTH_TOKEN` and a repo
at `/work` with an `origin`, and a sandbox image from `e2e_env_bake` (a repo at
`/root/app`; pass it as `sandbox/<name>:latest`). `tod-sandbox` and `tod-cli`
must sit beside the example binary.

```sh
set -a; . ./.env; set +a
TOD_RELAY_BIN=target/sandbox/tod-relay target/debug/examples/e2e_move_node   <data root> <container> sandbox/<image>:latest <host repo>
```

All three legs pass, the session id is the same throughout, and no fresh
session was started. What the test found: a live agent session stays where
it started, so after a Files change the next turn kept running in the old
place. The driver now closes it and resumes the same session in the new one
(`ConversationDriver::session_place`), and tells the user ("This node moved:
the agent's session continues in …", or an error when the session's log could
not be brought). Also, a node whose files cannot be made used to fall back to
a scratch directory silently; it is now an error in the transcript and the
turn is not sent. Failed background log copies (`session_log::take_problems`)
and an unreadable Agent capability or settings file are errors the user sees
too, rather than log lines.

An agent in a dev container is signed in by the user inside it. With
`claude_token_via = "env"` (the same setting as for sandboxes) the stored
Claude token is also given to the agent process there, by name, never on a
command line.

