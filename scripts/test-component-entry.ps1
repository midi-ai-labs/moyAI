param([Parameter(Mandatory=$true)][string]$EvidenceRoot)
$ErrorActionPreference='Stop'
$EvidenceRoot=[IO.Path]::GetFullPath($EvidenceRoot)
if(Test-Path -LiteralPath $EvidenceRoot) { throw 'Use a new evidence directory.' }
$package=Join-Path $EvidenceRoot 'package'
[void](New-Item -ItemType Directory -Path (Join-Path $package 'scripts'),(Join-Path $package 'app/bin'))
# Keep the real entrypoint, component transaction and runtime imports. Substitute
# only privileged machine boundaries; each run starts without inherited functions.
foreach($name in @('Setup-Component.ps1','component-install.ps1','common.ps1','runtime-access.ps1','sandbox-service.ps1')) {
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot "deployment/$name") -Destination (Join-Path $package "scripts/$name")
}
$boundaries=@'
function Assert-MoyaiSandboxAdministrator { }
function Assert-MoyaiSandboxProtectedPath([string]$Path) { Assert-MoyaiSandboxRegularPath $Path }
function Set-MoyaiSandboxProtection([string]$Path,[bool]$Directory,[switch]$Private) { }
function Set-MoyaiSandboxParentAccess([string]$Path) { }
function Set-MoyaiComponentTreeOwner([string]$Root) { Assert-MoyaiSandboxRegularPath $Root }
function Assert-MoyaiComponentStopped([string]$Root) { }
function Get-MoyaiComponentLocation([string]$Component) {
    $parent=Join-Path (Split-Path -Parent $SourceRoot) 'machine'
    [pscustomobject]@{parent=$parent;root=(Join-Path $parent $Component);component=$Component}
}
function Invoke-MoyaiMachineSandboxSetup($SourceRoot,$ExpectedManifestSha256,$ComponentContext,[scriptblock]$PublishComponent,[scriptblock]$RestoreComponent) {
    try {
        & $PublishComponent $ComponentContext
        if(Test-Path -LiteralPath (Join-Path (Split-Path -Parent $SourceRoot) 'fail-publication')) { throw 'Injected helper finalization failure' }
    } catch { & $RestoreComponent $ComponentContext; throw }
}
function Write-MoyaiComponentResult($OperationId,$ManifestSha256,$Components,$Success,$ExitCode,$Message,$Messages) {
    [IO.File]::WriteAllText((Join-Path (Split-Path -Parent $SourceRoot) ($OperationId+'.json')),(@{success=$Success;exit_code=$ExitCode;message=$Message;messages=$Messages}|ConvertTo-Json -Depth 8))
}
'@
foreach($name in @('component-install.ps1','sandbox-service.ps1')) {
    [IO.File]::AppendAllText((Join-Path $package "scripts/$name"),("`r`n"+$boundaries),[Text.UTF8Encoding]::new($true))
}
function Run-Entry([string]$Name,[string]$Payload,[int]$ExpectedExit) {
    [IO.File]::WriteAllText((Join-Path $package 'app/bin/moyai-desktop.exe'),$Payload)
    $files=@(Get-ChildItem -LiteralPath $package -Recurse -File | Where-Object {$_.Name -cne 'deployment.json'} | ForEach-Object {
        @{path=$_.FullName.Substring($package.Length+1).Replace('\','/');sha256=(Get-FileHash -LiteralPath $_.FullName).Hash.ToLowerInvariant();component='Desktop'}
    })
    $manifest=@{schema=1;product='moyai-windows-user';target='windows-x86_64';version='3.0.8';runtime=@{webview2='system-installed';visual_cpp='system-installed'};hub=$null;components=@{Desktop=@{version='3.0.8'}};files=$files}
    $manifestPath=Join-Path $package 'deployment.json'
    [IO.File]::WriteAllText($manifestPath,($manifest|ConvertTo-Json -Depth 12),[Text.UTF8Encoding]::new($true))
    $operation=[Guid]::NewGuid().ToString('N')
    $arguments='-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "'+(Join-Path $package 'scripts/Setup-Component.ps1')+'" -SourceRoot "'+$package+'" -Components Desktop -ExpectedManifestSha256 '+(Get-FileHash -LiteralPath $manifestPath).Hash.ToLowerInvariant()+' -OperationId '+$operation
    $process=Start-Process -FilePath (Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe') -ArgumentList $arguments -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $EvidenceRoot ($Name+'.stdout.txt')) -RedirectStandardError (Join-Path $EvidenceRoot ($Name+'.stderr.txt'))
    try {
        $null=$process.Handle
        if(-not $process.WaitForExit(15000)) { Stop-Process -Id $process.Id; throw 'Inert entry fixture timed out.' }
        $result=Get-Content -LiteralPath (Join-Path $EvidenceRoot ($operation+'.json')) -Raw -Encoding UTF8 | ConvertFrom-Json
        if($process.ExitCode -ne $ExpectedExit -or $result.exit_code -ne $ExpectedExit) { throw ($Name+': '+$result.message) }
        if($ExpectedExit -ne 0 -and $result.message -cne 'Injected helper finalization failure') { throw ('Unexpected failure: '+$result.message) }
    } finally { $process.Dispose() }
}
Run-Entry 'initial' 'first inert Desktop' 0
$installed=Join-Path $EvidenceRoot 'machine/Desktop/app/bin/moyai-desktop.exe'
$initialHash=(Get-FileHash -LiteralPath $installed).Hash
[IO.File]::WriteAllText((Join-Path $EvidenceRoot 'fail-publication'),'inert fixture')
Run-Entry 'rollback' 'replacement inert Desktop' 1
if((Get-FileHash -LiteralPath $installed).Hash -cne $initialHash) { throw 'Failed helper finalization did not restore Desktop.' }
Remove-Item -LiteralPath (Join-Path $EvidenceRoot 'fail-publication')
Run-Entry 'update' 'replacement inert Desktop' 0
if((Get-Content -LiteralPath $installed -Raw) -cne 'replacement inert Desktop') { throw 'Clean entry update did not publish Desktop.' }
[IO.File]::WriteAllText((Join-Path $EvidenceRoot 'results.json'),(@{passed=$true;cases=@('fresh-process install','paired publication rollback','fresh-process update')}|ConvertTo-Json))
Write-Output 'PASS: three fresh Windows PowerShell component entry cases.'
