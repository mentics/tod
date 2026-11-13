#!/usr/bin/env bash
# Builds the Linux binaries that run inside a cloud sandbox, into
# target/sandbox/. Run from Windows, macOS, or Linux.
#
# tod-relay has no C dependencies, so rust-lld cross-links it from any host
# (see .cargo/config.toml) with no extra tooling. Any crate that depends on
# tod-store (tod-supervisor, tod-orchestrator, once those crates exist) pulls
# in bundled SQLite, which is C, so cross-compiling it needs a Linux C
# toolchain for x86_64-unknown-linux-musl (as does ring, through rustls;
# reqwest uses rustls, so there is no OpenSSL). This script builds those with
# `cargo zigbuild` when it is installed, else in a Linux container when Docker
# is available (rust:1-alpine: a native musl toolchain); crates that need it
# are skipped, with instructions, if neither is available.
#
# zigbuild comes first: it compiles natively, in one target directory every
# worktree shares (below). The container is x86_64, so on an Apple Silicon
# host it runs emulated, and its target volume is per checkout, so a new
# worktree compiles every dependency again: an incremental rebuild that
# zigbuild does in seconds takes minutes there. --docker forces the container.
#
# The container mounts the repository read-only and keeps its own cargo
# target, registry, and git checkouts in Docker volumes
# (tod-sandbox-target-<checkout>, tod-sandbox-cargo, tod-sandbox-cargo-git),
# so it never touches the host's target/ beyond copying the finished binaries
# into target/sandbox/. The target volume is per checkout: every checkout is
# mounted at the same /src, and cargo decides a crate is fresh by comparing
# source mtimes, so a target shared between worktrees reuses whichever one
# built last whenever this one's files are older, and ships its code.
#
# The host (non-Docker) cross builds use one cargo target directory shared by
# every checkout (TOD_SANDBOX_TARGET_DIR, default ~/.cache/tod/sandbox-target),
# not each worktree's own target/: the registry crates (typenum, libc, ...) are
# the same in every worktree, so they compile once instead of once per
# worktree. Unlike the Docker volume above, this is safe: on the host each
# checkout has its own path, and a path crate's cargo fingerprint includes it,
# so worktrees never reuse each other's workspace crates.
#
# Usage:
#   scripts/build-sandbox-binaries.sh [--docker|--no-docker]
#   scripts/build-sandbox-binaries.sh --docker-test <cargo test args...>
#       runs `cargo test <args>` for Linux in the same container (e.g.
#       `--docker-test -p tod-relay`), for code only Linux compiles.
#
# Windows: run this under Git Bash (the same shell scripts/run.sh and
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
        # Always a release build (stripped, see [profile.release]): these
        # binaries are uploaded to every sandbox, so there is no debug option.
        --release) ;;
        --docker) USE_DOCKER=yes ;;
        --no-docker) USE_DOCKER=no ;;
        --docker-test) shift; DOCKER_TEST=("$@"); USE_DOCKER=yes; break ;;
        *) echo "unknown option $1" >&2; exit 1 ;;
    esac
    shift
done

DOCKER_IMAGE="${TOD_SANDBOX_BUILD_IMAGE:-rust:1-alpine}"

# This checkout's own target volume (see the header).
checkout_id() {
    if command -v sha256sum >/dev/null 2>&1; then
        printf '%s' "$REPO_ROOT" | sha256sum | cut -c1-12
    else
        printf '%s' "$REPO_ROOT" | shasum -a 256 | cut -c1-12
    fi
}
TARGET_VOLUME="tod-sandbox-target-$(checkout_id)"

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
    # Sandboxes are x86_64 Linux: without --platform, Docker on an arm64 host
    # (Apple Silicon) runs an arm64 container and builds arm64 binaries.
    MSYS_NO_PATHCONV=1 docker run --rm --platform linux/amd64 \
        -v "$(host_path "$REPO_ROOT"):/src:ro" \
        -v "$(host_path "$REPO_ROOT/target/sandbox"):/out" \
        -v "$TARGET_VOLUME:/target" \
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
# One target dir per toolchain: rust-lld and zigbuild compile with different
# settings, so sharing one makes each invalidate the other's dependencies and
# every run recompiles them all.
HOST_TARGET_DIR="${TOD_SANDBOX_TARGET_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/tod/sandbox-target}"
LLD_TARGET_DIR="$HOST_TARGET_DIR/lld"
ZIG_TARGET_DIR="$HOST_TARGET_DIR/zig"
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
# Docker only when asked for, or when zigbuild is not installed (see the header).
if [[ $USE_DOCKER == yes || ( $USE_DOCKER == auto && $HAVE_ZIGBUILD == 0 ) ]] \
    && command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
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

# With Docker (forced, or zigbuild is missing), build every sandbox binary in
# one container run: one cargo invocation shares the dependency graph.
if [[ $HAVE_DOCKER == 1 ]]; then
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

ZIG_PKGS=""
ZIG_BINS=""
for entry in "${CANDIDATES[@]}"; do
    IFS=':' read -r bin_name crate_name needs_c <<<"$entry"
    if ! cargo metadata --no-deps --format-version 1 2>/dev/null | grep -q "\"name\":\"$crate_name\""; then
        echo "Skipping $crate_name (crate not in the workspace yet)"
        continue
    fi

    if [[ "$needs_c" == "false" ]]; then
        # No C dependencies: plain cross-compile with rust-lld.
        echo "Building $crate_name for $TARGET (no C dependencies, rust-lld)"
        CARGO_TARGET_DIR="$LLD_TARGET_DIR" cargo build --profile "$PROFILE" -p "$crate_name" --target "$TARGET"
        install_bin "$bin_name" "$LLD_TARGET_DIR/$TARGET/$PROFILE_DIR" \
            || echo "warning: $bin_name not found after building $crate_name" >&2
        continue
    fi

    # Depends on tod-store (bundled SQLite, C): needs a cross C toolchain.
    if [[ $HAVE_ZIGBUILD == 1 ]]; then
        # Built together below, in one cargo invocation: separate -p sets
        # unify features differently and would recompile shared dependencies.
        case " $ZIG_PKGS " in *" -p $crate_name "*) ;; *) ZIG_PKGS="$ZIG_PKGS -p $crate_name" ;; esac
        ZIG_BINS="$ZIG_BINS $bin_name"
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

if [[ -n $ZIG_PKGS ]]; then
    echo "Building$ZIG_BINS for $TARGET with cargo zigbuild"
    if CARGO_TARGET_DIR="$ZIG_TARGET_DIR" cargo zigbuild --profile "$PROFILE" $ZIG_PKGS --target "$TARGET"; then
        for b in $ZIG_BINS; do
            install_bin "$b" "$ZIG_TARGET_DIR/$TARGET/$PROFILE_DIR" \
                || echo "warning: $b not found after zigbuild" >&2
        done
    else
        echo "error: cargo zigbuild failed for$ZIG_PKGS (--docker builds them in a container instead)" >&2
        exit 1
    fi
fi

echo
echo "Done. Sandbox binaries are in $OUT_DIR"
