param([string]$EvidenceRoot='')
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'deployment/component-install.ps1')
. (Join-Path $PSScriptRoot 'deployment/distribution-source.ps1')
if(-not $EvidenceRoot) { $EvidenceRoot=Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) ('project_sandbox/cloud-distribution-'+[Guid]::NewGuid().ToString('N')) }
$EvidenceRoot=[IO.Path]::GetFullPath($EvidenceRoot)
$root=Join-Path $EvidenceRoot 'cloud-input'
if(Test-Path -LiteralPath $root) { throw 'Use a new evidence directory; existing sync roots are never adopted.' }
[void](New-Item -ItemType Directory -Path (Join-Path $root 'scripts'),(Join-Path $root 'app/bin'))
function Get-MoyaiComponentUserRoot([string]$Component) { Join-Path $EvidenceRoot 'ordinary-local/desktop' }
# Native Windows API fixture, with no provider account, network, SCM or user-data changes.
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
public static class MoyaiCloudFixture {
 [StructLayout(LayoutKind.Sequential,CharSet=CharSet.Unicode)] struct Registration {
  public uint Size; public string Name, Version; public IntPtr RootIdentity; public uint RootLength; public IntPtr FileIdentity; public uint FileLength; public Guid Provider;
 }
 [StructLayout(LayoutKind.Sequential)] struct Policies { public uint Size; public ushort Hydration, HydrationModifier, Population, PopulationModifier; public uint InSync, HardLink, Management; }
 [DllImport("cldapi.dll",CharSet=CharSet.Unicode)] static extern int CfRegisterSyncRoot(string path,ref Registration registration,ref Policies policies,uint flags);
 [DllImport("cldapi.dll",CharSet=CharSet.Unicode)] public static extern int CfUnregisterSyncRoot(string path);
 [DllImport("cldapi.dll")] static extern int CfConvertToPlaceholder(SafeFileHandle file,byte[] identity,uint length,uint flags,IntPtr usn,IntPtr overlapped);
 public static void Register(string path) {
  var r=new Registration{Size=(uint)Marshal.SizeOf(typeof(Registration)),Name="moyAI isolated file test",Version="1",Provider=Guid.NewGuid()};
  var p=new Policies{Size=(uint)Marshal.SizeOf(typeof(Policies)),Hydration=2,Population=3};
  Marshal.ThrowExceptionForHR(CfRegisterSyncRoot(path,ref r,ref p,6));
 }
 public static void Convert(string path,bool offline) {
  using(var f=new FileStream(path,FileMode.Open,FileAccess.ReadWrite,FileShare.None)) { Marshal.ThrowExceptionForHR(CfConvertToPlaceholder(f.SafeFileHandle,new byte[]{1},1,offline?3U:1U,IntPtr.Zero,IntPtr.Zero)); }
 }
}
'@
$checks=[Collections.Generic.List[string]]::new()
function Check([bool]$condition,[string]$message) { if(-not $condition) { throw "FAIL: $message" }; $checks.Add($message) }
foreach($relative in @('scripts/component-install.ps1','scripts/Setup-Component.ps1','scripts/sandbox-service.ps1','app/bin/moyai-desktop.exe')) { [IO.File]::WriteAllText((Join-Path $root $relative),'inert cloud fixture '+$relative) }
$files=@(Get-ChildItem -LiteralPath $root -Recurse -File | ForEach-Object { @{path=$_.FullName.Substring($root.Length+1).Replace('\','/');component='Desktop';sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()} })
Write-MoyaiComponentJson (Join-Path $root 'deployment.json') @{schema=1;product='moyai-windows-user';target='windows-x86_64';components=@{Desktop=@{version='3.0.6'}};files=$files}
[MoyaiCloudFixture]::Register($root)
try {
  foreach($file in @(Get-ChildItem -LiteralPath $root -Recurse -File)) { [MoyaiCloudFixture]::Convert($file.FullName,$false) }
  Check (Test-MoyaiCloudDistribution $root) 'Real Windows cloud placeholders are recognized'
  $blocked=$false; try { Read-MoyaiComponentManifest $root | Out-Null } catch { $blocked=$true }
  Check $blocked 'The original strict installer rejects cloud payload directly'
  $prepared=New-MoyaiDistributionSource $root
  try {
    $manifest=Read-MoyaiComponentManifest $prepared.root
    Check ($manifest.files.Count -eq $files.Count) 'The new preparation passes the same strict installer validation'
    Check (-not(Test-MoyaiCloudDistribution $prepared.root)) 'Executable payload in staging is ordinary local data'
  } finally { Remove-MoyaiDistributionSource $prepared }
  $offline=Join-Path $root 'unavailable.bin'; [IO.File]::WriteAllText($offline,'not downloadable without a provider')
  $file=@{path='unavailable.bin';component='Desktop';sha256=(Get-FileHash -LiteralPath $offline -Algorithm SHA256).Hash.ToLowerInvariant()}
  Write-MoyaiComponentJson (Join-Path $root 'deployment.json') @{schema=1;product='moyai-windows-user';target='windows-x86_64';components=@{Desktop=@{version='3.0.6'}};files=@($files)+@($file)}
  [MoyaiCloudFixture]::Convert($offline,$true)
  $message=''; try { New-MoyaiDistributionSource $root | Out-Null } catch { $message=$_.Exception.Message }
  Check ($message.Contains('unavailable.bin') -and $message.Contains('ダウンロード完了')) 'Unavailable cloud content reports its path and download guidance'
  Check (@(Get-ChildItem -LiteralPath (Join-Path (Get-MoyaiComponentUserRoot Desktop) 'cache/distribution')).Count -eq 0) 'Cloud copy failure leaves no staged executable'
  Remove-Item -LiteralPath (Join-Path $root 'app/bin/moyai-desktop.exe')
  $removal=New-MoyaiDistributionSource $root -ForUninstall
  try {
    Check ($removal.manifest.uninstall_only -and $removal.manifest.files.Count -eq 3) 'Cloud removal needs only verified control scripts despite missing and unavailable application payload'
    Check (-not(Test-MoyaiCloudDistribution $removal.root)) 'Cloud removal control files are copied to ordinary local files'
  } finally { Remove-MoyaiDistributionSource $removal }
  Check (@(Get-ChildItem -LiteralPath (Join-Path (Get-MoyaiComponentUserRoot Desktop) 'cache/distribution')).Count -eq 0) 'Cloud removal releases its local staging'
} finally { [Runtime.InteropServices.Marshal]::ThrowExceptionForHR([MoyaiCloudFixture]::CfUnregisterSyncRoot($root)) }
$checks | ForEach-Object { "PASS: $_" }
Write-MoyaiComponentJson (Join-Path $EvidenceRoot 'results.json') @{checks=$checks;windows_cloud_files=$true;real_onedrive_sync=$false;sync_root_unregistered=$true}
