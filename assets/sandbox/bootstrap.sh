#!/bin/sh
# Prepares a cloud sandbox for tod. `tod-sandbox` uploads and runs this as root
# the first time it connects to a sandbox (and again whenever this file or the
# relay changes); a baked image runs it at build time instead.
#
# Installs only what is missing:
#   - what the relay's clients run: sh, git, curl, tar/gzip (Zed downloads its
#     remote server with curl), scp and sftp-server (Zed uploads files with scp)
#   - with --agents: Node.js 20.10+ (the distribution's when new enough, else
#     the official build in /opt/tod/node), the Claude Code ACP adapter, and
#     Claude Code itself
#
# Idempotent; safe to run again. Works with apk, apt-get, dnf, microdnf, yum.
set -eu

TOD=/opt/tod
AGENTS=0
for arg in "$@"; do
    case "$arg" in
        --agents) AGENTS=1 ;;
        *) echo "bootstrap: unknown option $arg" >&2; exit 2 ;;
    esac
done

mkdir -p "$TOD/bin" "$TOD/logs"

have() { command -v "$1" >/dev/null 2>&1; }

if have apk; then PM=apk
elif have apt-get; then PM=apt
elif have dnf; then PM=dnf
elif have microdnf; then PM=microdnf
elif have yum; then PM=yum
else PM=none
fi

# Package names per manager for each thing we need, by what provides it.
pkg() {
    case "$PM:$1" in
        *:git) echo git ;;
        *:curl) echo curl ca-certificates ;;
        *:tar) echo tar ;;
        *:gzip) echo gzip ;;
        *:bash) echo bash ;;
        *:ps) case "$PM" in apk) echo procps ;; apt) echo procps ;; *) echo procps-ng ;; esac ;;
        apk:scp) echo openssh-client ;;
        apt:scp) echo openssh-client ;;
        *:scp) echo openssh-clients ;;
        apk:sftp-server) echo openssh-sftp-server ;;
        apt:sftp-server) echo openssh-sftp-server ;;
        *:sftp-server) echo openssh-server ;;
        apk:node) echo nodejs npm ;;
        apt:node) echo nodejs npm ;;
        *:node) echo nodejs npm ;;
    esac
}

find_sftp_server() {
    for p in /usr/lib/ssh/sftp-server /usr/lib/openssh/sftp-server \
             /usr/libexec/openssh/sftp-server /usr/libexec/sftp-server; do
        [ -x "$p" ] && { echo "$p"; return 0; }
    done
    return 1
}

missing=""
for cmd in git curl tar gzip bash ps scp; do
    have "$cmd" || missing="$missing $(pkg "$cmd")"
done
find_sftp_server >/dev/null || missing="$missing $(pkg sftp-server)"
# The agent adapter uses import attributes: Node.js 20.10 or later.
node_ok() {
    have node && have npm || return 1
    node -e 'const [a, b] = process.versions.node.split(".").map(Number);
             process.exit(a > 20 || (a === 20 && b >= 10) ? 0 : 1)' 2>/dev/null
}
# Alpine's own is the only one that runs on musl; elsewhere an old or missing
# one is replaced by the official build below.
if [ "$AGENTS" = 1 ] && [ "$PM" = apk ] && ! node_ok; then
    missing="$missing $(pkg node)"
fi

if [ -n "$missing" ]; then
    echo "bootstrap: installing$missing ($PM)"
    case "$PM" in
        apk) apk add --no-cache $missing ;;
        apt) export DEBIAN_FRONTEND=noninteractive
             apt-get update -qq
             apt-get install -y -qq --no-install-recommends $missing ;;
        dnf) dnf install -y -q $missing ;;
        microdnf) microdnf install -y $missing ;;
        yum) yum install -y -q $missing ;;
        none) echo "bootstrap: no known package manager; missing:$missing" >&2; exit 1 ;;
    esac
fi

# One name for sftp-server, wherever the distribution keeps it.
if sftp=$(find_sftp_server); then
    ln -sf "$sftp" "$TOD/bin/sftp-server"
else
    echo "bootstrap: sftp-server not found after install" >&2
    exit 1
fi

# The official Node.js 22 build, in $TOD/node, linked into /usr/local/bin
# (ahead of a distribution's older one on PATH).
install_node() {
    case "$(uname -m)" in
        x86_64|amd64) arch=x64 ;;
        aarch64|arm64) arch=arm64 ;;
        *) echo "bootstrap: no Node.js build for $(uname -m)" >&2; exit 1 ;;
    esac
    base=https://nodejs.org/dist/latest-v22.x
    file=$(curl -fsSL "$base/SHASUMS256.txt" | awk -v want="-linux-$arch.tar.gz" \
        'substr($2, length($2) - length(want) + 1) == want { print $2; exit }')
    if [ -z "$file" ]; then
        echo "bootstrap: could not find Node.js 22 for linux-$arch" >&2
        exit 1
    fi
    echo "bootstrap: installing $file"
    rm -rf "$TOD/node"
    mkdir -p "$TOD/node" /usr/local/bin
    curl -fsSL "$base/$file" | tar -xz -C "$TOD/node" --strip-components=1
    for b in node npm npx; do
        ln -sf "$TOD/node/bin/$b" "/usr/local/bin/$b"
    done
    hash -r 2>/dev/null || true
}

if [ "$AGENTS" = 1 ]; then
    if ! node_ok; then
        if [ "$PM" = apk ]; then
            echo "bootstrap: this Alpine's Node.js is too old for the agent adapter (needs 20.10+)" >&2
            exit 1
        fi
        install_node
        node_ok || { echo "bootstrap: Node.js install failed" >&2; exit 1; }
    fi
    if ! have claude-code-acp; then
        echo "bootstrap: installing @zed-industries/claude-code-acp"
        # Into /usr/local, on PATH, whichever npm this is.
        npm install -g --silent --prefix /usr/local @zed-industries/claude-code-acp
    fi
    # `claude` itself: signing in (`claude /login`, once per sandbox) and
    # terminal agents.
    if ! have claude; then
        echo "bootstrap: installing @anthropic-ai/claude-code"
        npm install -g --silent --prefix /usr/local @anthropic-ai/claude-code
    fi
fi

echo "bootstrap: ready ($PM)"
