#Requires -Version 7.4
param(
  [Parameter(Mandatory)]
  [ValidateSet("ListDesktop", "Capture", "Profile", "StopOwner", "StopProfile", "MatchProfile")]
  [string]$Action,
  [int]$ProcessId = 0,
  [string]$OwnerPath,
  [string]$ProfilePath,
  [string]$ExecutionRoot,
  [string]$CommandLine,
  [string]$ExpectedExecutable,
  [int]$ExpectedParentProcessId = 0
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Assert-ExactChildPath {
  param([Parameter(Mandatory)][string]$Root, [Parameter(Mandatory)][string]$Candidate)
  $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd([IO.Path]::DirectorySeparatorChar)
  $candidatePath = [IO.Path]::GetFullPath($Candidate)
  $relative = [IO.Path]::GetRelativePath($rootPath, $candidatePath)
  if ([IO.Path]::IsPathRooted($relative) -or $relative -eq ".." -or $relative.StartsWith("..$([IO.Path]::DirectorySeparatorChar)", [StringComparison]::Ordinal)) {
    throw "Path escaped execution root: $candidatePath"
  }
  $current = $rootPath
  foreach ($part in @($relative.Split([IO.Path]::DirectorySeparatorChar, [StringSplitOptions]::RemoveEmptyEntries))) {
    $current = Join-Path $current $part
    if (Test-Path -LiteralPath $current) {
      $item = Get-Item -LiteralPath $current -Force
      if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Reparse point is forbidden in execution-owned path: $current"
      }
    }
  }
  return $candidatePath
}

function Get-ExactProcessOwner {
  param([Parameter(Mandatory)][int]$Id, [switch]$AllowExited)
  $process = Get-Process -Id $Id -ErrorAction SilentlyContinue
  $cim = Get-CimInstance Win32_Process -Filter "ProcessId = $Id" -ErrorAction SilentlyContinue
  if ($null -eq $process -or $null -eq $cim) {
    if ($AllowExited) { return $null }
    throw "Process $Id is not live"
  }
  $executable = if ([string]::IsNullOrWhiteSpace([string]$cim.ExecutablePath)) { [string]$process.Path } else { [string]$cim.ExecutablePath }
  if ([string]::IsNullOrWhiteSpace($executable)) { throw "Process $Id has no executable identity" }
  return [ordered]@{
    process_id = [int]$Id
    parent_process_id = [int]$cim.ParentProcessId
    process_start_time_utc_ticks = [string]$process.StartTime.ToUniversalTime().Ticks
    executable_path = [IO.Path]::GetFullPath($executable)
    command_line = [string]$cim.CommandLine
    name = [string]$cim.Name
  }
}

function Get-ValidatedOwner {
  param([Parameter(Mandatory)][object]$Owner, [switch]$AllowExited)
  $live = Get-ExactProcessOwner -Id ([int]$Owner.process_id) -AllowExited:$AllowExited
  if ($null -eq $live) { return $null }
  if ([string]$live.process_start_time_utc_ticks -cne [string]$Owner.process_start_time_utc_ticks) { throw "Process start identity changed" }
  if (-not ([IO.Path]::GetFullPath([string]$live.executable_path)).Equals([IO.Path]::GetFullPath([string]$Owner.executable_path), [StringComparison]::OrdinalIgnoreCase)) { throw "Process executable identity changed" }
  return $live
}

function Get-ProfileRows {
  param([Parameter(Mandatory)][string]$Profile)
  $exact = [IO.Path]::GetFullPath($Profile)
  return @(
    Get-CimInstance Win32_Process -ErrorAction Stop |
      Where-Object {
        [string]$_.Name -ieq "msedgewebview2.exe" -and
        -not [string]::IsNullOrWhiteSpace([string]$_.CommandLine) -and
        (Test-ProfileCommandLine -ProfileRoot $exact -Value ([string]$_.CommandLine))
      } |
      ForEach-Object {
        $owner = Get-ExactProcessOwner -Id ([int]$_.ProcessId) -AllowExited
        if ($null -ne $owner) { $owner }
      }
  )
}

function Test-ProfileCommandLine {
  param([Parameter(Mandatory)][string]$ProfileRoot, [Parameter(Mandatory)][string]$Value)
  $root = [IO.Path]::GetFullPath($ProfileRoot).TrimEnd([IO.Path]::DirectorySeparatorChar)
  $pattern = '(?:^|\s)--user-data-dir=(?:"(?<quoted>[^"]+)"|(?<plain>[^\s"]+))(?=\s|$)'
  foreach ($match in [regex]::Matches($Value, $pattern, [Text.RegularExpressions.RegexOptions]::CultureInvariant)) {
    $raw = if ($match.Groups['quoted'].Success) { $match.Groups['quoted'].Value } else { $match.Groups['plain'].Value }
    try { $candidate = [IO.Path]::GetFullPath($raw).TrimEnd([IO.Path]::DirectorySeparatorChar) } catch { continue }
    $relative = [IO.Path]::GetRelativePath($root, $candidate)
    if ($relative -eq '.' -or $relative -eq '') { return $true }
    if (-not [IO.Path]::IsPathRooted($relative) -and $relative -ne '..' -and -not $relative.StartsWith("..$([IO.Path]::DirectorySeparatorChar)", [StringComparison]::Ordinal)) {
      return $true
    }
  }
  return $false
}

