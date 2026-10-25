param([string]$EvidenceRoot = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'release-components.ps1')
if (-not $EvidenceRoot) { $EvidenceRoot = Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) ('project_sandbox/release-components-' + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssfff')) }
$packageRoot = Join-Path ([IO.Path]::GetFullPath($EvidenceRoot)) 'package'
$checks = [Collections.Generic.List[string]]::new()
function Fixture([string]$Relative) {
  $path = Join-Path $packageRoot $Relative
  [void](New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path))
  [IO.File]::WriteAllText($path, 'inert package fixture', [Text.UTF8Encoding]::new($false))
  return $path
}
function Check([bool]$Condition, [string]$Message) {
  if (-not $Condition) { throw "FAIL: $Message" }
  $checks.Add("PASS: $Message")
}
$desktop = Get-MoyaiReleaseComponentFile $packageRoot (Fixture 'app/bin/moyai-desktop.exe') $true
Check ($desktop.component -ceq 'Desktop' -and $desktop.path -ceq 'app/bin/moyai-desktop.exe') 'Desktop component retains its runtime layout'
$hub = Get-MoyaiReleaseComponentFile $packageRoot (Fixture 'hub/bin/moyai-hub.exe') $true
Check ($hub.component -ceq 'Hub' -and $hub.install_path -ceq 'moyai-hub.exe') 'Hub executable installs beside the standalone launchers'
$hubCleanup = Get-MoyaiReleaseComponentFile $packageRoot (Fixture 'hub/bin/moyai-cleanup-hub.exe') $true
Check ($hubCleanup.component -ceq 'Hub' -and $hubCleanup.install_path -ceq 'moyai-cleanup-hub.exe') 'Hub cleanup stays with the Hub component'
$guide = Get-MoyaiReleaseComponentFile $packageRoot (Fixture 'hub/docs/web-management.md') $true
Check ($guide.install_path -ceq 'docs/web-management.md') 'Hub operational guides remain within the Hub installation'
$script = Get-MoyaiReleaseComponentFile $packageRoot (Fixture 'scripts/component-install.ps1') $true
Check ((@($script.component) -join ',') -ceq 'Desktop,Hub') 'Both components receive the same protected installer implementation'
$scriptOnly = Get-MoyaiReleaseComponentFile $packageRoot (Join-Path $packageRoot 'scripts/component-install.ps1') $false
Check ($scriptOnly.component -ceq 'Desktop') 'Desktop-only package does not claim a Hub installation'
$notice = Get-MoyaiReleaseComponentFile $packageRoot (Fixture 'runtime/visual-cpp-notices/REDIST.txt') $true
Check ((@($notice.component) -join ',') -ceq 'Desktop,Hub') 'Hub-only installation keeps notices for its app-local runtime'
Check ($hub.sha256 -ceq (Get-FileHash -LiteralPath (Join-Path $packageRoot $hub.path) -Algorithm SHA256).Hash.ToLowerInvariant()) 'Installed inventory binds to the source file contents'
$failed = $false
try { Get-MoyaiReleaseComponentFile $packageRoot (Join-Path $packageRoot 'hub/bin/moyai-hub.exe') $false | Out-Null } catch { $failed = $true }
Check $failed 'Desktop-only package rejects accidental Hub payload'
$failed = $false
try { Get-MoyaiReleaseComponentFile $packageRoot (Join-Path $EvidenceRoot 'outside.txt') $true | Out-Null } catch { $failed = $true }
Check $failed 'Component inventory rejects files outside the package boundary'

