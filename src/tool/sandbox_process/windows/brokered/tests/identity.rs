use super::{fixture, literal, run};
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::mem::{offset_of, size_of};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::ptr::{null_mut, slice_from_raw_parts};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const INTERACTIVE: &str = "S-1-5-4";
const WRITE_CONTENT: u32 = 0x116; // data, append, extended attributes, attributes

struct QueryToken(HANDLE);
impl Drop for QueryToken {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn current_token() -> QueryToken {
    let mut token = null_mut();
    assert_ne!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
        0
    );
    QueryToken(token)
}

fn token_groups(token: HANDLE) -> Vec<(String, u32)> {
    let mut needed = 0;
    assert_eq!(
        unsafe { GetTokenInformation(token, 2, null_mut(), 0, &mut needed) },
        0
    );
    assert_eq!(unsafe { GetLastError() }, 122);
    let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
    assert_ne!(
        unsafe { GetTokenInformation(token, 2, buffer.as_mut_ptr().cast(), needed, &mut needed) },
        0
    );
    let count = unsafe { *(buffer.as_ptr() as *const u32) } as usize;
    let offset = offset_of!(TOKEN_GROUPS, Groups);
    assert!(offset + count * size_of::<SID_AND_ATTRIBUTES>() <= needed as usize);
    let entries = unsafe {
        &*slice_from_raw_parts(
            buffer
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<SID_AND_ATTRIBUTES>(),
            count,
        )
    };
    entries
        .iter()
        .map(|entry| {
            let mut text = null_mut();
            assert_ne!(unsafe { ConvertSidToStringSidW(entry.Sid, &mut text) }, 0);
            let mut length = 0;
            while unsafe { *text.add(length) } != 0 {
                length += 1;
            }
            let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) });
            unsafe {
                LocalFree(text.cast());
            }
            (sid.unwrap(), entry.Attributes)
        })
        .collect()
}

// Preserve the complete original descriptor until the assertion scope ends.
// The fixture's SYSTEM/owner cleanup authority is never replaced by a null DACL.
struct DenyContentWrite {
    file: File,
    descriptor: *mut c_void,
    original: *mut ACL,
    protected: bool,
}
impl DenyContentWrite {
    fn install(path: &camino::Utf8Path, denied_group: &str) -> Self {
        let file = OpenOptions::new()
            .access_mode(READ_CONTROL | WRITE_DAC)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(path)
            .unwrap();
        let handle = file.as_raw_handle() as HANDLE;
        let mut descriptor = null_mut();
        let mut original = null_mut();
        assert_eq!(
            unsafe {
                GetSecurityInfo(
                    handle,
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    &mut original,
                    null_mut(),
                    &mut descriptor,
                )
            },
            0
        );
        assert!(!descriptor.is_null() && !original.is_null());
        let mut control = 0u16;
        let mut revision = 0;
        assert_ne!(
            unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) },
            0
        );
        let guard = Self {
            file,
            descriptor,
            original,
            protected: control & 0x1000 != 0,
        };
        let mut sid = null_mut();
        let wide: Vec<_> = denied_group.encode_utf16().chain(Some(0)).collect();
        assert_ne!(
            unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) },
            0
        );
        let entry = EXPLICIT_ACCESS_W {
            grfAccessPermissions: WRITE_CONTENT,
            grfAccessMode: DENY_ACCESS,
            grfInheritance: 0,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: null_mut(),
                MultipleTrusteeOperation: 0,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_GROUP,
                ptstrName: sid.cast(),
            },
        };
        let mut changed = null_mut();
        let built = unsafe { SetEntriesInAclW(1, &entry, original, &mut changed) };
        unsafe {
            LocalFree(sid);
        }
        assert_eq!(built, 0);
        let status = unsafe {
            SetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                changed,
                null_mut(),
            )
        };
        unsafe {
            LocalFree(changed.cast());
        }
        assert_eq!(status, 0);
        guard
    }
}
impl Drop for DenyContentWrite {
    fn drop(&mut self) {
        let flags = DACL_SECURITY_INFORMATION
            | if self.protected {
                PROTECTED_DACL_SECURITY_INFORMATION
            } else {
                UNPROTECTED_DACL_SECURITY_INFORMATION
            };
        let status = unsafe {
            SetSecurityInfo(
                self.file.as_raw_handle() as HANDLE,
                SE_FILE_OBJECT,
                flags,
                null_mut(),
                null_mut(),
                self.original,
                null_mut(),
            )
        };
        unsafe {
            LocalFree(self.descriptor);
        }
        if status != 0 {
            eprintln!("restore identity fixture DACL: Win32 {status}");
        }
    }
}

