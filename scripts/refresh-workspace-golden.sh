#!/usr/bin/env bash
# Rebuilds the workspace golden (image and sandbox) from the latest origin/main.
# Run by the launchd job from scripts/install-golden-schedule.sh, or by hand.
#
# It keeps its own clone of the workspace repository (never your working copy,
# which may be dirty), moves it to origin/main, and runs that clone's
# `blaxel-sbx golden refresh --if-stale`. The golden's image is built from the
# commit on GitHub, so refreshing the golden always rebuilds the image too; the
# fingerprint includes the commit, so nothing is rebuilt when origin/main has
# not moved. tod's own golden (a fork of this one) follows within 10 minutes
# through the daemon's background check, or at once with `tod-sandbox golden
# refresh <repo>`.
#
# Credentials: BL_API_KEY / BL_WORKSPACE from the environment, else from the
# `.env` of $TOD_WORKSPACE_SRC (default ~/dev/git/workspace); GitHub from
# `gh auth token` (or GH_PAT). A failure exits non-zero, is logged, and raises a
# macOS notification.
set -euo pipefail

SRC="${TOD_WORKSPACE_SRC:-$HOME/dev/git/workspace}"
HOME_DIR="${TOD_GOLDEN_HOME:-$HOME/.cache/tod/workspace-golden}"
CLONE="$HOME_DIR/workspace"
LOG="$HOME_DIR/refresh.log"
mkdir -p "$HOME_DIR"
exec >>"$LOG" 2>&1
echo "==== $(date -u +%FT%TZ) refresh start"

notify_failure() {
  local code=$?
  echo "==== $(date -u +%FT%TZ) refresh FAILED (exit $code)"
  osascript -e 'display notification "Workspace golden refresh failed; see ~/.cache/tod/workspace-golden/refresh.log" with title "tod"' 2>/dev/null || true
  exit "$code"
}
trap notify_failure ERR

export PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

# One refresh at a time; a lock left by a dead run (pid gone) is taken over.
LOCK="$HOME_DIR/lock.d"
if ! mkdir "$LOCK" 2>/dev/null; then
  if kill -0 "$(cat "$LOCK/pid" 2>/dev/null)" 2>/dev/null; then echo "another refresh is running"; exit 0; fi
  rm -rf "$LOCK"; mkdir "$LOCK"
fi
echo $$ >"$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT

envval() { sed -n "s/^$1=//p" "$SRC/.env" 2>/dev/null | tail -1 | sed -e 's/^"//' -e 's/"$//'; }
: "${BL_API_KEY:=$(envval BL_API_KEY)}"
: "${BL_WORKSPACE:=$(envval BL_WORKSPACE)}"
[[ -n "$BL_API_KEY" && -n "$BL_WORKSPACE" ]] || { echo "no BL_API_KEY / BL_WORKSPACE (environment or $SRC/.env)"; false; }
export BL_API_KEY BL_WORKSPACE

origin="$(git -C "$SRC" remote get-url origin)"
if [[ ! -d "$CLONE/.git" ]]; then git clone --quiet "$origin" "$CLONE"; fi
git -C "$CLONE" fetch --quiet origin
git -C "$CLONE" checkout --quiet --force --detach origin/main
git -C "$CLONE" clean -fdq
git -C "$CLONE" submodule sync --quiet
git -C "$CLONE" submodule update --init --force --quiet
echo "at $(git -C "$CLONE" rev-parse --short HEAD)"

"$CLONE/scripts/shell/bin/blaxel-sbx" golden refresh --if-stale
echo "==== $(date -u +%FT%TZ) refresh ok"
