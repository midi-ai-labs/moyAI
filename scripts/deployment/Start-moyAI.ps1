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
    $machineHub = $false
    if (Get-Command Read-MoyaiInstalledComponent -ErrorAction SilentlyContinue) {
      $hub = Read-MoyaiInstalledComponent 'Hub'
      if ($null -ne $hub) { $executable = Join-Path $hub.install_root 'moyai-hub.exe'; $machineHub = $true }
    }
    if ((-not $machineHub -and -not $manifest.hub) -or -not (Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'Hubが導入されていません。Setup-Hub.cmd、またはHub同梱版のSetup-moyAI.cmdでHubを導入してください。仕事を依頼するだけのPCにはHubは不要です。' }
    # --launch exits after opening management; its Hub/Gateway children keep running.
    # PowerShell -Wait would wait for that entire process tree and never finish normally.
    $process = Start-Process -FilePath $executable -ArgumentList '--launch' -WorkingDirectory (Split-Path -Parent $executable) -WindowStyle Hidden -PassThru
    try {
      $null = $process.Handle
      $process.WaitForExit()
      $exitCode = $process.ExitCode
      if ($null -eq $exitCode -or $exitCode -ne 0) { throw "チーム管理用のHubを起動できませんでした。管理者にHubの保存先と稼働状況の確認を依頼してください。保存データの初期化は不要です。詳細: 終了コード $exitCode" }
    } finally { $process.Dispose() }
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
