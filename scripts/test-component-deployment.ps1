param([string]$EvidenceRoot='')
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'deployment/component-install.ps1')
if(-not $EvidenceRoot) { $EvidenceRoot=Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) ('project_sandbox/component-deployment-'+[DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfff')) }
$EvidenceRoot=[IO.Path]::GetFullPath($EvidenceRoot)
$package=Join-Path $EvidenceRoot 'package'
$machine=Join-Path $EvidenceRoot 'Program Files/moyAI'
[void](New-Item -ItemType Directory -Path (Join-Path $package 'scripts'),(Join-Path $package 'app/bin'),(Join-Path $package 'hub/bin'),$machine -Force)
foreach($name in @('component-install.ps1','Setup-Component.ps1','sandbox-service.ps1')) { Copy-Item -LiteralPath (Join-Path $PSScriptRoot "deployment/$name") -Destination (Join-Path $package "scripts/$name") }
[IO.File]::WriteAllText((Join-Path $package 'app/bin/moyai-desktop.exe'),'inert Desktop fixture')
[IO.File]::WriteAllText((Join-Path $package 'hub/bin/moyai-hub.exe'),'inert Hub fixture one')
[IO.File]::WriteAllText((Join-Path $package 'hub/bin/moyai-cleanup-hub.exe'),'inert Hub cleanup fixture')
[IO.File]::WriteAllText((Join-Path $package 'hub/bin/web-management.json'),'{"settings":{"local_port":9080}}')
$checks=[Collections.Generic.List[string]]::new()
function Assert-That([bool]$Value,[string]$Message) { if(-not $Value) { throw "FAIL: $Message" }; $checks.Add($Message) }
function Assert-Fails([scriptblock]$Action,[string]$Message) { $failed=$false; try { & $Action | Out-Null } catch { $failed=$true }; Assert-That $failed $Message }
function Write-FixtureManifest([string]$Version='1.0.1') {
    $files=@(Get-ChildItem -LiteralPath $package -Recurse -File | Where-Object { $_.Name -cne 'deployment.json' } | ForEach-Object {
        $relative=$_.FullName.Substring($package.Length+1).Replace('\','/')
        $component=if($relative.StartsWith('scripts/')) { @('Desktop','Hub') } elseif($relative.StartsWith('app/')) { 'Desktop' } else { 'Hub' }
        $target=if($relative.StartsWith('hub/bin/')) { $relative.Substring(8) } else { $relative }
        [ordered]@{path=$relative;sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant();component=$component;install_path=$target}
    })
    Write-MoyaiComponentJson (Join-Path $package 'deployment.json') ([ordered]@{schema=1;product='moyai-windows-user';target='windows-x86_64';version='3.0.4';runtime=@{webview2='system-installed';visual_cpp='system-installed'};hub=@{version=$Version};components=@{Desktop=@{version='3.0.4'};Hub=@{version=$Version}};files=$files})
    Read-MoyaiComponentManifest $package
}

# The transaction runs real copies/pins against this workspace. No SCM, ACL,
# Program Files, HKCU, or actual application process is accessed by the fixture.
function Assert-MoyaiSandboxAdministrator { }
function Assert-MoyaiSandboxProtectedPath([string]$Path) { Assert-MoyaiSandboxRegularPath $Path }
function Set-MoyaiSandboxProtection([string]$Path,[bool]$Directory,[switch]$Private) { }
function Set-MoyaiSandboxParentAccess([string]$Path) { }
function Set-MoyaiComponentTreeOwner([string]$Root) { Assert-MoyaiSandboxRegularPath $Root }
function Assert-MoyaiComponentStopped([string]$Root,[object[]]$Processes=@()) { }
function Get-MoyaiComponentLocation([string]$Component) { [pscustomobject]@{parent=$machine;root=(Join-Path $machine $Component);component=$Component} }
function Invoke-MoyaiMachineSandboxSetup { throw 'Fixture attempted to access actual machine sandbox setup.' }
function Get-MoyaiComponentResultRoot { Join-Path $EvidenceRoot 'ProgramData/moyAI/setup-results' }
function Write-MoyaiComponentResult([string]$OperationId,[string]$ManifestSha256,[string[]]$Components,[bool]$Success,[int]$ExitCode,[string]$Message,[string[]]$Messages=@()) {
    $path=Get-MoyaiComponentResultPath $OperationId
    [void](New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force)
    $stream=[IO.File]::Open($path,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::Read)
    try {
        $bytes=[Text.Encoding]::UTF8.GetBytes((@{schema=1;operation_id=$OperationId;source_manifest_sha256=$ManifestSha256;components=@($Components | Where-Object {-not[string]::IsNullOrWhiteSpace($_)});success=$Success;exit_code=$ExitCode;message=$Message;messages=$Messages} | ConvertTo-Json -Depth 8))
        $stream.Write($bytes,0,$bytes.Length)
    } finally { $stream.Dispose() }
}

$manifest=Write-FixtureManifest
Assert-That (@(Get-MoyaiComponentFiles $manifest 'Hub').Count -eq 6) 'shared scripts and remapped Hub files form an independent component'
$pins=New-MoyaiComponentSourcePins $package $manifest
try {
    Assert-Fails { [IO.File]::WriteAllText((Join-Path $package 'scripts/Setup-Component.ps1'),'changed') } 'source management script is pinned against replacement while approval is pending'
    Assert-Fails { [IO.File]::WriteAllText((Join-Path $package 'deployment.json'),'changed') } 'source manifest is pinned against modification'
} finally { Close-MoyaiSandboxSourcePins $pins }
Assert-Fails { Read-MoyaiComponentManifest $package ('0'*64) } 'elevated entry rejects a changed manifest snapshot'
$desktop=Join-Path $machine 'Desktop'
[void](New-Item -ItemType Directory -Path $desktop)
[IO.File]::WriteAllText((Join-Path $desktop 'sentinel.txt'),'existing Desktop must survive Hub operations')
$desktopHash=(Get-FileHash -LiteralPath (Join-Path $desktop 'sentinel.txt')).Hash
Invoke-MoyaiMachineComponent $package @('Hub') -ExpectedManifestSha256 $manifest.validated_manifest_sha256
$hub=Join-Path $machine 'Hub'
Assert-That (Test-Path -LiteralPath (Join-Path $hub 'moyai-hub.exe')) 'Hub-only installation uses its own Program Files component directory'
Assert-That (-not(Test-Path -LiteralPath (Join-Path $hub 'app'))) 'Hub installation does not contain Desktop payload'
Assert-That ((Get-FileHash -LiteralPath (Join-Path $desktop 'sentinel.txt')).Hash -ceq $desktopHash) 'Hub installation preserves an independent Desktop'
$installed=Read-MoyaiInstalledComponent 'Hub'
Assert-That ($installed.version -ceq '1.0.1' -and $installed.source_manifest_sha256 -ceq $manifest.validated_manifest_sha256) 'machine receipt identifies component version and source inventory'
$projection=Read-MoyaiComponentManifest $hub
Assert-That ($projection.product -ceq 'moyai-hub-windows' -and @($projection.components.PSObject.Properties.Name).Count -eq 1 -and $projection.components.PSObject.Properties.Name -ccontains 'Hub') 'installed Hub has a self-contained inventory for subsequent setup and removal'
[IO.File]::WriteAllText((Join-Path $hub 'user-added.txt'),'preserve unknown content')
[IO.File]::WriteAllText((Join-Path $package 'hub/bin/moyai-hub.exe'),'inert Hub fixture two')
$manifest=Write-FixtureManifest '1.0.2'
$update=@(Invoke-MoyaiMachineComponent $package @('Hub') -ExpectedManifestSha256 $manifest.validated_manifest_sha256)
Assert-That ((Get-Content -LiteralPath (Join-Path $hub 'moyai-hub.exe') -Raw) -ceq 'inert Hub fixture two') 'Hub update replaces only the selected component payload'
$backups=@(Get-ChildItem -LiteralPath $machine -Directory -Filter 'Hub.previous-*')
Assert-That ($backups.Count -eq 1 -and (Test-Path -LiteralPath (Join-Path $backups[0].FullName 'user-added.txt')) -and -not(Test-Path -LiteralPath (Join-Path $backups[0].FullName 'moyai-hub.exe'))) 'update removes known old binaries while retaining and reporting unowned files'
Assert-That (($update -join "`n").Contains('user-added.txt')) 'retained old content is explicitly reported'
[IO.File]::WriteAllText((Join-Path $hub 'moyai-hub.exe'),'changed installed payload')
Assert-Fails { Read-MoyaiInstalledComponent 'Hub' } 'normal component lookup refuses a modified installed executable'
$removal=@(Invoke-MoyaiMachineComponent $package @('Hub') -Uninstall -ExpectedManifestSha256 $manifest.validated_manifest_sha256)
Assert-That ((Test-Path -LiteralPath (Join-Path $hub 'moyai-hub.exe')) -and (Test-Path -LiteralPath (Join-Path $hub 'machine-component.json'))) 'uninstall retains changed payload and its ownership receipt'
Assert-That (($removal -join "`n").Contains('moyai-hub.exe')) 'uninstall reports changed files rather than claiming complete deletion'
Copy-Item -LiteralPath (Join-Path $package 'hub/bin/moyai-hub.exe') -Destination (Join-Path $hub 'moyai-hub.exe') -Force
Invoke-MoyaiMachineComponent $package @('Hub') -Uninstall -ExpectedManifestSha256 $manifest.validated_manifest_sha256
Assert-That (-not(Test-Path -LiteralPath $hub)) 'repeated uninstall can finish after partial removal'
Assert-That ((Get-FileHash -LiteralPath (Join-Path $desktop 'sentinel.txt')).Hash -ceq $desktopHash) 'Hub update and uninstall preserve Desktop bytes'
$bad=Get-Content -LiteralPath (Join-Path $package 'deployment.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$bad.files[0].install_path='../Desktop/sentinel.txt'
Write-MoyaiComponentJson (Join-Path $package 'deployment.json') $bad
Assert-Fails { Read-MoyaiComponentManifest $package } 'component target traversal cannot overwrite another product'
$manifest=Write-FixtureManifest
$bad=Get-Content -LiteralPath (Join-Path $package 'deployment.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$bad.files[0].component=@('Desktop','Hub','Unexpected')
Write-MoyaiComponentJson (Join-Path $package 'deployment.json') $bad
Assert-Fails { Read-MoyaiComponentManifest $package } 'unknown component owners are rejected before installation'
$manifest=Write-FixtureManifest
Invoke-MoyaiMachineComponent $package @('Hub') -ExpectedManifestSha256 $manifest.validated_manifest_sha256
$selfManifest=Read-MoyaiComponentManifest $hub
Assert-Fails { Invoke-MoyaiMachineComponent $hub @('Hub') -ExpectedManifestSha256 $selfManifest.validated_manifest_sha256 } 'update from its own installation is rejected before any files change'
function Start-Process($FilePath,$ArgumentList,$Verb,$WindowStyle,[switch]$PassThru,[switch]$Wait) {
    $snapshot=[regex]::Match($ArgumentList,' -SourceRoot "([^"]+)"').Groups[1].Value
    $expected=[regex]::Match($ArgumentList,' -ExpectedManifestSha256 ([a-f0-9]{64})').Groups[1].Value
    $operation=[regex]::Match($ArgumentList,' -OperationId ([a-f0-9]{32})').Groups[1].Value
    if(-not $snapshot -or -not $expected) { throw 'Fixture could not inspect fixed elevated arguments.' }
    $messages=@(Invoke-MoyaiMachineComponent $snapshot @('Hub') -Uninstall -ExpectedManifestSha256 $expected)
    Write-MoyaiComponentResult $operation $expected @('Hub') $true 0 'completed fixture' $messages
    [pscustomobject]@{ExitCode=0}
}
$elevation=Invoke-MoyaiComponentElevation $hub $selfManifest @('Hub') -Uninstall
try {
    Assert-That (-not(Test-Path -LiteralPath $hub) -and $elevation.source_root -ine $hub) 'self-uninstall executes from a pinned snapshot without pinning its deletion target'
    Assert-That (Test-Path -LiteralPath (Join-Path $elevation.source_root 'moyai-hub.exe')) 'post-approval user cleanup retains its source snapshot until final completion'
} finally { Close-MoyaiSandboxSourcePins $elevation.pins; Remove-MoyaiComponentSourceSnapshot $elevation.source_root $elevation.manifest }
Assert-That (-not(Test-Path -LiteralPath $elevation.source_root)) 'successful uninstall removes its temporary control snapshot'
function Assert-MoyaiUserSetup { }
$script:registryTouched=$false; $script:dataTouched=$false
function Remove-MoyaiHubUserRegistrations { $script:registryTouched=$true }
function Invoke-MoyaiComponentCleanup { $script:dataTouched=$true }
$script:approvalAttempted=$false
function Start-Process { $script:approvalAttempted=$true; throw [ComponentModel.Win32Exception]::new(1223,'User cancelled approval fixture') }
Assert-Fails { Invoke-MoyaiComponentUserSetup -SourceRoot $package -Components @('Hub') -Uninstall -ResetHubData } 'administrator approval cancellation stops uninstall before data reset'
Assert-That ($script:approvalAttempted -and -not $script:registryTouched -and -not $script:dataTouched) 'cancelled UAC preserves both user registrations and saved data'
Assert-Fails { Invoke-MoyaiComponentUserSetup -SourceRoot $package -Components @('Hub') -WebPort 0 -CheckOnly } 'port zero is rejected rather than treated as unspecified'
$cleanupRoot=Join-Path $desktop 'app/maintenance'
[void](New-Item -ItemType Directory -Path $cleanupRoot -Force)
$cleanupPath=Join-Path $cleanupRoot 'moyai-cleanup-desktop.exe'
[IO.File]::WriteAllText($cleanupPath,'inert Desktop cleanup fixture')
$cleanupHash=(Get-FileHash -LiteralPath $cleanupPath -Algorithm SHA256).Hash.ToLowerInvariant()
Write-MoyaiComponentJson (Join-Path $desktop 'machine-component.json') ([ordered]@{schema=1;product='moyai-machine-component';component='Desktop';install_root=$desktop;version='3.0.4';source_manifest_sha256=('1'*64);files=@(@{path='app/maintenance/moyai-cleanup-desktop.exe';sha256=$cleanupHash})})
$hubOnly=$manifest | ConvertTo-Json -Depth 32 | ConvertFrom-Json
$hubOnly.product='moyai-hub-windows'; $hubOnly.components=[pscustomobject]@{Hub=[pscustomobject]@{version='1.0.1'}}
$hubOnly.files=@($hubOnly.files | Where-Object { @($_.component) -ccontains 'Hub' } | ForEach-Object { $_.component='Hub'; $_ })
Write-MoyaiComponentJson (Join-Path $package 'deployment.json') $hubOnly
$hubOnly=Read-MoyaiComponentManifest $package
$snapshot=New-MoyaiComponentSourceSnapshot $package $hubOnly @('Hub','Desktop')
try {
    $desktopCleanup=@(Get-MoyaiComponentFiles $snapshot.manifest 'Desktop')
    Assert-That ($snapshot.manifest.uninstall_only -and $desktopCleanup.Count -eq 1 -and $desktopCleanup[0].sha256 -ceq $cleanupHash -and (Test-Path -LiteralPath (Join-Path $snapshot.root $desktopCleanup[0].source))) 'single-product setup can snapshot the other installed component cleanup for combined removal'
    Assert-Fails { Invoke-MoyaiMachineComponent $snapshot.root @('Hub') -ExpectedManifestSha256 $snapshot.manifest.validated_manifest_sha256 } 'combined uninstall snapshot cannot be reused for installation'
} finally { Remove-MoyaiComponentSourceSnapshot $snapshot.root $snapshot.manifest }
$script:legacySweepCalled=$false
$realLegacySweep=${function:Remove-MoyaiLegacyUserModules}
function Remove-MoyaiLegacyUserModules { $script:legacySweepCalled=$true }
function Start-Process($FilePath,$ArgumentList) {
    $expected=[regex]::Match($ArgumentList,' -ExpectedManifestSha256 ([a-f0-9]{64})').Groups[1].Value
    $operation=[regex]::Match($ArgumentList,' -OperationId ([a-f0-9]{32})').Groups[1].Value
    Write-MoyaiComponentResult $operation $expected @('Hub') $true 0 'completed fixture'
    [pscustomobject]@{ExitCode=0}
}
Invoke-MoyaiComponentUserSetup -SourceRoot $package -Components @('Hub') -Uninstall
Assert-That (-not $script:legacySweepCalled) 'Hub-only removal never sweeps legacy Desktop installations'
Set-Item -LiteralPath Function:Remove-MoyaiLegacyUserModules -Value $realLegacySweep
$legacy=Join-Path $EvidenceRoot 'old Programs/moyAI'
[void](New-Item -ItemType Directory -Path (Join-Path $legacy 'app/bin'),(Join-Path $legacy 'hub/bin') -Force)
[IO.File]::WriteAllText((Join-Path $legacy 'app/bin/moyai-desktop.exe'),'old Desktop fixture')
[IO.File]::WriteAllText((Join-Path $legacy 'hub/bin/moyai-hub.exe'),'old Hub fixture')
$legacyFiles=@('app/bin/moyai-desktop.exe','hub/bin/moyai-hub.exe') | ForEach-Object { [pscustomobject]@{path=$_;sha256=(Get-FileHash -LiteralPath (Join-Path $legacy $_)).Hash.ToLowerInvariant()} }
Write-MoyaiComponentJson (Join-Path $legacy 'deployment.json') ([ordered]@{version='3.0.3';hub=@{version='1.0.0'};files=@($legacyFiles)})
Write-MoyaiComponentJson (Join-Path $legacy 'installation.json') ([ordered]@{product='moyai-windows-user';version='3.0.3';install_root=$legacy})
function Get-MoyaiLegacyUserRoot { $legacy }
function Read-MoyaiDeployment([string]$Root,[switch]$AllowMissingFiles) { Get-Content -LiteralPath (Join-Path $Root 'deployment.json') -Raw -Encoding UTF8 | ConvertFrom-Json }
$retained=@(Remove-MoyaiLegacyUserModules)
Assert-That ((Test-Path -LiteralPath (Join-Path $legacy 'app/bin/moyai-desktop.exe')) -and (Test-Path -LiteralPath (Join-Path $legacy 'hub/bin/moyai-hub.exe')) -and ($retained -join "`n").Contains('Hub')) 'Desktop-only legacy cleanup retains an old bundle that still supplies Hub'
$script:oldStartupProjection=$null; $script:oldAssociationSelected=$false
function Remove-MoyaiAutostart($Root,$Receipt,$Manifest) { $script:oldStartupProjection=$Manifest }
function Remove-MoyaiJoinFileAssociation { $script:oldAssociationSelected=$true }
Move-MoyaiLegacyDesktopRegistrations
Assert-That ($script:oldAssociationSelected -and @($script:oldStartupProjection.files).Count -eq 1 -and $script:oldStartupProjection.files[0].path -ceq 'app/bin/moyai-desktop.exe') 'Desktop registration migration leaves legacy Hub autostart outside its selected scope'
Move-MoyaiLegacyDesktopRegistrations -IncludeHub
Assert-That (@($script:oldStartupProjection.files).Count -eq 1 -and $script:oldStartupProjection.files[0].path -ceq 'app/bin/moyai-desktop.exe') 'combined update does not retire Hub autostart before the Hub profile migration can preserve its enabled state'
$script:legacyReferenceEntries=@([pscustomobject]@{name='custom Hub startup';command=('"'+(Join-Path $legacy 'hub/bin/moyai-hub.exe')+'" --custom')})
function Get-MoyaiLegacyRegistrationReference($Root) { Find-MoyaiLegacyRegistrationReference $Root $script:legacyReferenceEntries }
$preserved=@(Remove-MoyaiLegacyUserModules -IncludeHub -ForUpdate)
Assert-That ((Test-Path -LiteralPath (Join-Path $legacy 'hub/bin/moyai-hub.exe')) -and ($preserved -join "`n").Contains('custom Hub startup')) 'update keeps an old module still referenced by a preserved custom startup command'
Assert-That ($null -eq (Find-MoyaiLegacyRegistrationReference $legacy @([pscustomobject]@{name='other';command=('"'+$legacy+'-other\moyai-hub.exe"')}))) 'reference detection does not mistake a similarly named installation for the selected root'
Remove-MoyaiLegacyUserModules -IncludeHub
Assert-That (-not(Test-Path -LiteralPath $legacy)) 'explicit combined legacy cleanup removes only the inventoried old module bundle'
Assert-That ((Get-MoyaiComponentUserReceiptPath 'Desktop') -notlike ((Get-MoyaiComponentUserRoot 'Desktop')+'\*')) 'installer metadata lives outside the Desktop data leaf and cannot block first data migration'
$operation=[Guid]::NewGuid().ToString('N')
Write-MoyaiComponentResult $operation ('1'*64) @('Hub') $false 1 'fixture: destination is busy; retained exact path' @('retained fixture directory')
$failure=Read-MoyaiComponentResult $operation ('1'*64) @('Hub') 1
Assert-That ($failure.message.Contains('destination is busy') -and $failure.messages[0].Contains('retained')) 'protected result preserves detailed failure and retained-path notices for the caller'
Assert-Fails { Read-MoyaiComponentResult $operation ('2'*64) @('Hub') 1 } 'caller rejects a result for another source inventory'
Assert-Fails { Read-MoyaiComponentResult $operation ('1'*64) @('Desktop') 1 } 'caller rejects a result for another selected component'
Assert-Fails { Write-MoyaiComponentResult $operation ('1'*64) @('Hub') $true 0 'overwrite' } 'result is create-new and cannot overwrite an existing request receipt'
Assert-Fails { Get-MoyaiComponentResultPath '../outside' } 'setup result request cannot select an arbitrary elevated output path'
$manifest=Write-FixtureManifest
$script:runtimeChecked=$false; $script:helperChecked=$false
function Test-MoyaiRuntime($Root,$Manifest,[switch]$PreparingInstall) { $script:runtimeChecked=[bool]$PreparingInstall }
function Ensure-MoyaiSandboxInstallation($Root,$Manifest,[switch]$CheckOnly) { $script:helperChecked=[bool]$CheckOnly }
Invoke-MoyaiComponentUserSetup -SourceRoot $package -Components @('Desktop') -CheckOnly
Assert-That ($script:runtimeChecked -and $script:helperChecked) 'Desktop check-only validates runtime and helper before reporting completion'
$script:serviceFixture=[pscustomobject]@{State=4}
$script:removalOrder=[Collections.Generic.List[string]]::new()
function Get-MoyaiSandboxLocation { [pscustomobject]@{parent=$machine;root=(Join-Path $machine 'SandboxService');service='MoyAISandbox'} }
function Get-MoyaiSandboxService([switch]$Security) { $script:serviceFixture }
function Assert-MoyaiSandboxService { }
function Read-MoyaiSandboxInstallation { [pscustomobject]@{files=@()} }
function Invoke-MoyaiMachineSandboxSetup($Root,[switch]$Uninstall) { if(-not $Uninstall){throw 'Unexpected fixture installation'}; $script:removalOrder.Add('service'); $script:serviceFixture=$null }
$beforeShared=(Get-FileHash -LiteralPath $cleanupPath).Hash
Assert-Fails { Invoke-MoyaiMachineComponent $package @('Desktop','Hub') -Uninstall -RemoveSharedRuntime -ExpectedManifestSha256 $manifest.validated_manifest_sha256 } 'running shared service refuses combined removal before touching selected payload'
Assert-That ((Get-FileHash -LiteralPath $cleanupPath).Hash -ceq $beforeShared -and $script:removalOrder.Count -eq 0) 'shared-service preflight rejection preserves application bytes'
$realRemovePayload=${function:Remove-MoyaiComponentPayload}
function Remove-MoyaiComponentPayload($Root,$Receipt) { $script:removalOrder.Add('payload'); & $realRemovePayload $Root $Receipt }
$script:serviceFixture.State=1
Invoke-MoyaiMachineComponent $package @('Desktop','Hub') -Uninstall -RemoveSharedRuntime -ExpectedManifestSha256 $manifest.validated_manifest_sha256
Assert-That ($script:removalOrder.Count -ge 2 -and $script:removalOrder[0] -ceq 'service' -and $script:removalOrder[1] -ceq 'payload') 'canonical service removal finishes before application deletion can begin'
$hub=(Get-MoyaiComponentLocation 'Hub').root
[void](New-Item -ItemType Directory -Path $hub -Force)
$newHub=Join-Path $hub 'moyai-hub.exe'; [IO.File]::WriteAllText($newHub,'new fixed Hub fixture')
$newHash=(Get-FileHash -LiteralPath $newHub).Hash.ToLowerInvariant()
Write-MoyaiComponentJson (Join-Path $hub 'machine-component.json') ([ordered]@{schema=1;product='moyai-machine-component';component='Hub';install_root=$hub;version='1.0.1';source_manifest_sha256=('3'*64);files=@(@{path='moyai-hub.exe';sha256=$newHash})})
$oldBundle=Join-Path $EvidenceRoot 'bundle'
[void](New-Item -ItemType Directory -Path (Join-Path $oldBundle 'hub/bin') -Force)
$oldBundleExe=Join-Path $oldBundle 'hub/bin/moyai-hub.exe'; [IO.File]::WriteAllText($oldBundleExe,'old bundled Hub fixture')
$oldBundleHash=(Get-FileHash -LiteralPath $oldBundleExe).Hash.ToLowerInvariant()
Write-MoyaiComponentJson (Join-Path $oldBundle 'deployment.json') ([ordered]@{schema=1;product='moyai-windows-user';target='windows-x86_64';hub=@{sha256=$oldBundleHash;source_clean=$true;git_commit=('4'*40)};files=@(@{path='hub/bin/moyai-hub.exe';sha256=$oldBundleHash})})
$oldStandalone=Join-Path $EvidenceRoot 'single'
[void](New-Item -ItemType Directory -Path $oldStandalone -Force)
$oldStandaloneExe=Join-Path $oldStandalone 'moyai-hub.exe'; [IO.File]::WriteAllText($oldStandaloneExe,'old standalone Hub fixture')
$oldStandaloneHash=(Get-FileHash -LiteralPath $oldStandaloneExe).Hash.ToLowerInvariant()
Write-MoyaiComponentJson (Join-Path $oldStandalone 'build-identity.json') ([ordered]@{version='1.0.0';target='windows-x86_64';git_commit=('5'*40);source_clean=$true;sha256=$oldStandaloneHash})
$identityHash=(Get-FileHash -LiteralPath (Join-Path $oldStandalone 'build-identity.json')).Hash.ToLowerInvariant()
Write-MoyaiComponentJson (Join-Path $oldStandalone 'manifest.json') ([ordered]@{schema='moyai-hub.standalone-release.v1';target='windows-x86_64';version='1.0.0';git_commit=('5'*40);source_clean=$true;sha256=$oldStandaloneHash;files=@(@{path='moyai-hub.exe';sha256=$oldStandaloneHash},@{path='build-identity.json';sha256=$identityHash})})
$entries=@{}
$entries['moyAI Hub 1111111111111111']=@{kind=[Microsoft.Win32.RegistryValueKind]::String;value=('"'+$oldBundleExe+'" --launch --no-browser --data-dir "C:\old-profile"')}
$entries['moyAI Hub 2222222222222222']=@{kind=[Microsoft.Win32.RegistryValueKind]::String;value=('"'+$oldStandaloneExe+'" --launch --no-browser --data-dir "C:\custom 日本語"')}
$entries['moyAI Hub 3333333333333333']=@{kind=[Microsoft.Win32.RegistryValueKind]::String;value=('"'+$oldBundleExe+'" --launch --no-browser --data-dir "C:\edited" --extra')}
$entries['moyAI Hub 4444444444444444']=@{kind=[Microsoft.Win32.RegistryValueKind]::String;value='"C:\unknown\moyai-hub.exe" --launch --no-browser --data-dir "C:\unknown-profile"'}
$entries['unrelated']=@{kind=[Microsoft.Win32.RegistryValueKind]::String;value='unrelated startup'}
$oldMachine=Join-Path $EvidenceRoot 'oldMachine'
[void](New-Item -ItemType Directory -Path $oldMachine -Force)
$oldMachineExe=Join-Path $oldMachine 'moyai-hub.exe'; [IO.File]::WriteAllText($oldMachineExe,'old machine component fixture')
$oldMachineHash=(Get-FileHash -LiteralPath $oldMachineExe).Hash.ToLowerInvariant()
Write-MoyaiComponentJson (Join-Path $oldMachine 'machine-component.json') ([ordered]@{schema=1;product='moyai-machine-component';component='Hub';install_root=$hub;source_manifest_sha256=('6'*64);files=@(@{path='moyai-hub.exe';sha256=$oldMachineHash})})
$entries['moyAI Hub 7777777777777777']=@{kind=[Microsoft.Win32.RegistryValueKind]::String;value=('"'+$oldMachineExe+'" --launch --no-browser --data-dir "C:\machine-profile"')}
$originalEntries=@{}; foreach($name in $entries.Keys){$originalEntries[$name]=$entries[$name].value}
$fakeKey=[pscustomobject]@{items=$entries;writes=[Collections.Generic.List[string]]::new();reads=@{};race_name=''}
$fakeKey | Add-Member ScriptMethod GetValueNames { @($this.items.Keys) }
$fakeKey | Add-Member ScriptMethod GetValueKind {param($name) $this.items[$name].kind}
$fakeKey | Add-Member ScriptMethod GetValue {param($name,$fallback,$options) if(-not $this.reads.ContainsKey($name)){$this.reads[$name]=0};$this.reads[$name]++;if($this.race_name -ceq $name -and $this.reads[$name] -eq 2){$this.items[$name].value='changed concurrently'};$this.items[$name].value}
$fakeKey | Add-Member ScriptMethod SetValue {param($name,$value,$kind) $this.items[$name].value=$value;$this.items[$name].kind=$kind;$this.writes.Add($name)}
$retarget=@(Move-MoyaiHubAutostartExecutable $fakeKey)
Assert-That ($fakeKey.writes.Count -eq 3 -and $entries['moyAI Hub 1111111111111111'].value -ceq ('"'+$newHub+'" --launch --no-browser --data-dir "C:\old-profile"')) 'known old bundle Hub autostart keeps its profile and ON state while only its executable changes'
Assert-That ($entries['moyAI Hub 2222222222222222'].value -ceq ('"'+$newHub+'" --launch --no-browser --data-dir "C:\custom 日本語"')) 'standalone custom profile autostart follows the fixed executable without moving its data directory'
Assert-That ($entries['moyAI Hub 3333333333333333'].value -ceq $originalEntries['moyAI Hub 3333333333333333'] -and $entries['moyAI Hub 4444444444444444'].value -ceq $originalEntries['moyAI Hub 4444444444444444'] -and $entries['unrelated'].value -ceq $originalEntries['unrelated']) 'edited, unknown and unrelated startup commands remain unchanged'
Assert-That (($retarget -join "`n").Contains('保持しました') -and $entries.Count -eq 6) 'unowned registrations have a retention reason and OFF profiles are not enabled'
Assert-That ($entries['moyAI Hub 7777777777777777'].value -ceq ('"'+$newHub+'" --launch --no-browser --data-dir "C:\machine-profile"')) 'protected component receipt can identify a known old Hub executable independently of profile storage'
$fakeKey.items=@{'moyAI Hub 5555555555555555'=@{kind=[Microsoft.Win32.RegistryValueKind]::String;value=('"'+$oldStandaloneExe+'" --launch --no-browser --data-dir "C:\race"')}}
$fakeKey.reads=@{};$fakeKey.writes.Clear();$fakeKey.race_name='moyAI Hub 5555555555555555'
Move-MoyaiHubAutostartExecutable $fakeKey | Out-Null
Assert-That ($fakeKey.writes.Count -eq 0 -and $fakeKey.items[$fakeKey.race_name].value -ceq 'changed concurrently') 'autostart retarget rereads and preserves a concurrent user change'
[IO.File]::AppendAllText($oldStandaloneExe,' changed after release')
$fakeKey.items=@{'moyAI Hub 6666666666666666'=@{kind=[Microsoft.Win32.RegistryValueKind]::String;value=('"'+$oldStandaloneExe+'" --launch --no-browser --data-dir "C:\modified"')}}
$fakeKey.reads=@{};$fakeKey.writes.Clear();$fakeKey.race_name=''
Move-MoyaiHubAutostartExecutable $fakeKey | Out-Null
Assert-That ($fakeKey.writes.Count -eq 0) 'changed old executable cannot be adopted just because its name matches Hub'
Assert-That (-not(Read-MoyaiHubRunEntry 'moyAI Hub 8888888888888888' '"C:\bad|path\moyai-hub.exe" --launch --no-browser --data-dir "C:\profile"' 'String').valid) 'malformed executable path is retained as an unknown command without aborting setup'
Assert-That (-not(Read-MoyaiHubRunEntry 'moyAI Hub 9999999999999999' '"C:\Hub\moyai-hub.exe" --launch --no-browser --data-dir "relative-profile"' 'String').valid) 'relative custom startup profile is not adopted as a generated Hub registration'
function Select-MoyaiUninstallOptions { [pscustomobject]@{components=@();reset_desktop=$false;reset_hub=$false;remove_shared=$true} }
$script:sharedOnlyArguments=''
function Start-Process($FilePath,$ArgumentList) {
    $script:sharedOnlyArguments=$ArgumentList
    $snapshot=[regex]::Match($ArgumentList,' -SourceRoot "([^"]+)"').Groups[1].Value
    $expected=[regex]::Match($ArgumentList,' -ExpectedManifestSha256 ([a-f0-9]{64})').Groups[1].Value
    $operation=[regex]::Match($ArgumentList,' -OperationId ([a-f0-9]{32})').Groups[1].Value
    $componentText=[regex]::Match($ArgumentList,' -Components "([^"]*)"').Groups[1].Value
    $selected=@($componentText.Split(',') | Where-Object {-not[string]::IsNullOrWhiteSpace($_)})
    $messages=@(Invoke-MoyaiMachineComponent $snapshot $selected -Uninstall -RemoveSharedRuntime -ExpectedManifestSha256 $expected)
    Write-MoyaiComponentResult -OperationId $operation -ManifestSha256 $expected -Components $selected -Success $true -ExitCode 0 -Message 'shared-only fixture' -Messages $messages
    [pscustomobject]@{ExitCode=0}
}
$script:serviceFixture=[pscustomobject]@{State=1}
$beforeSharedOnly=(Get-FileHash -LiteralPath $newHub).Hash
Invoke-MoyaiComponentUserSetup -SourceRoot $package -Uninstall -Interactive
Assert-That ($null -eq $script:serviceFixture -and (Get-FileHash -LiteralPath $newHub).Hash -ceq $beforeSharedOnly) 'interactive shared-service-only selection removes the service while preserving application payload'
Assert-That ($script:sharedOnlyArguments.Contains('-Components ""') -and $script:sharedOnlyArguments.Contains('-RemoveSharedRuntime')) 'empty selected component set is passed as one safely quoted native argument'
$emptyOperation=[Guid]::NewGuid().ToString('N')
Write-MoyaiComponentResult -OperationId $emptyOperation -ManifestSha256 ('7'*64) -Components @() -Success $true -ExitCode 0 -Message 'shared only'
$emptyResult=Read-MoyaiComponentResult $emptyOperation ('7'*64) @() 0
Assert-That ($emptyResult.components -is [array] -and $emptyResult.components.Count -eq 0) 'shared-service-only result preserves and validates the empty component set'
Assert-Fails { Invoke-MoyaiMachineComponent $package @() -Uninstall -ExpectedManifestSha256 $manifest.validated_manifest_sha256 } 'empty component selection without shared removal is rejected'
$argumentFixture=Join-Path $EvidenceRoot 'native arguments'
[void](New-Item -ItemType Directory -Path $argumentFixture -Force)
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'deployment/Setup-Component.ps1') -Destination (Join-Path $argumentFixture 'Setup-Component.ps1')
[IO.File]::WriteAllText((Join-Path $argumentFixture 'component-install.ps1'),@'
function Invoke-MoyaiMachineComponent($SourceRoot,[string[]]$Components,[switch]$Uninstall,[switch]$RemoveSharedRuntime,$ExpectedManifestSha256) {
 [IO.File]::WriteAllText((Join-Path $SourceRoot 'received.json'),(@{count=@($Components).Count;uninstall=[bool]$Uninstall;shared=[bool]$RemoveSharedRuntime}|ConvertTo-Json))
}
function Write-MoyaiComponentResult { }
'@,[Text.UTF8Encoding]::new($true))
$nativeArguments='-NoProfile -NonInteractive -ExecutionPolicy Bypass -File '+(ConvertTo-MoyaiSetupArgument (Join-Path $argumentFixture 'Setup-Component.ps1'))+' -SourceRoot '+(ConvertTo-MoyaiSetupArgument $argumentFixture)+' -Components "" -ExpectedManifestSha256 '+('8'*64)+' -OperationId '+([Guid]::NewGuid().ToString('N'))+' -Uninstall -RemoveSharedRuntime'
$argumentProcess=Microsoft.PowerShell.Management\Start-Process -FilePath (Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe') -ArgumentList $nativeArguments -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $argumentFixture 'stdout.txt') -RedirectStandardError (Join-Path $argumentFixture 'stderr.txt')
try {
    $null=$argumentProcess.Handle
    if(-not $argumentProcess.WaitForExit(5000)) { Stop-Process -Id $argumentProcess.Id; throw 'Inert native argument fixture did not terminate.' }
    Assert-That ($argumentProcess.ExitCode -eq 0) 'Windows PowerShell 5.1 binds the safely quoted empty component argument without prompting'
} finally { $argumentProcess.Dispose() }
$received=Get-Content -LiteralPath (Join-Path $argumentFixture 'received.json') -Raw -Encoding UTF8 | ConvertFrom-Json
Assert-That ($received.count -eq 0 -and $received.uninstall -and $received.shared) 'actual elevated entry parser forwards an empty set only with the explicit shared uninstall switches'
Write-MoyaiComponentJson (Join-Path $EvidenceRoot 'results.json') ([ordered]@{passed=$true;checks=@($checks);count=$checks.Count;fixture_root=$EvidenceRoot})
Write-Output "PASS: $($checks.Count) component deployment checks. Evidence: $EvidenceRoot"
