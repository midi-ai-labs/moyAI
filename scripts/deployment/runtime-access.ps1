Set-StrictMode -Version Latest

# Fixed WebView2 120+ uses an AppContainer renderer on unpackaged Windows 10.
# These permissions belong only to the bundled runtime, never application data.
function Test-MoyaiFixedRuntimeAccessRequired([int]$WindowsBuild, [int]$RuntimeMajor) {
  return $WindowsBuild -ge 10240 -and $WindowsBuild -lt 22000 -and $RuntimeMajor -ge 120
}

function Assert-MoyaiFixedRuntimeLocation([string]$Root) {
  $path = Resolve-MoyaiChild $Root 'runtime/webview2'
  if ($path.StartsWith('\\', [StringComparison]::Ordinal)) { throw '同梱WebView2はネットワーク共有から起動できません。Setup-moyAI.cmdを開き、このPCのローカルフォルダーへmoyAIを導入してください。' }
  $drive = [IO.DriveInfo]::new([IO.Path]::GetPathRoot($path))
  if ($drive.DriveType -eq [IO.DriveType]::Network) { throw '同梱WebView2はネットワークドライブから起動できません。Setup-moyAI.cmdを開き、このPCのローカルフォルダーへmoyAIを導入してください。' }
  Assert-MoyaiRegularPath $Root $path
  # A local drive letter can still lead to a network directory through an
  # ancestor link above the package root. Check the complete existing ancestry.
  $ancestor = [IO.DirectoryInfo]::new($path)
  while ($null -ne $ancestor) {
    if (Test-Path -LiteralPath $ancestor.FullName) {
      $item = Get-Item -LiteralPath $ancestor.FullName -Force
      if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw '同梱WebView2の保存先または親フォルダーがリンクになっています。リンクを使わないローカルフォルダーへ、Setup-moyAI.cmdで導入してください。' }
    }
    $ancestor = $ancestor.Parent
  }
  if (-not (Test-Path -LiteralPath $path -PathType Container)) { throw '画面表示に必要な同梱WebView2のフォルダーがありません。配布物を一式すべて展開し直してください。対象: runtime/webview2' }
  return $path
}

function Get-MoyaiFixedRuntimeItems([string]$Root) {
  $path = Assert-MoyaiFixedRuntimeLocation $Root
  # Enumerate without following junctions, including before inheritance is changed.
  $pending = [Collections.Generic.Queue[string]]::new()
  $pending.Enqueue($path)
  while ($pending.Count -gt 0) {
    $directory = $pending.Dequeue()
    Get-Item -LiteralPath $directory -Force
    foreach ($item in Get-ChildItem -LiteralPath $directory -Force) {
      if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "同梱WebView2にリンクが含まれています。配布担当から一式を受け取り直してください。対象: $($item.FullName)" }
      if ($item.PSIsContainer) { $pending.Enqueue($item.FullName) } else { $item }
    }
  }
}

function Test-MoyaiRuntimeReadRule($Acl, [string]$Sid, [bool]$Directory) {
  $rights = [Security.AccessControl.FileSystemRights]::ReadAndExecute
  $inheritance = [Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [Security.AccessControl.InheritanceFlags]::ObjectInherit
  $allowed = $false
  foreach ($rule in $Acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
    if ($rule.IdentityReference.Value -ne $Sid) { continue }
    if (($rule.PropagationFlags -band [Security.AccessControl.PropagationFlags]::InheritOnly) -ne 0) { continue }
    if ($rule.AccessControlType -eq [Security.AccessControl.AccessControlType]::Deny -and ($rule.FileSystemRights -band $rights) -ne 0) { return $false }
    if ($rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow -or ($rule.FileSystemRights -band $rights) -ne $rights) { continue }
    if ($Directory -and (($rule.InheritanceFlags -band $inheritance) -ne $inheritance -or $rule.PropagationFlags -ne [Security.AccessControl.PropagationFlags]::None)) { continue }
    $allowed = $true
  }
  return $allowed
}

function Assert-MoyaiRuntimeReadAccess([string]$Root) {
  foreach ($item in @(Get-MoyaiFixedRuntimeItems $Root)) {
    $acl = Get-Acl -LiteralPath $item.FullName
    foreach ($sid in @('S-1-15-2-1', 'S-1-15-2-2')) {
      if (-not (Test-MoyaiRuntimeReadRule $acl $sid $item.PSIsContainer)) {
        throw '同梱WebView2を動かすためのWindows 10の読込み・実行権限が不足しています。元の配布物のSetup-moyAI.cmdから再度導入してください。設定と履歴は保持します。'
      }
    }
  }
}

function Grant-MoyaiRuntimeReadAccess([string]$Root) {
  $items = @(Get-MoyaiFixedRuntimeItems $Root)
  $path = $items[0].FullName
  # Fresh setup staging inherits normally. Do not rewrite a protected child DACL.
  foreach ($item in $items | Select-Object -Skip 1) {
    if ((Get-Acl -LiteralPath $item.FullName).AreAccessRulesProtected) { throw '同梱WebView2の一部に必要な権限を引き継げません。配布物を別のローカルフォルダーへ一式すべて展開し直してください。' }
  }
  # Read and persist the DACL only. PowerShell 5.1 Set-Acl can request the
  # unrelated SeSecurityPrivilege when handed a complete security descriptor.
  $acl = [Security.AccessControl.DirectorySecurity]::new($path, [Security.AccessControl.AccessControlSections]::Access)
  $changed = $false
  foreach ($sid in @('S-1-15-2-1', 'S-1-15-2-2')) {
    if (Test-MoyaiRuntimeReadRule $acl $sid $true) { continue }
    $rule = [Security.AccessControl.FileSystemAccessRule]::new(
      [Security.Principal.SecurityIdentifier]::new($sid),
      [Security.AccessControl.FileSystemRights]::ReadAndExecute,
      ([Security.AccessControl.InheritanceFlags]::ContainerInherit -bor [Security.AccessControl.InheritanceFlags]::ObjectInherit),
      [Security.AccessControl.PropagationFlags]::None,
      [Security.AccessControl.AccessControlType]::Allow)
    [void]$acl.AddAccessRule($rule)
    $changed = $true
  }
  if ($changed) {
    if ($PSVersionTable.PSEdition -eq 'Core') {
      [IO.FileSystemAclExtensions]::SetAccessControl([IO.DirectoryInfo]::new($path), $acl)
    } else {
      [IO.Directory]::SetAccessControl($path, $acl)
    }
  }
  Assert-MoyaiRuntimeReadAccess $Root
}

function Test-MoyaiFixedRuntimeNeedsAccess([string]$Root) {
  $binary = Resolve-MoyaiChild $Root 'runtime/webview2/msedgewebview2.exe'
  $version = [Diagnostics.FileVersionInfo]::GetVersionInfo($binary)
  return Test-MoyaiFixedRuntimeAccessRequired ([Environment]::OSVersion.Version.Build) $version.FileMajorPart
}

function Initialize-MoyaiFixedRuntimeAccess([string]$Root, $Manifest) {
  if ($Manifest.runtime.webview2 -ne 'bundled-fixed') { return }
  [void](Assert-MoyaiFixedRuntimeLocation $Root)
  if (Test-MoyaiFixedRuntimeNeedsAccess $Root) { Grant-MoyaiRuntimeReadAccess $Root }
}

function Assert-MoyaiFixedRuntimeAccess([string]$Root) {
  [void](Assert-MoyaiFixedRuntimeLocation $Root)
  if (Test-MoyaiFixedRuntimeNeedsAccess $Root) { Assert-MoyaiRuntimeReadAccess $Root }
}
