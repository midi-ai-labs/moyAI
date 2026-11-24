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
# Restart Manager is read-only; this checks the real query on a task-local file.
$heldPath=Join-Path $EvidenceRoot 'held-by-this-test.exe'; [IO.File]::WriteAllText($heldPath,'inert')
$held=[IO.File]::Open($heldPath,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
try { $reported=@([MoyaiSandboxSetup.Native]::FileHolders(@($heldPath))) } finally { $held.Dispose() }
Assert-That (@($reported | Where-Object { $_.StartsWith("$PID|") }).Count -eq 1 -and @([MoyaiSandboxSetup.Native]::FileHolders(@($heldPath))).Count -eq 0) 'Restart Manager names the process that holds a helper file and nothing after release'
$script:realProtection=${function:Set-MoyaiSandboxProtection}
$script:realElevation=${function:Invoke-MoyaiSandboxElevation}

# Only filesystem fixtures under EvidenceRoot are used. All SCM/elevation/ACL mutation
# boundaries are replaced before a product operation is called; no executable runs.
$script:caseNumber=0
function New-Case {
    $script:markedForDeletion=$false; $script:keepDeletePending=$false; $script:pendingReadable=$false; $script:pendingService=$null
    $script:holders=@(); $script:payloadRemovalFailures=0; $script:pendingClearsOnStop=$false
    $script:caseNumber++
    $script:caseRoot=Join-Path $EvidenceRoot ([string]$script:caseNumber)
    $script:machine=Join-Path $script:caseRoot 'machine'
    $script:source=Join-Path $script:caseRoot 'package'
    [void](New-Item -ItemType Directory -Path (Join-Path $script:source 'app/bin'),(Join-Path $script:source 'scripts'))
    foreach ($name in @('common.ps1','runtime-access.ps1','sandbox-service.ps1','Setup-Sandbox.ps1','Start-moyAI.ps1','Setup-moyAI.ps1')) { Copy-Item -LiteralPath (Join-Path $PSScriptRoot "deployment/$name") -Destination (Join-Path $script:source "scripts/$name") }
    foreach ($name in @('moyai-desktop.exe','moyai-runner.exe','moyai-sandbox.exe')) { Write-MoyaiUtf8 (Join-Path $script:source "app/bin/$name") ('inert fixture ' + $name) }
    $script:service=$null; $script:calls=[Collections.Generic.List[string]]::new(); $script:failEnable=$false; $script:failDelete=$false; $script:raceStart=$false; $script:elevationCount=0; $script:protections=@{}; $script:corruptCopy=$false
    $script:clientRunning=$false; $script:failStop=$false
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
function Get-MoyaiComponentLocation([string]$Component) { [pscustomobject]@{parent=$script:machine;root=(Join-Path $script:machine $Component);component=$Component} }
function Assert-MoyaiComponentStopped([string]$Root) { Assert-MoyaiSandboxRegularPath $Root }
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
function Get-MoyaiSandboxService([switch]$Security) {
    if($script:markedForDeletion -and -not $script:pendingReadable) { throw [ComponentModel.Win32Exception]::new(1072) }
    if($script:markedForDeletion) { return $script:pendingService }
    return $script:service
}
function Test-MoyaiSandboxServiceDeletePending { [bool]$script:markedForDeletion }
function Get-MoyaiSandboxRemovalWaitMilliseconds { 300 }
function Get-MoyaiSandboxFileRetryMilliseconds { 400 }
function Get-MoyaiSandboxProcessWaitMilliseconds { 8000 }
function Get-MoyaiSandboxFileHolders([string[]]$Paths) { if(@($Paths).Count){ @($script:holders) } }
function Get-MoyaiServicesReferencingPath([string]$Path) { @() }
$script:realPayloadRemoval=${function:Remove-MoyaiSandboxPayloadDirectory}
function Remove-MoyaiSandboxPayloadDirectory([string]$Path,$Files) {
    if($script:payloadRemovalFailures -gt 0) { $script:payloadRemovalFailures--; throw [IO.IOException]::new('Injected sharing violation.',[int]0x80070020) }
    & $script:realPayloadRemoval $Path $Files
}
function Set-MoyaiSandboxService([string]$Command,[uint32]$Start,[switch]$Create) {
    $script:calls.Add("configure:$Start")
    if ($script:markedForDeletion) { throw [ComponentModel.Win32Exception]::new(1072) }
    if ($Start -eq 3 -and $script:failEnable) { $script:failEnable=$false; throw 'Injected change-config failure.' }
    if ($Create) { if ($null -ne $script:service) { throw 'Service already exists.' }; $script:service=[pscustomobject]@{Type=16;Start=$Start;ErrorControl=1;State=1;Command=$Command;Account='LocalSystem';Dependencies='';Sddl='D:'} }
    else { if ($null -eq $script:service) { throw 'Missing service.' }; $script:service.Start=$Start; $script:service.Command=$Command }
    if ($Start -eq 4 -and $script:raceStart) { $script:service.State=4; $script:raceStart=$false }
}
function Set-MoyaiSandboxServiceSecurity([string]$Sddl) { $script:calls.Add('security'); $script:service.Sddl=$Sddl }
function Remove-MoyaiSandboxService {
    $script:calls.Add('delete')
    if ($script:failDelete) { $script:failDelete=$false; throw 'Injected delete failure.' }
    if ($script:keepDeletePending) { $script:markedForDeletion=$true; $script:pendingService=$script:service; $script:service=$null }
    else { $script:service=$null }
}
function Assert-MoyaiSandboxClientsStopped { if($script:clientRunning){throw 'A client still owns work.'} }
function Stop-MoyaiSandboxService {
    $script:calls.Add('stop'); if($script:failStop){throw 'Stop timed out.'}
    if($script:markedForDeletion) { $script:pendingService.State=1; if($script:pendingClearsOnStop) { $script:markedForDeletion=$false; $script:pendingService=$null } ; return }
    $script:service.State=1
}
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
$script:clientRunning=$true
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source } 'a live client prevents update'
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall } 'a live client prevents machine uninstall'
Assert-That ((Get-FileHash -LiteralPath $location.helper).Hash -ceq $oldHash -and $script:service.State -eq 4) 'live-client rejection preserves payload and does not stop work'
$script:clientRunning=$false; $script:failStop=$true
Assert-Fails { Invoke-MoyaiMachineSandboxSetup $script:source } 'stop timeout prevents payload replacement'
Assert-That ((Get-FileHash -LiteralPath $location.helper).Hash -ceq $oldHash -and $script:service.Start -eq 3) 'stop timeout preserves old files and restores start mode'
$script:failStop=$false
$script:service.State=1; $script:raceStart=$true
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
Assert-That ((Get-FileHash -LiteralPath $location.helper).Hash -cne $oldHash -and $script:service.State -eq 1 -and $script:service.Start -eq 3) 'a start racing setup is stopped after disabling starts before replacement'
$oldHash=(Get-FileHash -LiteralPath $location.helper).Hash
Write-MoyaiUtf8 (Join-Path $script:source 'app/bin/moyai-sandbox.exe') 'third inert helper'
Write-FixtureManifest 'system-installed'
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

