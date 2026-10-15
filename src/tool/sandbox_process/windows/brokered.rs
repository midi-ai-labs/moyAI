use super::*;
use std::sync::Mutex;
use windows_sys::Win32::Security::OWNER_SECURITY_INFORMATION;

// Only security-descriptor mutations are serialized; child processes run in parallel.
static ACL_MUTATION: Mutex<()> = Mutex::new(());
static WINDOW_STATION_SWITCH: Mutex<()> = Mutex::new(());
const MAX_HANDOFF_OBJECTS: usize = 100_000;
const MAX_HANDOFF_DEPTH: usize = 256;
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(30);

mod changes;

#[link(name = "user32")]
unsafe extern "system" {
    fn CreateWindowStationW(
        name: *const u16,
        flags: u32,
        access: u32,
        attributes: *const SECURITY_ATTRIBUTES,
    ) -> HANDLE;
    fn GetProcessWindowStation() -> HANDLE;
    fn SetProcessWindowStation(station: HANDLE) -> i32;
    fn CloseWindowStation(station: HANDLE) -> i32;
    fn CreateDesktopW(
        name: *const u16,
        device: *const u16,
        mode: *const c_void,
        flags: u32,
        access: u32,
        attributes: *const SECURITY_ATTRIBUTES,
    ) -> HANDLE;
    fn CloseDesktop(desktop: HANDLE) -> i32;
}

pub(super) fn impersonated<T>(
    caller: HANDLE,
    operation: impl FnOnce() -> Result<T, SandboxExecutionError>,
) -> Result<T, SandboxExecutionError> {
    let mut previous = null_mut();
    let has_previous = unsafe {
        windows_sys::Win32::System::Threading::OpenThreadToken(
            windows_sys::Win32::System::Threading::GetCurrentThread(),
            TOKEN_QUERY | windows_sys::Win32::Security::TOKEN_IMPERSONATE,
            1,
            &mut previous,
        )
    } != 0;
    if !has_previous && unsafe { GetLastError() } != windows_sys::Win32::Foundation::ERROR_NO_TOKEN
    {
        return Err(win32("save current thread impersonation"));
    }
    struct Restore(Option<OwnedHandle>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(token) = &self.0 {
                if unsafe { windows_sys::Win32::Security::ImpersonateLoggedOnUser(token.raw()) }
                    == 0
                {
                    std::process::abort();
                }
            }
        }
    }
    let restore = Restore(has_previous.then(|| OwnedHandle(previous)));
    let result = crate::sandbox_broker::with_impersonation(caller, || Ok(operation()))
        .map_err(SandboxExecutionError::Initialization);
    drop(restore);
    result?
}

