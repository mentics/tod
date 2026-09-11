# Build tod-cli and tod, then run tod. Both binaries land in the same
# target\{profile}\ dir, so tod-cli is automatically a sibling of tod --
# no copying needed for dev. Any args are passed through to `tod`, e.g.:
#   scripts/dev.ps1 --data-root .local/test/sandbox --agent mock --no-focus
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

cargo build -p tod-cli
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo run -p tod -- @args
