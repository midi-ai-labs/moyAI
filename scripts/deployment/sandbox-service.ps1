# PC-wide sandbox installation. Merely loading this file never changes the machine.
function Get-MoyaiSandboxLocation {
    if (-not [Environment]::Is64BitOperatingSystem -or -not [Environment]::Is64BitProcess) { throw 'Sandbox setup requires 64-bit Windows PowerShell.' }
    $programFiles = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles)
    if (-not [IO.Path]::IsPathRooted($programFiles)) { throw 'Windows Program Files could not be resolved.' }
    $parent = Join-Path $programFiles 'moyAI'
    $root = Join-Path $parent 'SandboxService'
    [pscustomobject]@{ parent=$parent; root=$root; helper=(Join-Path $root 'moyai-sandbox.exe'); service='MoyAISandbox' }
}

function Get-MoyaiSandboxServiceSddl { 'O:BAG:BAD:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x15;;;IU)' }

function Assert-MoyaiSandboxRegularPath([string]$Path) {
    $current = [IO.Path]::GetFullPath($Path)
    while ($current) {
        if (Test-Path -LiteralPath $current) {
            if (((Get-Item -LiteralPath $current -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw "Sandbox path contains a reparse point: $current" }
        }
        $parent = [IO.Directory]::GetParent($current)
        $current = if ($null -eq $parent) { $null } else { $parent.FullName }
    }
}

function Assert-MoyaiSandboxProtectedPath([string]$Path) {
    Assert-MoyaiSandboxRegularPath $Path
    $acl = Get-Acl -LiteralPath $Path
    $owner = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
    if ($owner -notin @('S-1-5-18','S-1-5-32-544')) { throw "Sandbox path has an unexpected owner: $Path" }
    foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
        if ($rule.AccessControlType -ne [Security.AccessControl.AccessControlType]::Allow) { continue }
        if ($rule.IdentityReference.Value -in @('S-1-5-18','S-1-5-32-544')) { continue }
        if (([long]$rule.FileSystemRights -band 0x500d0156) -ne 0) { throw "Sandbox path is writable by an untrusted identity: $Path" }
    }
}

function Set-MoyaiSandboxProtection([string]$Path, [bool]$Directory, [switch]$Private) {
    $acl = if ($Directory) { [Security.AccessControl.DirectorySecurity]::new() } else { [Security.AccessControl.FileSecurity]::new() }
    $inheritance = if ($Directory) { 'OICI' } else { '' }
    $sddl = 'O:BAG:BAD:P(A;' + $inheritance + ';FA;;;SY)(A;' + $inheritance + ';FA;;;BA)'
    if (-not $Private) { $sddl += '(A;' + $inheritance + ';FRFX;;;IU)' }
    $acl.SetSecurityDescriptorSddlForm($sddl)
    Set-Acl -LiteralPath $Path -AclObject $acl
    Assert-MoyaiSandboxProtectedPath $Path
}

function Set-MoyaiSandboxParentAccess([string]$Path) {
    Assert-MoyaiSandboxProtectedPath $Path
    $acl = Get-Acl -LiteralPath $Path
    $interactive = [Security.Principal.SecurityIdentifier]::new('S-1-5-4')
    # Existing private legacy children keep their own permissions. Only this parent
    # gains traversal/read access; SandboxService receives its separate protected ACL.
    $rule = [Security.AccessControl.FileSystemAccessRule]::new($interactive, [Security.AccessControl.FileSystemRights]::ReadAndExecute, [Security.AccessControl.InheritanceFlags]::None, [Security.AccessControl.PropagationFlags]::None, [Security.AccessControl.AccessControlType]::Allow)
    [void]$acl.AddAccessRule($rule)
    Set-Acl -LiteralPath $Path -AclObject $acl
    Assert-MoyaiSandboxProtectedPath $Path
}

function Initialize-MoyaiSandboxNative {
    if ('MoyaiSandboxSetup.Native' -as [type]) { return }
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
namespace MoyaiSandboxSetup {
 public sealed class ServiceState {
  public uint Type, Start, ErrorControl, State;
  public string Command, Account, Dependencies, Sddl;
 }
 public static class Native {
  [StructLayout(LayoutKind.Sequential)] struct FileInformation {
   public uint attributes;
   public System.Runtime.InteropServices.ComTypes.FILETIME created, accessed, written;
   public uint volume, sizeHigh, sizeLow, links, indexHigh, indexLow;
  }
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern SafeFileHandle CreateFile(string name,uint access,uint sharing,IntPtr security,uint disposition,uint flags,IntPtr template);
  [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetFileInformationByHandle(SafeFileHandle file,out FileInformation info);
  static SafeFileHandle Pin(string path,bool directory) {
   // LIST_DIRECTORY participates in share-delete checks; attributes alone do not.
   var handle=CreateFile(path,directory?0x81U:0x80000000U,directory?3U:1U,IntPtr.Zero,3,0x02200000,IntPtr.Zero);
   if(handle.IsInvalid) { handle.Dispose(); throw Error(); }
   try {
    FileInformation info; if(!GetFileInformationByHandle(handle,out info)) throw Error();
    if((info.attributes&0x400)!=0 || ((info.attributes&0x10)!=0)!=directory) throw new IOException("Unexpected source file type or reparse point.");
    return handle;
   } catch { handle.Dispose(); throw; }
  }
  public static SafeFileHandle PinDirectory(string path) { return Pin(path,true); }
  public static FileStream PinFile(string path) { return new FileStream(Pin(path,false),FileAccess.Read); }
  [StructLayout(LayoutKind.Sequential)] struct Config {
   public uint type, start, error; public IntPtr command, group; public uint tag;
   public IntPtr dependencies, account, display;
  }
  [StructLayout(LayoutKind.Sequential)] struct Status {
   public uint type, state, accepted, win32exit, specificexit, checkpoint, waitHint;
  }
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr OpenSCManager(string machine,string database,uint access);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr OpenService(IntPtr manager,string name,uint access);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool CloseServiceHandle(IntPtr handle);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool QueryServiceConfig(IntPtr service,IntPtr config,uint size,out uint needed);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool QueryServiceStatus(IntPtr service,out Status status);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr CreateService(IntPtr manager,string name,string display,uint access,uint type,uint start,uint error,string command,string group,IntPtr tag,string deps,string account,string password);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool ChangeServiceConfig(IntPtr service,uint type,uint start,uint error,string command,string group,IntPtr tag,string deps,string account,string password,string display);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool DeleteService(IntPtr service);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool QueryServiceObjectSecurity(IntPtr service,uint info,byte[] buffer,uint size,out uint needed);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool SetServiceObjectSecurity(IntPtr service,uint info,IntPtr descriptor);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool ConvertStringSecurityDescriptorToSecurityDescriptor(string text,uint revision,out IntPtr descriptor,out uint size);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool ConvertSecurityDescriptorToStringSecurityDescriptor(byte[] descriptor,uint revision,uint info,out IntPtr text,out uint size);
  [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr value);
  static Exception Error() { return new Win32Exception(Marshal.GetLastWin32Error()); }
  static IntPtr Manager(uint access) { var h=OpenSCManager(null,null,access); if(h==IntPtr.Zero) throw Error(); return h; }
  static IntPtr Service(IntPtr manager,uint access,bool optional) {
   var h=OpenService(manager,"MoyAISandbox",access); if(h==IntPtr.Zero && !(optional && Marshal.GetLastWin32Error()==1060)) throw Error(); return h;
  }
  public static ServiceState Read(bool security) {
   var m=Manager(1); IntPtr h=IntPtr.Zero, b=IntPtr.Zero;
   try {
    h=Service(m,security?0x20005U:5U,true); if(h==IntPtr.Zero) return null;
    uint n; QueryServiceConfig(h,IntPtr.Zero,0,out n); if(n==0 || n>65536) throw Error();
    b=Marshal.AllocHGlobal((int)n); if(!QueryServiceConfig(h,b,n,out n)) throw Error();
    var c=(Config)Marshal.PtrToStructure(b,typeof(Config)); Status s; if(!QueryServiceStatus(h,out s)) throw Error();
    var result=new ServiceState { Type=c.type, Start=c.start, ErrorControl=c.error, State=s.state,
      Command=Marshal.PtrToStringUni(c.command), Account=Marshal.PtrToStringUni(c.account), Dependencies=Marshal.PtrToStringUni(c.dependencies) };
    if(security) {
     QueryServiceObjectSecurity(h,7,null,0,out n); if(n==0 || n>65536) throw Error();
     var bytes=new byte[n]; if(!QueryServiceObjectSecurity(h,7,bytes,n,out n)) throw Error();
     IntPtr text; if(!ConvertSecurityDescriptorToStringSecurityDescriptor(bytes,1,7,out text,out n)) throw Error();
     try { result.Sddl=Marshal.PtrToStringUni(text); } finally { LocalFree(text); }
    }
    return result;
   } finally { if(b!=IntPtr.Zero) Marshal.FreeHGlobal(b); if(h!=IntPtr.Zero) CloseServiceHandle(h); CloseServiceHandle(m); }
  }
  public static void Configure(string command,uint start,bool create) {
   var m=Manager(create?3U:1U); IntPtr h=IntPtr.Zero;
   try {
    if(create) { h=CreateService(m,"MoyAISandbox","moyAI Sandbox",0xf01ff,0x10,start,1,command,null,IntPtr.Zero,"\0","LocalSystem",null); if(h==IntPtr.Zero) throw Error(); }
    else { h=Service(m,2,false); if(!ChangeServiceConfig(h,0x10,start,1,command,"",IntPtr.Zero,"\0","LocalSystem",null,"moyAI Sandbox")) throw Error(); }
   } finally { if(h!=IntPtr.Zero) CloseServiceHandle(h); CloseServiceHandle(m); }
  }
  public static void Security(string sddl) {
   var m=Manager(1); IntPtr h=IntPtr.Zero, d=IntPtr.Zero;
   try { h=Service(m,0xc0000,false); uint n; if(!ConvertStringSecurityDescriptorToSecurityDescriptor(sddl,1,out d,out n)) throw Error(); if(!SetServiceObjectSecurity(h,7,d)) throw Error(); }
   finally { if(d!=IntPtr.Zero) LocalFree(d); if(h!=IntPtr.Zero) CloseServiceHandle(h); CloseServiceHandle(m); }
  }
  public static void Remove() {
   var m=Manager(1); IntPtr h=IntPtr.Zero;
   try { h=Service(m,0x10000,false); if(!DeleteService(h)) throw Error(); }
   finally { if(h!=IntPtr.Zero) CloseServiceHandle(h); CloseServiceHandle(m); }
  }
 }
}
'@
}

function Get-MoyaiSandboxService([switch]$Security) { Initialize-MoyaiSandboxNative; [MoyaiSandboxSetup.Native]::Read([bool]$Security) }
function Set-MoyaiSandboxService([string]$Command, [uint32]$Start, [switch]$Create) { Initialize-MoyaiSandboxNative; [MoyaiSandboxSetup.Native]::Configure($Command, $Start, [bool]$Create) }
function Set-MoyaiSandboxServiceSecurity([string]$Sddl) { Initialize-MoyaiSandboxNative; [MoyaiSandboxSetup.Native]::Security($Sddl) }
function Remove-MoyaiSandboxService { Initialize-MoyaiSandboxNative; [MoyaiSandboxSetup.Native]::Remove() }

function Close-MoyaiSandboxSourcePins($Pins) {
    if ($null -eq $Pins) { return }
    for ($i=$Pins.handles.Count-1; $i -ge 0; $i--) { $Pins.handles[$i].Dispose() }
}

function New-MoyaiSandboxSourcePins([string]$SourceRoot, $Manifest) {
    if ($Manifest.PSObject.Properties.Name -notcontains 'validated_manifest_sha256' -or $Manifest.validated_manifest_sha256 -cnotmatch '^[a-f0-9]{64}$') { throw 'Sandbox source requires the validated inventory snapshot.' }
    $required = @('scripts/Setup-Sandbox.ps1','scripts/common.ps1','scripts/runtime-access.ps1','scripts/sandbox-service.ps1')
    $payload = @(Get-MoyaiSandboxPayload $SourceRoot $Manifest)
    $entries = [Collections.Generic.List[object]]::new()
    $entries.Add([pscustomobject]@{relative='deployment.json';path=(Join-Path $SourceRoot 'deployment.json');sha256=$Manifest.validated_manifest_sha256})
    foreach ($relative in $required) {
        $found = @($Manifest.files | Where-Object { $_.path -ceq $relative })
        if ($found.Count -ne 1) { throw 'Sandbox management scripts are missing from the package inventory.' }
        $entries.Add([pscustomobject]@{relative=$relative;path=(Resolve-MoyaiChild $SourceRoot $relative);sha256=$found[0].sha256})
    }
    foreach ($file in $payload) { $entries.Add([pscustomobject]@{relative=('app/bin/'+$file.name);path=$file.source;sha256=$file.sha256}) }
    $directories = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($entry in $entries) {
        $directory = [IO.Directory]::GetParent($entry.path)
        while ($null -ne $directory) { [void]$directories.Add($directory.FullName); $directory=$directory.Parent }
    }
    Initialize-MoyaiSandboxNative
    $pins = [pscustomobject]@{handles=[Collections.Generic.List[object]]::new();files=@{};manifest_sha256=$Manifest.validated_manifest_sha256}
    try {
        foreach ($directory in @($directories | Sort-Object Length)) { $pins.handles.Add([MoyaiSandboxSetup.Native]::PinDirectory($directory)) }
        foreach ($entry in $entries) {
            $stream = [MoyaiSandboxSetup.Native]::PinFile($entry.path)
            $pins.handles.Add($stream)
            $hasher = [Security.Cryptography.SHA256]::Create()
            try { $actual=([BitConverter]::ToString($hasher.ComputeHash($stream))).Replace('-','').ToLowerInvariant() } finally { $hasher.Dispose(); $stream.Position=0 }
            if ($actual -cne $entry.sha256) { throw 'A sandbox setup source changed after its inventory was validated.' }
            $pins.files[$entry.relative]=$stream
        }
        return $pins
    } catch { Close-MoyaiSandboxSourcePins $pins; throw }
}

function Copy-MoyaiSandboxPinnedPayload($Stream, [string]$Target) {
    # Create with a private descriptor atomically; an elevated copy must not expose
    # bytes to interactive users before their expected hash has been verified.
    $acl=[Security.AccessControl.FileSecurity]::new()
    $acl.SetSecurityDescriptorSddlForm('O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)')
    $output=[IO.FileStream]::new($Target,[IO.FileMode]::CreateNew,[Security.AccessControl.FileSystemRights]::Write,[IO.FileShare]::None,4096,[IO.FileOptions]::None,$acl)
    try { $Stream.Position=0; $Stream.CopyTo($output); $output.Flush($true) } finally { $output.Dispose(); $Stream.Position=0 }
}

function Assert-MoyaiSandboxService($Service, $Location, [switch]$Disabled, [switch]$Security) {
    $start = if ($Disabled) { 4 } else { 3 }
    if ($null -eq $Service -or $Service.Type -ne 0x10 -or $Service.Start -ne $start -or $Service.ErrorControl -ne 1 -or $Service.Account -cne 'LocalSystem' -or $Service.Command -cne ('"' + $Location.helper + '" --service') -or $Service.Dependencies) {
        throw 'MoyAISandbox has an unexpected service configuration. It was not replaced.'
    }
    if ($Security) {
        $actual = [Security.AccessControl.RawSecurityDescriptor]::new($Service.Sddl)
        if ($null -eq $actual.Owner -or $actual.Owner.Value -notin @('S-1-5-18','S-1-5-32-544')) { throw 'MoyAISandbox service has an unexpected owner.' }
        if ($null -eq $actual.DiscretionaryAcl -or $actual.DiscretionaryAcl.Count -ne 3) { throw 'MoyAISandbox service permissions differ from its installed configuration.' }
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($ace in $actual.DiscretionaryAcl) {
            if ($ace -isnot [Security.AccessControl.CommonAce] -or $ace.IsCallback -or $ace.AceQualifier -ne [Security.AccessControl.AceQualifier]::AccessAllowed -or [int]$ace.AceFlags -ne 0) { throw 'Unexpected sandbox service permission entry.' }
            $sid = $ace.SecurityIdentifier.Value
            if (-not $seen.Add($sid)) { throw 'Duplicate sandbox service permission entry.' }
            # SCM may expand GENERIC_ALL to SERVICE_ALL_ACCESS when storing the DACL.
            $allowed = if ($sid -in @('S-1-5-18','S-1-5-32-544')) { $ace.AccessMask -in @(0x10000000,0x000f01ff) } elseif ($sid -ceq 'S-1-5-4') { $ace.AccessMask -eq 0x15 } else { $false }
            if (-not $allowed) { throw 'MoyAISandbox service permissions differ from its installed configuration.' }
        }
    }
}

function Get-MoyaiSandboxPayload([string]$SourceRoot, $Manifest) {
    $files = @($Manifest.files | Where-Object { $_.path -ceq 'app/bin/moyai-sandbox.exe' -or ($_.path.StartsWith('app/bin/', [StringComparison]::Ordinal) -and [IO.Path]::GetExtension($_.path) -ieq '.dll') })
    $dlls = @($files | Where-Object { $_.path -cne 'app/bin/moyai-sandbox.exe' })
    if (@($files | Where-Object { $_.path -ceq 'app/bin/moyai-sandbox.exe' }).Count -ne 1) { throw 'Sandbox helper is missing from the package inventory.' }
    if ($Manifest.runtime.visual_cpp -eq 'app-local') {
        if ($dlls.Count -ne 3) { throw 'Sandbox runtime must contain exactly its three declared Visual C++ DLLs.' }
        foreach ($name in @('vcruntime140.dll','vcruntime140_1.dll','msvcp140.dll')) {
            if (@($dlls | Where-Object { [IO.Path]::GetFileName($_.path) -ieq $name }).Count -ne 1) { throw "Sandbox runtime set is incomplete: $name" }
        }
    } elseif ($Manifest.runtime.visual_cpp -ne 'system-installed' -or $dlls.Count -ne 0) { throw 'Sandbox runtime inventory does not match its runtime mode.' }
    $actualDlls = @(Get-ChildItem -LiteralPath (Join-Path $SourceRoot 'app/bin') -File | Where-Object { $_.Extension -ieq '.dll' })
    if ($actualDlls.Count -ne $dlls.Count) { throw 'Unlisted sandbox runtime DLLs were found in the package.' }
    $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $result = foreach ($file in $files) {
        $name = [IO.Path]::GetFileName($file.path)
        if ($file.path -cne ('app/bin/' + $name) -or -not $names.Add($name)) { throw 'Sandbox payload paths must be unique direct children of app/bin.' }
        $path = Resolve-MoyaiChild $SourceRoot $file.path
        Assert-MoyaiSandboxRegularPath $path
        if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -cne $file.sha256) { throw 'Sandbox source payload changed during validation.' }
        [pscustomobject]@{ name=$name; sha256=$file.sha256; source=$path }
    }
    return @($result | Sort-Object name)
}

function Read-MoyaiSandboxInstallation($Location) {
    Assert-MoyaiSandboxProtectedPath $Location.parent
    Assert-MoyaiSandboxProtectedPath $Location.root
    $receiptPath = Join-Path $Location.root 'installation.json'
    Assert-MoyaiSandboxProtectedPath $receiptPath
    $receipt = Get-Content -LiteralPath $receiptPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($receipt.schema -ne 1 -or $receipt.product -cne 'moyai-machine-sandbox' -or $receipt.service -cne $Location.service -or $receipt.install_root -cne $Location.root) { throw 'Sandbox installation ownership does not match its fixed location.' }
    $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    [void]$names.Add('installation.json')
    foreach ($file in @($receipt.files)) {
        if ($file.name -cne [IO.Path]::GetFileName($file.name) -or $file.name -notmatch '^[A-Za-z0-9_.-]+\.(exe|dll)$' -or -not $names.Add($file.name) -or $file.sha256 -cnotmatch '^[a-f0-9]{64}$') { throw 'Invalid installed sandbox payload inventory.' }
        if ($file.name -ine 'moyai-sandbox.exe' -and [IO.Path]::GetExtension($file.name) -ine '.dll') { throw 'Unexpected executable in sandbox inventory.' }
        $path = Join-Path $Location.root $file.name
        Assert-MoyaiSandboxProtectedPath $path
        if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -cne $file.sha256) { throw 'Installed sandbox payload differs from its protected inventory.' }
    }
    if (-not $names.Contains('moyai-sandbox.exe')) { throw 'Installed sandbox helper is missing.' }
    if ($receipt.visual_cpp -eq 'app-local') {
        if ($names.Count -ne 5) { throw 'Installed sandbox runtime contains an unexpected DLL.' }
        foreach ($name in @('vcruntime140.dll','vcruntime140_1.dll','msvcp140.dll')) { if (-not $names.Contains($name)) { throw 'Installed sandbox runtime is incomplete.' } }
    } elseif ($receipt.visual_cpp -ne 'system-installed' -or $names.Count -ne 2) { throw 'Installed sandbox runtime mode is inconsistent.' }
    $entries = @(Get-ChildItem -LiteralPath $Location.root -Force)
    if ($entries.Count -ne $names.Count) { throw 'Sandbox directory contains unowned entries; no files were removed.' }
    foreach ($entry in $entries) { if ($entry.PSIsContainer -or -not $names.Contains($entry.Name)) { throw 'Sandbox directory contains an unexpected entry.' }; Assert-MoyaiSandboxProtectedPath $entry.FullName }
    return $receipt
}