pub(crate) fn execute(
    mut profile: WorkspaceWriteSandboxProfile,
    mut request: SandboxedProcessRequest,
    started: Option<crate::tool::sandbox_process::ProcessStarted>,
    caller: HANDLE,
) -> Result<SandboxedProcessOutput, SandboxExecutionError> {
    if request.argv.is_empty() || request.argv[0] != request.executable.path().as_str() {
        return Err(SandboxExecutionError::InvalidProfile(
            "sandbox executable argument does not match its pinned identity".into(),
        ));
    }
    // The token includes one additional, execution-local TEMP capability.
    if profile.writable_roots.len() >= 128 || profile.effect_temp_directory.is_some() {
        return Err(SandboxExecutionError::InvalidProfile(
            "sandbox writable root count or effect TEMP is invalid".into(),
        ));
    }
    if request.cancel.is_cancelled() {
        return Ok(cancelled_before_effect(Vec::new(), true));
    }
    apply_advisory_offline_environment(&mut request.environment);
    let effect_temp = impersonated(caller, || {
        create_effect_local_temp(&mut profile, &mut request.environment)
    })?;
    let prepared = impersonated(caller, || {
        let _lock = ACL_MUTATION.lock().map_err(|_| {
            SandboxExecutionError::Initialization("sandbox ACL mutation lock is poisoned".into())
        })?;
        prepare_capabilities_and_acls(
            &profile,
            &request.cwd,
            &request.environment,
            &request.cancel,
        )
    });
    let mut prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => return finish_before_spawn(caller, effect_temp, error),
    };
    let host_sid = match crate::sandbox_broker::token::user_sid(caller) {
        Ok(sid) => sid,
        Err(error) => {
            drop(prepared);
            return finish_before_spawn(
                caller,
                effect_temp,
                SandboxExecutionError::Initialization(error),
            );
        }
    };
    let owner_sid = sandbox_process_private_sid();
    let roots: Vec<_> = prepared
        .roots
        .iter()
        .map(|root| capability_sid_for_root(&root.root))
        .collect();
    let token = match crate::sandbox_broker::token::mint_token(caller, &roots, &owner_sid).and_then(
        |token| {
            token.set_session(0)?;
            Ok(token)
        },
    ) {
        Ok(token) => token,
        Err(error) => {
            drop(prepared);
            return finish_before_spawn(
                caller,
                effect_temp,
                SandboxExecutionError::Initialization(error),
            );
        }
    };
    let desktop = match PrivateDesktop::create(&host_sid, &owner_sid) {
        Ok(desktop) => desktop,
        Err(error) => {
            drop(prepared);
            return finish_before_spawn(caller, effect_temp, error);
        }
    };
    let watches = match impersonated(caller, || changes::WatchSet::start(&profile, &prepared)) {
        Ok(watches) => watches,
        Err(error) => {
            drop(desktop);
            drop(token);
            drop(prepared);
            return finish_before_spawn(caller, effect_temp, error);
        }
    };
    // A panic cannot prove Job-zero. Retain the TEMP, rather than racing surviving children.
    let output = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        spawn_and_capture(token.raw(), request, started, &desktop.name, caller)
    }));
    let output = match output {
        Ok(output) => output,
        Err(_) => {
            return Err(SandboxExecutionError::Worker(format!(
                "sandbox worker panicked; effect TEMP `{}` retained because process-tree exit is unknown",
                effect_temp.path.display()
            )));
        }
    };
    // The exact TEMP pin must be released before deleting that directory; all
    // admitted workspace and protected-path pins remain held for handoff.
    prepared
        .roots
        .retain(|root| profile.effect_temp_directory.as_ref() != Some(&root.root));
    let mut cleanup_errors = Vec::new();
    if matches!(
        effect_temp_finalization_for_result(&output),
        EffectTempFinalization::Remove
    ) {
        let changed = watches.finish(caller);
        let cleanup = impersonated(token.raw(), || {
            let _lock = ACL_MUTATION.lock().map_err(|_| {
                SandboxExecutionError::Initialization(
                    "sandbox ACL mutation lock is poisoned".into(),
                )
            })?;
            let mut errors = Vec::new();
            if let Err(error) = changed.and_then(|changed| {
                hand_back_workspace(&profile, &prepared, &owner_sid, &host_sid, caller, changed)
            }) {
                errors.push(format!("workspace handoff failed: {error}"));
            }
            // Directory removal stays on this impersonating thread. A background worker
            // would otherwise run with the broker's SYSTEM process identity.
            if let Err(error) = validate_temp_tree(&profile, &effect_temp) {
                errors.push(format!("effect TEMP retained: {error}"));
            } else {
                errors.extend(remove_temp(effect_temp));
            }
            Ok(errors)
        });
        match cleanup {
            Ok(errors) => cleanup_errors.extend(errors),
            Err(error) => cleanup_errors.push(format!(
                "restricted cleanup failed; effect TEMP retained: {error}"
            )),
        }
    } else {
        drop(watches);
        cleanup_errors.push(format!(
            "effect TEMP `{}` retained because process-tree exit is unknown",
            effect_temp.path.display()
        ));
    }
    drop(desktop);
    drop(token);
    drop(prepared);
    match output {
        Ok(mut output) => {
            output.cleanup_errors.extend(cleanup_errors);
            Ok(output)
        }
        Err(error) => Err(with_cleanup_errors(error, cleanup_errors)),
    }
}

fn finish_before_spawn(
    caller: HANDLE,
    temp: EffectLocalTempDirectory,
    error: SandboxExecutionError,
) -> Result<SandboxedProcessOutput, SandboxExecutionError> {
    let errors = match impersonated(caller, || Ok(remove_temp(temp))) {
        Ok(errors) => errors,
        Err(error) => vec![format!("effect TEMP cleanup failed: {error}")],
    };
    Err(with_cleanup_errors(error, errors))
}

