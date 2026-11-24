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
  [StructLayout(LayoutKind.Sequential)] struct StatusProcess {
   public uint type, state, accepted, win32exit, specificexit, checkpoint, waitHint, pid, flags;
  }
  [StructLayout(LayoutKind.Sequential)] struct UniqueProcess { public uint pid; public System.Runtime.InteropServices.ComTypes.FILETIME start; }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct RmProcessInfo {
   public UniqueProcess process;
   [MarshalAs(UnmanagedType.ByValTStr,SizeConst=256)] public string app;
   [MarshalAs(UnmanagedType.ByValTStr,SizeConst=64)] public string service;
   public uint type, status, session;
   [MarshalAs(UnmanagedType.Bool)] public bool restartable;
  }
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool QueryServiceStatusEx(IntPtr service,int level,ref StatusProcess status,uint size,out uint needed);
  [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr OpenProcess(uint access,bool inherit,uint pid);
  [DllImport("kernel32.dll")] static extern uint WaitForSingleObject(IntPtr handle,uint milliseconds);
  [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
  [DllImport("rstrtmgr.dll", CharSet=CharSet.Unicode)] static extern int RmStartSession(out uint session,int flags,System.Text.StringBuilder key);
  [DllImport("rstrtmgr.dll")] static extern int RmEndSession(uint session);
  [DllImport("rstrtmgr.dll", CharSet=CharSet.Unicode)] static extern int RmRegisterResources(uint session,uint files,string[] names,uint applications,IntPtr processes,uint services,string[] serviceNames);
  [DllImport("rstrtmgr.dll")] static extern int RmGetList(uint session,out uint needed,ref uint count,[In,Out] RmProcessInfo[] processes,ref uint reasons);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr OpenSCManager(string machine,string database,uint access);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr OpenService(IntPtr manager,string name,uint access);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool CloseServiceHandle(IntPtr handle);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool QueryServiceConfig(IntPtr service,IntPtr config,uint size,out uint needed);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool QueryServiceStatus(IntPtr service,out Status status);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool ControlService(IntPtr service,uint control,out Status status);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr CreateService(IntPtr manager,string name,string display,uint access,uint type,uint start,uint error,string command,string group,IntPtr tag,string deps,string account,string password);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool ChangeServiceConfig(IntPtr service,uint type,uint start,uint error,string command,string group,IntPtr tag,string deps,string account,string password,string display);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool DeleteService(IntPtr service);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool QueryServiceObjectSecurity(IntPtr service,uint info,byte[] buffer,uint size,out uint needed);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool SetServiceObjectSecurity(IntPtr service,uint info,IntPtr descriptor);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool ConvertStringSecurityDescriptorToSecurityDescriptor(string text,uint revision,out IntPtr descriptor,out uint size);
  [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern bool ConvertSecurityDescriptorToStringSecurityDescriptor(byte[] descriptor,uint revision,uint info,out IntPtr text,out uint size);
  [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr value);
  static Exception Error() {
   int code=Marshal.GetLastWin32Error();
   if(code==1072) return new Win32Exception(code,"WindowsがMoyAISandboxの削除を保留しています。サービスの管理画面や監視ツールを閉じて再実行してください。保留が続く場合はPCを再起動してください。");
   return new Win32Exception(code);
  }
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
  static StatusProcess Query(IntPtr service) {
   var s=new StatusProcess(); uint needed;
   if(!QueryServiceStatusEx(service,0,ref s,(uint)Marshal.SizeOf(typeof(StatusProcess)),out needed)) throw Error();
   return s;
  }
  // Sends one ordinary STOP and waits for the helper to drain its own work. The
  // reported STOPPED state precedes process exit, and a still-mapped helper image
  // cannot be deleted, so the old helper process is also awaited. Nothing is killed.
  public static void Stop(uint timeoutMs) {
   var m=Manager(1); IntPtr h=IntPtr.Zero, process=IntPtr.Zero;
   try {
    h=Service(m,0x24,false);
    var s=Query(h);
    if(s.state==1) return;
    var timer=System.Diagnostics.Stopwatch.StartNew();
    bool requested=s.state==3;
    while(true) {
     if(process==IntPtr.Zero && s.pid!=0) process=OpenProcess(0x00100000,false,s.pid);
     if(!requested) {
      Status ignored;
      if(ControlService(h,1,out ignored)) requested=true;
      else {
       int code=Marshal.GetLastWin32Error();
       if(code==1062) requested=true;
       // START_PENDING cannot accept STOP yet; ask again after it reports.
       else if(code!=1061) throw new Win32Exception(code);
      }
     }
     if(timer.ElapsedMilliseconds>=timeoutMs) throw new TimeoutException("MoyAISandboxが"+(timeoutMs/1000)+"秒以内に停止しませんでした。実行中の処理は強制停止していません。moyAIの仕事が終わってから再実行してください。停止しない状態が続く場合はPCを再起動してから再実行してください。");
     System.Threading.Thread.Sleep(100);
     s=Query(h);
     if(s.state==1) break;
     if(s.state==3) requested=true;
    }
    if(s.win32exit!=0 || s.specificexit!=0) throw new IOException("MoyAISandboxは停止しましたが、終了時の片付けを確認できませんでした（Windowsの終了コード "+s.win32exit+"、サービスの終了コード "+s.specificexit+"）。今回は変更を中止しました。再実行すると、停止済みのサービスとして処理を続けます。");
    if(process!=IntPtr.Zero) {
     long remaining=Math.Max(5000L,timeoutMs-timer.ElapsedMilliseconds);
     if(WaitForSingleObject(process,(uint)remaining)!=0) throw new TimeoutException("MoyAISandboxは停止を報告しましたが、helperのプロセス（PID "+s.pid+"）がまだ終了していません。強制終了はしていません。しばらく待ってから再実行してください。続く場合はPCを再起動してから再実行してください。");
    }
   } finally { if(process!=IntPtr.Zero) CloseHandle(process); if(h!=IntPtr.Zero) CloseServiceHandle(h); CloseServiceHandle(m); }
  }
  // Read-only Restart Manager query: which processes hold any of these files.
  public static string[] FileHolders(string[] files) {
   if(files==null || files.Length==0) return new string[0];
   uint session; var key=new System.Text.StringBuilder(64);
   int e=RmStartSession(out session,0,key); if(e!=0) throw new Win32Exception(e);
   try {
    e=RmRegisterResources(session,(uint)files.Length,files,0,IntPtr.Zero,0,null); if(e!=0) throw new Win32Exception(e);
    uint needed=0,count=0,reasons=0; RmProcessInfo[] info=null;
    for(int attempt=0;attempt<4;attempt++) {
     e=RmGetList(session,out needed,ref count,info,ref reasons);
     if(e!=234) break;
     info=new RmProcessInfo[needed]; count=needed;
    }
    if(e!=0) throw new Win32Exception(e);
    var result=new string[count];
    for(int i=0;i<count;i++) result[i]=info[i].process.pid+"|"+info[i].app+"|"+info[i].service;
    return result;
   } finally { RmEndSession(session); }
  }
 }
}
'@
}

