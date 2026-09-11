# Builds tod in release mode and installs it (binaries + process/media bundles)
# into a target directory.
#
# Usage:
#   scripts\install.ps1 -TargetDir C:\path\to\install
#   scripts\install.ps1 -TargetDir C:\path\to\install -NoAgentSocket   # release-safe, excludes agent-control socket
#
param(
    [Parameter(Mandatory = $true)]
    [string]$TargetDir,

    [switch]$NoAgentSocket
)

$ErrorActionPreference = "Stop"

$RepoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $RepoRoot
try {
    $cargoArgs = @("build", "--release", "-p", "tod", "-p", "tod-cli")
    if ($NoAgentSocket) {
        $cargoArgs += @("--no-default-features")
    }

    Write-Host "Building: cargo $($cargoArgs -join ' ')"
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build failed with exit code $LASTEXITCODE"
    }

    New-Item -ItemType Directory -Force -Path $TargetDir | Out-Null

    $releaseDir = Join-Path $RepoRoot "target\release"

    foreach ($bin in @("tod.exe", "tod-cli.exe")) {
        $src = Join-Path $releaseDir $bin
        if (Test-Path $src) {
            Copy-Item -Force -Path $src -Destination (Join-Path $TargetDir $bin)
            Write-Host "Installed $bin"
        } else {
            Write-Warning "$bin not found at $src; skipping"
        }
    }

    foreach ($dir in @("process", "media")) {
        $src = Join-Path $releaseDir $dir
        if (Test-Path $src) {
            $dst = Join-Path $TargetDir $dir
            Remove-Item -Recurse -Force -ErrorAction SilentlyContinue -Path $dst
            Copy-Item -Recurse -Force -Path $src -Destination $dst
            Write-Host "Installed $dir\"
        } else {
            Write-Warning "$dir bundle not found at $src; skipping"
        }
    }

    Write-Host ""
    Write-Host "Done. Installed to $TargetDir"
    Write-Host "Run: `"$TargetDir\tod.exe`""
    Write-Host "(First run asks where to store your data and remembers it via install.toml.)"
} finally {
    Pop-Location
}
