//! Local and Active Directory S4U token preparation for the privileged sandbox broker.
//!
//! The caller handle is borrowed from the broker's authenticated client owner.
//! This module does not authenticate IPC or accept additional normal groups.
#![cfg(windows)]

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::mem::{offset_of, size_of, size_of_val, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, HANDLE, HLOCAL, INVALID_HANDLE_VALUE, LUID, LocalFree, SetLastError,
};
use windows_sys::Win32::Security::{
    ACL, AdjustTokenPrivileges, CopySid, CreateRestrictedToken, DuplicateTokenEx,
    GetSecurityDescriptorDacl, GetTokenInformation, IsTokenRestricted, LookupAccountSidW,
    LookupPrivilegeValueW, SID_AND_ATTRIBUTES, SetTokenInformation, TOKEN_PRIVILEGES,
};

const TOKEN_ALL_ACCESS: u32 = 0x000f_01ff;
const OWNER_GROUP_ATTRIBUTES: u32 = 0x0000_000f;
const SE_GROUP_ENABLED: u32 = 0x0000_0004;
const SE_GROUP_OWNER: u32 = 0x0000_0008;
const SE_GROUP_DENY_ONLY: u32 = 0x0000_0010;
const RESTRICTED_FLAGS: u32 = 0x01 | 0x04 | 0x08;
const WORLD_SID: &str = "S-1-1-0";
const MEDIUM_SID: &str = "S-1-16-8192";
const MAX_ROOT_SIDS: usize = 128;

#[repr(C)]
struct LsaString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct S4uLogon {
    message_type: u32,
    flags: u32,
    user_name: UnicodeString,
    domain_name: UnicodeString,
}

#[repr(C)]
struct OneGroup {
    count: u32,
    group: SID_AND_ATTRIBUTES,
}

#[repr(C)]
struct TokenSource {
    name: [u8; 8],
    identifier: LUID,
}

#[repr(C)]
struct QuotaLimits {
    paged_pool: usize,
    nonpaged_pool: usize,
    minimum_working_set: usize,
    maximum_working_set: usize,
    pagefile: usize,
    time_limit: i64,
}

#[link(name = "secur32")]
unsafe extern "system" {
    fn LsaRegisterLogonProcess(name: *const LsaString, handle: *mut HANDLE, mode: *mut u32) -> i32;
    fn LsaLookupAuthenticationPackage(
        handle: HANDLE,
        name: *const LsaString,
        package: *mut u32,
    ) -> i32;
    fn LsaLogonUser(
        handle: HANDLE,
        origin: *const LsaString,
        logon_type: i32,
        package: u32,
        information: *const c_void,
        information_length: u32,
        groups: *const OneGroup,
        source: *const TokenSource,
        profile: *mut *mut c_void,
        profile_length: *mut u32,
        logon_id: *mut LUID,
        token: *mut HANDLE,
        quotas: *mut QuotaLimits,
        substatus: *mut i32,
    ) -> i32;
    fn LsaDeregisterLogonProcess(handle: HANDLE) -> i32;
    fn LsaFreeReturnBuffer(buffer: *mut c_void) -> i32;
    fn TranslateNameW(
        account: *const u16,
        account_format: i32,
        desired_format: i32,
        translated: *mut u16,
        size: *mut u32,
    ) -> u8;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn LsaNtStatusToWinError(status: i32) -> u32;
    fn AllocateLocallyUniqueId(luid: *mut LUID) -> i32;
    fn ConvertStringSidToSidW(value: *const u16, sid: *mut *mut c_void) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, value: *mut *mut u16) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        value: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetComputerNameW(name: *mut u16, size: *mut u32) -> i32;
}

#[derive(Debug)]
pub struct OwnedToken(HANDLE);

// Windows token handles may be moved between threads; this wrapper owns the close.
unsafe impl Send for OwnedToken {}

impl OwnedToken {
    pub fn raw(&self) -> HANDLE {
        self.0
    }

    /// Trusted broker configuration before spawn; the session is not a client request field.
    pub fn set_session(&self, session: u32) -> Result<(), String> {
        check(
            unsafe {
                SetTokenInformation(
                    self.raw(),
                    12,
                    (&session as *const u32).cast(),
                    size_of::<u32>() as u32,
                )
            },
            "set trusted broker child session",
        )?;
        if session_id(self.raw())? != session {
            return Err("broker child session did not retain its configured value".into());
        }
        Ok(())
    }

    fn from_raw(raw: HANDLE, stage: &str) -> Result<Self, String> {
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            Err(win32_error(stage))
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for OwnedToken {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0 as HLOCAL) };
        }
    }
}

struct LsaConnection(HANDLE);

impl Drop for LsaConnection {
    fn drop(&mut self) {
        unsafe { LsaDeregisterLogonProcess(self.0) };
    }
}

struct LsaProfile(*mut c_void);

impl Drop for LsaProfile {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LsaFreeReturnBuffer(self.0) };
        }
    }
}

struct InformationBuffer {
    storage: Vec<usize>,
    bytes: usize,
}

impl InformationBuffer {
    fn new(bytes: usize) -> Self {
        Self {
            storage: vec![0; bytes.div_ceil(size_of::<usize>())],
            bytes,
        }
    }

    fn pointer(&self) -> *const c_void {
        self.storage.as_ptr().cast()
    }

