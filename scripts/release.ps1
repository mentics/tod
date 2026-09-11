# Build the release binaries for distribution. Both land in target\release\
# as siblings automatically (shared workspace target dir); this script exists
# so a package/install step has one command that guarantees both are present
# and up to date together, rather than building tod alone and shipping a
# stale or missing tod-cli.
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

cargo build --release -p tod-cli
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo build --release -p tod --no-default-features
