param([switch]$TeamManagement, [switch]$CheckOnly, [string]$JoinConfig = '')
$ErrorActionPreference = 'Stop'
$packageRoot = Split-Path -Parent $PSScriptRoot
try {
  . (Join-Path $PSScriptRoot 'common.ps1')
  $manifest = Read-MoyaiDeployment $packageRoot
  $webview = Test-MoyaiRuntime $packageRoot $manifest -NoWebView:$TeamManagement
  if ($CheckOnly) { Write-Output '必要な実行環境の検査に通りました。まだアプリは起動していません。最初の仕事が実行できるかは、接続と利用準備の後に確認してください。'; exit 0 }
  if ($TeamManagement) {
    $executable = Join-Path $packageRoot 'hub/bin/moyai-hub.exe'
    if (-not $manifest.hub -or -not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'この配布物にはチーム管理用のHubが含まれていません。管理者はHub同梱版を導入するか、既存の管理PCでHubを開いてください。仕事を依頼するだけのPCにはHubは不要です。' }
    $process = Start-Process -FilePath $executable -ArgumentList '--launch' -WorkingDirectory (Split-Path -Parent $executable) -WindowStyle Hidden -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw "チーム管理用のHubを起動できませんでした。管理者にHubの保存先と稼働状況の確認を依頼してください。保存データの初期化は不要です。詳細: 終了コード $($process.ExitCode)" }
  } else {
    $env:WEBVIEW2_BROWSER_EXECUTABLE_FOLDER = $webview
    $executable = Join-Path $packageRoot 'app/bin/moyai-desktop.exe'
    if ($JoinConfig) {
      if (-not (Test-Path -LiteralPath $JoinConfig -PathType Leaf) -or $JoinConfig.Contains('"')) { throw "接続ファイルが見つからないか、保存先の指定が不正です。管理者から受け取った .moyai-join または .toml ファイルを選び直してください。対象: $JoinConfig" }
      $joinPath = (Resolve-Path -LiteralPath $JoinConfig).Path
      Start-Process -FilePath $executable -ArgumentList @('--join-config', ('"' + $joinPath + '"')) -WorkingDirectory (Split-Path -Parent $executable) -WindowStyle Normal | Out-Null
    } else {
      Start-Process -FilePath $executable -WorkingDirectory (Split-Path -Parent $executable) -WindowStyle Normal | Out-Null
    }
  }
} catch {
  $message = "moyAIを起動できませんでした。`n`n$($_.Exception.Message)`n`n配布物の場所: $packageRoot`n解消しない場合は、この表示全体を社内の配布担当へ伝えてください。"
  [Console]::Error.WriteLine($message)
  if (-not $CheckOnly) {
    Add-Type -AssemblyName System.Windows.Forms
    [void][Windows.Forms.MessageBox]::Show($message, 'moyAIの起動', 'OK', 'Error')
  }
  exit 1
}
