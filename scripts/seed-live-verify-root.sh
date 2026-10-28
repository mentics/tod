#!/usr/bin/env bash
# Seeds the persistent data root used for live lifecycle verification
# (default ~/.local/share/tod-live-verify) from your real data root, so a node
# can run in the Golden Sandbox exactly as it does for an engineer.
#
# Run it yourself once: it copies credentials, which agents must not read.
# It only READS the real root and never touches install.toml.
#
#   scripts/seed-live-verify-root.sh [REAL_ROOT] [TEST_ROOT]
set -euo pipefail

REAL="${1:-$HOME/.config/tod}"
TEST="${2:-$HOME/.local/share/tod-live-verify}"

[ -d "$REAL" ] || { echo "no real data root at $REAL" >&2; exit 1; }
[ "$REAL" != "$TEST" ] || { echo "real and test roots must differ" >&2; exit 1; }
mkdir -p "$TEST"

# Project config, linear filters, and credentials (keyring entries are shared
# per OS user; credentials/ holds the encrypted-file fallback).
for f in tod.yml linear_filter_presets.json linear_introspection_cache.json orchestrator-github.json; do
  [ -f "$REAL/$f" ] && cp -p "$REAL/$f" "$TEST/$f"
done
[ -d "$REAL/credentials" ] && { mkdir -p "$TEST/credentials"; cp -Rp "$REAL/credentials/." "$TEST/credentials/"; chmod 700 "$TEST/credentials"; }

# tod.yml may point at the real root's storage (fleet_storage_root), which would
# make the test app open the real database and daemon: never carry it over.
sed -i.bak '/^fleet_storage_root:/d' "$TEST/tod.yml" && rm -f "$TEST/tod.yml.bak"
sed -i.bak "s#^treehouse_worktrees_root:.*#treehouse_worktrees_root: $TEST/worktrees#" "$TEST/tod.yml" && rm -f "$TEST/tod.yml.bak"
if grep -qE '^fleet_storage_root:' "$TEST/tod.yml"; then echo "tod.yml still names a storage root" >&2; exit 1; fi

# sandboxes.toml: keep the account ([blaxel], golden) and [[database]] entries,
# drop the per-node [[sandbox]] entries, which belong to the real root's nodes.
python3 - "$REAL/sandboxes.toml" "$TEST/sandboxes.toml" <<'PY'
import re, sys
src = open(sys.argv[1]).read()
blocks = re.split(r'(?m)^(?=\[\[sandbox\]\])', src)
out = [blocks[0]] + [b for b in blocks[1:] if not b.startswith('[[sandbox]]')]
# a [[database]] block follows the last [[sandbox]] block: keep those
keep = [blocks[0]]
for b in blocks[1:]:
    m = re.search(r'(?m)^\[\[database\]\]', b)
    if m:
        keep.append(b[m.start():])
open(sys.argv[2], 'w').write(''.join(keep))
PY

echo "Seeded $TEST"
echo "Golden sandbox: $(grep -E '^golden' "$TEST/sandboxes.toml" || echo '(none)')"