function Get-MoyaiSandboxService([switch]$Security) { Initialize-MoyaiSandboxNative; [MoyaiSandboxSetup.Native]::Read([bool]$Security) }
function Set-MoyaiSandboxService([string]$Command, [uint32]$Start, [switch]$Create) { Initialize-MoyaiSandboxNative; [MoyaiSandboxSetup.Native]::Configure($Command, $Start, [bool]$Create) }
function Set-MoyaiSandboxServiceSecurity([string]$Sddl) { Initialize-MoyaiSandboxNative; [MoyaiSandboxSetup.Native]::Security($Sddl) }
function Remove-MoyaiSandboxService { Initialize-MoyaiSandboxNative; [MoyaiSandboxSetup.Native]::Remove() }
function Stop-MoyaiSandboxService { Initialize-MoyaiSandboxNative; [MoyaiSandboxSetup.Native]::Stop(45000) }

# The SCM keeps a deleted service until its process has exited and every open
# service handle is closed. It records that pending state as DeleteFlag; reading
# the value does not change the registration.
function Test-MoyaiSandboxServiceDeletePending {
    $key=[Microsoft.Win32.Registry]::LocalMachine.OpenSubKey('SYSTEM\CurrentControlSet\Services\MoyAISandbox',$false)
    if($null -eq $key) { return $false }
    try { return [int]$key.GetValue('DeleteFlag',0) -eq 1 } finally { $key.Dispose() }
}

function Test-MoyaiSandboxDeletePendingError($Exception) {
    for($cause=$Exception; $null -ne $cause; $cause=$cause.InnerException) {
        if($cause -is [ComponentModel.Win32Exception] -and $cause.NativeErrorCode -eq 1072) { return $true }
    }
    return $false
}

function Get-MoyaiSandboxServiceState([switch]$Security) {
    try { $service=Get-MoyaiSandboxService -Security:$Security }
    catch { if(Test-MoyaiSandboxDeletePendingError $_.Exception) { return [pscustomobject]@{service=$null;pending=$true} }; throw }
    $pending=$null -ne $service -and (Test-MoyaiSandboxServiceDeletePending)
    [pscustomobject]@{service=$service;pending=$pending}
}

function Get-MoyaiSandboxRemovalWaitMilliseconds { 30000 }

# Windows does not report which process keeps a service handle open. These are the
# usual viewers of service registrations; listing them only helps the user choose.
function Get-MoyaiServiceViewerProcesses {
    @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.ProcessName -in @('mmc','Taskmgr','perfmon','resmon') -or $_.ProcessName -like 'procexp*' -or $_.ProcessName -like 'ProcessHacker*' -or $_.ProcessName -like 'SystemInformer*' } | ForEach-Object { "$($_.ProcessName) (PID $($_.Id))" })
}

function Get-MoyaiSandboxPendingRemovalNotice {
    $viewers=@(Get-MoyaiServiceViewerProcesses)
    $candidates=if($viewers.Count){"起動中の該当しそうなアプリ: $($viewers -join '、')。"}else{''}
    'Windowsは実行補助サービス（MoyAISandbox）の削除を受け付けましたが、登録の消去を保留しています。Windowsの「サービス」画面、タスクマネージャーの「サービス」、監視ツールなど、サービスを表示しているアプリを閉じると消去されます。' + $candidates + '消えない場合はPCを再起動してください。同じ名前での再導入は、Windowsが登録を消去した後に行えます。'
}

