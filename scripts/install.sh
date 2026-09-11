#!/usr/bin/env bash
# Builds tod in release mode and installs it (binaries + process/media bundles)
# into a target directory.
#
# Usage:
#   scripts/install.sh /path/to/install
#   scripts/install.sh /path/to/install --no-agent-socket   # release-safe, excludes agent-control socket
set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <target-dir> [--no-agent-socket]" >&2
    exit 1
fi

TARGET_DIR="$1"
shift || true

CARGO_ARGS=(build --release -p tod -p tod-cli)
for arg in "$@"; do
    if [[ "$arg" == "--no-agent-socket" ]]; then
        CARGO_ARGS+=(--no-default-features)
    fi
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

echo "Building: cargo ${CARGO_ARGS[*]}"
cargo "${CARGO_ARGS[@]}"

mkdir -p "$TARGET_DIR"

RELEASE_DIR="$REPO_ROOT/target/release"

for bin in tod tod-cli; do
    src="$RELEASE_DIR/$bin"
    if [[ -f "$src" ]]; then
        cp -f "$src" "$TARGET_DIR/$bin"
        echo "Installed $bin"
    else
        echo "warning: $bin not found at $src; skipping" >&2
    fi
done

for dir in process media; do
    src="$RELEASE_DIR/$dir"
    if [[ -d "$src" ]]; then
        rm -rf "${TARGET_DIR:?}/$dir"
        cp -R "$src" "$TARGET_DIR/$dir"
        echo "Installed $dir/"
    else
        echo "warning: $dir bundle not found at $src; skipping" >&2
    fi
done

echo
echo "Done. Installed to $TARGET_DIR"
echo "Run: $TARGET_DIR/tod"
echo "(First run asks where to store your data and remembers it via install.toml.)"
