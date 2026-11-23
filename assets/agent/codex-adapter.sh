# Run as the same user who will run the adapter. stdout returns its path.
set -eu
prefix=${1:-${XDG_DATA_HOME:-${HOME:?no home directory}/.local/share}/tod/adapters/codex-acp}
bin=$prefix/node_modules/.bin/codex-acp
cli=$prefix/node_modules/.bin/codex
stamp=$prefix/.checked
existing=$(command -v codex-acp 2>/dev/null || true)

ready() { [ -x "$bin" ] && [ -x "$cli" ]; }
fresh() { ready && [ -f "$stamp" ] && [ -n "$(find "$stamp" -mmin -1440 -print)" ]; }
fallback() {
    printf '%s\n' "$1" >&2
    if [ -x "$bin" ]; then
        printf '%s\n' "$bin"
    elif [ -n "$existing" ] && [ -x "$existing" ]; then
        printf '%s\n' "$existing"
    else
        exit 1
    fi
    exit 0
}

if ! fresh; then
    mkdir -p "$prefix" || fallback "Could not create the TOD Codex adapter directory $prefix."
    if command -v flock >/dev/null 2>&1; then
        exec 9>"$prefix/.install.lock"
        flock -w 180 9 || fallback "Timed out waiting for another Codex ACP installation."
    else
        # macOS and some minimal containers have no flock. Expire locks left
        # by a killed installer after the longest allowed install has ended.
        lock=$prefix/.installing
        attempts=0
        while ! mkdir "$lock" 2>/dev/null; do
            if [ -n "$(find "$lock" -maxdepth 0 -mmin +5 -print 2>/dev/null || true)" ]; then
                rmdir "$lock" 2>/dev/null || true
            fi
            attempts=$((attempts + 1))
            [ "$attempts" -lt 180 ] || fallback "Timed out waiting for another Codex ACP installation."
            sleep 1
        done
        trap 'rmdir "$lock"' EXIT
    fi
    if ! fresh; then
        command -v npm >/dev/null 2>&1 || fallback "Node.js with npm is required to install Codex ACP for TOD."
        cd "$prefix"
        if ! npm install --prefix "$prefix" --no-audit --no-fund --fetch-retries=1 --fetch-timeout=30000 \
            @agentclientprotocol/codex-acp@latest @openai/codex@latest >&2 || ! ready; then
            fallback "npm could not install or update the TOD Codex ACP adapter."
        fi
        touch "$stamp"
    fi
fi

# Container shells can already include this user bin directory. Publish only
# missing commands there so terminal login and older TOD builds can find them.
if [ -z "${1:-}" ] && [ -n "${HOME:-}" ]; then
    user_bin=$HOME/.local/bin
    case ":$PATH:" in
        *":$user_bin:"*)
            if mkdir -p "$user_bin"; then
                for tool in codex-acp codex; do
                    if ! command -v "$tool" >/dev/null 2>&1; then
                        ln -s "$prefix/node_modules/.bin/$tool" "$user_bin/$tool" 2>/dev/null || true
                    fi
                done
            fi
            ;;
    esac
fi
printf '%s\n' "$bin"