# Returns $true once the registration is gone; $false while Windows still keeps
# a deleted registration. Callers decide whether that blocks their operation.
function Wait-MoyaiSandboxRemoval {
    $elapsed=[Diagnostics.Stopwatch]::StartNew()
    $limit=Get-MoyaiSandboxRemovalWaitMilliseconds
    do {
        $state=Get-MoyaiSandboxServiceState
        if($null -eq $state.service -and -not $state.pending) { return $true }
        Start-Sleep -Milliseconds 200
    } while($elapsed.ElapsedMilliseconds -lt $limit)
    return $false
}

function Get-MoyaiSandboxFileHolders([string[]]$Paths) {
    $files=@($Paths | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) })
    if(-not $files.Count) { return @() }
    Initialize-MoyaiSandboxNative
    try { $entries=[MoyaiSandboxSetup.Native]::FileHolders([string[]]$files) }
    catch { return @("使用中のアプリを特定できませんでした（$($_.Exception.Message)）") }
    foreach($entry in $entries) {
        $parts=$entry.Split('|')
        $name=if($parts[2]){"サービス $($parts[2])"}elseif($parts[1]){$parts[1]}else{'不明なアプリ'}
        "$name (PID $($parts[0]))"
    }
}

function Get-MoyaiSandboxInUseMessage([string]$Path,[string[]]$Holders) {
    $detail=if(@($Holders).Count){"使用中: $(@($Holders) -join '、')。"}else{''}
    "ファイルが使用中のため、$Path を入れ替え・片付けできません。$detail" +
    'moyAI Desktopは「ファイル」から「moyAIを終了（このPCの仕事も停止）」を選び、CLI・Runner・試験用のmoyAIプロセスも通常の方法で終了してから再実行してください。終了できない場合や使用中のアプリが分からない場合は、PCを再起動してから再実行してください。Setupはプロセスを強制終了しません。'
}

function Get-MoyaiSandboxProcessWaitMilliseconds { 20000 }

function Get-MoyaiProcessesUnderRoot([string]$Root) {
    $boundary=[IO.Path]::GetFullPath($Root).TrimEnd('\')+'\'
    @(foreach($process in @(Get-Process -ErrorAction SilentlyContinue)) {
        try { $path=$process.Path } catch { $path=$null }
        if($path -and $path.StartsWith($boundary,[StringComparison]::OrdinalIgnoreCase)) { "$($process.ProcessName) (PID $($process.Id)): $path" }
    })
}

# An app or helper that was just closed can still be leaving for a few seconds.
# Wait for such processes to exit on their own; nothing is terminated.
function Wait-MoyaiProcessesExit([scriptblock]$Query) {
    $elapsed=[Diagnostics.Stopwatch]::StartNew()
    $limit=Get-MoyaiSandboxProcessWaitMilliseconds
    while($true) {
        $remaining=@(& $Query)
        if(-not $remaining.Count -or $elapsed.ElapsedMilliseconds -ge $limit) { return $remaining }
        Start-Sleep -Milliseconds 500
    }
}

# A process started from the helper folder keeps its image mapped: the folder can
# still be renamed, but its files cannot be deleted. Other open files are found by
# the move or delete itself and are then diagnosed with Restart Manager.
function Assert-MoyaiSandboxPayloadUnused([string]$Root) {
    if(-not(Test-Path -LiteralPath $Root)) { return }
    $holders=@(Wait-MoyaiProcessesExit { Get-MoyaiProcessesUnderRoot $Root })
    if($holders.Count) { throw (Get-MoyaiSandboxInUseMessage $Root $holders) }
}

function Get-MoyaiSandboxFileRetryMilliseconds { 10000 }

function Invoke-MoyaiSandboxFileRetry([scriptblock]$Action) {
    $TimeoutMs=Get-MoyaiSandboxFileRetryMilliseconds
    $elapsed=[Diagnostics.Stopwatch]::StartNew()
    while($true) {
        try { return (& $Action) }
        catch {
            $cause=$_.Exception.GetBaseException()
            $code=$cause.HResult -band 65535
            # A helper that just reported STOPPED may still be leaving; directory
            # rename reports access denied while an open child pins the folder.
            if(($cause -isnot [IO.IOException] -and $cause -isnot [UnauthorizedAccessException]) -or $code -notin @(5,32,33) -or $elapsed.ElapsedMilliseconds -ge $TimeoutMs) { throw }
            Start-Sleep -Milliseconds 200
        }
    }
}

function Move-MoyaiStoppedSandboxToBackup([string]$Source,[string]$Destination) {
    try { Invoke-MoyaiSandboxFileRetry { [IO.Directory]::Move($Source,$Destination) } }
    catch {
        $holders=@(Get-MoyaiSandboxFileHolders @(Get-ChildItem -LiteralPath $Source -Force -File -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName }))
        throw (Get-MoyaiSandboxInUseMessage $Source $holders)
    }
}

function New-MoyaiSandboxRetainedItem([string]$Message) {
    # Same type name as component setup, so its summary lists the item as retained.
    $Message | Add-Member -TypeName 'Moyai.RetainedItem' -PassThru
}

function Get-MoyaiServicesReferencingPath([string]$Path) {
    @(Get-CimInstance Win32_Service -ErrorAction Stop | Where-Object { $_.PathName -and $_.PathName.IndexOf($Path,[StringComparison]::OrdinalIgnoreCase) -ge 0 } | ForEach-Object { $_.Name })
}

