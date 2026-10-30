#!/usr/bin/env bash
# Deploys whatever shared cloud piece is out of date, and nothing that is not:
#   - the cloud orchestrator, recorded in <data root>/orchestrator-deployed.hash,
#   - the hourly watchdog job, recorded in <data root>/watchdog-deployed.hash.
# Each record is compared with the hash of the sandbox sources. A deploy
# records the hash it deployed, so the next run finds it up to date.
#
# Usage: scripts/deploy.sh
#   Takes no arguments. The data root is TOD_DATA_ROOT, else the one in
#   install.toml (what tod itself uses). Without one there is nothing to check
#   against, so it stops.
#
# The Linux sandbox binaries a deploy uses are built first, by build.sh.
# Nodes are not redeployed here: each node sandbox keeps the binaries it was
# made with, and tod uploads the new ones when it makes or replaces one.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib/stamp.sh

if [[ $# -gt 0 ]]; then
    echo "usage: $0 (takes no arguments)" >&2
    exit 1
fi

root="$(default_data_root)"
if [[ -z $root ]]; then
    echo "no data root: set TOD_DATA_ROOT, or run tod once to create install.toml" >&2
    exit 1
fi

scripts/build.sh --data-root "$root"
want="$(sandbox_source_hash)"

if [[ "$(cat "$root/orchestrator-deployed.hash" 2>/dev/null)" == "$want" ]]; then
    echo "==> orchestrator up to date ($root)"
else
    echo "==> deploying the orchestrator"
    cargo run --release -q -p tod-sandbox-cli -- --data-root "$root" orchestrator
    printf '%s\n' "$want" > "$root/orchestrator-deployed.hash"
fi

# The watchdog job's image is built from target/sandbox/tod-watchdog.
if [[ "$(cat "$root/watchdog-deployed.hash" 2>/dev/null)" == "$want" ]]; then
    echo "==> watchdog up to date ($root)"
else
    echo "==> deploying the watchdog"
    cargo run --release -q -p tod-sandbox-cli -- --data-root "$root" watchdog deploy
    printf '%s\n' "$want" > "$root/watchdog-deployed.hash"
fi