New-Case
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
$location=Get-MoyaiSandboxLocation
$script:keepDeletePending=$true
$pendingOutput=@(Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall)
$pendingNotice=@($pendingOutput | Where-Object { $_.PSObject.TypeNames -contains 'Moyai.RetainedItem' })
Assert-That ($pendingNotice.Count -eq 1 -and $pendingNotice[0].Contains('消去を保留') -and $pendingNotice[0].Contains('再起動') -and -not(Test-Path -LiteralPath $location.root) -and @(Get-ChildItem -LiteralPath $location.parent -Directory -Filter 'SandboxService.*').Count -eq 0) 'an accepted deletion that Windows keeps pending completes the uninstall with a retained notice and next steps'
$before=$script:calls.Count
$script:clientRunning=$true
$repeatPending=@(Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall)
Assert-That (@($repeatPending | Where-Object { $_.Contains('消去を保留') }).Count -eq 1 -and $script:calls.Count -eq $before+1) 'repeated uninstall while deletion is pending succeeds without client checks or SCM changes'
$script:clientRunning=$false
$installFailure=''
try { Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null } catch { $installFailure=$_.Exception.Message }
Assert-That ($installFailure.Contains('導入できません') -and $installFailure.Contains('再起動') -and -not(Test-Path -LiteralPath $location.root) -and @(Get-ChildItem -LiteralPath $location.parent -Directory -Filter 'SandboxService.stage-*').Count -eq 0) 'install waits for a pending deletion and explains the next step without creating files'
$script:keepDeletePending=$false; $script:markedForDeletion=$false; $script:pendingService=$null
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
Assert-That ($script:service.Start -eq 3 -and -not(Get-MoyaiSandboxInstallPlan $script:source $script:manifest).needed) 'reinstall succeeds once Windows has finished the pending deletion'
Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall | Out-Null
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
Assert-That ($script:service.Start -eq 3 -and -not(Get-MoyaiSandboxInstallPlan $script:source $script:manifest).needed) 'uninstall before an update followed by install completes end to end'
Assert-That ((Get-Content -LiteralPath $legacySentinel -Raw) -ceq 'previous transient helper evidence') 'install, update and explicit removal preserve the legacy Sandbox directory'

