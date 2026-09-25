#!/usr/bin/env bash
# Builds tod in release mode and installs it (binaries + process/media bundles)
# into a target directory, with cloud-sandbox support (tod-sandbox, Zed's ssh
# shim, and the Linux relay that runs in each sandbox).
#
# Usage:
#   scripts/install.sh /path/to/install
#   scripts/install.sh /path/to/install --no-agent-socket   # release-safe, excludes agent-control socket
#   scripts/install.sh /path/to/install --sandbox-workspace my-team   # also sign in to Blaxel
#   scripts/install.sh /path/to/install --no-sandbox        # skip cloud sandboxes
set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <target-dir> [--no-agent-socket] [--sandbox-workspace W] [--no-sandbox]" >&2
    exit 1
fi

TARGET_DIR="$1"
shift || true

CARGO_ARGS=(build --release -p tod -p tod-cli)
SANDBOX=1
SANDBOX_WORKSPACE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-agent-socket) CARGO_ARGS+=(--no-default-features) ;;
        --no-sandbox) SANDBOX=0 ;;
        --sandbox-workspace) SANDBOX_WORKSPACE="${2:?--sandbox-workspace needs a value}"; shift ;;
        *) echo "unknown option $1" >&2; exit 1 ;;
    esac
    shift
done
if [[ $SANDBOX == 1 ]]; then
    CARGO_ARGS+=(-p tod-sandbox-cli -p tod-zed-shim)
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

echo "Building: cargo ${CARGO_ARGS[*]}"
cargo "${CARGO_ARGS[@]}"

mkdir -p "$TARGET_DIR"

RELEASE_DIR="$REPO_ROOT/target/release"

BINS=(tod tod-cli)
if [[ $SANDBOX == 1 ]]; then
    BINS+=(tod-sandbox tod-zed-shim)
fi
for bin in "${BINS[@]}"; do
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

# The relay runs inside Linux sandboxes: a static binary cross-built from this
# machine (rust-lld links it; see .cargo/config.toml). A failure here leaves tod
# installed without cloud-sandbox support.
install_sandbox_relay() {
    local target=x86_64-unknown-linux-musl
    if ! rustup target add "$target" >/dev/null; then
        echo "warning: rustup could not add $target; cloud sandboxes will not work" >&2
        return
    fi
    echo "Building: cargo build --release -p tod-relay --target $target"
    if ! cargo build --release -p tod-relay --target "$target"; then
        echo "warning: building tod-relay failed; cloud sandboxes will not work" >&2
        return
    fi
    mkdir -p "$TARGET_DIR/sandbox"
    cp -f "$REPO_ROOT/target/$target/release/tod-relay" "$TARGET_DIR/sandbox/tod-relay"
    echo "Installed sandbox/tod-relay"
}
if [[ $SANDBOX == 1 ]]; then
    install_sandbox_relay
fi

echo
echo "Done. Installed to $TARGET_DIR"
echo "Run: $TARGET_DIR/tod"
echo "(First run asks where to store your data and remembers it via install.toml.)"

if [[ $SANDBOX == 1 ]]; then
    echo
    if [[ -n "$SANDBOX_WORKSPACE" ]]; then
        echo "Setting up cloud sandboxes (workspace $SANDBOX_WORKSPACE)..."
        if ! "$TARGET_DIR/tod-sandbox" setup --workspace "$SANDBOX_WORKSPACE"; then
            echo "warning: tod-sandbox setup failed. If tod has not chosen a data root yet," >&2
            echo "  run tod once, then: $TARGET_DIR/tod-sandbox setup --workspace $SANDBOX_WORKSPACE" >&2
        fi
    else
        echo "Cloud sandboxes: in tod's Settings -> Cloud sandboxes, set the Blaxel workspace"
        echo "  and paste an API key (or run $TARGET_DIR/tod-sandbox setup --workspace <workspace>),"
        echo "  then create sandboxes from a node's Files section."
    fi
fi
