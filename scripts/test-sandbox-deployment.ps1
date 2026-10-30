param([Parameter(Mandatory=$true)][string]$EvidenceRoot)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'deployment/common.ps1')
$EvidenceRoot = [IO.Path]::GetFullPath($EvidenceRoot)
if (Test-Path -LiteralPath $EvidenceRoot) { throw 'Use a new empty evidence directory.' }
[void](New-Item -ItemType Directory -Path $EvidenceRoot)
$checks = [Collections.Generic.List[string]]::new()
function Assert-That([bool]$Condition,[string]$Message) { if (-not $Condition) { throw "FAIL: $Message" }; $checks.Add($Message) }
function Assert-Fails([scriptblock]$Action,[string]$Message) { $failed=$false; try { & $Action | Out-Null } catch { $failed=$true }; Assert-That $failed $Message }

# Compile declarations without invoking any Windows service API.
Initialize-MoyaiSandboxNative
Assert-That ($null -ne ('MoyaiSandboxSetup.Native' -as [type])) 'native SCM interop declarations compile without invoking SCM'
$script:realProtection=${function:Set-MoyaiSandboxProtection}
$script:realElevation=${function:Invoke-MoyaiSandboxElevation}

# Only filesystem fixtures under EvidenceRoot are used. All SCM/elevation/ACL mutation
# boundaries are replaced before a product operation is called; no executable runs.
$script:caseNumber=0
function New-Case {
    $script:caseNumber++
    $script:caseRoot=Join-Path $EvidenceRoot ([string]$script:caseNumber)
    $script:machine=Join-Path $script:caseRoot 'machine'
    $script:source=Join-Path $script:caseRoot 'package'
    [void](New-Item -ItemType Directory -Path (Join-Path $script:source 'app/bin'),(Join-Path $script:source 'scripts'))
    foreach ($name in @('common.ps1','runtime-access.ps1','sandbox-service.ps1','Setup-Sandbox.ps1','Start-moyAI.ps1','Setup-moyAI.ps1')) { Copy-Item -LiteralPath (Join-Path $PSScriptRoot "deployment/$name") -Destination (Join-Path $script:source "scripts/$name") }
    foreach ($name in @('moyai-desktop.exe','moyai-runner.exe','moyai-sandbox.exe')) { Write-MoyaiUtf8 (Join-Path $script:source "app/bin/$name") ('inert fixture ' + $name) }
    $script:service=$null; $script:calls=[Collections.Generic.List[string]]::new(); $script:failEnable=$false; $script:failDelete=$false; $script:raceStart=$false; $script:elevationCount=0; $script:protections=@{}; $script:corruptCopy=$false
    Write-FixtureManifest 'system-installed'
}
function Write-FixtureManifest([string]$Runtime) {
    $files=@(Get-ChildItem -LiteralPath $script:source -Recurse -File | Where-Object { $_.Name -ne 'deployment.json' } | Sort-Object FullName | ForEach-Object {
        [ordered]@{path=$_.FullName.Substring($script:source.Length+1).Replace('\','/');sha256=(Get-FileHash -LiteralPath $_.FullName).Hash.ToLowerInvariant()}
    })
    $script:manifest=[pscustomobject]@{schema=1;product='moyai-windows-user';target='windows-x86_64';version='fixture';hub=$null;machine_sandbox=[pscustomobject]@{service='MoyAISandbox';setup='scripts/Setup-Sandbox.ps1'};runtime=[pscustomobject]@{visual_cpp=$Runtime;webview2='system-installed'};files=$files}
    Write-MoyaiUtf8 (Join-Path $script:source 'deployment.json') ($script:manifest | ConvertTo-Json -Depth 6)
    $script:manifest=Read-MoyaiDeployment $script:source -VerifyHashes
}
function Get-MoyaiSandboxLocation { [pscustomobject]@{parent=$script:machine;root=(Join-Path $script:machine 'SandboxService');helper=(Join-Path $script:machine 'SandboxService/moyai-sandbox.exe');service='MoyAISandbox'} }
function Assert-MoyaiSandboxAdministrator { $script:calls.Add('administrator-checked') }
function Assert-MoyaiSandboxProtectedPath([string]$Path) {
    Assert-MoyaiSandboxRegularPath $Path
    if (-not $Path.StartsWith($script:machine,[StringComparison]::OrdinalIgnoreCase) -or -not (Test-Path -LiteralPath $Path)) { throw 'Fixture path is outside the isolated machine or missing.' }
}
function Set-MoyaiSandboxProtection([string]$Path,[bool]$Directory,[switch]$Private) { Assert-MoyaiSandboxProtectedPath $Path; $script:protections[$Path]=[bool]$Private }
function Set-MoyaiSandboxParentAccess([string]$Path) { Assert-MoyaiSandboxProtectedPath $Path }
function Set-Acl([string]$LiteralPath,$AclObject) { $script:capturedAcl=$AclObject }
function Copy-MoyaiSandboxPinnedPayload($Stream,[string]$Target) {
    $Stream.Position=0; $output=[IO.File]::Open($Target,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
    try { $Stream.CopyTo($output) } finally { $output.Dispose(); $Stream.Position=0 }
    if ($script:corruptCopy) { [IO.File]::AppendAllText($Target,'injected mismatch'); $script:corruptCopy=$false }
}
function Get-MoyaiSandboxService([switch]$Security) { return $script:service }
function Set-MoyaiSandboxService([string]$Command,[uint32]$Start,[switch]$Create) {
    $script:calls.Add("configure:$Start")
    if ($Start -eq 3 -and $script:failEnable) { $script:failEnable=$false; throw 'Injected change-config failure.' }
    if ($Create) { if ($null -ne $script:service) { throw 'Service already exists.' }; $script:service=[pscustomobject]@{Type=16;Start=$Start;ErrorControl=1;State=1;Command=$Command;Account='LocalSystem';Dependencies='';Sddl='D:'} }
    else { if ($null -eq $script:service) { throw 'Missing service.' }; $script:service.Start=$Start; $script:service.Command=$Command }
    if ($Start -eq 4 -and $script:raceStart) { $script:service.State=4; $script:raceStart=$false }
}
function Set-MoyaiSandboxServiceSecurity([string]$Sddl) { $script:calls.Add('security'); $script:service.Sddl=$Sddl }
function Remove-MoyaiSandboxService { $script:calls.Add('delete'); if ($script:failDelete) { $script:failDelete=$false; throw 'Injected delete failure.' }; $script:service=$null }
function Invoke-MoyaiSandboxElevation([string]$SourceRoot,$Manifest) { $script:elevationCount++; throw 'Simulated user cancellation; no process was started.' }

New-Case
Assert-That ((ConvertTo-MoyaiSetupArgument 'C:\配布 & $value\') -ceq '"C:\配布 & $value\\"') 'native setup argument quoting preserves literal shell characters and trailing slashes'
Assert-Fails { ConvertTo-MoyaiSetupArgument ('C:\bad'+[char]0+'path') } 'NUL in a setup argument is rejected'
Assert-Fails { ConvertTo-MoyaiSetupArgument 'C:\bad"path' } 'embedded quotes cannot alter setup arguments'
$descriptor=[Security.AccessControl.RawSecurityDescriptor]::new((Get-MoyaiSandboxServiceSddl))
$interactive=@($descriptor.DiscretionaryAcl | Where-Object { $_.SecurityIdentifier.Value -ceq 'S-1-5-4' })
Assert-That ($interactive.Count -eq 1 -and $interactive[0].AccessMask -eq 0x15 -and $descriptor.DiscretionaryAcl.Count -eq 3) 'service DACL grants IU only query config, query status and start'
$beforeCalls=$script:calls.Count
Ensure-MoyaiSandboxInstallation $script:source $script:manifest -CheckOnly
Assert-That ($script:elevationCount -eq 0 -and $script:calls.Count -eq $beforeCalls -and -not (Test-Path -LiteralPath $script:machine)) 'check-only reports needed installation without elevation or machine writes'
Assert-Fails { Ensure-MoyaiSandboxInstallation $script:source $script:manifest } 'cancelled elevation rejects user setup'
Assert-That ($script:elevationCount -eq 1 -and -not (Test-Path -LiteralPath $script:machine)) 'cancellation leaves machine payload absent'
$legacyDirectory=Join-Path $script:machine 'Sandbox/previous-transient-owner'
[void](New-Item -ItemType Directory -Path $legacyDirectory)
$legacySentinel=Join-Path $legacyDirectory 'legacy-evidence.txt'
Write-MoyaiUtf8 $legacySentinel 'previous transient helper evidence'
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
$location=Get-MoyaiSandboxLocation
Assert-That ($script:service.Start -eq 3 -and $script:service.State -eq 1 -and $script:service.Command -ceq ('"'+$location.helper+'" --service')) 'install registers a stopped fixed demand-start LocalSystem helper'
Assert-That (($script:calls -join ',') -ceq 'administrator-checked,configure:4,security,configure:3') 'initial install is disabled until protected payload and DACL are verified'
& $script:realProtection $location.root $true
$directoryDescriptor=[Security.AccessControl.RawSecurityDescriptor]::new($script:capturedAcl.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::All))
$fileInteractive=@($directoryDescriptor.DiscretionaryAcl | Where-Object { $_.SecurityIdentifier.Value -ceq 'S-1-5-4' })
Assert-That ($directoryDescriptor.Owner.Value -ceq 'S-1-5-32-544' -and $directoryDescriptor.DiscretionaryAcl.Count -eq 3 -and $fileInteractive.Count -eq 1 -and ($fileInteractive[0].AccessMask -band 0x500d0156) -eq 0 -and ([int]$fileInteractive[0].AceFlags -band 3) -eq 3) 'actual directory descriptor gives BA ownership and inheritable read/execute-only access to IU'
& $script:realProtection $location.root $true -Private
$privateDescriptor=[Security.AccessControl.RawSecurityDescriptor]::new($script:capturedAcl.GetSecurityDescriptorSddlForm([Security.AccessControl.AccessControlSections]::All))
Assert-That ($privateDescriptor.DiscretionaryAcl.Count -eq 2 -and @($privateDescriptor.DiscretionaryAcl | Where-Object { $_.SecurityIdentifier.Value -notin @('S-1-5-18','S-1-5-32-544') }).Count -eq 0) 'actual private staging descriptor grants access only to administrators and SYSTEM'
$script:service.Sddl='O:BAG:BAD:P(A;;0xf01ff;;;SY)(A;;0xf01ff;;;BA)(A;;0x15;;;IU)'
Assert-MoyaiSandboxService $script:service $location -Security
Assert-That $true 'SCM-expanded SERVICE_ALL_ACCESS has the same protected authority'
$script:service.Sddl=Get-MoyaiSandboxServiceSddl
$beforeCalls=$script:calls.Count; $beforeElevations=$script:elevationCount
Ensure-MoyaiSandboxInstallation $script:source $script:manifest
Assert-That ($script:calls.Count -eq $beforeCalls -and $script:elevationCount -eq $beforeElevations) 'identical installed payload needs no UAC or service changes'
$script:service.State=4
Ensure-MoyaiSandboxInstallation $script:source $script:manifest
Write-MoyaiUtf8 (Join-Path $script:source 'app/bin/moyai-sandbox.exe') 'second inert helper'
Write-FixtureManifest 'system-installed'
$oldHash=(Get-FileHash -LiteralPath $location.helper).Hash
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source } 'running service prevents update'
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall } 'running service prevents machine uninstall'
Assert-That ((Get-FileHash -LiteralPath $location.helper).Hash -ceq $oldHash -and $script:service.State -eq 4) 'running-service rejection preserves payload and does not stop the process'
$script:service.State=1; $script:raceStart=$true
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source } 'a start racing setup is rejected after disabling new starts'
Assert-That ((Get-FileHash -LiteralPath $location.helper).Hash -ceq $oldHash -and $script:service.State -eq 4 -and $script:service.Start -eq 3) 'racing start retains running process and old payload, and restores demand-start configuration'
$script:service.State=1; $script:failEnable=$true
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source } 'injected final service configuration failure propagates'
Assert-That ((Get-FileHash -LiteralPath $location.helper).Hash -ceq $oldHash -and $script:service.Start -eq 3) 'failed update restores old payload and demand-start configuration together'
Assert-That (Test-MoyaiSandboxPayloadMatch (Read-MoyaiSandboxInstallation $location) @([pscustomobject]@{name='moyai-sandbox.exe';sha256=$oldHash.ToLowerInvariant()}) 'system-installed') 'rollback receipt still identifies the restored payload'
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
Assert-That ((Get-FileHash -LiteralPath $location.helper).Hash -cne $oldHash -and $script:service.Start -eq 3) 'successful update replaces helper and returns to demand start'
$script:failDelete=$true
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall } 'failed service deletion is reported'
Assert-That ((Test-Path -LiteralPath $location.helper) -and $script:service.Start -eq 3) 'failed deletion restores payload and original service configuration'
$sentinel=Join-Path $script:caseRoot 'user-data.txt'; Write-MoyaiUtf8 $sentinel 'profile/history/device policy sentinel'
Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall | Out-Null
Assert-That ($null -eq $script:service -and -not (Test-Path -LiteralPath $location.root) -and (Get-Content -LiteralPath $sentinel -Raw) -ceq 'profile/history/device policy sentinel') 'explicit machine removal deletes only owned payload and service'
Assert-That ((Get-Content -LiteralPath $legacySentinel -Raw) -ceq 'previous transient helper evidence') 'install, update and explicit removal preserve the legacy Sandbox directory'

