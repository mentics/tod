#!/usr/bin/env bash
# Rebuilds everything a dev `tod` runs, so a pulled change takes effect:
#   1. the host binaries beside the app (tod, tod-agentd, tod-cli,
#      tod-sandbox, tod-zed-shim), in the dev-fast profile scripts/dev.sh uses;
#   2. the Linux binaries that run inside cloud sandboxes (tod-relay,
#      tod-supervisor, tod-orchestrator, tod-cli, ...) into target/sandbox/,
#      which tod prefers over any other copy of them.
# Then restart tod (it uploads the sandbox binaries when it makes or replaces
# a node's sandbox; an existing sandbox keeps the ones it has).
#
# Usage: scripts/rebuild-all.sh [--host-only|--sandbox-only]
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

host=1
sandbox=1
case "${1:-}" in
    --host-only) sandbox=0 ;;
    --sandbox-only) host=0 ;;
    "") ;;
    *) echo "usage: $0 [--host-only|--sandbox-only]" >&2; exit 1 ;;
esac

if [[ $host == 1 ]]; then
    echo "==> host binaries (dev-fast)"
    cargo build --profile dev-fast -p tod -p tod-cli -p tod-sandbox-cli -p tod-zed-shim
fi

if [[ $sandbox == 1 ]]; then
    echo "==> sandbox (Linux) binaries -> target/sandbox/"
    scripts/build-sandbox-binaries.sh --release
    ls -l target/sandbox/
fi

echo "==> done. Restart tod to use the new build."