    fn pointer_mut(&mut self) -> *mut c_void {
        self.storage.as_mut_ptr().cast()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallerIdentity {
    pub user_sid: String,
    pub user_name: String,
    pub domain_name: String,
    pub session_id: u32,
    pub(crate) account_kind: CallerAccountKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallerAccountKind {
    LocalSam,
    ActiveDirectory,
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn win32_error(stage: &str) -> String {
    let code = unsafe { GetLastError() };
    format!(
        "{stage}: Win32={code} ({})",
        std::io::Error::from_raw_os_error(code as i32)
    )
}

fn check(ok: i32, stage: &str) -> Result<(), String> {
    if ok == 0 {
        Err(win32_error(stage))
    } else {
        Ok(())
    }
}

fn check_status(status: i32, substatus: i32, stage: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        let code = unsafe { LsaNtStatusToWinError(status) };
        Err(format!(
            "{stage}: NTSTATUS=0x{:08x}, SubStatus=0x{:08x}, Win32={code}",
            status as u32, substatus as u32
        ))
    }
}

fn information(token: HANDLE, kind: i32) -> Result<InformationBuffer, String> {
    let mut needed = 0;
    let sized = unsafe { GetTokenInformation(token, kind, null_mut(), 0, &mut needed) };
    if sized == 0 {
        let error = unsafe { GetLastError() };
        if !matches!(error, 122 | 24) || needed == 0 {
            return Err(win32_error(&format!("size token information {kind}")));
        }
    }
    if needed == 0 {
        return Err(format!("token information {kind} has zero length"));
    }
    let mut buffer = InformationBuffer::new(needed as usize);
    check(
        unsafe { GetTokenInformation(token, kind, buffer.pointer_mut(), needed, &mut needed) },
        &format!("read token information {kind}"),
    )?;
    Ok(buffer)
}

fn sid_text(sid: *const c_void) -> Result<String, String> {
    let mut text = null_mut();
    check(
        unsafe { ConvertSidToStringSidW(sid.cast_mut(), &mut text) },
        "format token SID",
    )?;
    let allocation = LocalAllocation(text.cast());
    let mut length = 0;
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    let result = String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
        .map_err(|error| format!("token SID text: {error}"));
    drop(allocation);
    result
}

fn local_sid(value: &str) -> Result<LocalAllocation, String> {
    let text = wide(value);
    let mut sid = null_mut();
    check(
        unsafe { ConvertStringSidToSidW(text.as_ptr(), &mut sid) },
        "parse token SID",
    )?;
    Ok(LocalAllocation(sid))
}

pub fn user_sid(token: HANDLE) -> Result<String, String> {
    let data = information(token, 1)?;
    if data.bytes < size_of::<SID_AND_ATTRIBUTES>() {
        return Err("TokenUser buffer is too short".into());
    }
    sid_text(unsafe { (*(data.pointer().cast::<SID_AND_ATTRIBUTES>())).Sid })
}

pub fn integrity_sid(token: HANDLE) -> Result<String, String> {
    let data = information(token, 25)?;
    if data.bytes < size_of::<SID_AND_ATTRIBUTES>() {
        return Err("TokenIntegrityLevel buffer is too short".into());
    }
    sid_text(unsafe { (*(data.pointer().cast::<SID_AND_ATTRIBUTES>())).Sid })
}

pub fn session_id(token: HANDLE) -> Result<u32, String> {
    token_scalar(token, 12)
}

pub fn authentication_id(token: HANDLE) -> Result<u64, String> {
    let data = information(token, 10)?;
    if data.bytes < 2 * size_of::<LUID>() {
        return Err("TokenStatistics buffer is too short for AuthenticationId".into());
    }
    let authentication = unsafe { &*data.pointer().cast::<LUID>().add(1) };
    Ok((u64::from(authentication.HighPart as u32) << 32) | u64::from(authentication.LowPart))
}

fn token_scalar(token: HANDLE, kind: i32) -> Result<u32, String> {
    let mut value = 0u32;
    let mut needed = 0;
    check(
        unsafe {
            GetTokenInformation(
                token,
                kind,
                (&mut value as *mut u32).cast(),
                size_of::<u32>() as u32,
                &mut needed,
            )
        },
        &format!("read scalar token information {kind}"),
    )?;
    if needed != size_of::<u32>() as u32 {
        return Err(format!(
            "token scalar information {kind} returned an unexpected length"
        ));
    }
    Ok(value)
}

pub fn is_restricted(token: HANDLE) -> Result<bool, String> {
    // A successful query distinguishes an invalid handle from an unrestricted token.
    token_scalar(token, 8)?;
    Ok(unsafe { IsTokenRestricted(token) } != 0)
}

pub fn is_app_container(token: HANDLE) -> Result<bool, String> {
    Ok(token_scalar(token, 29)? != 0)
}

pub fn is_elevated(token: HANDLE) -> Result<bool, String> {
    Ok(token_scalar(token, 20)? != 0)
}

fn utf16_z(value: &[u16]) -> Result<String, String> {
    let end = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    String::from_utf16(&value[..end]).map_err(|error| format!("account name UTF-16: {error}"))
}

fn caller_account_kind(
    user_sid: &str,
    user_name: &str,
    domain_name: &str,
    computer_name: &str,
) -> Result<CallerAccountKind, String> {
    if user_name.is_empty()
        || domain_name.is_empty()
        || user_name.contains('\0')
        || domain_name.contains('\0')
    {
        return Err("caller account lookup did not return a valid user and domain".into());
    }
    if capability_parts(user_sid).is_none() {
        return Err(
            "Sandbox helper supports local Windows users and Active Directory domain users; Microsoft Entra ID and other account types are not supported".into(),
        );
    }
    if domain_name.eq_ignore_ascii_case(computer_name) {
        return Ok(CallerAccountKind::LocalSam);
    }
    if ["AzureAD", "MicrosoftAccount", "NT AUTHORITY", "BUILTIN"]
        .iter()
        .any(|unsupported| domain_name.eq_ignore_ascii_case(unsupported))
    {
        return Err(
            "Sandbox helper requires a local Windows account or an Active Directory domain account; this account provider is not supported".into(),
        );
    }
    // Domain reachability is required when minting, not for the startup handshake.
    Ok(CallerAccountKind::ActiveDirectory)
}

fn active_directory_upn(identity: &CallerIdentity) -> Result<String, String> {
    let account = wide(&format!("{}\\{}", identity.domain_name, identity.user_name));
    let mut length = 0;
    let sized = unsafe { TranslateNameW(account.as_ptr(), 2, 8, null_mut(), &mut length) };
    if sized == 0 && !matches!(unsafe { GetLastError() }, 122 | 234) {
        return Err(win32_error(
            "resolve Active Directory caller UPN (domain controller required)",
        ));
    }
    if length < 2 || length > u32::from(u16::MAX) / 2 {
        return Err("Active Directory caller UPN exceeds UNICODE_STRING capacity".into());
    }
    let mut translated = vec![0; length as usize];
    if unsafe { TranslateNameW(account.as_ptr(), 2, 8, translated.as_mut_ptr(), &mut length) } == 0
    {
        return Err(win32_error(
            "resolve Active Directory caller UPN (domain controller required)",
        ));
    }
    let upn = utf16_z(&translated)?;
    if upn.is_empty() {
        return Err("Active Directory caller lookup returned an empty UPN".into());
    }
    Ok(upn)
}

fn s4u_logon_information(
    principal: &str,
    realm: Option<&str>,
) -> Result<InformationBuffer, String> {
    let encode = |value: &str| -> Result<Vec<u16>, String> {
        if value.contains('\0') {
            return Err("S4U account names must not contain a NUL character".into());
        }
        let encoded = wide(value);
        if encoded.len() > usize::from(u16::MAX) / 2 {
            return Err("S4U account name exceeds UNICODE_STRING capacity".into());
        }
        Ok(encoded)
    };
    if principal.is_empty() {
        return Err("S4U principal must not be empty".into());
    }
    let name = encode(principal)?;
    let domain = realm.map(encode).transpose()?;
    let header = size_of::<S4uLogon>();
    let total = header + (name.len() + domain.as_ref().map_or(0, Vec::len)) * 2;
    let mut information = InformationBuffer::new(total);
    let name_pointer = unsafe {
        information
            .pointer_mut()
            .cast::<u8>()
            .add(header)
            .cast::<u16>()
    };
    let domain_string = if let Some(domain) = &domain {
        let domain_pointer = unsafe { name_pointer.add(name.len()) };
        unsafe { std::ptr::copy_nonoverlapping(domain.as_ptr(), domain_pointer, domain.len()) };
        UnicodeString {
            length: ((domain.len() - 1) * 2) as u16,
            maximum_length: (domain.len() * 2) as u16,
            buffer: domain_pointer,
        }
    } else {
        UnicodeString {
            length: 0,
            maximum_length: 0,
            buffer: null_mut(),
        }
    };
    unsafe {
        std::ptr::copy_nonoverlapping(name.as_ptr(), name_pointer, name.len());
        information
            .pointer_mut()
            .cast::<S4uLogon>()
            .write(S4uLogon {
                // MsV1_0S4ULogon and KerbS4ULogon share this submit value/layout.
                message_type: 12,
                flags: 0,
                user_name: UnicodeString {
                    length: ((name.len() - 1) * 2) as u16,
                    maximum_length: (name.len() * 2) as u16,
                    buffer: name_pointer,
                },
                domain_name: domain_string,
            });
    }
    Ok(information)
}

pub fn caller_identity(caller: HANDLE) -> Result<CallerIdentity, String> {
    let user_sid = user_sid(caller)?;
    let sid = local_sid(&user_sid)?;
    let mut name_length = 0;
    let mut domain_length = 0;
    let mut use_type = 0;
    let sized = unsafe {
        LookupAccountSidW(
            null(),
            sid.0,
            null_mut(),
            &mut name_length,
            null_mut(),
            &mut domain_length,
            &mut use_type,
        )
    };
    if sized == 0 && unsafe { GetLastError() } != 122 {
        return Err(win32_error("size caller account lookup"));
    }
    if name_length == 0 || domain_length == 0 {
        return Err("caller account lookup did not return a user and domain".into());
    }
    let mut name = vec![0; name_length as usize];
    let mut domain = vec![0; domain_length as usize];
    check(
        unsafe {
            LookupAccountSidW(
                null(),
                sid.0,
                name.as_mut_ptr(),
                &mut name_length,
                domain.as_mut_ptr(),
                &mut domain_length,
                &mut use_type,
            )
        },
        "lookup caller account",
    )?;
    if use_type != 1 {
        return Err("S4U caller must resolve to a user account".into());
    }
    let user_name = utf16_z(&name)?;
    let domain_name = utf16_z(&domain)?;
    let mut computer = vec![0u16; 256];
    let mut length = computer.len() as u32;
    check(
        unsafe { GetComputerNameW(computer.as_mut_ptr(), &mut length) },
        "read local computer name",
    )?;
    let computer = utf16_z(&computer)?;
    let account_kind = caller_account_kind(&user_sid, &user_name, &domain_name, &computer)?;
    Ok(CallerIdentity {
        user_sid,
        user_name,
        domain_name,
        session_id: session_id(caller)?,
        account_kind,
    })
}

fn capability_parts(sid: &str) -> Option<[u32; 4]> {
    let mut pieces = sid.strip_prefix("S-1-5-21-")?.split('-');
    let mut result = [0; 4];
    for value in &mut result {
        let piece = pieces.next()?;
        if piece.is_empty()
            || !piece.bytes().all(|byte| byte.is_ascii_digit())
            || (piece.len() > 1 && piece.starts_with('0'))
        {
            return None;
        }
        *value = piece.parse().ok()?;
    }
    if pieces.next().is_some() {
        None
    } else {
        Some(result)
    }
}

/// Validate the existing synthetic root-capability namespace before native calls.
/// Provenance (fresh owner and roots derived from pinned paths) belongs to the server.
pub fn validate_capability_sids(roots: &[String], owner: &str, caller: &str) -> Result<(), String> {
    if roots.is_empty() || roots.len() > MAX_ROOT_SIDS {
        return Err("broker requires 1..=128 root capability SIDs".into());
    }
    let caller_domain = capability_parts(caller).map(|parts| [parts[0], parts[1], parts[2]]);
    let mut seen = HashSet::new();
    for sid in roots.iter().map(String::as_str).chain(Some(owner)) {
        let parts = capability_parts(sid).ok_or_else(|| {
            "capability SID must be canonical S-1-5-21 with four u32 components".to_string()
        })?;
        if sid == caller || caller_domain == Some([parts[0], parts[1], parts[2]]) {
            return Err("capability SID must not identify the caller or its SAM domain".into());
        }
        if !seen.insert(sid) {
            return Err("owner and root capability SIDs must be distinct and nonduplicated".into());
        }
    }
    Ok(())
}

fn group_entries(token: HANDLE, kind: i32) -> Result<Vec<(String, u32)>, String> {
    let data = information(token, kind)?;
    let offset = offset_of!(OneGroup, group);
    if data.bytes < offset {
        return Err(format!("token group information {kind} is too short"));
    }
    let count = unsafe { *data.pointer().cast::<u32>() } as usize;
    let bytes = count
        .checked_mul(size_of::<SID_AND_ATTRIBUTES>())
        .and_then(|bytes| bytes.checked_add(offset))
        .ok_or_else(|| "token group count overflow".to_string())?;
    if bytes > data.bytes {
        return Err(format!("token group information {kind} exceeds its buffer"));
    }
    let first = unsafe {
        data.pointer()
            .cast::<u8>()
            .add(offset)
            .cast::<SID_AND_ATTRIBUTES>()
    };
    (0..count)
        .map(|index| {
            let entry = unsafe { &*first.add(index) };
            Ok((sid_text(entry.Sid)?, entry.Attributes))
        })
        .collect()
}

fn enabled_for_allow(attributes: u32) -> bool {
    attributes & SE_GROUP_ENABLED != 0 && attributes & SE_GROUP_DENY_ONLY == 0
}

fn applicable_for_deny(attributes: u32) -> bool {
    attributes & (SE_GROUP_ENABLED | SE_GROUP_DENY_ONLY) != 0
}

fn missing_deny_groups(caller: &[(String, u32)], logon: &[(String, u32)]) -> Vec<String> {
    caller
        .iter()
        .filter(|(sid, attributes)| {
            applicable_for_deny(*attributes) && !logon.iter().any(|(other, _)| other == sid)
        })
        .map(|(sid, _)| sid.clone())
        .collect()
}

// S4U may introduce logon-type groups. They must not grant access beyond the
// authenticated caller. Enabled caller groups also participate in deny ACEs.
fn groups_to_disable(
    caller: &[(String, u32)],
    logon: &[(String, u32)],
    owner: &str,
) -> Result<Vec<String>, String> {
    let caller_groups: HashMap<_, _> = caller
        .iter()
        .map(|(sid, attributes)| (sid.as_str(), *attributes))
        .collect();
    if let Some(sid) = missing_deny_groups(caller, logon).first() {
        return Err(format!(
            "S4U token omitted caller deny-applicable group {sid}"
        ));
    }
    let mut disable = Vec::new();
    for (sid, attributes) in logon {
        if sid == owner {
            continue;
        }
        let original = caller_groups.get(sid.as_str()).copied();
        let lost_deny = original.is_some_and(|attributes| attributes & SE_GROUP_DENY_ONLY != 0)
            && attributes & SE_GROUP_DENY_ONLY == 0
            || original.is_some_and(applicable_for_deny) && !applicable_for_deny(*attributes);
        let new_allow = enabled_for_allow(*attributes) && !original.is_some_and(enabled_for_allow);
        if lost_deny || new_allow {
            disable.push(sid.clone());
        }
    }
    Ok(disable)
}

fn verify_owner_group(token: HANDLE, owner: &str) -> Result<(), String> {
    if !group_entries(token, 2)?.iter().any(|(sid, attributes)| {
        sid == owner
            && attributes & (SE_GROUP_ENABLED | SE_GROUP_OWNER)
                == (SE_GROUP_ENABLED | SE_GROUP_OWNER)
            && attributes & SE_GROUP_DENY_ONLY == 0
    }) {
        return Err("token did not retain the enabled owner-qualified fresh group".into());
    }
    Ok(())
}

fn default_dacl_descriptor(caller_sid: &str, owner_sid: &str) -> Result<LocalAllocation, String> {
    let value = wide(&format!(
        "D:P(A;;GA;;;SY)(A;;GA;;;{caller_sid})(A;;GA;;;{owner_sid})"
    ));
    let mut descriptor = null_mut();
    let mut size = 0;
    check(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                value.as_ptr(),
                1,
                &mut descriptor,
                &mut size,
            )
        },
        "parse broker token default DACL",
    )?;
    Ok(LocalAllocation(descriptor))
}

fn set_default_dacl(token: HANDLE, caller_sid: &str, owner_sid: &str) -> Result<(), String> {
    let descriptor = default_dacl_descriptor(caller_sid, owner_sid)?;
    let mut present = 0;
    let mut defaulted = 0;
    let mut acl = null_mut();
    check(
        unsafe { GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted) },
        "extract broker token default DACL",
    )?;
    if present == 0 || acl.is_null() {
        return Err("broker token default DACL must be explicit and non-NULL".into());
    }
    check(
        unsafe {
            SetTokenInformation(
                token,
                6,
                (&acl as *const *mut ACL).cast(),
                size_of_val(&acl) as u32,
            )
        },
        "set broker token default DACL",
    )
}

