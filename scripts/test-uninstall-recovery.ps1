param([Parameter(Mandatory=$true)][string]$EvidenceRoot,[switch]$ElevationSmoke)
$ErrorActionPreference='Stop'
$EvidenceRoot=[IO.Path]::GetFullPath($EvidenceRoot)
if(Test-Path -LiteralPath $EvidenceRoot){throw 'Use a new evidence directory.'}
[void](New-Item -ItemType Directory -Path $EvidenceRoot)
$env:TEMP=Join-Path $EvidenceRoot 'temp';$env:TMP=$env:TEMP
[void](New-Item -ItemType Directory -Path $env:TEMP)
$owner=Join-Path $PSScriptRoot 'deployment/component-install.ps1'
. $owner
$checks=[Collections.Generic.List[string]]::new()
function Assert-That([bool]$Value,[string]$Message){if(-not $Value){throw ('FAIL: '+$Message)};$checks.Add($Message)}
function Assert-Fails([scriptblock]$Action,[string]$Message){$failed=$false;try{& $Action|Out-Null}catch{$failed=$true;[IO.File]::AppendAllText((Join-Path $EvidenceRoot 'expected-failures.log'),($Message+': '+$_.Exception.Message+[Environment]::NewLine))};Assert-That $failed $Message}
function Quote([string]$Value){"'"+$Value.Replace("'","''")+"'"}
$mock=Join-Path $EvidenceRoot 'boundaries.ps1'
$boundaryCode=@'
$fixtureRoot=FIXTURE_ROOT
function Assert-FixturePath([string]$Path){$full=[IO.Path]::GetFullPath($Path);if($full -ine $fixtureRoot -and -not $full.StartsWith($fixtureRoot+'\',[StringComparison]::OrdinalIgnoreCase)){throw ('Fixture boundary escaped: '+$Path)};Assert-MoyaiSandboxRegularPath $Path}
function Assert-MoyaiSandboxAdministrator {}
function Assert-MoyaiUserSetup {}
function Assert-MoyaiSandboxProtectedPath([string]$Path){Assert-FixturePath $Path}
function Set-MoyaiSandboxProtection([string]$Path,[bool]$Directory,[switch]$Private){Assert-FixturePath $Path}
function Set-MoyaiSandboxParentAccess([string]$Path){Assert-FixturePath $Path}
function Set-MoyaiComponentTreeOwner([string]$Root){Assert-FixturePath $Root}
function Get-MoyaiComponentLocation([string]$Component){$parent=Join-Path $fixtureRoot 'machine';[pscustomobject]@{parent=$parent;root=(Join-Path $parent $Component);component=$Component}}
function Get-MoyaiComponentResultRoot {Join-Path $fixtureRoot 'results'}
function Write-MoyaiComponentResult([string]$OperationId,[string]$ManifestSha256,[string[]]$Components,[bool]$Success,[int]$ExitCode,[string]$Message,[string[]]$Messages=@()){
 $path=Get-MoyaiComponentResultPath $OperationId;[void](New-Item -ItemType Directory -Path (Split-Path -Parent $path) -Force)
 $bytes=[Text.Encoding]::UTF8.GetBytes((@{schema=1;operation_id=$OperationId;source_manifest_sha256=$ManifestSha256;components=@($Components);success=$Success;exit_code=$ExitCode;message=$Message;messages=@($Messages)}|ConvertTo-Json -Depth 6))
 $stream=[IO.File]::Open($path,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
 try{$stream.Write($bytes,0,$bytes.Length)}finally{$stream.Dispose()}
}
function Remove-MoyaiHubUserRegistrations {}
function Get-MoyaiSandboxService {throw 'Fixture must not access SCM'}
'@
if($ElevationSmoke){$boundaryCode=$boundaryCode.Replace('function Assert-MoyaiSandboxAdministrator {}','')}
[IO.File]::WriteAllText($mock,$boundaryCode.Replace('FIXTURE_ROOT',(Quote $EvidenceRoot)),[Text.UTF8Encoding]::new($true))
. $mock
# Fixture provisioning occurs without elevation. Only the removal transaction
# exercises UAC; its script retains the production administrator assertion.
$administratorAssertion=${function:Assert-MoyaiSandboxAdministrator}
function Assert-MoyaiSandboxAdministrator {}
$package=Join-Path $EvidenceRoot 'package';[void](New-Item -ItemType Directory -Path (Join-Path $package 'scripts'))
# The real elevated entrypoint runs in an ordinary child with only machine and
# registration boundaries replaced. The wait/result/decision protocol is real.
[IO.File]::WriteAllText((Join-Path $package 'scripts/component-install.ps1'),('. '+(Quote $owner)+[Environment]::NewLine+'. '+(Quote $mock)),[Text.UTF8Encoding]::new($true))
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'deployment/Setup-Component.ps1') -Destination (Join-Path $package 'scripts/Setup-Component.ps1')
foreach($name in @('scripts/sandbox-service.ps1','moyai-hub.exe','moyai-cleanup-hub.exe','README.txt')){[IO.File]::WriteAllText((Join-Path $package $name),'inert fixture')}
$files=@(Get-ChildItem -LiteralPath $package -Recurse -File|ForEach-Object{[pscustomobject]@{path=$_.FullName.Substring($package.Length+1).Replace('\','/');sha256=(Get-FileHash -LiteralPath $_.FullName).Hash.ToLowerInvariant();component='Hub'}})
$manifest=[ordered]@{schema=1;product='moyai-hub-windows';target='windows-x86_64';version='1.0.4';components=@{Hub=@{version='1.0.4'}};files=$files}
Write-MoyaiComponentJson (Join-Path $package 'deployment.json') $manifest
$manifest=Read-MoyaiComponentManifest $package
Invoke-MoyaiMachineComponent $package @('Hub') -ExpectedManifestSha256 $manifest.validated_manifest_sha256|Out-Null
Set-Item -Path Function:Assert-MoyaiSandboxAdministrator -Value $administratorAssertion
$hub=(Get-MoyaiComponentLocation 'Hub').root
$missing=Join-Path $hub 'README.txt';Assert-FixturePath $missing;Remove-Item -LiteralPath $missing
Assert-Fails {Read-MoyaiComponentManifest $hub} 'normal installation validation still rejects missing payload'
$removal=Read-MoyaiComponentManifest $hub -ForUninstall
Assert-That ($removal.uninstall_only -and @($removal.files|Where-Object path -eq 'moyai-hub.exe').Count -eq 0) 'self-uninstall does not require missing ordinary payload or a complete app image'
$snapshot=New-MoyaiComponentSourceSnapshot $hub $removal @('Hub')
try{Assert-That (Test-Path -LiteralPath (Join-Path $snapshot.root 'scripts/Setup-Component.ps1')) 'partial self-uninstall constructs verified removal machinery'}finally{Remove-MoyaiComponentSourceSnapshot $snapshot.root $snapshot.manifest}
$script:cleanupFails=$true;$script:cleanupCalls=0;$script:cancelElevation=$true;$script:rejectChildPreflight=$false
function Invoke-MoyaiComponentCleanup($Component,$HubDataDir,$SourceRoot,$Manifest){
 $script:cleanupCalls++
 Assert-That (Test-Path -LiteralPath (Join-Path $hub 'moyai-hub.exe')) 'app remains installed while the non-elevated cleanup runs'
 if($ElevationSmoke){$identity=[Security.Principal.WindowsIdentity]::GetCurrent();try{Assert-That (-not([Security.Principal.WindowsPrincipal]::new($identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))) 'profile cleanup runs as the original non-elevated user'}finally{$identity.Dispose()}}
 if($script:cleanupFails){throw 'Injected profile reset failure'}
 [IO.File]::WriteAllText((Join-Path $EvidenceRoot 'cleanup-completed'),'fixture reset completed')
}
function Start-Process([string]$FilePath,[string]$ArgumentList,[string]$Verb,[string]$WindowStyle,[switch]$PassThru,[switch]$Wait){
 if($Verb -cne 'RunAs'){throw 'Expected the elevation boundary'}
 if($script:cancelElevation){throw 'Injected UAC cancellation'}
 if($script:rejectChildPreflight){[IO.File]::WriteAllText((Join-Path $hub 'machine-component.json'),'invalid receipt')}
 if($ElevationSmoke){return Microsoft.PowerShell.Management\Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -Verb RunAs -WindowStyle Hidden -PassThru}
 Microsoft.PowerShell.Management\Start-Process -FilePath $FilePath -ArgumentList $ArgumentList -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $EvidenceRoot ('child-'+[Guid]::NewGuid().ToString('N')+'.stdout.log')) -RedirectStandardError (Join-Path $EvidenceRoot ('child-'+[Guid]::NewGuid().ToString('N')+'.stderr.log'))
}
if(-not $ElevationSmoke){
Assert-Fails {Invoke-MoyaiComponentUserSetup $package @('Hub') -Uninstall -ResetHubData} 'UAC cancellation cannot begin profile cleanup'
Assert-That ($script:cleanupCalls -eq 0 -and (Test-Path -LiteralPath (Join-Path $hub 'moyai-hub.exe'))) 'cancelled elevation preserves app and profile'
$script:cancelElevation=$false
$receiptPath=Join-Path $hub 'machine-component.json';$originalReceipt=[IO.File]::ReadAllText($receiptPath)
$script:rejectChildPreflight=$true
try {
 Assert-Fails {Invoke-MoyaiComponentUserSetup $package @('Hub') -Uninstall -ResetHubData} 'failed elevated preflight cannot begin profile cleanup'
 Assert-That ($script:cleanupCalls -eq 0 -and (Test-Path -LiteralPath (Join-Path $hub 'moyai-hub.exe'))) 'a changed installation after elevation preserves app and profile'
} finally {$script:rejectChildPreflight=$false;[IO.File]::WriteAllText($receiptPath,$originalReceipt,[Text.UTF8Encoding]::new($true))}
Assert-Fails {Invoke-MoyaiComponentUserSetup $package @('Hub') -Uninstall -ResetHubData} 'cleanup failure aborts the elevated removal transaction'
Assert-That ($script:cleanupCalls -eq 1 -and (Test-Path -LiteralPath (Join-Path $hub 'moyai-hub.exe'))) 'failed reset preserves the installed app and performs no payload deletion'
}
$script:cancelElevation=$false
$script:cleanupFails=$false
Invoke-MoyaiComponentUserSetup $package @('Hub') -Uninstall -ResetHubData|Out-Null
$expectedCalls=if($ElevationSmoke){1}else{2}
Assert-That ($script:cleanupCalls -eq $expectedCalls -and -not(Test-Path -LiteralPath $hub) -and (Test-Path -LiteralPath (Join-Path $EvidenceRoot 'cleanup-completed'))) 'successful reset precedes app deletion and the same request is not replayed'
Assert-That (@(Get-ChildItem -LiteralPath $env:TEMP -Directory -Filter 'moyai-uninstall-*').Count -eq 0) 'both success and cleanup refusal release and remove control snapshots'
$decisionRoot=Join-Path $EvidenceRoot 'decision';[void](New-Item -ItemType Directory -Path $decisionRoot)
$operation=[Guid]::NewGuid().ToString('N')
Write-MoyaiRemovalDecision $decisionRoot $operation $false
Assert-Fails {Wait-MoyaiRemovalDecision $decisionRoot $operation} 'explicit refusal cannot authorize payload removal'
$operation=[Guid]::NewGuid().ToString('N')
Write-MoyaiComponentJson (Get-MoyaiRemovalDecisionPath $decisionRoot $operation) @{operation_id=('0'*32);proceed=$true}
Assert-Fails {Wait-MoyaiRemovalDecision $decisionRoot $operation} 'a decision for another operation cannot authorize removal'
@{passed=$checks.Count;checks=$checks;real_elevation=[bool]$ElevationSmoke;scope='Task-local inert payloads and child process handshake. SCM, machine locations, ACL checks and user cleanup effects substituted; elevation used only with ElevationSmoke.'}|ConvertTo-Json -Depth 5|Set-Content -LiteralPath (Join-Path $EvidenceRoot 'results.json') -Encoding UTF8
$checks
