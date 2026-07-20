use std::collections::HashMap;

use camino::Utf8PathBuf;
use tokio_util::sync::CancellationToken;

use crate::tool::os_sandbox::WorkspaceWriteSandboxProfile;
use crate::tool::truncate::BoundedPipeOutput;

pub(crate) fn captured_process_environment(
    shell: &crate::config::ShellConfig,
) -> HashMap<String, String> {
    shell
        .env_allowlist
        .iter()
        .filter_map(|key| {
            std::env::var_os(key).map(|value| (key.clone(), value.to_string_lossy().into_owned()))
        })
        .collect()
}

#[derive(Debug)]
pub(crate) struct SandboxedProcessRequest {
    pub argv: Vec<String>,
    pub cwd: Utf8PathBuf,
    pub environment: HashMap<String, String>,
    pub stdin: Vec<u8>,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
    pub hide_window: bool,
    pub cancel: CancellationToken,
}

#[derive(Debug)]
pub(crate) struct SandboxedProcessOutput {
    pub exit_code: Option<i32>,
    pub stdout: BoundedPipeOutput,
    pub stderr: BoundedPipeOutput,
    pub timed_out: bool,
    pub cancelled: bool,
    pub effect_started: bool,
    pub cleanup_errors: Vec<String>,
}

impl SandboxedProcessOutput {
    pub fn cleanup_error(&self) -> Option<String> {
        (!self.cleanup_errors.is_empty()).then(|| self.cleanup_errors.join("; "))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxExecutionError {
    #[cfg(not(windows))]
    #[error("OS sandbox is unavailable on this platform")]
    UnsupportedPlatform,
    #[error("OS sandbox profile is invalid: {0}")]
    InvalidProfile(String),
    #[error("OS sandbox initialization failed before process start: {0}")]
    Initialization(String),
    #[error("OS sandbox process launch failed: {0}")]
    Spawn(String),
    #[error("OS sandbox worker failed: {0}")]
    Worker(String),
}

pub(crate) async fn execute_workspace_write(
    profile: WorkspaceWriteSandboxProfile,
    request: SandboxedProcessRequest,
) -> Result<SandboxedProcessOutput, SandboxExecutionError> {
    #[cfg(windows)]
    {
        return tokio::task::spawn_blocking(move || windows::execute(profile, request))
            .await
            .map_err(|error| SandboxExecutionError::Worker(error.to_string()))?;
    }
    #[cfg(not(windows))]
    {
        let _ = (profile, request);
        Err(SandboxExecutionError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::{OsStr, c_void};
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle as _;
    use std::ptr::{null, null_mut};
    use std::time::{Duration, Instant};

    use camino::Utf8PathBuf;
    use sha2::{Digest, Sha256};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_NOT_FOUND, ERROR_SUCCESS, GetLastError, HANDLE, HANDLE_FLAG_INHERIT,
        HLOCAL, INVALID_HANDLE_VALUE, LUID, LocalFree, SetHandleInformation, WAIT_FAILED,
        WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Security::Authorization::{
        DENY_ACCESS, EXPLICIT_ACCESS_W, GRANT_ACCESS, GetSecurityInfo, SET_ACCESS,
        SetEntriesInAclW, SetSecurityInfo, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACCESS_DENIED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION,
        AclSizeInformation, AdjustTokenPrivileges, CopySid, CreateRestrictedToken,
        CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
        GetLengthSid, GetTokenInformation, InitializeSecurityDescriptor, IsTokenRestricted,
        LookupPrivilegeValueW, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SID_AND_ATTRIBUTES,
        SetSecurityDescriptorDacl, SetTokenInformation, TOKEN_ADJUST_DEFAULT,
        TOKEN_ADJUST_PRIVILEGES, TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
        TOKEN_PRIVILEGES, TOKEN_QUERY, TOKEN_USER, TokenDefaultDacl, TokenGroups,
        TokenRestrictedSids, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, DELETE, FILE_APPEND_DATA,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA, FileAttributeTagInfo, FileIdInfo,
        GetDriveTypeW, GetFileInformationByHandle, GetFileInformationByHandleEx,
        GetFinalPathNameByHandleW, OPEN_EXISTING, ReadFile, WriteFile,
    };
    use windows_sys::Win32::System::IO::CancelSynchronousIo;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS,
        JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_GLOBALATOMS, JOB_OBJECT_UILIMIT_HANDLES,
        JOB_OBJECT_UILIMIT_READCLIPBOARD, JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
        JOB_OBJECT_UILIMIT_WRITECLIPBOARD, JOBOBJECT_BASIC_UI_RESTRICTIONS,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicUIRestrictions,
        JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::{
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
        CreateProcessAsUserW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
        GetCurrentProcess, GetExitCodeProcess, InitializeProcThreadAttributeList,
        PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
        UpdateProcThreadAttribute, WaitForSingleObject,
    };

    use super::{
        SandboxExecutionError, SandboxedProcessOutput, SandboxedProcessRequest,
        WorkspaceWriteSandboxProfile,
    };
    use crate::tool::os_sandbox::{
        MAX_PINNED_PROTECTED_FILE_BYTES, SandboxPathSnapshot, WindowsSandboxObjectIdentity,
    };
    use crate::tool::truncate::BoundedPipeOutput;

    const DISABLE_MAX_PRIVILEGE: u32 = 0x01;
    const LUA_TOKEN: u32 = 0x04;
    const WRITE_RESTRICTED: u32 = 0x08;
    const WIN_WORLD_SID: i32 = 1;
    const WIN_LOCAL_SYSTEM_SID: i32 = 22;
    const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;
    const GENERIC_ALL: u32 = 0x1000_0000;
    const GENERIC_WRITE_MASK: u32 = 0x4000_0000;
    const READ_CONTROL: u32 = 0x0002_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    const WRITE_OWNER: u32 = 0x0008_0000;
    const SE_FILE_OBJECT: i32 = 1;
    const CONTAINER_AND_OBJECT_INHERIT: u32 = 0x03;
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_RAMDISK: u32 = 6;
    const NO_PROPAGATE_INHERIT_ACE: u8 = 0x04;
    const INHERIT_ONLY_ACE: u8 = 0x08;
    const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: usize = 0x0002_0002;
    const PROCESS_REAP_TIMEOUT: Duration = Duration::from_secs(3);
    const WORKER_DRAIN_GRACE: Duration = Duration::from_millis(250);
    const WORKER_CANCEL_TIMEOUT: Duration = Duration::from_secs(1);

    #[repr(C)]
    struct TokenDefaultDaclInfo {
        default_dacl: *mut ACL,
    }

    struct ProcessObjectSecurity {
        descriptor: SECURITY_DESCRIPTOR,
        _dacl: LocalAcl,
    }

    impl ProcessObjectSecurity {
        fn system_only() -> Result<Self, SandboxExecutionError> {
            let mut system = system_sid()?;
            let entry = EXPLICIT_ACCESS_W {
                grfAccessPermissions: GENERIC_ALL,
                grfAccessMode: GRANT_ACCESS,
                grfInheritance: 0,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: null_mut(),
                    MultipleTrusteeOperation: 0,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_UNKNOWN,
                    ptstrName: system.as_mut_ptr().cast(),
                },
            };
            let mut dacl = null_mut();
            let status = unsafe { SetEntriesInAclW(1, &entry, null_mut(), &mut dacl) };
            if status != ERROR_SUCCESS {
                return Err(SandboxExecutionError::Initialization(format!(
                    "failed to build sandbox process-object DACL with {status}"
                )));
            }
            let dacl = LocalAcl(dacl);
            let mut descriptor: SECURITY_DESCRIPTOR = unsafe { zeroed() };
            if unsafe {
                InitializeSecurityDescriptor(
                    (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast::<c_void>(),
                    1,
                )
            } == 0
            {
                return Err(SandboxExecutionError::Initialization(format!(
                    "InitializeSecurityDescriptor failed for sandbox process objects: {}",
                    std::io::Error::last_os_error()
                )));
            }
            if unsafe {
                SetSecurityDescriptorDacl(
                    (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast::<c_void>(),
                    1,
                    dacl.0,
                    0,
                )
            } == 0
            {
                return Err(SandboxExecutionError::Initialization(format!(
                    "SetSecurityDescriptorDacl failed for sandbox process objects: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(Self {
                descriptor,
                _dacl: dacl,
            })
        }

        fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: (&mut self.descriptor as *mut SECURITY_DESCRIPTOR).cast(),
                bInheritHandle: 0,
            }
        }
    }

    struct OwnedHandle(HANDLE);

    unsafe impl Send for OwnedHandle {}

    impl OwnedHandle {
        fn new(handle: HANDLE, operation: &str) -> Result<Self, SandboxExecutionError> {
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return Err(SandboxExecutionError::Initialization(format!(
                    "{operation}: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(Self(handle))
        }

        fn raw(&self) -> HANDLE {
            self.0
        }
    }

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
    }

    struct LocalSid(*mut c_void);

    impl LocalSid {
        fn from_string(value: &str) -> Result<Self, SandboxExecutionError> {
            #[link(name = "advapi32")]
            unsafe extern "system" {
                fn ConvertStringSidToSidW(value: *const u16, sid: *mut *mut c_void) -> i32;
            }

            let value = to_wide(value);
            let mut sid = null_mut();
            if unsafe { ConvertStringSidToSidW(value.as_ptr(), &mut sid) } == 0 {
                return Err(SandboxExecutionError::Initialization(format!(
                    "failed to materialize capability SID `{}`: {}",
                    String::from_utf16_lossy(&value[..value.len().saturating_sub(1)]),
                    std::io::Error::last_os_error()
                )));
            }
            Ok(Self(sid))
        }

        fn raw(&self) -> *mut c_void {
            self.0
        }
    }

    impl Drop for LocalSid {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    LocalFree(self.0 as HLOCAL);
                }
            }
        }
    }

    struct AttributeList {
        storage: Vec<u8>,
    }

    impl AttributeList {
        fn for_handles(handles: &mut [HANDLE]) -> Result<Self, SandboxExecutionError> {
            let mut bytes = 0usize;
            unsafe {
                InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut bytes);
            }
            if bytes == 0 {
                return Err(SandboxExecutionError::Initialization(format!(
                    "failed to size inherited-handle allowlist: {}",
                    std::io::Error::last_os_error()
                )));
            }
            let mut storage = vec![0u8; bytes];
            let list = storage.as_mut_ptr().cast();
            if unsafe { InitializeProcThreadAttributeList(list, 1, 0, &mut bytes) } == 0 {
                return Err(SandboxExecutionError::Initialization(format!(
                    "failed to initialize inherited-handle allowlist: {}",
                    std::io::Error::last_os_error()
                )));
            }
            if unsafe {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                    handles.as_mut_ptr().cast(),
                    std::mem::size_of_val(handles),
                    null_mut(),
                    null_mut(),
                )
            } == 0
            {
                unsafe {
                    DeleteProcThreadAttributeList(list);
                }
                return Err(SandboxExecutionError::Initialization(format!(
                    "failed to set inherited-handle allowlist: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(Self { storage })
        }

        fn raw(&mut self) -> *mut c_void {
            self.storage.as_mut_ptr().cast()
        }
    }

    impl Drop for AttributeList {
        fn drop(&mut self) {
            unsafe {
                DeleteProcThreadAttributeList(self.storage.as_mut_ptr().cast());
            }
        }
    }

    pub(super) fn execute(
        profile: WorkspaceWriteSandboxProfile,
        mut request: SandboxedProcessRequest,
    ) -> Result<SandboxedProcessOutput, SandboxExecutionError> {
        if request.argv.is_empty() || request.argv[0].trim().is_empty() {
            return Err(SandboxExecutionError::InvalidProfile(
                "sandbox command argv must not be empty".to_string(),
            ));
        }
        if request.cancel.is_cancelled() {
            return Ok(SandboxedProcessOutput {
                exit_code: None,
                stdout: empty_output(),
                stderr: empty_output(),
                timed_out: false,
                cancelled: true,
                effect_started: false,
                cleanup_errors: Vec::new(),
            });
        }

        apply_advisory_offline_environment(&mut request.environment);
        let prepared = prepare_capabilities_and_acls(
            &profile,
            &request.cwd,
            &request.environment,
            &request.cancel,
        )?;
        if request.cancel.is_cancelled() {
            return Ok(cancelled_before_effect(Vec::new()));
        }
        let token = create_workspace_write_token(&prepared.roots)?;
        if request.cancel.is_cancelled() {
            return Ok(cancelled_before_effect(Vec::new()));
        }
        let output = spawn_and_capture(token, request);
        drop(prepared);
        output
    }

    struct RootCapability {
        root: SandboxPathSnapshot,
        sid: LocalSid,
        _handle: OwnedHandle,
    }

    struct PreparedSandbox {
        roots: Vec<RootCapability>,
        _protected_handles: Vec<OwnedHandle>,
        _audit_handles: Vec<OwnedHandle>,
    }

    fn prepare_capabilities_and_acls(
        profile: &WorkspaceWriteSandboxProfile,
        cwd: &camino::Utf8Path,
        environment: &std::collections::HashMap<String, String>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<PreparedSandbox, SandboxExecutionError> {
        if profile.writable_roots.is_empty() {
            return Err(SandboxExecutionError::InvalidProfile(
                "workspace-write profile has no writable roots".to_string(),
            ));
        }
        let mut roots = Vec::with_capacity(profile.writable_roots.len());
        for root in &profile.writable_roots {
            let handle = open_verified_acl_path(root)?;
            let sid = LocalSid::from_string(&capability_sid_for_root(root))?;
            update_opened_path_acl(
                handle.raw(),
                &root.requested,
                sid.raw(),
                AclChange::AllowWrite,
            )?;
            roots.push(RootCapability {
                root: root.clone(),
                sid,
                _handle: handle,
            });
        }
        let mut protected_handles = Vec::with_capacity(profile.read_only_roots.len());
        for protected in &profile.read_only_roots {
            let handle = open_verified_acl_path(protected)?;
            for root in &roots {
                // A protected object may live outside every writable root (for
                // example a worktree gitdir). Deny every active capability SID
                // unconditionally so ambient Everyone/logon grants cannot make
                // that exact authority writable through the restricted token.
                update_opened_path_acl(
                    handle.raw(),
                    &protected.requested,
                    root.sid.raw(),
                    AclChange::DenyWrite,
                )?;
            }
            protected_handles.push(handle);
        }
        let audit_handles =
            apply_world_writable_capability_denies(cwd, environment, &roots, cancel)?;
        Ok(PreparedSandbox {
            roots,
            _protected_handles: protected_handles,
            _audit_handles: audit_handles,
        })
    }

    const AUDIT_MAX_CANDIDATES: usize = 4_096;
    const AUDIT_TIME_LIMIT: Duration = Duration::from_secs(2);
    const AUDIT_SKIP_SUFFIXES: &[&str] = &[
        "\\windows\\installer",
        "\\windows\\registration",
        "\\programdata",
    ];

    fn apply_world_writable_capability_denies(
        cwd: &camino::Utf8Path,
        environment: &std::collections::HashMap<String, String>,
        roots: &[RootCapability],
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Vec<OwnedHandle>, SandboxExecutionError> {
        let started = Instant::now();
        let mut world = world_sid()?;
        let world_sid = world.as_mut_ptr().cast();
        let mut retained = Vec::new();
        for candidate in world_writable_audit_candidates(cwd, environment, started, cancel) {
            if cancel.is_cancelled() || started.elapsed() >= AUDIT_TIME_LIMIT {
                break;
            }
            let metadata = match std::fs::symlink_metadata(&candidate) {
                Ok(metadata) if metadata.is_dir() => metadata,
                Ok(_) => continue,
                Err(_) => continue,
            };
            use std::os::windows::fs::MetadataExt as _;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                continue;
            }
            let read_handle = match open_audit_candidate(&candidate, READ_CONTROL) {
                Ok(handle) => handle,
                Err(_) => continue,
            };
            let canonical = opened_identity_path(read_handle.raw(), &candidate)?;
            if !canonical_windows_path_is_local(&canonical) {
                continue;
            }
            if roots
                .iter()
                .any(|root| path_is_within(&canonical, &root.root.canonical))
            {
                continue;
            }
            if !opened_acl_allows_world_write(read_handle.raw(), &candidate, world_sid)? {
                continue;
            }
            let expected_identity = opened_object_identity(read_handle.raw())?;
            let write_handle = match open_audit_candidate(&candidate, READ_CONTROL | WRITE_DAC) {
                Ok(handle) => handle,
                Err(_) => continue,
            };
            let write_canonical = opened_identity_path(write_handle.raw(), &candidate)?;
            let write_identity = opened_object_identity(write_handle.raw())?;
            if !same_windows_path(&canonical, &write_canonical)
                || expected_identity != write_identity
            {
                return Err(SandboxExecutionError::Initialization(format!(
                    "Everyone-writable sandbox audit candidate `{candidate}` changed identity"
                )));
            }
            let mut deny_applied = true;
            for root in roots {
                if update_opened_path_acl(
                    write_handle.raw(),
                    &candidate,
                    root.sid.raw(),
                    AclChange::DenyWriteNonInheriting,
                )
                .is_err()
                {
                    deny_applied = false;
                    break;
                }
            }
            if !deny_applied {
                continue;
            }
            retained.push(write_handle);
        }
        Ok(retained)
    }

    fn world_writable_audit_candidates(
        cwd: &camino::Utf8Path,
        environment: &std::collections::HashMap<String, String>,
        started: Instant,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Vec<Utf8PathBuf> {
        let mut bases = vec![cwd.to_path_buf()];
        for key in ["TEMP", "TMP"] {
            if let Some(path) = environment
                .get(key)
                .cloned()
                .or_else(|| std::env::var(key).ok())
                .map(Utf8PathBuf::from)
            {
                bases.push(path);
            }
        }
        if let Some(path) = environment
            .get("PATH")
            .cloned()
            .or_else(|| std::env::var("PATH").ok())
        {
            bases.extend(
                std::env::split_paths(std::ffi::OsStr::new(&path))
                    .filter_map(|path| Utf8PathBuf::from_path_buf(path).ok()),
            );
        }
        for key in ["USERPROFILE", "PUBLIC", "SystemRoot"] {
            if let Some(path) = environment
                .get(key)
                .cloned()
                .or_else(|| std::env::var(key).ok())
                .map(Utf8PathBuf::from)
            {
                bases.push(path);
            }
        }
        if let Ok(system_drive) = std::env::var("SystemDrive") {
            bases.push(Utf8PathBuf::from(format!(
                "{}\\",
                system_drive.trim_end_matches(['\\', '/'])
            )));
        }

        let mut candidates = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for base in bases {
            if cancel.is_cancelled()
                || candidates.len() >= AUDIT_MAX_CANDIDATES
                || started.elapsed() >= AUDIT_TIME_LIMIT
            {
                break;
            }
            push_audit_candidate(&mut candidates, &mut seen, base);
        }
        candidates
    }

    fn push_audit_candidate(
        candidates: &mut Vec<Utf8PathBuf>,
        seen: &mut std::collections::HashSet<String>,
        path: Utf8PathBuf,
    ) {
        if !path.is_absolute() || !is_fixed_local_windows_path(&path) {
            return;
        }
        let key = normalize_windows_identity(path.as_str());
        if AUDIT_SKIP_SUFFIXES
            .iter()
            .any(|suffix| key.ends_with(suffix))
        {
            return;
        }
        if seen.insert(key) {
            candidates.push(path);
        }
    }

    fn is_fixed_local_windows_path(path: &camino::Utf8Path) -> bool {
        let bytes = path.as_str().as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || !matches!(bytes[2], b'\\' | b'/')
        {
            return false;
        }
        let root = format!("{}:\\", bytes[0] as char);
        let root = to_wide(root);
        matches!(
            unsafe { GetDriveTypeW(root.as_ptr()) },
            DRIVE_FIXED | DRIVE_RAMDISK
        )
    }

    fn canonical_windows_path_is_local(path: &camino::Utf8Path) -> bool {
        use std::path::{Component, Prefix};

        !matches!(
            path.as_std_path().components().next(),
            Some(Component::Prefix(prefix))
                if matches!(
                    prefix.kind(),
                    Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) | Prefix::DeviceNS(_)
                )
        )
    }

    fn open_audit_candidate(
        path: &camino::Utf8Path,
        desired_access: u32,
    ) -> Result<OwnedHandle, SandboxExecutionError> {
        let wide = to_wide(path.as_str());
        let handle = OwnedHandle::new(
            unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    desired_access | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    null_mut(),
                )
            },
            &format!("open sandbox audit candidate `{path}`"),
        )?;
        let mut attributes: FILE_ATTRIBUTE_TAG_INFO = unsafe { zeroed() };
        if unsafe {
            GetFileInformationByHandleEx(
                handle.raw(),
                FileAttributeTagInfo,
                (&mut attributes as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                u32::try_from(size_of::<FILE_ATTRIBUTE_TAG_INFO>())
                    .expect("file attribute tag info size fits u32"),
            )
        } == 0
        {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to classify sandbox audit candidate `{path}`: {}",
                std::io::Error::last_os_error()
            )));
        }
        if attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "sandbox audit candidate `{path}` is a reparse point"
            )));
        }
        Ok(handle)
    }

    fn opened_acl_allows_world_write(
        handle: HANDLE,
        path: &camino::Utf8Path,
        world_sid: *mut c_void,
    ) -> Result<bool, SandboxExecutionError> {
        let mut descriptor = null_mut();
        let mut dacl = null_mut();
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(SandboxExecutionError::Initialization(format!(
                "GetSecurityInfo failed for sandbox audit candidate `{path}` with {status}"
            )));
        }
        let _descriptor = LocalAllocation(descriptor);
        if dacl.is_null() {
            return Ok(true);
        }
        Ok(acl_has_any_write_allow(dacl, world_sid))
    }

    fn acl_has_any_write_allow(dacl: *mut ACL, sid: *mut c_void) -> bool {
        let mut info: ACL_SIZE_INFORMATION = unsafe { zeroed() };
        if unsafe {
            GetAclInformation(
                dacl,
                (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == 0
        {
            return false;
        }
        let write_mask = FILE_GENERIC_WRITE
            | FILE_WRITE_DATA
            | FILE_APPEND_DATA
            | FILE_WRITE_EA
            | FILE_WRITE_ATTRIBUTES
            | GENERIC_WRITE_MASK
            | DELETE
            | 0x0000_0040;
        for index in 0..info.AceCount {
            let mut raw_ace = null_mut();
            if unsafe { GetAce(dacl, index, &mut raw_ace) } == 0 {
                continue;
            }
            let header = unsafe { &*(raw_ace as *const ACE_HEADER) };
            if header.AceType != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags & INHERIT_ONLY_ACE != 0
            {
                continue;
            }
            let sid_ptr =
                (raw_ace as usize + size_of::<ACE_HEADER>() + size_of::<u32>()) as *mut c_void;
            if unsafe { EqualSid(sid_ptr, sid) } == 0 {
                continue;
            }
            let mask = unsafe { (*(raw_ace as *const ACCESS_ALLOWED_ACE)).Mask };
            if mask & write_mask != 0 {
                return true;
            }
        }
        false
    }

    fn capability_sid_for_root(root: &SandboxPathSnapshot) -> String {
        let path = root.requested.as_str().replace('/', "\\").to_lowercase();
        let identity = format!("{:?}", root.identity);
        let digest =
            Sha256::digest(format!("moyai.workspace-write.v2\0{path}\0{identity}").as_bytes());
        let mut parts = [0u32; 4];
        for (index, part) in parts.iter_mut().enumerate() {
            let start = index * 4;
            *part = u32::from_le_bytes(digest[start..start + 4].try_into().expect("four bytes"));
        }
        format!(
            "S-1-5-21-{}-{}-{}-{}",
            parts[0], parts[1], parts[2], parts[3]
        )
    }

    fn path_is_within(path: &camino::Utf8Path, root: &camino::Utf8Path) -> bool {
        let path = path.as_str().replace('/', "\\").to_lowercase();
        let root = root
            .as_str()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase();
        path == root
            || path
                .strip_prefix(&root)
                .is_some_and(|suffix| suffix.starts_with('\\'))
    }

    enum AclChange {
        AllowWrite,
        DenyWrite,
        DenyWriteNonInheriting,
    }

    impl AclChange {
        fn mask(&self) -> u32 {
            match self {
                Self::AllowWrite => {
                    FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE
                }
                Self::DenyWrite | Self::DenyWriteNonInheriting => {
                    GENERIC_ALL
                        | FILE_GENERIC_WRITE
                        | FILE_WRITE_DATA
                        | FILE_APPEND_DATA
                        | FILE_WRITE_EA
                        | FILE_WRITE_ATTRIBUTES
                        | GENERIC_WRITE_MASK
                        | WRITE_DAC
                        | WRITE_OWNER
                        | DELETE
                        | 0x0000_0040 // FILE_DELETE_CHILD
                }
            }
        }

        fn ace_type(&self) -> u8 {
            match self {
                Self::AllowWrite => ACCESS_ALLOWED_ACE_TYPE,
                Self::DenyWrite | Self::DenyWriteNonInheriting => ACCESS_DENIED_ACE_TYPE,
            }
        }

        fn access_mode(&self) -> i32 {
            match self {
                Self::AllowWrite => SET_ACCESS,
                Self::DenyWrite | Self::DenyWriteNonInheriting => DENY_ACCESS,
            }
        }

        fn inheritance(&self) -> u32 {
            match self {
                Self::AllowWrite | Self::DenyWrite => CONTAINER_AND_OBJECT_INHERIT,
                Self::DenyWriteNonInheriting => 0,
            }
        }
    }

    fn open_verified_acl_path(
        snapshot: &SandboxPathSnapshot,
    ) -> Result<OwnedHandle, SandboxExecutionError> {
        let path = &snapshot.requested;
        let wide = to_wide(path.as_str());
        let share_mode = if snapshot.content_sha256.is_some() {
            // Protected regular files remain open for the process lifetime.
            // Excluding write/delete sharing closes the post-hash in-place
            // rewrite window and makes a pre-existing writer fail closed.
            FILE_SHARE_READ
        } else {
            FILE_SHARE_READ | FILE_SHARE_WRITE
        };
        let handle = OwnedHandle::new(
            unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
                    share_mode,
                    null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                    null_mut(),
                )
            },
            &format!("open sandbox ACL path `{path}`"),
        )?;
        verify_opened_path(handle.raw(), &snapshot.canonical)?;
        let mut opened_attributes: FILE_ATTRIBUTE_TAG_INFO = unsafe { zeroed() };
        if unsafe {
            GetFileInformationByHandleEx(
                handle.raw(),
                FileAttributeTagInfo,
                (&mut opened_attributes as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                u32::try_from(size_of::<FILE_ATTRIBUTE_TAG_INFO>())
                    .expect("file attribute tag info size fits u32"),
            )
        } == 0
        {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to classify opened sandbox ACL path `{path}`: {}",
                std::io::Error::last_os_error()
            )));
        }
        if opened_attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "opened sandbox ACL path `{path}` is a reparse point"
            )));
        }
        let identity = opened_object_identity(handle.raw())?;
        if identity != snapshot.identity {
            return Err(SandboxExecutionError::Initialization(format!(
                "sandbox ACL path `{path}` changed object identity after permission admission"
            )));
        }
        if let Some(expected) = snapshot.content_sha256 {
            let actual = opened_file_content_sha256(handle.raw(), path)?;
            if actual != expected {
                return Err(SandboxExecutionError::Initialization(format!(
                    "protected sandbox file `{path}` changed contents after permission admission"
                )));
            }
        }
        Ok(handle)
    }

    fn opened_file_content_sha256(
        handle: HANDLE,
        path: &camino::Utf8Path,
    ) -> Result<[u8; 32], SandboxExecutionError> {
        let mut digest = Sha256::new();
        let mut total = 0u64;
        let mut buffer = [0u8; 8 * 1024];
        loop {
            let mut read = 0u32;
            if unsafe {
                ReadFile(
                    handle,
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    &mut read,
                    null_mut(),
                )
            } == 0
            {
                return Err(SandboxExecutionError::Initialization(format!(
                    "failed to read protected sandbox file `{path}` during launch verification: {}",
                    std::io::Error::last_os_error()
                )));
            }
            if read == 0 {
                break;
            }
            total = total.saturating_add(read as u64);
            if total > MAX_PINNED_PROTECTED_FILE_BYTES {
                return Err(SandboxExecutionError::Initialization(format!(
                    "protected sandbox file `{path}` exceeds {MAX_PINNED_PROTECTED_FILE_BYTES} bytes during launch verification"
                )));
            }
            digest.update(&buffer[..read as usize]);
        }
        Ok(digest.finalize().into())
    }

    fn update_opened_path_acl(
        handle: HANDLE,
        path: &camino::Utf8Path,
        sid: *mut c_void,
        change: AclChange,
    ) -> Result<(), SandboxExecutionError> {
        let mut descriptor = null_mut();
        let mut dacl = null_mut();
        let status = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(SandboxExecutionError::Initialization(format!(
                "GetSecurityInfo failed for `{path}` with {status}"
            )));
        }
        let descriptor = LocalAllocation(descriptor);
        if dacl.is_null() {
            return Err(SandboxExecutionError::Initialization(format!(
                "sandbox ACL path `{path}` has a NULL DACL and cannot be safely narrowed"
            )));
        }
        if acl_has_entry(
            dacl,
            sid,
            change.ace_type(),
            change.mask(),
            change.inheritance() as u8,
        ) {
            return Ok(());
        }

        let mut entry: EXPLICIT_ACCESS_W = unsafe { zeroed() };
        entry.grfAccessPermissions = change.mask();
        entry.grfAccessMode = change.access_mode();
        entry.grfInheritance = change.inheritance();
        entry.Trustee = TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.cast(),
        };
        let mut new_dacl = null_mut();
        let status = unsafe { SetEntriesInAclW(1, &entry, dacl, &mut new_dacl) };
        if status != ERROR_SUCCESS {
            return Err(SandboxExecutionError::Initialization(format!(
                "SetEntriesInAclW failed for `{path}` with {status}"
            )));
        }
        let new_dacl = LocalAcl(new_dacl);
        let status = unsafe {
            SetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                new_dacl.0,
                null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(SandboxExecutionError::Initialization(format!(
                "SetSecurityInfo failed for `{path}` with {status}"
            )));
        }
        drop(descriptor);
        Ok(())
    }

    fn opened_object_identity(
        handle: HANDLE,
    ) -> Result<WindowsSandboxObjectIdentity, SandboxExecutionError> {
        let mut extended: FILE_ID_INFO = unsafe { zeroed() };
        if unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileIdInfo,
                (&mut extended as *mut FILE_ID_INFO).cast(),
                u32::try_from(size_of::<FILE_ID_INFO>()).expect("file identity size fits u32"),
            )
        } != 0
        {
            return Ok(WindowsSandboxObjectIdentity::Extended {
                volume_serial_number: extended.VolumeSerialNumber,
                file_id: extended.FileId.Identifier,
            });
        }

        let mut legacy: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
        if unsafe { GetFileInformationByHandle(handle, &mut legacy) } == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to read opened sandbox object identity: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(WindowsSandboxObjectIdentity::Legacy {
            volume_serial_number: legacy.dwVolumeSerialNumber,
            file_index: ((legacy.nFileIndexHigh as u64) << 32) | legacy.nFileIndexLow as u64,
        })
    }

    struct LocalAllocation(*mut c_void);
    impl Drop for LocalAllocation {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { LocalFree(self.0 as HLOCAL) };
            }
        }
    }

    struct LocalAcl(*mut ACL);
    impl Drop for LocalAcl {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { LocalFree(self.0 as HLOCAL) };
            }
        }
    }

    fn acl_has_entry(
        dacl: *mut ACL,
        sid: *mut c_void,
        ace_type: u8,
        mask: u32,
        required_inheritance: u8,
    ) -> bool {
        if dacl.is_null() {
            return false;
        }
        let mut info: ACL_SIZE_INFORMATION = unsafe { zeroed() };
        if unsafe {
            GetAclInformation(
                dacl,
                (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == 0
        {
            return false;
        }
        let mut overlapping_allow_precedes_deny = false;
        for index in 0..info.AceCount {
            let mut raw_ace = null_mut();
            if unsafe { GetAce(dacl, index, &mut raw_ace) } == 0 {
                continue;
            }
            let header = unsafe { &*(raw_ace as *const ACE_HEADER) };
            if ace_type == ACCESS_DENIED_ACE_TYPE
                && header.AceType != ACCESS_DENIED_ACE_TYPE
                && header.AceFlags & INHERIT_ONLY_ACE == 0
            {
                // Be conservative about object/callback/conditional allow ACEs
                // and generic-right mapping. Any effective non-deny before our
                // deny means the DACL order is not proven safe; rebuild it via
                // SetEntriesInAcl(DENY_ACCESS), which canonicalizes the deny.
                overlapping_allow_precedes_deny = true;
            }
            if header.AceType != ace_type
                || !ace_inheritance_is_sufficient(header.AceFlags, required_inheritance)
            {
                continue;
            }
            let sid_ptr =
                (raw_ace as usize + size_of::<ACE_HEADER>() + size_of::<u32>()) as *mut c_void;
            if unsafe { EqualSid(sid_ptr, sid) } == 0 {
                continue;
            }
            let existing_mask = if ace_type == ACCESS_ALLOWED_ACE_TYPE {
                unsafe { (*(raw_ace as *const ACCESS_ALLOWED_ACE)).Mask }
            } else {
                unsafe { (*(raw_ace as *const ACCESS_DENIED_ACE)).Mask }
            };
            if existing_mask & mask == mask {
                return ace_type != ACCESS_DENIED_ACE_TYPE || !overlapping_allow_precedes_deny;
            }
        }
        false
    }

    fn ace_inheritance_is_sufficient(flags: u8, required: u8) -> bool {
        flags & INHERIT_ONLY_ACE == 0
            && flags & NO_PROPAGATE_INHERIT_ACE == 0
            && flags & required == required
    }

    fn verify_opened_path(
        handle: HANDLE,
        expected: &camino::Utf8Path,
    ) -> Result<(), SandboxExecutionError> {
        let opened = opened_identity_path(handle, expected)?;
        if !same_windows_path(&opened, expected) {
            return Err(SandboxExecutionError::Initialization(format!(
                "sandbox ACL path identity changed: expected `{expected}`, opened `{opened}`"
            )));
        }
        Ok(())
    }

    fn opened_identity_path(
        handle: HANDLE,
        label: &camino::Utf8Path,
    ) -> Result<Utf8PathBuf, SandboxExecutionError> {
        let needed = unsafe { GetFinalPathNameByHandleW(handle, null_mut(), 0, 0) };
        if needed == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to resolve opened sandbox path `{label}`: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut buffer = vec![0u16; needed as usize + 1];
        let written = unsafe {
            GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, 0)
        };
        if written == 0 || written as usize >= buffer.len() {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to read opened sandbox path identity `{label}`: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(Utf8PathBuf::from(normalize_windows_identity(
            &String::from_utf16_lossy(&buffer[..written as usize]),
        )))
    }

    fn same_windows_path(left: &camino::Utf8Path, right: &camino::Utf8Path) -> bool {
        normalize_windows_identity(left.as_str()) == normalize_windows_identity(right.as_str())
    }

    fn normalize_windows_identity(value: &str) -> String {
        value
            .strip_prefix(r"\\?\UNC\")
            .map(|rest| format!(r"\\{rest}"))
            .or_else(|| value.strip_prefix(r"\\?\").map(ToOwned::to_owned))
            .unwrap_or_else(|| value.to_string())
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    }

    fn create_workspace_write_token(
        roots: &[RootCapability],
    ) -> Result<OwnedHandle, SandboxExecutionError> {
        let base = open_current_process_token()?;
        let mut user_sid = token_user_sid(base.raw())?;
        let mut logon_sid = token_logon_sid(base.raw())?;
        let mut world_sid = world_sid()?;
        let mut system_sid = system_sid()?;
        let private_sid = LocalSid::from_string(&sandbox_process_private_sid())?;
        // The logon and Everyone SIDs are required by native process, desktop,
        // and CLR initialization. Before this token is created, the sandbox
        // preflight applies active-capability deny ACEs to detected
        // Everyone-writable locations outside the configured roots.
        let mut entries = Vec::with_capacity(roots.len() + 3);
        for root in roots {
            entries.push(SID_AND_ATTRIBUTES {
                Sid: root.sid.raw(),
                Attributes: 0,
            });
        }
        entries.push(SID_AND_ATTRIBUTES {
            Sid: logon_sid.as_mut_ptr().cast(),
            Attributes: 0,
        });
        entries.push(SID_AND_ATTRIBUTES {
            Sid: world_sid.as_mut_ptr().cast(),
            Attributes: 0,
        });
        entries.push(SID_AND_ATTRIBUTES {
            Sid: private_sid.raw(),
            Attributes: 0,
        });
        let mut restricted = null_mut();
        if unsafe {
            CreateRestrictedToken(
                base.raw(),
                DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED,
                0,
                null(),
                0,
                null(),
                entries.len() as u32,
                entries.as_mut_ptr(),
                &mut restricted,
            )
        } == 0
        {
            return Err(SandboxExecutionError::Initialization(format!(
                "CreateRestrictedToken failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        let token = OwnedHandle::new(restricted, "CreateRestrictedToken")?;
        if unsafe { IsTokenRestricted(token.raw()) } == 0 {
            return Err(SandboxExecutionError::Initialization(
                "CreateRestrictedToken returned an unrestricted token".to_string(),
            ));
        }
        if !token_has_restricted_sid(token.raw(), private_sid.raw())? {
            return Err(SandboxExecutionError::Initialization(
                "restricted token dropped the process-private restricting SID".to_string(),
            ));
        }
        // The token's default DACL becomes the default security descriptor for
        // process/thread and other kernel objects created by the child. Never
        // grant it to World, the logon SID, or reusable root/TEMP capabilities:
        // another restricted child could otherwise open/inject into this one.
        // The normal access-check pass uses the current user/SYSTEM; the
        // restricted pass uses a fresh process-private restricting SID.
        let default_dacl_sids = vec![
            user_sid.as_mut_ptr().cast(),
            system_sid.as_mut_ptr().cast(),
            private_sid.raw(),
        ];
        set_token_default_dacl(token.raw(), &default_dacl_sids)?;
        enable_change_notify_privilege(token.raw())?;
        Ok(token)
    }

    fn open_current_process_token() -> Result<OwnedHandle, SandboxExecutionError> {
        #[link(name = "advapi32")]
        unsafe extern "system" {
            fn OpenProcessToken(process: HANDLE, desired_access: u32, token: *mut HANDLE) -> i32;
        }
        let desired = TOKEN_DUPLICATE
            | TOKEN_QUERY
            | TOKEN_ASSIGN_PRIMARY
            | TOKEN_ADJUST_DEFAULT
            | TOKEN_ADJUST_SESSIONID
            | TOKEN_ADJUST_PRIVILEGES;
        let mut token = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), desired, &mut token) } == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "OpenProcessToken failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        OwnedHandle::new(token, "OpenProcessToken")
    }

    fn token_logon_sid(token: HANDLE) -> Result<Vec<u8>, SandboxExecutionError> {
        let mut needed = 0u32;
        unsafe {
            GetTokenInformation(token, TokenGroups, null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to size token groups: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut buffer = vec![0u8; needed as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenGroups,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to read token groups: {}",
                std::io::Error::last_os_error()
            )));
        }
        let group_count = unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<u32>()) };
        let after_count = unsafe { buffer.as_ptr().add(size_of::<u32>()) } as usize;
        let alignment = std::mem::align_of::<SID_AND_ATTRIBUTES>();
        let groups =
            ((after_count + alignment - 1) & !(alignment - 1)) as *const SID_AND_ATTRIBUTES;
        for index in 0..group_count as usize {
            let entry = unsafe { std::ptr::read_unaligned(groups.add(index)) };
            if entry.Attributes & SE_GROUP_LOGON_ID == SE_GROUP_LOGON_ID {
                return copy_sid(entry.Sid);
            }
        }
        Err(SandboxExecutionError::Initialization(
            "current token has no logon SID".to_string(),
        ))
    }

    fn token_has_restricted_sid(
        token: HANDLE,
        expected: *mut c_void,
    ) -> Result<bool, SandboxExecutionError> {
        let mut needed = 0u32;
        unsafe {
            GetTokenInformation(token, TokenRestrictedSids, null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to size restricted SID list: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut buffer = vec![0u8; needed as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenRestrictedSids,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to read restricted SID list: {}",
                std::io::Error::last_os_error()
            )));
        }
        let group_count = unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<u32>()) };
        let after_count = unsafe { buffer.as_ptr().add(size_of::<u32>()) } as usize;
        let alignment = std::mem::align_of::<SID_AND_ATTRIBUTES>();
        let groups =
            ((after_count + alignment - 1) & !(alignment - 1)) as *const SID_AND_ATTRIBUTES;
        for index in 0..group_count as usize {
            let entry = unsafe { std::ptr::read_unaligned(groups.add(index)) };
            if unsafe { EqualSid(entry.Sid, expected) } != 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn token_user_sid(token: HANDLE) -> Result<Vec<u8>, SandboxExecutionError> {
        let mut needed = 0u32;
        unsafe {
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
        }
        if needed == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to size token user: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut buffer = vec![0u8; needed as usize];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to read token user: {}",
                std::io::Error::last_os_error()
            )));
        }
        let user = unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<TOKEN_USER>()) };
        copy_sid(user.User.Sid)
    }

    fn world_sid() -> Result<Vec<u8>, SandboxExecutionError> {
        well_known_sid(WIN_WORLD_SID, "World")
    }

    fn system_sid() -> Result<Vec<u8>, SandboxExecutionError> {
        well_known_sid(WIN_LOCAL_SYSTEM_SID, "LocalSystem")
    }

    fn well_known_sid(kind: i32, label: &str) -> Result<Vec<u8>, SandboxExecutionError> {
        let mut needed = 0u32;
        unsafe {
            CreateWellKnownSid(kind, null_mut(), null_mut(), &mut needed);
        }
        let mut sid = vec![0u8; needed as usize];
        if needed == 0
            || unsafe { CreateWellKnownSid(kind, null_mut(), sid.as_mut_ptr().cast(), &mut needed) }
                == 0
        {
            return Err(SandboxExecutionError::Initialization(format!(
                "CreateWellKnownSid({label}) failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(sid)
    }

    fn sandbox_process_private_sid() -> String {
        let digest =
            Sha256::digest(format!("moyai.sandbox-process.v1\0{}", ulid::Ulid::new()).as_bytes());
        let mut parts = [0u32; 4];
        for (index, part) in parts.iter_mut().enumerate() {
            let start = index * 4;
            *part = u32::from_le_bytes(digest[start..start + 4].try_into().expect("four bytes"));
        }
        format!(
            "S-1-5-21-{}-{}-{}-{}",
            parts[0], parts[1], parts[2], parts[3]
        )
    }

    fn copy_sid(sid: *mut c_void) -> Result<Vec<u8>, SandboxExecutionError> {
        let length = unsafe { GetLengthSid(sid) };
        if length == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "GetLengthSid failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut bytes = vec![0u8; length as usize];
        if unsafe { CopySid(length, bytes.as_mut_ptr().cast(), sid) } == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "CopySid failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(bytes)
    }

    fn set_token_default_dacl(
        token: HANDLE,
        sids: &[*mut c_void],
    ) -> Result<(), SandboxExecutionError> {
        let entries = sids
            .iter()
            .map(|sid| EXPLICIT_ACCESS_W {
                grfAccessPermissions: GENERIC_ALL,
                grfAccessMode: GRANT_ACCESS,
                grfInheritance: 0,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: null_mut(),
                    MultipleTrusteeOperation: 0,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: TRUSTEE_IS_UNKNOWN,
                    ptstrName: (*sid).cast(),
                },
            })
            .collect::<Vec<_>>();
        let mut dacl = null_mut();
        let status = unsafe {
            SetEntriesInAclW(
                entries.len() as u32,
                entries.as_ptr(),
                null_mut(),
                &mut dacl,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to build restricted-token default DACL with {status}"
            )));
        }
        let dacl = LocalAcl(dacl);
        let mut info = TokenDefaultDaclInfo {
            default_dacl: dacl.0,
        };
        if unsafe {
            SetTokenInformation(
                token,
                TokenDefaultDacl,
                (&mut info as *mut TokenDefaultDaclInfo).cast(),
                size_of::<TokenDefaultDaclInfo>() as u32,
            )
        } == 0
        {
            return Err(SandboxExecutionError::Initialization(format!(
                "SetTokenInformation(TokenDefaultDacl) failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    fn enable_change_notify_privilege(token: HANDLE) -> Result<(), SandboxExecutionError> {
        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        let name = to_wide("SeChangeNotifyPrivilege");
        if unsafe { LookupPrivilegeValueW(null(), name.as_ptr(), &mut luid) } == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "LookupPrivilegeValueW failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        let mut privileges: TOKEN_PRIVILEGES = unsafe { zeroed() };
        privileges.PrivilegeCount = 1;
        privileges.Privileges[0].Luid = luid;
        privileges.Privileges[0].Attributes = 0x0000_0002;
        if unsafe { AdjustTokenPrivileges(token, 0, &privileges, 0, null_mut(), null_mut()) } == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "AdjustTokenPrivileges failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        if unsafe { GetLastError() } != ERROR_SUCCESS {
            return Err(SandboxExecutionError::Initialization(format!(
                "restricted token could not enable SeChangeNotifyPrivilege: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    fn spawn_and_capture(
        token: OwnedHandle,
        request: SandboxedProcessRequest,
    ) -> Result<SandboxedProcessOutput, SandboxExecutionError> {
        let (child_stdin, parent_stdin) = create_pipe_pair("stdin")?;
        let (parent_stdout, child_stdout) = create_pipe_pair("stdout")?;
        let (parent_stderr, child_stderr) = create_pipe_pair("stderr")?;
        mark_inheritable(child_stdin.raw(), "stdin")?;
        mark_inheritable(child_stdout.raw(), "stdout")?;
        mark_inheritable(child_stderr.raw(), "stderr")?;
        clear_inheritable(parent_stdin.raw(), "parent stdin")?;
        clear_inheritable(parent_stdout.raw(), "parent stdout")?;
        clear_inheritable(parent_stderr.raw(), "parent stderr")?;

        let mut inherited = [child_stdin.raw(), child_stdout.raw(), child_stderr.raw()];
        let mut attributes = AttributeList::for_handles(&mut inherited)?;
        let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = child_stdin.raw();
        startup.StartupInfo.hStdOutput = child_stdout.raw();
        startup.StartupInfo.hStdError = child_stderr.raw();
        let mut desktop = to_wide("Winsta0\\Default");
        startup.StartupInfo.lpDesktop = desktop.as_mut_ptr();
        startup.lpAttributeList = attributes.raw().cast();

        let mut command_line = to_wide(&argv_to_command_line(&request.argv));
        let cwd = to_wide(request.cwd.as_str());
        let environment = environment_block(&request.environment);
        let mut process_info: PROCESS_INFORMATION = unsafe { zeroed() };
        let mut object_security = ProcessObjectSecurity::system_only()?;
        let mut process_security = object_security.attributes();
        let mut thread_security = object_security.attributes();
        let mut creation_flags = CREATE_UNICODE_ENVIRONMENT
            | EXTENDED_STARTUPINFO_PRESENT
            | CREATE_SUSPENDED
            | CREATE_NEW_PROCESS_GROUP;
        if request.hide_window {
            creation_flags |= CREATE_NO_WINDOW;
        }
        if request.cancel.is_cancelled() {
            return Ok(cancelled_before_effect(Vec::new()));
        }
        let spawned = unsafe {
            CreateProcessAsUserW(
                token.raw(),
                null(),
                command_line.as_mut_ptr(),
                &mut process_security,
                &mut thread_security,
                1,
                creation_flags,
                environment.as_ptr().cast_mut().cast(),
                cwd.as_ptr(),
                &startup.StartupInfo,
                &mut process_info,
            )
        };
        if spawned == 0 {
            return Err(SandboxExecutionError::Spawn(format!(
                "CreateProcessAsUserW failed for `{}` in `{}`: {}",
                request.argv[0],
                request.cwd,
                std::io::Error::last_os_error()
            )));
        }
        if !valid_handle(process_info.hProcess) || !valid_handle(process_info.hThread) {
            let error = std::io::Error::last_os_error();
            if valid_handle(process_info.hProcess) {
                unsafe {
                    TerminateProcess(process_info.hProcess, 1);
                    WaitForSingleObject(process_info.hProcess, 3_000);
                    CloseHandle(process_info.hProcess);
                }
            }
            if valid_handle(process_info.hThread) {
                unsafe {
                    CloseHandle(process_info.hThread);
                }
            }
            return Err(SandboxExecutionError::Spawn(format!(
                "CreateProcessAsUserW returned an invalid process or thread handle: {error}"
            )));
        }
        let process = OwnedHandle(process_info.hProcess);
        let thread = OwnedHandle(process_info.hThread);
        drop(child_stdin);
        drop(child_stdout);
        drop(child_stderr);
        drop(attributes);

        let job = match create_kill_on_close_job(process.raw()) {
            Ok(job) => job,
            Err(error) => {
                unsafe {
                    TerminateProcess(process.raw(), 1);
                    WaitForSingleObject(process.raw(), 3_000);
                }
                return Err(error);
            }
        };
        if request.cancel.is_cancelled() {
            let cleanup_errors = terminate_and_reap_job(&job, &process);
            return Ok(cancelled_before_effect(cleanup_errors));
        }
        if unsafe { ResumeThread(thread.raw()) } == u32::MAX {
            let error = std::io::Error::last_os_error();
            let cleanup_errors = terminate_and_reap_job(&job, &process);
            let cleanup = (!cleanup_errors.is_empty())
                .then(|| format!("; cleanup: {}", cleanup_errors.join("; ")))
                .unwrap_or_default();
            return Err(SandboxExecutionError::Spawn(format!(
                "failed to resume sandbox process after Job assignment: {error}{cleanup}"
            )));
        }
        drop(thread);

        let max_output = request.max_output_bytes.max(1);
        let stdout_reader = read_pipe_bounded(parent_stdout, max_output);
        let stderr_reader = read_pipe_bounded(parent_stderr, max_output);
        let stdin_writer = write_pipe(parent_stdin, request.stdin);
        let deadline = Instant::now() + Duration::from_millis(request.timeout_ms.max(1));
        let wait_outcome = loop {
            if request.cancel.is_cancelled() {
                break WaitOutcome::Cancelled;
            }
            if Instant::now() >= deadline {
                break WaitOutcome::TimedOut;
            }
            match unsafe { WaitForSingleObject(process.raw(), 50) } {
                WAIT_OBJECT_0 => break WaitOutcome::Exited,
                WAIT_TIMEOUT => {}
                WAIT_FAILED => {
                    break WaitOutcome::Failed(std::io::Error::last_os_error().to_string());
                }
                value => {
                    break WaitOutcome::Failed(format!(
                        "WaitForSingleObject returned unexpected status {value}"
                    ));
                }
            }
        };

        let mut cleanup_errors = Vec::new();
        let timed_out = matches!(wait_outcome, WaitOutcome::TimedOut);
        let cancelled = matches!(wait_outcome, WaitOutcome::Cancelled);
        if let WaitOutcome::Failed(error) = &wait_outcome {
            cleanup_errors.push(format!("failed while waiting for sandbox process: {error}"));
        }
        let mut exit_code = None;
        if matches!(wait_outcome, WaitOutcome::Exited) {
            let mut code = 0u32;
            if unsafe { GetExitCodeProcess(process.raw(), &mut code) } == 0 {
                cleanup_errors.push(format!(
                    "failed to read sandbox process exit code: {}",
                    std::io::Error::last_os_error()
                ));
            } else {
                exit_code = Some(code as i32);
            }
        }
        if unsafe { TerminateJobObject(job.raw(), 1) } == 0 {
            let error = std::io::Error::last_os_error();
            cleanup_errors.push(format!("failed to terminate sandbox Job: {error}"));
        }
        let reap_deadline = Instant::now() + PROCESS_REAP_TIMEOUT;
        loop {
            match unsafe { WaitForSingleObject(process.raw(), 50) } {
                WAIT_OBJECT_0 => break,
                WAIT_TIMEOUT if Instant::now() < reap_deadline => continue,
                WAIT_TIMEOUT => {
                    cleanup_errors.push(format!(
                        "sandbox process did not exit within {} ms after Job termination",
                        PROCESS_REAP_TIMEOUT.as_millis()
                    ));
                    break;
                }
                WAIT_FAILED => {
                    cleanup_errors.push(format!(
                        "failed to reap sandbox process: {}",
                        std::io::Error::last_os_error()
                    ));
                    break;
                }
                value => {
                    cleanup_errors.push(format!(
                        "sandbox process reap returned unexpected status {value}"
                    ));
                    break;
                }
            }
        }
        drop(job);
        drop(process);
        drop(token);

        match join_worker_bounded(stdin_writer, "stdin writer", &mut cleanup_errors) {
            Some(Ok(())) => {}
            Some(Err(_error)) if cancelled || timed_out => {}
            Some(Err(error)) => cleanup_errors.push(error),
            None => {}
        }
        let (stdout, stdout_error) =
            join_worker_bounded(stdout_reader, "stdout reader", &mut cleanup_errors)
                .unwrap_or_else(|| (empty_output(), None));
        if let Some(error) = stdout_error {
            cleanup_errors.push(error);
        }
        let (stderr, stderr_error) =
            join_worker_bounded(stderr_reader, "stderr reader", &mut cleanup_errors)
                .unwrap_or_else(|| (empty_output(), None));
        if let Some(error) = stderr_error {
            cleanup_errors.push(error);
        }

        Ok(SandboxedProcessOutput {
            exit_code,
            stdout,
            stderr,
            timed_out,
            cancelled,
            effect_started: true,
            cleanup_errors,
        })
    }

    fn join_worker_bounded<T>(
        worker: std::thread::JoinHandle<T>,
        label: &str,
        cleanup_errors: &mut Vec<String>,
    ) -> Option<T> {
        let grace_deadline = Instant::now() + WORKER_DRAIN_GRACE;
        while !worker.is_finished() && Instant::now() < grace_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !worker.is_finished() {
            if unsafe { CancelSynchronousIo(worker.as_raw_handle() as HANDLE) } == 0 {
                let error = unsafe { GetLastError() };
                if error != ERROR_NOT_FOUND {
                    cleanup_errors.push(format!(
                        "failed to cancel sandbox {label} I/O: {}",
                        std::io::Error::from_raw_os_error(error as i32)
                    ));
                }
            }
            let cancel_deadline = Instant::now() + WORKER_CANCEL_TIMEOUT;
            while !worker.is_finished() && Instant::now() < cancel_deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if !worker.is_finished() {
            cleanup_errors.push(format!(
                "sandbox {label} did not stop within {} ms after I/O cancellation",
                WORKER_CANCEL_TIMEOUT.as_millis()
            ));
            return None;
        }
        match worker.join() {
            Ok(value) => Some(value),
            Err(_) => {
                cleanup_errors.push(format!("sandbox {label} panicked"));
                None
            }
        }
    }

    enum WaitOutcome {
        Exited,
        TimedOut,
        Cancelled,
        Failed(String),
    }

    fn valid_handle(handle: HANDLE) -> bool {
        !handle.is_null() && handle != INVALID_HANDLE_VALUE
    }

    fn cancelled_before_effect(cleanup_errors: Vec<String>) -> SandboxedProcessOutput {
        SandboxedProcessOutput {
            exit_code: None,
            stdout: empty_output(),
            stderr: empty_output(),
            timed_out: false,
            cancelled: true,
            effect_started: false,
            cleanup_errors,
        }
    }

    fn terminate_and_reap_job(job: &OwnedHandle, process: &OwnedHandle) -> Vec<String> {
        let mut cleanup_errors = Vec::new();
        if unsafe { TerminateJobObject(job.raw(), 1) } == 0 {
            cleanup_errors.push(format!(
                "failed to terminate sandbox Job: {}",
                std::io::Error::last_os_error()
            ));
        }
        match unsafe {
            WaitForSingleObject(
                process.raw(),
                u32::try_from(PROCESS_REAP_TIMEOUT.as_millis()).unwrap_or(u32::MAX),
            )
        } {
            WAIT_OBJECT_0 => {}
            WAIT_TIMEOUT => cleanup_errors.push(format!(
                "sandbox process did not exit within {} ms after Job termination",
                PROCESS_REAP_TIMEOUT.as_millis()
            )),
            WAIT_FAILED => cleanup_errors.push(format!(
                "failed to reap sandbox process: {}",
                std::io::Error::last_os_error()
            )),
            value => cleanup_errors.push(format!(
                "sandbox process reap returned unexpected status {value}"
            )),
        }
        cleanup_errors
    }

    fn create_pipe_pair(label: &str) -> Result<(OwnedHandle, OwnedHandle), SandboxExecutionError> {
        let mut read = null_mut();
        let mut write = null_mut();
        if unsafe { CreatePipe(&mut read, &mut write, null(), 0) } == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to create sandbox {label} pipe: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok((
            OwnedHandle::new(read, &format!("sandbox {label} read pipe"))?,
            OwnedHandle::new(write, &format!("sandbox {label} write pipe"))?,
        ))
    }

    fn mark_inheritable(handle: HANDLE, label: &str) -> Result<(), SandboxExecutionError> {
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) } == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to mark sandbox {label} handle inheritable: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    fn clear_inheritable(handle: HANDLE, label: &str) -> Result<(), SandboxExecutionError> {
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to clear sandbox {label} handle inheritance: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }

    fn create_kill_on_close_job(process: HANDLE) -> Result<OwnedHandle, SandboxExecutionError> {
        let job = OwnedHandle::new(
            unsafe { CreateJobObjectW(null(), null()) },
            "CreateJobObjectW for sandbox process",
        )?;
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        } == 0
        {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to configure sandbox Job: {}",
                std::io::Error::last_os_error()
            )));
        }
        let ui_restrictions = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT_HANDLES
                | JOB_OBJECT_UILIMIT_READCLIPBOARD
                | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
                | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS
                | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
                | JOB_OBJECT_UILIMIT_GLOBALATOMS
                | JOB_OBJECT_UILIMIT_DESKTOP
                | JOB_OBJECT_UILIMIT_EXITWINDOWS,
        };
        if unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectBasicUIRestrictions,
                (&ui_restrictions as *const JOBOBJECT_BASIC_UI_RESTRICTIONS).cast(),
                size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
            )
        } == 0
        {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to configure sandbox Job UI restrictions: {}",
                std::io::Error::last_os_error()
            )));
        }
        if unsafe { AssignProcessToJobObject(job.raw(), process) } == 0 {
            return Err(SandboxExecutionError::Initialization(format!(
                "failed to assign suspended sandbox process to Job: {}",
                std::io::Error::last_os_error()
            )));
        }
        Ok(job)
    }

    fn read_pipe_bounded(
        handle: OwnedHandle,
        maximum: usize,
    ) -> std::thread::JoinHandle<(BoundedPipeOutput, Option<String>)> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut truncated = false;
            let mut error = None;
            let mut buffer = [0u8; 8 * 1024];
            loop {
                let mut read = 0u32;
                let result = unsafe {
                    ReadFile(
                        handle.raw(),
                        buffer.as_mut_ptr(),
                        buffer.len() as u32,
                        &mut read,
                        null_mut(),
                    )
                };
                if result == 0 {
                    let read_error = std::io::Error::last_os_error();
                    if read_error.raw_os_error() != Some(109) {
                        error = Some(format!("failed to read sandbox output pipe: {read_error}"));
                    }
                    break;
                }
                if read == 0 {
                    break;
                }
                let chunk = &buffer[..read as usize];
                let remaining = maximum.saturating_sub(bytes.len());
                bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                truncated |= chunk.len() > remaining;
            }
            (BoundedPipeOutput { bytes, truncated }, error)
        })
    }

    fn write_pipe(
        handle: OwnedHandle,
        input: Vec<u8>,
    ) -> std::thread::JoinHandle<Result<(), String>> {
        std::thread::spawn(move || {
            let mut offset = 0usize;
            while offset < input.len() {
                let chunk_len = (input.len() - offset).min(u32::MAX as usize);
                let mut written = 0u32;
                if unsafe {
                    WriteFile(
                        handle.raw(),
                        input[offset..].as_ptr(),
                        chunk_len as u32,
                        &mut written,
                        null_mut(),
                    )
                } == 0
                {
                    return Err(format!(
                        "failed to write complete sandbox stdin: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                if written == 0 {
                    return Err(
                        "failed to write complete sandbox stdin: WriteFile wrote zero bytes"
                            .to_string(),
                    );
                }
                offset += written as usize;
            }
            Ok(())
        })
    }

    fn apply_advisory_offline_environment(
        environment: &mut std::collections::HashMap<String, String>,
    ) {
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
            environment.insert(key.to_string(), "http://127.0.0.1:9".to_string());
        }
        for (key, value) in [
            ("SBX_NONET_ACTIVE", "1"),
            ("NO_PROXY", ""),
            ("PIP_NO_INDEX", "1"),
            ("PIP_DISABLE_PIP_VERSION_CHECK", "1"),
            ("NPM_CONFIG_OFFLINE", "true"),
            ("CARGO_NET_OFFLINE", "true"),
            ("GIT_ALLOW_PROTOCOL", ""),
        ] {
            environment.insert(key.to_string(), value.to_string());
        }
    }

    fn environment_block(environment: &std::collections::HashMap<String, String>) -> Vec<u16> {
        let mut entries = environment.iter().collect::<Vec<_>>();
        entries.sort_by(|(left, _), (right, _)| {
            left.to_uppercase()
                .cmp(&right.to_uppercase())
                .then(left.cmp(right))
        });
        let mut block = Vec::new();
        let mut previous_key = None;
        for (key, value) in entries {
            let normalized_key = key.to_uppercase();
            if previous_key.as_deref() == Some(normalized_key.as_str()) {
                continue;
            }
            let item = format!("{normalized_key}={value}");
            block.extend(OsStr::new(&item).encode_wide());
            block.push(0);
            previous_key = Some(normalized_key);
        }
        block.push(0);
        block
    }

    fn argv_to_command_line(argv: &[String]) -> String {
        argv.iter()
            .map(|argument| quote_windows_argument(argument))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn quote_windows_argument(argument: &str) -> String {
        if !argument.is_empty()
            && !argument
                .chars()
                .any(|character| character.is_whitespace() || character == '"')
        {
            return argument.to_string();
        }
        let mut quoted = String::from("\"");
        let mut backslashes = 0usize;
        for character in argument.chars() {
            if character == '\\' {
                backslashes += 1;
                continue;
            }
            if character == '"' {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
            } else {
                quoted.push_str(&"\\".repeat(backslashes));
                quoted.push(character);
            }
            backslashes = 0;
        }
        quoted.push_str(&"\\".repeat(backslashes * 2));
        quoted.push('"');
        quoted
    }

    fn to_wide(value: impl AsRef<OsStr>) -> Vec<u16> {
        value.as_ref().encode_wide().chain(Some(0)).collect()
    }

    fn empty_output() -> BoundedPipeOutput {
        BoundedPipeOutput {
            bytes: Vec::new(),
            truncated: false,
        }
    }

    #[cfg(test)]
    mod tests {
        use std::collections::HashMap;

        use super::{
            ACCESS_ALLOWED_ACE_TYPE, CONTAINER_AND_OBJECT_INHERIT, GENERIC_ALL, INHERIT_ONLY_ACE,
            NO_PROPAGATE_INHERIT_ACE, TokenDefaultDaclInfo, ace_inheritance_is_sufficient,
            acl_has_entry, apply_advisory_offline_environment, argv_to_command_line,
            canonical_windows_path_is_local, capability_sid_for_root, create_workspace_write_token,
            environment_block, normalize_windows_identity, open_current_process_token,
            token_logon_sid, token_user_sid, world_sid,
        };
        use crate::tool::os_sandbox::{SandboxPathSnapshot, WindowsSandboxObjectIdentity};
        use camino::Utf8PathBuf;

        fn snapshot(path: &str, file_id: u8) -> SandboxPathSnapshot {
            SandboxPathSnapshot {
                requested: Utf8PathBuf::from(path),
                canonical: Utf8PathBuf::from(path),
                content_sha256: None,
                identity: WindowsSandboxObjectIdentity::Extended {
                    volume_serial_number: 7,
                    file_id: [file_id; 16],
                },
            }
        }

        #[test]
        fn capability_sid_is_stable_and_root_scoped() {
            let first = capability_sid_for_root(&snapshot("C:/Workspace", 1));
            let equivalent = capability_sid_for_root(&snapshot("c:\\workspace", 1));
            let other = capability_sid_for_root(&snapshot("C:/Other", 1));
            let replacement = capability_sid_for_root(&snapshot("C:/Workspace", 2));
            assert_eq!(first, equivalent);
            assert_ne!(first, other);
            assert_ne!(first, replacement);
            assert!(first.starts_with("S-1-5-21-"));
        }

        #[test]
        fn reusable_acl_entry_must_propagate_without_inherit_only_or_no_propagate() {
            let required = CONTAINER_AND_OBJECT_INHERIT as u8;
            assert!(ace_inheritance_is_sufficient(required, required));
            assert!(!ace_inheritance_is_sufficient(0x01, required));
            assert!(!ace_inheritance_is_sufficient(0x02, required));
            assert!(!ace_inheritance_is_sufficient(
                required | INHERIT_ONLY_ACE,
                required
            ));
            assert!(!ace_inheritance_is_sufficient(
                required | NO_PROPAGATE_INHERIT_ACE,
                required
            ));
            assert!(ace_inheritance_is_sufficient(0, 0));
        }

        #[test]
        fn windows_argv_quoting_preserves_spaces_quotes_and_trailing_slashes() {
            assert_eq!(
                argv_to_command_line(&[
                    "tool.exe".to_string(),
                    "plain".to_string(),
                    "two words".to_string(),
                    "quote\"inside".to_string(),
                    "trailing\\".to_string(),
                ]),
                r#"tool.exe plain "two words" "quote\"inside" trailing\"#
            );
        }

        #[test]
        fn final_handle_path_prefixes_normalize_to_the_same_identity() {
            assert_eq!(
                normalize_windows_identity(r"\\?\C:\Workspace\"),
                normalize_windows_identity(r"c:/workspace")
            );
        }

        #[test]
        fn world_audit_rejects_remote_or_device_final_paths_before_acl_mutation() {
            assert!(canonical_windows_path_is_local(camino::Utf8Path::new(
                "C:/local"
            )));
            for path in [
                r"\\server\share\path",
                r"\\?\UNC\server\share\path",
                r"\\.\pipe\x",
            ] {
                assert!(!canonical_windows_path_is_local(camino::Utf8Path::new(
                    path
                )));
            }
        }

        #[test]
        fn windows_environment_block_has_no_case_insensitive_duplicate_keys() {
            let mut environment = HashMap::from([
                ("Path".to_string(), "first".to_string()),
                ("PATH".to_string(), "canonical".to_string()),
                ("https_proxy".to_string(), "old".to_string()),
            ]);
            apply_advisory_offline_environment(&mut environment);

            let block = String::from_utf16_lossy(&environment_block(&environment));
            assert_eq!(block.matches("PATH=").count(), 1);
            assert!(block.contains("PATH=canonical\0"));
            assert_eq!(block.matches("HTTPS_PROXY=").count(), 1);
            assert!(block.contains("HTTPS_PROXY=http://127.0.0.1:9\0"));
            assert!(!block.contains("https_proxy="));
        }

        #[test]
        fn restricted_token_default_dacl_excludes_shared_restricting_sids() {
            let base = open_current_process_token().expect("base token");
            let mut user = token_user_sid(base.raw()).expect("user SID");
            let mut logon = token_logon_sid(base.raw()).expect("logon SID");
            let mut world = world_sid().expect("World SID");
            let token = create_workspace_write_token(&[]).expect("restricted token");
            let mut needed = 0u32;
            unsafe {
                windows_sys::Win32::Security::GetTokenInformation(
                    token.raw(),
                    windows_sys::Win32::Security::TokenDefaultDacl,
                    std::ptr::null_mut(),
                    0,
                    &mut needed,
                );
            }
            assert!(needed > 0);
            let mut buffer = vec![0u8; needed as usize];
            assert_ne!(
                unsafe {
                    windows_sys::Win32::Security::GetTokenInformation(
                        token.raw(),
                        windows_sys::Win32::Security::TokenDefaultDacl,
                        buffer.as_mut_ptr().cast(),
                        needed,
                        &mut needed,
                    )
                },
                0
            );
            let info =
                unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<TokenDefaultDaclInfo>()) };
            assert!(acl_has_entry(
                info.default_dacl,
                user.as_mut_ptr().cast(),
                ACCESS_ALLOWED_ACE_TYPE,
                GENERIC_ALL,
                0,
            ));
            assert!(!acl_has_entry(
                info.default_dacl,
                logon.as_mut_ptr().cast(),
                ACCESS_ALLOWED_ACE_TYPE,
                GENERIC_ALL,
                0,
            ));
            assert!(!acl_has_entry(
                info.default_dacl,
                world.as_mut_ptr().cast(),
                ACCESS_ALLOWED_ACE_TYPE,
                GENERIC_ALL,
                0,
            ));
        }
    }
}