fn set_medium_integrity(token: HANDLE) -> Result<(), String> {
    let sid = local_sid(MEDIUM_SID)?;
    let sid_length = unsafe { windows_sys::Win32::Security::GetLengthSid(sid.0) };
    if sid_length == 0 {
        return Err(win32_error("size medium integrity SID"));
    }
    let header = size_of::<SID_AND_ATTRIBUTES>();
    let mut data = InformationBuffer::new(header + sid_length as usize);
    let embedded = unsafe { data.pointer_mut().cast::<u8>().add(header).cast() };
    check(
        unsafe { CopySid(sid_length, embedded, sid.0) },
        "copy medium integrity SID",
    )?;
    unsafe {
        data.pointer_mut()
            .cast::<SID_AND_ATTRIBUTES>()
            .write(SID_AND_ATTRIBUTES {
                Sid: embedded,
                Attributes: 0x20,
            })
    };
    check(
        unsafe { SetTokenInformation(token, 25, data.pointer(), data.bytes as u32) },
        "lower broker token integrity to medium",
    )?;
    if integrity_sid(token)? != MEDIUM_SID {
        return Err("broker token integrity did not become exactly medium".into());
    }
    Ok(())
}

fn enable_only_change_notify(token: HANDLE) -> Result<(), String> {
    let name = wide("SeChangeNotifyPrivilege");
    let mut luid: LUID = unsafe { zeroed() };
    check(
        unsafe { LookupPrivilegeValueW(null(), name.as_ptr(), &mut luid) },
        "lookup SeChangeNotifyPrivilege",
    )?;
    let mut state: TOKEN_PRIVILEGES = unsafe { zeroed() };
    state.PrivilegeCount = 1;
    state.Privileges[0].Luid = luid;
    state.Privileges[0].Attributes = 2;
    unsafe { SetLastError(0) };
    check(
        unsafe { AdjustTokenPrivileges(token, 0, &state, 0, null_mut(), null_mut()) },
        "enable SeChangeNotifyPrivilege",
    )?;
    if unsafe { GetLastError() } != 0 {
        return Err(win32_error("assign SeChangeNotifyPrivilege"));
    }
    let data = information(token, 3)?;
    let offset = offset_of!(TOKEN_PRIVILEGES, Privileges);
    if data.bytes < offset {
        return Err("token privileges buffer is too short".into());
    }
    let count = unsafe { *data.pointer().cast::<u32>() } as usize;
    let stride = size_of_val(&state.Privileges[0]);
    if count
        .checked_mul(stride)
        .and_then(|bytes| bytes.checked_add(offset))
        .is_none_or(|bytes| bytes > data.bytes)
    {
        return Err("token privilege count exceeds buffer".into());
    }
    let first = unsafe {
        data.pointer()
            .cast::<u8>()
            .add(offset)
            .cast::<windows_sys::Win32::Security::LUID_AND_ATTRIBUTES>()
    };
    for index in 0..count {
        let entry = unsafe { &*first.add(index) };
        if entry.Attributes & 2 != 0
            && (entry.Luid.LowPart != luid.LowPart || entry.Luid.HighPart != luid.HighPart)
        {
            return Err(
                "broker token has an enabled privilege other than SeChangeNotifyPrivilege".into(),
            );
        }
    }
    Ok(())
}

