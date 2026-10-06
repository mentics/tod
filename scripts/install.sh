#!/usr/bin/env bash
# Builds tod in release mode and installs it (binaries + process/media bundles)
# into a target directory, with cloud-sandbox support (tod-sandbox, Zed's ssh
# shim, and the Linux relay, supervisor (agent daemon), and orchestrator
# binaries that run in sandboxes).
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

BINS=(tod tod-cli tod-agentd)
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

# On Linux the shell shows an app's icon only through a .desktop file whose
# name matches the window's app id ("tod", crates/tod-ui/src/app/app_icon.rs),
# with the icon installed in the hicolor theme. Both go in the user's own data
# dir, so this needs no root.
install_desktop_entry() {
    local data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
    local size
    for png in "$REPO_ROOT"/assets/icon/png/tod-*.png; do
        size="${png##*/tod-}"
        size="${size%.png}"
        mkdir -p "$data_home/icons/hicolor/${size}x${size}/apps"
        cp -f "$png" "$data_home/icons/hicolor/${size}x${size}/apps/tod.png"
    done
    mkdir -p "$data_home/applications"
    local exec_path
    exec_path="$(cd "$TARGET_DIR" && pwd)/tod"
    cat > "$data_home/applications/tod.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=tod
Comment=Tasks, obligations, and the agents working on them
Exec="$exec_path"
Icon=tod
StartupWMClass=tod
Terminal=false
Categories=Development;
EOF
    if command -v gtk-update-icon-cache >/dev/null; then
        gtk-update-icon-cache -q -t "$data_home/icons/hicolor" || true
    fi
    if command -v update-desktop-database >/dev/null; then
        update-desktop-database -q "$data_home/applications" || true
    fi
    echo "Installed tod.desktop and icons under $data_home"
}
if [[ "$(uname -s)" == Linux ]]; then
    install_desktop_entry
fi

# The Linux binaries that run inside cloud sandboxes: the relay, the node
# supervisor (the agent daemon that runs a node's lifecycle on its own), the
# orchestrator, the sandbox's tod-cli, and the hourly watchdog. They are built
# by scripts/build-sandbox-binaries.sh (Docker, else cargo zigbuild, for the
# ones that bundle SQLite) into target/sandbox/ and installed to sandbox/,
# where tod-sandbox and tod find them. A failure leaves tod installed with
# whichever of them were built.
install_sandbox_binaries() {
    if ! "$REPO_ROOT/scripts/build-sandbox-binaries.sh" --release; then
        echo "warning: building the sandbox binaries failed; cloud sandboxes will not work" >&2
        return
    fi
    mkdir -p "$TARGET_DIR/sandbox"
    local bin
    for bin in tod-relay tod-supervisor tod-orchestrator tod-cli tod-watchdog; do
        local src="$REPO_ROOT/target/sandbox/$bin"
        if [[ -f "$src" ]]; then
            cp -f "$src" "$TARGET_DIR/sandbox/$bin"
            echo "Installed sandbox/$bin"
        else
            echo "warning: sandbox/$bin was not built; $(case $bin in
                tod-relay) echo "cloud sandboxes will not work" ;;
                tod-supervisor) echo "autonomous nodes will not run on their own" ;;
                tod-orchestrator) echo "the team orchestrator cannot be deployed" ;;
                tod-cli) echo "tod-cli pr will not work in a node" ;;
                *) echo "the watchdog cannot be deployed" ;;
            esac)" >&2
        fi
    done
}
if [[ $SANDBOX == 1 ]]; then
    install_sandbox_binaries
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
