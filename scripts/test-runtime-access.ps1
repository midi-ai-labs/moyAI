param([string]$EvidenceRoot = '')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'deployment/common.ps1')
. (Join-Path $PSScriptRoot 'deployment/runtime-access.ps1')
if (-not $EvidenceRoot) { $EvidenceRoot = Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) ('project_sandbox/runtime-access-' + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmss')) }
$EvidenceRoot = [IO.Path]::GetFullPath($EvidenceRoot)
if (Test-Path -LiteralPath $EvidenceRoot) { throw 'Use a new evidence directory for this test.' }
New-Item -ItemType Directory -Path $EvidenceRoot | Out-Null
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
function New-RuntimeFixture([string]$Name) {
  $root = Join-Path $EvidenceRoot $Name
  New-Item -ItemType Directory -Path (Join-Path $root 'runtime/webview2/locales') -Force | Out-Null
  [IO.File]::WriteAllText((Join-Path $root 'runtime/webview2/msedgewebview2.exe'), 'inert file; never execute')
  [IO.File]::WriteAllText((Join-Path $root 'runtime/webview2/locales/en-US.pak'), 'inert runtime content')
  return $root
}
Assert-That (Test-MoyaiFixedRuntimeAccessRequired 19045 120) 'Windows 10 and WebView2 120 require renderer read access'
Assert-That (Test-MoyaiFixedRuntimeAccessRequired 19045 150) 'newer fixed runtimes retain Windows 10 requirement'
Assert-That (-not (Test-MoyaiFixedRuntimeAccessRequired 19045 119)) 'older runtime does not add the version 120 requirement'
Assert-That (-not (Test-MoyaiFixedRuntimeAccessRequired 22000 120)) 'Windows 11 does not require these additional grants'
Assert-That (-not (Test-MoyaiFixedRuntimeAccessRequired 26100 150)) 'current Windows 11 build does not require these grants'
Assert-Fails { Assert-MoyaiFixedRuntimeLocation '\\server\share\moyAI' } 'UNC runtime execution is rejected before network access'
$root = New-RuntimeFixture 'package'
$runtime = Join-Path $root 'runtime/webview2'
$outside = Join-Path $EvidenceRoot 'private-profile.txt'
[IO.File]::WriteAllText($outside, 'private fixture remains private')
$outsideAcl = (Get-Acl -LiteralPath $outside).Sddl
$parentAcl = (Get-Acl -LiteralPath $root).Sddl
# Make the before state deterministic while preserving this test user's access.
$acl = Get-Acl -LiteralPath $runtime
$acl.SetAccessRuleProtection($true, $true)
foreach ($rule in @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))) {
  if ($rule.IdentityReference.Value -in @('S-1-15-2-1', 'S-1-15-2-2')) { [void]$acl.RemoveAccessRuleSpecific($rule) }
}
Set-Acl -LiteralPath $runtime -AclObject $acl
$before = (Get-Acl -LiteralPath $runtime).Sddl
Assert-Fails { Assert-MoyaiRuntimeReadAccess $root } 'missing runtime ACL is diagnosed before launch'
Assert-That ((Get-Acl -LiteralPath $runtime).Sddl -eq $before) 'read-only prerequisite check does not change permissions'
Grant-MoyaiRuntimeReadAccess $root
Assert-MoyaiRuntimeReadAccess $root
Assert-That $true 'grant reaches runtime root, subdirectory and files'
foreach ($sid in @('S-1-15-2-1', 'S-1-15-2-2')) {
  $rules = @((Get-Acl -LiteralPath $runtime).GetAccessRules($true, $false, [Security.Principal.SecurityIdentifier]) | Where-Object { $_.IdentityReference.Value -eq $sid })
  Assert-That ($rules.Count -eq 1) "one explicit rule for $sid"
  $write = [Security.AccessControl.FileSystemRights]::Write -bor [Security.AccessControl.FileSystemRights]::Delete -bor [Security.AccessControl.FileSystemRights]::ChangePermissions -bor [Security.AccessControl.FileSystemRights]::TakeOwnership
  Assert-That (($rules[0].FileSystemRights -band $write) -eq 0) "no write, delete or ownership rights for $sid"
}
$after = (Get-Acl -LiteralPath $runtime).Sddl
Grant-MoyaiRuntimeReadAccess $root
Assert-That ((Get-Acl -LiteralPath $runtime).Sddl -eq $after) 'reapplying the runtime grants is idempotent'
Assert-That ((Get-Acl -LiteralPath $outside).Sddl -eq $outsideAcl -and (Get-Acl -LiteralPath $root).Sddl -eq $parentAcl) 'private data and package root ACLs are unchanged'
$protected = New-RuntimeFixture 'protected-child'
$child = Join-Path $protected 'runtime/webview2/locales/en-US.pak'
$acl = Get-Acl -LiteralPath $child
$acl.SetAccessRuleProtection($true, $true)
Set-Acl -LiteralPath $child -AclObject $acl
$protectedBefore = (Get-Acl -LiteralPath (Join-Path $protected 'runtime/webview2')).Sddl
Assert-Fails { Grant-MoyaiRuntimeReadAccess $protected } 'protected child permissions are not silently rewritten'
Assert-That ((Get-Acl -LiteralPath (Join-Path $protected 'runtime/webview2')).Sddl -eq $protectedBefore) 'protected-child rejection occurs before runtime ACL mutation'
$linked = New-RuntimeFixture 'junction-child'
New-Item -ItemType Junction -Path (Join-Path $linked 'runtime/webview2/escape') -Target $root | Out-Null
Assert-Fails { Grant-MoyaiRuntimeReadAccess $linked } 'runtime directory junction is rejected before recursive grants'
Assert-That ((Get-Acl -LiteralPath $outside).Sddl -eq $outsideAcl) 'junction rejection does not affect external data'
$ancestorRoot = New-RuntimeFixture 'alias-target/package'
$ancestorAcl = (Get-Acl -LiteralPath (Join-Path $ancestorRoot 'runtime/webview2')).Sddl
$alias = Join-Path $EvidenceRoot 'alias-parent'
New-Item -ItemType Junction -Path $alias -Target (Join-Path $EvidenceRoot 'alias-target') | Out-Null
$aliasedPackage = Join-Path $alias 'package'
Assert-Fails { Assert-MoyaiFixedRuntimeLocation $aliasedPackage } 'junction above the package root cannot bypass the local-runtime boundary'
Assert-Fails { Grant-MoyaiRuntimeReadAccess $aliasedPackage } 'ancestor junction is rejected before any runtime ACL mutation'
Assert-That ((Get-Acl -LiteralPath (Join-Path $ancestorRoot 'runtime/webview2')).Sddl -eq $ancestorAcl) 'target ACL remains unchanged through a rejected ancestor alias'
$result = @('# Fixed WebView2 runtime access regression', '', "PowerShell $($PSVersionTable.PSVersion); Windows build $([Environment]::OSVersion.Version.Build).", 'Only inert files and ACLs under this evidence directory were changed. This is not Windows 10 WebView2 execution evidence.', '') + $checks
[IO.File]::WriteAllLines((Join-Path $EvidenceRoot 'RESULTS.md'), $result, [Text.UTF8Encoding]::new($false))
$checks
Write-Output "Evidence: $EvidenceRoot"
