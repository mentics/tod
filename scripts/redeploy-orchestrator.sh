#!/usr/bin/env bash
# Rebuild the Linux sandbox binaries and push them to the team's orchestrator
# sandbox, restarting its server. Use it when tod says the cloud orchestrator
# is out of date (for example "404 on problems/pull"), or after changing
# anything tod-orchestrator, tod-cli, or tod-relay is built from.
#
#   scripts/redeploy-orchestrator.sh --data-root DIR   # DIR holds sandboxes.toml
#   TOD_DATA_ROOT=DIR scripts/redeploy-orchestrator.sh
#   scripts/redeploy-orchestrator.sh --data-root DIR --move-data   # extra args go to
#                                                    # `tod-sandbox orchestrator`
#
# Needs Docker running (or zig + cargo-zigbuild) for the Linux build, and the
# Blaxel account configured in the data root's sandboxes.toml. Existing data
# is kept: the orchestrator's /data is untouched. See
# doc/cloud-sandboxes/orchestrator.md ("Provisioning").
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

root_args=()
rest=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --data-root) root_args=(--data-root "${2:?--data-root needs a directory}"); shift ;;
        *) rest+=("$1") ;;
    esac
    shift
done
if [[ ${#root_args[@]} -eq 0 && -z "${TOD_DATA_ROOT:-}" ]]; then
    echo "error: pass --data-root DIR or set TOD_DATA_ROOT (the directory with sandboxes.toml)" >&2
    exit 1
fi

echo "==> Building the Linux sandbox binaries"
scripts/build-sandbox-binaries.sh --release

echo "==> Deploying to the orchestrator sandbox"
cargo run --release -q -p tod-sandbox-cli -- "${root_args[@]}" orchestrator ${rest[@]+"${rest[@]}"}
