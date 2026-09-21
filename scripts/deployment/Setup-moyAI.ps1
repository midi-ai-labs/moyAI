param(
  [string]$InstallRoot = (Join-Path $env:LOCALAPPDATA 'Programs/moyAI'),
  [switch]$Uninstall,
  [switch]$CheckOnly,
  [switch]$NoShortcuts
)
$ErrorActionPreference = 'Stop'
try {
  . (Join-Path $PSScriptRoot 'common.ps1')
  $sourceRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot)).TrimEnd('\', '/')
  $InstallRoot = [IO.Path]::GetFullPath($InstallRoot).TrimEnd('\', '/')
  if ($InstallRoot -eq [IO.Path]::GetPathRoot($InstallRoot) -or $InstallRoot -eq $env:USERPROFILE -or $InstallRoot -eq $env:LOCALAPPDATA) { throw '導入先にはmoyAI専用のフォルダーを指定してください。通常は導入先を指定せず、Setup-moyAI.cmdを開いてください。' }
  Assert-MoyaiRegularPath (Split-Path -Parent $InstallRoot) $InstallRoot
  $receiptPath = Join-Path $InstallRoot 'installation.json'
  $receipt = $null
  if (Test-Path -LiteralPath $InstallRoot) {
    if (-not (Test-Path -LiteralPath $receiptPath -PathType Leaf)) { throw '導入先に、このセットアップで導入した記録がありません。既存ファイルは変更していません。新しい専用フォルダーを指定するか、元の導入先を配布担当へ確認してください。' }
    try { $receipt = Get-Content -LiteralPath $receiptPath -Raw -Encoding UTF8 | ConvertFrom-Json }
    catch { throw "導入記録を読み込めません。記録を削除せず、配布担当へ確認してください。対象: $receiptPath`n詳細: $($_.Exception.Message)" }
    if ($receipt.product -ne 'moyai-windows-user' -or $receipt.install_root -ine $InstallRoot) { throw '導入記録と現在のフォルダーが一致しません。元の導入先から実行するか、配布担当へ確認してください。対象: installation.json' }
  } elseif ($Uninstall) { throw '指定した導入先にmoyAIがありません。アンインストールする導入先を確認してください。' }
  if ($sourceRoot -eq $InstallRoot -and -not $Uninstall) { throw '導入済みフォルダーから更新はできません。新しいZIPを別のフォルダーへ展開し、そちらのSetup-moyAI.cmdを開いてください。' }
  if ($sourceRoot.StartsWith($InstallRoot + '\', [StringComparison]::OrdinalIgnoreCase) -and -not $Uninstall) { throw '新しいZIPが導入済みフォルダーの内側にあります。導入先の外へ展開し直し、そちらのSetup-moyAI.cmdを開いてください。' }
  $runningNotice = Get-MoyaiRunningAppNotice $InstallRoot @(Get-Process -ErrorAction SilentlyContinue)
  if ($runningNotice) { throw $runningNotice }
  $shortcutFolder = Join-Path ([Environment]::GetFolderPath('Programs')) 'moyAI'
  if ($Uninstall) {
    $installed = Read-MoyaiDeployment $InstallRoot -AllowMissingFiles
    $autostart = @(Remove-MoyaiAutostart $InstallRoot $receipt $installed -Preview:$CheckOnly)
    foreach ($registration in $autostart) { Write-Output "サインイン時の自動起動$(if ($CheckOnly) { 'の解除予定' } else { 'を解除しました' }): $($registration.name)" }
    $association = @(Remove-MoyaiJoinFileAssociation $InstallRoot $receipt $installed -Preview:$CheckOnly)
    foreach ($path in $association) { Write-Output "接続ファイルの関連付け$(if ($CheckOnly) { 'の解除予定' } else { 'を解除しました' }): $path" }
    if ($CheckOnly) { Write-Output "確認のみ完了しました。アプリの削除予定先: $InstallRoot`n設定・履歴と、導入後に追加・変更したファイルは残します。まだ削除していません。"; exit 0 }
    foreach ($file in $installed.files) {
      $path = Resolve-MoyaiChild $InstallRoot $file.path
      # Do not erase a file replaced by the user after installation.
      if ((Test-Path -LiteralPath $path -PathType Leaf) -and (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ieq $file.sha256) { Remove-Item -LiteralPath $path -Force }
    }
    foreach ($name in @('deployment.json', 'installation.json')) { Remove-Item -LiteralPath (Join-Path $InstallRoot $name) -Force }
    $directories = @{}
    foreach ($file in $installed.files) {
      $directory = Split-Path -Parent (Resolve-MoyaiChild $InstallRoot $file.path)
      while ($directory.Length -gt $InstallRoot.Length) { $directories[$directory] = $true; $directory = Split-Path -Parent $directory }
    }
    foreach ($directory in @($directories.Keys | Sort-Object { $_.Length } -Descending)) {
      Assert-MoyaiRegularPath $InstallRoot $directory
      if ((Test-Path -LiteralPath $directory) -and @(Get-ChildItem -LiteralPath $directory -Force).Count -eq 0) { Remove-Item -LiteralPath $directory -Force }
    }
    if (@(Get-ChildItem -LiteralPath $InstallRoot -Force).Count -eq 0) { Remove-Item -LiteralPath $InstallRoot -Force }
    if (-not $NoShortcuts) {
      foreach ($name in @('moyAI.lnk', 'moyAI Team Management.lnk')) {
        $path = Join-Path $shortcutFolder $name
        if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
      }
    }
    Write-Output "アンインストールが完了しました。`nアプリ本体と、この配置に一致するRunner・Hubの自動起動を取り除きました。`n設定・ログイン情報・履歴・作業フォルダー、追加・変更したファイル、別の導入先の登録は残しています。`nこの画面を閉じてください。"
    exit 0
  }
  $manifest = Read-MoyaiDeployment $sourceRoot -VerifyHashes
  if (Test-Path -LiteralPath $receiptPath) {
    $installed = Read-MoyaiDeployment $InstallRoot -AllowMissingFiles
    if ($installed.hub -and -not $manifest.hub) { throw '現在の導入先にはチーム管理用のHubがありますが、新しい配布物には含まれていません。配布担当から対応するHub同梱版を受け取り、更新してください。既存のHubと設定は保持しています。' }
  }
  [void](Test-MoyaiRuntime $sourceRoot $manifest -PreparingInstall)
  if ($CheckOnly) { Write-Output "確認のみ完了しました。配布物の照合と必要な実行環境の検査に通りました。`n導入予定先: $InstallRoot`nまだ導入・起動していません。初めて使うPCでの動作や仕事の実行は、この検査だけでは確認できません。"; exit 0 }
  $parent = Split-Path -Parent $InstallRoot
  New-Item -ItemType Directory -Path $parent -Force | Out-Null
  $staging = Resolve-MoyaiChild $parent ('.moyai-stage-' + [Guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Path $staging | Out-Null
  foreach ($file in $manifest.files) {
    $from = Resolve-MoyaiChild $sourceRoot $file.path
    $to = Resolve-MoyaiChild $staging $file.path
    New-Item -ItemType Directory -Path (Split-Path -Parent $to) -Force | Out-Null
    Copy-Item -LiteralPath $from -Destination $to
  }
  Copy-Item -LiteralPath (Join-Path $sourceRoot 'deployment.json') -Destination (Join-Path $staging 'deployment.json')
  [void](Read-MoyaiDeployment $staging -VerifyHashes)
  Initialize-MoyaiFixedRuntimeAccess $staging $manifest
  [void](Test-MoyaiRuntime $staging $manifest)
  $newReceipt = [ordered]@{product='moyai-windows-user'; install_root=$InstallRoot; version=$manifest.version; installed_at_utc=[DateTime]::UtcNow.ToString('o')}
  if ($null -ne $receipt -and (Test-MoyaiJoinFileReceipt $InstallRoot $receipt $installed)) { $newReceipt.join_file_association = $receipt.join_file_association }
  Write-MoyaiUtf8 (Join-Path $staging 'installation.json') ($newReceipt | ConvertTo-Json -Depth 8)
  $previous = $null
  if (Test-Path -LiteralPath $InstallRoot) {
    $previous = Resolve-MoyaiChild $parent ('moyAI.previous-' + [Guid]::NewGuid().ToString('N'))
    Move-Item -LiteralPath $InstallRoot -Destination $previous
  }
  try { Move-Item -LiteralPath $staging -Destination $InstallRoot }
  catch {
    if ($previous -and -not (Test-Path -LiteralPath $InstallRoot)) { Move-Item -LiteralPath $previous -Destination $InstallRoot }
    throw
  }
  Write-Output (Set-MoyaiJoinFileAssociation $InstallRoot)
  if (-not $NoShortcuts) {
    New-Item -ItemType Directory -Path $shortcutFolder -Force | Out-Null
    $shell = New-Object -ComObject WScript.Shell
    $entries = @(@{Name='moyAI'; Arguments=''})
    if ($manifest.hub) { $entries += @{Name='moyAI Team Management'; Arguments=' -TeamManagement'} }
    foreach ($entry in $entries) {
      $shortcut = $shell.CreateShortcut((Join-Path $shortcutFolder ($entry.Name + '.lnk')))
      $shortcut.TargetPath = Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
      $shortcut.Arguments = '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + (Join-Path $InstallRoot 'scripts/Start-moyAI.ps1') + '"' + $entry.Arguments
      $shortcut.WorkingDirectory = $InstallRoot
      $shortcut.IconLocation = (Join-Path $InstallRoot 'app/bin/moyai-desktop.exe') + ',0'
      $shortcut.Save()
    }
    if (-not $manifest.hub) {
      $oldShortcut = Join-Path $shortcutFolder 'moyAI Team Management.lnk'
      if (Test-Path -LiteralPath $oldShortcut) { Remove-Item -LiteralPath $oldShortcut -Force }
    }
  }
  Write-Output "`n導入が完了しました。moyAI $($manifest.version)`n導入先: $InstallRoot"
  if ($NoShortcuts) { Write-Output "次に開くもの: $InstallRoot\Start-moyAI.cmd（ショートカットは作成していません）" }
  else { Write-Output '次に開くもの: Windowsのスタートメニュー → moyAI' }
  Write-Output '・依頼と結果確認: 「チームに参加する」。管理者から接続ファイルを受け取り、PCの参加許可を待ちます。ID・パスワードの入力は不要です。'
  Write-Output '・仕事を実行するPC: 「チームの仕事をこのPCで実行する」。接続ファイルを読み込み、PCの参加、作業の保存先と操作の許可を進めてください。AIはHubの設定を使います。'
  Write-Output '・チームの管理者: Hub同梱版で「チーム環境を用意する」。個人で使う方は「自分のPCで使う」を選んでください。'
  if ($manifest.hub) { Write-Output '・Hubは今回の導入先から起動してください。更新時も同じ導入先を使うと、Windowsの受信許可を引き継げます。初回はHubの画面で接続するLANと受信許可を確認します。' }
  Write-Output '導入完了は、PCの参加承認や最初の仕事の準備完了とは別です。既存の設定と履歴は保持しています。手順: docs/user/windows-setup.md'
  if ($previous) { Write-Output "以前のアプリは次の場所に残しました。必要なファイルがないことを確認してから削除できます: $previous`n設定と履歴はAppDataに残っています。" }
  Write-Output 'この画面を閉じて、上の案内からmoyAIを開いてください。moyAIは自動では起動しません。'
} catch {
  [Console]::Error.WriteLine("セットアップを完了できませんでした。`n`n$($_.Exception.Message)`n`n導入先: $InstallRoot`n解消しない場合は、この表示全体を社内の配布担当へ伝えてください。")
  exit 1
}
