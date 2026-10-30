#!/usr/bin/env bash
# Brings everything a dev `tod` runs up to date, doing only what is stale:
#   1. host binaries (tod, tod-agentd, tod-cli, tod-sandbox, tod-zed-shim),
#      dev-fast profile: cargo's own fingerprints make this a no-op when fresh;
#   2. Linux sandbox binaries (tod-relay, tod-supervisor, tod-orchestrator,
#      tod-cli, ...) in target/sandbox/: rebuilt when a hash of their sources
#      differs from target/sandbox/.source-hash, or a binary is missing;
#   3. the cloud orchestrator: reported when its recorded hash,
#      <data root>/orchestrator-deployed.hash, differs from the sources;
#   4. the hourly watchdog job: reported when its recorded hash,
#      <data root>/watchdog-deployed.hash, differs the same way.
#
# Steps 3 and 4 only report. Deploying is scripts/deploy.sh's job.
#
# Usage: scripts/build.sh [--data-root DIR]
#   The data root is --data-root, else TOD_DATA_ROOT, else the one in
#   install.toml (what tod itself uses), so normally you pass nothing. With
#   none of them the orchestrator and the watchdog are not checked (and the
#   output says so).
#   --no-sandbox   skip steps 2 to 4 (no cloud sandboxes in use)
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib/stamp.sh

root=""
sandbox=1
while [[ $# -gt 0 ]]; do
    case "$1" in
        --data-root) root="${2:?--data-root needs a directory}"; shift ;;
        --no-sandbox) sandbox=0 ;;
        *) echo "usage: $0 [--data-root DIR] [--no-sandbox]" >&2; exit 1 ;;
    esac
    shift
done

[[ -n $root ]] || root="$(default_data_root)"

echo "==> host binaries"
cargo build --profile dev-fast -p tod -p tod-cli -p tod-sandbox-cli -p tod-zed-shim

[[ $sandbox == 1 ]] || { echo "==> sandbox binaries and cloud checks skipped (--no-sandbox)"; exit 0; }

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
    echo "==> watchdog not checked: no data root: pass --data-root DIR, set TOD_DATA_ROOT, or run tod once to create install.toml"
    exit 0
fi

if [[ "$(cat "$root/orchestrator-deployed.hash" 2>/dev/null)" == "$want" ]]; then
    echo "==> orchestrator up to date ($root)"
else
    echo "==> orchestrator is out of date (or never deployed from here). It is shared, so it was left alone." >&2
    echo "    Deploy it with: scripts/deploy.sh" >&2
fi

if [[ "$(cat "$root/watchdog-deployed.hash" 2>/dev/null)" == "$want" ]]; then
    echo "==> watchdog up to date ($root)"
else
    echo "==> watchdog is out of date (or never deployed from here). It is shared, so it was left alone." >&2
    echo "    Deploy it with: scripts/deploy.sh" >&2
fi
