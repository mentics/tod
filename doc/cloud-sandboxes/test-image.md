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
(`claude setup-token`), never your own sign-in, and pass it only as the
environment variable `CLAUDE_CODE_OAUTH_TOKEN` of the command that needs it.
The gated tests read it from `TOD_TEST_CLAUDE_TOKEN`.
