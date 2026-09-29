#!/usr/bin/env bash
# Download problem journeys that users submitted as bundles. Builds and runs
# `tod-journeys pull`, which fetches every bundle waiting on the relay inbox,
# decrypts it, and files it as <home>/received/<bundle-id>.journey.
#
#   scripts/pull-journeys.sh              # fetch what is waiting, then exit
#   scripts/pull-journeys.sh --watch      # fetch, then keep listening
#   scripts/pull-journeys.sh --home DIR   # use a different tod-journeys home
#
# One-time setup: `cargo run -p tod-journeys -- init` (prints the relay code
# to paste into tod's settings). See doc/journeys/spec.md section 9.5.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

args=()
once=--once
for a in "$@"; do
    if [ "$a" = "--watch" ]; then once=; else args+=("$a"); fi
done

cargo run --release -q -p tod-journeys -- pull $once "${args[@]}"
