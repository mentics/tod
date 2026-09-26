#!/usr/bin/env bash
# Frees disk space taken by Claude desktop worktrees under .claude/worktrees.
# Bash counterpart of clean-worktrees.ps1; the two make the same decisions.
#
# Dry run by default: prints what it would do. Pass --apply to act.
#
# A worktree is IN USE, and never removed, when any of these hold:
#   - a non-archived Claude desktop session has it as its worktree or cwd
#     (read from the app's claude-code-sessions/.../local_*.json);
#   - the app's lease file (git-worktrees.json) leases it to a session that is
#     not archived, or to one this script cannot find;
#   - git has it locked (subagent worktrees are locked by their claude process);
#   - its git index or target/ was written within --recent-hours.
#
# Everything else is handled as follows:
#   - Registered worktree, not in use: removed with `git worktree remove`
#     (no --force) when it has no uncommitted or untracked changes and its
#     commits are safe: HEAD is on a branch (which is kept) or already in main.
#     Otherwise it is reported and left alone.
#   - Directory under .claude/worktrees that git no longer knows about and no
#     session uses (leftover target/ etc.): deleted.
#   - In-use worktree whose session has been idle for --idle-days: only its
#     target/ is deleted. The next build recompiles it.
#
# Branches are never deleted. Needs jq.
#
#   scripts/clean-worktrees.sh                  # dry run
#   scripts/clean-worktrees.sh --apply
#   scripts/clean-worktrees.sh --apply --idle-days 1 --sizes

set -euo pipefail

APPLY=0
IDLE_DAYS=2
RECENT_HOURS=6
SIZES=0
REPO="$(cd "$(dirname "$0")/.." && pwd)"

while [ $# -gt 0 ]; do
    case "$1" in
        --apply) APPLY=1 ;;
        --idle-days) IDLE_DAYS="$2"; shift ;;
        --recent-hours) RECENT_HOURS="$2"; shift ;;
        --sizes) SIZES=1 ;;
        --repo) REPO="$2"; shift ;;
        -h|--help) sed -n '2,29p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }

WT_ROOT="$REPO/.claude/worktrees"
NOW=$(date +%s)
TAB=$(printf '\t')