fn remove_temp(temp: EffectLocalTempDirectory) -> Vec<String> {
    let path = temp.path.clone();
    temp.remove()
        .err()
        .map(|error| format!("remove effect TEMP `{}`: {error}", path.display()))
        .into_iter()
        .collect()
}

struct PrivateDesktop {
    station: HANDLE,
    desktop: HANDLE,
    name: String,
}

impl PrivateDesktop {
    fn create(host: &str, owner: &str) -> Result<Self, SandboxExecutionError> {
        let _lock = WINDOW_STATION_SWITCH.lock().map_err(|_| {
            SandboxExecutionError::Initialization("window station lock is poisoned".into())
        })?;
        let name = format!("moyai-{}", ulid::Ulid::new());
        let sddl = to_wide(&format!("D:P(A;;GA;;;SY)(A;;GA;;;{host})(A;;GA;;;{owner})"));
        let mut descriptor = null_mut();
        if unsafe {
            windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut descriptor, null_mut())
        } == 0
        {
            return Err(win32("private desktop security descriptor"));
        }
        let _descriptor = LocalAllocation(descriptor);
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let station_name = to_wide(&name);
        let station =
            unsafe { CreateWindowStationW(station_name.as_ptr(), 0, GENERIC_ALL, &attributes) };
        if !valid_handle(station) {
            return Err(win32("create private window station"));
        }
        let mut result = Self {
            station,
            desktop: null_mut(),
            name: format!("{name}\\Default"),
        };
        let previous = unsafe { GetProcessWindowStation() };
        if !valid_handle(previous) {
            return Err(win32("query broker window station"));
        }
        if unsafe { SetProcessWindowStation(station) } == 0 {
            return Err(win32("select private window station"));
        }
        struct Restore(HANDLE);
        impl Drop for Restore {
            fn drop(&mut self) {
                if unsafe { SetProcessWindowStation(self.0) } == 0 {
                    // Other workers must never continue with a different process station.
                    std::process::abort();
                }
            }
        }
        let restore = Restore(previous);
        let desktop_name = to_wide("Default");
        result.desktop = unsafe {
            CreateDesktopW(
                desktop_name.as_ptr(),
                null(),
                null(),
                0,
                GENERIC_ALL,
                &attributes,
            )
        };
        let failure = (!valid_handle(result.desktop)).then(|| win32("create private desktop"));
        drop(restore);
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(result)
    }
}

impl Drop for PrivateDesktop {
    fn drop(&mut self) {
        if valid_handle(self.desktop) {
            unsafe {
                CloseDesktop(self.desktop);
            }
        }
        if valid_handle(self.station) {
            unsafe {
                CloseWindowStation(self.station);
            }
        }
    }
}

fn win32(operation: &str) -> SandboxExecutionError {
    SandboxExecutionError::Initialization(format!(
        "{operation}: {}",
        std::io::Error::last_os_error()
    ))
}

struct WalkBudget {
    objects: usize,
    deadline: Instant,
}
impl WalkBudget {
    fn visit(&mut self, depth: usize) -> Result<(), SandboxExecutionError> {
        self.objects += 1;
        if self.objects > MAX_HANDOFF_OBJECTS
            || depth > MAX_HANDOFF_DEPTH
            || Instant::now() >= self.deadline
        {
            return Err(SandboxExecutionError::Initialization(
                "sandbox handoff traversal did not complete within its safety bounds".into(),
            ));
        }
        Ok(())
    }
}

