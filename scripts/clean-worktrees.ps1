<#
.SYNOPSIS
  Frees disk space taken by Claude desktop worktrees under .claude/worktrees.

.DESCRIPTION
  Dry run by default: prints what it would do. Pass -Apply to act.

  A worktree is IN USE, and never removed, when any of these hold:
    - a non-archived Claude desktop session has it as its worktree or cwd
      (read from %APPDATA%\Claude\claude-code-sessions\...\local_*.json);
    - the app's lease file (%APPDATA%\Claude\git-worktrees.json) leases it to
      a session that is not archived, or to one this script cannot find;
    - git has it locked (subagent worktrees are locked by their claude process);
    - its git index or target/ was written within -RecentHours.

  Everything else is handled as follows:
    - Registered worktree, not in use: removed with `git worktree remove`
      (no --force) when it has no uncommitted or untracked changes and its
      commits are safe: HEAD is on a branch (which is kept) or already in main.
      Otherwise it is reported and left alone.
    - Directory under .claude/worktrees that git no longer knows about and no
      session uses (leftover target/ etc.): deleted.
    - In-use worktree whose session has been idle for -IdleDays: only its
      target/ is deleted. The next build recompiles it.

  Branches are never deleted.

.EXAMPLE
  pwsh scripts/clean-worktrees.ps1            # dry run
  pwsh scripts/clean-worktrees.ps1 -Apply
  pwsh scripts/clean-worktrees.ps1 -Apply -IdleDays 1 -Sizes
