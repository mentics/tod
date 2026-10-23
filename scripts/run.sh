#!/usr/bin/env bash
# Brings everything up to date (scripts/build.sh), then runs tod. Args go to
# tod, e.g.:
#   scripts/run.sh --data-root .local/test/sandbox --agent mock --no-focus
# A --data-root among them is also the one build.sh checks the orchestrator for.
# SKIP_BUILD=1 launches without the build step.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Keep credentials in the encrypted file, not the OS keyring: an unsigned dev
# binary is a new app to macOS after every build, so the keychain re-prompts.
# Override with TOD_CREDENTIALS_BACKEND=keyring.
export TOD_CREDENTIALS_BACKEND="${TOD_CREDENTIALS_BACKEND:-file}"

if [[ -z "${SKIP_BUILD:-}" ]]; then
    build_args=()
    prev=""
    for a in "$@"; do
        [[ $prev == --data-root ]] && build_args=(--data-root "$a")
        prev="$a"
    done
    scripts/build.sh ${build_args[@]+"${build_args[@]}"}
fi

exec cargo run --profile dev-fast -p tod -- "$@"
