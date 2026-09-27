#!/usr/bin/env bash
# Builds the Linux binaries that run inside a cloud sandbox, into
# target/sandbox/. Run from Windows, macOS, or Linux.
#
# tod-relay has no C dependencies, so rust-lld cross-links it from any host
# (see .cargo/config.toml) with no extra tooling. Any crate that depends on
# tod-store (tod-supervisor, tod-orchestrator, once those crates exist) pulls
# in bundled SQLite, which is C, so cross-compiling it needs a Linux C
# toolchain for x86_64-unknown-linux-musl (and OpenSSL, through reqwest's
# native-tls). This script builds those in a Linux container when Docker is
# available (rust:1-alpine: a native musl toolchain, static OpenSSL), else
# tries `cargo zigbuild`; crates that need it are skipped, with instructions,
# if neither is available.
#
# The container mounts the repository read-only and keeps its own cargo
# target, registry, and git checkouts in Docker volumes (tod-sandbox-target,
# tod-sandbox-cargo, tod-sandbox-cargo-git), so it never touches the host's target/ beyond copying
# the finished binaries into target/sandbox/.
#
# Usage:
#   scripts/build-sandbox-binaries.sh [--release|--debug] [--docker|--no-docker]
#   scripts/build-sandbox-binaries.sh --docker-test <cargo test args...>
#       runs `cargo test <args>` for Linux in the same container (e.g.
#       `--docker-test -p tod-relay`), for code only Linux compiles.
#
# Windows: run this under Git Bash (the same shell scripts/dev.sh and
# scripts/install.sh already assume), not PowerShell — there is no .ps1
# twin. `sh scripts/build-sandbox-binaries.sh` from Git Bash, or
# `bash scripts/build-sandbox-binaries.sh`, both work.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

PROFILE=release
PROFILE_DIR=release
USE_DOCKER=auto
DOCKER_TEST=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --release) PROFILE=release; PROFILE_DIR=release ;;
        --debug) PROFILE=dev; PROFILE_DIR=debug ;;
        --docker) USE_DOCKER=yes ;;
        --no-docker) USE_DOCKER=no ;;
        --docker-test) shift; DOCKER_TEST=("$@"); USE_DOCKER=yes; break ;;
        *) echo "unknown option $1" >&2; exit 1 ;;
    esac
    shift
done

DOCKER_IMAGE="${TOD_SANDBOX_BUILD_IMAGE:-rust:1-alpine}"

# Host path Docker can mount: Git Bash on Windows needs the Windows form.
host_path() {
    if command -v cygpath >/dev/null 2>&1; then cygpath -w "$1"; else echo "$1"; fi
}

# Runs a shell script in the build container with the repository at /src
# (read-only), cargo's target dir and registry in volumes, and target/sandbox
# at /out.
docker_run() {
    local script="$1"
    mkdir -p "$REPO_ROOT/target/sandbox"
    # .cargo/config.toml links the musl target with rust-lld (for cross
    # builds from other hosts); in the container musl is the host, so the
    # linker is its own C toolchain's (CARGO_TARGET_..._LINKER=cc).
    MSYS_NO_PATHCONV=1 docker run --rm \
        -v "$(host_path "$REPO_ROOT"):/src:ro" \
        -v "$(host_path "$REPO_ROOT/target/sandbox"):/out" \
        -v tod-sandbox-target:/target \
        -v tod-sandbox-cargo:/usr/local/cargo/registry \
        -v tod-sandbox-cargo-git:/usr/local/cargo/git \
        -e CARGO_TARGET_DIR=/target \
        -e OPENSSL_STATIC=1 \
        -e OPENSSL_NO_VENDOR=1 \
        -e CARGO_TERM_COLOR=never \
        -e CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=cc \
        -w /src \
        "$DOCKER_IMAGE" sh -euc "
            apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconf git bash perl make >/dev/null
            $script"
}

TARGET=x86_64-unknown-linux-musl
OUT_DIR="$REPO_ROOT/target/sandbox"
mkdir -p "$OUT_DIR"

rustup target add "$TARGET" >/dev/null 2>&1 || true

# name:crate — crates that exist today, plus the ones later waves add. A
# crate not yet in the workspace is skipped so this script keeps working as
# tod-supervisor and tod-orchestrator land.
CANDIDATES=(
    "tod-relay:tod-relay:false"
    "tod-supervisor:tod-supervisor:true"
    "tod-orchestrator:tod-orchestrator:true"
    "tod-cli:tod-cli:true"
    # The hourly watchdog job (a bin of tod-sandbox); rustls pulls in ring (C).
    "tod-watchdog:tod-sandbox:true"
)

