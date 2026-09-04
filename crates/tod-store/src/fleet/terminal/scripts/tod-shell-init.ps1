param(
    [Parameter(Mandatory)][string]$TodShellId,
    [Parameter(Mandatory)][string]$TodStateDir,
    [string]$TodCwd = ""
)

$ErrorActionPreference = 'Stop'

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class TodWin32 {
  [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow();
}
"@

$shellPid = $PID
$hwnd = [int64]0
$backend = $env:TOD_TERMINAL_BACKEND
if (-not $backend) { $backend = "windows" }

$p = Get-Process -Id $shellPid -ErrorAction SilentlyContinue
$walk = $p
while ($null -ne $walk) {
    if ($walk.ProcessName -eq 'WindowsTerminal') { break }
    try { $walk = Get-Process -Id $walk.Parent.Id -ErrorAction Stop } catch { $walk = $null }
}
if ($null -ne $walk -and $walk.MainWindowHandle -ne [IntPtr]::Zero) {
    $hwnd = [int64]$walk.MainWindowHandle
}
else {
    $console = [TodWin32]::GetConsoleWindow()
    if ($console -ne [IntPtr]::Zero) { $hwnd = [int64]$console }
}

$obj = @{
    pid     = $shellPid
    hwnd    = $hwnd
    backend = $backend
}
$json = ($obj | ConvertTo-Json -Compress)
$tmp = Join-Path $TodStateDir "$TodShellId.json.tmp"
$out = Join-Path $TodStateDir "$TodShellId.json"
[System.IO.File]::WriteAllText($tmp, $json)
Move-Item -Force $tmp $out | Out-Null

if ($TodCwd -and (Test-Path -LiteralPath $TodCwd)) {
    Set-Location -LiteralPath $TodCwd
}

$envHook = Join-Path $TodStateDir "env.ps1"
if (Test-Path -LiteralPath $envHook) {
    . $envHook
}
