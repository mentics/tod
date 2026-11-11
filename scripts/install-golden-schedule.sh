#!/usr/bin/env bash
# Installs (or with --remove, removes) a launchd job that runs
# refresh-workspace-golden.sh three times each weekday: 08:00, 12:30, 17:00 local.
set -euo pipefail
LABEL=com.tod.workspace-golden
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
DOMAIN="gui/$(id -u)"
if [[ "${1:-}" == "--remove" ]]; then
  launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
  rm -f "$PLIST"; echo "removed $LABEL"; exit 0
fi
SCRIPT="$(cd "$(dirname "$0")" && pwd)/refresh-workspace-golden.sh"
# A stable copy, so the job survives this worktree being deleted.
DEST="$HOME/.cache/tod/workspace-golden/refresh-workspace-golden.sh"
mkdir -p "$(dirname "$DEST")" "$HOME/Library/LaunchAgents"
cp "$SCRIPT" "$DEST"; chmod +x "$DEST"
{
  echo '<?xml version="1.0" encoding="UTF-8"?>'
  echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
  echo "<plist version=\"1.0\"><dict><key>Label</key><string>$LABEL</string>"
  echo "<key>ProgramArguments</key><array><string>/bin/bash</string><string>$DEST</string></array>"
  echo '<key>StartCalendarInterval</key><array>'
  for t in "8 0" "12 30" "17 0"; do set -- $t
    for wd in 1 2 3 4 5; do
      echo "<dict><key>Weekday</key><integer>$wd</integer><key>Hour</key><integer>$1</integer><key>Minute</key><integer>$2</integer></dict>"
    done
  done
  echo '</array></dict></plist>'
} >"$PLIST"
plutil -lint "$PLIST"
launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
launchctl bootstrap "$DOMAIN" "$PLIST"
echo "installed $LABEL (weekdays 08:00, 12:30, 17:00); log: ~/.cache/tod/workspace-golden/refresh.log"
