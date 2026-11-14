#!/usr/bin/env bash
# Fail when the test suite leaves anything in the system temp dir.
#
# Tests that make directories under `std::env::temp_dir()` and do not remove
# them piled up 82,000 directories (110 GB) and filled a disk. This runs
# `cargo nextest run` with TMPDIR pointed at a fresh, empty directory and lists
# whatever is left in it afterwards; any leftover (or any failing test) fails
# the script. New tests take their directories from `tod_test_util::scratch_dir`
# (removed when the test's thread ends, even on failure) or own a
# `tempfile::TempDir`.
#
# Usage:
#   scripts/check-test-leaks.sh                      # the whole workspace
#   scripts/check-test-leaks.sh -p tod-core -p tod-store
#   scripts/check-test-leaks.sh --workspace --exclude tod-ui
#
# Arguments are passed to `cargo nextest run` (default: `--workspace`), so the
# usual `-p`, `--exclude` and `-E` filters work. `--no-fail-fast` is always set.
#
# Environment:
#   TOD_LEAKCHECK_BASE   where the fresh directory is made (default /tmp). It
#                        must be outside any git checkout: some tests look for
#                        a repository from the temp dir upward.
#   TOD_LEAKCHECK_KEEP=1 keep the fresh directory afterwards, to look at it.
#
# Exclusions are explicit: anything matching an EXPECTED pattern below is not
# counted, and each must say why nothing can remove it.
set -u

EXPECTED=(
  # (none) Every test cleans up after itself. Add `'glob'` entries here only
  # with a comment on who creates the entry and why it cannot be removed.
)

base="${TOD_LEAKCHECK_BASE:-/tmp}"
if git -C "$base" rev-parse --git-dir >/dev/null 2>&1; then
  echo "check-test-leaks: $base is inside a git checkout; set TOD_LEAKCHECK_BASE to a directory outside any" >&2
  exit 2
fi
scratch="$(mktemp -d "$base/tod-leakcheck.XXXXXX")" || exit 2
scratch="$(cd "$scratch" && pwd -P)" # canonical (macOS: /tmp is /private/tmp), as std::env::temp_dir reports it
cleanup() { [ "${TOD_LEAKCHECK_KEEP:-0}" = 1 ] || rm -rf "$scratch"; }
trap cleanup EXIT

if [ "$#" -eq 0 ]; then
  set -- --workspace
fi

echo "check-test-leaks: running tests with TMPDIR=$scratch"
TMPDIR="$scratch" cargo nextest run --no-fail-fast "$@"
status=$?

# `find` rather than `ls`: dotfiles (tempfile's own `.tmpXXXXXX`) count too.
leftovers=()
while IFS= read -r entry; do
  [ -n "$entry" ] || continue
  name="${entry#"$scratch"/}"
  skip=0
  for pattern in ${EXPECTED[@]+"${EXPECTED[@]}"}; do
    # shellcheck disable=SC2254
    case "$name" in $pattern) skip=1 ;; esac
  done
  [ "$skip" = 1 ] || leftovers+=("$name")
done < <(find "$scratch" -mindepth 1 -maxdepth 1 | sort)

if [ "${#leftovers[@]}" -gt 0 ]; then
  echo
  echo "check-test-leaks: ${#leftovers[@]} entries left in the temp dir:" >&2
  printf '%s\n' "${leftovers[@]}" | sed -E 's/-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f-]*//; s/[-.][0-9]+(-[0-9]+)*$//; s/\.tmp[A-Za-z0-9]{6}$/.tmp*/' | sort | uniq -c | sort -rn | sed 's/^/  /' >&2
  echo "check-test-leaks: first few (full names):" >&2
  printf '  %s\n' "${leftovers[@]:0:10}" >&2
  echo "check-test-leaks: use tod_test_util::scratch_dir(..) or a tempfile::TempDir in those tests" >&2
  exit 1
fi

if [ "$status" -ne 0 ]; then
  echo "check-test-leaks: nothing left behind, but the tests failed (exit $status)" >&2
  exit "$status"
fi
echo "check-test-leaks: nothing left behind"
