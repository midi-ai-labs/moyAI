Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'runtime-access.ps1')

function Resolve-MoyaiChild([string]$Root, [string]$Relative) {
  if ([string]::IsNullOrWhiteSpace($Relative) -or [IO.Path]::IsPathRooted($Relative) -or $Relative.Contains(':')) {
    throw "配布物に使用できない保存先が含まれています。配布担当から一式を受け取り直してください。対象: $Relative"
  }
  if (@($Relative -split '[\\/]' | Where-Object { $_ -in @('', '.', '..') }).Count -ne 0) { throw "配布物の保存先の記載が不正です。配布担当から一式を受け取り直してください。対象: $Relative" }
  $boundary = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
  $result = [IO.Path]::GetFullPath((Join-Path $boundary $Relative))
  if (-not $result.StartsWith($boundary, [StringComparison]::OrdinalIgnoreCase)) { throw "配布物の保存先が指定フォルダーの外を指しています。配布担当へ確認してください。対象: $Relative" }
  return $result
}

function Assert-MoyaiRegularPath([string]$Root, [string]$Path) {
  $boundary = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
  $current = [IO.Path]::GetFullPath($Path)
  while ($current.Length -ge $boundary.Length) {
    if (Test-Path -LiteralPath $current) {
      $item = Get-Item -LiteralPath $current -Force
      if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "配布物または導入先に別フォルダーへのリンクがあります。リンクを使わない専用フォルダーへ展開・導入してください。対象: $current" }
    }
    if ($current -eq $boundary) { return }
    $current = Split-Path -Parent $current
  }
  throw "指定フォルダーの外への操作はできません。配布物と導入先を確認してください。対象: $Path"
}