function Test-MoyaiSandboxPayloadMatch($Receipt, $Payload, [string]$RuntimeMode) {
    if ($Receipt.visual_cpp -cne $RuntimeMode -or @($Receipt.files).Count -ne @($Payload).Count) { return $false }
    foreach ($file in $Payload) {
        if (@($Receipt.files | Where-Object { $_.name -ceq $file.name -and $_.sha256 -ceq $file.sha256 }).Count -ne 1) { return $false }
    }
    return $true
}

function Get-MoyaiSandboxInstallPlan([string]$SourceRoot, $Manifest) {
    $location = Get-MoyaiSandboxLocation
    Assert-MoyaiSandboxRegularPath $location.root
    $payload = @(Get-MoyaiSandboxPayload $SourceRoot $Manifest)
    $service = Get-MoyaiSandboxService
    $exists = Test-Path -LiteralPath $location.root
    if (($null -ne $service) -ne $exists) { throw 'Sandbox service and protected installation are inconsistent. Ask an administrator to inspect them.' }
    if ($exists) {
        Assert-MoyaiSandboxService $service $location
        $receipt = Read-MoyaiSandboxInstallation $location
        $needed = -not (Test-MoyaiSandboxPayloadMatch $receipt $payload $Manifest.runtime.visual_cpp)
        if ($needed -and $service.State -ne 1) { throw 'Sandbox helper is in use. Close all moyAI Desktop and Runner instances before updating; setup will not force them to stop.' }
    } else { $needed = $true }
    [pscustomobject]@{ needed=$needed; location=$location; payload=$payload }
}

