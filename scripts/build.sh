#!/usr/bin/env bash
# Brings everything a dev `tod` runs up to date, doing only what is stale:
#   1. host binaries (tod, tod-agentd, tod-cli, tod-sandbox, tod-zed-shim),
#      dev-fast profile: cargo's own fingerprints make this a no-op when fresh;
#   2. Linux sandbox binaries (tod-relay, tod-supervisor, tod-orchestrator,
#      tod-cli, ...) in target/sandbox/: rebuilt when a hash of their sources
#      differs from target/sandbox/.source-hash, or a binary is missing;
#   3. the cloud orchestrator: compared with the hash recorded in
#      <data root>/orchestrator-deployed.hash when it was last deployed.
#
# The orchestrator is shared by everyone in the workspace, so a stale one is
# reported, never replaced, unless you pass --deploy-orchestrator.
#
# Usage: scripts/build.sh [--data-root DIR] [--deploy-orchestrator] [-- <extra args to `tod-sandbox orchestrator`>]
#   The data root is --data-root, else TOD_DATA_ROOT, else the one in
#   install.toml (what tod itself uses), so normally you pass nothing. With
#   none of them the orchestrator is not checked (and the output says so).
#   --no-sandbox   skip steps 2 and 3 (no cloud sandboxes in use)
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib/stamp.sh

root=""
deploy=0
sandbox=1
orch_args=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --data-root) root="${2:?--data-root needs a directory}"; shift ;;
        --deploy-orchestrator) deploy=1 ;;
        --no-sandbox) sandbox=0 ;;
        --) shift; orch_args=("$@"); break ;;
        *) echo "usage: $0 [--data-root DIR] [--deploy-orchestrator] [--no-sandbox] [-- orchestrator args]" >&2; exit 1 ;;
    esac
    shift
done

[[ -n $root ]] || root="$(default_data_root)"

echo "==> host binaries"
cargo build --profile dev-fast -p tod -p tod-cli -p tod-sandbox-cli -p tod-zed-shim

[[ $sandbox == 1 ]] || { echo "==> sandbox binaries and orchestrator skipped (--no-sandbox)"; exit 0; }

want="$(sandbox_source_hash)"
bins=(tod-relay tod-supervisor tod-orchestrator tod-cli tod-watchdog)
stale=""
[[ "$(cat target/sandbox/.source-hash 2>/dev/null)" == "$want" ]] || stale="sources changed"
for b in "${bins[@]}"; do [[ -f target/sandbox/$b ]] || stale="$b is missing"; done
if [[ -n $stale ]]; then
    echo "==> sandbox binaries ($stale)"
    scripts/build-sandbox-binaries.sh --release
    printf '%s\n' "$want" > target/sandbox/.source-hash
else
    echo "==> sandbox binaries up to date"
fi

if [[ -z $root ]]; then
    echo "==> orchestrator not checked: no data root: pass --data-root DIR, set TOD_DATA_ROOT, or run tod once to create install.toml"
    exit 0
fi
deployed="$root/orchestrator-deployed.hash"
if [[ "$(cat "$deployed" 2>/dev/null)" == "$want" ]]; then
    echo "==> orchestrator up to date ($root)"
elif [[ $deploy == 1 ]]; then
    echo "==> deploying the orchestrator"
    cargo run --release -q -p tod-sandbox-cli -- --data-root "$root" orchestrator ${orch_args[@]+"${orch_args[@]}"}
    printf '%s\n' "$want" > "$deployed"
else
    echo "==> orchestrator is out of date (or never deployed from here). It is shared, so it was left alone." >&2
    echo "    Deploy it with: scripts/build.sh --data-root $root --deploy-orchestrator" >&2
fi