$commands = @('Setup-Hub.cmd', 'Uninstall-Hub.cmd', 'Start-Hub.cmd', 'Stop-Hub.cmd')
foreach ($name in @($commands) + @('Setup-Hub.ps1')) { [void](Fixture "Hub source/scripts/$name") }
[void](Fixture 'Hub source/docs/index.md')
$sourceScripts = Join-Path $packageRoot 'Hub source/scripts'
$sourceResources = Resolve-MoyaiHubSetupResources $sourceScripts
Check ($sourceResources.command_root -ceq $sourceScripts -and $sourceResources.powershell_path -ceq (Join-Path $sourceScripts 'Setup-Hub.ps1')) 'Existing flat source scripts supply both launchers and PowerShell setup'
$sourceRoot = (Get-Item -LiteralPath (Join-Path $packageRoot 'Hub source/docs')).Parent.FullName
$inferredSource = Resolve-MoyaiHubSetupResources $sourceRoot
Check ($inferredSource.command_root -ceq $sourceScripts -and $inferredSource.powershell_path -ceq $sourceResources.powershell_path) 'The source docs parent resolves its scripts without an explicit override'
foreach ($name in $commands) { [void](Fixture "Formal Hub/$name") }
[void](Fixture 'Formal Hub/scripts/Setup-Hub.ps1')
[void](Fixture 'Formal Hub/docs/index.md')
$formalRoot = Join-Path $packageRoot 'Formal Hub'
$formalResources = Resolve-MoyaiHubSetupResources $formalRoot
Check ($formalResources.command_root -ceq $formalRoot -and $formalResources.powershell_path -ceq (Join-Path $formalRoot 'scripts/Setup-Hub.ps1')) 'A formal standalone package supplies root launchers and nested PowerShell setup'
$inferredFormal = Resolve-MoyaiHubSetupResources (Get-Item -LiteralPath (Join-Path $formalRoot 'docs')).Parent.FullName
Check ($inferredFormal.command_root -ceq $formalResources.command_root -and $inferredFormal.powershell_path -ceq $formalResources.powershell_path) 'The formal docs parent resolves the split standalone layout'
foreach ($relative in @('Incomplete Hub/Setup-Hub.cmd', 'Incomplete Hub/scripts/Setup-Hub.ps1', 'Incomplete Hub/scripts/Start-Hub.cmd')) { [void](Fixture $relative) }
$failed = $false
try { Resolve-MoyaiHubSetupResources (Join-Path $packageRoot 'Incomplete Hub') | Out-Null } catch { $failed = $true }
Check $failed 'Incomplete Hub resources are rejected instead of mixing partial layouts'
foreach ($name in $commands) { [void](Fixture "No PowerShell/$name") }
$failed = $false
try { Resolve-MoyaiHubSetupResources (Join-Path $packageRoot 'No PowerShell') | Out-Null } catch { $failed = $true }
Check $failed 'A Hub package without its PowerShell setup is rejected'

$releaseName = 'moyAI-v3.0.4-windows-x86_64'
$publicPaths = Get-MoyaiReleasePublicPaths $releaseName $true
$publicRoot = Join-Path $packageRoot 'public output'
foreach ($relative in @($publicPaths.zip, $publicPaths.zip_sha256_file, $publicPaths.manual_results)) { [void](Fixture "public output/$relative") }
foreach ($relative in @($publicPaths.directory, $publicPaths.zip, $publicPaths.zip_sha256_file, $publicPaths.manual_results)) {
  $resolved = [IO.Path]::GetFullPath((Join-Path $publicRoot $relative))
  Check (-not [IO.Path]::IsPathRooted($relative) -and $resolved.StartsWith($publicRoot + [IO.Path]::DirectorySeparatorChar) -and (Test-Path -LiteralPath $resolved)) "Public manifest reference resolves beside the manifest: $relative"
}
Check ((Join-Path (Join-Path $publicRoot $publicPaths.directory) $publicPaths.manual_package_copy) -ceq (Join-Path $publicRoot $publicPaths.manual_results)) 'External and package-relative gate references identify the same copied evidence'
$publicJson = $publicPaths | ConvertTo-Json
Check (-not $publicJson.Contains($publicRoot) -and -not $publicJson.Contains($env:USERPROFILE)) 'Public path metadata does not contain a local absolute path or user profile'
$diagnosticPaths = Get-MoyaiReleasePublicPaths $releaseName $false
Check ($null -eq $diagnosticPaths.manual_results -and $null -eq $diagnosticPaths.manual_package_copy) 'An unpublished package without a manual gate has null evidence references'
$failed = $false
try { Get-MoyaiReleasePublicPaths '../outside' $true | Out-Null } catch { $failed = $true }
Check $failed 'Public release references reject a name that escapes the output directory'
[IO.File]::WriteAllLines((Join-Path $EvidenceRoot 'results.txt'), $checks, [Text.UTF8Encoding]::new($false))
$checks
