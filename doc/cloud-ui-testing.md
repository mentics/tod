# Running and screenshotting the UI in Claude's cloud environment

The cloud container has no display or GPU. `tod` (GPUI, Vulkan) runs there
under a virtual X server with a software Vulkan driver.

Already in the image (nothing to add): `Xvfb`/`xvfb-run`, `libvulkan1`,
`libxkbcommon0`, `libxcb*`, `libwayland*`, `libfontconfig`, `libssl-dev`,
Rust, Chromium.

## Setup script (Environment → Setup script)

```bash
apt-get update -qq || true   # a broken third-party PPA may 403; ignore it
apt-get install -y -qq --no-install-recommends mesa-vulkan-drivers libxkbcommon-x11-0
```

- `mesa-vulkan-drivers` supplies `lvp_icd.json` (lavapipe, CPU Vulkan). GPUI
  cannot open a window without a Vulkan device.
- `libxkbcommon-x11-0` is GPUI's X11 keyboard backend.

Network policy must allow the package mirror, crates.io, and
`github.com` (GPUI is a git dependency of zed-industries/zed).

## Running

```bash
xvfb-run -a -s "-screen 0 1600x1000x24" \
  cargo run -p tod -- --data-root <scratch dir> --agent mock --no-focus \
  --agent-socket-port 7777
```

Drive it over the agent socket (`key`, `text`, `click`, `sync`, `shot`); see
README.md. Set `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json` if
Vulkan picks another driver.
