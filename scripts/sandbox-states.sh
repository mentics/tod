#!/usr/bin/env bash
# Lists the workspace's sandboxes with Blaxel's own view of each: `state`
# (RUNNING / STANDBY), `status`, and `lastUsedAt`. Read-only, and it never
# contacts a sandbox, so it wakes nothing.
#
#   BLAXEL_API_KEY=... scripts/sandbox-states.sh          # table
#   BLAXEL_API_KEY=... scripts/sandbox-states.sh --raw    # the list, secrets blanked
#   scripts/sandbox-states.sh --file list.json            # parse a saved list
#
# The workspace comes from BLAXEL_WORKSPACE, else `[blaxel] workspace` in
# $TOD_DATA_ROOT/sandboxes.toml (default ~/.config/tod). `--raw` blanks
# `spec.network` and `spec.runtime.envs`, which hold credentials.
set -euo pipefail

mode=table
file=
while [ $# -gt 0 ]; do
  case "$1" in
    --raw) mode=raw ;;
    --file) file=${2:?--file needs a path}; shift ;;
    *) echo "usage: $0 [--raw] [--file list.json]" >&2; exit 2 ;;
  esac
  shift
done

if [ -n "$file" ]; then
  json=$(cat "$file")
else
  : "${BLAXEL_API_KEY:?set BLAXEL_API_KEY (the workspace API key)}"
  root=${TOD_DATA_ROOT:-$HOME/.config/tod}
  workspace=${BLAXEL_WORKSPACE:-$(sed -n '/^\[blaxel\]/,/^\[/{s/^workspace *= *"\(.*\)"/\1/p;}' "$root/sandboxes.toml" 2>/dev/null | head -1)}
  : "${workspace:?set BLAXEL_WORKSPACE, or have [blaxel] workspace in $root/sandboxes.toml}"
  json=$(curl -sS --fail-with-body \
    -H "Authorization: Bearer $BLAXEL_API_KEY" \
    -H "X-Blaxel-Workspace: $workspace" \
    https://api.blaxel.ai/v0/sandboxes)
fi

MODE=$mode JSON=$json python3 -I - <<'PY'
import json, os, sys
from datetime import datetime, timezone

HIDE = {"network", "envs", "env", "secrets", "headers", "token", "apikey", "authorization"}

def redact(v, hide=False):
    if isinstance(v, str):
        return "<redacted>" if hide else v
    if isinstance(v, list):
        return [redact(x, hide) for x in v]
    if isinstance(v, dict):
        return {k: redact(x, hide or k.lower() in HIDE) for k, x in v.items()}
    return v

data = json.loads(os.environ["JSON"])
if os.environ["MODE"] == "raw":
    print(json.dumps(redact(data), indent=2))
    sys.exit(0)

now = datetime.now(timezone.utc)

def idle(stamp):
    if not stamp:
        return "-"
    secs = int((now - datetime.fromisoformat(stamp.replace("Z", "+00:00"))).total_seconds())
    d, r = divmod(max(secs, 0), 86400)
    h, r = divmod(r, 3600)
    return f"{d}d{h}h" if d else f"{h}h{r // 60}m" if h else f"{r // 60}m"

rows = sorted(data, key=lambda s: (s.get("state") or "", s["metadata"]["name"]))
width = max(len(s["metadata"]["name"]) for s in rows)
print(f"{'NAME':<{width}}  {'STATE':<8} {'STATUS':<9} {'IDLE':<7} LAST USED")
for s in rows:
    name = s["metadata"]["name"]
    state = s.get("state") or "?"
    status = s.get("status") or "?"
    used = s.get("lastUsedAt")
    print(f"{name:<{width}}  {state:<8} {status:<9} {idle(used):<7} {used or '-'}")
PY