pub fn mint_token(
    caller: HANDLE,
    root_capability_sids: &[String],
    owner_sid: &str,
) -> Result<OwnedToken, String> {
    if is_elevated(caller)?
        || is_restricted(caller)?
        || is_app_container(caller)?
        || integrity_sid(caller)? != MEDIUM_SID
    {
        return Err(
            "broker caller must be an unrestricted, non-elevated, medium-integrity desktop token"
                .into(),
        );
    }
    let identity = caller_identity(caller)?;
    validate_capability_sids(root_capability_sids, owner_sid, &identity.user_sid)?;
    let caller_groups = group_entries(caller, 2)?;
    for sid in root_capability_sids
        .iter()
        .map(String::as_str)
        .chain(Some(owner_sid))
    {
        if caller_groups.iter().any(|(group, _)| group == sid) {
            return Err("root/owner capability must not be an existing caller group".into());
        }
    }
    let owner = local_sid(owner_sid)?;
    let principal = match identity.account_kind {
        CallerAccountKind::LocalSam => identity.user_name.clone(),
        CallerAccountKind::ActiveDirectory => active_directory_upn(&identity)?,
    };
    let logon_information = s4u_logon_information(
        &principal,
        (identity.account_kind == CallerAccountKind::LocalSam).then_some("."),
    )?;
    let mut process_bytes = b"moyAI sandbox broker\0".to_vec();
    let mut package_bytes = match identity.account_kind {
        CallerAccountKind::LocalSam => b"MICROSOFT_AUTHENTICATION_PACKAGE_V1_0\0".as_slice(),
        CallerAccountKind::ActiveDirectory => b"Kerberos\0".as_slice(),
    }
    .to_vec();
    let mut origin_bytes = b"moyAI workspace-write\0".to_vec();
    let lsa_string = |value: &mut Vec<u8>| LsaString {
        length: (value.len() - 1) as u16,
        maximum_length: value.len() as u16,
        buffer: value.as_mut_ptr(),
    };
    let process_name = lsa_string(&mut process_bytes);
    let package_name = lsa_string(&mut package_bytes);
    let origin_name = lsa_string(&mut origin_bytes);
    let mut handle = null_mut();
    let mut mode = 0;
    check_status(
        unsafe { LsaRegisterLogonProcess(&process_name, &mut handle, &mut mode) },
        0,
        "register sandbox broker logon process",
    )?;
    let connection = LsaConnection(handle);
    let mut package = 0;
    check_status(
        unsafe { LsaLookupAuthenticationPackage(connection.0, &package_name, &mut package) },
        0,
        "lookup S4U authentication package",
    )?;
    let logon_stage = match identity.account_kind {
        CallerAccountKind::LocalSam => "local S4U with fresh owner group",
        CallerAccountKind::ActiveDirectory => {
            "Active Directory Kerberos S4U (domain controller and user group-read access required)"
        }
    };
    let groups = OneGroup {
        count: 1,
        group: SID_AND_ATTRIBUTES {
            Sid: owner.0,
            Attributes: OWNER_GROUP_ATTRIBUTES,
        },
    };
    let mut source = TokenSource {
        name: *b"moyAI-sb",
        identifier: unsafe { zeroed() },
    };
    check(
        unsafe { AllocateLocallyUniqueId(&mut source.identifier) },
        "allocate broker token source LUID",
    )?;
    let logon_with_groups = |groups: *const OneGroup| -> Result<OwnedToken, String> {
        let mut profile = null_mut();
        let mut profile_length = 0;
        let mut logon_id = unsafe { zeroed() };
        let mut quotas = unsafe { zeroed() };
        let mut raw = null_mut();
        let mut substatus = 0;
        let status = unsafe {
            LsaLogonUser(
                connection.0,
                &origin_name,
                3,
                package,
                logon_information.pointer(),
                logon_information.bytes as u32,
                groups,
                &source,
                &mut profile,
                &mut profile_length,
                &mut logon_id,
                &mut raw,
                &mut quotas,
                &mut substatus,
            )
        };
        let _profile = LsaProfile(profile);
        if status != 0 {
            if !raw.is_null() {
                unsafe { CloseHandle(raw) };
            }
            check_status(status, substatus, logon_stage)?;
        }
        let token = OwnedToken::from_raw(raw, "S4U token handle")?;
        if user_sid(token.raw())? != identity.user_sid {
            return Err("S4U returned a different caller User SID".into());
        }
        verify_owner_group(token.raw(), owner_sid)?;
        Ok(token)
    };
    let mut logon = logon_with_groups(&groups)?;
    let missing = missing_deny_groups(&caller_groups, &group_entries(logon.raw(), 2)?);
    if !missing.is_empty() {
        let missing_sids: Vec<_> = missing
            .iter()
            .map(|value| local_sid(value))
            .collect::<Result<_, _>>()?;
        let count = missing_sids
            .len()
            .checked_add(1)
            .ok_or("S4U LocalGroups count overflow")?;
        let offset = offset_of!(OneGroup, group);
        let bytes = count
            .checked_mul(size_of::<SID_AND_ATTRIBUTES>())
            .and_then(|size| size.checked_add(offset))
            .ok_or("S4U LocalGroups size overflow")?;
        let mut retained = InformationBuffer::new(bytes);
        unsafe {
            retained
                .pointer_mut()
                .cast::<u32>()
                .write(u32::try_from(count).map_err(|_| "S4U LocalGroups count is too large")?)
        };
        let entries = unsafe {
            retained
                .pointer_mut()
                .cast::<u8>()
                .add(offset)
                .cast::<SID_AND_ATTRIBUTES>()
        };
        unsafe { entries.write(groups.group) };
        for (index, sid) in missing_sids.iter().enumerate() {
            // A missing caller group is restored only for deny ACE evaluation.
            unsafe {
                entries.add(index + 1).write(SID_AND_ATTRIBUTES {
                    Sid: sid.0,
                    Attributes: SE_GROUP_DENY_ONLY,
                })
            };
        }
        logon = logon_with_groups(retained.pointer().cast())?;
    }
    let mut disable_values =
        groups_to_disable(&caller_groups, &group_entries(logon.raw(), 2)?, owner_sid)?;
    for sid in &missing {
        if !disable_values.contains(sid) {
            disable_values.push(sid.clone());
        }
    }
    let disabled_sids: Vec<_> = disable_values
        .iter()
        .map(|value| local_sid(value))
        .collect::<Result<_, _>>()?;
    let disabled: Vec<_> = disabled_sids
        .iter()
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid.0,
            Attributes: 0,
        })
        .collect();
    let mut raw = null_mut();
    check(
        unsafe { DuplicateTokenEx(logon.raw(), TOKEN_ALL_ACCESS, null(), 2, 1, &mut raw) },
        "duplicate broker primary token",
    )?;
    let primary = OwnedToken::from_raw(raw, "broker primary token handle")?;
    let sid_values: Vec<_> = root_capability_sids
        .iter()
        .map(String::as_str)
        .chain([owner_sid, WORLD_SID])
        .collect();
    let sids: Vec<_> = sid_values
        .iter()
        .map(|value| local_sid(value))
        .collect::<Result<_, _>>()?;
    let entries: Vec<_> = sids
        .iter()
        .map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid.0,
            Attributes: 0,
        })
        .collect();
    let mut raw = null_mut();
    check(
        unsafe {
            CreateRestrictedToken(
                primary.raw(),
                RESTRICTED_FLAGS,
                disabled.len() as u32,
                if disabled.is_empty() {
                    null()
                } else {
                    disabled.as_ptr()
                },
                0,
                null(),
                entries.len() as u32,
                entries.as_ptr(),
                &mut raw,
            )
        },
        "create broker workspace-write restricted token",
    )?;
    let token = OwnedToken::from_raw(raw, "broker restricted token handle")?;
    if !is_restricted(token.raw())? {
        return Err("broker token is not restricted".into());
    }
    verify_owner_group(token.raw(), owner_sid)?;
    let final_groups = group_entries(token.raw(), 2)?;
    if !groups_to_disable(&caller_groups, &final_groups, owner_sid)?.is_empty() {
        return Err("restricted S4U token exceeded the caller normal-group access ceiling".into());
    }
    for sid in &missing {
        if !final_groups.iter().any(|(group, attributes)| {
            group == sid
                && attributes & SE_GROUP_DENY_ONLY != 0
                && attributes & SE_GROUP_ENABLED == 0
        }) {
            return Err("restored caller identity did not remain deny-only".into());
        }
    }
    let owner_pointer = owner.0;
    check(
        unsafe {
            SetTokenInformation(
                token.raw(),
                4,
                (&owner_pointer as *const *mut c_void).cast(),
                size_of_val(&owner_pointer) as u32,
            )
        },
        "set fresh broker TokenOwner",
    )?;
    check(
        unsafe {
            SetTokenInformation(
                token.raw(),
                12,
                (&identity.session_id as *const u32).cast(),
                size_of::<u32>() as u32,
            )
        },
        "set broker caller session",
    )?;
    set_medium_integrity(token.raw())?;
    set_default_dacl(token.raw(), &identity.user_sid, owner_sid)?;
    enable_only_change_notify(token.raw())?;
    let owner_data = information(token.raw(), 4)?;
    if owner_data.bytes < size_of::<*mut c_void>()
        || sid_text(unsafe { *owner_data.pointer().cast::<*mut c_void>() })? != owner_sid
    {
        return Err("broker token did not retain its requested owner".into());
    }
    let actual: HashSet<_> = group_entries(token.raw(), 11)?
        .into_iter()
        .map(|(sid, _)| sid)
        .collect();
    let expected: HashSet<_> = sid_values.into_iter().map(str::to_string).collect();
    if actual != expected {
        return Err("broker token restricting SID set changed".into());
    }
    if user_sid(token.raw())? != identity.user_sid
        || session_id(token.raw())? != identity.session_id
    {
        return Err("broker token caller identity/session changed".into());
    }
    if is_elevated(token.raw())? || is_app_container(token.raw())? {
        return Err("broker token must remain non-elevated and outside AppContainer mode".into());
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::{
        CallerAccountKind, S4uLogon, SE_GROUP_DENY_ONLY, SE_GROUP_ENABLED, caller_account_kind,
        default_dacl_descriptor, groups_to_disable, missing_deny_groups, s4u_logon_information,
        sid_text, validate_capability_sids,
    };
    use std::collections::HashSet;
    use std::mem::size_of;
    use std::ptr::null_mut;
    use windows_sys::Win32::Security::{ACCESS_ALLOWED_ACE, GetAce, GetSecurityDescriptorDacl};
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;

    fn roots() -> Vec<String> {
        vec!["S-1-5-21-10-20-30-40".into()]
    }
    const CALLER: &str = "S-1-5-21-100-200-300-1001";
    const OWNER: &str = "S-1-5-21-50-60-70-80";

    #[test]
    fn accepts_local_and_ad_domain_users_without_reclassifying_local_backed_accounts() {
        assert_eq!(
            caller_account_kind(CALLER, "alice", "workstation", "WORKSTATION").unwrap(),
            CallerAccountKind::LocalSam
        );
        assert_eq!(
            caller_account_kind(CALLER, "alice", "CORP", "WORKSTATION").unwrap(),
            CallerAccountKind::ActiveDirectory
        );
    }

    #[test]
    fn rejects_cloud_providers_non_user_sid_namespaces_and_invalid_account_names() {
        for sid in [
            "S-1-12-1-1-2-3-4",
            "S-1-5-18",
            "S-1-5-32-544",
            "S-1-5-21-1-2-3",
        ] {
            assert!(caller_account_kind(sid, "alice", "CORP", "WORKSTATION").is_err());
        }
        for domain in [
            "AzureAD",
            "azuread",
            "MicrosoftAccount",
            "NT AUTHORITY",
            "BUILTIN",
        ] {
            assert!(caller_account_kind(CALLER, "alice", domain, "WORKSTATION").is_err());
        }
        for (user, domain) in [
            ("", "CORP"),
            ("alice", ""),
            ("alice\0bob", "CORP"),
            ("alice", "CORP\0"),
        ] {
            assert!(caller_account_kind(CALLER, user, domain, "WORKSTATION").is_err());
        }
    }

    #[test]
    fn s4u_request_keeps_utf16_strings_contiguous_and_omits_unknown_ad_realm() {
        for (principal, realm) in [
            ("alice", Some(".")),
            ("利用者@example.test", None),
            ("alice", Some("部署.example")),
        ] {
            let information = s4u_logon_information(principal, realm).unwrap();
            let logon = unsafe { &*information.pointer().cast::<S4uLogon>() };
            assert_eq!(logon.message_type, 12);
            assert_eq!(logon.flags, 0);
            let expected: Vec<_> = principal.encode_utf16().collect();
            assert_eq!(usize::from(logon.user_name.length), expected.len() * 2);
            assert_eq!(logon.user_name.maximum_length, logon.user_name.length + 2);
            assert_eq!(
                logon.user_name.buffer.cast::<u8>(),
                unsafe {
                    information
                        .pointer()
                        .cast::<u8>()
                        .add(size_of::<S4uLogon>())
                }
                .cast_mut()
            );
            assert_eq!(
                unsafe { std::slice::from_raw_parts(logon.user_name.buffer, expected.len()) },
                expected
            );
            assert_eq!(unsafe { *logon.user_name.buffer.add(expected.len()) }, 0);
            if let Some(realm) = realm {
                let expected_realm: Vec<_> = realm.encode_utf16().collect();
                assert_eq!(
                    usize::from(logon.domain_name.length),
                    expected_realm.len() * 2
                );
                assert_eq!(
                    logon.domain_name.maximum_length,
                    logon.domain_name.length + 2
                );
                assert_eq!(logon.domain_name.buffer, unsafe {
                    logon.user_name.buffer.add(expected.len() + 1)
                });
                assert_eq!(
                    unsafe {
                        std::slice::from_raw_parts(logon.domain_name.buffer, expected_realm.len())
                    },
                    expected_realm
                );
                assert_eq!(
                    unsafe { *logon.domain_name.buffer.add(expected_realm.len()) },
                    0
                );
            } else {
                assert!(logon.domain_name.buffer.is_null());
                assert_eq!(logon.domain_name.length, 0);
                assert_eq!(logon.domain_name.maximum_length, 0);
            }
        }
    }

    #[test]
    fn s4u_request_rejects_empty_principal_embedded_nuls_and_oversized_unicode_strings() {
        assert!(s4u_logon_information("", None).is_err());
        assert!(s4u_logon_information("alice\0bob", None).is_err());
        assert!(s4u_logon_information("alice", Some("CORP\0")).is_err());
        let oversized = "x".repeat(usize::from(u16::MAX) / 2);
        assert!(s4u_logon_information(&oversized, None).is_err());
        assert!(s4u_logon_information("alice", Some(&oversized)).is_err());
        let maximum = "x".repeat(usize::from(u16::MAX) / 2 - 1);
        assert!(s4u_logon_information(&maximum, Some(&maximum)).is_ok());
    }

    #[test]
    fn accepts_distinct_synthetic_capabilities() {
        assert!(validate_capability_sids(&roots(), OWNER, CALLER).is_ok());
    }

    #[test]
    fn rejects_well_known_aliases_and_noncanonical_components() {
        for owner in [
            "WD",
            "BA",
            "S-1-1-0",
            "S-1-3-4",
            "S-1-5-18",
            "S-1-5-32-544",
            "S-1-5-5-1-2",
            "S-1-16-8192",
            "S-1-5-21-1-2-3",
            "S-1-5-21-1-2-3-4-5",
            "S-1-5-21-01-2-3-4",
            "S-1-5-21-1-2-3-4294967296",
            "S-1-5-21-1-2-3-4\0",
        ] {
            assert!(
                validate_capability_sids(&roots(), owner, CALLER).is_err(),
                "{owner:?}"
            );
        }
    }

    #[test]
    fn rejects_caller_domain_and_duplicate_authorities() {
        assert!(validate_capability_sids(&roots(), CALLER, CALLER).is_err());
        assert!(validate_capability_sids(&roots(), "S-1-5-21-100-200-300-999", CALLER).is_err());
        assert!(validate_capability_sids(&roots(), &roots()[0], CALLER).is_err());
        assert!(
            validate_capability_sids(&[roots()[0].clone(), roots()[0].clone()], OWNER, CALLER)
                .is_err()
        );
        assert!(validate_capability_sids(&[], OWNER, CALLER).is_err());
    }

    #[test]
    fn validates_root_sids_and_limits_the_request() {
        assert!(validate_capability_sids(&["S-1-1-0".into()], OWNER, CALLER).is_err());
        let roots: Vec<_> = (0..129)
            .map(|index| format!("S-1-5-21-10-20-30-{index}"))
            .collect();
        assert!(validate_capability_sids(&roots, OWNER, CALLER).is_err());
    }

    #[test]
    fn disables_new_s4u_allow_groups_and_preserves_caller_deny_groups() {
        let caller = vec![
            ("shared".into(), SE_GROUP_ENABLED),
            ("disabled".into(), 0),
            ("deny".into(), SE_GROUP_DENY_ONLY),
        ];
        let logon = vec![
            ("shared".into(), SE_GROUP_ENABLED),
            ("disabled".into(), SE_GROUP_ENABLED),
            ("deny".into(), SE_GROUP_ENABLED),
            ("network".into(), SE_GROUP_ENABLED),
            (OWNER.into(), 0xF),
        ];
        assert_eq!(
            groups_to_disable(&caller, &logon, OWNER).unwrap(),
            vec!["disabled", "deny", "network"]
        );
        let final_groups = vec![
            ("shared".into(), SE_GROUP_ENABLED),
            ("disabled".into(), SE_GROUP_DENY_ONLY),
            ("deny".into(), SE_GROUP_DENY_ONLY),
            ("network".into(), SE_GROUP_DENY_ONLY),
            (OWNER.into(), 0xF),
        ];
        assert!(
            groups_to_disable(&caller, &final_groups, OWNER)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn fails_closed_if_s4u_loses_a_caller_deny_applicable_identity() {
        for attributes in [SE_GROUP_ENABLED, SE_GROUP_DENY_ONLY] {
            let caller = vec![("deny-applicable".into(), attributes)];
            let logon = vec![(OWNER.into(), 0xF)];
            assert!(groups_to_disable(&caller, &logon, OWNER).is_err());
        }
    }

    #[test]
    fn restores_only_missing_deny_applicable_groups_without_duplicate_logon_groups() {
        let caller = vec![
            ("interactive".into(), SE_GROUP_ENABLED),
            ("console".into(), SE_GROUP_ENABLED),
            ("shared".into(), SE_GROUP_ENABLED),
            ("deny".into(), SE_GROUP_DENY_ONLY),
            ("inactive".into(), 0),
        ];
        let logon = vec![
            ("shared".into(), SE_GROUP_ENABLED),
            ("deny".into(), SE_GROUP_DENY_ONLY),
        ];
        assert_eq!(
            missing_deny_groups(&caller, &logon),
            vec!["interactive", "console"]
        );
        let retained = vec![
            ("shared".into(), SE_GROUP_ENABLED),
            ("deny".into(), SE_GROUP_DENY_ONLY),
            ("interactive".into(), SE_GROUP_DENY_ONLY),
            ("console".into(), SE_GROUP_DENY_ONLY),
        ];
        assert!(
            groups_to_disable(&caller, &retained, OWNER)
                .unwrap()
                .is_empty()
        );
        let inactive = vec![("shared".into(), 0)];
        assert_eq!(
            groups_to_disable(&[("shared".into(), SE_GROUP_ENABLED)], &inactive, OWNER).unwrap(),
            vec!["shared"]
        );
    }

    #[test]
    fn default_dacl_allows_only_system_literal_caller_and_fresh_owner() {
        let descriptor = default_dacl_descriptor(CALLER, OWNER).unwrap();
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = null_mut();
        assert_ne!(
            unsafe {
                GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted)
            },
            0
        );
        assert_ne!(present, 0);
        assert!(!acl.is_null());
        let count = unsafe { (*acl).AceCount };
        assert_eq!(count, 3);
        let mut allowed = HashSet::new();
        for index in 0..u32::from(count) {
            let mut raw = null_mut();
            assert_ne!(unsafe { GetAce(acl, index, &mut raw) }, 0);
            let ace = unsafe { &*raw.cast::<ACCESS_ALLOWED_ACE>() };
            assert_eq!(ace.Header.AceType, ACCESS_ALLOWED_ACE_TYPE);
            assert_eq!(ace.Header.AceFlags, 0);
            assert_eq!(ace.Mask, 0x1000_0000);
            let sid = sid_text((&ace.SidStart as *const u32).cast()).unwrap();
            assert!(allowed.insert(sid));
        }
        assert_eq!(
            allowed,
            HashSet::from([
                "S-1-5-18".to_string(),
                CALLER.to_string(),
                OWNER.to_string()
            ])
        );
    }
}
