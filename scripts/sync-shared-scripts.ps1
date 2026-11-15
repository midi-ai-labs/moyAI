<#
.SYNOPSIS
Copies the Windows setup scripts shared with moyAI Hub and records their hashes.

.DESCRIPTION
moyAI/scripts/deployment is the only place to edit these files. This script copies them to
moyAI-Hub/scripts and writes shared-scripts.lock.json in both repositories. Each product's
package script checks its own copies against its lock, so a file edited on one side only is
rejected before a release is built.

Hashes use the file content with CRLF line endings normalized to LF. The Git checkout setting
can change line endings on disk; it does not change the recorded content.

-Check compares without writing and exits 1 when anything differs. -Check -LocalOnly checks
only this repository's files against its own lock; the Desktop package script uses it.
#>
param(
    [string]$HubRoot = '',
    [switch]$Check,
    [switch]$LocalOnly
)
$ErrorActionPreference = 'Stop'

$sharedNames = @('component-install.ps1', 'sandbox-service.ps1', 'Setup-Component.ps1')
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
if (-not $HubRoot) { $HubRoot = Join-Path (Split-Path -Parent $repoRoot) 'moyAI-Hub' }
$HubRoot = [IO.Path]::GetFullPath($HubRoot)
$canonicalRoot = Join-Path $repoRoot 'scripts\deployment'
$hubScripts = Join-Path $HubRoot 'scripts'
if ($LocalOnly -and -not $Check) { throw '-LocalOnly は -Check と同時に指定してください。' }
if (-not $LocalOnly -and -not (Test-Path -LiteralPath (Join-Path $hubScripts 'package-release.py') -PathType Leaf)) {
    throw "moyAI Hubのリポジトリが見つかりません: $HubRoot`n-HubRoot でHubのリポジトリを指定してください。"
}

function Get-NormalizedSha256([string]$Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    $normalized = [IO.MemoryStream]::new($bytes.Length)
    for ($i = 0; $i -lt $bytes.Length; $i++) {
        if ($bytes[$i] -eq 13 -and $i + 1 -lt $bytes.Length -and $bytes[$i + 1] -eq 10) { continue }
        $normalized.WriteByte($bytes[$i])
    }
    $sha = [Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($sha.ComputeHash($normalized.ToArray()))).Replace('-', '').ToLowerInvariant() }
    finally { $sha.Dispose(); $normalized.Dispose() }
}

function Get-LockText([hashtable]$Hashes) {
    # Fixed text, so Windows PowerShell 5.1 and PowerShell 7 write identical files.
    $entries = @($sharedNames | ForEach-Object { '    "' + $_ + '": "' + $Hashes[$_] + '"' }) -join ",`n"
    return "{`n  `"schema`": 1,`n  `"canonical`": `"moyAI/scripts/deployment`",`n  `"files`": {`n$entries`n  }`n}`n"
}

function Read-LockText([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $null }
    return ([IO.File]::ReadAllText($Path) -replace "`r`n", "`n")
}

$hashes = @{}
foreach ($name in $sharedNames) {
    $source = Join-Path $canonicalRoot $name
    if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw "共通スクリプトがありません: $source" }
    $hashes[$name] = Get-NormalizedSha256 $source
}
$lockText = Get-LockText $hashes
$locks = @(Join-Path $canonicalRoot 'shared-scripts.lock.json')
if (-not $LocalOnly) { $locks += Join-Path $hubScripts 'shared-scripts.lock.json' }

$differences = [Collections.Generic.List[string]]::new()
foreach ($name in $(if ($LocalOnly) { @() } else { $sharedNames })) {
    $copy = Join-Path $hubScripts $name
    if (-not (Test-Path -LiteralPath $copy -PathType Leaf)) { $differences.Add("Hubにありません: $copy") }
    elseif ((Get-NormalizedSha256 $copy) -cne $hashes[$name]) { $differences.Add("Hubの内容が異なります: $copy") }
}
foreach ($lock in $locks) {
    if ((Read-LockText $lock) -cne $lockText) { $differences.Add("記録が現在の内容と一致しません: $lock") }
}

if ($Check) {
    if ($differences.Count) {
        foreach ($line in $differences) { Write-Output $line }
        Write-Output "同期するには、moyAIで scripts\sync-shared-scripts.ps1 を実行してください。Hub側のファイルは直接編集しないでください。"
        exit 1
    }
    Write-Output '共通スクリプトはDesktopとHubで一致しています。'
    exit 0
}

foreach ($name in $sharedNames) {
    $copy = Join-Path $hubScripts $name
    if (-not (Test-Path -LiteralPath $copy -PathType Leaf) -or (Get-NormalizedSha256 $copy) -cne $hashes[$name]) {
        Copy-Item -LiteralPath (Join-Path $canonicalRoot $name) -Destination $copy -Force
        Write-Output "Hubへコピーしました: $name"
    }
}
foreach ($lock in $locks) {
    if ((Read-LockText $lock) -cne $lockText) {
        [IO.File]::WriteAllText($lock, $lockText, [Text.UTF8Encoding]::new($false))
        Write-Output "記録を更新しました: $lock"
    }
}
Write-Output '共通スクリプトを同期しました。両方のリポジトリで変更を確認し、commitしてください。'