foreach($damage in @('helper','directory','parent','service')) {
    New-Case
    Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
    $location=Get-MoyaiSandboxLocation
    if($damage -eq 'service') { $script:service=$null }
    else {
        Remove-Item -LiteralPath $location.helper
        if($damage -in @('directory','parent')) {
            Remove-Item -LiteralPath (Join-Path $location.root 'installation.json')
            Remove-Item -LiteralPath $location.root
            if($damage -eq 'parent') {
                Remove-Item -LiteralPath (Join-Path $location.parent 'sandbox-setup.lock')
                Remove-Item -LiteralPath $location.parent
            }
            $script:failDelete=$true
            Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall } 'missing-directory service deletion failure is reported'
            Assert-That ($null -ne $script:service -and $script:service.Start -eq 3) 'failed deletion restores demand-start even when the payload directory was already absent'
        }
    }
    Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall | Out-Null
    Assert-That ($null -eq $script:service -and -not(Test-Path -LiteralPath $location.root)) "uninstall completes with an already missing $damage"
    Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall | Out-Null
    Assert-That ($null -eq $script:service -and -not(Test-Path -LiteralPath $location.root)) "repeated uninstall after missing $damage is a successful no-op"
}

New-Case
foreach($name in @('vcruntime140.dll','vcruntime140_1.dll','msvcp140.dll')) { Write-MoyaiUtf8 (Join-Path $script:source "app/bin/$name") ('inert '+$name) }
Write-FixtureManifest 'app-local'
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
$location=Get-MoyaiSandboxLocation
Assert-That (@((Read-MoyaiSandboxInstallation $location).files).Count -eq 4) 'bundled mode installs the complete inventoried runtime with the helper'
Write-MoyaiUtf8 (Join-Path $script:source 'app/bin/foreign.dll') 'listed but outside the fixed helper runtime set'
Write-FixtureManifest 'app-local'
$beforeCalls=$script:calls.Count
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source } 'a fourth DLL is rejected even when the package manifest lists it'
Assert-That ($script:calls.Count -eq ($beforeCalls+1)) 'listed extra DLL rejection performs only the administrator check and no SCM mutation'
Remove-Item -LiteralPath (Join-Path $script:source 'app/bin/foreign.dll')
Write-FixtureManifest 'app-local'
$receiptPath=Join-Path $location.root 'installation.json'
$savedReceipt=[IO.File]::ReadAllBytes($receiptPath)
$extraInstalled=Join-Path $location.root 'foreign.dll'
Write-MoyaiUtf8 $extraInstalled 'listed but outside the installed helper runtime set'
$receipt=[Text.Encoding]::UTF8.GetString($savedReceipt) | ConvertFrom-Json
$receipt.files=@($receipt.files)+@([pscustomobject]@{name='foreign.dll';sha256=(Get-FileHash -LiteralPath $extraInstalled).Hash.ToLowerInvariant()})
Write-MoyaiUtf8 $receiptPath ($receipt | ConvertTo-Json -Depth 5)
Assert-Fails { Read-MoyaiSandboxInstallation $location } 'the installed protected inventory cannot widen the fixed three-DLL runtime set'
[IO.File]::WriteAllBytes($receiptPath,$savedReceipt)
Remove-Item -LiteralPath $extraInstalled
Remove-Item -LiteralPath (Join-Path $script:source 'app/bin/msvcp140.dll'); Write-FixtureManifest 'app-local'
Assert-Fails { Get-MoyaiSandboxInstallPlan $script:source $script:manifest } 'incomplete bundled runtime is rejected before any service mutation'
Write-MoyaiUtf8 (Join-Path $location.root 'unexpected.dll') 'unowned'
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall } 'machine uninstall refuses unowned files rather than recursively deleting them'

