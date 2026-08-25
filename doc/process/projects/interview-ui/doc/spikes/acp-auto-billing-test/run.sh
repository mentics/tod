#!/usr/bin/env bash
# Run the ACP Auto billing spike from repo root or any cwd.
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$DIR"
exec node run.mjs "$@"