function Read-MoyaiDeployment([string]$Root, [switch]$VerifyHashes, [switch]$AllowMissingFiles) {
  $manifestPath = Join-Path $Root 'deployment.json'
  Assert-MoyaiRegularPath $Root $manifestPath
  try { $manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json }
  catch { throw "配布物の構成情報を読み込めません。ZIPを一式すべて展開し直してください。対象: $manifestPath`n詳細: $($_.Exception.Message)" }
  if ($manifest.schema -ne 1 -or $manifest.product -ne 'moyai-windows-user') { throw 'この配布物の形式には対応していません。配布担当から対応するWindows版を受け取ってください。対象: deployment.json' }
  if ($manifest.target -ne 'windows-x86_64') { throw 'このPCにはWindows x64版の配布物が必要です。配布担当へ確認してください。' }
  if ($manifest.runtime.webview2 -notin @('bundled-fixed', 'system-installed') -or $manifest.runtime.visual_cpp -notin @('app-local', 'system-installed')) { throw '配布物の実行環境の指定に対応していません。配布担当へ確認してください。対象: deployment.json' }
  $seen = @{}
  foreach ($file in $manifest.files) {
    $path = Resolve-MoyaiChild $Root $file.path
    if ($file.path -in @('deployment.json', 'installation.json')) { throw "配布物の一覧に保護対象のファイルが含まれています。配布担当へ確認してください。対象: $($file.path)" }
    if ($seen.ContainsKey($path)) { throw "配布物の一覧に同じファイルが重複しています。配布担当へ確認してください。対象: $($file.path)" }
    $seen[$path] = $true
    Assert-MoyaiRegularPath $Root $path
    $present = Test-Path -LiteralPath $path -PathType Leaf
    if (-not $present -and -not $AllowMissingFiles) { throw "配布物のファイルが不足しています。ZIPを一式すべて展開し直してください。対象: $($file.path)" }
    if ($file.sha256 -notmatch '^[a-f0-9]{64}$') { throw "配布物の照合情報が不正です。配布担当から一式を受け取り直してください。対象: $($file.path)" }
    if ($VerifyHashes -and $present -and (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $file.sha256) { throw "配布物のファイルが元の内容と一致しません。信頼できる配布元からZIPを受け取り直し、別のフォルダーへ展開してください。対象: $($file.path)" }
  }
  foreach ($required in @('app/bin/moyai-desktop.exe', 'app/bin/moyai-runner.exe', 'scripts/common.ps1', 'scripts/runtime-access.ps1', 'scripts/Start-moyAI.ps1', 'scripts/Setup-moyAI.ps1')) {
    if (-not $seen.ContainsKey((Resolve-MoyaiChild $Root $required))) { throw "配布物の一覧に必要なファイルがありません。配布担当へ確認してください。対象: $required" }
  }
  if ($manifest.hub) {
    $hubPath = Resolve-MoyaiChild $Root 'hub/bin/moyai-hub.exe'
    if (-not $seen.ContainsKey($hubPath)) { throw 'Hub同梱版の構成情報に対し、Hub本体が不足しています。配布担当からHub同梱版を一式受け取り直してください。' }
    $hubFile = @($manifest.files | Where-Object { $_.path -eq 'hub/bin/moyai-hub.exe' })[0]
    if ($hubFile.sha256 -ine $manifest.hub.sha256) { throw 'Hub本体の照合情報が配布物の一覧と一致しません。配布担当へ確認してください。対象: deployment.json' }
  }
  return $manifest
}

function Assert-MoyaiX64Pe([string]$Path) {
  $stream = [IO.File]::OpenRead($Path)
  $reader = [IO.BinaryReader]::new($stream)
  try {
    if ($reader.ReadUInt16() -ne 0x5a4d) { throw "Windows用の実行ファイルとして読み込めません。配布担当から一式を受け取り直してください。対象: $Path" }
    $stream.Position = 0x3c
    $peOffset = $reader.ReadInt32()
    if ($peOffset -lt 64 -or $peOffset -gt ($stream.Length - 6)) { throw "実行ファイルの形式が壊れています。配布担当から一式を受け取り直してください。対象: $Path" }
    $stream.Position = $peOffset
    if ($reader.ReadUInt32() -ne 0x4550 -or $reader.ReadUInt16() -ne 0x8664) { throw "Windows x64用の実行ファイルが必要です。配布担当へ確認してください。対象: $Path" }
  } finally { $reader.Dispose(); $stream.Dispose() }
}

function Get-MoyaiWebViewPath([string]$Root, $Manifest) {
  if ($Manifest.runtime.webview2 -eq 'bundled-fixed') {
    $path = Resolve-MoyaiChild $Root 'runtime/webview2'
    if (-not (Test-Path -LiteralPath (Join-Path $path 'msedgewebview2.exe') -PathType Leaf)) { throw '画面表示に必要な同梱WebView2のファイルが不足しています。配布担当から実行環境を含む一式を受け取り、展開し直してください。対象: runtime/webview2/msedgewebview2.exe' }
    return $path
  }
  $roots = @(
    (Join-Path ${env:ProgramFiles(x86)} 'Microsoft/EdgeWebView/Application'),
    (Join-Path $env:ProgramFiles 'Microsoft/EdgeWebView/Application'),
    (Join-Path $env:LOCALAPPDATA 'Microsoft/EdgeWebView/Application')
  )
  foreach ($candidateRoot in $roots) {
    if (-not (Test-Path -LiteralPath $candidateRoot -PathType Container)) { continue }
    $versions = @(Get-ChildItem -LiteralPath $candidateRoot -Directory | Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } | Sort-Object { [version]$_.Name } -Descending)
    foreach ($version in $versions) {
      $binary = Join-Path $version.FullName 'msedgewebview2.exe'
      if (Test-Path -LiteralPath $binary -PathType Leaf) { return $version.FullName }
    }
  }
  throw '画面表示に必要なMicrosoft Edge WebView2 Runtimeが見つかりません。社内の配布担当にオフライン導入を依頼するか、実行環境を同梱したmoyAIを受け取ってください。自動ダウンロードは行いません。'
}