fn hand_back_workspace(
    profile: &WorkspaceWriteSandboxProfile,
    prepared: &PreparedSandbox,
    owner: &str,
    host: &str,
    caller: HANDLE,
    changed: Vec<changes::ChangedRoot>,
) -> Result<(), SandboxExecutionError> {
    let owner = LocalSid::from_string(owner)?;
    let host = LocalSid::from_string(host)?;
    let mut budget = WalkBudget {
        objects: 0,
        deadline: Instant::now() + HANDOFF_TIMEOUT,
    };
    for changed in changed {
        let root = prepared
            .roots
            .iter()
            .find(|root| root.root == changed.root)
            .ok_or_else(|| {
                SandboxExecutionError::Initialization(
                    "changed handoff root was not admitted".into(),
                )
            })?;
        verify_opened_path(root._handle.raw(), &root.root.canonical)?;
        if opened_object_identity(root._handle.raw())? != root.root.identity {
            return Err(SandboxExecutionError::Initialization(
                "admitted handoff root identity changed".into(),
            ));
        }
        let mut visited_directories = Vec::<Utf8PathBuf>::new();
        for path in changed.paths {
            if visited_directories
                .iter()
                .any(|directory| path_is_within(&path, directory))
            {
                continue;
            }
            if profile
                .read_only_roots
                .iter()
                .any(|protected| path_is_within(&path, &protected.requested))
            {
                continue;
            }
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if matches!(error.raw_os_error(), Some(2 | 3)) => continue,
                Err(error) => {
                    return Err(SandboxExecutionError::Initialization(format!(
                        "changed handoff object `{path}`: {error}"
                    )));
                }
            };
            let _ancestors = pin_handoff_ancestors(&path, &root.root)?;
            walk(
                &path,
                &root.root.canonical,
                profile,
                &mut budget,
                0,
                Some((owner.raw(), host.raw(), root.sid.raw(), caller)),
            )?;
            if metadata.is_dir() && !metadata.file_type().is_symlink() {
                visited_directories.push(path);
            }
        }
    }
    Ok(())
}

fn pin_handoff_ancestors(
    path: &camino::Utf8Path,
    root: &crate::tool::os_sandbox::SandboxPathSnapshot,
) -> Result<Vec<OwnedHandle>, SandboxExecutionError> {
    let relative = path.strip_prefix(&root.requested).map_err(|_| {
        SandboxExecutionError::Initialization(
            "changed handoff path escaped its requested root".into(),
        )
    })?;
    let mut pins = Vec::new();
    let mut current = root.requested.clone();
    let components: Vec<_> = relative.components().collect();
    for component in components.iter().take(components.len().saturating_sub(1)) {
        current.push(component.as_str());
        let pin = open_walk_object(
            &current,
            READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
        )?;
        checked_walk_object(pin.raw(), &current, &root.canonical)?;
        pins.push(pin);
    }
    Ok(pins)
}

fn validate_temp_tree(
    profile: &WorkspaceWriteSandboxProfile,
    temp: &EffectLocalTempDirectory,
) -> Result<(), SandboxExecutionError> {
    let snapshot = profile.effect_temp_directory.as_ref().ok_or_else(|| {
        SandboxExecutionError::InvalidProfile("effect TEMP snapshot missing".into())
    })?;
    if temp.path != snapshot.requested.as_std_path() {
        return Err(SandboxExecutionError::InvalidProfile(
            "effect TEMP cleanup path differs from its admission".into(),
        ));
    }
    let pin = open_verified_identity_path(snapshot)?;
    let mut budget = WalkBudget {
        objects: 0,
        deadline: Instant::now() + HANDOFF_TIMEOUT,
    };
    walk(
        &snapshot.requested,
        &snapshot.canonical,
        profile,
        &mut budget,
        0,
        None,
    )?;
    drop(pin);
    Ok(())
}

