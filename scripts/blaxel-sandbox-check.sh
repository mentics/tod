#!/usr/bin/env bash
# Watches the Blaxel workspace's sandboxes, forever, waking every 2 minutes
# (override with INTERVAL=seconds). Read-only. Each round reports:
#   - anything but exactly one orchestrator (tod-orchestrator),
#     one workspace golden (wsx-golden-*) and one tod golden (tod-golden-*)
#   - every other tod-* sandbox created more than 10 minutes ago
#     (MAX_AGE_MIN overrides)
#   - the sandboxes unrelated to tod, when there are more than 2 (MAX_OTHER)
#
# Uses the `bl` CLI, so it needs `bl login` (or the workspace from the
# environment). Prints "OK" when a round finds nothing to report.
set -uo pipefail

INTERVAL=${INTERVAL:-120}
MAX_AGE_MIN=${MAX_AGE_MIN:-10}
MAX_OTHER=${MAX_OTHER:-2}

check() {
  local json
  json=$(bl get sandboxes -o json) || { echo "ERROR: bl get sandboxes failed" >&2; return 1; }
  JSON=$json MAX_AGE_MIN=$MAX_AGE_MIN MAX_OTHER=$MAX_OTHER python3 -I - <<'PY'
import json, os
from datetime import datetime, timezone

max_age = int(os.environ["MAX_AGE_MIN"])
max_other = int(os.environ["MAX_OTHER"])
now = datetime.now(timezone.utc)
orch, wsx, golden, nodes, other = [], [], [], [], []

def age_min(ts):
    ts = ts.rstrip("Z")
    if "." in ts:
        head, frac = ts.split(".")
        ts = head + "." + frac[:6]
    return (now - datetime.fromisoformat(ts).replace(tzinfo=timezone.utc)).total_seconds() / 60

for s in json.loads(os.environ["JSON"]):
    m = s["metadata"]
    n = m["name"]
    if n == "tod-orchestrator":
        orch.append(n)
    elif n.startswith("wsx-golden"):
        wsx.append(n)
    elif n.startswith("tod-golden"):
        golden.append(n)
    elif n.startswith("tod-"):
        nodes.append((n, age_min(m["createdAt"])))
    else:
        other.append(n)

problems = []
for label, found in (("orchestrator", orch), ("workspace golden (wsx-golden)", wsx), ("tod golden", golden)):
    if len(found) != 1:
        problems.append(f"expected exactly 1 {label}, found {len(found)}" + (": " + ", ".join(found) if found else ""))
for n, a in sorted(nodes, key=lambda x: -x[1]):
    if a > max_age:
        problems.append(f"tod sandbox older than {max_age} min: {n} ({a:.0f} min old)")
if len(other) > max_other:
    problems.append(f"{len(other)} sandboxes unrelated to tod: " + ", ".join(sorted(other)))

stamp = now.astimezone().strftime("%Y-%m-%d %H:%M:%S %Z")
if problems:
    print(f"[{stamp}] PROBLEMS")
    for p in problems:
        print("  - " + p)
else:
    print(f"[{stamp}] OK ({len(orch)} orchestrator, {len(wsx)} wsx golden, {len(golden)} tod golden, {len(nodes)} node, {len(other)} other)")
PY
}

while true; do
  check
  sleep "$INTERVAL"
done