const TOKEN_EVIDENCE: &str = r#"
$ErrorActionPreference='Stop';
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;
public static class BrokerTokenEvidence {
 [StructLayout(LayoutKind.Sequential)] struct Luid { public uint Low; public int High; }
 [DllImport("advapi32.dll",SetLastError=true)] static extern bool GetTokenInformation(IntPtr token,int kind,IntPtr data,uint size,out uint needed);
 [DllImport("advapi32.dll",SetLastError=true)] static extern bool IsTokenRestricted(IntPtr token);
 [DllImport("advapi32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern bool LookupPrivilegeNameW(string system,ref Luid luid,StringBuilder name,ref uint count);
 static IntPtr Query(IntPtr token,int kind) {
  uint needed; GetTokenInformation(token,kind,IntPtr.Zero,0,out needed);
  int error=Marshal.GetLastWin32Error();
  if(needed==0 || (error!=122 && error!=24)) throw new Win32Exception(error,"token info size "+kind);
  IntPtr data=Marshal.AllocHGlobal(checked((int)needed));
  if(!GetTokenInformation(token,kind,data,needed,out needed)) { error=Marshal.GetLastWin32Error(); Marshal.FreeHGlobal(data); throw new Win32Exception(error,"token info "+kind); }
  return data;
 }
 public static string Sid(int kind) {
  using(var identity=WindowsIdentity.GetCurrent()) {
   IntPtr data=Query(identity.Token,kind);
   try { return new SecurityIdentifier(Marshal.ReadIntPtr(data)).Value; } finally { Marshal.FreeHGlobal(data); }
  }
 }
 public static uint Scalar(int kind) {
  using(var identity=WindowsIdentity.GetCurrent()) {
   IntPtr data=Marshal.AllocHGlobal(4);
   try {
    uint needed;
    if(!GetTokenInformation(identity.Token,kind,data,4,out needed)) throw new Win32Exception(Marshal.GetLastWin32Error(),"token scalar "+kind);
    if(needed!=4) throw new InvalidOperationException("unexpected token scalar length "+kind+": "+needed);
    return unchecked((uint)Marshal.ReadInt32(data));
   } finally { Marshal.FreeHGlobal(data); }
  }
 }
 public static uint Attributes(string wanted) {
  using(var identity=WindowsIdentity.GetCurrent()) {
   IntPtr data=Query(identity.Token,2);
   try {
    int count=Marshal.ReadInt32(data), start=IntPtr.Size==8?8:4, stride=IntPtr.Size==8?16:8;
    for(int i=0;i<count;i++) { IntPtr item=IntPtr.Add(data,start+i*stride);
     if(new SecurityIdentifier(Marshal.ReadIntPtr(item)).Value==wanted) return unchecked((uint)Marshal.ReadInt32(item,IntPtr.Size));
    }
    throw new InvalidOperationException("missing token group "+wanted);
   } finally { Marshal.FreeHGlobal(data); }
  }
 }
 public static string[] EnabledPrivileges() {
  using(var identity=WindowsIdentity.GetCurrent()) {
   IntPtr data=Query(identity.Token,3);
   try {
    var names=new List<string>(); int count=Marshal.ReadInt32(data);
    for(int i=0;i<count;i++) { IntPtr item=IntPtr.Add(data,4+i*12);
     if((Marshal.ReadInt32(item,8)&2)==0) continue;
     var luid=new Luid { Low=unchecked((uint)Marshal.ReadInt32(item)),High=Marshal.ReadInt32(item,4) };
     uint length=0; LookupPrivilegeNameW(null,ref luid,null,ref length);
     var name=new StringBuilder(checked((int)length+1)); length=unchecked((uint)name.Capacity);
     if(!LookupPrivilegeNameW(null,ref luid,name,ref length)) throw new Win32Exception();
     names.Add(name.ToString());
    }
    return names.ToArray();
   } finally { Marshal.FreeHGlobal(data); }
  }
 }
 public static bool Restricted() { using(var identity=WindowsIdentity.GetCurrent()) return IsTokenRestricted(identity.Token); }
}
'@;
$owner=[BrokerTokenEvidence]::Sid(4);
$evidence=[pscustomobject]@{
 user_sid=[BrokerTokenEvidence]::Sid(1); owner_sid=$owner;
 integrity_sid=[BrokerTokenEvidence]::Sid(25); session_id=[BrokerTokenEvidence]::Scalar(12);
 elevated=[BrokerTokenEvidence]::Scalar(20); app_container=[BrokerTokenEvidence]::Scalar(29);
 restricted=[BrokerTokenEvidence]::Restricted();
 interactive_attributes=[BrokerTokenEvidence]::Attributes('S-1-5-4');
 owner_attributes=[BrokerTokenEvidence]::Attributes($owner);
 enabled_privileges=@([BrokerTokenEvidence]::EnabledPrivileges());
};
"#;

#[test]
#[ignore = "requires an approved broker testhost with an interactive standard-medium caller"]
fn broker_preserves_interactive_deny_and_standard_child_identity() {
    let caller = current_token();
    let caller_attributes = token_groups(caller.0)
        .iter()
        .find(|(sid, _)| sid == INTERACTIVE)
        .map(|(_, attributes)| *attributes)
        .expect("native gate needs an interactive caller, not a service/test SYSTEM token");
    verify_group_deny_and_standard_child_identity(caller.0, INTERACTIVE, caller_attributes);
}

#[test]
#[ignore = "requires an approved broker testhost logged on as an Active Directory domain user"]
fn broker_preserves_domain_group_deny_and_standard_child_identity() {
    use crate::sandbox_broker::token::{CallerAccountKind, caller_identity};

    let caller = current_token();
    let identity = caller_identity(caller.0).unwrap();
    assert_eq!(
        identity.account_kind,
        CallerAccountKind::ActiveDirectory,
        "AD native gate requires an Active Directory domain user; a local account is not evidence"
    );
    assert!(identity.user_sid.starts_with("S-1-5-21-"));
    let (domain_sid, _) = identity.user_sid.rsplit_once('-').unwrap();
    let domain_prefix = format!("{domain_sid}-");
    let (denied_group, caller_attributes) = token_groups(caller.0)
        .into_iter()
        .find(|(sid, attributes)| {
            sid.starts_with(&domain_prefix)
                && sid != &identity.user_sid
                && attributes & 4 != 0
                && attributes & 16 == 0
                && attributes & 0xc000_0000 == 0
        })
        .expect("AD native gate requires an enabled non-logon group from the caller's AD domain");
    verify_group_deny_and_standard_child_identity(caller.0, &denied_group, caller_attributes);
}

fn verify_group_deny_and_standard_child_identity(
    caller: HANDLE,
    denied_group: &str,
    caller_attributes: u32,
) {
    let caller_user = crate::sandbox_broker::token::user_sid(caller).unwrap();
    assert_ne!(
        caller_attributes & 4,
        0,
        "the denied group must be enabled in the authenticated caller"
    );
    assert_eq!(caller_attributes & 16, 0);
    let (_fixture, root, profile) = fixture();
    let denied = root.join("group-denied.txt");
    let allowed = root.join("control.txt");
    std::fs::write(&denied, "sentinel").unwrap();
    std::fs::write(&allowed, "host-control").unwrap();
    // Admit the same roots and prove ordinary access before installing the DENY.
    let warmup = run(
        profile.clone(),
        &root,
        format!(
            "$ErrorActionPreference='Stop'; [IO.File]::WriteAllText('{}','sentinel'); [IO.File]::WriteAllText('{}','warmup-control')",
            literal(&denied),
            literal(&allowed)
        ),
    );
    assert_eq!(
        warmup.exit_code,
        Some(0),
        "{}",
        String::from_utf8_lossy(&warmup.stderr.bytes)
    );
    assert!(warmup.process_tree_reaped && warmup.cleanup_errors.is_empty());
    let deny = DenyContentWrite::install(&denied, denied_group);
    let error = std::fs::write(&denied, "host-must-not-write").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(5));
    std::fs::write(&allowed, "unrelated-host-control").unwrap();
    assert_eq!(
        std::fs::read_to_string(&allowed).unwrap(),
        "unrelated-host-control"
    );
    let script = format!(
        r#"{TOKEN_EVIDENCE}
$denied=$false;
try {{ [IO.File]::WriteAllText('{}','child-must-not-write') }} catch {{
 $denialException=$_.Exception; while($null -ne $denialException.InnerException) {{ $denialException=$denialException.InnerException }};
 if(($denialException.HResult -band 65535) -ne 5) {{ throw }}; $denied=$true;
}};
if(-not $denied) {{ throw 'caller group DENY was lost' }};
[IO.File]::WriteAllText('{}','restricted-control');
$evidence | Add-Member -NotePropertyName denied_group_attributes -NotePropertyValue ([BrokerTokenEvidence]::Attributes('{denied_group}'));
$evidence | ConvertTo-Json -Depth 4 -Compress;
"#,
        literal(&denied),
        literal(&allowed)
    );
    let output = run(profile, &root, script);
    let stdout = String::from_utf8_lossy(&output.stdout.bytes);
    assert_eq!(
        output.exit_code,
        Some(0),
        "stdout={stdout}\nstderr={}",
        String::from_utf8_lossy(&output.stderr.bytes)
    );
    assert!(output.effect_started && output.process_tree_reaped);
    assert!(
        output.cleanup_errors.is_empty(),
        "{:?}",
        output.cleanup_errors
    );
    let evidence: serde_json::Value = serde_json::from_slice(&output.stdout.bytes).unwrap();
    assert_eq!(evidence["user_sid"], caller_user);
    assert_eq!(evidence["integrity_sid"], "S-1-16-8192");
    assert_eq!(evidence["session_id"], 0);
    assert_eq!(evidence["elevated"], 0);
    assert_eq!(evidence["app_container"], 0);
    assert_eq!(evidence["restricted"], true);
    let interactive = evidence["interactive_attributes"].as_u64().unwrap();
    assert!(interactive & 4 != 0 || interactive & 16 != 0);
    let denied_group_attributes = evidence["denied_group_attributes"].as_u64().unwrap();
    assert!(denied_group_attributes & 4 != 0 || denied_group_attributes & 16 != 0);
    let owner = evidence["owner_sid"].as_str().unwrap();
    assert_ne!(owner, caller_user);
    assert_ne!(owner, "S-1-1-0");
    assert_ne!(owner, "S-1-3-4");
    assert_eq!(evidence["owner_attributes"].as_u64().unwrap() & 0x1c, 0x0c);
    assert_eq!(
        evidence["enabled_privileges"],
        serde_json::json!(["SeChangeNotifyPrivilege"])
    );
    assert_eq!(std::fs::read_to_string(&denied).unwrap(), "sentinel");
    assert_eq!(
        std::fs::read_to_string(&allowed).unwrap(),
        "restricted-control"
    );
    println!(
        "caller_denied_group={denied_group}; caller_group_attributes={caller_attributes}; child_token={evidence}"
    );
    drop(deny);
    std::fs::write(&denied, "restored-host-write").unwrap();
    assert_eq!(
        std::fs::read_to_string(&denied).unwrap(),
        "restored-host-write"
    );
}
