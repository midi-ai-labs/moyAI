param([string]$SourceRoot = (Split-Path -Parent $PSScriptRoot), [switch]$Uninstall, [string]$ExpectedManifestSha256 = '')
$ErrorActionPreference = 'Stop'
try {
    . (Join-Path $PSScriptRoot 'common.ps1')
    Invoke-MoyaiMachineSandboxSetup ([IO.Path]::GetFullPath($SourceRoot)) -Uninstall:$Uninstall -ExpectedManifestSha256 $ExpectedManifestSha256
} catch {
    Write-Error ("PC共通sandbox helperの管理処理を完了できませんでした。`n" + $_.Exception.Message) -ErrorAction Continue
    exit 1
}
