# Sourced by scripts/build.sh.

# A hash of every file the Linux sandbox binaries are built from (the whole
# crates/ tree, the lockfile, cargo config, sandbox assets, and the build
# script), tracked or not, minus deleted files. Hashes file contents, so
# touching a file or switching branches back and forth does not rebuild.
sandbox_source_hash() {
    local paths=(crates Cargo.toml Cargo.lock .cargo assets/sandbox scripts/build-sandbox-binaries.sh)
    local sha=(shasum -a 256)
    command -v shasum >/dev/null 2>&1 || sha=(sha256sum)
    comm -23 \
        <(git ls-files -co --exclude-standard -- "${paths[@]}" | sort -u) \
        <(git ls-files -d -- "${paths[@]}" | sort -u) \
        | tr '\n' '\0' | xargs -0 "${sha[@]}" | "${sha[@]}" | cut -c1-16
}

# The data root tod itself would use when given no --data-root: TOD_DATA_ROOT,
# else `data_root` in install.toml in the OS config dir (TOD_CONFIG_DIR
# overrides it). Read only: install.toml is the user's and is never written.
# Prints nothing when there is none.
default_data_root() {
    if [[ -n "${TOD_DATA_ROOT:-}" ]]; then printf '%s\n' "$TOD_DATA_ROOT"; return; fi
    local dir
    if [[ -n "${TOD_CONFIG_DIR:-}" ]]; then dir="$TOD_CONFIG_DIR"
    elif [[ -n "${APPDATA:-}" ]]; then dir="$APPDATA/tod"
    elif [[ "$(uname -s)" == Darwin ]]; then dir="$HOME/Library/Application Support/tod"
    else dir="${XDG_CONFIG_HOME:-$HOME/.config}/tod"
    fi
    [[ -f "$dir/install.toml" ]] || return 0
    sed -n 's/^data_root *= *"\(.*\)" *$/\1/p' "$dir/install.toml" | head -1
}