New-Case
Write-MoyaiUtf8 (Join-Path $script:source 'app/bin/foreign.dll') 'not inventoried'
Assert-Fails { Get-MoyaiSandboxInstallPlan $script:source $script:manifest } 'unlisted source DLLs are rejected'
Remove-Item -LiteralPath (Join-Path $script:source 'app/bin/foreign.dll')
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
$location=Get-MoyaiSandboxLocation
$script:service.Command='"C:\unrelated.exe" --service'
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall } 'a same-name service with another image path is not adopted or removed'
Assert-That ((Test-Path -LiteralPath $location.helper) -and $script:service.Command -ceq '"C:\unrelated.exe" --service') 'foreign service conflict leaves both registrations and files unchanged'
$script:service.Command='"'+$location.helper+'" --service'
$script:service.Sddl='O:BAG:BAD:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;IU)'
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall } 'unexpected service authority fails closed before removal'
$script:service.Sddl='O:IUG:BAD:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x15;;;IU)'
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall } 'non-administrator service ownership is rejected despite a restricted DACL'
$script:service.Sddl=Get-MoyaiSandboxServiceSddl
$receiptPath=Join-Path $location.root 'installation.json'
$receipt=Get-Content -LiteralPath $receiptPath -Raw | ConvertFrom-Json
$receipt.files[0].name='../../elsewhere.exe'
Write-MoyaiUtf8 $receiptPath ($receipt | ConvertTo-Json -Depth 5)
Assert-Fails { Read-MoyaiSandboxInstallation $location } 'receipt paths cannot select an executable or deletion target outside the fixed directory'

