param(
    [Parameter(Mandatory=$true)][string]$SourceRoot,
    [Parameter(Mandatory=$true)][AllowEmptyString()][string]$Components,
    [Parameter(Mandatory=$true)][string]$ExpectedManifestSha256,
    [Parameter(Mandatory=$true)][ValidatePattern('^[a-f0-9]{32}$')][string]$OperationId,
    [switch]$Uninstall,
    [switch]$RemoveSharedRuntime
)
$ErrorActionPreference='Stop'
$messages=[Collections.Generic.List[string]]::new()
$selected=@($Components.Split(',') | Where-Object {-not[string]::IsNullOrWhiteSpace($_)})
try {
    . (Join-Path $PSScriptRoot 'component-install.ps1')
    Invoke-MoyaiMachineComponent -SourceRoot ([IO.Path]::GetFullPath($SourceRoot)) -Components $selected -Uninstall:$Uninstall -RemoveSharedRuntime:$RemoveSharedRuntime -ExpectedManifestSha256 $ExpectedManifestSha256 | ForEach-Object { $messages.Add([string]$_) }
    Write-MoyaiComponentResult -OperationId $OperationId -ManifestSha256 $ExpectedManifestSha256 -Components $selected -Success $true -ExitCode 0 -Message '管理者による配置・削除が完了しました。' -Messages @($messages)
    exit 0
} catch {
    $detail=$_.Exception.Message
    try { Write-MoyaiComponentResult -OperationId $OperationId -ManifestSha256 $ExpectedManifestSha256 -Components $selected -Success $false -ExitCode 1 -Message $detail -Messages @($messages) }
    catch { [Console]::Error.WriteLine(('診断記録を保存できませんでした: '+$_.Exception.Message)) }
    [Console]::Error.WriteLine($detail)
    exit 1
}
