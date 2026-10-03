#Requires -Version 7.4
param(
  [Parameter(Mandatory)][string]$ExecutionRoot,
  [Parameter(Mandatory)][string]$CaptureDirectory
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if (-not [IO.Path]::IsPathFullyQualified($ExecutionRoot) -or -not [IO.Path]::IsPathFullyQualified($CaptureDirectory)) { throw 'Capture preparation requires absolute paths' }
$rootPath = [IO.Path]::GetFullPath($ExecutionRoot).TrimEnd([IO.Path]::DirectorySeparatorChar)
$logsPath = [IO.Path]::Combine($rootPath, 'logs')
$capturePath = [IO.Path]::GetFullPath($CaptureDirectory)
$expectedPath = [IO.Path]::Combine($logsPath, 'case2-provider-requests')
if (-not $capturePath.Equals($expectedPath, [StringComparison]::OrdinalIgnoreCase)) { throw 'Capture directory must be the fresh Case2 directory below executionRoot/logs' }
foreach ($candidate in @($rootPath, $logsPath)) {
  $item = Get-Item -LiteralPath $candidate -Force
  if (-not $item.PSIsContainer) { throw 'Capture parent must be a physical directory' }
  while ($null -ne $item) {
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'Capture path must not traverse a reparse point' }
    $item = $item.Parent
  }
}
if (Test-Path -LiteralPath $capturePath) { throw 'Capture directory must not already exist' }
$rootAclBefore = (Get-Acl -LiteralPath $rootPath).Sddl
$logsAclBefore = (Get-Acl -LiteralPath $logsPath).Sddl
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$currentSid = $identity.User
$allowedSids = @($currentSid.Value, 'S-1-5-18', 'S-1-5-32-544')
if (@($allowedSids | Select-Object -Unique).Count -ne 3) { throw 'Case2 capture requires an ordinary execution account distinct from SYSTEM/Administrators' }
$inherit = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [Security.AccessControl.InheritanceFlags]::ObjectInherit
$acl = [Security.AccessControl.DirectorySecurity]::new()
$acl.SetAccessRuleProtection($true, $false)
$acl.SetOwner($currentSid)
foreach ($sidValue in $allowedSids) {
  $rule = [Security.AccessControl.FileSystemAccessRule]::new([Security.Principal.SecurityIdentifier]::new($sidValue),
    [Security.AccessControl.FileSystemRights]::FullControl, $inherit, [Security.AccessControl.PropagationFlags]::None,
    [Security.AccessControl.AccessControlType]::Allow)
  [void]$acl.AddAccessRule($rule)
}
[void][IO.Directory]::CreateDirectory($capturePath)
Set-Acl -LiteralPath $capturePath -AclObject $acl

function Read-PrivateAcl([string]$Path, [bool]$Directory) {
  $readback = Get-Acl -LiteralPath $Path
  $ownerSid = $readback.GetOwner([Security.Principal.SecurityIdentifier]).Value
  $rules = @($readback.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]) | ForEach-Object {
    [ordered]@{ sid = $_.IdentityReference.Value; type = $_.AccessControlType.ToString(); rights = $_.FileSystemRights.ToString();
      full_control = ($_.FileSystemRights -band [Security.AccessControl.FileSystemRights]::FullControl) -eq [Security.AccessControl.FileSystemRights]::FullControl;
      inherited = $_.IsInherited; inheritance = $_.InheritanceFlags.ToString(); propagation = $_.PropagationFlags.ToString() }
  })
  if ($ownerSid -ne $currentSid.Value -or $rules.Count -ne 3 -or @($rules.sid | Select-Object -Unique).Count -ne 3) { throw 'Capture ACL owner or rule cardinality differs from the execution account contract' }
  foreach ($rule in $rules) {
    if ($rule.sid -notin $allowedSids -or $rule.type -ne 'Allow' -or -not $rule.full_control) { throw 'Capture ACL grants an unintended account or lacks full owner access' }
    if ($Directory -and ($rule.inherited -or $rule.inheritance -ne 'ContainerInherit, ObjectInherit' -or $rule.propagation -ne 'None')) { throw 'Capture directory ACL does not propagate the exact private rules' }
    if (-not $Directory -and -not $rule.inherited) { throw 'Capture file did not inherit the private directory rules' }
  }
  if ($Directory -and -not $readback.AreAccessRulesProtected) { throw 'Capture directory DACL still inherits parent access' }
  return [ordered]@{ owner_sid = $ownerSid; protected = $readback.AreAccessRulesProtected; rules = $rules }
}
$directoryAcl = Read-PrivateAcl $capturePath $true
$probePath = [IO.Path]::Combine($capturePath, 'acl-inheritance-probe.txt')
[IO.File]::WriteAllText($probePath, '')
$fileAcl = Read-PrivateAcl $probePath $false
Remove-Item -LiteralPath $probePath
$parentsUnchanged = (Get-Acl -LiteralPath $rootPath).Sddl -eq $rootAclBefore -and (Get-Acl -LiteralPath $logsPath).Sddl -eq $logsAclBefore
if (-not $parentsUnchanged) { throw 'Capture preparation changed a parent ACL' }
[ordered]@{ schema_version = 'desktop-e2e.manual-case2-capture-acl.v1'; execution_root = $rootPath; capture_directory = $capturePath;
  current_account = $identity.Name; current_sid = $currentSid.Value; allowed_sids = $allowedSids; directory_acl = $directoryAcl;
  inherited_file_acl = $fileAcl; parent_acls_unchanged = $parentsUnchanged; probe_removed = -not (Test-Path -LiteralPath $probePath) } | ConvertTo-Json -Depth 8 -Compress
$identity.Dispose()