switch ($Action) {
  "ListDesktop" {
    $rows = @(
      Get-CimInstance Win32_Process -ErrorAction Stop |
        Where-Object { [string]$_.Name -ieq "moyai-desktop.exe" } |
        ForEach-Object {
          $owner = Get-ExactProcessOwner -Id ([int]$_.ProcessId) -AllowExited
          if ($null -ne $owner) { $owner }
        }
    )
    ConvertTo-Json -InputObject $rows -Compress -Depth 8
  }
  "Capture" {
    if ($ProcessId -le 0) { throw "Capture requires ProcessId" }
    $owner = Get-ExactProcessOwner -Id $ProcessId
    if (-not [string]::IsNullOrWhiteSpace($ExpectedExecutable) -and -not ([IO.Path]::GetFullPath([string]$owner.executable_path)).Equals([IO.Path]::GetFullPath($ExpectedExecutable), [StringComparison]::OrdinalIgnoreCase)) {
      throw "Captured process executable does not match the expected executable"
    }
    if ($ExpectedParentProcessId -gt 0 -and [int]$owner.parent_process_id -ne $ExpectedParentProcessId) {
      throw "Captured process parent does not match the expected runner"
    }
    ConvertTo-Json -InputObject $owner -Compress -Depth 8
  }
  "Profile" {
    if ([string]::IsNullOrWhiteSpace($ExecutionRoot) -or [string]::IsNullOrWhiteSpace($ProfilePath)) { throw "Profile requires ExecutionRoot and ProfilePath" }
    $profile = Assert-ExactChildPath -Root $ExecutionRoot -Candidate $ProfilePath
    ConvertTo-Json -InputObject @(Get-ProfileRows -Profile $profile) -Compress -Depth 8
  }
  "StopOwner" {
    if ([string]::IsNullOrWhiteSpace($ExecutionRoot) -or [string]::IsNullOrWhiteSpace($OwnerPath)) { throw "StopOwner requires ExecutionRoot and OwnerPath" }
    $ownerFile = Assert-ExactChildPath -Root $ExecutionRoot -Candidate $OwnerPath
    $owner = Get-Content -LiteralPath $ownerFile -Raw -Encoding UTF8 | ConvertFrom-Json
    $live = Get-ValidatedOwner -Owner $owner -AllowExited
    $stopped = $false
    if ($null -ne $live) {
      Stop-Process -Id ([int]$live.process_id) -Force -ErrorAction Stop
      $process = Get-Process -Id ([int]$live.process_id) -ErrorAction SilentlyContinue
      if ($null -ne $process) { $process.WaitForExit(10000) | Out-Null }
      if ($null -ne (Get-ValidatedOwner -Owner $owner -AllowExited)) { throw "Exact owner remained after stop" }
      $stopped = $true
    }
    ConvertTo-Json -InputObject ([ordered]@{ stopped = $stopped; process_id = [int]$owner.process_id }) -Compress -Depth 4
  }
  "StopProfile" {
    if ([string]::IsNullOrWhiteSpace($ExecutionRoot) -or [string]::IsNullOrWhiteSpace($ProfilePath)) { throw "StopProfile requires ExecutionRoot and ProfilePath" }
    $profile = Assert-ExactChildPath -Root $ExecutionRoot -Candidate $ProfilePath
    $stopped = [Collections.Generic.HashSet[int]]::new()
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
      $owners = @(Get-ProfileRows -Profile $profile)
      if ($owners.Count -eq 0) { break }
      foreach ($owner in $owners) {
        $live = Get-ValidatedOwner -Owner $owner -AllowExited
        if ($null -ne $live) {
          Stop-Process -Id ([int]$live.process_id) -Force -ErrorAction Stop
          $process = Get-Process -Id ([int]$live.process_id) -ErrorAction SilentlyContinue
          if ($null -ne $process) { $process.WaitForExit(1000) | Out-Null }
          $stopped.Add([int]$live.process_id) | Out-Null
        }
      }
      Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    $remaining = @(Get-ProfileRows -Profile $profile)
    if ($remaining.Count -ne 0) { throw "Profile owners did not converge to zero" }
    ConvertTo-Json -InputObject ([ordered]@{ stopped_process_ids = @($stopped | Sort-Object); profile = $profile }) -Compress -Depth 4
  }
  "MatchProfile" {
    if ([string]::IsNullOrWhiteSpace($ExecutionRoot) -or [string]::IsNullOrWhiteSpace($ProfilePath)) { throw "MatchProfile requires ExecutionRoot and ProfilePath" }
    $profile = Assert-ExactChildPath -Root $ExecutionRoot -Candidate $ProfilePath
    ConvertTo-Json -InputObject ([ordered]@{ matches = (Test-ProfileCommandLine -ProfileRoot $profile -Value ([string]$CommandLine)); profile = $profile }) -Compress -Depth 4
  }
}