# Paths as the app and git write them (C:\x or C:/x on Windows), lowercased
# with forward slashes, so both compare equal.
canon() { local p=${1//\\//}; printf '%s' "$p" | tr '[:upper:]' '[:lower:]' | sed 's:/*$::'; }
if command -v cygpath >/dev/null; then
    ROOT_KEY=$(canon "$(cygpath -m "$WT_ROOT")")
else
    ROOT_KEY=$(canon "$WT_ROOT")
fi

# The worktree directory name a path falls under, or nothing.
wt_name() {
    local p rest
    p=$(canon "$1")
    case "$p" in
        "$ROOT_KEY"/*) rest=${p#"$ROOT_KEY"/}; printf '%s' "${rest%%/*}" ;;
    esac
}

# --- App data -----------------------------------------------------------------
# The Windows Store (MSIX) install keeps its data under Packages\...\LocalCache;
# only processes inside the package see it at %APPDATA%\Claude.
APP_DIR=""
candidates=()
if [ -n "${LOCALAPPDATA:-}" ]; then
    la="$LOCALAPPDATA"; command -v cygpath >/dev/null && la=$(cygpath -u "$la")
    for d in "$la"/Packages/Claude_*/LocalCache/Roaming/Claude; do candidates+=("$d"); done
fi
if [ -n "${APPDATA:-}" ]; then
    ad="$APPDATA"; command -v cygpath >/dev/null && ad=$(cygpath -u "$ad")
    candidates+=("$ad/Claude")
fi
candidates+=("$HOME/Library/Application Support/Claude" "${XDG_CONFIG_HOME:-$HOME/.config}/Claude")
for d in "${candidates[@]}"; do
    if [ -d "$d/claude-code-sessions" ]; then APP_DIR="$d"; break; fi
done
[ -n "$APP_DIR" ] || { echo "Claude desktop app data not found; refusing to guess what is in use." >&2; exit 1; }

# sessionId, archived, last activity (epoch s), title, worktreePath, cwd
SESSIONS=$(find "$APP_DIR/claude-code-sessions" -name 'local_*.json' -type f -print0 |
    xargs -0 jq -r 'select(.sessionId) | [.sessionId, (.isArchived // false | tostring),
        ((.lastActivityAt // 0) / 1000 | floor | tostring), (.title // ""),
        (.worktreePath // "" | gsub("\\\\"; "/")), (.cwd // "" | gsub("\\\\"; "/"))] | @tsv' 2>/dev/null |
    tr -d '\r' || true)
[ -n "$SESSIONS" ] || { echo "No Claude desktop session files found under $APP_DIR; refusing to guess what is in use." >&2; exit 1; }

# In-use table: name, epoch s of latest activity, why
IN_USE=""
mark_in_use() {  # path when why
    local n; n=$(wt_name "$1")
    [ -n "$n" ] && IN_USE+="$n$TAB$2$TAB$3"$'\n'
    return 0
}
while IFS="$TAB" read -r id archived when title wtp cwd; do
    [ "$archived" = "true" ] && continue
    mark_in_use "$wtp" "$when" "session '$title'"
    mark_in_use "$cwd" "$when" "session '$title'"
done <<< "$SESSIONS"

LEASES="$APP_DIR/git-worktrees.json"
if [ -f "$LEASES" ]; then
    while IFS="$TAB" read -r leased_by path; do
        [ -n "$leased_by" ] || continue
        row=$(printf '%s\n' "$SESSIONS" | awk -F'\t' -v id="$leased_by" '$1 == id { print; exit }')
        if [ -z "$row" ]; then
            # A lease to a session we cannot see counts as live.
            mark_in_use "$path" "$NOW" "leased to unknown session $leased_by"
        else
            IFS="$TAB" read -r _ archived when title _ _ <<< "$row"
            [ "$archived" = "true" ] && continue
            mark_in_use "$path" "$when" "leased to '$title'"
        fi
    done < <(jq -r '.worktrees[]? | select(.leasedBy) | [.leasedBy, (.path | gsub("\\\\"; "/"))] | @tsv' "$LEASES" |
        tr -d '\r')
fi

in_use_lookup() {  # name -> "when<TAB>why" for its most recent live session
    printf '%s' "$IN_USE" | awk -F'\t' -v n="$1" '$1 == n && (m == "" || $2 > m) { m = $2; w = $3 }
        END { if (m != "") print m "\t" w }'
}

# --- Git worktrees: name, path, head, branch, locked ---------------------------
REGISTERED=$(git -C "$REPO" worktree list --porcelain | awk '
    function flush() { if (path != "") print name "\t" path "\t" head "\t" branch "\t" locked }
    /^worktree / { flush(); path = substr($0, 10); head = ""; branch = ""; locked = "";
                   n = split(path, parts, /[\/\\]/); name = parts[n] }
    /^HEAD /     { head = substr($0, 6) }
    /^branch /   { branch = substr($0, 8); sub(/^refs\/heads\//, "", branch) }
    /^locked/    { locked = substr($0, 8); if (locked == "") locked = "(no reason)" }
    END { flush() }')

registered_lookup() {  # name -> its row, only if it lives under the worktrees root
    local row path
    row=$(printf '%s\n' "$REGISTERED" | awk -F'\t' -v n="$1" '$1 == n { print; exit }')
    [ -n "$row" ] || return 0
    path=$(printf '%s' "$row" | cut -f2)
    [ "$(wt_name "$path")" = "$1" ] && printf '%s' "$row"
    return 0
}

# --- Helpers ------------------------------------------------------------------
recently_written() {  # two levels down reaches target/debug/deps, which every build touches
    [ -e "$1" ] || return 1
    [ -n "$(find "$1" -maxdepth 2 -mmin "-$((RECENT_HOURS * 60))" -print 2>/dev/null | head -n 1)" ]
}

size_of() {
    [ "$SIZES" = 1 ] && [ -e "$1" ] || return 0
    printf ' (%s)' "$(du -sh "$1" 2>/dev/null | cut -f1)"
}

pid_alive() {
    if command -v tasklist >/dev/null; then
        tasklist //FI "PID eq $1" //NH 2>/dev/null | grep -q " $1 "
    else
        kill -0 "$1" 2>/dev/null
    fi
}

remove_dir() {
    rm -rf "$1"
    [ ! -e "$1" ] || { echo "could not fully delete $1" >&2; exit 1; }
}

say() {  # colour text
    local c
    case "$1" in keep) c=90 ;; act) c=33 ;; skip) c=35 ;; err) c=31 ;; head) c=36 ;; *) c=0 ;; esac
    if [ -t 1 ]; then printf '\033[%sm%s\033[0m\n' "$c" "$2"; else printf '%s\n' "$2"; fi
}

# --- Main ---------------------------------------------------------------------
if [ "$APPLY" = 1 ]; then mode="APPLY"; else mode="DRY RUN (pass --apply to act)"; fi
say head "Worktree cleanup: $mode"
n_sessions=$(printf '%s\n' "$SESSIONS" | grep -c .)
n_in_use=$(printf '%s' "$IN_USE" | cut -f1 | sort -u | grep -c . || true)
n_reg=$(( $(printf '%s\n' "$REGISTERED" | grep -c .) - 1 ))
echo "  $n_sessions sessions, $n_in_use worktrees in use, $n_reg registered worktrees"
echo

actions=0
for dir in "$WT_ROOT"/*/; do
    [ -d "$dir" ] || continue
    dir=${dir%/}
    name=$(basename "$dir")
    target="$dir/target"
    wt=$(registered_lookup "$name")
    use=$(in_use_lookup "$name")

    # Locked by git: keep, whatever else is true.
    if [ -n "$wt" ]; then
        locked=$(printf '%s' "$wt" | cut -f5)
        if [ -n "$locked" ]; then
            state="locked"
            pid=$(printf '%s' "$locked" | sed -n 's/.*pid \([0-9][0-9]*\).*/\1/p')
            if [ -n "$pid" ] && ! pid_alive "$pid"; then
                state="lock holder has exited; unlock by hand if abandoned"
            fi
            say keep "KEEP    $name  [$state: $locked]"
            continue
        fi
    fi

    if [ -n "$use" ]; then
        when=$(printf '%s' "$use" | cut -f1)
        why=$(printf '%s' "$use" | cut -f2)
        idle=$(awk -v s=$((NOW - when)) 'BEGIN { printf "%.1f", s / 86400 }')
        if [ -d "$target" ] && awk -v i="$idle" -v d="$IDLE_DAYS" 'BEGIN { exit !(i >= d) }' &&
            ! recently_written "$target"; then
            say act "TRIM    $name  [$why, idle ${idle}d] delete target/$(size_of "$target")"
            [ "$APPLY" = 1 ] && remove_dir "$target"
            actions=$((actions + 1))
        else
            say keep "KEEP    $name  [$why, idle ${idle}d]"
        fi
        continue
    fi

    if [ -z "$wt" ]; then
        # Not a git worktree and no live session uses it: a leftover directory.
        if recently_written "$dir"; then
            say keep "KEEP    $name  [unregistered, but written in the last ${RECENT_HOURS} h]"
            continue
        fi
        say act "DELETE  $name  [leftover directory, not a git worktree]$(size_of "$dir")"
        [ "$APPLY" = 1 ] && remove_dir "$dir"
        actions=$((actions + 1))
        continue
    fi

    # Registered, unused worktree: remove only if nothing can be lost.
    wt_path=$(printf '%s' "$wt" | cut -f2)
    head=$(printf '%s' "$wt" | cut -f3)
    branch=$(printf '%s' "$wt" | cut -f4)
    git_dir=$(git -C "$dir" rev-parse --absolute-git-dir 2>/dev/null || true)
    if [ -n "$git_dir" ] && [ -e "$git_dir/index" ] &&
        [ -n "$(find "$git_dir/index" -mmin "-$((RECENT_HOURS * 60))" 2>/dev/null)" ]; then
        say keep "KEEP    $name  [git index written in the last ${RECENT_HOURS} h]"
        continue
    fi
    dirty=$(git -C "$dir" status --porcelain 2>/dev/null | grep -c . || true)
    if [ "$dirty" -gt 0 ]; then
        say skip "SKIP    $name  [$dirty uncommitted/untracked changes; review by hand]"
        continue
    fi
    if [ -n "$branch" ]; then
        keeps="branch $branch kept"
    elif git -C "$REPO" merge-base --is-ancestor "$head" main 2>/dev/null; then
        keeps="detached, already in main"
    else
        say skip "SKIP    $name  [detached HEAD with commits not in main]"
        continue
    fi
    say act "REMOVE  $name  [no live session, clean, $keeps]$(size_of "$dir")"
    if [ "$APPLY" = 1 ]; then
        # Drop target/ first: git's own delete can choke on its long paths.
        [ -d "$target" ] && remove_dir "$target"
        git -C "$REPO" worktree remove "$wt_path" || say err "        git refused; left in place"
    fi
    actions=$((actions + 1))
done

[ "$APPLY" = 1 ] && git -C "$REPO" worktree prune
echo
if [ "$APPLY" = 1 ]; then echo "$actions action(s) taken."; else echo "$actions action(s) would be taken."; fi