New-Case
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
$location=Get-MoyaiSandboxLocation
# DeleteService accepted earlier, but the registration can still be opened and read.
$script:service.Start=4; $script:pendingService=$script:service; $script:service=$null; $script:markedForDeletion=$true; $script:pendingReadable=$true
$before=$script:calls.Count
$readablePending=@(Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall)
Assert-That (-not(Test-Path -LiteralPath $location.root) -and @($readablePending | Where-Object { $_.Contains('消去を保留') }).Count -eq 1 -and -not(@($script:calls | Select-Object -Skip $before) -contains 'delete')) 'a readable pending registration is not deleted again while its owned payload is removed'
$script:markedForDeletion=$false; $script:pendingReadable=$false; $script:pendingService=$null

New-Case
[void](New-Item -ItemType Directory -Path $script:machine)
Write-MoyaiUtf8 (Join-Path $script:machine 'sandbox-setup.lock') ''
$script:clientRunning=$true
$absent=@(Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall)
Assert-That ($absent.Count -eq 1 -and $absent[0].Contains('既に削除済み')) 'uninstall of an absent helper succeeds even while a moyAI app is running'
Remove-Item -LiteralPath (Join-Path $script:machine 'sandbox-setup.lock'); Remove-Item -LiteralPath $script:machine
$absent=@(Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall)
Assert-That ($absent.Count -eq 1 -and $absent[0].Contains('既に削除済み') -and -not(Test-Path -LiteralPath $script:machine)) 'uninstall without the machine folder is a successful no-op that creates nothing'
$script:clientRunning=$false