#[cfg(all(test, windows))]
mod integration_tests {
    use std::time::Duration;

    use super::*;
    use crate::config::{AccessMode, ResolvedConfig};
    use crate::tool::os_sandbox::ProcessSandboxPlan;
    use crate::workspace::WorkspaceDiscovery;

    fn ps_literal(path: &camino::Utf8Path) -> String {
        path.as_str().replace('\'', "''")
    }

    #[tokio::test]
    async fn restricted_child_writes_allowed_roots_but_not_outside_or_protected_authority() {
        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let fixture =
            Utf8PathBuf::from_path_buf(fixture.path().to_path_buf()).expect("utf8 fixture");
        let workspace_root = fixture.join("workspace");
        let additional_root = fixture.join("additional");
        let outside_root = fixture.join("outside");
        let git_file = workspace_root.join(".git");
        let moyai_root = workspace_root.join(".moyai");
        let agents_root = workspace_root.join(".agents");
        let claude_root = workspace_root.join(".claude");
        let codex_root = workspace_root.join(".codex");
        let agents_file = workspace_root.join("AGENTS.md");
        let claude_file = workspace_root.join("CLAUDE.md");
        let configured_instruction = workspace_root.join("policy.md");
        let configured_root = workspace_root.join("configured-protected");
        let git_meta = additional_root.join("git-meta");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        std::fs::create_dir_all(&additional_root).expect("additional root");
        std::fs::create_dir_all(&outside_root).expect("outside");
        std::fs::create_dir_all(moyai_root.join("rules")).expect("moyai authority");
        std::fs::create_dir_all(&agents_root).expect("agents authority");
        std::fs::create_dir_all(claude_root.join("skills/example")).expect("claude authority");
        std::fs::create_dir_all(&codex_root).expect("codex authority");
        std::fs::create_dir_all(&configured_root).expect("configured authority");
        std::fs::create_dir_all(&git_meta).expect("resolved git authority");
        std::fs::write(&git_file, "gitdir: ../additional/git-meta\n").expect("gitdir file");
        std::fs::write(&agents_file, "agents authority\n").expect("agents instruction");
        std::fs::write(&claude_file, "claude authority\n").expect("claude instruction");
        std::fs::write(&configured_instruction, "configured authority\n")
            .expect("configured instruction");

        let mut config = ResolvedConfig::default();
        config.permissions.additional_write_roots = vec![additional_root.clone()];
        config.workspace.protected_paths = vec![configured_root.clone()];
        config.instructions.additional_files = vec![Utf8PathBuf::from("policy.md")];
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(profile) =
            ProcessSandboxPlan::for_access_mode_with_config(
                AccessMode::Default,
                &workspace,
                &config,
            )
            .expect("sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let inside = workspace_root.join("inside.txt");
        let additional = additional_root.join("additional.txt");
        let outside = outside_root.join("outside.txt");
        let protected_targets = [
            moyai_root.join("rules/policy.md"),
            agents_root.join("config"),
            claude_root.join("skills/example/SKILL.md"),
            codex_root.join("config"),
            configured_root.join("config"),
            git_meta.join("config"),
        ];
        let protected_instruction_files = [&agents_file, &claude_file, &configured_instruction];
        let temp_target = Utf8PathBuf::from_path_buf(std::env::temp_dir())
            .expect("utf8 temp")
            .join(format!("moyai-sandbox-{}.txt", ulid::Ulid::new()));
        let mut script = format!(
            "$ErrorActionPreference='Stop'; Set-Content -LiteralPath '{}' -Value inside; Set-Content -LiteralPath '{}' -Value additional; Set-Content -LiteralPath '{}' -Value temporary; Get-Content -LiteralPath '{}' | Out-Null; Write-Output protected-read-ok; try {{ Set-Content -LiteralPath '{}' -Value outside; Write-Output outside-allowed }} catch {{ Write-Output outside-denied }}; ",
            ps_literal(&inside),
            ps_literal(&additional),
            ps_literal(&temp_target),
            ps_literal(&git_file),
            ps_literal(&outside),
        );
        script.push_str(&format!(
            "try {{ Set-Content -LiteralPath '{}' -Value changed; Write-Output git-file-allowed }} catch {{ Write-Output git-file-denied }}; ",
            ps_literal(&git_file)
        ));
        for (index, protected) in protected_targets.iter().enumerate() {
            script.push_str(&format!(
                "try {{ Set-Content -LiteralPath '{}' -Value protected; Write-Output protected-{index}-allowed }} catch {{ Write-Output protected-{index}-denied }}; ",
                ps_literal(protected)
            ));
        }
        for (index, protected) in protected_instruction_files.iter().enumerate() {
            script.push_str(&format!(
                "try {{ Set-Content -LiteralPath '{}' -Value changed; Write-Output instruction-{index}-allowed }} catch {{ Write-Output instruction-{index}-denied }}; ",
                ps_literal(protected)
            ));
        }
        script.push_str(&format!(
            "& icacls.exe '{}' /grant '*S-1-1-0:(OI)(CI)F' | Out-Null; if ($LASTEXITCODE -eq 0) {{ Write-Output acl-tamper-allowed }} else {{ Write-Output acl-tamper-denied }}; ",
            ps_literal(&codex_root)
        ));
        let shell = config.shell.clone();
        let output = execute_workspace_write(
            profile,
            SandboxedProcessRequest {
                argv: vec![
                    "powershell.exe".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    script,
                ],
                cwd: workspace_root.clone(),
                environment: captured_process_environment(&shell),
                stdin: Vec::new(),
                timeout_ms: 30_000,
                max_output_bytes: 64 * 1024,
                hide_window: true,
                cancel: CancellationToken::new(),
            },
        )
        .await
        .expect("sandboxed child");

        let stdout = String::from_utf8_lossy(&output.stdout.bytes);
        assert_eq!(
            output.exit_code,
            Some(0),
            "stderr={}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        assert!(inside.exists());
        assert!(additional.exists());
        assert!(temp_target.exists());
        assert!(!outside.exists(), "outside write escaped sandbox");
        assert_eq!(
            std::fs::read_to_string(&git_file).expect("gitdir content"),
            "gitdir: ../additional/git-meta\n"
        );
        assert!(
            protected_targets.iter().all(|target| !target.exists()),
            "a protected authority was writable"
        );
        assert_eq!(
            std::fs::read_to_string(&agents_file).expect("agents instruction content"),
            "agents authority\n"
        );
        assert_eq!(
            std::fs::read_to_string(&claude_file).expect("claude instruction content"),
            "claude authority\n"
        );
        assert_eq!(
            std::fs::read_to_string(&configured_instruction)
                .expect("configured instruction content"),
            "configured authority\n"
        );
        assert!(stdout.contains("outside-denied"), "stdout={stdout}");
        assert!(stdout.contains("protected-read-ok"), "stdout={stdout}");
        assert!(stdout.contains("git-file-denied"), "stdout={stdout}");
        assert!(stdout.contains("acl-tamper-denied"), "stdout={stdout}");
        for index in 0..protected_targets.len() {
            assert!(
                stdout.contains(&format!("protected-{index}-denied")),
                "stdout={stdout}"
            );
        }
        for index in 0..protected_instruction_files.len() {
            assert!(
                stdout.contains(&format!("instruction-{index}-denied")),
                "stdout={stdout}"
            );
        }
        std::fs::remove_file(&temp_target).expect("remove sandbox temp output");
    }

    #[tokio::test]
    async fn pre_cancel_and_initialization_failure_never_fall_back_to_unsandboxed_spawn() {
        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-fail-closed-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let fixture =
            Utf8PathBuf::from_path_buf(fixture.path().to_path_buf()).expect("utf8 fixture");
        let workspace_root = fixture.join("workspace");
        let marker = fixture.join("must-not-start.txt");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &workspace)
                .expect("sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let command = vec![
            "powershell.exe".to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            format!(
                "Set-Content -LiteralPath '{}' -Value started",
                ps_literal(&marker)
            ),
        ];
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let output = execute_workspace_write(
            profile.clone(),
            SandboxedProcessRequest {
                argv: command.clone(),
                cwd: workspace_root.clone(),
                environment: captured_process_environment(&config.shell),
                stdin: Vec::new(),
                timeout_ms: 10_000,
                max_output_bytes: 4_096,
                hide_window: true,
                cancel: cancelled,
            },
        )
        .await
        .expect("pre-cancel is a normal non-started outcome");
        assert!(output.cancelled);
        assert!(!output.effect_started);
        assert!(!marker.exists());

        let original_root = fixture.join("workspace-original");
        std::fs::rename(&workspace_root, &original_root).expect("move admitted root");
        std::fs::create_dir(&workspace_root).expect("replacement root");
        let error = execute_workspace_write(
            profile.clone(),
            SandboxedProcessRequest {
                argv: command.clone(),
                cwd: workspace_root.clone(),
                environment: captured_process_environment(&config.shell),
                stdin: Vec::new(),
                timeout_ms: 10_000,
                max_output_bytes: 4_096,
                hide_window: true,
                cancel: CancellationToken::new(),
            },
        )
        .await
        .expect_err("replacement root must fail its admitted identity snapshot");
        assert!(matches!(error, SandboxExecutionError::Initialization(_)));
        assert!(error.to_string().contains("identity"));
        assert!(!marker.exists());
        std::fs::remove_dir(&workspace_root).expect("remove replacement root");
        std::fs::rename(&original_root, &workspace_root).expect("restore admitted root");

        std::fs::remove_dir(&workspace_root).expect("remove root after profile compilation");
        let error = execute_workspace_write(
            profile,
            SandboxedProcessRequest {
                argv: command,
                cwd: workspace_root,
                environment: captured_process_environment(&config.shell),
                stdin: Vec::new(),
                timeout_ms: 10_000,
                max_output_bytes: 4_096,
                hide_window: true,
                cancel: CancellationToken::new(),
            },
        )
        .await
        .expect_err("missing sandbox root must fail before spawn");
        assert!(matches!(error, SandboxExecutionError::Initialization(_)));
        assert!(
            !marker.exists(),
            "sandbox failure fell back to normal spawn"
        );
    }

    #[tokio::test]
    async fn in_place_gitdir_rewrite_is_rejected_before_process_spawn() {
        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-gitdir-rewrite-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let workspace_root =
            Utf8PathBuf::from_path_buf(fixture.path().join("workspace")).expect("utf8 workspace");
        std::fs::create_dir_all(workspace_root.join("git-a")).expect("original gitdir");
        std::fs::create_dir_all(workspace_root.join("git-b")).expect("replacement gitdir");
        std::fs::write(workspace_root.join(".git"), "gitdir: git-a\n")
            .expect("original gitdir control file");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &workspace)
                .expect("sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        std::fs::write(workspace_root.join(".git"), "gitdir: git-b\n")
            .expect("same-object gitdir rewrite");
        let marker = workspace_root.join("must-not-start.txt");
        let error = execute_workspace_write(
            profile,
            SandboxedProcessRequest {
                argv: vec![
                    "powershell.exe".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    format!(
                        "Set-Content -LiteralPath '{}' -Value started",
                        ps_literal(&marker)
                    ),
                ],
                cwd: workspace_root,
                environment: captured_process_environment(&config.shell),
                stdin: Vec::new(),
                timeout_ms: 10_000,
                max_output_bytes: 4_096,
                hide_window: true,
                cancel: CancellationToken::new(),
            },
        )
        .await
        .expect_err("in-place gitdir rewrite must fail before spawn");

        assert!(matches!(error, SandboxExecutionError::Initialization(_)));
        assert!(error.to_string().contains("changed contents"));
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn pre_existing_protected_file_writer_fails_closed_before_spawn() {
        use std::os::windows::fs::OpenOptionsExt as _;

        use windows_sys::Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-protected-writer-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let workspace_root =
            Utf8PathBuf::from_path_buf(fixture.path().join("workspace")).expect("utf8 workspace");
        std::fs::create_dir_all(workspace_root.join("git-meta")).expect("gitdir");
        let git_file = workspace_root.join(".git");
        std::fs::write(&git_file, "gitdir: git-meta\n").expect("gitdir control file");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &workspace)
                .expect("sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let _writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&git_file)
            .expect("pre-existing writer");
        let marker = workspace_root.join("must-not-start.txt");

        let error = execute_workspace_write(
            profile,
            SandboxedProcessRequest {
                argv: vec![
                    "powershell.exe".to_string(),
                    "-NoProfile".to_string(),
                    "-Command".to_string(),
                    format!(
                        "Set-Content -LiteralPath '{}' -Value started",
                        ps_literal(&marker)
                    ),
                ],
                cwd: workspace_root,
                environment: captured_process_environment(&config.shell),
                stdin: Vec::new(),
                timeout_ms: 10_000,
                max_output_bytes: 4_096,
                hide_window: true,
                cancel: CancellationToken::new(),
            },
        )
        .await
        .expect_err("pre-existing protected writer must make launch fail closed");

        assert!(matches!(error, SandboxExecutionError::Initialization(_)));
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn external_gitdir_receives_capability_deny_even_without_writable_root_overlap() {
        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-external-gitdir-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let fixture =
            Utf8PathBuf::from_path_buf(fixture.path().to_path_buf()).expect("utf8 fixture");
        let workspace_root = fixture.join("workspace");
        let git_meta = fixture.join("external-git-meta");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        std::fs::create_dir_all(&git_meta).expect("external gitdir");
        std::fs::write(
            workspace_root.join(".git"),
            format!("gitdir: {}\n", git_meta.as_str()),
        )
        .expect("gitdir control file");
        let grant = std::process::Command::new("icacls.exe")
            .arg(git_meta.as_std_path())
            .args(["/grant", "*S-1-1-0:(OI)(CI)M"])
            .output()
            .expect("icacls grant");
        assert!(
            grant.status.success(),
            "icacls stderr={}",
            String::from_utf8_lossy(&grant.stderr)
        );
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &workspace)
                .expect("sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let escape = git_meta.join("escape.txt");
        let script = format!(
            "try {{ Set-Content -LiteralPath '{}' -Value escaped -ErrorAction Stop; Write-Output gitdir-allowed }} catch {{ Write-Output gitdir-denied }}",
            ps_literal(&escape)
        );
        let output = execute_workspace_write(
            profile,
            SandboxedProcessRequest {
                argv: vec![
                    "powershell.exe".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    script,
                ],
                cwd: workspace_root,
                environment: captured_process_environment(&config.shell),
                stdin: Vec::new(),
                timeout_ms: 30_000,
                max_output_bytes: 8_192,
                hide_window: true,
                cancel: CancellationToken::new(),
            },
        )
        .await
        .expect("sandbox execution");

        assert!(
            String::from_utf8_lossy(&output.stdout.bytes).contains("gitdir-denied"),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        assert!(!escape.exists(), "external gitdir was writable");
    }

    #[tokio::test]
    async fn separate_workspace_child_cannot_open_victim_process_with_injection_rights() {
        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-process-dacl-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let fixture =
            Utf8PathBuf::from_path_buf(fixture.path().to_path_buf()).expect("utf8 fixture");
        let victim_root = fixture.join("victim");
        let attacker_root = fixture.join("attacker");
        std::fs::create_dir_all(&victim_root).expect("victim workspace");
        std::fs::create_dir_all(&attacker_root).expect("attacker workspace");
        let config = ResolvedConfig::default();
        let victim_workspace = WorkspaceDiscovery::discover_fixed_root(&victim_root, &config)
            .expect("victim workspace discovery");
        let attacker_workspace = WorkspaceDiscovery::discover_fixed_root(&attacker_root, &config)
            .expect("attacker workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(victim_profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &victim_workspace)
                .expect("victim sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let ProcessSandboxPlan::WorkspaceWrite(attacker_profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &attacker_workspace)
                .expect("attacker sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let victim_pid_file = victim_root.join("pid.txt");
        let victim_cancel = CancellationToken::new();
        let victim_worker_cancel = victim_cancel.clone();
        let victim_environment = captured_process_environment(&config.shell);
        let victim_worker_root = victim_root.clone();
        let victim_pid_target = victim_pid_file.clone();
        let victim = tokio::spawn(async move {
            execute_workspace_write(
                victim_profile,
                SandboxedProcessRequest {
                    argv: vec![
                        "powershell.exe".to_string(),
                        "-NoProfile".to_string(),
                        "-NonInteractive".to_string(),
                        "-Command".to_string(),
                        format!(
                            "Set-Content -LiteralPath '{}' -Value $PID; Start-Sleep -Seconds 30",
                            ps_literal(&victim_pid_target)
                        ),
                    ],
                    cwd: victim_worker_root,
                    environment: victim_environment,
                    stdin: Vec::new(),
                    timeout_ms: 60_000,
                    max_output_bytes: 8_192,
                    hide_window: true,
                    cancel: victim_worker_cancel,
                },
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            while !victim_pid_file.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("victim process must publish PID");
        let victim_pid = std::fs::read_to_string(&victim_pid_file)
            .expect("victim PID")
            .trim()
            .parse::<u32>()
            .expect("numeric victim PID");
        let attacker_script = format!(
            concat!(
                "Add-Type -TypeDefinition @'\n",
                "using System; using System.Runtime.InteropServices; ",
                "public static class Native {{ ",
                "[DllImport(\"kernel32.dll\", SetLastError=true)] public static extern IntPtr OpenProcess(uint access, bool inherit, uint pid); ",
                "[DllImport(\"kernel32.dll\")] public static extern bool CloseHandle(IntPtr handle); }}\n",
                "'@; ",
                "$control=[Native]::OpenProcess(0x1F0FFF,$false,{}); ",
                "if ($control -eq [IntPtr]::Zero) {{ Write-Output control-blocked }} else {{ [Native]::CloseHandle($control) | Out-Null; Write-Output control-opened }}; ",
                "$read=[Native]::OpenProcess(0x410,$false,{}); ",
                "if ($read -eq [IntPtr]::Zero) {{ Write-Output read-blocked }} else {{ [Native]::CloseHandle($read) | Out-Null; Write-Output read-opened }}"
            ),
            victim_pid, victim_pid
        );
        let attacker = execute_workspace_write(
            attacker_profile,
            SandboxedProcessRequest {
                argv: vec![
                    "powershell.exe".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    attacker_script,
                ],
                cwd: attacker_root,
                environment: captured_process_environment(&config.shell),
                stdin: Vec::new(),
                timeout_ms: 30_000,
                max_output_bytes: 8_192,
                hide_window: true,
                cancel: CancellationToken::new(),
            },
        )
        .await
        .expect("attacker sandbox execution");
        victim_cancel.cancel();
        let victim_result = tokio::time::timeout(Duration::from_secs(10), victim)
            .await
            .expect("victim cleanup timeout")
            .expect("victim task")
            .expect("victim sandbox result");

        assert!(victim_result.cancelled);
        assert!(
            String::from_utf8_lossy(&attacker.stdout.bytes).contains("control-blocked")
                && String::from_utf8_lossy(&attacker.stdout.bytes).contains("read-blocked"),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&attacker.stdout.bytes),
            String::from_utf8_lossy(&attacker.stderr.bytes)
        );
        assert!(!String::from_utf8_lossy(&attacker.stdout.bytes).contains("opened"));
    }

    #[tokio::test]
    async fn bounded_audit_denies_an_everyone_writable_outside_candidate() {
        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-world-write-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let fixture =
            Utf8PathBuf::from_path_buf(fixture.path().to_path_buf()).expect("utf8 fixture");
        let workspace_root = fixture.join("workspace");
        let outside_root = fixture.join("everyone-writable");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        std::fs::create_dir_all(&outside_root).expect("outside root");
        let grant = std::process::Command::new("icacls.exe")
            .arg(outside_root.as_std_path())
            .args(["/grant", "*S-1-1-0:(OI)(CI)M"])
            .output()
            .expect("icacls grant");
        assert!(
            grant.status.success(),
            "icacls stderr={}",
            String::from_utf8_lossy(&grant.stderr)
        );

        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &workspace)
                .expect("sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let outside_target = outside_root.join("escape.txt");
        let script = format!(
            "try {{ Set-Content -LiteralPath '{}' -Value escaped -ErrorAction Stop; Write-Output outside-allowed }} catch {{ Write-Output outside-denied }}",
            ps_literal(&outside_target)
        );
        let mut environment = captured_process_environment(&config.shell);
        let existing_path = environment
            .get("PATH")
            .cloned()
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();
        environment.insert(
            "PATH".to_string(),
            format!("{};{existing_path}", outside_root),
        );
        let output = execute_workspace_write(
            profile,
            SandboxedProcessRequest {
                argv: vec![
                    "powershell.exe".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    script,
                ],
                cwd: workspace_root,
                environment,
                stdin: Vec::new(),
                timeout_ms: 30_000,
                max_output_bytes: 8_192,
                hide_window: true,
                cancel: CancellationToken::new(),
            },
        )
        .await
        .expect("sandbox execution");

        assert_eq!(output.exit_code, Some(0));
        assert!(
            String::from_utf8_lossy(&output.stdout.bytes).contains("outside-denied"),
            "stdout={} stderr={} cleanup={:?}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes),
            output.cleanup_errors
        );
        assert!(!outside_target.exists(), "Everyone ACE bypassed sandbox");
    }

    #[tokio::test]
    async fn restricted_capture_bounds_concurrent_stdout_and_stderr() {
        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-output-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let workspace_root =
            Utf8PathBuf::from_path_buf(fixture.path().join("workspace")).expect("utf8 workspace");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &workspace)
                .expect("sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let output = execute_workspace_write(
            profile,
            SandboxedProcessRequest {
                argv: vec![
                    "powershell.exe".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    "$chunk='x' * 8192; 1..32 | ForEach-Object { [Console]::Out.Write($chunk); [Console]::Error.Write($chunk) }; exit 7".to_string(),
                ],
                cwd: workspace_root,
                environment: captured_process_environment(&config.shell),
                stdin: Vec::new(),
                timeout_ms: 30_000,
                max_output_bytes: 1_024,
                hide_window: true,
                cancel: CancellationToken::new(),
            },
        )
        .await
        .expect("sandbox execution");

        assert_eq!(output.exit_code, Some(7));
        assert!(output.stdout.truncated);
        assert!(output.stderr.truncated);
        assert_eq!(output.stdout.bytes.len(), 1_024);
        assert_eq!(output.stderr.bytes.len(), 1_024);
        assert!(
            output.cleanup_errors.is_empty(),
            "{:?}",
            output.cleanup_errors
        );
    }

    #[tokio::test]
    async fn unrelated_inheritable_parent_handle_is_not_visible_to_the_child() {
        use std::io::Write as _;
        use std::os::windows::io::AsRawHandle as _;

        use windows_sys::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation};

        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-handle-list-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let workspace_root =
            Utf8PathBuf::from_path_buf(fixture.path().join("workspace")).expect("utf8 workspace");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        let marker = workspace_root.join("unrelated-handle.txt");
        let mut unrelated = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&marker)
            .expect("unrelated parent handle");
        unrelated.flush().expect("flush marker");
        let raw_handle = unrelated.as_raw_handle() as HANDLE;
        assert_ne!(
            unsafe { SetHandleInformation(raw_handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) },
            0,
            "mark unrelated handle inheritable: {}",
            std::io::Error::last_os_error()
        );

        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &workspace)
                .expect("sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let script = format!(
            concat!(
                "$handle = [IntPtr]::new({}); ",
                "try {{ ",
                "$safe = [Microsoft.Win32.SafeHandles.SafeFileHandle]::new($handle, $false); ",
                "$stream = [System.IO.FileStream]::new($safe, [System.IO.FileAccess]::Write); ",
                "$stream.WriteByte(88); $stream.Flush(); Write-Output leaked ",
                "}} catch {{ Write-Output blocked }}"
            ),
            raw_handle as usize
        );
        let output = execute_workspace_write(
            profile,
            SandboxedProcessRequest {
                argv: vec![
                    "powershell.exe".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    script,
                ],
                cwd: workspace_root,
                environment: captured_process_environment(&config.shell),
                stdin: Vec::new(),
                timeout_ms: 30_000,
                max_output_bytes: 8_192,
                hide_window: true,
                cancel: CancellationToken::new(),
            },
        )
        .await
        .expect("sandbox execution");
        drop(unrelated);

        assert_eq!(output.exit_code, Some(0));
        assert!(
            String::from_utf8_lossy(&output.stdout.bytes).contains("blocked"),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        assert!(
            std::fs::read(&marker).expect("marker read").is_empty(),
            "an unrelated inheritable parent handle leaked into the child"
        );
    }

    #[tokio::test]
    async fn incomplete_sandbox_stdin_is_not_reported_as_clean_success() {
        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-stdin-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let workspace_root =
            Utf8PathBuf::from_path_buf(fixture.path().join("workspace")).expect("utf8 workspace");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &workspace)
                .expect("sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let output = execute_workspace_write(
            profile,
            SandboxedProcessRequest {
                argv: vec![
                    "powershell.exe".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    "exit 0".to_string(),
                ],
                cwd: workspace_root,
                environment: captured_process_environment(&config.shell),
                stdin: vec![b'x'; 1024 * 1024],
                timeout_ms: 30_000,
                max_output_bytes: 1_024,
                hide_window: true,
                cancel: CancellationToken::new(),
            },
        )
        .await
        .expect("sandbox execution");

        assert_eq!(output.exit_code, Some(0));
        assert!(
            output
                .cleanup_error()
                .is_some_and(|error| error.contains("complete sandbox stdin")),
            "{:?}",
            output.cleanup_errors
        );
    }

    #[tokio::test]
    async fn cancellation_terminates_the_sandboxed_process_tree() {
        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-cancel-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let fixture =
            Utf8PathBuf::from_path_buf(fixture.path().to_path_buf()).expect("utf8 fixture");
        let workspace_root = fixture.join("workspace");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        let child_started = workspace_root.join("child-started.txt");
        let late = workspace_root.join("late.txt");
        let descendant = workspace_root.join("descendant.ps1");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &workspace)
                .expect("sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let descendant_script = format!(
            "Set-Content -LiteralPath '{}' -Value started; Start-Sleep -Seconds 3; Set-Content -LiteralPath '{}' -Value late",
            ps_literal(&child_started),
            ps_literal(&late)
        );
        std::fs::write(&descendant, descendant_script).expect("descendant script");
        let script = format!(
            "& powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File '{}'; exit $LASTEXITCODE",
            ps_literal(&descendant)
        );
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let environment = captured_process_environment(&config.shell);
        let worker_root = workspace_root.clone();
        let worker = tokio::spawn(async move {
            execute_workspace_write(
                profile,
                SandboxedProcessRequest {
                    argv: vec![
                        "powershell.exe".to_string(),
                        "-NoProfile".to_string(),
                        "-NonInteractive".to_string(),
                        "-Command".to_string(),
                        script,
                    ],
                    cwd: worker_root,
                    environment,
                    stdin: Vec::new(),
                    timeout_ms: 60_000,
                    max_output_bytes: 8_192,
                    hide_window: true,
                    cancel: worker_cancel,
                },
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(10), async {
            while !child_started.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("sandboxed descendant must start");
        cancel.cancel();
        let output = tokio::time::timeout(Duration::from_secs(10), worker)
            .await
            .expect("sandbox cancellation timeout")
            .expect("sandbox worker")
            .expect("sandbox cancellation result");
        assert!(output.cancelled);
        assert!(output.effect_started);
        assert!(
            output.cleanup_errors.is_empty(),
            "{:?}",
            output.cleanup_errors
        );
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(
            !late.exists(),
            "sandboxed descendant survived Job termination"
        );
    }

    #[tokio::test]
    async fn normal_parent_exit_terminates_sandboxed_descendants() {
        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-exit-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let fixture =
            Utf8PathBuf::from_path_buf(fixture.path().to_path_buf()).expect("utf8 fixture");
        let workspace_root = fixture.join("workspace");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        let child_started = workspace_root.join("child-started.txt");
        let late = workspace_root.join("late.txt");
        let descendant = workspace_root.join("descendant.ps1");
        std::fs::write(
            &descendant,
            format!(
                "Set-Content -LiteralPath '{}' -Value started; Start-Sleep -Seconds 3; Set-Content -LiteralPath '{}' -Value late",
                ps_literal(&child_started),
                ps_literal(&late)
            ),
        )
        .expect("descendant script");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &workspace)
                .expect("sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let script = format!(
            "Start-Process -FilePath powershell.exe -ArgumentList @('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File','{}') -NoNewWindow | Out-Null; Start-Sleep -Milliseconds 750; exit 0",
            ps_literal(&descendant)
        );
        let output = execute_workspace_write(
            profile,
            SandboxedProcessRequest {
                argv: vec![
                    "powershell.exe".to_string(),
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    script,
                ],
                cwd: workspace_root,
                environment: captured_process_environment(&config.shell),
                stdin: Vec::new(),
                timeout_ms: 10_000,
                max_output_bytes: 8_192,
                hide_window: true,
                cancel: CancellationToken::new(),
            },
        )
        .await
        .expect("sandbox execution");

        assert_eq!(output.exit_code, Some(0));
        assert!(
            child_started.exists(),
            "sandboxed descendant did not start; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout.bytes),
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        assert!(
            output.cleanup_errors.is_empty(),
            "{:?}",
            output.cleanup_errors
        );
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!late.exists(), "descendant survived normal parent exit");
    }

    #[tokio::test]
    async fn timeout_terminates_an_already_started_sandboxed_descendant() {
        std::fs::create_dir_all("target").expect("target directory");
        let fixture = tempfile::Builder::new()
            .prefix("moyai-sandbox-timeout-")
            .tempdir_in("target")
            .expect("sandbox fixture");
        let fixture =
            Utf8PathBuf::from_path_buf(fixture.path().to_path_buf()).expect("utf8 fixture");
        let workspace_root = fixture.join("workspace");
        std::fs::create_dir_all(&workspace_root).expect("workspace");
        let child_started = workspace_root.join("child-started.txt");
        let late = workspace_root.join("late.txt");
        let descendant = workspace_root.join("descendant.ps1");
        let config = ResolvedConfig::default();
        let workspace = WorkspaceDiscovery::discover_fixed_root(&workspace_root, &config)
            .expect("workspace discovery");
        let ProcessSandboxPlan::WorkspaceWrite(profile) =
            ProcessSandboxPlan::for_access_mode(AccessMode::Default, &workspace)
                .expect("sandbox profile")
        else {
            panic!("default must resolve workspace-write");
        };
        let descendant_script = format!(
            "Set-Content -LiteralPath '{}' -Value started; Start-Sleep -Seconds 8; Set-Content -LiteralPath '{}' -Value late",
            ps_literal(&child_started),
            ps_literal(&late)
        );
        std::fs::write(&descendant, descendant_script).expect("descendant script");
        let script = format!(
            "& powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File '{}'; exit $LASTEXITCODE",
            ps_literal(&descendant)
        );
        let environment = captured_process_environment(&config.shell);
        let worker_root = workspace_root.clone();
        let worker = tokio::spawn(async move {
            execute_workspace_write(
                profile,
                SandboxedProcessRequest {
                    argv: vec![
                        "powershell.exe".to_string(),
                        "-NoProfile".to_string(),
                        "-NonInteractive".to_string(),
                        "-Command".to_string(),
                        script,
                    ],
                    cwd: worker_root,
                    environment,
                    stdin: Vec::new(),
                    timeout_ms: 5_000,
                    max_output_bytes: 8_192,
                    hide_window: true,
                    cancel: CancellationToken::new(),
                },
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(10), async {
            while !child_started.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("sandboxed descendant must start before timeout");
        let output = tokio::time::timeout(Duration::from_secs(10), worker)
            .await
            .expect("sandbox timeout result")
            .expect("sandbox worker")
            .expect("sandbox execution result");
        assert!(output.timed_out);
        assert!(output.effect_started);
        assert!(
            output.cleanup_errors.is_empty(),
            "{:?}",
            output.cleanup_errors
        );
        tokio::time::sleep(Duration::from_secs(9)).await;
        assert!(
            !late.exists(),
            "timed-out descendant survived Job termination"
        );
    }
}
