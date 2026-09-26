#!/usr/bin/env bash
# Builds the Linux binaries that run inside a cloud sandbox, into
# target/sandbox/. Run from Windows, macOS, or Linux.
#
# tod-relay has no C dependencies, so rust-lld cross-links it from any host
# (see .cargo/config.toml) with no extra tooling. Any crate that depends on
# tod-store (tod-supervisor, tod-orchestrator, once those crates exist) pulls
# in bundled SQLite, which is C, so cross-compiling it needs a Linux C
# toolchain for x86_64-unknown-linux-musl. This script tries `cargo zigbuild`
# for that (see "Installing zig / cargo-zigbuild" below); crates that need it
# are skipped, with instructions, if zigbuild is not available.
#
# Usage:
#   scripts/build-sandbox-binaries.sh [--release|--debug]
#
# Windows: run this under Git Bash (the same shell scripts/dev.sh and
# scripts/install.sh already assume), not PowerShell — there is no .ps1
# twin. `sh scripts/build-sandbox-binaries.sh` from Git Bash, or
# `bash scripts/build-sandbox-binaries.sh`, both work.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

PROFILE=release
PROFILE_DIR=release
while [[ $# -gt 0 ]]; do
    case "$1" in
        --release) PROFILE=release; PROFILE_DIR=release ;;
        --debug) PROFILE=dev; PROFILE_DIR=debug ;;
        *) echo "unknown option $1" >&2; exit 1 ;;
    esac
    shift
done

TARGET=x86_64-unknown-linux-musl
OUT_DIR="$REPO_ROOT/target/sandbox"
mkdir -p "$OUT_DIR"

rustup target add "$TARGET" >/dev/null 2>&1 || true

HAVE_ZIGBUILD=0
if command -v cargo-zigbuild >/dev/null 2>&1 && command -v zig >/dev/null 2>&1; then
    HAVE_ZIGBUILD=1
fi

# name:crate — crates that exist today, plus the ones later waves add. A
# crate not yet in the workspace is skipped so this script keeps working as
# tod-supervisor and tod-orchestrator land.
CANDIDATES=(
    "tod-relay:tod-relay:false"
    "tod-supervisor:tod-supervisor:true"
    "tod-orchestrator:tod-orchestrator:true"
    "tod-cli:tod-cli:true"
)

install_bin() {
    local name="$1" src_dir="$2"
    local src="$src_dir/$name"
    if [[ -f "$src" ]]; then
        cp -f "$src" "$OUT_DIR/$name"
        echo "Built $OUT_DIR/$name"
        return 0
    fi
    return 1
}

for entry in "${CANDIDATES[@]}"; do
    IFS=':' read -r bin_name crate_name needs_c <<<"$entry"
    if ! cargo metadata --no-deps --format-version 1 2>/dev/null | grep -q "\"name\":\"$crate_name\""; then
        echo "Skipping $crate_name (crate not in the workspace yet)"
        continue
    fi

    if [[ "$needs_c" == "false" ]]; then
        # No C dependencies: plain cross-compile with rust-lld.
        echo "Building $crate_name for $TARGET (no C dependencies, rust-lld)"
        cargo build --profile "$PROFILE" -p "$crate_name" --target "$TARGET"
        install_bin "$bin_name" "$REPO_ROOT/target/$TARGET/$PROFILE_DIR" \
            || echo "warning: $bin_name not found after building $crate_name" >&2
        continue
    fi

    # Depends on tod-store (bundled SQLite, C): needs a cross C toolchain.
    if [[ $HAVE_ZIGBUILD == 1 ]]; then
        echo "Building $crate_name for $TARGET with cargo zigbuild"
        if cargo zigbuild --profile "$PROFILE" -p "$crate_name" --target "$TARGET"; then
            install_bin "$bin_name" "$REPO_ROOT/target/$TARGET/$PROFILE_DIR" \
                || echo "warning: $bin_name not found after zigbuild" >&2
        else
            echo "warning: cargo zigbuild failed for $crate_name; see the fallback below" >&2
        fi
    else
        cat >&2 <<EOF
warning: $crate_name depends on tod-store (bundled SQLite, C) and cargo-zigbuild
  is not installed, so it cannot be cross-compiled from this host for $TARGET.

  Install zig and cargo-zigbuild, then re-run this script:
    cargo install cargo-zigbuild
    # zig: https://ziglang.org/download/ (or a package manager: brew install zig,
    # choco install zig, apt/dnf/pacman install zig, pip install ziglang)

  Or build it inside a Linux sandbox / container instead:
    tod-sandbox exec <name> -- sh -c 'cd /root/project && cargo build --release -p $crate_name'
  and copy the resulting binary into target/sandbox/$bin_name yourself.
EOF
    fi
done

echo
echo "Done. Sandbox binaries are in $OUT_DIR"