New-Case
$manifestHash=(Get-FileHash -LiteralPath (Join-Path $script:source 'deployment.json')).Hash.ToLowerInvariant()
$script:manifest.version='changed-during-approval'
Write-MoyaiUtf8 (Join-Path $script:source 'deployment.json') ($script:manifest | ConvertTo-Json -Depth 6)
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source -ExpectedManifestSha256 $manifestHash } 'changed inventory after UAC request is rejected before creating protected directories'
Assert-That (-not (Test-Path -LiteralPath $script:machine)) 'approval-bound inventory failure leaves machine state absent'
$legacy=$script:manifest | ConvertTo-Json -Depth 6 | ConvertFrom-Json
$legacy.PSObject.Properties.Remove('machine_sandbox')
$legacy.files=@($legacy.files | Where-Object { $_.path -notin @('scripts/sandbox-service.ps1','scripts/Setup-Sandbox.ps1') })
Write-MoyaiUtf8 (Join-Path $script:source 'deployment.json') ($legacy | ConvertTo-Json -Depth 6)
Assert-That ($null -ne (Read-MoyaiDeployment $script:source -AllowMissingFiles)) 'previous per-user package inventories remain readable for update and uninstall'
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source } 'old per-user packages cannot register an incompatible helper as the fixed service'
$target=Join-Path $script:caseRoot 'ordinary'; [void](New-Item -ItemType Directory -Path $target)
$junction=Join-Path $script:caseRoot 'junction'; [void](New-Item -ItemType Junction -Path $junction -Target $target)
Assert-Fails { Assert-MoyaiSandboxRegularPath (Join-Path $junction 'future-file') } 'reparse ancestors are rejected even when the final payload does not yet exist'