fn walk(
    path: &camino::Utf8Path,
    root: &camino::Utf8Path,
    profile: &WorkspaceWriteSandboxProfile,
    budget: &mut WalkBudget,
    depth: usize,
    handoff: Option<(*mut c_void, *mut c_void, *mut c_void, HANDLE)>,
) -> Result<(), SandboxExecutionError> {
    budget.visit(depth)?;
    if profile
        .read_only_roots
        .iter()
        .any(|protected| path_is_within(path, &protected.requested))
    {
        return Ok(());
    }
    let pin = open_walk_object(
        path,
        READ_CONTROL | FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY,
    )?;
    let info = walk_object_information(pin.raw())?;
    let fresh_owned = match handoff {
        Some((owner, ..)) => owned_by(pin.raw(), owner)?,
        None => true,
    };
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || (info.dwFileAttributes & 0x10 == 0 && info.nNumberOfLinks != 1)
    {
        // TEMP deletion unlinks these objects without following them. Workspace
        // handback keeps rejecting fresh-owner links rather than altering targets.
        if handoff.is_none() {
            return Ok(());
        }
        if fresh_owned {
            return Err(SandboxExecutionError::Initialization(format!(
                "sandbox cleanup rejects fresh-owner reparse or hard-linked object `{path}`"
            )));
        }
        return Ok(());
    }
    checked_walk_object(pin.raw(), path, root)?;
    let is_directory = info.dwFileAttributes & 0x10 != 0;
    if is_directory {
        for entry in std::fs::read_dir(path).map_err(|error| {
            SandboxExecutionError::Initialization(format!(
                "enumerate sandbox handoff `{path}`: {error}"
            ))
        })? {
            let entry = entry.map_err(|error| {
                SandboxExecutionError::Initialization(format!(
                    "read sandbox handoff entry: {error}"
                ))
            })?;
            let child = Utf8PathBuf::from_path_buf(entry.path()).map_err(|_| {
                SandboxExecutionError::Initialization("sandbox handoff path is not UTF-8".into())
            })?;
            walk(&child, root, profile, budget, depth + 1, handoff)?;
        }
    }
    if let Some((fresh_owner, host, capability, caller)) = handoff {
        if fresh_owned {
            // Inherited host/root-capability ACLs already preserve ordinary 0777
            // objects. They need no owner change; WRITE_OWNER is deliberately absent
            // from root capabilities. Protected owner-only 0700 objects do need it.
            let host_access = impersonated(caller, || {
                // The identity pin intentionally denies delete sharing. Probe
                // host read/write rights without requesting DELETE on that pin.
                let handle = open_walk_object(path, AclChange::AllowWrite.mask() & !DELETE)?;
                checked_walk_object(handle.raw(), path, root)?;
                if opened_object_identity(handle.raw())? != opened_object_identity(pin.raw())? {
                    return Err(SandboxExecutionError::Initialization(
                        "host access probe identity changed".into(),
                    ));
                }
                Ok(())
            });
            if host_access.is_ok() && has_capability_write(pin.raw(), capability)? {
                return Ok(());
            }
            let writable = open_walk_object(
                path,
                READ_CONTROL | WRITE_DAC | WRITE_OWNER | FILE_READ_ATTRIBUTES,
            )?;
            checked_walk_object(writable.raw(), path, root)?;
            if opened_object_identity(pin.raw())? != opened_object_identity(writable.raw())?
                || !owned_by(writable.raw(), fresh_owner)?
            {
                return Err(SandboxExecutionError::Initialization(format!(
                    "sandbox handoff object identity changed at `{path}`"
                )));
            }
            handoff_security(writable.raw(), path, host, capability)?;
        }
    }
    Ok(())
}

fn open_walk_object(
    path: &camino::Utf8Path,
    access: u32,
) -> Result<OwnedHandle, SandboxExecutionError> {
    let wide = to_wide(path.as_str());
    OwnedHandle::new(
        unsafe {
            CreateFileW(
                wide.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
                null_mut(),
            )
        },
        &format!("sandbox handoff object `{path}` (access 0x{access:x})"),
    )
}

fn checked_walk_object(
    handle: HANDLE,
    path: &camino::Utf8Path,
    root: &camino::Utf8Path,
) -> Result<BY_HANDLE_FILE_INFORMATION, SandboxExecutionError> {
    let info = walk_object_information(handle)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(SandboxExecutionError::Initialization(format!(
            "sandbox handoff rejects reparse object `{path}`"
        )));
    }
    if info.dwFileAttributes & 0x10 == 0 && info.nNumberOfLinks != 1 {
        return Err(SandboxExecutionError::Initialization(format!(
            "sandbox handoff rejects hard-linked file `{path}`"
        )));
    }
    let canonical = opened_identity_path(handle, path)?;
    if !path_is_within(&canonical, root) {
        return Err(SandboxExecutionError::Initialization(format!(
            "sandbox handoff object `{path}` escaped its admitted root"
        )));
    }
    Ok(info)
}

fn walk_object_information(
    handle: HANDLE,
) -> Result<BY_HANDLE_FILE_INFORMATION, SandboxExecutionError> {
    let mut info = unsafe { zeroed() };
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(win32("sandbox handoff object information"));
    }
    Ok(info)
}

fn has_capability_write(
    handle: HANDLE,
    capability: *mut c_void,
) -> Result<bool, SandboxExecutionError> {
    let mut descriptor = null_mut();
    let mut acl = null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(SandboxExecutionError::Initialization(format!(
            "read retained capability DACL: Win32 {status}"
        )));
    }
    let _descriptor = LocalAllocation(descriptor);
    Ok(!acl.is_null()
        && acl_has_entry(
            acl,
            capability,
            ACCESS_ALLOWED_ACE_TYPE,
            AclChange::AllowWrite.mask(),
            0,
        ))
}

