#!/usr/bin/env bash
# Build everything tod runs beside it, then run tod. The binaries land in the
# same target/{profile}/ dir, so tod-cli and the cloud-sandbox tools
# (tod-sandbox, tod-zed-shim) are automatically siblings of tod -- no copying
# needed for dev. Any args are passed through to `tod`, e.g.:
#   scripts/dev.sh --data-root .local/test/sandbox --agent mock --no-focus
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

cargo build --profile dev-fast -p tod -p tod-cli -p tod-sandbox-cli -p tod-zed-shim

# The relay that runs inside each cloud sandbox: a static Linux binary,
# cross-built (rust-lld links it; see .cargo/config.toml). tod finds it in
# target/x86_64-unknown-linux-musl/release/. Without it everything but cloud
# sandboxes still works, so a failure only warns.
RELAY_TARGET=x86_64-unknown-linux-musl
if rustup target add "$RELAY_TARGET" >/dev/null 2>&1 \
    && cargo build --release -p tod-relay --target "$RELAY_TARGET"; then
    :
else
    echo "warning: could not build tod-relay; cloud sandboxes will not work" >&2
fi

cargo run --profile dev-fast -p tod -- "$@"
