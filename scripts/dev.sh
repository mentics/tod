#!/usr/bin/env bash
# Build tod-cli and tod, then run tod. Both binaries land in the same
# target/{profile}/ dir, so tod-cli is automatically a sibling of tod --
# no copying needed for dev. Any args are passed through to `tod`, e.g.:
#   scripts/dev.sh --data-root .local/test/sandbox --agent mock --no-focus
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

cargo build -p tod-cli
# cargo run -p tod -- "$@"
cargo run --profile dev-fast -p tod -- "$@"