function ConvertTo-MoyaiSetupArgument([string]$Value) {
    foreach ($character in $Value.ToCharArray()) {
        if ([int]$character -lt 32 -or [int]$character -eq 127 -or $character -eq '"') { throw 'Setup paths cannot contain quotes or control characters.' }
    }
    # Windows command-line quoting doubles backslashes only before the closing quote.
    $trailing = 0
    for ($i=$Value.Length-1; $i -ge 0 -and $Value[$i] -eq '\'; $i--) { $trailing++ }
    return '"' + $Value + ('\' * $trailing) + '"'
}

function Invoke-MoyaiSandboxElevation([string]$SourceRoot, $Manifest) {
    $scriptPath = Join-Path $SourceRoot 'scripts/Setup-Sandbox.ps1'
    # This is a fixed -File invocation, not PowerShell source. Bypass is process-local,
    # matching Setup-moyAI.cmd; it never changes the user's execution-policy setting.
    $arguments = '-NoProfile -ExecutionPolicy Bypass -File ' + (ConvertTo-MoyaiSetupArgument $scriptPath) + ' -SourceRoot ' + (ConvertTo-MoyaiSetupArgument $SourceRoot) + ' -ExpectedManifestSha256 ' + $Manifest.validated_manifest_sha256
    $powershell = Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell/v1.0/powershell.exe'
    $pins = New-MoyaiSandboxSourcePins $SourceRoot $Manifest
    try {
        try { $process = Start-Process -FilePath $powershell -ArgumentList $arguments -Verb RunAs -WindowStyle Hidden -PassThru -Wait }
        catch {
            $exception=$_.Exception
            $baseException=$exception.GetBaseException()
            $detail=$exception.Message
            if($baseException.Message -cne $detail){$detail += ' Base exception: ' + $baseException.Message}
            if($baseException -is [ComponentModel.Win32Exception]){$detail += ' NativeErrorCode=' + $baseException.NativeErrorCode}
            throw ('Sandbox setup was not approved or could not start. User application installation has not begun. ' + $detail)
        }
        if ($process.ExitCode -ne 0) { throw 'Administrator sandbox setup did not complete. User application installation has not begun; close all Desktop/Runner instances and inspect the setup error.' }
    } finally { Close-MoyaiSandboxSourcePins $pins }
}

function Ensure-MoyaiSandboxInstallation([string]$SourceRoot, $Manifest, [switch]$CheckOnly) {
    $plan = Get-MoyaiSandboxInstallPlan $SourceRoot $Manifest
    if (-not $plan.needed) { return }
    if ($CheckOnly) { Write-Output 'Sandbox helper の初回導入または更新には管理者承認が必要です。確認のみのため変更しません。'; return }
    Invoke-MoyaiSandboxElevation $SourceRoot $Manifest
    if ((Get-MoyaiSandboxInstallPlan $SourceRoot $Manifest).needed) { throw 'Sandbox installation did not produce the required protected helper.' }
}

function Assert-MoyaiSandboxAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    try { if (-not [Security.Principal.WindowsPrincipal]::new($identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run Setup-Sandbox.ps1 as an administrator.' } } finally { $identity.Dispose() }
}

function Assert-MoyaiUserSetup {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    try {
        if ([Security.Principal.WindowsPrincipal]::new($identity).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Setup-moyAI.cmd は通常の権限で開いてください。PC共通helperの管理者承認だけを必要時に別画面で行います。' }
    } finally { $identity.Dispose() }
}

function Remove-MoyaiSandboxPayloadDirectory([string]$Path, $Files) {
    Assert-MoyaiSandboxProtectedPath $Path
    $expected = @('installation.json') + @($Files | ForEach-Object { $_.name })
    $entries = @(Get-ChildItem -LiteralPath $Path -Force)
    if ($entries.Count -ne $expected.Count) { throw 'Sandbox cleanup found an unexpected file. The directory was retained.' }
    foreach ($entry in $entries) { if ($entry.PSIsContainer -or $entry.Name -notin $expected) { throw 'Sandbox cleanup found an unowned entry.' }; Assert-MoyaiSandboxProtectedPath $entry.FullName }
    foreach ($entry in $entries) { Remove-Item -LiteralPath $entry.FullName }
    Remove-Item -LiteralPath $Path
}

function Invoke-MoyaiMachineSandboxSetup([string]$SourceRoot, [switch]$Uninstall, [string]$ExpectedManifestSha256 = '') {
    if ($PSVersionTable.PSEdition -ne 'Desktop' -or $PSVersionTable.PSVersion.Major -ne 5 -or $PSVersionTable.PSVersion.Minor -ne 1) { throw 'Run machine sandbox setup with 64-bit Windows PowerShell 5.1 (powershell.exe), not PowerShell 7 (pwsh.exe).' }
    Assert-MoyaiSandboxAdministrator
    $location = Get-MoyaiSandboxLocation
    Assert-MoyaiSandboxRegularPath $location.root
    $manifest = $null; $payload = @()
    if (-not $Uninstall) {
        $manifestPath = Join-Path $SourceRoot 'deployment.json'
        Assert-MoyaiSandboxRegularPath $manifestPath
        $manifest = Read-MoyaiDeployment $SourceRoot -VerifyHashes -ExpectedSha256 $ExpectedManifestSha256
        if ($manifest.PSObject.Properties.Name -notcontains 'machine_sandbox' -or $manifest.machine_sandbox.service -cne 'MoyAISandbox' -or $manifest.machine_sandbox.setup -cne 'scripts/Setup-Sandbox.ps1') { throw 'This package does not declare the installed sandbox service contract.' }
        $payload = @(Get-MoyaiSandboxPayload $SourceRoot $manifest)
    }
    $pins = $null
    if (-not $Uninstall) { $pins = New-MoyaiSandboxSourcePins $SourceRoot $manifest }
    try {
    if (-not (Test-Path -LiteralPath $location.parent)) {
        if ($Uninstall) { throw 'No machine sandbox installation exists.' }
        [void](New-Item -ItemType Directory -Path $location.parent)
        Set-MoyaiSandboxProtection $location.parent $true
    }
    Assert-MoyaiSandboxProtectedPath $location.parent
    $lockPath = Join-Path $location.parent 'sandbox-setup.lock'
    Assert-MoyaiSandboxRegularPath $lockPath
    $lock = [IO.File]::Open($lockPath,[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
    try {
        $service = Get-MoyaiSandboxService -Security
        $exists = Test-Path -LiteralPath $location.root
        if (($null -ne $service) -ne $exists) { throw 'Sandbox service and protected installation are inconsistent; no replacement was performed.' }
        $old = $null
        if ($exists) {
            Assert-MoyaiSandboxService $service $location -Security
            $old = Read-MoyaiSandboxInstallation $location
            if (-not $Uninstall -and (Test-MoyaiSandboxPayloadMatch $old $payload $manifest.runtime.visual_cpp)) { return }
            if ($service.State -ne 1) { throw 'Close all moyAI Desktop and Runner instances before updating/removing the sandbox helper. No process will be forced to stop.' }
        } elseif ($Uninstall) { throw 'No machine sandbox installation exists.' }
        Set-MoyaiSandboxParentAccess $location.parent
        $command = '"' + $location.helper + '" --service'
        $stage = Join-Path $location.parent ('SandboxService.stage-' + [Guid]::NewGuid().ToString('N'))
        $backup = Join-Path $location.parent ('SandboxService.previous-' + [Guid]::NewGuid().ToString('N'))
        $disabled = $false; $created = $false; $removed = $false; $movedOld = $false; $movedNew = $false
        try {
            if (-not $Uninstall) {
                [void](New-Item -ItemType Directory -Path $stage)
                Set-MoyaiSandboxProtection $stage $true -Private
                foreach ($file in $payload) {
                    Assert-MoyaiSandboxRegularPath $file.source
                    $target = Join-Path $stage $file.name
                    Copy-MoyaiSandboxPinnedPayload $pins.files['app/bin/'+$file.name] $target
                    Set-MoyaiSandboxProtection $target $false -Private
                    if ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant() -cne $file.sha256) { throw 'Sandbox payload changed while it was copied.' }
                }
                $receipt = [ordered]@{schema=1;product='moyai-machine-sandbox';service=$location.service;install_root=$location.root;version=$manifest.version;visual_cpp=$manifest.runtime.visual_cpp;files=@($payload | ForEach-Object { [ordered]@{name=$_.name;sha256=$_.sha256} })}
                Write-MoyaiUtf8 (Join-Path $stage 'installation.json') ($receipt | ConvertTo-Json -Depth 5)
                Set-MoyaiSandboxProtection (Join-Path $stage 'installation.json') $false -Private
                foreach ($file in $payload) { Set-MoyaiSandboxProtection (Join-Path $stage $file.name) $false }
                Set-MoyaiSandboxProtection (Join-Path $stage 'installation.json') $false
                Set-MoyaiSandboxProtection $stage $true
            }
            if ($exists) {
                Set-MoyaiSandboxService $command 4
                $disabled = $true
                $stopped = Get-MoyaiSandboxService -Security
                Assert-MoyaiSandboxService $stopped $location -Disabled -Security
                if ($stopped.State -ne 1) { throw 'Sandbox began running during setup. Close Desktop/Runner and try again; no files were replaced.' }
                Move-Item -LiteralPath $location.root -Destination $backup
                $movedOld = $true
            }
            if ($Uninstall) {
                Remove-MoyaiSandboxService
                $removed = $true
                $disabled = $false
                Remove-MoyaiSandboxPayloadDirectory $backup $old.files
                Write-Output 'PC共通sandbox helperを削除しました。利用者の設定・会話・作業フォルダーは変更していません。'
                return
            }
            Move-Item -LiteralPath $stage -Destination $location.root
            $movedNew = $true
            if (-not $exists) { Set-MoyaiSandboxService $command 4 -Create; $created = $true; $disabled = $true; Set-MoyaiSandboxServiceSecurity (Get-MoyaiSandboxServiceSddl) }
            Assert-MoyaiSandboxService (Get-MoyaiSandboxService -Security) $location -Disabled -Security
            [void](Read-MoyaiSandboxInstallation $location)
            # Nothing fallible changes authority after demand-start is restored.
            Set-MoyaiSandboxService $command 3
            $disabled = $false
        } catch {
            $failure = $_
            if ($removed) { throw 'The sandbox service was removed. Protected payload backup cleanup did not complete; inspect SandboxService.previous-* as administrator. User data was not changed.' }
            try {
                if ($disabled -and ($movedOld -or $movedNew)) {
                    $current = Get-MoyaiSandboxService
                    if ($null -ne $current -and $current.State -ne 1) { throw 'Sandbox is running; rollback was not forced.' }
                }
                if ($created) { Remove-MoyaiSandboxService; $created = $false }
                if ($movedNew) { Move-Item -LiteralPath $location.root -Destination $stage; $movedNew = $false }
                if ($movedOld) { Move-Item -LiteralPath $backup -Destination $location.root; $movedOld = $false }
                if ($exists -and $disabled) { Set-MoyaiSandboxService $command 3; $disabled = $false }
            } catch { throw "Sandbox setup failed and rollback requires administrator inspection. Protected staging/backup files were retained. $($_.Exception.Message)" }
            throw $failure
        }
        if ($movedOld) { Remove-MoyaiSandboxPayloadDirectory $backup $old.files }
        Write-Output 'PC共通sandbox helperを準備しました。通常の起動で管理者承認は不要です。'
    } finally { $lock.Dispose() }
    } finally { Close-MoyaiSandboxSourcePins $pins }
}
