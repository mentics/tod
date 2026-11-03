# Sourced by scripts/build.sh.

# The workspace crates the Linux sandbox binaries (tod-relay, tod-supervisor,
# tod-orchestrator, tod-cli, tod-watchdog) are built from: the packages that
# own those binaries and every workspace crate they depend on (normal and
# build dependencies; dev-only ones never reach a binary). A change anywhere
# else, such as the UI, cannot change what is deployed, so it is not hashed.
#
# $1 picks the artifact: `orchestrator` (tod-orchestrator and tod-watchdog),
# `node` (what a node sandbox runs: tod-supervisor, tod-relay, tod-cli), or
# `all` (the default, every sandbox binary).
sandbox_crate_dirs() {
    local group="${1:-all}"
    case "$group" in
        orchestrator) group="tod-orchestrator tod-watchdog" ;;
        node) group="tod-relay tod-supervisor tod-cli" ;;
        all) group="tod-relay tod-supervisor tod-orchestrator tod-cli tod-watchdog" ;;
        *) echo "sandbox_crate_dirs: unknown artifact '$group'" >&2; return 1 ;;
    esac
    cargo metadata --format-version 1 --locked --offline 2>/dev/null | BINS="$group" python3 -c '
import json, os, sys
m = json.load(sys.stdin)
bins = set(os.environ["BINS"].split())
pkgs = {p["id"]: p for p in m["packages"]}
members = set(m["workspace_members"])
nodes = {n["id"]: n for n in m["resolve"]["nodes"]}
todo = [i for i in members if any(b in bins and "bin" in b_kinds
        for t in pkgs[i]["targets"] for b, b_kinds in [(t["name"], t["kind"])])]
if not todo:
    sys.exit("no package owns the sandbox binaries")
seen = set()
while todo:
    i = todo.pop()
    if i in seen:
        continue
    seen.add(i)
    for d in nodes[i]["deps"]:
        if d["pkg"] in members and any(k["kind"] != "dev" for k in d["dep_kinds"]):
            todo.append(d["pkg"])
root = m["workspace_root"]
for i in sorted(seen):
    print(os.path.relpath(os.path.dirname(pkgs[i]["manifest_path"]), root))
'
}

# A hash of every file the Linux sandbox binaries are built from (the crates
# above, the lockfile, cargo config, sandbox assets, and the build script),
# tracked or not, minus deleted files. Hashes file contents, so touching a file
# or switching branches back and forth does not rebuild.
# $1 is the artifact, as for sandbox_crate_dirs; the shared files (lockfile,
# cargo config, sandbox assets) count for every artifact.
sandbox_source_hash() {
    local group="${1:-all}"
    local paths=(Cargo.toml Cargo.lock .cargo assets/sandbox scripts/build-sandbox-binaries.sh)
    local dirs
    dirs="$(sandbox_crate_dirs "$group")" || return 1
    [[ -n $dirs ]] || { echo "sandbox_source_hash: no crates found" >&2; return 1; }
    # Only what is compiled: a crate's src/, manifest and build script. Its
    # tests/, benches and docs cannot change a binary. (Whether a running
    # orchestrator is compatible is not decided here but by the version
    # contract, `GET /health`; this hash only says whether the Linux binaries
    # need a rebuild.)
    while IFS= read -r d; do paths+=("$d/src" "$d/Cargo.toml" "$d/build.rs"); done <<< "$dirs"
    local sha=(shasum -a 256)
    command -v shasum >/dev/null 2>&1 || sha=(sha256sum)
    comm -23 \
        <(git ls-files -co --exclude-standard -- "${paths[@]}" | sort -u) \
        <(git ls-files -d -- "${paths[@]}" | sort -u) \
        | tr '\n' '\0' | xargs -0 "${sha[@]}" | "${sha[@]}" | cut -c1-16
}

# The data root tod itself would use when given no --data-root: TOD_DATA_ROOT,
# else `data_root` in install.toml in the OS config dir (TOD_CONFIG_DIR
# overrides it). Read only: install.toml is the user's and is never written.
# Prints nothing when there is none.
default_data_root() {
    if [[ -n "${TOD_DATA_ROOT:-}" ]]; then printf '%s\n' "$TOD_DATA_ROOT"; return; fi
    local dir
    if [[ -n "${TOD_CONFIG_DIR:-}" ]]; then dir="$TOD_CONFIG_DIR"
    elif [[ -n "${APPDATA:-}" ]]; then dir="$APPDATA/tod"
    elif [[ "$(uname -s)" == Darwin ]]; then dir="$HOME/Library/Application Support/tod"
    else dir="${XDG_CONFIG_HOME:-$HOME/.config}/tod"
    fi
    [[ -f "$dir/install.toml" ]] || return 0
    sed -n 's/^data_root *= *"\(.*\)" *$/\1/p' "$dir/install.toml" | head -1
}

# A deploy record is "<hash> <short commit> <branch>": only the hash is
# compared; the rest says what was deployed, from where.
recorded_hash() { cut -d' ' -f1 "$1" 2>/dev/null | head -1; }
recorded_from() { cut -d' ' -f2- "$1" 2>/dev/null | head -1; }
record_deploy() {
    printf '%s %s %s\n' "$2" "$(git rev-parse --short HEAD)" "$(git branch --show-current)" > "$1"
}