New-Case
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
$location=Get-MoyaiSandboxLocation
# A client keeps the helper pinned like InstalledService does (read, no delete sharing).
$pin=[IO.File]::Open($location.helper,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
$script:holders=@('moyai-0123456789abcdef (PID 4242)')
$busy=''
try { Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall | Out-Null } catch { $busy=$_.Exception.Message } finally { $pin.Dispose() }
Assert-That ($busy.Contains('使用中') -and $busy.Contains('PID 4242') -and $busy.Contains('再起動') -and $busy.Contains('強制終了しません')) 'a pinned helper folder names the holder and the next step instead of a raw access error'
Assert-That ($null -ne $script:service -and $script:service.Start -eq 3 -and (Test-Path -LiteralPath $location.helper)) 'an in-use helper keeps both its registration and files usable'
$script:holders=@()
Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall | Out-Null
Assert-That ($null -eq $script:service -and -not(Test-Path -LiteralPath $location.root)) 'uninstall succeeds after the holder exits'

New-Case
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
$location=Get-MoyaiSandboxLocation
# The helper reported STOPPED but its image is still mapped, so deletion keeps failing.
$script:payloadRemovalFailures=1000; $script:holders=@('moyai-sandbox.exe (PID 777)')
$partial=@(Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall)
$kept=@($partial | Where-Object { $_.PSObject.TypeNames -contains 'Moyai.RetainedItem' })
$leftover=@(Get-ChildItem -LiteralPath $location.parent -Directory -Filter 'SandboxService.previous-*')
Assert-That ($null -eq $script:service -and -not(Test-Path -LiteralPath $location.root) -and $leftover.Count -eq 1 -and $kept.Count -eq 1 -and $kept[0].Contains('PID 777') -and $kept[0].Contains('残ったファイルだけ')) 'a still-used old helper is kept outside the fixed path with guidance after the registration is removed'
$script:payloadRemovalFailures=0; $script:holders=@()
$retry=@(Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall)
Assert-That (@(Get-ChildItem -LiteralPath $location.parent -Directory -Filter 'SandboxService.previous-*').Count -eq 0 -and @($retry | Where-Object { $_.PSObject.TypeNames -contains 'Moyai.RetainedItem' }).Count -eq 0) 'rerunning uninstall removes only the leftover copy'
$script:payloadRemovalFailures=1
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall | Out-Null
Assert-That ($null -eq $script:service -and @(Get-ChildItem -LiteralPath $location.parent -Directory -Filter 'SandboxService.*').Count -eq 0) 'a helper that leaves within the retry window is removed in the same run'
$foreign=Join-Path $location.parent 'SandboxService.previous-foreign'
[void](New-Item -ItemType Directory -Path $foreign); Write-MoyaiUtf8 (Join-Path $foreign 'notes.txt') 'not created by setup'
$foreignRun=@(Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall)
Assert-That ((Test-Path -LiteralPath (Join-Path $foreign 'notes.txt')) -and @($foreignRun | Where-Object { $_.PSObject.TypeNames -contains 'Moyai.RetainedItem' -and $_.Contains('所有を確認できません') }).Count -eq 1) 'a leftover folder without an installation record is kept and reported'

New-Case
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
$location=Get-MoyaiSandboxLocation
# Damaged old module: its record is gone and an unknown file was added.
Remove-Item -LiteralPath (Join-Path $location.root 'installation.json')
Write-MoyaiUtf8 (Join-Path $location.root 'notes.txt') 'kept by an administrator'
Write-MoyaiUtf8 (Join-Path $script:source 'app/bin/moyai-sandbox.exe') 'helper for overwrite install'
Write-FixtureManifest 'system-installed'
Assert-That (Get-MoyaiSandboxInstallPlan $script:source $script:manifest).needed 'an unverifiable old module is seen as an update before UAC instead of an error'
$overwrite=@(Invoke-MoyaiMachineSandboxSetup $script:source)
$aside=@(Get-ChildItem -LiteralPath $location.parent -Directory -Filter 'SandboxService.previous-*' | Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'notes.txt') })
Assert-That ($script:service.Start -eq 3 -and -not(Get-MoyaiSandboxInstallPlan $script:source $script:manifest).needed -and $aside.Count -eq 1 -and (Get-Content -LiteralPath (Join-Path $aside[0].FullName 'notes.txt') -Raw) -ceq 'kept by an administrator' -and @($overwrite | Where-Object { $_.PSObject.TypeNames -contains 'Moyai.RetainedItem' -and $_.Contains('所有を確認できません') }).Count -eq 1) 'overwrite install replaces an unverifiable old module, keeps it intact aside and reports it'