fn owned_by(handle: HANDLE, expected: *mut c_void) -> Result<bool, SandboxExecutionError> {
    let mut descriptor = null_mut();
    let mut owner = null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(SandboxExecutionError::Initialization(format!(
            "read sandbox handoff owner: Win32 {status}"
        )));
    }
    let _descriptor = LocalAllocation(descriptor);
    if owner.is_null() || unsafe { IsValidSid(owner) } == 0 {
        return Err(SandboxExecutionError::Initialization(
            "sandbox handoff owner SID is invalid".into(),
        ));
    }
    Ok(unsafe { EqualSid(owner, expected) } != 0)
}

fn handoff_security(
    handle: HANDLE,
    path: &camino::Utf8Path,
    host: *mut c_void,
    capability: *mut c_void,
) -> Result<(), SandboxExecutionError> {
    let mut descriptor = null_mut();
    let mut existing = null_mut();
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut existing,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(SandboxExecutionError::Initialization(format!(
            "read sandbox handoff DACL: Win32 {status}"
        )));
    }
    let _descriptor = LocalAllocation(descriptor);
    if existing.is_null() {
        return Err(SandboxExecutionError::Initialization(format!(
            "sandbox handoff rejects NULL DACL at `{path}`"
        )));
    }
    let inheritance = if walk_object_information(handle)?.dwFileAttributes & 0x10 != 0 {
        CONTAINER_AND_OBJECT_INHERIT
    } else {
        0
    };
    let entries = [
        (host, GENERIC_ALL),
        (capability, AclChange::AllowWrite.mask()),
    ]
    .map(|(sid, mask)| EXPLICIT_ACCESS_W {
        grfAccessPermissions: mask,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.cast(),
        },
    });
    let mut acl = null_mut();
    let status =
        unsafe { SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), existing, &mut acl) };
    if status != ERROR_SUCCESS {
        return Err(SandboxExecutionError::Initialization(format!(
            "build sandbox handoff DACL: Win32 {status}"
        )));
    }
    let acl = LocalAcl(acl);
    apply_handoff_dacl(handle, path, acl.0)?;
    let status = unsafe {
        SetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            host,
            null_mut(),
            null_mut(),
            null_mut(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(SandboxExecutionError::Initialization(format!(
            "restore sandbox handoff owner: Win32 {status}"
        )));
    }
    if !owned_by(handle, host)? {
        return Err(SandboxExecutionError::Initialization(
            "sandbox handoff owner verification failed".into(),
        ));
    }
    Ok(())
}

fn apply_handoff_dacl(
    handle: HANDLE,
    path: &camino::Utf8Path,
    acl: *mut ACL,
) -> Result<(), SandboxExecutionError> {
    if walk_object_information(handle)?.dwFileAttributes & 0x10 == 0 {
        let status = unsafe {
            SetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                acl,
                null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(SandboxExecutionError::Initialization(format!(
                "apply sandbox handoff DACL: Win32 {status}"
            )));
        }
        return Ok(());
    }
    let mut descriptor: SECURITY_DESCRIPTOR = unsafe { zeroed() };
    let raw = (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast();
    if unsafe { InitializeSecurityDescriptor(raw, 1) } == 0
        || unsafe { SetSecurityDescriptorDacl(raw, 1, acl, 0) } == 0
        || unsafe {
            windows_sys::Win32::Security::SetSecurityDescriptorControl(
                raw,
                windows_sys::Win32::Security::SE_DACL_PROTECTED,
                windows_sys::Win32::Security::SE_DACL_PROTECTED,
            )
        } == 0
    {
        return Err(win32("build protected directory handoff DACL"));
    }
    // SetSecurityInfo propagates inheritable ACEs through existing descendants.
    // This legacy file API sets only this pinned, ancestor-pinned directory; its
    // inheritable ACEs apply to future children, not unchecked existing objects.
    if unsafe {
        windows_sys::Win32::Security::SetFileSecurityW(
            to_wide(path.as_str()).as_ptr(),
            DACL_SECURITY_INFORMATION,
            raw,
        )
    } == 0
    {
        return Err(win32("apply pinned directory handoff DACL"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
