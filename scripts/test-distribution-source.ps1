param([string]$EvidenceRoot='')
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'deployment/component-install.ps1')
. (Join-Path $PSScriptRoot 'deployment/distribution-source.ps1')
if(-not $EvidenceRoot) { $EvidenceRoot=Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) ('project_sandbox/distribution-source-'+[Guid]::NewGuid().ToString('N')) }
$EvidenceRoot=[IO.Path]::GetFullPath($EvidenceRoot)
$package=Join-Path $EvidenceRoot '配布 folder & literal'
[void](New-Item -ItemType Directory -Path (Join-Path $package 'scripts'),(Join-Path $package 'app/bin') -Force)
$checks=[Collections.Generic.List[string]]::new()
function Check([bool]$condition,[string]$message) { if(-not $condition) { throw "FAIL: $message" }; $checks.Add($message) }
function Fails([scriptblock]$action,[string]$message) { $failed=$false; try { & $action | Out-Null } catch { $failed=$true }; Check $failed $message }
function Get-MoyaiComponentUserRoot([string]$Component) { Join-Path $EvidenceRoot 'local-data/desktop' }
foreach($relative in @('scripts/component-install.ps1','scripts/Setup-Component.ps1','scripts/sandbox-service.ps1','app/bin/moyai-desktop.exe')) { [IO.File]::WriteAllText((Join-Path $package $relative),'inert fixture '+$relative) }
function Write-Inventory {
  $files=@(Get-ChildItem -LiteralPath $package -Recurse -File | Where-Object Name -ne 'deployment.json' | ForEach-Object {
    @{path=$_.FullName.Substring($package.Length+1).Replace('\','/');component='Desktop';sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()}
  })
  Write-MoyaiComponentJson (Join-Path $package 'deployment.json') @{schema=1;product='moyai-windows-user';target='windows-x86_64';version='3.0.6';components=@{Desktop=@{version='3.0.6'}};files=$files}
}
Write-Inventory
Initialize-MoyaiDistributionNative
foreach($variant in 0..15) { Check ([MoyaiDistribution.Source]::IsCloud([uint32](0x9000001aL -bor ($variant -shl 12)))) "Cloud tag $variant is recognized" }
foreach($tag in @(0L,0xa0000003L,0xa000000cL,0x8000001bL,0x9001001aL)) { Check (-not [MoyaiDistribution.Source]::IsCloud([uint32]$tag)) "Non-cloud tag $tag is rejected" }
Check (-not(Test-MoyaiCloudDistribution $package)) 'An ordinary package is not classified as cloud input'
$prepared=New-MoyaiDistributionSource $package
try {
  Check ($prepared.root -ne $package -and $prepared.root.StartsWith($EvidenceRoot)) 'Preparation uses an isolated ordinary local cache'
  $verified=Read-MoyaiComponentManifest $prepared.root
  Check ($verified.files.Count -eq 4) 'All inventory files were copied and verified'
  $installed=[pscustomobject]@{version='3.0.6';files=@(Get-MoyaiComponentFiles $verified 'Desktop')}
  Check (Test-MoyaiInstalledDesktopMatches $installed $verified) 'Matching installed component can be reused'
  $installed.files[0].sha256='0'*64
  Check (-not(Test-MoyaiInstalledDesktopMatches $installed $verified)) 'Changed installed content requires installation'
  $other=New-MoyaiDistributionSource $package
  try { Check ($other.root -ne $prepared.root) 'Concurrent preparations use independent directories' } finally { Remove-MoyaiDistributionSource $other }
} finally { Remove-MoyaiDistributionSource $prepared }
Check (-not(Test-Path -LiteralPath $prepared.root)) 'Successful preparation cleanup removes its exact temporary tree'
[IO.File]::WriteAllText((Join-Path $package 'app/bin/moyai-desktop.exe'),'changed')
Fails { New-MoyaiDistributionSource $package } 'Changed source hash aborts before installation'
Check (@(Get-ChildItem -LiteralPath (Join-Path (Get-MoyaiComponentUserRoot Desktop) 'cache/distribution')).Count -eq 0) 'Failed preparation removes temporary files'
Write-Inventory
$manifestPath=Join-Path $package 'deployment.json'
$original=[IO.File]::ReadAllText($manifestPath)
foreach($badPath in @('../escape','app/../escape','C:/escape','deployment.json')) {
  $inventory=$original | ConvertFrom-Json
  $inventory.files[0].path=$badPath
  Write-MoyaiComponentJson $manifestPath $inventory
  Fails { New-MoyaiDistributionSource $package } "Invalid inventory path is refused: $badPath"
}
[IO.File]::WriteAllText($manifestPath,$original)
$outside=Join-Path $EvidenceRoot 'outside'; [void](New-Item -ItemType Directory -Path $outside)
$link=Join-Path $EvidenceRoot 'linked package'; [void](New-Item -ItemType Junction -Path $link -Target $package)
try {
  Fails { Test-MoyaiCloudDistribution $link } 'A junction is not accepted as a cloud source'
  Fails { New-MoyaiDistributionSource $link } 'Preparation refuses a junction ancestor'
} finally { [IO.Directory]::Delete($link) }
$pin=[MoyaiDistribution.Source]::Pin((Join-Path $package 'app/bin/moyai-desktop.exe'),$false)
$read=[MoyaiDistribution.Source]::Read((Join-Path $package 'app/bin/moyai-desktop.exe'),$pin)
try { Fails { [IO.File]::WriteAllText((Join-Path $package 'app/bin/moyai-desktop.exe'),'race') } 'A source held for copying cannot be overwritten during preparation' } finally { $read.Dispose(); $pin.Dispose() }
Check ((Get-Content -LiteralPath (Join-Path $package 'app/bin/moyai-desktop.exe') -Raw) -ceq 'changed') 'Source files are preserved'

# Removal needs verified control files, but must not hydrate an absent app image.
$appImage=Join-Path $package 'app/bin/moyai-desktop.exe'
Remove-Item -LiteralPath $appImage
Fails { New-MoyaiDistributionSource $package } 'Installation still rejects an absent app image'
$partial=New-MoyaiDistributionSource $package -ForUninstall
try {
  Check ($partial.manifest.uninstall_only -and $partial.manifest.files.Count -eq 3) 'Removal stages only the remaining verified control files'
  Check (-not(Test-Path -LiteralPath (Join-Path $partial.root 'app/bin/moyai-desktop.exe'))) 'Removal does not require or reconstruct missing application files'
} finally { Remove-MoyaiDistributionSource $partial }
$control=Join-Path $package 'scripts/Setup-Component.ps1'
$originalControl=[IO.File]::ReadAllText($control)
[IO.File]::WriteAllText($control,'changed control')
Fails { New-MoyaiDistributionSource $package -ForUninstall } 'Removal refuses a changed control script'
Remove-Item -LiteralPath $control
Fails { New-MoyaiDistributionSource $package -ForUninstall } 'Removal refuses a missing required control script'
[IO.File]::WriteAllText($control,$originalControl)
[IO.File]::WriteAllText($appImage,'changed')
Check (@(Get-ChildItem -LiteralPath (Join-Path (Get-MoyaiComponentUserRoot Desktop) 'cache/distribution')).Count -eq 0) 'Partial removal preparation leaves no staging after success or refusal'

# Run the production handoff against real staged files. Only the privileged
# installation and installed-component lookup are substituted by this fixture.
$script:installedFixture=$null; $script:installCount=0; $script:cancelInstall=$false
function Read-MoyaiInstalledComponent([string]$Component) { Check ($Component -ceq 'Desktop') 'Cloud entry looks up only Desktop'; return $script:installedFixture }
function Invoke-MoyaiComponentUserSetup([string]$SourceRoot,[string[]]$Components,[switch]$SkipStartupCheck) {
  Check (($Components -join ',') -ceq 'Desktop' -and $SkipStartupCheck) 'Cloud entry installs only Desktop and preserves the requested launch'
  if($script:cancelInstall) { throw 'fixture: UAC cancelled' }
  $verified=Read-MoyaiComponentManifest $SourceRoot
  $script:installCount++
  $script:installedFixture=[pscustomobject]@{version=$verified.components.Desktop.version;install_root=(Join-Path $EvidenceRoot 'Program Files/Desktop');files=@(Get-MoyaiComponentFiles $verified 'Desktop')}
}
$resolved=Resolve-MoyaiDesktopDistribution $package
Check ($resolved -ceq $script:installedFixture.install_root -and $script:installCount -eq 1) 'A new cloud launch hands off to the installed location'
Check ((Resolve-MoyaiDesktopDistribution $package) -ceq $resolved -and $script:installCount -eq 1) 'Matching installation is reused without another setup'
$script:installedFixture.version='older'
[void](Resolve-MoyaiDesktopDistribution $package)
Check ($script:installCount -eq 2) 'A different installed version is updated'
$script:installedFixture=$null; $script:cancelInstall=$true
Fails { Resolve-MoyaiDesktopDistribution $package } 'Cancelled installation cannot launch a staged or cloud executable'
Check (@(Get-ChildItem -LiteralPath (Join-Path (Get-MoyaiComponentUserRoot Desktop) 'cache/distribution')).Count -eq 0) 'Handoff cleans staging after both success and cancellation'

# Exercise the actual entry script with inert setup effects, including defaults.
$entryRoot=Join-Path $EvidenceRoot 'entry'
$entryScripts=Join-Path $entryRoot 'scripts'; [void](New-Item -ItemType Directory -Path $entryScripts)
foreach($name in @('Setup-moyAI.ps1','distribution-source.ps1')) { Copy-Item -LiteralPath (Join-Path $PSScriptRoot "deployment/$name") -Destination (Join-Path $entryScripts $name) }
$stub=@'
function Assert-MoyaiUserSetup { }
function Get-MoyaiComponentLocation([string]$Component) { @{parent='fixture'} }
function Invoke-MoyaiComponentUserSetup {
 param($SourceRoot,$Components,[switch]$Uninstall,[switch]$CheckOnly,[switch]$NoShortcuts,[switch]$SkipStartupCheck,[switch]$ResetDesktopData,[switch]$ResetHubData,[switch]$RemoveSharedRuntime,[switch]$Interactive,$WebPort,$HubDataDir)
 [IO.File]::WriteAllText((Join-Path $SourceRoot 'selection.json'),(@{components=@($Components);uninstall=[bool]$Uninstall;shared=[bool]$RemoveSharedRuntime} | ConvertTo-Json))
}
'@
[IO.File]::WriteAllText((Join-Path $entryScripts 'common.ps1'),$stub,[Text.UTF8Encoding]::new($true))
Write-MoyaiComponentJson (Join-Path $entryRoot 'deployment.json') @{components=@{Desktop=@{version='3.0.6'};Hub=@{version='1.0.3'}};files=@()}
$ps=Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell/v1.0/powershell.exe'
foreach($case in @(@{args=@();expected='Desktop'},@{args=@('-Uninstall');expected='Desktop'},@{args=@('-Components','Hub');expected='Hub'},@{args=@('-Uninstall','-RemoveSharedRuntime');expected=''})) {
  & $ps -NoProfile -ExecutionPolicy Bypass -File (Join-Path $entryScripts 'Setup-moyAI.ps1') @($case.args)
  Check ($LASTEXITCODE -eq 0) 'Setup entry completed with fixture effects'
  $selection=Get-Content -LiteralPath (Join-Path $entryRoot 'selection.json') -Raw | ConvertFrom-Json
  Check (($selection.components -join ',') -ceq $case.expected) "Setup component selection: $($case.args -join ' ') => $($case.expected)"
}
$checks | ForEach-Object { "PASS: $_" }
Write-MoyaiComponentJson (Join-Path $EvidenceRoot 'results.json') @{checks=$checks;real_cloud_provider_tested=$false}
