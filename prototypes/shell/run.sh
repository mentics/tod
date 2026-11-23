#!/bin/sh
set -eu

source_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
build_dir=$(mktemp -d "${TMPDIR:-/tmp}/tod-shell.XXXXXX")
trap 'rm -rf "$build_dir"' EXIT
swiftc -module-cache-path "$build_dir/cache" "$source_dir"/*.swift -o "$build_dir/Tod Shell"

if [ "${1:-}" = "--smoke-test" ]; then
    "$build_dir/Tod Shell" "$@"
    exit
fi

app="$source_dir/../../.local/agent/scratchpad/tod-shell/Tod Shell.app"
mkdir -p "$app/Contents/MacOS"
cp "$source_dir/Info.plist" "$app/Contents/Info.plist"
mv -f "$build_dir/Tod Shell" "$app/Contents/MacOS/Tod Shell"
open "$app" --args "$@"
