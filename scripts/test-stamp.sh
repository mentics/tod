#!/usr/bin/env bash
# Shows the per-artifact source hashes and checks that the orchestrator and the
# node-sandbox artifacts cover different crates, so a supervisor-only change
# cannot make the orchestrator look stale. Run: scripts/test-stamp.sh
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/lib/stamp.sh
orch_dirs="$(sandbox_crate_dirs orchestrator)"
node_dirs="$(sandbox_crate_dirs node)"
echo "orchestrator crates: $(echo "$orch_dirs" | tr '\n' ' ')"
echo "node crates:         $(echo "$node_dirs" | tr '\n' ' ')"
echo "$orch_dirs" | grep -qx crates/tod-orchestrator || { echo "FAIL: orchestrator group lacks tod-orchestrator" >&2; exit 1; }
if echo "$orch_dirs" | grep -qx crates/tod-supervisor; then echo "FAIL: orchestrator group includes tod-supervisor" >&2; exit 1; fi
echo "$node_dirs" | grep -qx crates/tod-supervisor || { echo "FAIL: node group lacks tod-supervisor" >&2; exit 1; }
if echo "$node_dirs" | grep -qx crates/tod-orchestrator; then echo "FAIL: node group includes tod-orchestrator" >&2; exit 1; fi
for g in orchestrator node all; do echo "hash $g: $(sandbox_source_hash $g)"; done
echo ok