New-Case
$script:corruptCopy=$true
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source } 'copied payload hash mismatch rejects installation'
$stages=@(Get-ChildItem -LiteralPath $script:machine -Directory | Where-Object { $_.Name.StartsWith('SandboxService.stage-',[StringComparison]::Ordinal) })
Assert-That ($stages.Count -eq 1 -and $script:protections[$stages[0].FullName] -eq $true -and $script:protections[(Join-Path $stages[0].FullName 'moyai-sandbox.exe')] -eq $true -and $null -eq $script:service) 'failed elevated copy never publishes stage or payload with interactive-user read access (mocked ACL boundary)'

New-Case
foreach($name in @('vcruntime140.dll','vcruntime140_1.dll','msvcp140.dll')) { Write-MoyaiUtf8 (Join-Path $script:source "app/bin/$name") ('inert '+$name) }
Write-FixtureManifest 'app-local'
$helperBefore=Join-Path $script:source 'app/bin/moyai-sandbox.exe'
$writable=[IO.File]::Open($helperBefore,[IO.FileMode]::Open,[IO.FileAccess]::Write,[IO.FileShare]::ReadWrite); $writable.Dispose()
[IO.Directory]::Move($script:source,($script:source+'-control')); [IO.Directory]::Move(($script:source+'-control'),$script:source)
Assert-That $true 'without pins the same source can be opened for writing and its ancestor renamed'
$pins=New-MoyaiSandboxSourcePins $script:source $script:manifest
try {
    $ordinaryChild=Join-Path $script:source 'ordinary-child.tmp'
    [IO.File]::WriteAllText($ordinaryChild,'normal child creation remains available')
    Assert-That ([IO.File]::ReadAllText($ordinaryChild) -ceq 'normal child creation remains available') 'directory pins allow ordinary child creation while denying ancestor replacement'
    [IO.File]::Delete($ordinaryChild)
    foreach($relative in @('deployment.json','scripts/Setup-Sandbox.ps1','scripts/common.ps1','scripts/runtime-access.ps1','scripts/sandbox-service.ps1','app/bin/moyai-sandbox.exe','app/bin/vcruntime140.dll','app/bin/vcruntime140_1.dll','app/bin/msvcp140.dll')) {
        $path=Join-Path $script:source $relative
        Assert-Fails { $file=[IO.File]::Open($path,[IO.FileMode]::Open,[IO.FileAccess]::Write,[IO.FileShare]::ReadWrite); $file.Dispose() } "pinned $relative refuses a concurrent write"
        Assert-Fails { [IO.File]::Move($path,($path+'.moved')) } "pinned $relative refuses replacement by rename"
    }
    Assert-Fails { [IO.Directory]::Move((Join-Path $script:source 'scripts'),(Join-Path $script:source 'scripts-moved')) } 'pinned script ancestor refuses rename during the UAC-equivalent wait'
    Assert-Fails { [IO.Directory]::Move($script:source,($script:source+'-moved')) } 'pinned package ancestor refuses replacement during the UAC-equivalent wait'
} finally { Close-MoyaiSandboxSourcePins $pins }
$helper=Join-Path $script:source 'app/bin/moyai-sandbox.exe'
$stream=[IO.File]::Open($helper,[IO.FileMode]::Open,[IO.FileAccess]::Write,[IO.FileShare]::ReadWrite); $stream.Dispose()
[IO.File]::Move($helper,($helper+'.moved')); [IO.File]::Move(($helper+'.moved'),$helper)
[IO.Directory]::Move($script:source,($script:source+'-moved')); [IO.Directory]::Move(($script:source+'-moved'),$script:source)
Assert-That $true 'disposing source pins releases normal file-write and ancestor-rename operations'
$scriptPath=Join-Path $script:source 'scripts/Setup-Sandbox.ps1'
[IO.File]::AppendAllText($scriptPath,'changed after first manifest validation')
Assert-Fails { New-MoyaiSandboxSourcePins $script:source $script:manifest } 'a setup script changed before pin acquisition is rejected using the initial manifest hash inventory'