function Get-MoyaiSandboxLeftoverDirectories($Location) {
    if(-not(Test-Path -LiteralPath $Location.parent)) { return @() }
    @(Get-ChildItem -LiteralPath $Location.parent -Directory -Force | Where-Object { $_.Name.StartsWith('SandboxService.previous-',[StringComparison]::OrdinalIgnoreCase) -or $_.Name.StartsWith('SandboxService.stage-',[StringComparison]::OrdinalIgnoreCase) } | ForEach-Object { $_.FullName })
}

# Removes owned backup/staging copies left by an interrupted update or uninstall.
# Unverifiable or in-use copies are kept and returned as retained items.
function Remove-MoyaiSandboxLeftovers($Location) {
    foreach($candidate in @(Get-MoyaiSandboxLeftoverDirectories $Location)) {
        try {
            Assert-MoyaiSandboxProtectedPath $candidate
            $receiptPath=Join-Path $candidate 'installation.json'
            if(-not(Test-Path -LiteralPath $receiptPath -PathType Leaf)) {
                if(@(Get-ChildItem -LiteralPath $candidate -Force).Count -eq 0) { Remove-Item -LiteralPath $candidate; continue }
                throw '導入記録がないため、所有を確認できません。中身を確認してから管理者が削除してください。'
            }
            Assert-MoyaiSandboxProtectedPath $receiptPath
            $receipt=Get-Content -LiteralPath $receiptPath -Raw -Encoding UTF8 | ConvertFrom-Json
            if($receipt.schema -ne 1 -or $receipt.product -cne 'moyai-machine-sandbox' -or $receipt.service -cne 'MoyAISandbox' -or $receipt.install_root -cne $Location.root) { throw '共通サービスの旧導入記録を確認できません。' }
            foreach($file in @($receipt.files)) {
                if($file.name -cne [IO.Path]::GetFileName($file.name) -or $file.name -notmatch '^[A-Za-z0-9_.-]+\.(exe|dll)$' -or $file.sha256 -cnotmatch '^[a-f0-9]{64}$') { throw '旧導入記録のファイル一覧が不正です。' }
                $path=Join-Path $candidate $file.name
                if(-not(Test-Path -LiteralPath $path)) { continue }
                Assert-MoyaiSandboxProtectedPath $path
                if((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ine $file.sha256) { throw '旧実行ファイルが導入記録と一致しません。中身を確認してから管理者が削除してください。' }
            }
            $owned=@('installation.json')+@($receipt.files | ForEach-Object { $_.name })
            if(@(Get-ChildItem -LiteralPath $candidate -Force | Where-Object { $_.PSIsContainer -or $_.Name -notin $owned }).Count) { throw '導入記録にないファイルがあるため、所有を確認できません。中身を確認してから管理者が削除してください。' }
            if(@(Get-MoyaiServicesReferencingPath $candidate).Count) { throw '旧配置を参照するサービスが残っています。' }
            Assert-MoyaiSandboxPayloadUnused $candidate
            try { Invoke-MoyaiSandboxFileRetry { Remove-MoyaiSandboxPayloadDirectory $candidate $receipt.files } | Out-Null }
            catch {
                $holders=@(Get-MoyaiSandboxFileHolders @(Get-ChildItem -LiteralPath $candidate -Force -File -ErrorAction SilentlyContinue | ForEach-Object { $_.FullName }))
                throw ((Get-MoyaiSandboxInUseMessage $candidate $holders) + ' 再実行すると、残ったファイルだけを片付けます。')
            }
        } catch { New-MoyaiSandboxRetainedItem "共通サービスの旧配置を保持しました: $candidate / $($_.Exception.Message)" }
    }
}

function Get-MoyaiSandboxClientProcesses {
    # Include portable and other-user clients: the helper is shared by the PC.
    @(Get-CimInstance Win32_Process -Filter "Name='moyai.exe' OR Name='moyai-desktop.exe' OR Name='moyai-runner.exe'" -ErrorAction Stop | ForEach-Object { "$($_.Name) (PID $($_.ProcessId))" })
}

function Assert-MoyaiSandboxClientsStopped {
    $clients = @(Wait-MoyaiProcessesExit { Get-MoyaiSandboxClientProcesses })
    if ($clients.Count) {
        $details = $clients -join ', '
        throw "実行補助サービスを使用するアプリが起動中です。Desktopは「ファイル」から「moyAIを終了（このPCの仕事も停止）」を選びます。CLIを閉じ、単独のRunnerはWindows導入手順の「更新・アンインストール」に従って終了してください。対象: $details"
    }
}

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

function Assert-MoyaiSandboxService($Service, $Location, [switch]$Disabled, [switch]$AllowDisabled, [switch]$Security) {
    $start = if ($Disabled) { @(4) } elseif ($AllowDisabled) { @(3,4) } else { @(3) }
    if ($null -eq $Service) { throw 'MoyAISandboxの登録を確認できません。' }
    $expectedCommand='"' + $Location.helper + '" --service'
    $differences=[Collections.Generic.List[string]]::new()
    if($Service.Type -ne 0x10) { $differences.Add("種類 $($Service.Type)（想定 16）") }
    if($Service.Start -notin $start) { $differences.Add("開始方法 $($Service.Start)（想定 $($start -join ' または ')）") }
    if($Service.ErrorControl -ne 1) { $differences.Add("エラー制御 $($Service.ErrorControl)（想定 1）") }
    if($Service.Account -cne 'LocalSystem') { $differences.Add("実行アカウント $($Service.Account)（想定 LocalSystem）") }
    if($Service.Command -cne $expectedCommand) { $differences.Add("起動コマンド $($Service.Command)（想定 $expectedCommand）") }
    if($Service.Dependencies) { $differences.Add("依存サービス $($Service.Dependencies)（想定 なし）") }
    if($differences.Count) { throw "MoyAISandboxの登録が導入時の設定と異なります。PCの管理者に確認してください。$($differences -join '、')" }
    if ($Security) {
        $actual = [Security.AccessControl.RawSecurityDescriptor]::new($Service.Sddl)
        if ($null -eq $actual.Owner -or $actual.Owner.Value -notin @('S-1-5-18','S-1-5-32-544')) { throw "MoyAISandboxの所有者が想定外です。現在 $($actual.Owner)、想定 SYSTEM または Administrators。PCの管理者に確認してください。" }
        if ($null -eq $actual.DiscretionaryAcl -or $actual.DiscretionaryAcl.Count -ne 3) { throw "MoyAISandboxのアクセス許可の項目数が想定の3件と異なります。PCの管理者に確認してください。現在の設定: $($Service.Sddl)" }
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($ace in $actual.DiscretionaryAcl) {
            if ($ace -isnot [Security.AccessControl.CommonAce] -or $ace.IsCallback -or $ace.AceQualifier -ne [Security.AccessControl.AceQualifier]::AccessAllowed -or [int]$ace.AceFlags -ne 0) { throw "MoyAISandboxのアクセス許可に想定外の項目があります。PCの管理者に確認してください。現在の設定: $($Service.Sddl)" }
            $sid = $ace.SecurityIdentifier.Value
            if (-not $seen.Add($sid)) { throw "MoyAISandboxのアクセス許可で $sid が重複しています。PCの管理者に確認してください。" }
            # SCM may expand GENERIC_ALL to SERVICE_ALL_ACCESS when storing the DACL.
            $allowed = if ($sid -in @('S-1-5-18','S-1-5-32-544')) { $ace.AccessMask -in @(0x10000000,0x000f01ff) } elseif ($sid -ceq 'S-1-5-4') { $ace.AccessMask -eq 0x15 } else { $false }
            if (-not $allowed) { throw "MoyAISandboxのアクセス許可が導入時の設定と異なります。PCの管理者に確認してください。対象 $sid、アクセスマスク $($ace.AccessMask)。" }
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

function Read-MoyaiSandboxInstallation($Location, [switch]$AllowMissingFiles) {
    Assert-MoyaiSandboxProtectedPath $Location.parent
    Assert-MoyaiSandboxProtectedPath $Location.root
    $receiptPath = Join-Path $Location.root 'installation.json'
    if ($AllowMissingFiles -and -not(Test-Path -LiteralPath $receiptPath) -and @(Get-ChildItem -LiteralPath $Location.root -Force).Count -eq 0) { return $null }
    Assert-MoyaiSandboxProtectedPath $receiptPath
    $receipt = Get-Content -LiteralPath $receiptPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($receipt.schema -ne 1 -or $receipt.product -cne 'moyai-machine-sandbox' -or $receipt.service -cne $Location.service -or $receipt.install_root -cne $Location.root) { throw 'Sandbox installation ownership does not match its fixed location.' }
    $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    [void]$names.Add('installation.json')
    foreach ($file in @($receipt.files)) {
        if ($file.name -cne [IO.Path]::GetFileName($file.name) -or $file.name -notmatch '^[A-Za-z0-9_.-]+\.(exe|dll)$' -or -not $names.Add($file.name) -or $file.sha256 -cnotmatch '^[a-f0-9]{64}$') { throw 'Invalid installed sandbox payload inventory.' }
        if ($file.name -ine 'moyai-sandbox.exe' -and [IO.Path]::GetExtension($file.name) -ine '.dll') { throw 'Unexpected executable in sandbox inventory.' }
        $path = Join-Path $Location.root $file.name
        if($AllowMissingFiles -and -not(Test-Path -LiteralPath $path)) { continue }
        Assert-MoyaiSandboxProtectedPath $path
        if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() -cne $file.sha256) { throw 'Installed sandbox payload differs from its protected inventory.' }
    }
    if (-not $names.Contains('moyai-sandbox.exe')) { throw 'Installed sandbox helper is missing.' }
    if ($receipt.visual_cpp -eq 'app-local') {
        if ($names.Count -ne 5) { throw 'Installed sandbox runtime contains an unexpected DLL.' }
        foreach ($name in @('vcruntime140.dll','vcruntime140_1.dll','msvcp140.dll')) { if (-not $names.Contains($name)) { throw 'Installed sandbox runtime is incomplete.' } }
    } elseif ($receipt.visual_cpp -ne 'system-installed' -or $names.Count -ne 2) { throw 'Installed sandbox runtime mode is inconsistent.' }
    $entries = @(Get-ChildItem -LiteralPath $Location.root -Force)
    if (-not $AllowMissingFiles -and $entries.Count -ne $names.Count) { throw 'Sandbox directory contains unowned entries; no files were removed.' }
    foreach ($entry in $entries) { if ($entry.PSIsContainer -or -not $names.Contains($entry.Name)) { throw 'Sandbox directory contains an unexpected entry.' }; Assert-MoyaiSandboxProtectedPath $entry.FullName }
    return $receipt
}

# Separates "this setup cannot prove it owns the fixed folder" from unsafe paths.
# An unverifiable folder is moved aside intact so the fixed path can be reused;
# a reparse point or an unprotected parent still stops the operation.
function Read-MoyaiSandboxInstallationState($Location) {
    Assert-MoyaiSandboxProtectedPath $Location.parent
    Assert-MoyaiSandboxRegularPath $Location.root
    try { [pscustomobject]@{receipt=(Read-MoyaiSandboxInstallation $Location -AllowMissingFiles);unverified=$false;reason=''} }
    catch { [pscustomobject]@{receipt=$null;unverified=$true;reason=$_.Exception.Message} }
}

function Test-MoyaiSandboxPayloadMatch($Receipt, $Payload, [string]$RuntimeMode) {
    if ($null -eq $Receipt) { return $false }
    if ($Receipt.visual_cpp -cne $RuntimeMode -or @($Receipt.files).Count -ne @($Payload).Count) { return $false }
    foreach ($file in $Payload) {
        if (@($Receipt.files | Where-Object { $_.name -ceq $file.name -and $_.sha256 -ceq $file.sha256 }).Count -ne 1) { return $false }
        if (-not(Test-Path -LiteralPath (Join-Path $Receipt.install_root $file.name) -PathType Leaf)) { return $false }
    }
    return $true
}

function Get-MoyaiSandboxInstallPlan([string]$SourceRoot, $Manifest) {
    $location = Get-MoyaiSandboxLocation
    Assert-MoyaiSandboxRegularPath $location.root
    $payload = @(Get-MoyaiSandboxPayload $SourceRoot $Manifest)
    $state = Get-MoyaiSandboxServiceState
    $service = $state.service
    $exists = Test-Path -LiteralPath $location.root
    if ($null -ne $service) { Assert-MoyaiSandboxService $service $location -AllowDisabled }
    if ($exists) {
        $installation = Read-MoyaiSandboxInstallationState $location
        $receipt = $installation.receipt
        $needed = $installation.unverified -or $state.pending -or $null -eq $service -or $service.Start -ne 3 -or -not (Test-MoyaiSandboxPayloadMatch $receipt $payload $Manifest.runtime.visual_cpp)
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
    $expected = @('installation.json') + @($Files | Where-Object { $null -ne $_ } | ForEach-Object { $_.name })
    $entries = @(Get-ChildItem -LiteralPath $Path -Force)
    foreach ($entry in $entries) { if ($entry.PSIsContainer -or $entry.Name -notin $expected) { throw 'Sandbox cleanup found an unowned entry.' }; Assert-MoyaiSandboxProtectedPath $entry.FullName }
    foreach ($entry in $entries) { Remove-Item -LiteralPath $entry.FullName }
    Remove-Item -LiteralPath $Path
}

function Invoke-MoyaiSandboxUninstall($Location) {
    $command = '"' + $Location.helper + '" --service'
    if (-not (Test-Path -LiteralPath $Location.parent)) {
        $state = Get-MoyaiSandboxServiceState
        if ($null -eq $state.service -and -not $state.pending) { Write-Output 'PC共通の実行補助サービスは導入されていません。既に削除済みとして終了します。'; return }
        if ($null -eq $state.service) { New-MoyaiSandboxRetainedItem (Get-MoyaiSandboxPendingRemovalNotice); return }
        [void](New-Item -ItemType Directory -Path $Location.parent)
        Set-MoyaiSandboxProtection $Location.parent $true
    }
    Assert-MoyaiSandboxProtectedPath $Location.parent
    $lockPath = Join-Path $Location.parent 'sandbox-setup.lock'
    Assert-MoyaiSandboxRegularPath $lockPath
    $lock = [IO.File]::Open($lockPath,[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
    try {
        $state = Get-MoyaiSandboxServiceState -Security
        $service = $state.service; $pending = $state.pending
        # A same-name registration that this setup did not create is never removed.
        if ($null -ne $service) { Assert-MoyaiSandboxService $service $Location -AllowDisabled -Security }
        $exists = Test-Path -LiteralPath $Location.root
        # An unverifiable fixed folder is moved aside intact and reported, not deleted.
        if ($exists) { [void](Read-MoyaiSandboxInstallationState $Location) }
        $leftovers = @(Get-MoyaiSandboxLeftoverDirectories $Location)
        if ($null -eq $service -and -not $exists -and -not $leftovers.Count) {
            if ($pending) { New-MoyaiSandboxRetainedItem (Get-MoyaiSandboxPendingRemovalNotice) }
            else { Write-Output 'PC共通の実行補助サービスは導入されていません。既に削除済みとして終了します。' }
            return
        }
        if ($null -ne $service -or $exists) { Assert-MoyaiSandboxClientsStopped }
        $backup = Join-Path $Location.parent ('SandboxService.previous-' + [Guid]::NewGuid().ToString('N'))
        $disabled = $false; $movedOld = $false; $removed = $false
        $originalStart = if ($null -ne $service) { $service.Start } else { 3 }
        try {
            if ($null -ne $service) {
                if (-not $pending) {
                    try { Set-MoyaiSandboxService $command 4; $disabled = $true }
                    catch { if (-not (Test-MoyaiSandboxDeletePendingError $_.Exception)) { throw }; $pending = $true }
                    Assert-MoyaiSandboxClientsStopped
                }
                # STOP lets the helper cancel and drain its own work; never kill its process.
                # Windows completes a pending deletion once a stopped service is unused.
                if (-not $pending -or $service.State -ne 1) { Stop-MoyaiSandboxService }
                $stopped = (Get-MoyaiSandboxServiceState -Security).service
                if ($null -ne $stopped) {
                    if ($disabled) { Assert-MoyaiSandboxService $stopped $Location -Disabled -Security }
                    if ($stopped.State -ne 1) { throw 'MoyAISandboxの停止を確認できないため、ファイルとサービス登録は変更していません。moyAIの仕事が終わってから再実行してください。' }
                }
            }
            if ($exists) {
                Assert-MoyaiSandboxPayloadUnused $Location.root
                Move-MoyaiStoppedSandboxToBackup $Location.root $backup
                $movedOld = $true
            }
            if ($null -ne $service -and -not $pending) {
                Remove-MoyaiSandboxService
                $removed = $true
            }
        } catch {
            $failure = $_
            try {
                if ($movedOld) { Move-Item -LiteralPath $backup -Destination $Location.root; $movedOld = $false }
                if ($disabled) { Set-MoyaiSandboxService $command $originalStart; $disabled = $false }
            } catch { throw "実行補助サービスの削除を中止しましたが、元の状態へ戻せませんでした。保護された退避先は残しています: $backup / $($_.Exception.Message)" }
            throw $failure
        }
        # From here the registration is deleted or pending deletion; nothing is rolled back.
        # The moved payload is a SandboxService.previous-* copy and is removed with
        # any copies that earlier interrupted runs left behind.
        # Collect plain text; the retained type is attached on output because .NET
        # collections drop the PowerShell type name.
        $retained = [Collections.Generic.List[string]]::new()
        foreach ($item in @(Remove-MoyaiSandboxLeftovers $Location)) { $retained.Add([string]$item) }
        if (($removed -or $pending) -and -not (Wait-MoyaiSandboxRemoval)) { $retained.Add((Get-MoyaiSandboxPendingRemovalNotice)) }
        if ($retained.Count) {
            Write-Output 'PC共通の実行補助サービスの削除を、一部を残して完了しました。残した対象と次の操作は次のとおりです。'
            foreach ($message in $retained) { New-MoyaiSandboxRetainedItem $message }
        } else { Write-Output 'PC共通の実行補助サービスを削除しました。利用者の設定・会話・作業フォルダーは変更していません。' }
    } finally { $lock.Dispose() }
}

function Invoke-MoyaiMachineSandboxSetup([string]$SourceRoot, [switch]$Uninstall, [string]$ExpectedManifestSha256 = '', [scriptblock]$PublishComponent, [scriptblock]$RestoreComponent, $ComponentContext) {
    if ($PSVersionTable.PSEdition -ne 'Desktop' -or $PSVersionTable.PSVersion.Major -ne 5 -or $PSVersionTable.PSVersion.Minor -ne 1) { throw 'Run machine sandbox setup with 64-bit Windows PowerShell 5.1 (powershell.exe), not PowerShell 7 (pwsh.exe).' }
    Assert-MoyaiSandboxAdministrator
    if (($null -ne $PublishComponent) -ne ($null -ne $RestoreComponent) -or ($Uninstall -and $null -ne $PublishComponent)) { throw 'Component publication requires paired install/rollback callbacks and cannot be combined with uninstall.' }
    $location = Get-MoyaiSandboxLocation
    Assert-MoyaiSandboxRegularPath $location.root
    if ($Uninstall) { Invoke-MoyaiSandboxUninstall $location; return }
    $manifestPath = Join-Path $SourceRoot 'deployment.json'
    Assert-MoyaiSandboxRegularPath $manifestPath
    $manifest = Read-MoyaiDeployment $SourceRoot -VerifyHashes -ExpectedSha256 $ExpectedManifestSha256
    if ($manifest.PSObject.Properties.Name -notcontains 'machine_sandbox' -or $manifest.machine_sandbox.service -cne 'MoyAISandbox' -or $manifest.machine_sandbox.setup -cne 'scripts/Setup-Sandbox.ps1') { throw 'This package does not declare the installed sandbox service contract.' }
    $payload = @(Get-MoyaiSandboxPayload $SourceRoot $manifest)
    $pins = New-MoyaiSandboxSourcePins $SourceRoot $manifest
    try {
    if (-not (Test-Path -LiteralPath $location.parent)) {
        [void](New-Item -ItemType Directory -Path $location.parent)
        Set-MoyaiSandboxProtection $location.parent $true
    }
    Assert-MoyaiSandboxProtectedPath $location.parent
    $lockPath = Join-Path $location.parent 'sandbox-setup.lock'
    Assert-MoyaiSandboxRegularPath $lockPath
    $lock = [IO.File]::Open($lockPath,[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
    try {
        $state = Get-MoyaiSandboxServiceState -Security
        if ($state.pending) {
            # An accepted uninstall must finish before the same name can be registered again.
            # Windows keeps it while the old helper still runs, so ask it to stop normally.
            if ($null -ne $state.service -and $state.service.State -ne 1) { Assert-MoyaiSandboxClientsStopped; Stop-MoyaiSandboxService }
            if (-not (Wait-MoyaiSandboxRemoval)) { throw ('実行補助サービスを導入できません。' + (Get-MoyaiSandboxPendingRemovalNotice)) }
            $state = Get-MoyaiSandboxServiceState -Security
        }
        $service = $state.service
        $exists = Test-Path -LiteralPath $location.root
        $old = $null
        if($null -ne $service) {
            Assert-MoyaiSandboxService $service $location -AllowDisabled -Security
        }
        $installation = [pscustomobject]@{receipt=$null;unverified=$false;reason=''}
        if ($exists) {
            $installation = Read-MoyaiSandboxInstallationState $location
            $old = $installation.receipt
            if (-not $installation.unverified -and $null -ne $service -and $service.Start -eq 3 -and (Test-MoyaiSandboxPayloadMatch $old $payload $manifest.runtime.visual_cpp)) {
                if($null -ne $PublishComponent) {
                    try { & $PublishComponent $ComponentContext }
                    catch { & $RestoreComponent $ComponentContext; throw }
                }
                return
            }
        }
        Assert-MoyaiSandboxClientsStopped
        Set-MoyaiSandboxParentAccess $location.parent
        $command = '"' + $location.helper + '" --service'
        $stage = Join-Path $location.parent ('SandboxService.stage-' + [Guid]::NewGuid().ToString('N'))
        $backup = Join-Path $location.parent ('SandboxService.previous-' + [Guid]::NewGuid().ToString('N'))
        $disabled = $false; $created = $false; $movedOld = $false; $movedNew = $false
        $originalStart = if($null -ne $service){$service.Start}else{3}
        try {
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
            if ($null -ne $service) {
                Assert-MoyaiSandboxClientsStopped
                Set-MoyaiSandboxService $command 4
                $disabled = $true
                Assert-MoyaiSandboxClientsStopped
                # Disabling new starts closes the start/update race. STOP lets the
                # helper cancel and drain its own work; never kill its process.
                Stop-MoyaiSandboxService
                $stopped = Get-MoyaiSandboxService -Security
                Assert-MoyaiSandboxService $stopped $location -Disabled -Security
                if ($stopped.State -ne 1) { throw 'Sandbox did not stop; no files were replaced.' }
            }
            if($exists) {
                Assert-MoyaiSandboxPayloadUnused $location.root
                Move-MoyaiStoppedSandboxToBackup $location.root $backup; $movedOld = $true
            }
            Move-Item -LiteralPath $stage -Destination $location.root
            $movedNew = $true
            if ($null -eq $service) { Set-MoyaiSandboxService $command 4 -Create; $created = $true; $disabled = $true; Set-MoyaiSandboxServiceSecurity (Get-MoyaiSandboxServiceSddl) }
            Assert-MoyaiSandboxService (Get-MoyaiSandboxService -Security) $location -Disabled -Security
            [void](Read-MoyaiSandboxInstallation $location)
            # Desktop is published while new service starts are still disabled.
            # Either publication or final registration failure restores both sides.
            if($null -ne $PublishComponent) { & $PublishComponent $ComponentContext }
            # Nothing fallible changes authority after demand-start is restored.
            Set-MoyaiSandboxService $command 3
            $disabled = $false
        } catch {
            $failure = $_
            try {
                if($null -ne $RestoreComponent) { & $RestoreComponent $ComponentContext }
                if ($disabled -and ($movedOld -or $movedNew)) {
                    $current = Get-MoyaiSandboxService
                    if ($null -ne $current -and $current.State -ne 1) { throw 'Sandbox is running; rollback was not forced.' }
                }
                if ($created) { Remove-MoyaiSandboxService; $created = $false }
                if ($movedNew) { Move-Item -LiteralPath $location.root -Destination $stage; $movedNew = $false }
                if ($movedOld) { Move-Item -LiteralPath $backup -Destination $location.root; $movedOld = $false }
                if ($null -ne $service -and $disabled) { Set-MoyaiSandboxService $command $originalStart; $disabled = $false }
            } catch { throw "Sandbox setup failed and rollback requires administrator inspection. Protected staging/backup files were retained. $($_.Exception.Message)" }
            throw $failure
        }
        Write-Output 'PC共通sandbox helperを準備しました。通常の起動で管理者承認は不要です。'
        # The new helper is active. A still-used old copy is kept for the next update or uninstall;
        # an unverifiable one is never deleted and is reported by the leftover cleanup.
        if ($movedOld -and -not $installation.unverified) {
            try { Invoke-MoyaiSandboxFileRetry { Remove-MoyaiSandboxPayloadDirectory $backup $(if($old){$old.files}else{@()}) } | Out-Null }
            catch { New-MoyaiSandboxRetainedItem "以前のhelperは使用中のため保持しました。次回の更新・削除で片付けます: $backup" }
        }
        # Component setup reports leftovers once, after all selected applications.
        if ($null -eq $PublishComponent) { Remove-MoyaiSandboxLeftovers $location }
    } finally { $lock.Dispose() }
    } finally { Close-MoyaiSandboxSourcePins $pins }
}
