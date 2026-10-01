#!/usr/bin/env bash
# Builds the Linux (glibc) `tod` GUI, `tod-agentd`, `tod-cli` into
# target/linux/, for the test image (assets/sandbox/image/Dockerfile) and any
# Ubuntu 24.04 or newer machine. Unlike scripts/build-sandbox-binaries.sh
# (static musl, no GUI) this links GPUI's X11 and Wayland libraries, so it
# builds in a Debian bookworm container with their -dev packages (glibc 2.36,
# older than the image's, so the result runs there).
#
# The repository is mounted read-only; cargo's target, registry, and git
# checkouts live in Docker volumes (tod-linux-target-<checkout>, tod-linux-cargo,
# tod-linux-cargo-git), so the host's target/ gets only the finished binaries.
#
# Usage: scripts/build-linux-tod.sh [--release]   (debug by default)
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"
PROFILE=debug; FLAG=""
[ "${1:-}" = "--release" ] && { PROFILE=release; FLAG="--release"; }
CHECKOUT="$(basename "$REPO_ROOT" | tr -c 'a-zA-Z0-9\n' '-')"
IMAGE="${TOD_LINUX_BUILD_IMAGE:-rust:1-bookworm}"
mkdir -p target/linux
export MSYS_NO_PATHCONV=1
docker run --rm \
  -v "$REPO_ROOT":/src:ro \
  -v "tod-linux-target-$CHECKOUT":/target \
  -v tod-linux-cargo:/usr/local/cargo/registry \
  -v tod-linux-cargo-git:/usr/local/cargo/git \
  -v "$REPO_ROOT/target/linux":/out \
  -e CARGO_TARGET_DIR=/target -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
  -w /src "$IMAGE" sh -ec '
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends \
      pkg-config cmake clang libssl-dev libxkbcommon-dev libxkbcommon-x11-dev \
      libxcb1-dev libx11-xcb-dev libwayland-dev libfontconfig-dev libfreetype-dev \
      libasound2-dev libvulkan-dev libx11-dev libgtk-3-dev libwebkit2gtk-4.1-dev libsoup-3.0-dev libxdo-dev >/dev/null
    cargo build '"$FLAG"' -p tod -p tod-cli
    for b in tod tod-agentd tod-cli; do cp /target/'"$PROFILE"'/$b /out/; done
    cp -r /target/'"$PROFILE"'/process /target/'"$PROFILE"'/media /out/ 2>/dev/null || true
  '
ls -la target/linux
