# Download problem journeys that users submitted as bundles. Builds and runs
# `tod-journeys pull`, which fetches every bundle waiting on the relay inbox,
# decrypts it, and files it as <home>\received\<bundle-id>.journey.
#
#   scripts\pull-journeys.ps1              # fetch what is waiting, then exit
#   scripts\pull-journeys.ps1 -Watch       # fetch, then keep listening
#   scripts\pull-journeys.ps1 -JourneysHome DIR   # use a different tod-journeys home
#
# One-time setup: `cargo run -p tod-journeys -- init` (prints the relay code
# to paste into tod's settings). See doc/journeys/spec.md section 9.5.
param([switch]$Watch, [string]$JourneysHome)
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$pull = @('pull')
if (-not $Watch) { $pull += '--once' }
if ($JourneysHome) { $pull += @("--home", $JourneysHome) }

cargo run --release -q -p tod-journeys -- @pull