New-Case
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
$location=Get-MoyaiSandboxLocation
# A process started from the old helper folder that is still leaving (a just-stopped helper).
$lingering=Join-Path $location.root 'lingering.exe'
Copy-Item -LiteralPath (Join-Path ([Environment]::SystemDirectory) 'PING.EXE') -Destination $lingering
$process=Start-Process -FilePath $lingering -ArgumentList @('-n','4','127.0.0.1') -WindowStyle Hidden -PassThru
try {
    Write-MoyaiUtf8 (Join-Path $script:source 'app/bin/moyai-sandbox.exe') 'helper installed after the old process left'
    Write-FixtureManifest 'system-installed'
    $timer=[Diagnostics.Stopwatch]::StartNew()
    Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
    Assert-That ($process.HasExited -and $timer.ElapsedMilliseconds -ge 1000 -and $script:service.Start -eq 3 -and -not(Get-MoyaiSandboxInstallPlan $script:source $script:manifest).needed) 'overwrite install waits for a just-closed process from the old folder to leave instead of failing'
} finally { [void]$process.WaitForExit(15000) }
$process=Start-Process -FilePath (Join-Path ([Environment]::SystemDirectory) 'PING.EXE') -ArgumentList @('-n','12','127.0.0.1') -WindowStyle Hidden -PassThru
$realProcessQuery=${function:Get-MoyaiProcessesUnderRoot}
try {
    # A process that does not leave keeps the update from starting; nothing is killed.
    function Get-MoyaiProcessesUnderRoot([string]$Root) { if(-not $process.HasExited){ "held (PID $($process.Id)): $Root\held.exe" } }
    $helperBefore=(Get-FileHash -LiteralPath $location.helper).Hash
    Write-MoyaiUtf8 (Join-Path $script:source 'app/bin/moyai-sandbox.exe') 'helper blocked by a live process'
    Write-FixtureManifest 'system-installed'
    $blocked=''
    try { Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null } catch { $blocked=$_.Exception.Message }
    Assert-That ($blocked.Contains("PID $($process.Id)") -and $blocked.Contains('再起動') -and -not $process.HasExited -and (Get-FileHash -LiteralPath $location.helper).Hash -ceq $helperBefore -and $script:service.Start -eq 3) 'a process that keeps running is named, left running, and the old helper stays usable'
} finally { Set-Item Function:Get-MoyaiProcessesUnderRoot $realProcessQuery; if(-not $process.HasExited){ [void]$process.WaitForExit(20000) } }

New-Case
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
$location=Get-MoyaiSandboxLocation
# Uninstall was accepted while the old helper still ran; Windows keeps the name until it stops.
$script:service.State=4; $script:service.Start=4; $script:pendingService=$script:service; $script:service=$null
$script:markedForDeletion=$true; $script:pendingReadable=$true; $script:pendingClearsOnStop=$true
Write-MoyaiUtf8 (Join-Path $script:source 'app/bin/moyai-sandbox.exe') 'helper after pending deletion'
Write-FixtureManifest 'system-installed'
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
Assert-That ($script:calls -contains 'stop' -and $null -ne $script:service -and $script:service.Start -eq 3 -and -not(Get-MoyaiSandboxInstallPlan $script:source $script:manifest).needed) 'install stops a still-running pending registration normally and registers again once Windows removes it'

foreach($damage in @('helper','directory','parent','service','empty','disabled')) {
    New-Case
    Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
    $location=Get-MoyaiSandboxLocation
    if($damage -eq 'service') { $script:service=$null }
    elseif($damage -eq 'disabled') { $script:service.Start=4 }
    else {
        Remove-Item -LiteralPath $location.helper
        if($damage -in @('directory','parent','empty')) {
            Remove-Item -LiteralPath (Join-Path $location.root 'installation.json')
            if($damage -ne 'empty'){Remove-Item -LiteralPath $location.root}
            if($damage -eq 'parent') {
                Remove-Item -LiteralPath (Join-Path $location.parent 'sandbox-setup.lock')
                Remove-Item -LiteralPath $location.parent
            }
        }
    }
    Assert-That (Get-MoyaiSandboxInstallPlan $script:source $script:manifest).needed "partial $damage installation reaches the repair transaction"
    Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
    Assert-That (-not(Get-MoyaiSandboxInstallPlan $script:source $script:manifest).needed) "setup repairs $damage without a prior uninstall"
    $before=$script:calls.Count
    Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
    Assert-That ($script:calls.Count -eq $before+1) "repeated setup after $damage needs no SCM mutation"
    Assert-That (@(Get-ChildItem -LiteralPath $location.parent -Directory -Filter 'SandboxService.previous-*').Count -eq 0) "successful $damage repair removes its owned old modules"
}

New-Case
Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
$script:service.State=4
Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall | Out-Null
Assert-That ($null -eq $script:service -and -not(Test-Path -LiteralPath (Get-MoyaiSandboxLocation).root)) 'uninstall stops a residual service and removes its registration and payload'

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

