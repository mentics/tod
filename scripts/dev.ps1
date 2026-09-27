# Build everything tod runs beside it, then run tod. The binaries land in the
# same target\{profile}\ dir, so tod-cli and the cloud-sandbox tools
# (tod-sandbox, tod-zed-shim) are automatically siblings of tod -- no copying
# needed for dev. Any args are passed through to `tod`, e.g.:
#   scripts/dev.ps1 --data-root .local/test/sandbox --agent mock --no-focus
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

cargo build -p tod -p tod-cli -p tod-sandbox-cli -p tod-zed-shim
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# The relay that runs inside each cloud sandbox: a static Linux binary,
# cross-built (rust-lld links it; see .cargo/config.toml). tod finds it in
# target\x86_64-unknown-linux-musl\release\. Without it everything but cloud
# sandboxes still works, so a failure only warns.
$relayTarget = "x86_64-unknown-linux-musl"
& rustup target add $relayTarget *> $null
if ($LASTEXITCODE -eq 0) { & cargo build --release -p tod-relay --target $relayTarget }
if ($LASTEXITCODE -ne 0) { Write-Warning "could not build tod-relay; cloud sandboxes will not work" }

cargo run -p tod -- @args
