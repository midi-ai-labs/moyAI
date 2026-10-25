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
[IO.File]::WriteAllLines((Join-Path $EvidenceRoot 'results.txt'), $checks, [Text.UTF8Encoding]::new($false))
$checks