foreach($failure in @('publication','enable','none','unchanged-helper')) {
    New-Case
    Invoke-MoyaiMachineSandboxSetup $script:source | Out-Null
    $helper=Get-MoyaiSandboxLocation
    $helperHash=(Get-FileHash -LiteralPath $helper.helper).Hash
    $component=Get-MoyaiComponentLocation 'Desktop'
    $stage=Join-Path $script:machine '.Desktop.stage-fixture'
    foreach($directory in @($component.root,$stage)) {
        [void](New-Item -ItemType Directory -Path $directory)
        Write-MoyaiUtf8 (Join-Path $directory 'app.exe') $(if($directory -eq $stage){'new Desktop'}else{'old Desktop'})
        $hash=(Get-FileHash -LiteralPath (Join-Path $directory 'app.exe')).Hash.ToLowerInvariant()
        Write-MoyaiComponentJson (Join-Path $directory 'machine-component.json') @{schema=1;product='moyai-machine-component';component='Desktop';install_root=$component.root;source_manifest_sha256=('1'*64);files=@(@{path='app.exe';sha256=$hash})}
    }
    $context=[pscustomobject]@{location=$component;stage=$stage;old=(Read-MoyaiInstalledComponent 'Desktop');backup=$null;published=$false}
    if($failure -ne 'unchanged-helper') {
        Write-MoyaiUtf8 (Join-Path $script:source 'app/bin/moyai-sandbox.exe') 'new helper for paired update'
        Write-FixtureManifest 'system-installed'
    }
    if($failure -eq 'publication') { Write-MoyaiUtf8 (Join-Path $stage 'app.exe') 'injected corrupt copy' }
    $script:failEnable=$failure -eq 'enable'
    $invoke={Invoke-MoyaiMachineSandboxSetup $script:source -ComponentContext $context -PublishComponent {param($state) Publish-MoyaiComponentPayload $state} -RestoreComponent {param($state) Restore-MoyaiComponentPayload $state}}
    if($failure -in @('publication','enable')) {
        Assert-Fails $invoke "$failure failure is reported during a paired update"
        Assert-That ((Get-Content -LiteralPath (Join-Path $component.root 'app.exe') -Raw) -ceq 'old Desktop' -and (Get-FileHash -LiteralPath $helper.helper).Hash -ceq $helperHash -and $script:service.Start -eq 3) "$failure failure restores both the old Desktop and matching helper"
    } else {
        & $invoke | Out-Null
        Assert-That ((Get-Content -LiteralPath (Join-Path $component.root 'app.exe') -Raw) -ceq 'new Desktop' -and $script:service.Start -eq 3) "$failure paired update publishes Desktop and enables the matching helper"
        [void](Remove-MoyaiComponentPayload $context.backup $context.old)
        Assert-That (-not(Test-Path -LiteralPath $context.backup)) "$failure successful pair can retire the old Desktop payload"
    }
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
$unowned=@(Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall)
$aside=@(Get-ChildItem -LiteralPath $location.parent -Directory -Filter 'SandboxService.previous-*' | Where-Object { Test-Path -LiteralPath (Join-Path $_.FullName 'unexpected.dll') })
Assert-That ($null -eq $script:service -and -not(Test-Path -LiteralPath $location.root) -and $aside.Count -eq 1 -and @($unowned | Where-Object { $_.PSObject.TypeNames -contains 'Moyai.RetainedItem' -and $_.Contains('所有を確認できません') }).Count -eq 1) 'uninstall with unowned files removes the registration and moves the folder aside intact instead of deleting it'

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
$script:service.Start=2
$registrationFailure=''
try { Invoke-MoyaiMachineSandboxSetup $script:source -Uninstall | Out-Null } catch { $registrationFailure=$_.Exception.Message }
Assert-That ($registrationFailure.Contains('開始方法 2') -and $registrationFailure.Contains('想定 3 または 4') -and $script:service.Start -eq 2 -and (Test-Path -LiteralPath $location.helper)) 'unexpected registered start mode reports observed and expected values while preserving payload and configuration'
$script:service.Start=3
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