function Test-MoyaiRuntime([string]$Root, $Manifest, [switch]$NoWebView, [switch]$PreparingInstall) {
  if (-not [Environment]::Is64BitOperatingSystem -or -not [Environment]::Is64BitProcess) { throw '64ビットのWindowsとWindows PowerShellが必要です。Windows x64のPCでSetup-moyAI.cmdを開いてください。' }
  if ([Environment]::OSVersion.Version.Major -lt 10) { throw 'Windows 10以降が必要です。対応するPCで導入してください。' }
  $webview = $null
  if (-not $NoWebView) {
    $webview = Get-MoyaiWebViewPath $Root $Manifest
    Assert-MoyaiX64Pe (Join-Path $webview 'msedgewebview2.exe')
    if ($Manifest.runtime.webview2 -eq 'bundled-fixed' -and -not $PreparingInstall) { Assert-MoyaiFixedRuntimeAccess $Root }
  }
  if (-not ('MoyaiDeployment.NativeLibrary' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace MoyaiDeployment {
  public static class NativeLibrary {
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern IntPtr LoadLibraryEx(string path, IntPtr reserved, uint flags);
    [DllImport("kernel32.dll")] public static extern bool FreeLibrary(IntPtr handle);
  }
}
'@
  }
  foreach ($name in @('vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll')) {
    $path = Join-Path $Root "app/bin/$name"
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { $path = Join-Path ([Environment]::SystemDirectory) $name }
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "起動に必要なMicrosoft Visual C++ x64の実行環境が見つかりません。社内の配布担当にオフライン導入を依頼するか、実行環境を同梱したmoyAIを受け取ってください。自動ダウンロードは行いません。対象: $name" }
    $handle = [MoyaiDeployment.NativeLibrary]::LoadLibraryEx($path, [IntPtr]::Zero, 0x1100)
    if ($handle -eq [IntPtr]::Zero) { throw "Microsoft Visual C++ x64の実行環境を読み込めません。社内の配布担当へ、この表示と対象ファイルを伝えて確認を依頼してください。対象: $name / Windowsエラー: $([Runtime.InteropServices.Marshal]::GetLastWin32Error())" }
    [void][MoyaiDeployment.NativeLibrary]::FreeLibrary($handle)
  }
  return $webview
}

function Write-MoyaiUtf8([string]$Path, [string]$Content) {
  [IO.File]::WriteAllText($Path, $Content, [Text.UTF8Encoding]::new($false))
}

# Report only processes in the installation being replaced; never stop them here.
function Get-MoyaiRunningAppNotice([string]$Root, [object[]]$Processes) {
  $boundary = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/') + '\'
  $running = @($Processes | Where-Object { $_.Path -and $_.Path.StartsWith($boundary, [StringComparison]::OrdinalIgnoreCase) })
  if ($running.Count -eq 0) { return $null }
  $lines = [Collections.Generic.List[string]]::new()
  $lines.Add('更新・アンインストールはまだ行っていません。この導入先のアプリが動いています。')
  foreach ($process in $running) {
    $label = switch ([IO.Path]::GetFileName($process.Path)) {
      'moyai-desktop.exe' { 'moyAIの画面'; break }
      'moyai-runner.exe' { '仕事の実行機能（Runner）'; break }
      'moyai-hub.exe' { 'チーム管理（Hub）'; break }
      default { '関連プログラム'; break }
    }
    $lines.Add("・$label / PID $($process.Id) / $($process.Path)")
  }
  $lines.Add('次の操作:')
  $lines.Add('1. 仕事を実行するPCでは新しい仕事の受付を一時停止し、実行中の仕事の完了と、停止を確認できない処理がないことを確認してください。')
  $lines.Add('2. moyAIはタスクトレイのアイコンから「終了」してください。画面の×だけでは終了しません。')
  $lines.Add('3. 上にRunnerがある場合は実行PCの担当者に終了を依頼してください。Hubがある場合は管理者がそのHubを --stop で終了します。ブラウザーを閉じるだけではHubは終了しません。')
  $lines.Add('4. 終了後に同じセットアップを開き直してください。停止手順: docs/user/windows-setup.md「更新・アンインストール」')
  $lines.Add('セットアップは稼働中のアプリや仕事を強制停止しません。')
  return $lines -join "`n"
}

