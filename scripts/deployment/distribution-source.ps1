# A cloud-synced package is input only. Installation and execution use verified,
# ordinary local files; the existing protected-destination checks remain strict.
function Initialize-MoyaiDistributionNative {
  if ('MoyaiDistribution.Source' -as [type]) { return }
  Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
namespace MoyaiDistribution {
 public static class Source {
  [StructLayout(LayoutKind.Sequential)] struct Tag { public uint Attributes, Value; }
  [StructLayout(LayoutKind.Sequential)] struct Info {
   public uint Attributes; public System.Runtime.InteropServices.ComTypes.FILETIME Created, Accessed, Written;
   public uint Volume, SizeHigh, SizeLow, Links, IndexHigh, IndexLow;
  }
  [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern SafeFileHandle CreateFile(string path,uint access,uint share,IntPtr security,uint mode,uint flags,IntPtr template);
  [DllImport("kernel32.dll",SetLastError=true)] static extern bool GetFileInformationByHandleEx(SafeFileHandle file,int kind,out Tag tag,uint size);
  [DllImport("kernel32.dll",SetLastError=true)] static extern bool GetFileInformationByHandle(SafeFileHandle file,out Info info);
  [DllImport("ntdll.dll")] static extern sbyte RtlSetThreadPlaceholderCompatibilityMode(sbyte mode);
  public static bool IsCloud(uint tag) { return (tag & 0xffff0fffU) == 0x9000001aU; }
  static Tag Inspect(SafeFileHandle file) {
   var previous=RtlSetThreadPlaceholderCompatibilityMode(2);
   if(previous<0) throw new IOException("Cloud placeholder attributes could not be exposed.");
   try { Tag tag; if(!GetFileInformationByHandleEx(file,9,out tag,8)) throw new Win32Exception(); return tag; }
   finally { RtlSetThreadPlaceholderCompatibilityMode(previous); }
  }
  public static SafeFileHandle Pin(string path,bool directory) {
   var file=CreateFile(path,directory?0x81U:0x80U,directory?3U:1U,IntPtr.Zero,3,0x02200000U,IntPtr.Zero);
   if(file.IsInvalid) { file.Dispose(); throw new Win32Exception(); }
   try {
    var tag=Inspect(file);
    if(((tag.Attributes&0x10)!=0)!=directory || ((tag.Attributes&0x400)!=0 && !IsCloud(tag.Value))) throw new IOException("Unsupported link in distribution source: "+path);
    return file;
   } catch { file.Dispose(); throw; }
  }
  public static bool HasCloud(string path) {
   bool cloud=false;
   while(!String.IsNullOrEmpty(path)) {
    bool directory=Directory.Exists(path);
    using(var file=Pin(path,directory)) { cloud|=IsCloud(Inspect(file).Value); }
    var parent=Directory.GetParent(path); path=parent==null?null:parent.FullName;
   }
   return cloud;
  }
  public static FileStream Read(string path,SafeFileHandle pin) {
   // Normal open requests hydration. Both handles must name one file;
   // the read handle then denies writers while copying and verifying it.
   var file=CreateFile(path,0x80000000U,1,IntPtr.Zero,3,0,IntPtr.Zero);
   if(file.IsInvalid) { file.Dispose(); throw new Win32Exception(); }
   try {
    Info a,b;
    if(!GetFileInformationByHandle(pin,out a)||!GetFileInformationByHandle(file,out b)) throw new Win32Exception();
    if(a.Volume!=b.Volume||a.IndexHigh!=b.IndexHigh||a.IndexLow!=b.IndexLow) throw new IOException("Distribution source changed: "+path);
    return new FileStream(file,FileAccess.Read);
   } catch { file.Dispose(); throw; }
  }
 }
}
'@
}

function Test-MoyaiCloudDistribution([string]$Root) {
  Initialize-MoyaiDistributionNative
  $Root=[IO.Path]::GetFullPath($Root)
  if([MoyaiDistribution.Source]::HasCloud($Root)) { return $true }
  $path=Join-Path $Root 'deployment.json'
  if(-not(Test-Path -LiteralPath $path)) { return $false }
  if([MoyaiDistribution.Source]::HasCloud($path)) { return $true }
  $inventory=Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json
  foreach($file in $inventory.files) {
    $source=Resolve-MoyaiComponentChild $Root $file.path
    if((Test-Path -LiteralPath $source) -and [MoyaiDistribution.Source]::HasCloud($source)) { return $true }
  }
  return $false
}

function New-MoyaiDistributionSource([string]$SourceRoot) {
  Initialize-MoyaiDistributionNative
  $SourceRoot=[IO.Path]::GetFullPath($SourceRoot)
  $source=$SourceRoot
  $cache=Join-Path (Get-MoyaiComponentUserRoot 'Desktop') 'cache/distribution'
  Assert-MoyaiSandboxRegularPath $cache
  [void](New-Item -ItemType Directory -Path $cache -Force)
  $root=Join-Path $cache ([Guid]::NewGuid().ToString('N'))
  $pins=[Collections.Generic.List[object]]::new()
  $directories=[Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
  $created=[Collections.Generic.List[string]]::new()
  $prepared=[pscustomobject]@{root=$root;files=$created;manifest=$null}
  try {
    $identity=[Security.Principal.WindowsIdentity]::GetCurrent()
    try { $sid=$identity.User.Value } finally { $identity.Dispose() }
    $acl=[Security.AccessControl.DirectorySecurity]::new()
    $acl.SetSecurityDescriptorSddlForm(('O:'+$sid+'G:BAD:P(A;OICI;FA;;;'+$sid+')(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)'))
    [void][IO.Directory]::CreateDirectory($root,$acl)
    $entries=@([pscustomobject]@{path='deployment.json';sha256=$null})
    for($index=0;$index -lt $entries.Count;$index++) {
      $entry=$entries[$index]
      $source=Resolve-MoyaiComponentChild $SourceRoot $entry.path
      $directory=[IO.Directory]::GetParent($source)
      $ancestors=@(); while($null -ne $directory) { $ancestors+=@($directory.FullName); $directory=$directory.Parent }
      foreach($ancestor in @($ancestors | Sort-Object Length)) {
        if($directories.Add($ancestor)) { $pins.Add([MoyaiDistribution.Source]::Pin($ancestor,$true)) }
      }
      $pin=[MoyaiDistribution.Source]::Pin($source,$false); $pins.Add($pin)
      $stream=[MoyaiDistribution.Source]::Read($source,$pin); $pins.Add($stream)
      $target=Resolve-MoyaiComponentChild $root $entry.path
      [void](New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force)
      $output=[IO.File]::Open($target,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
      $created.Add($entry.path)
      try {
        if($index -eq 0 -and $stream.Length -gt 8MB) { throw '配布物の一覧が上限を超えています。' }
        $stream.CopyTo($output); $output.Flush($true)
      } finally { $output.Dispose() }
      if($index -eq 0) {
        if((Get-Item -LiteralPath $target).Length -gt 8MB) { throw '配布物の一覧が上限を超えています。' }
        $inventory=Get-Content -LiteralPath $target -Raw -Encoding UTF8 | ConvertFrom-Json
        if($inventory.schema -ne 1 -or $inventory.target -cne 'windows-x86_64' -or $inventory.files.Count -gt 20000) { throw '対応するWindows配布物ではありません。' }
        $seen=@{'deployment.json'=$true}
        foreach($file in $inventory.files) {
          [void](Resolve-MoyaiComponentChild $root $file.path)
          if($seen.ContainsKey($file.path) -or $file.sha256 -cnotmatch '^[a-f0-9]{64}$') { throw '配布物の一覧に重複または不正な照合値があります。' }
          $seen[$file.path]=$true; $entries+=@($file)
        }
      } elseif((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -ine $entry.sha256) {
        throw ('同期中または変更されたファイルです: '+$entry.path)
      }
    }
    $prepared.manifest=Read-MoyaiComponentManifest $root
    return $prepared
  } catch {
    $message=$_.Exception.Message
    try { Remove-MoyaiDistributionSource $prepared } catch { Write-Warning "一時ファイルの削除を完了できませんでした: $root" }
    throw "配布物をローカルへ準備できませんでした。OneDriveの接続・ダウンロード完了を確認し、再実行してください。`n対象: $source`n詳細: $message"
  } finally { for($i=$pins.Count-1;$i -ge 0;$i--) { $pins[$i].Dispose() } }
}

function Test-MoyaiInstalledDesktopMatches($Installed, $Manifest) {
  if($null -eq $Installed -or $Installed.version -cne $Manifest.components.Desktop.version) { return $false }
  $files=@{}; foreach($file in $Installed.files) { $files[$file.path]=$file.sha256 }
  foreach($file in @(Get-MoyaiComponentFiles $Manifest 'Desktop')) {
    if(-not $files.ContainsKey($file.path) -or $files[$file.path] -ine $file.sha256) { return $false }
  }
  return $true
}

function Resolve-MoyaiDesktopDistribution([string]$Root, [switch]$CheckOnly) {
  $prepared=New-MoyaiDistributionSource $Root
  try {
    if($CheckOnly) {
      [void](Test-MoyaiRuntime $prepared.root $prepared.manifest -PreparingInstall)
      return $null
    }
    $installed=Read-MoyaiInstalledComponent 'Desktop'
    if(-not(Test-MoyaiInstalledDesktopMatches $installed $prepared.manifest)) {
      Invoke-MoyaiComponentUserSetup -SourceRoot $prepared.root -Components @('Desktop') -SkipStartupCheck | Out-Host
      $installed=Read-MoyaiInstalledComponent 'Desktop'
      if(-not(Test-MoyaiInstalledDesktopMatches $installed $prepared.manifest)) { throw 'Desktopの導入結果を確認できません。Setup-moyAI.cmdを実行してください。' }
    }
    return $installed.install_root
  } finally { Remove-MoyaiDistributionSource $prepared }
}

function Remove-MoyaiDistributionSource($Prepared) {
  if($null -eq $Prepared -or -not(Test-Path -LiteralPath $Prepared.root)) { return }
  Assert-MoyaiSandboxRegularPath $Prepared.root
  $directories=@{}
  foreach($relative in $Prepared.files) {
    $path=Resolve-MoyaiComponentChild $Prepared.root $relative
    Assert-MoyaiSandboxRegularPath $path
    if(Test-Path -LiteralPath $path -PathType Leaf) { Remove-Item -LiteralPath $path }
    $parent=Split-Path -Parent $path
    while($parent.Length -gt $Prepared.root.Length) { $directories[$parent]=$true; $parent=Split-Path -Parent $parent }
  }
  foreach($path in @($directories.Keys | Sort-Object Length -Descending)+@($Prepared.root)) {
    Assert-MoyaiSandboxRegularPath $path
    if((Test-Path -LiteralPath $path) -and @(Get-ChildItem -LiteralPath $path -Force).Count -eq 0) { Remove-Item -LiteralPath $path }
  }
}
