param([string]$EvidenceRoot = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'deployment/common.ps1')
if (-not $EvidenceRoot) { $EvidenceRoot = Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) ('project_sandbox/onboarding-deployment-' + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmss')) }
$EvidenceRoot = [IO.Path]::GetFullPath($EvidenceRoot)
New-Item -ItemType Directory -Path $EvidenceRoot -Force | Out-Null
$package = Join-Path $EvidenceRoot 'package'
$destination = Join-Path $EvidenceRoot 'installed'
New-Item -ItemType Directory -Path (Join-Path $package 'scripts'), (Join-Path $package 'app/bin'), (Join-Path $package 'hub/bin') -Force | Out-Null
foreach ($name in @('common.ps1', 'runtime-access.ps1', 'Setup-moyAI.ps1', 'Start-moyAI.ps1')) { Copy-Item -LiteralPath (Join-Path $PSScriptRoot "deployment/$name") -Destination (Join-Path $package "scripts/$name") }
# Subprocess fixtures never open actual logon/association registry keys, even read-only.
$fixtureCommon = Join-Path $package 'scripts/common.ps1'
[IO.File]::WriteAllText($fixtureCommon, ((Get-Content -LiteralPath $fixtureCommon -Raw -Encoding UTF8) + "`nfunction Open-MoyaiRunKey([bool]`$Writable) { return `$null }`nfunction Open-MoyaiClassesKey([bool]`$Writable) { return `$null }`nfunction Get-MoyaiJoinFileDefaultOverrides { throw 'Fixture attempted to read actual registry defaults.' }`n"), [Text.UTF8Encoding]::new($true))
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'deployment/Start-moyAI.cmd') -Destination (Join-Path $package 'Start-moyAI.cmd')
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'deployment/Setup-moyAI.cmd') -Destination (Join-Path $package 'Setup-moyAI.cmd')
Write-MoyaiUtf8 (Join-Path $package 'app/bin/moyai-runner.exe') 'inert deployment fixture; never execute'
Write-MoyaiUtf8 (Join-Path $package 'hub/bin/moyai-hub.exe') 'inert Hub deployment fixture; never execute'
$compileFixture = Join-Path $EvidenceRoot 'compile-fixture.ps1'
Write-MoyaiUtf8 $compileFixture @'
param([string]$Output)
$ErrorActionPreference = 'Stop'
Add-Type -OutputAssembly $Output -OutputType WindowsApplication -TypeDefinition @"
using System;
using System.IO;
using System.Diagnostics;
using System.Threading;
public static class ActivationFixture {
  public static void Main(string[] arguments) {
    if (arguments.Length == 1 && arguments[0] == "--worker") {
      File.WriteAllText(Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "worker-started.txt"), "started");
      Thread.Sleep(10000);
      File.WriteAllText(Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "worker-finished.txt"), "finished");
      return;
    }
    if (arguments.Length == 1 && arguments[0] == "--launch") {
      Process.Start(new ProcessStartInfo(Process.GetCurrentProcess().MainModule.FileName, "--worker") { UseShellExecute = false, CreateNoWindow = true });
      for (int i = 0; i < 100 && !File.Exists(Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "worker-started.txt")); i++) Thread.Sleep(50);
      return;
    }
    File.WriteAllLines(Path.Combine(AppDomain.CurrentDomain.BaseDirectory, "activation.txt"), arguments);
  }
}
"@
'@
& (Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe') -NoProfile -ExecutionPolicy Bypass -File $compileFixture -Output (Join-Path $package 'app/bin/moyai-desktop.exe')
if ($LASTEXITCODE -ne 0) { throw 'Failed to build the argument-recording fixture.' }
Copy-Item -LiteralPath (Join-Path $package 'app/bin/moyai-desktop.exe') -Destination (Join-Path $package 'hub/bin/moyai-hub.exe') -Force
Write-MoyaiUtf8 (Join-Path $package 'release.txt') 'first version'
$profile = Join-Path $EvidenceRoot 'saved-profile.txt'
Write-MoyaiUtf8 $profile 'existing credential and history fixture'
$checks = [Collections.Generic.List[string]]::new()
function Assert-That([bool]$Condition, [string]$Message) {
  if (-not $Condition) { throw "FAIL: $Message" }
  $checks.Add("PASS: $Message")
}
function Assert-Fails([scriptblock]$Action, [string]$Message) {
  $failed = $false
  try { & $Action | Out-Null } catch { $failed = $true }
  Assert-That $failed $Message
}
function Write-FixtureManifest([string]$Version) {
  $files = @(Get-ChildItem -LiteralPath $package -Recurse -File | Where-Object { $_.Name -ne 'deployment.json' } | ForEach-Object {
    [ordered]@{path=$_.FullName.Substring($package.Length + 1).Replace('\', '/'); sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()}
  })
  $manifest = [ordered]@{schema=1; product='moyai-windows-user'; target='windows-x86_64'; version=$Version;
    hub=@{sha256=(Get-FileHash -LiteralPath (Join-Path $package 'hub/bin/moyai-hub.exe') -Algorithm SHA256).Hash.ToLowerInvariant()};
    runtime=@{webview2='system-installed'; visual_cpp='system-installed'}; files=$files}
  Write-MoyaiUtf8 (Join-Path $package 'deployment.json') ($manifest | ConvertTo-Json -Depth 8)
}
function Invoke-FixtureSetup([string[]]$Extra = @()) {
  $output = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $package 'scripts/Setup-moyAI.ps1') -InstallRoot $destination -NoShortcuts @Extra 2>&1 | Out-String
  if ($LASTEXITCODE -ne 0) { throw "Setup failed: $output" }
  $script:lastSetupOutput = $output
}
Write-FixtureManifest '1.0-fixture'
[void](Read-MoyaiDeployment $package -VerifyHashes)
Assert-That $true 'valid inventory and hashes accepted'
Assert-Fails { Resolve-MoyaiChild $package '../saved-profile.txt' } 'traversal outside the package rejected'
Assert-Fails { Resolve-MoyaiChild $package 'C:\other\file' } 'absolute inventory paths rejected'
Assert-Fails { Resolve-MoyaiChild $package 'file:stream' } 'alternate data streams rejected'
Assert-Fails { Resolve-MoyaiChild $package './installation.json' } 'aliased reserved paths cannot bypass package validation'
Write-MoyaiUtf8 (Join-Path $package 'release.txt') 'tampered'
Assert-Fails { Read-MoyaiDeployment $package -VerifyHashes } 'tampered file rejected before installation'
Write-MoyaiUtf8 (Join-Path $package 'release.txt') 'first version'
$originalManifest = Get-Content -LiteralPath (Join-Path $package 'deployment.json') -Raw -Encoding UTF8
$duplicate = $originalManifest | ConvertFrom-Json
$duplicate.files += $duplicate.files[0]
Write-MoyaiUtf8 (Join-Path $package 'deployment.json') ($duplicate | ConvertTo-Json -Depth 8)
Assert-Fails { Read-MoyaiDeployment $package -VerifyHashes } 'duplicate normalized paths rejected'
Write-MoyaiUtf8 (Join-Path $package 'deployment.json') $originalManifest
$badRuntime = [pscustomobject]@{runtime=[pscustomobject]@{webview2='bundled-fixed'}}
try { Get-MoyaiWebViewPath $package $badRuntime; throw 'Expected missing runtime failure.' }
catch { Assert-That ($_.Exception.Message.Contains('WebView2') -and $_.Exception.Message.Contains('配布担当') -and $_.Exception.Message.Contains('runtime/webview2/msedgewebview2.exe')) 'missing runtime reports its cause, Japanese next action and exact file in Windows PowerShell 5.1' }
$missingPackage = Join-Path $EvidenceRoot 'missing-package'
New-Item -ItemType Directory -Path $missingPackage | Out-Null
try { Read-MoyaiDeployment $missingPackage; throw 'Expected missing manifest failure.' }
catch { Assert-That ($_.Exception.Message.Contains('展開し直してください') -and $_.Exception.Message.Contains((Join-Path $missingPackage 'deployment.json'))) 'missing package inventory gives Japanese recovery advice and exact location' }
New-Item -ItemType Directory -Path (Join-Path $missingPackage 'scripts') | Out-Null
foreach ($name in @('common.ps1', 'runtime-access.ps1', 'Start-moyAI.ps1')) { Copy-Item -LiteralPath (Join-Path $package "scripts/$name") -Destination (Join-Path $missingPackage "scripts/$name") }
# CheckOnly suppresses the native error dialog, but exercises the real error presentation.
$previousErrorPreference = $ErrorActionPreference
try {
  $ErrorActionPreference = 'Continue'
  $startFailure = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $missingPackage 'scripts/Start-moyAI.ps1') -CheckOnly 2>&1 | ForEach-Object { $_.ToString() } | Out-String
  $startFailureCode = $LASTEXITCODE
  $setupFailure = & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $package 'scripts/Setup-moyAI.ps1') -InstallRoot $missingPackage -NoShortcuts -CheckOnly 2>&1 | ForEach-Object { $_.ToString() } | Out-String
  $setupFailureCode = $LASTEXITCODE
} finally { $ErrorActionPreference = $previousErrorPreference }
Assert-That ($startFailureCode -eq 1 -and $startFailure.Contains('moyAIを起動できませんでした') -and $startFailure.Contains('展開し直してください') -and $startFailure.Contains($missingPackage)) 'standard Windows PowerShell startup failure retains Japanese cause, next action, package path and failure status'
Assert-That ($setupFailureCode -eq 1 -and $setupFailure.Contains('導入した記録がありません') -and $setupFailure.Contains('配布担当へ確認') -and $setupFailure.Contains($missingPackage)) 'standard Windows PowerShell setup failure identifies an unowned destination and next action in Japanese'
Write-MoyaiUtf8 (Join-Path $EvidenceRoot 'startup-failure.txt') $startFailure
Write-MoyaiUtf8 (Join-Path $EvidenceRoot 'setup-failure.txt') $setupFailure
$processFixtures = @(
  [pscustomobject]@{Id=101; Path=(Join-Path $destination 'app/bin/moyai-desktop.exe')},
  [pscustomobject]@{Id=102; Path=(Join-Path $destination 'app/bin/moyai-runner.exe')},
  [pscustomobject]@{Id=103; Path=(Join-Path $destination 'hub/bin/moyai-hub.exe')},
  [pscustomobject]@{Id=999; Path=($destination + '-other\moyai-desktop.exe')},
  [pscustomobject]@{Id=998; Path=$null}
)
$notice = Get-MoyaiRunningAppNotice $destination $processFixtures
Assert-That ($notice.Contains('moyAIの画面 / PID 101') -and $notice.Contains('仕事の実行機能（Runner） / PID 102') -and $notice.Contains('チーム管理（Hub） / PID 103') -and $notice.Contains($processFixtures[1].Path) -and -not $notice.Contains('999')) 'update notice identifies only this installation, including each role, PID and executable'
Assert-That ($notice.Contains('受付を一時停止') -and $notice.Contains('タスクトレイ') -and $notice.Contains('--stop') -and $notice.Contains('強制停止しません')) 'update notice explains waiting for work and distinct Desktop, Runner and Hub exit steps without stopping processes'
Assert-That ($null -eq (Get-MoyaiRunningAppNotice $destination @($processFixtures[3], $processFixtures[4]))) 'another installation and inaccessible process paths never block this installation'
# Uses this test PC's installed prerequisites; it is not fresh-PC runtime evidence.
Invoke-FixtureSetup @('-CheckOnly')
Assert-That (-not (Test-Path -LiteralPath $destination)) 'check-only does not install or create a profile'
Assert-That ($lastSetupOutput.Contains('確認のみ完了') -and $lastSetupOutput.Contains('まだ導入・起動していません')) 'noninteractive validation reports its limited result in Japanese'
$unicodeName = -join ([char[]]@(0x65e5, 0x672c, 0x8a9e))
$joinFile = Join-Path $EvidenceRoot ('team & ' + $unicodeName + ' $value.moyai-join')
Write-MoyaiUtf8 $joinFile 'public configuration fixture; only its path is forwarded'
$launcher = Join-Path $package 'Start-moyAI.cmd'
& $launcher --join-config $joinFile
Assert-That ($LASTEXITCODE -eq 0) 'portable launcher accepts explicit connection-file activation'
$activation = Join-Path $package 'app/bin/activation.txt'
$deadline = [DateTime]::UtcNow.AddSeconds(5)
while (-not (Test-Path -LiteralPath $activation) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
$received = @(Get-Content -LiteralPath $activation -Encoding UTF8)
Assert-That ($received.Count -eq 2 -and $received[0] -eq '--join-config' -and $received[1] -eq $joinFile) 'connection-file path with spaces and ampersand arrives as one argument'
Invoke-FixtureSetup
Assert-That (Test-Path -LiteralPath (Join-Path $destination 'installation.json')) 'first installation writes an installation receipt'
Assert-That ($lastSetupOutput.Contains('導入が完了しました') -and $lastSetupOutput.Contains((Join-Path $destination 'Start-moyAI.cmd')) -and $lastSetupOutput.Contains('チームの仕事をこのPCで実行する') -and $lastSetupOutput.Contains('参加承認や最初の仕事の準備完了とは別')) 'successful setup shows the next launcher, role-based actions and remaining team preparation in Japanese'
Write-MoyaiUtf8 (Join-Path $EvidenceRoot 'setup-success.txt') $lastSetupOutput
Assert-That (-not (Test-Path -LiteralPath (Join-Path $destination 'app/bin/activation.txt'))) 'setup does not automatically launch the application'
function New-FakeClassesKey($Store = $null, [string]$Path = '') {
  if ($null -eq $Store) { $Store = @{Nodes=@{''=@{Values=@{}; Kinds=@{}}}; Writes=[Collections.Generic.List[string]]::new(); Deleted=[Collections.Generic.List[string]]::new(); Reads=@{}; RacePath=$null} }
  $key = [pscustomobject]@{Store=$Store; Path=$Path}
  $key | Add-Member ScriptMethod OpenSubKey {
    param($child, $writable)
    $path = if ($this.Path) { $this.Path + '\' + $child } else { $child }
    if (-not $this.Store.Nodes.ContainsKey($path)) { return $null }
    return New-FakeClassesKey $this.Store $path
  }
  $key | Add-Member ScriptMethod CreateSubKey {
    param($child)
    $path = if ($this.Path) { $this.Path + '\' + $child } else { $child }
    $prefix = ''
    foreach ($part in ($path -split '\\')) {
      $prefix = if ($prefix) { $prefix + '\' + $part } else { $part }
      if (-not $this.Store.Nodes.ContainsKey($prefix)) { $this.Store.Nodes[$prefix]=@{Values=@{}; Kinds=@{}} }
    }
    return New-FakeClassesKey $this.Store $path
  }
  $key | Add-Member ScriptMethod GetValueNames { return @($this.Store.Nodes[$this.Path].Values.Keys) }
  $key | Add-Member ScriptMethod GetSubKeyNames {
    $prefix = if ($this.Path) { $this.Path + '\' } else { '' }
    return @($this.Store.Nodes.Keys | Where-Object { $_.StartsWith($prefix, [StringComparison]::Ordinal) -and $_.Length -gt $prefix.Length } | ForEach-Object { $_.Substring($prefix.Length).Split('\')[0] } | Sort-Object -Unique)
  }
  $key | Add-Member ScriptMethod GetValueKind { param($name) return $this.Store.Nodes[$this.Path].Kinds[$name] }
  $key | Add-Member ScriptMethod GetValue {
    param($name, $fallback, $options)
    if (-not $this.Store.Reads.ContainsKey($this.Path)) { $this.Store.Reads[$this.Path]=0 }; $this.Store.Reads[$this.Path]++
    if ($this.Store.RacePath -ceq $this.Path -and $this.Store.Reads[$this.Path] -gt 1) { $this.Store.Nodes[$this.Path].Values[$name]='foreign edit during uninstall' }
    if (-not $this.Store.Nodes[$this.Path].Values.ContainsKey($name)) { return $fallback }
    return $this.Store.Nodes[$this.Path].Values[$name]
  }
  $key | Add-Member ScriptMethod SetValue {
    param($name, $value, $kind)
    $this.Store.Nodes[$this.Path].Values[$name]=$value; $this.Store.Nodes[$this.Path].Kinds[$name]=$kind; $this.Store.Writes.Add($this.Path)
  }
  $key | Add-Member ScriptMethod DeleteValue {
    param($name, $throw)
    $this.Store.Nodes[$this.Path].Values.Remove($name); $this.Store.Nodes[$this.Path].Kinds.Remove($name); $this.Store.Deleted.Add($this.Path)
  }
  $key | Add-Member ScriptMethod Dispose { }
  return $key
}
$classes = New-FakeClassesKey
$receiptPath = Join-Path $destination 'installation.json'
$legacyReceipt = Get-Content -LiteralPath $receiptPath -Raw -Encoding UTF8
[void](Set-MoyaiJoinFileAssociation $destination -RegistryKey $classes -DefaultOverrides @())
$associationReceipt = Get-Content -LiteralPath $receiptPath -Raw -Encoding UTF8 | ConvertFrom-Json
$registration = Get-MoyaiJoinFileRegistration $destination
Assert-That ($classes.Store.Writes.Count -eq 5 -and $associationReceipt.join_file_association.command -ceq $registration.command) 'fresh registration writes the dedicated extension and exact launch ownership into its receipt'
Assert-That (-not $classes.Store.Nodes.ContainsKey('.toml') -and -not $classes.Store.Nodes.ContainsKey('UserChoice')) 'association never creates .toml or protected UserChoice entries'
$commandPath = 'moyAI.JoinConfig\shell\open\command'
$openCommand = (Get-MoyaiAssociationValue $classes $commandPath).value
$fixedPowerShell = Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell/v1.0/powershell.exe'
Assert-That ($openCommand.StartsWith('"' + $fixedPowerShell + '" -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "', [StringComparison]::Ordinal) -and $openCommand.EndsWith('" -JoinConfig "%1"', [StringComparison]::Ordinal)) 'shell open uses fixed Windows PowerShell, the runtime launcher, and a quoted single file argument'
# Exercise the registered command structure with an inert executable, without ShellExecute/registry mutation.
$launchScript = Join-Path $destination 'scripts/Start-moyAI.ps1'
$joinedArguments = '-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + $launchScript + '" -JoinConfig "' + $joinFile + '"'
$record = Join-Path $destination 'app/bin/activation.txt'
if (Test-Path -LiteralPath $record) { Remove-Item -LiteralPath $record }
$launched = Start-Process -FilePath $fixedPowerShell -ArgumentList $joinedArguments -WindowStyle Hidden -Wait -PassThru
$deadline = [DateTime]::UtcNow.AddSeconds(5)
while (-not (Test-Path -LiteralPath $record) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
$received = @(Get-Content -LiteralPath $record -Encoding UTF8)
Assert-That ($launched.ExitCode -eq 0 -and $received.Count -eq 2 -and $received[0] -ceq '--join-config' -and $received[1] -ceq $joinFile) 'association launch forwarding preserves spaces, Unicode, ampersand and literal dollar characters as one file argument'
Remove-Item -LiteralPath $record
# The launcher must wait only for --launch, not the long-running Hub child.
$hubBin = Join-Path $destination 'hub/bin'
$hubStart = Start-Process -FilePath $fixedPowerShell -ArgumentList ('-NoProfile -ExecutionPolicy Bypass -File "' + $launchScript + '" -TeamManagement') -WindowStyle Hidden -PassThru
$hubStart.WaitForExit()
Assert-That ($hubStart.ExitCode -eq 0 -and (Test-Path -LiteralPath (Join-Path $hubBin 'worker-started.txt')) -and -not (Test-Path -LiteralPath (Join-Path $hubBin 'worker-finished.txt'))) 'Hub launcher returns after management opens while its server child continues running'
$deadline = [DateTime]::UtcNow.AddSeconds(15)
while (-not (Test-Path -LiteralPath (Join-Path $hubBin 'worker-finished.txt')) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 100 }
Assert-That (Test-Path -LiteralPath (Join-Path $hubBin 'worker-finished.txt')) 'finite Hub child fixture exits without stopping unrelated processes'
Remove-Item -LiteralPath (Join-Path $hubBin 'worker-started.txt'),(Join-Path $hubBin 'worker-finished.txt')
Write-MoyaiUtf8 (Join-Path $destination 'user-added.txt') 'preserve this extra file'
Write-MoyaiUtf8 (Join-Path $package 'release.txt') 'second version'
Write-FixtureManifest '2.0-fixture'
Invoke-FixtureSetup
Assert-That ((Get-Content -LiteralPath (Join-Path $destination 'release.txt') -Raw) -eq 'second version') 'update replaces the application at the same path'
$previous = @(Get-ChildItem -LiteralPath $EvidenceRoot -Directory -Filter 'moyAI.previous-*')
Assert-That ($previous.Count -eq 1 -and (Get-Content -LiteralPath (Join-Path $previous[0].FullName 'user-added.txt') -Raw) -eq 'preserve this extra file') 'update retains previous and unlisted files'
$installedManifest = Read-MoyaiDeployment $destination -VerifyHashes
$receipt = Get-Content -LiteralPath (Join-Path $destination 'installation.json') -Raw -Encoding UTF8 | ConvertFrom-Json
Assert-That ($receipt.join_file_association.command -ceq $registration.command) 'update retains association ownership while replacing the receipt version'
$writesBefore = $classes.Store.Writes.Count
[void](Set-MoyaiJoinFileAssociation $destination -RegistryKey $classes -DefaultOverrides @())
Assert-That ($classes.Store.Writes.Count -eq $writesBefore + 5) 'an owned unchanged association can be maintained during update'
$savedReceipt = Get-Content -LiteralPath $receiptPath -Raw -Encoding UTF8
foreach ($conflict in @('extension', 'command', 'non-string', 'user-choice', 'machine-default', 'unowned-progid')) {
  $foreign = New-FakeClassesKey
  $defaults = @()
  Write-MoyaiUtf8 $receiptPath $savedReceipt
  switch ($conflict) {
    'extension' { $target=$foreign.CreateSubKey('.moyai-join'); $target.SetValue('', 'Other.Application', [Microsoft.Win32.RegistryValueKind]::String) }
    'command' { $target=$foreign.CreateSubKey($commandPath); $target.SetValue('', '"C:\Other\start.exe" "%1"', [Microsoft.Win32.RegistryValueKind]::String) }
    'non-string' { $target=$foreign.CreateSubKey('.moyai-join'); $target.SetValue('', $registration.prog_id, [Microsoft.Win32.RegistryValueKind]::ExpandString) }
    'user-choice' { $defaults=@('Other.ChosenApplication') }
    'machine-default' { $defaults=@('Other.MachineApplication') }
    'unowned-progid' { $target=$foreign.CreateSubKey('moyAI.JoinConfig'); $target.SetValue('foreign-metadata', 'retained', [Microsoft.Win32.RegistryValueKind]::String); $old=$savedReceipt | ConvertFrom-Json; $old.PSObject.Properties.Remove('join_file_association'); Write-MoyaiUtf8 $receiptPath ($old | ConvertTo-Json -Depth 8) }
  }
  $before = $foreign.Store.Writes.Count
  $status = Set-MoyaiJoinFileAssociation $destination -RegistryKey $foreign -DefaultOverrides $defaults
  Assert-That ($status.StartsWith('接続ファイルを開くアプリは既存の設定を保持しました。') -and $foreign.Store.Writes.Count -eq $before) "association preserves $conflict without writing any registry value"
}
Write-MoyaiUtf8 $receiptPath $savedReceipt
$preview = @(Remove-MoyaiJoinFileAssociation $destination $receipt $installedManifest -Preview -RegistryKey $classes)
Assert-That ($preview.Count -eq 5 -and $classes.Store.Deleted.Count -eq 0) 'association uninstall preview selects exact owned values without removing them'
$otherReceipt = $savedReceipt | ConvertFrom-Json; $otherReceipt.install_root = Join-Path $EvidenceRoot 'another-installation'
Assert-Fails { Remove-MoyaiJoinFileAssociation $destination $otherReceipt $installedManifest -RegistryKey $classes } 'association uninstall rejects another installation receipt'
$target = $classes.OpenSubKey($commandPath, $true)
$target.SetValue('', $registration.command + ' -CustomArgument', [Microsoft.Win32.RegistryValueKind]::String)
Assert-That (@(Remove-MoyaiJoinFileAssociation $destination $receipt $installedManifest -RegistryKey $classes).Count -eq 0 -and $classes.Store.Deleted.Count -eq 0) 'a changed open command preserves the whole association on uninstall'
$target.SetValue('', $registration.command, [Microsoft.Win32.RegistryValueKind]::String)
$target = $classes.OpenSubKey('moyAI.JoinConfig\DefaultIcon', $true); $target.SetValue('', 'foreign icon', [Microsoft.Win32.RegistryValueKind]::String)
$target = $classes.OpenSubKey('moyAI.JoinConfig', $true); $target.SetValue('foreign-note', 'keep this', [Microsoft.Win32.RegistryValueKind]::String)
[void](Remove-MoyaiJoinFileAssociation $destination $receipt $installedManifest -RegistryKey $classes)
Assert-That ($classes.Store.Deleted.Count -eq 4 -and (Get-MoyaiAssociationValue $classes 'moyAI.JoinConfig\DefaultIcon').value -ceq 'foreign icon' -and $classes.Store.Nodes['moyAI.JoinConfig'].Values['foreign-note'] -ceq 'keep this') 'uninstall preserves changed values and foreign named values while removing only exact owned defaults'
$race = New-FakeClassesKey
[void](Set-MoyaiJoinFileAssociation $destination -RegistryKey $race -DefaultOverrides @())
$race.Store.Reads.Clear(); $race.Store.RacePath='.moyai-join'
Assert-Fails { Remove-MoyaiJoinFileAssociation $destination $receipt $installedManifest -RegistryKey $race } 'association value is re-read immediately before deletion'
Assert-That ($race.Store.Deleted.Count -eq 0) 'a concurrent association change is never deleted'
$legacy = $savedReceipt | ConvertFrom-Json; $legacy.PSObject.Properties.Remove('join_file_association')
Assert-That (@(Remove-MoyaiJoinFileAssociation $destination $legacy $installedManifest -RegistryKey $race).Count -eq 0) 'legacy receipts do not claim any existing file association'
$clean = New-FakeClassesKey
[void](Set-MoyaiJoinFileAssociation $destination -RegistryKey $clean -DefaultOverrides @())
[void](Remove-MoyaiJoinFileAssociation $destination $receipt $installedManifest -RegistryKey $clean)
Write-MoyaiUtf8 $receiptPath ($legacy | ConvertTo-Json -Depth 8)
$status = Set-MoyaiJoinFileAssociation $destination -RegistryKey $clean -DefaultOverrides @()
Assert-That ($status.StartsWith('接続ファイル（.moyai-join）') -and (Get-MoyaiAssociationValue $clean $commandPath).value -ceq $registration.command) 'reinstallation reuses only the empty known hierarchy left by a complete uninstall'
$launcherBytes = [IO.File]::ReadAllBytes($launchScript)
$launcherContent = Get-Content -LiteralPath $launchScript -Raw -Encoding UTF8
Write-MoyaiUtf8 $launchScript ($launcherContent + "`n# user-modified launcher`n")
Assert-That (@(Remove-MoyaiJoinFileAssociation $destination $receipt $installedManifest -RegistryKey $clean).Count -eq 0) 'association to a user-modified launcher is retained with that launcher'
[IO.File]::WriteAllBytes($launchScript, $launcherBytes)
$runnerCommand = '"' + (Resolve-MoyaiChild $destination 'app/bin/moyai-runner.exe') + '" serve --background'
$hubPrefix = '"' + (Resolve-MoyaiChild $destination 'hub/bin/moyai-hub.exe') + '" --launch --no-browser --data-dir '
$registrations = @(
  [pscustomobject]@{name='moyAI Runner'; kind='String'; command=$runnerCommand},
  [pscustomobject]@{name='moyAI Hub d080a372cf777bc3'; kind='String'; command=$hubPrefix + '"\\?\C:\Hub Data"'},
  [pscustomobject]@{name='moyAI Hub 0cbb629de32bb86b'; kind='String'; command=$hubPrefix + '"C:\Other Profile"'},
  [pscustomobject]@{name='moyAI Hub 1234567890abcdef'; kind='String'; command=$hubPrefix + '"C:\日本語 İ Σ profile"'}
)
Assert-That (@(Get-MoyaiAutostartCandidates $destination $receipt $installedManifest $registrations).Count -eq 4) 'exact Runner and all Hub profiles, including Unicode, on the installed exe are selected without owning profile identity'
foreach ($invalid in @(
  [pscustomobject]@{name='moyAI Runner'; kind='String'; command='"C:\Other\moyai-runner.exe" serve --background'},
  [pscustomobject]@{name='moyAI Runner'; kind='ExpandString'; command=$runnerCommand},
  [pscustomobject]@{name='moyAI Runner'; kind='String'; command=$runnerCommand + ' --custom'},
  [pscustomobject]@{name='moyAI Hub d080a372cf777bc3'; kind='String'; command='"C:\Other\moyai-hub.exe" --launch --no-browser --data-dir "C:\Hub Data"'},
  [pscustomobject]@{name='Other Hub 0000000000000000'; kind='String'; command=$hubPrefix + '"C:\Hub Data"'},
  [pscustomobject]@{name='moyAI Hub d080a372cf777bc3'; kind='String'; command=$hubPrefix + '"C:\Hub Data" --extra'}
)) { Assert-That (@(Get-MoyaiAutostartCandidates $destination $receipt $installedManifest @($invalid)).Count -eq 0) 'foreign, custom, unknown-namespace or non-string startup registration retained' }
$wrongReceipt = [pscustomobject]@{product=$receipt.product; version=$receipt.version; install_root=(Join-Path $EvidenceRoot 'other')}
Assert-Fails { Get-MoyaiAutostartCandidates $destination $wrongReceipt $installedManifest $registrations } 'autostart cleanup rejects another installation receipt'
function New-FakeRunKey([switch]$ChangeOnReread) {
  $values = @{ unrelated='unrelated command' }; foreach ($row in $registrations) { $values[$row.name]=$row.command }
  $key = [pscustomobject]@{Values=$values; Reads=@{}; Deleted=[Collections.Generic.List[string]]::new(); Change=[bool]$ChangeOnReread}
  $key | Add-Member ScriptMethod GetValueNames { return @($this.Values.Keys) }
  $key | Add-Member ScriptMethod GetValueKind { param($name) return [Microsoft.Win32.RegistryValueKind]::String }
  $key | Add-Member ScriptMethod GetValue {
    param($name, $fallback, $options)
    if (-not $this.Reads.ContainsKey($name)) { $this.Reads[$name]=0 }; $this.Reads[$name]++
    if ($this.Change -and $this.Reads[$name] -gt 1) { return 'changed after preview' }
    return $this.Values[$name]
  }
  $key | Add-Member ScriptMethod DeleteValue { param($name, $throw) $this.Deleted.Add($name); $this.Values.Remove($name) }
  return $key
}
$fake = New-FakeRunKey
Assert-That (@(Remove-MoyaiAutostart $destination $receipt $installedManifest -Preview -RegistryKey $fake).Count -eq 4 -and $fake.Deleted.Count -eq 0) 'uninstall preview leaves fake startup registrations untouched'
$fake = New-FakeRunKey
[void](Remove-MoyaiAutostart $destination $receipt $installedManifest -RegistryKey $fake)
Assert-That ($fake.Deleted.Count -eq 4 -and $fake.Values.Count -eq 1 -and $fake.Values.ContainsKey('unrelated')) 'uninstall removes only exact owned values from an isolated fake Run key'
$fake = New-FakeRunKey -ChangeOnReread
Assert-Fails { Remove-MoyaiAutostart $destination $receipt $installedManifest -RegistryKey $fake } 'changed startup command is rejected immediately before deletion'
Assert-That ($fake.Deleted.Count -eq 0) 'changed registration is never deleted'
$hubExe = Resolve-MoyaiChild $destination 'hub/bin/moyai-hub.exe'
Write-MoyaiUtf8 $hubExe 'user replacement Hub'
Assert-That (@(Get-MoyaiAutostartCandidates $destination $receipt $installedManifest $registrations).Count -eq 1) 'startup for a user-modified Hub binary is retained with that binary'
Write-MoyaiUtf8 $hubExe 'inert Hub deployment fixture; never execute'
Write-MoyaiUtf8 (Join-Path $destination 'release.txt') 'user modified'
Remove-Item -LiteralPath (Join-Path $destination 'app/bin/moyai-runner.exe')
Invoke-FixtureSetup @('-Uninstall')
Assert-That ((Get-Content -LiteralPath (Join-Path $destination 'release.txt') -Raw) -eq 'user modified') 'uninstall retains files modified after installation'
Assert-That (-not (Test-Path -LiteralPath (Join-Path $destination 'app/bin/moyai-desktop.exe'))) 'uninstall removes only unchanged application payload'
Assert-That (-not (Test-Path -LiteralPath (Join-Path $destination 'installation.json'))) 'uninstall resumes when a previous attempt already removed a file'
Assert-That ((Get-Content -LiteralPath $profile -Raw) -eq 'existing credential and history fixture') 'install, update, and uninstall retain data outside the installation'
# Exercise the actual cmd pause and exit status with an inert script, never an installer.
$wrapperRoot = Join-Path $EvidenceRoot 'interactive wrapper'
New-Item -ItemType Directory -Path (Join-Path $wrapperRoot 'scripts') | Out-Null
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'deployment/Setup-moyAI.cmd') -Destination (Join-Path $wrapperRoot 'Setup-moyAI.cmd')
Write-MoyaiUtf8 (Join-Path $wrapperRoot 'scripts/Setup-moyAI.ps1') @'
param([int]$FixtureExitCode)
[IO.File]::WriteAllText((Join-Path (Split-Path -Parent $PSScriptRoot) 'returned.txt'), [string]$FixtureExitCode)
Write-Output "fixture setup returned $FixtureExitCode"
exit $FixtureExitCode
'@
foreach ($expectedExit in @(0, 7)) {
  $returned = Join-Path $wrapperRoot 'returned.txt'
  if (Test-Path -LiteralPath $returned) { Remove-Item -LiteralPath $returned }
  $startInfo = [Diagnostics.ProcessStartInfo]::new()
  $startInfo.FileName = $env:ComSpec
  $startInfo.Arguments = '/d /c ""' + (Join-Path $wrapperRoot 'Setup-moyAI.cmd') + '" -FixtureExitCode ' + $expectedExit + '"'
  $startInfo.UseShellExecute = $false; $startInfo.CreateNoWindow = $true
  $startInfo.RedirectStandardInput = $true; $startInfo.RedirectStandardOutput = $true; $startInfo.RedirectStandardError = $true
  $wrapperProcess = [Diagnostics.Process]::Start($startInfo)
  $stdout = $wrapperProcess.StandardOutput.ReadToEndAsync(); $stderr = $wrapperProcess.StandardError.ReadToEndAsync()
  try {
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    while (-not (Test-Path -LiteralPath $returned) -and -not $wrapperProcess.HasExited -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
    Assert-That ((Test-Path -LiteralPath $returned) -and -not $wrapperProcess.WaitForExit(300)) "interactive wrapper keeps result visible until input after exit $expectedExit"
    $wrapperProcess.StandardInput.WriteLine(' '); $wrapperProcess.StandardInput.Close()
    Assert-That ($wrapperProcess.WaitForExit(5000) -and $wrapperProcess.ExitCode -eq $expectedExit) "interactive wrapper preserves exit $expectedExit after pause"
    Write-MoyaiUtf8 (Join-Path $EvidenceRoot "wrapper-$expectedExit.txt") ($stdout.Result + $stderr.Result)
  } finally {
    if (-not $wrapperProcess.HasExited) { $wrapperProcess.Kill(); [void]$wrapperProcess.WaitForExit(5000) }
    $wrapperProcess.Dispose()
  }
}
Write-MoyaiUtf8 (Join-Path $EvidenceRoot 'RESULTS.md') ("# Deployment focused tests`n`n" + ($checks -join "`n") + "`n`nOnly a generated argument-recording fixture and inert cmd wrapper were launched; no product executable was launched. Autostart and dedicated connection-file association tests used isolated fake registry owners; subprocess packages override registry openers and never open the real user's Run, Classes, or UserChoice keys. Existing system runtimes were used for prerequisite checking. Fresh-PC, bundled-runtime execution, Explorer/ShellExecute association resolution, and real shortcut GUI behavior remain separate validation.`n")
$checks | Write-Output
Write-Output "evidence=$EvidenceRoot"