New-Case
function Start-Process([string]$FilePath,[string]$ArgumentList,[string]$Verb,[string]$WindowStyle,[switch]$PassThru,[switch]$Wait) {
    $script:elevationStubCalls++
    $script:pinnedAtStart=$false
    try { $stream=[IO.File]::Open((Join-Path $script:source 'deployment.json'),[IO.FileMode]::Open,[IO.FileAccess]::Write,[IO.FileShare]::ReadWrite); $stream.Dispose() }
    catch { $script:pinnedAtStart=$true }
    $native=[ComponentModel.Win32Exception]::new($script:elevationFailureCode)
    $script:expectedElevationMessage=$native.Message
    if($script:elevationFailureCode -eq 5){throw [InvalidOperationException]::new('Inert launch wrapper failed.',$native)}
    throw $native
}
$script:elevationStubCalls=0
foreach($code in @(1223,5)) {
    $script:elevationFailureCode=$code; $diagnostic=''
    try { & $script:realElevation $script:source $script:manifest } catch { $diagnostic=$_.Exception.Message }
    Assert-That ($diagnostic.Contains($script:expectedElevationMessage) -and $diagnostic.Contains("NativeErrorCode=$code")) "elevation failure retains the Windows reason and native code $code"
    if($code -eq 5){Assert-That ($diagnostic.Contains('Inert launch wrapper failed.')) 'wrapped native failure retains both wrapper and base-exception details'}
    Assert-That $script:pinnedAtStart "source inventory remains pinned while the inert launch stub fails with $code"
    $stream=[IO.File]::Open((Join-Path $script:source 'deployment.json'),[IO.FileMode]::Open,[IO.FileAccess]::Write,[IO.FileShare]::ReadWrite); $stream.Dispose()
    [IO.Directory]::Move($script:source,($script:source+'-released')); [IO.Directory]::Move(($script:source+'-released'),$script:source)
    Assert-That $true "elevation failure $code disposes file and ancestor pins"
}
Assert-That ($script:elevationStubCalls -eq 2 -and -not (Test-Path -LiteralPath $script:machine)) 'diagnostic fixtures use two inert launch calls and never begin machine installation'

$result=[ordered]@{passed=$checks.Count;checks=$checks;scope='Inert task-local files and mocked SCM/elevation/ACL mutation only. No machine service, UAC, executable, registry, user profile or Device policy was changed.'}
Write-MoyaiUtf8 (Join-Path $EvidenceRoot 'results.json') ($result | ConvertTo-Json -Depth 4)
$result | ConvertTo-Json -Depth 4
