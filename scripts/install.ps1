# Builds tod in release mode and installs it (binaries + process/media bundles)
# into a target directory, with cloud-sandbox support (tod-sandbox, Zed's ssh
# shim, and the Linux relay that runs in each sandbox).
#
# Usage:
#   scripts\install.ps1 -TargetDir C:\path\to\install
#   scripts\install.ps1 -TargetDir C:\path\to\install -NoAgentSocket   # release-safe, excludes agent-control socket
#   scripts\install.ps1 -TargetDir C:\path\to\install -SandboxWorkspace my-team   # also sign in to Blaxel
#   scripts\install.ps1 -TargetDir C:\path\to\install -NoSandbox       # skip cloud sandboxes
#
param(
    [Parameter(Mandatory = $true)]
    [string]$TargetDir,

    [switch]$NoAgentSocket,

    # Blaxel workspace to set up for cloud sandboxes (`tod-sandbox setup`).
    [string]$SandboxWorkspace,

    [switch]$NoSandbox
)

$ErrorActionPreference = "Stop"

# The relay runs inside Linux sandboxes: a static binary cross-built from this
# machine (rust-lld links it; see .cargo/config.toml), installed as
# sandbox\tod-relay. A failure here leaves tod installed without cloud-sandbox
# support.
function Install-SandboxRelay([string]$RepoRoot, [string]$TargetDir) {
    $target = "x86_64-unknown-linux-musl"
    & rustup target add $target | Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "rustup could not add $target; cloud sandboxes will not work"
        return
    }
    Write-Host "Building: cargo build --release -p tod-relay --target $target"
    & cargo build --release -p tod-relay --target $target
    if ($LASTEXITCODE -ne 0) {
        Write-Warning "building tod-relay failed; cloud sandboxes will not work"
        return
    }
    $dst = Join-Path $TargetDir "sandbox"
    New-Item -ItemType Directory -Force -Path $dst | Out-Null
    Copy-Item -Force -Path (Join-Path $RepoRoot "target\$target\release\tod-relay") -Destination (Join-Path $dst "tod-relay")
    Write-Host "Installed sandbox\tod-relay"
}

$RepoRoot = Split-Path -Parent $PSScriptRoot
Push-Location $RepoRoot
try {
    $cargoArgs = @("build", "--release", "-p", "tod", "-p", "tod-cli")
    if (-not $NoSandbox) {
        $cargoArgs += @("-p", "tod-sandbox-cli", "-p", "tod-zed-shim")
    }
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

    $bins = @("tod.exe", "tod-cli.exe")
    if (-not $NoSandbox) {
        $bins += @("tod-sandbox.exe", "tod-zed-shim.exe")
    }
    foreach ($bin in $bins) {
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

    if (-not $NoSandbox) {
        Install-SandboxRelay -RepoRoot $RepoRoot -TargetDir $TargetDir
    }

    Write-Host ""
    Write-Host "Done. Installed to $TargetDir"
    Write-Host "Run: `"$TargetDir\tod.exe`""
    Write-Host "(First run asks where to store your data and remembers it via install.toml.)"

    if (-not $NoSandbox) {
        $sandbox = Join-Path $TargetDir "tod-sandbox.exe"
        Write-Host ""
        if ($SandboxWorkspace) {
            Write-Host "Setting up cloud sandboxes (workspace $SandboxWorkspace)..."
            & $sandbox setup --workspace $SandboxWorkspace
            if ($LASTEXITCODE -ne 0) {
                Write-Warning "tod-sandbox setup failed. If tod has not chosen a data root yet, run tod once, then: `"$sandbox`" setup --workspace $SandboxWorkspace"
            }
        } else {
            Write-Host "Cloud sandboxes: set the Blaxel workspace in tod's Settings -> Cloud sandboxes"
            Write-Host "  (or run `"$sandbox`" setup --workspace <blaxel-workspace>), sign in once with"
            Write-Host "  bl login <workspace>, then create sandboxes from a node's Files section."
        }
    }
} finally {
    Pop-Location
}
