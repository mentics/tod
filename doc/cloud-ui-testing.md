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
  mesa-vulkan-drivers libxkbcommon-x11-dev libwebkit2gtk-4.1-dev scrot
```

- `mesa-vulkan-drivers`: `lvp_icd.json` (lavapipe, CPU Vulkan). GPUI cannot
  open a window without a Vulkan device.
- `libxkbcommon-x11-dev`: GPUI's X11 keyboard backend; the link step needs the
  `.so`, so the runtime package alone is not enough.
- `libwebkit2gtk-4.1-dev`: `tod-ui` embeds a webview (`gpui-wry` → `wry` →
  GTK/WebKit), so the build needs `gdk-3.0` and friends (this pulls in GTK).
- `scrot`: screenshots. The agent socket's `shot` is not supported on Linux.

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