function Get-MoyaiAutostartCandidates([string]$Root, $Receipt, $Manifest, [object[]]$Entries) {
  $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
  if ($Receipt.product -cne 'moyai-windows-user' -or $Receipt.install_root -ine $rootPath -or $Receipt.version -cne $Manifest.version) {
    throw '自動起動の登録と、この導入先の記録が一致しません。自動起動は変更していません。導入先を確認し、解消しない場合は表示内容を配布担当へ伝えてください。'
  }
  $owned = @{}
  foreach ($relative in @('app/bin/moyai-runner.exe', 'hub/bin/moyai-hub.exe')) {
    $files = @($Manifest.files | Where-Object { $_.path -ceq $relative })
    if ($files.Count -ne 1) { continue }
    $executable = Resolve-MoyaiChild $rootPath $relative
    Assert-MoyaiRegularPath $rootPath $executable
    # Match the payload deletion rule, including resumption after a previous partial uninstall.
    if ((Test-Path -LiteralPath $executable) -and ((Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash -ine $files[0].sha256)) { continue }
    $owned[$relative] = $executable
  }
  foreach ($entry in $Entries) {
    if ($entry.kind -cne 'String' -or $entry.command -isnot [string] -or $entry.command.Length -gt 260 -or $entry.command.Contains([char]0)) { continue }
    $relative = $null; $match = $null
    if ($entry.name -ceq 'moyAI Runner') {
      $relative = 'app/bin/moyai-runner.exe'
      $match = [regex]::Match($entry.command, '\A"(?<exe>[^"\r\n]+)" serve --background\z')
    } elseif ($entry.name -cmatch '\AmoyAI Hub [a-f0-9]{16}\z') {
      $relative = 'hub/bin/moyai-hub.exe'
      # Remove starts of this exact executable for every profile. Profile identity/data remains Hub-owned.
      $match = [regex]::Match($entry.command, '\A"(?<exe>[^"\r\n]+)" --launch --no-browser --data-dir "[^"\r\n]+"\z')
    }
    if (-not $relative -or -not $match.Success -or -not $owned.ContainsKey($relative)) { continue }
    $registeredExe = $match.Groups['exe'].Value
    if ($registeredExe.StartsWith('\\?\', [StringComparison]::Ordinal)) { $registeredExe = $registeredExe.Substring(4) }
    if (-not $registeredExe.Equals($owned[$relative], [StringComparison]::OrdinalIgnoreCase)) { continue }
    [pscustomobject]@{ name = $entry.name; command = $entry.command }
  }
}

function Open-MoyaiRunKey([bool]$Writable) {
  return [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Microsoft\Windows\CurrentVersion\Run', $Writable)
}

function Remove-MoyaiAutostart([string]$Root, $Receipt, $Manifest, [switch]$Preview, $RegistryKey = $null) {
  $ownsKey = $null -eq $RegistryKey
  $key = if ($ownsKey) { Open-MoyaiRunKey (-not $Preview) } else { $RegistryKey }
  if ($null -eq $key) { return }
  try {
    $entries = @($key.GetValueNames() | Where-Object { $_ -ceq 'moyAI Runner' -or $_ -cmatch '\AmoyAI Hub [a-f0-9]{16}\z' } | ForEach-Object {
      [pscustomobject]@{ name = $_; kind = $key.GetValueKind($_).ToString();
        command = $key.GetValue($_, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) }
    })
    $selected = @(Get-MoyaiAutostartCandidates $Root $Receipt $Manifest $entries)
    foreach ($entry in $selected) {
      if (-not $Preview) {
        # Re-read the complete command immediately before deleting its value, never the Run key.
        $current = $key.GetValue($entry.name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        if ($null -eq $current) { continue }
        if ($key.GetValueKind($entry.name).ToString() -cne 'String' -or $current -cne $entry.command) {
          throw 'アンインストール中に自動起動の登録が変更されました。変更された登録とアプリ本体は残しています。自動起動の設定を変更した担当者に確認してから再実行してください。'
        }
        $key.DeleteValue($entry.name, $false)
      }
      $entry
    }
  } finally { if ($ownsKey) { $key.Dispose() } }
}

# Only the dedicated public connection-file extension belongs to this installer.
# The launch command goes through the same offline-runtime owner as the Start menu.
function Get-MoyaiJoinFileRegistration([string]$Root) {
  $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
  $powershell = Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell/v1.0/powershell.exe'
  $command = '"' + $powershell + '" -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + (Resolve-MoyaiChild $rootPath 'scripts/Start-moyAI.ps1') + '" -JoinConfig "%1"'
  return [pscustomobject]@{schema=1; prog_id='moyAI.JoinConfig'; command=$command}
}

function Test-MoyaiJoinFileReceipt([string]$Root, $Receipt, $Manifest) {
  $rootPath = [IO.Path]::GetFullPath($Root).TrimEnd('\', '/')
  if ($Receipt.product -cne 'moyai-windows-user' -or $Receipt.install_root -ine $rootPath -or $Receipt.version -cne $Manifest.version) { throw '接続ファイルの関連付けと、この導入先の記録が一致しません。関連付けは変更していません。導入先を確認し、表示内容を配布担当へ伝えてください。' }
  $property = $Receipt.PSObject.Properties['join_file_association']
  if ($null -eq $property -or $null -eq $property.Value) { return $false }
  $expected = Get-MoyaiJoinFileRegistration $Root
  $actual = $property.Value
  return $actual.schema -eq 1 -and $actual.prog_id -ceq $expected.prog_id -and $actual.command -ceq $expected.command
}

function Open-MoyaiClassesKey([bool]$Writable) {
  if ($Writable) { return [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Software\Classes') }
  return [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Software\Classes', $false)
}

function Get-MoyaiJoinFileDefaultOverrides {
  $locations = @(
    @{root=[Microsoft.Win32.Registry]::CurrentUser; path='Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.moyai-join\UserChoice'; name='ProgId'},
    @{root=[Microsoft.Win32.Registry]::LocalMachine; path='Software\Classes\.moyai-join'; name=''}
  )
  foreach ($location in $locations) {
    $key = $location.root.OpenSubKey($location.path, $false)
    if ($null -eq $key) { continue }
    try {
      $value = $key.GetValue($location.name, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
      if ($null -ne $value -and $value -cne '') { $value }
    } finally { $key.Dispose() }
  }
}

function Get-MoyaiAssociationValue($ClassesKey, [string]$Path) {
  $key = $ClassesKey.OpenSubKey($Path, $false)
  if ($null -eq $key) { return $null }
  try {
    if (@($key.GetValueNames()) -cnotcontains '') { return $null }
    return [pscustomobject]@{kind=$key.GetValueKind('').ToString(); value=$key.GetValue('', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)}
  } finally { $key.Dispose() }
}

function Test-MoyaiEmptyAssociationKey($Key, [int]$Depth = 0) {
  if ($Depth -gt 3 -or @($Key.GetValueNames()).Count -ne 0) { return $false }
  foreach ($name in $Key.GetSubKeyNames()) {
    # Only our known empty hierarchy is reusable after uninstall; never adopt custom keys.
    if ($name -cnotin @('DefaultIcon', 'shell', 'open', 'command')) { return $false }
    $child = $Key.OpenSubKey($name, $false)
    if ($null -eq $child) { continue }
    try { if (-not (Test-MoyaiEmptyAssociationKey $child ($Depth + 1))) { return $false } } finally { $child.Dispose() }
  }
  return $true
}

function Get-MoyaiAssociationValues([string]$Root) {
  $registration = Get-MoyaiJoinFileRegistration $Root
  # Command is last so interrupted removal can safely resume using the receipt.
  @(
    [pscustomobject]@{path='.moyai-join'; value=$registration.prog_id},
    [pscustomobject]@{path=$registration.prog_id + '\DefaultIcon'; value='"' + (Resolve-MoyaiChild $Root 'app/bin/moyai-desktop.exe') + '",0'},
    [pscustomobject]@{path=$registration.prog_id; value='moyAI Hub connection'},
    [pscustomobject]@{path=$registration.prog_id + '\shell'; value='open'},
    [pscustomobject]@{path=$registration.prog_id + '\shell\open\command'; value=$registration.command}
  )
}

function Send-MoyaiAssociationChange {
  if (-not ('MoyaiDeployment.Associations' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace MoyaiDeployment {
  public static class Associations {
    [DllImport("shell32.dll")] public static extern void SHChangeNotify(uint id, uint flags, IntPtr item1, IntPtr item2);
  }
}
'@
  }
  [MoyaiDeployment.Associations]::SHChangeNotify(0x08000000, 0, [IntPtr]::Zero, [IntPtr]::Zero)
}

function Set-MoyaiJoinFileAssociation([string]$Root, $RegistryKey = $null, $DefaultOverrides = $null) {
  $receiptPath = Resolve-MoyaiChild $Root 'installation.json'
  $receipt = Get-Content -LiteralPath $receiptPath -Raw -Encoding UTF8 | ConvertFrom-Json
  $manifest = Read-MoyaiDeployment $Root -VerifyHashes
  $owned = Test-MoyaiJoinFileReceipt $Root $receipt $manifest
  $registration = Get-MoyaiJoinFileRegistration $Root
  $ownsKey = $null -eq $RegistryKey
  $key = if ($ownsKey) { Open-MoyaiClassesKey $true } else { $RegistryKey }
  if ($null -eq $key) { return '接続ファイルの関連付けは登録できませんでした。moyAIの画面から接続ファイルを読み込むか、Start-moyAI.cmd --join-config "接続ファイルの保存先" を使ってください。' }
  try {
    if ($null -eq $DefaultOverrides) { $DefaultOverrides = @(Get-MoyaiJoinFileDefaultOverrides) }
    $conflict = @($DefaultOverrides | Where-Object { $_ -cne $registration.prog_id }).Count -ne 0
    $existingProgId = $key.OpenSubKey($registration.prog_id, $false)
    if ($null -ne $existingProgId) {
      try { if (-not $owned -and -not (Test-MoyaiEmptyAssociationKey $existingProgId)) { $conflict = $true } } finally { $existingProgId.Dispose() }
    }
    foreach ($entry in @(Get-MoyaiAssociationValues $Root)) {
      $current = Get-MoyaiAssociationValue $key $entry.path
      if ($null -ne $current -and ($current.kind -cne 'String' -or $current.value -cne $entry.value)) { $conflict = $true }
      if ($entry.path -ceq '.moyai-join' -and $null -ne $current -and -not $owned) { $conflict = $true }
    }
    if ($conflict) { return '接続ファイルを開くアプリは既存の設定を保持しました。moyAIの画面から接続ファイルを読み込むか、Start-moyAI.cmd --join-config "接続ファイルの保存先" を使ってください。' }
    # Persist ownership intent first, so interrupted registration can be uninstalled.
    $receipt | Add-Member NoteProperty join_file_association $registration -Force
    Write-MoyaiUtf8 $receiptPath ($receipt | ConvertTo-Json -Depth 8)
    $values = @(Get-MoyaiAssociationValues $Root)
    [array]::Reverse($values) # Publish the extension only after the launch command is in place.
    foreach ($entry in $values) {
      $target = $key.CreateSubKey($entry.path)
      try {
        $current = Get-MoyaiAssociationValue $key $entry.path
        if ($null -ne $current -and ($current.kind -cne 'String' -or $current.value -cne $entry.value)) { throw 'セットアップ中に接続ファイルを開くアプリの設定が変わりました。その設定は保持しています。moyAIの画面から接続ファイルを読み込み、必要ならWindowsの「既定のアプリ」を確認してください。' }
        $target.SetValue('', $entry.value, [Microsoft.Win32.RegistryValueKind]::String)
      } finally { $target.Dispose() }
    }
    if ($ownsKey) { Send-MoyaiAssociationChange }
    return '接続ファイル（.moyai-join）をブラウザーのダウンロード欄やエクスプローラーから開けます。moyAIの画面で接続先を確認してから接続します。'
  } finally { if ($ownsKey) { $key.Dispose() } }
}

function Remove-MoyaiJoinFileAssociation([string]$Root, $Receipt, $Manifest, [switch]$Preview, $RegistryKey = $null) {
  if (-not (Test-MoyaiJoinFileReceipt $Root $Receipt $Manifest)) { return }
  $launcher = Resolve-MoyaiChild $Root 'scripts/Start-moyAI.ps1'
  $listed = @($Manifest.files | Where-Object { $_.path -ceq 'scripts/Start-moyAI.ps1' })
  if ($listed.Count -ne 1 -or ((Test-Path -LiteralPath $launcher) -and (Get-FileHash -LiteralPath $launcher -Algorithm SHA256).Hash -ine $listed[0].sha256)) { return }
  $ownsKey = $null -eq $RegistryKey
  $key = if ($ownsKey) { Open-MoyaiClassesKey (-not $Preview) } else { $RegistryKey }
  if ($null -eq $key) { return }
  try {
    $registration = Get-MoyaiJoinFileRegistration $Root
    $command = Get-MoyaiAssociationValue $key ($registration.prog_id + '\shell\open\command')
    if ($null -ne $command -and ($command.kind -cne 'String' -or $command.value -cne $registration.command)) { return }
    foreach ($entry in @(Get-MoyaiAssociationValues $Root)) {
      $current = Get-MoyaiAssociationValue $key $entry.path
      if ($null -eq $current -or $current.kind -cne 'String' -or $current.value -cne $entry.value) { continue }
      if (-not $Preview) {
        $target = $key.OpenSubKey($entry.path, $true)
        if ($null -eq $target) { continue }
        try {
          $reread = $target.GetValue('', $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
          if ($null -eq $reread) { continue }
          if ($target.GetValueKind('').ToString() -cne 'String' -or $reread -cne $entry.value) { throw 'アンインストール中に接続ファイルを開くアプリの設定が変わりました。変更された設定とアプリ本体は残しています。Windowsの「既定のアプリ」を確認してから再実行してください。' }
          $target.DeleteValue('', $false)
        } finally { $target.Dispose() }
      }
      $entry.path
    }
    # Leave registry keys themselves intact: foreign named values/subkeys can coexist.
    if (-not $Preview -and $ownsKey) { Send-MoyaiAssociationChange }
  } finally { if ($ownsKey) { $key.Dispose() } }
}