#>
[CmdletBinding()]
param(
    [switch]$Apply,
    # Sessions idle at least this long lose their target/ (not their worktree).
    [double]$IdleDays = 2,
    # Anything written this recently is treated as in use.
    [double]$RecentHours = 6,
    # Report directory sizes (slow on large target/ trees).
    [switch]$Sizes,
    [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
)

$ErrorActionPreference = 'Stop'
$worktreesRoot = Join-Path $Repo '.claude\worktrees'
# The Store (MSIX) install keeps its data under Packages\...\LocalCache; only
# processes inside the package see it at %APPDATA%\Claude.
$appDir = @(
    Get-ChildItem (Join-Path $env:LOCALAPPDATA 'Packages') -Directory -Filter 'Claude_*' -ErrorAction SilentlyContinue |
        ForEach-Object { Join-Path $_.FullName 'LocalCache\Roaming\Claude' }
    Join-Path $env:APPDATA 'Claude'
) | Where-Object { Test-Path (Join-Path $_ 'claude-code-sessions') } | Select-Object -First 1
if (-not $appDir) { throw 'Claude desktop app data not found; refusing to guess what is in use.' }
$now = Get-Date

function Norm([string]$p) {
    if (-not $p) { return $null }
    return ([IO.Path]::GetFullPath($p.Replace('/', '\'))).TrimEnd('\').ToLowerInvariant()
}

function Size-Of([string]$p) {
    if (-not $Sizes -or -not (Test-Path -LiteralPath $p)) { return '' }
    $bytes = (Get-ChildItem -LiteralPath $p -Recurse -File -Force -ErrorAction SilentlyContinue |
        Measure-Object Length -Sum).Sum
    return ' ({0:N1} GB)' -f ($bytes / 1GB)
}

function Remove-Dir([string]$p) {
    # rd copes with the long paths deep inside target/ better than Remove-Item.
    cmd /c "rd /s /q `"\\?\$p`"" 2>$null
    if (Test-Path -LiteralPath $p) { throw "could not fully delete $p" }
}

function Recently-Written([string]$p) {
    # Two levels down reaches target/debug/deps, which every build touches.
    if (-not (Test-Path -LiteralPath $p)) { return $false }
    $items = @(Get-Item -LiteralPath $p -Force) +
        @(Get-ChildItem -LiteralPath $p -Force -Depth 1 -ErrorAction SilentlyContinue)
    $newest = ($items | Measure-Object LastWriteTime -Maximum).Maximum
    return ($now - $newest).TotalHours -lt $RecentHours
}

# --- Sessions -----------------------------------------------------------------
$sessions = @{}
$sessionFiles = Get-ChildItem (Join-Path $appDir 'claude-code-sessions') -Recurse -Filter 'local_*.json' -ErrorAction SilentlyContinue
if (-not $sessionFiles) { throw "No Claude desktop session files found under $appDir; refusing to guess what is in use." }
foreach ($f in $sessionFiles) {
    try { $s = Get-Content -LiteralPath $f.FullName -Raw | ConvertFrom-Json } catch { continue }
    if ($s.sessionId) { $sessions[$s.sessionId] = $s }
}

# path -> latest activity (DateTime) of the live sessions using it
$inUse = @{}
function Mark-InUse([string]$path, $when, [string]$why) {
    $k = Norm $path
    if (-not $k -or -not $k.StartsWith((Norm $worktreesRoot) + '\')) { return }
    # Only the worktree directory itself, not a subdirectory a session cd'd into.
    $rel = $k.Substring((Norm $worktreesRoot).Length + 1).Split('\')[0]
    $k = Join-Path (Norm $worktreesRoot) $rel
    if (-not $inUse.ContainsKey($k) -or $inUse[$k].When -lt $when) {
        $inUse[$k] = [pscustomobject]@{ When = $when; Why = $why }
    }
}
foreach ($s in $sessions.Values) {
    if ($s.isArchived) { continue }
    $when = [DateTimeOffset]::FromUnixTimeMilliseconds([int64]$s.lastActivityAt).LocalDateTime
    foreach ($p in @($s.worktreePath, $s.cwd)) { Mark-InUse $p $when "session '$($s.title)'" }
}

$leaseFile = Join-Path $appDir 'git-worktrees.json'
if (Test-Path $leaseFile) {
    $leases = (Get-Content $leaseFile -Raw | ConvertFrom-Json).worktrees
    foreach ($w in $leases.PSObject.Properties.Value) {
        if (-not $w.leasedBy) { continue }
        $s = $sessions[$w.leasedBy]
        if ($s -and $s.isArchived) { continue }
        # A lease to a session we cannot see counts as live.
        $when = if ($s) { [DateTimeOffset]::FromUnixTimeMilliseconds([int64]$s.lastActivityAt).LocalDateTime } else { $now }
        $why = if ($s) { "leased to '$($s.title)'" } else { "leased to unknown session $($w.leasedBy)" }
        Mark-InUse $w.path $when $why
    }
}

# --- Git worktrees ------------------------------------------------------------
$registered = @{}
$cur = $null
foreach ($line in (git -C $Repo worktree list --porcelain)) {
    if ($line -like 'worktree *') {
        $cur = [pscustomobject]@{ Path = $line.Substring(9); Head = $null; Branch = $null; Locked = $null }
        $registered[(Norm $cur.Path)] = $cur
    } elseif ($line -like 'HEAD *') { $cur.Head = $line.Substring(5) }
    elseif ($line -like 'branch *') { $cur.Branch = $line.Substring(7) -replace '^refs/heads/', '' }
    elseif ($line -like 'locked*') { $cur.Locked = ($line -replace '^locked ?', ''); if (-not $cur.Locked) { $cur.Locked = '(no reason)' } }
}

$mode = if ($Apply) { 'APPLY' } else { 'DRY RUN (pass -Apply to act)' }
Write-Host "Worktree cleanup: $mode" -ForegroundColor Cyan
Write-Host "  $($sessions.Count) sessions, $($inUse.Count) worktrees in use, $($registered.Count - 1) registered worktrees`n"

$actions = 0
foreach ($dir in Get-ChildItem -LiteralPath $worktreesRoot -Directory -Force) {
    $k = Norm $dir.FullName
    $name = $dir.Name
    $wt = $registered[$k]
    $use = $inUse[$k]
    $target = Join-Path $dir.FullName 'target'

    # Locked by git: keep, whatever else is true.
    if ($wt -and $wt.Locked) {
        $pidMatch = [regex]::Match($wt.Locked, 'pid (\d+)')
        $alive = $pidMatch.Success -and (Get-Process -Id ([int]$pidMatch.Groups[1].Value) -ErrorAction SilentlyContinue)
        $state = if ($pidMatch.Success -and -not $alive) { 'lock holder has exited; unlock by hand if abandoned' } else { 'locked' }
        Write-Host "KEEP    $name  [${state}: $($wt.Locked)]" -ForegroundColor DarkGray
        continue
    }

    if ($use) {
        $idle = ($now - $use.When).TotalDays
        $targetFresh = Recently-Written $target
        if ((Test-Path -LiteralPath $target) -and $idle -ge $IdleDays -and -not $targetFresh) {
            Write-Host ("TRIM    $name  [{0}, idle {1:N1}d] delete target/{2}" -f $use.Why, $idle, (Size-Of $target)) -ForegroundColor Yellow
            if ($Apply) { Remove-Dir $target }
            $actions++
        } else {
            Write-Host ("KEEP    $name  [{0}, idle {1:N1}d]" -f $use.Why, $idle) -ForegroundColor DarkGray
        }
        continue
    }

    if (-not $wt) {
        # Not a git worktree and no live session uses it: a leftover directory.
        if (Recently-Written $dir.FullName) {
            Write-Host "KEEP    $name  [unregistered, but written in the last $RecentHours h]" -ForegroundColor DarkGray
            continue
        }
        Write-Host "DELETE  $name  [leftover directory, not a git worktree]$(Size-Of $dir.FullName)" -ForegroundColor Yellow
        if ($Apply) { Remove-Dir $dir.FullName }
        $actions++
        continue
    }

    # Registered, unused worktree: remove only if nothing can be lost.
    $gitDir = (git -C $dir.FullName rev-parse --absolute-git-dir 2>$null)
    if ($gitDir -and (Recently-Written (Join-Path $gitDir 'index'))) {
        Write-Host "KEEP    $name  [git index written in the last $RecentHours h]" -ForegroundColor DarkGray
        continue
    }
    $dirty = @(git -C $dir.FullName status --porcelain 2>$null)
    if ($dirty.Count -gt 0) {
        Write-Host "SKIP    $name  [$($dirty.Count) uncommitted/untracked changes; review by hand]" -ForegroundColor Magenta
        continue
    }
    $safe = [bool]$wt.Branch
    if (-not $safe) {
        git -C $Repo merge-base --is-ancestor $wt.Head main 2>$null
        $safe = $LASTEXITCODE -eq 0
    }
    if (-not $safe) {
        Write-Host "SKIP    $name  [detached HEAD with commits not in main]" -ForegroundColor Magenta
        continue
    }
    $keeps = if ($wt.Branch) { "branch $($wt.Branch) kept" } else { 'detached, already in main' }
    Write-Host "REMOVE  $name  [no live session, clean, $keeps]$(Size-Of $dir.FullName)" -ForegroundColor Yellow
    if ($Apply) {
        # Drop target/ first: git's own delete can choke on its long paths.
        if (Test-Path -LiteralPath $target) { Remove-Dir $target }
        git -C $Repo worktree remove $dir.FullName
        if ($LASTEXITCODE -ne 0) { Write-Host "        git refused; left in place" -ForegroundColor Red }
    }
    $actions++
}

if ($Apply) { git -C $Repo worktree prune }
Write-Host "`n$actions action(s) $(if ($Apply) { 'taken' } else { 'would be taken' })."