HAVE_ZIGBUILD=0
if command -v cargo-zigbuild >/dev/null 2>&1 && command -v zig >/dev/null 2>&1; then
    HAVE_ZIGBUILD=1
fi

HAVE_DOCKER=0
if [[ $USE_DOCKER != no ]] && command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    HAVE_DOCKER=1
fi
if [[ $USE_DOCKER == yes && $HAVE_DOCKER == 0 ]]; then
    echo "error: --docker given but docker is not available (is Docker running?)" >&2
    exit 1
fi

if [[ ${#DOCKER_TEST[@]} -gt 0 ]]; then
    docker_run "cargo test --locked ${DOCKER_TEST[*]}"
    exit 0
fi

# With Docker (and no zigbuild), build every sandbox binary in one container
# run: one cargo invocation shares the dependency graph.
if [[ $HAVE_DOCKER == 1 && ( $USE_DOCKER == yes || $HAVE_ZIGBUILD == 0 ) ]]; then
    PKGS=""
    BINS=""
    for entry in "${CANDIDATES[@]}"; do
        IFS=':' read -r bin_name crate_name _needs_c <<<"$entry"
        if cargo metadata --no-deps --format-version 1 2>/dev/null | grep -q "\"name\":\"$crate_name\""; then
            PKGS="$PKGS -p $crate_name"
            BINS="$BINS $bin_name"
        else
            echo "Skipping $crate_name (crate not in the workspace yet)"
        fi
    done
    echo "Building$BINS for $TARGET in Docker ($DOCKER_IMAGE)"
    docker_run "cargo build --locked --profile $PROFILE $PKGS
        for b in $BINS; do cp -f /target/$PROFILE_DIR/\$b /out/\$b; echo \"Built target/sandbox/\$b\"; done"
    echo
    echo "Done. Sandbox binaries are in $OUT_DIR"
    exit 0
fi

install_bin() {
    local name="$1" src_dir="$2"
    local src="$src_dir/$name"
    if [[ -f "$src" ]]; then
        cp -f "$src" "$OUT_DIR/$name"
        echo "Built $OUT_DIR/$name"
        return 0
    fi
    return 1
}

for entry in "${CANDIDATES[@]}"; do
    IFS=':' read -r bin_name crate_name needs_c <<<"$entry"
    if ! cargo metadata --no-deps --format-version 1 2>/dev/null | grep -q "\"name\":\"$crate_name\""; then
        echo "Skipping $crate_name (crate not in the workspace yet)"
        continue
    fi

    if [[ "$needs_c" == "false" ]]; then
        # No C dependencies: plain cross-compile with rust-lld.
        echo "Building $crate_name for $TARGET (no C dependencies, rust-lld)"
        cargo build --profile "$PROFILE" -p "$crate_name" --target "$TARGET"
        install_bin "$bin_name" "$REPO_ROOT/target/$TARGET/$PROFILE_DIR" \
            || echo "warning: $bin_name not found after building $crate_name" >&2
        continue
    fi

    # Depends on tod-store (bundled SQLite, C): needs a cross C toolchain.
    if [[ $HAVE_ZIGBUILD == 1 ]]; then
        echo "Building $crate_name for $TARGET with cargo zigbuild"
        if cargo zigbuild --profile "$PROFILE" -p "$crate_name" --target "$TARGET"; then
            install_bin "$bin_name" "$REPO_ROOT/target/$TARGET/$PROFILE_DIR" \
                || echo "warning: $bin_name not found after zigbuild" >&2
        else
            echo "warning: cargo zigbuild failed for $crate_name; see the fallback below" >&2
        fi
    else
        cat >&2 <<EOF
warning: $crate_name depends on tod-store (bundled SQLite, C) and cargo-zigbuild
  is not installed, so it cannot be cross-compiled from this host for $TARGET.

  Install zig and cargo-zigbuild, then re-run this script:
    cargo install cargo-zigbuild
    # zig: https://ziglang.org/download/ (or a package manager: brew install zig,
    # choco install zig, apt/dnf/pacman install zig, pip install ziglang)

  Or build it inside a Linux sandbox / container instead:
    tod-sandbox exec <name> -- sh -c 'cd /root/project && cargo build --release -p $crate_name'
  and copy the resulting binary into target/sandbox/$bin_name yourself.
EOF
    fi
done

echo
echo "Done. Sandbox binaries are in $OUT_DIR"
