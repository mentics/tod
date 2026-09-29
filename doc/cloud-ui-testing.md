# Running and screenshotting the UI in Claude's cloud environment

The cloud container has no display or GPU. `tod` (GPUI, Vulkan) runs there
under a virtual X server with a software Vulkan driver.

Already in the image (nothing to add): `Xvfb`/`xvfb-run`, `libvulkan1`,
`libxkbcommon0`, `libxcb*`, `libwayland*`, `libfontconfig`, `libssl-dev`,
Rust, Chromium.

## Setup script (Environment → Setup script)

```bash
apt-get update -qq || true   # a broken third-party PPA may 403; ignore it
apt-get install -y -qq --no-install-recommends \
  mesa-vulkan-drivers libxkbcommon-x11-dev libwebkit2gtk-4.1-dev scrot xdotool
```

- `mesa-vulkan-drivers`: `lvp_icd.json` (lavapipe, CPU Vulkan). GPUI cannot
  open a window without a Vulkan device.
- `libxkbcommon-x11-dev`: GPUI's X11 keyboard backend; the link step needs the
  `.so`, so the runtime package alone is not enough.
- `libwebkit2gtk-4.1-dev`: `tod-ui` embeds a webview (`gpui-wry` → `wry` →
  GTK/WebKit), so the build needs `gdk-3.0` and friends (this pulls in GTK).
- `scrot`, `xdotool`: screenshots and real mouse clicks (the agent socket's `shot` and `click` are Windows-only).

Network policy must allow the package mirror, crates.io, and
`github.com` (GPUI is a git dependency of zed-industries/zed).

## Running

`xvfb-run` hides its X authority file from other processes, so start Xvfb
yourself with access control off:

```bash
Xvfb :77 -ac -screen 0 1600x1000x24 &
DISPLAY=:77 VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json \
  ./target/debug/tod --data-root <scratch dir> --agent mock --no-focus \
  --agent-socket-port 9911 &
DISPLAY=:77 scrot -o shot.png      # then Read shot.png
```

Drive input over the socket (`key`, `text`, `click`, `sync`; not `shot`):
`exec 3<>/dev/tcp/127.0.0.1/9911; echo "key ctrl-j" >&3; head -1 <&3`.
Never `pkill -f tod`: it matches your own shell; use `pkill -x`.

The first build takes several minutes (GPUI); keep `target/` if you can.

## Driving the app

- `click`, `drag` and `shot` on the socket are Windows-only. On Linux use real
  X input for clicks (`DISPLAY=:77 xdotool mousemove X Y click 1`, needs the
  `xdotool` package) and `scrot` for screenshots. `key` and `text` on the
  socket work; `text` needs an input in edit mode, so click it first.
- `xdotool type` does not reach GPUI (no window manager gives it focus); use
  the socket's `text`.
- Ctrl+J toggles the chat drawer: press it once. Ctrl+Enter sends.
- After changing `tod-core`/`tod-store`, build `tod-cli` too
  (`cargo build -p tod -p tod-cli`), or a turn is refused as "built from
  different source".

## Real Claude instead of the mock

The container already has Claude credentials in its environment, so no login
is needed. Two extra steps:

```bash
npm install --prefix ~/acp @agentclientprotocol/claude-agent-acp@latest
IS_SANDBOX=1 CLAUDE_ACP_BIN=~/acp/node_modules/.bin/claude-agent-acp \
  ./target/debug/tod --agent claude ...
```

`IS_SANDBOX=1` is needed because the container runs as root and Claude Code
refuses `--dangerously-skip-permissions` as root otherwise.
