#!/usr/bin/env bash
# Moves a node host -> dev container -> cloud sandbox -> host through the app's
# own path with a real Claude (crates/tod-core/examples/e2e_move_node.rs), with
# everything it needs made and removed here. Costs a few Claude turns and one
# billed Blaxel sandbox. See doc/cloud-sandboxes/test-image.md.
#
#   scripts/e2e-move.sh <data root with sandboxes.toml for the Blaxel account>
#
# Needs: docker, TOD_TEST_CLAUDE_TOKEN in ./.env (a token made for tests), the
# `tod-test:base` image (built here if missing), target/sandbox/tod-relay
# (scripts/build-sandbox-binaries.sh). Uses a root of its own: the account's
# sandboxes.toml is copied without its Claude token or its sandboxes.
set -euo pipefail
cd "$(dirname "$0")/.."
account_root=${1:?usage: e2e-move.sh <data root holding sandboxes.toml>}
# The git-ignored .env of this checkout, or of the main one when this is a worktree.
env_file=.env
[ -f "$env_file" ] || env_file="$(git rev-parse --git-common-dir)/../.env"
[ -f "$env_file" ] || { echo ".env with TOD_TEST_CLAUDE_TOKEN is needed" >&2; exit 2; }
set -a; . "$env_file"; set +a
: "${TOD_TEST_CLAUDE_TOKEN:?TOD_TEST_CLAUDE_TOKEN is not set in .env}"
export TOD_RELAY_BIN=${TOD_RELAY_BIN:-$PWD/target/sandbox/tod-relay}
[ -f "$TOD_RELAY_BIN" ] || { echo "build the relay first: scripts/build-sandbox-binaries.sh" >&2; exit 2; }

work=.local/agent/scratchpad/e2e-move-$$
ctr=tod-move-ctr-$$
img=tod-move-img-$$
ext=""; case "$(uname -s)" in MINGW*|MSYS*|CYGWIN*) ext=.exe;; esac
mkdir -p "$work/root"
cleanup() { docker rm -f "$ctr" >/dev/null 2>&1 || true; rm -rf "$work"; }
trap cleanup EXIT

grep -v claude_token "$account_root/sandboxes.toml" | sed '/^\[\[sandbox\]\]/,$d' > "$work/root/sandboxes.toml"

cargo build -p tod-cli -p tod-sandbox-cli -q
cargo build -p tod-core --example e2e_move_node -q
cargo build -p tod-store --example e2e_env_bake -q
cp target/debug/tod-cli$ext target/debug/tod-sandbox$ext target/debug/examples/

docker image inspect tod-test:base >/dev/null 2>&1 ||
  docker build -f assets/sandbox/image/Dockerfile -t tod-test:base assets/sandbox

# A repository with an origin, on the host and in the container.
host=$PWD/$work/hostrepo
git init -q --bare "$host-origin.git"
git init -q -b main "$host"
git -C "$host" -c user.email=e2e@example.invalid -c user.name=e2e commit -q --allow-empty -m init
git -C "$host" remote add origin "$host-origin.git"
git -C "$host" push -q origin main
docker run -d --name "$ctr" -e CLAUDE_CODE_OAUTH_TOKEN="$TOD_TEST_CLAUDE_TOKEN" tod-test:base >/dev/null
docker exec "$ctr" sh -c 'git config --global user.email e2e@example.invalid; git config --global user.name e2e;
  git init -q --bare /origin.git; git init -q -b main /work; cd /work; git commit -q --allow-empty -m init;
  git remote add origin /origin.git; git push -q origin main'

# The sandbox image: a repository at /root/app with an origin inside it.
target/debug/examples/e2e_env_bake$ext "$PWD/$work/root" "$img" >/dev/null

target/debug/examples/e2e_move_node$ext "$PWD/$work/root" "$ctr" "sandbox/$img:latest" "$host"
