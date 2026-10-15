//! Owns the elevated, per-application service and its protected executable copy.

#[cfg(not(windows))]
pub fn run_bootstrap(
    _session: &str,
    _parent_pid: u32,
    _parent_created: u64,
    _expected_user: &str,
) -> Result<(), String> {
    Err("the sandbox bootstrap is supported only on Windows".into())
}

#[cfg(windows)]
pub use windows::run_bootstrap;

#[cfg(windows)]
mod windows {
    use super::super::token;
    use sha2::{Digest, Sha256};
    use std::ffi::c_void;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::path::{Path, PathBuf};
    use std::ptr::{null, null_mut};
    use std::time::{Duration, Instant};

    type Handle = *mut c_void;
    const INVALID_HANDLE: Handle = -1isize as Handle;
    const QUERY_PROCESS: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x100000;
    const WAIT_TIMEOUT: u32 = 258;
    const STOPPED: u32 = 1;
    const RUNNING: u32 = 4;
    const WRITE_MASK: u32 = 0x000d_0156;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const GENERIC_ALL: u32 = 0x1000_0000;
    const SY: &str = "S-1-5-18";
    const BA: &str = "S-1-5-32-544";

    #[repr(C)]
    struct Guid {
        a: u32,
        b: u16,
        c: u16,
        d: [u8; 8],
    }
    #[repr(C)]
    struct Security {
        size: u32,
        descriptor: Handle,
        inherit: i32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    #[repr(C)]
    struct AttributeTag {
        attributes: u32,
        tag: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ServiceStatus {
        kind: u32,
        state: u32,
        controls: u32,
        win32_exit: u32,
        specific_exit: u32,
        checkpoint: u32,
        wait_hint: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ServiceProcessStatus {
        basic: ServiceStatus,
        pid: u32,
        flags: u32,
    }
    #[repr(C)]
    struct Acl {
        revision: u8,
        reserved: u8,
        size: u16,
        count: u16,
        reserved2: u16,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> Handle;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetProcessTimes(
            process: Handle,
            created: *mut FileTime,
            exited: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn WaitForSingleObject(handle: Handle, millis: u32) -> u32;
        fn TerminateProcess(process: Handle, code: u32) -> i32;
        fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
        fn QueryFullProcessImageNameW(
            process: Handle,
            flags: u32,
            buffer: *mut u16,
            length: *mut u32,
        ) -> i32;
        fn CreateFileW(
            path: *const u16,
            access: u32,
            share: u32,
            security: *const Security,
            disposition: u32,
            flags: u32,
            template: Handle,
        ) -> Handle;
        fn CreateDirectoryW(path: *const u16, security: *const Security) -> i32;
        fn GetFileInformationByHandleEx(
            handle: Handle,
            kind: i32,
            info: *mut c_void,
            size: u32,
        ) -> i32;
        fn GetFinalPathNameByHandleW(
            handle: Handle,
            buffer: *mut u16,
            size: u32,
            flags: u32,
        ) -> u32;
        fn LocalFree(memory: Handle) -> Handle;
    }
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            text: *const u16,
            revision: u32,
            descriptor: *mut Handle,
            size: *mut u32,
        ) -> i32;
        fn ConvertSidToStringSidW(sid: Handle, text: *mut *mut u16) -> i32;
        fn GetSecurityInfo(
            object: Handle,
            kind: u32,
            info: u32,
            owner: *mut Handle,
            group: *mut Handle,
            dacl: *mut *mut Acl,
            sacl: *mut Handle,
            descriptor: *mut Handle,
        ) -> u32;
        fn GetAce(acl: *const Acl, index: u32, ace: *mut Handle) -> i32;
        fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> Handle;
        fn CreateServiceW(
            manager: Handle,
            name: *const u16,
            display: *const u16,
            access: u32,
            kind: u32,
            start: u32,
            errors: u32,
            binary: *const u16,
            group: *const u16,
            tag: *mut u32,
            dependencies: *const u16,
            account: *const u16,
            password: *const u16,
        ) -> Handle;
        fn SetServiceObjectSecurity(service: Handle, info: u32, descriptor: Handle) -> i32;
        fn StartServiceW(service: Handle, count: u32, arguments: *const *const u16) -> i32;
        fn ControlService(service: Handle, control: u32, status: *mut ServiceStatus) -> i32;
        fn QueryServiceStatusEx(
            service: Handle,
            kind: i32,
            buffer: *mut u8,
            size: u32,
            needed: *mut u32,
        ) -> i32;
        fn DeleteService(service: Handle) -> i32;
        fn CloseServiceHandle(service: Handle) -> i32;
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHGetKnownFolderPath(
            folder: *const Guid,
            flags: u32,
            token: Handle,
            path: *mut *mut u16,
        ) -> i32;
    }
    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(memory: Handle);
    }

    struct OwnedHandle(Handle);
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    struct ServiceHandle(Handle);
    impl Drop for ServiceHandle {
        fn drop(&mut self) {
            unsafe {
                CloseServiceHandle(self.0);
            }
        }
    }
    struct Descriptor(Handle);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0);
            }
        }
    }

    fn last_error(stage: &str) -> String {
        format!("{stage}: {}", std::io::Error::last_os_error())
    }
    fn check(ok: i32, stage: &str) -> Result<(), String> {
        if ok == 0 {
            Err(last_error(stage))
        } else {
            Ok(())
        }
    }
    fn wide(value: &std::ffi::OsStr) -> Result<Vec<u16>, String> {
        let mut value: Vec<_> = value.encode_wide().collect();
        if value.contains(&0) {
            return Err("a Windows argument contains NUL".into());
        }
        value.push(0);
        Ok(value)
    }
    fn text(value: &str) -> Result<Vec<u16>, String> {
        wide(std::ffi::OsStr::new(value))
    }
    fn descriptor(value: &str) -> Result<Descriptor, String> {
        let value = text(value)?;
        let mut result = null_mut();
        check(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    value.as_ptr(),
                    1,
                    &mut result,
                    null_mut(),
                )
            },
            "parse bootstrap security descriptor",
        )?;
        Ok(Descriptor(result))
    }
    fn token_of(process: Handle) -> Result<OwnedHandle, String> {
        let mut result = null_mut();
        check(
            unsafe { OpenProcessToken(process, 8, &mut result) },
            "open process token",
        )?;
        Ok(OwnedHandle(result))
    }
    fn created_at(process: Handle) -> Result<u64, String> {
        let (mut created, mut exited, mut kernel, mut user) = (
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
        );
        check(
            unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) },
            "query process creation time",
        )?;
        Ok((u64::from(created.high) << 32) | u64::from(created.low))
    }
    fn parent_handle(pid: u32, created: u64, expected_user: &str) -> Result<OwnedHandle, String> {
        let process = unsafe { OpenProcess(QUERY_PROCESS | SYNCHRONIZE, 0, pid) };
        if process.is_null() {
            return Err(last_error("open original application"));
        }
        let process = OwnedHandle(process);
        if created_at(process.0)? != created
            || unsafe { WaitForSingleObject(process.0, 0) } != WAIT_TIMEOUT
        {
            return Err("the original application identity expired".into());
        }
        let token_handle = token_of(process.0)?;
        let identity = token::caller_identity(token_handle.0)?;
        if identity.user_sid != expected_user
            || token::integrity_sid(token_handle.0)? != "S-1-16-8192"
            || token::is_elevated(token_handle.0)?
            || token::is_restricted(token_handle.0)?
            || token::is_app_container(token_handle.0)?
        {
            return Err(
                "the original application must be the pinned ordinary medium-integrity user".into(),
            );
        }
        Ok(process)
    }
    fn final_path(handle: Handle) -> Result<PathBuf, String> {
        let mut buffer = vec![0u16; 32768];
        let count = unsafe {
            GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, 0)
        };
        if count == 0 || count as usize >= buffer.len() {
            return Err(last_error("resolve pinned bootstrap path"));
        }
        Ok(PathBuf::from(
            String::from_utf16(&buffer[..count as usize]).map_err(|e| e.to_string())?,
        ))
    }
    fn normalized(path: &Path) -> String {
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('/', "\\")
            .to_lowercase()
    }
    fn open_path(path: &Path, directory: bool) -> Result<OwnedHandle, String> {
        let value = wide(path.as_os_str())?;
        let flags = 0x0020_0000 | if directory { 0x0200_0000 } else { 0 };
        let handle = unsafe {
            CreateFileW(
                value.as_ptr(),
                0x8000_0000,
                1 | if directory { 2 } else { 0 },
                null(),
                3,
                flags,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE {
            return Err(last_error("pin bootstrap path"));
        }
        let handle = OwnedHandle(handle);
        let mut attributes = AttributeTag {
            attributes: 0,
            tag: 0,
        };
        check(
            unsafe {
                GetFileInformationByHandleEx(
                    handle.0,
                    9,
                    (&mut attributes as *mut AttributeTag).cast(),
                    std::mem::size_of::<AttributeTag>() as u32,
                )
            },
            "inspect bootstrap path",
        )?;
        if attributes.attributes & 0x400 != 0
            || (attributes.attributes & 0x10 != 0) != directory
            || normalized(&final_path(handle.0)?) != normalized(path)
        {
            return Err("bootstrap path changed identity or is a reparse point".into());
        }
        Ok(handle)
    }
    fn sid_text(sid: Handle) -> Result<String, String> {
        let mut value = null_mut();
        check(
            unsafe { ConvertSidToStringSidW(sid, &mut value) },
            "format staging owner SID",
        )?;
        let allocation = Descriptor(value.cast());
        let mut length = 0;
        while unsafe { *value.add(length) } != 0 {
            length += 1;
        }
        let result = String::from_utf16(unsafe { std::slice::from_raw_parts(value, length) })
            .map_err(|e| e.to_string());
        drop(allocation);
        result
    }
    fn verify_protected_directory(handle: Handle) -> Result<(), String> {
        let mut owner = null_mut();
        let mut acl = null_mut();
        let mut sd = null_mut();
        let status = unsafe {
            GetSecurityInfo(
                handle,
                1,
                5,
                &mut owner,
                null_mut(),
                &mut acl,
                null_mut(),
                &mut sd,
            )
        };
        if status != 0 {
            return Err(format!("read staging security: Windows error {status}"));
        }
        let _allocation = Descriptor(sd);
        if acl.is_null() || !matches!(sid_text(owner)?.as_str(), SY | BA) {
            return Err(
                "staging must be owned by SYSTEM/Administrators and have an explicit DACL".into(),
            );
        }
        for index in 0..u32::from(unsafe { (*acl).count }) {
            let mut ace = null_mut();
            check(
                unsafe { GetAce(acl, index, &mut ace) },
                "inspect staging ACL",
            )?;
            let bytes = ace.cast::<u8>();
            let kind = unsafe { *bytes };
            if kind == 1 {
                continue;
            }
            if kind != 0 {
                return Err("unsupported ACE in bootstrap staging DACL".into());
            }
            let mask = unsafe { std::ptr::read_unaligned(bytes.add(4).cast::<u32>()) };
            let sid = sid_text(unsafe { bytes.add(8) }.cast())?;
            if mask & (WRITE_MASK | GENERIC_WRITE | GENERIC_ALL) != 0 && sid != SY && sid != BA {
                return Err("bootstrap staging permits non-administrator modification".into());
            }
        }
        Ok(())
    }
    fn program_files() -> Result<PathBuf, String> {
        let id = Guid {
            a: 0x905e63b6,
            b: 0xc1bf,
            c: 0x494e,
            d: [0xb2, 0x9c, 0x65, 0xb7, 0x32, 0xd3, 0xd2, 0x1a],
        };
        let mut value = null_mut();
        let status = unsafe { SHGetKnownFolderPath(&id, 0, null_mut(), &mut value) };
        if status < 0 {
            return Err(format!("resolve ProgramFiles: HRESULT {status:#x}"));
        }
        let mut length = 0;
        while unsafe { *value.add(length) } != 0 {
            length += 1;
        }
        let result = String::from_utf16(unsafe { std::slice::from_raw_parts(value, length) })
            .map(PathBuf::from)
            .map_err(|e| e.to_string());
        unsafe {
            CoTaskMemFree(value.cast());
        }
        let result = result?;
        let value: Vec<_> = result.as_os_str().encode_wide().collect();
        if value.len() < 3 || value[1] != b':' as u16 || value[2] != b'\\' as u16 {
            return Err("the OS ProgramFiles location must be a local absolute drive path".into());
        }
        Ok(result)
    }
    struct Staging {
        instance: PathBuf,
        executable: PathBuf,
        files: Vec<PathBuf>,
        pins: Vec<OwnedHandle>,
        images: Vec<OwnedHandle>,
    }
    fn pinned_file(path: &Path) -> Result<File, String> {
        let source = open_path(path, false)?;
        let handle = source.0;
        std::mem::forget(source);
        Ok(unsafe { File::from_raw_handle(handle) })
    }
    fn copy_verified(
        source: &mut File,
        destination: &Path,
        security: &Security,
    ) -> Result<OwnedHandle, String> {
        let name = wide(destination.as_os_str())?;
        let handle =
            unsafe { CreateFileW(name.as_ptr(), 0x4000_0000, 0, security, 1, 0x80, null_mut()) };
        if handle == INVALID_HANDLE {
            return Err(last_error("create protected helper component"));
        }
        let mut target = unsafe { File::from_raw_handle(handle) };
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            let count = source.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            target
                .write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
        }
        target.sync_all().map_err(|e| e.to_string())?;
        drop(target);
        let image = open_path(destination, false)?;
        let mut verify = File::open(destination).map_err(|e| e.to_string())?;
        let mut copied = Sha256::new();
        loop {
            let count = verify.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            copied.update(&buffer[..count]);
        }
        if hash.finalize() != copied.finalize() {
            return Err("the protected helper component hash differs".into());
        }
        Ok(image)
    }
    fn stage(session: &str, expected_user: &str) -> Result<Staging, String> {
        let source_path = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut sources = vec![("moyai-sandbox.exe", pinned_file(&source_path)?)];
        // The offline release contract bundles these three app-local VC runtime files.
        const VC_FILES: [&str; 3] = ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"];
        let source_directory = source_path
            .parent()
            .ok_or("helper source directory is missing")?;
        let mut present = Vec::new();
        for name in VC_FILES {
            present.push(
                source_directory
                    .join(name)
                    .try_exists()
                    .map_err(|e| e.to_string())?,
            );
        }
        if present.iter().any(|found| *found) {
            if !present.iter().all(|found| *found) {
                return Err("the bundled VC runtime set is incomplete".into());
            }
            for name in VC_FILES {
                sources.push((name, pinned_file(&source_directory.join(name))?));
            }
        }
        let sd = descriptor(&format!(
            "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;GRGX;;;{expected_user})"
        ))?;
        let security = Security {
            size: std::mem::size_of::<Security>() as u32,
            descriptor: sd.0,
            inherit: 0,
        };
        let base = program_files()?;
        let mut pins = vec![open_path(&base, true)?];
        let mut path = base;
        for component in ["moyAI", "Sandbox"] {
            path.push(component);
            let name = wide(path.as_os_str())?;
            if unsafe { CreateDirectoryW(name.as_ptr(), &security) } == 0
                && std::io::Error::last_os_error().raw_os_error() != Some(183)
            {
                return Err(last_error("create protected bootstrap base"));
            }
            let pin = open_path(&path, true)?;
            verify_protected_directory(pin.0)?;
            pins.push(pin);
        }
        let instance = path.join(session);
        let name = wide(instance.as_os_str())?;
        check(
            unsafe { CreateDirectoryW(name.as_ptr(), &security) },
            "create new bootstrap instance",
        )?;
        let files: Vec<_> = sources
            .iter()
            .map(|(name, _)| instance.join(name))
            .collect();
        let result = (|| {
            let pin = open_path(&instance, true)?;
            verify_protected_directory(pin.0)?;
            pins.push(pin);
            let executable = instance.join("moyai-sandbox.exe");
            let mut images = Vec::new();
            for ((_, source), destination) in sources.iter_mut().zip(&files) {
                images.push(copy_verified(source, destination, &security)?);
            }
            Ok(Staging {
                instance: instance.clone(),
                executable,
                files: files.clone(),
                pins,
                images,
            })
        })();
        if let Err(error) = result {
            // No service exists yet; only this newly-created instance can be removed.
            let mut failures = vec![error];
            for file in &files {
                if let Err(error) = std::fs::remove_file(file) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        failures.push(format!("remove incomplete helper component: {error}"));
                    }
                }
            }
            if let Err(error) = std::fs::remove_dir(&instance) {
                failures.push(format!("remove incomplete staging directory: {error}"));
            }
            return Err(failures.join("; "));
        }
        result
    }
    fn service_status(service: Handle) -> Result<ServiceProcessStatus, String> {
        let mut status = ServiceProcessStatus::default();
        let mut needed = 0;
        check(
            unsafe {
                QueryServiceStatusEx(
                    service,
                    0,
                    (&mut status as *mut ServiceProcessStatus).cast(),
                    std::mem::size_of::<ServiceProcessStatus>() as u32,
                    &mut needed,
                )
            },
            "query owned sandbox service",
        )?;
        Ok(status)
    }
    fn service_process(pid: u32, executable: &Path) -> Result<OwnedHandle, String> {
        let process = unsafe { OpenProcess(QUERY_PROCESS | SYNCHRONIZE | 1, 0, pid) };
        if process.is_null() {
            return Err(last_error("pin owned service process"));
        }
        let process = OwnedHandle(process);
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        check(
            unsafe { QueryFullProcessImageNameW(process.0, 0, path.as_mut_ptr(), &mut length) },
            "query service executable",
        )?;
        let image =
            PathBuf::from(String::from_utf16(&path[..length as usize]).map_err(|e| e.to_string())?);
        if normalized(&image) != normalized(executable)
            || token::user_sid(token_of(process.0)?.0)? != SY
        {
            return Err("service PID is not the protected instance's SYSTEM process".into());
        }
        created_at(process.0)?;
        Ok(process)
    }
    fn settle_service(
        service: Handle,
        process: &mut Option<OwnedHandle>,
        executable: &Path,
        attempted: bool,
    ) -> Result<(), String> {
        if !attempted {
            return Ok(());
        }
        let started = Instant::now();
        let mut stop_sent = false;
        while started.elapsed() < Duration::from_secs(30) {
            if let Some(process) = process.as_ref() {
                if unsafe { WaitForSingleObject(process.0, 0) } == 0 {
                    return Ok(());
                }
            }
            let status = service_status(service)?;
            if process.is_none() && status.pid != 0 {
                *process = Some(service_process(status.pid, executable)?);
            }
            if let Some(process) = process.as_ref() {
                if unsafe { WaitForSingleObject(process.0, 0) } == 0 {
                    return Ok(());
                }
            }
            if status.basic.state != STOPPED && !stop_sent {
                let mut output = ServiceStatus::default();
                stop_sent = unsafe { ControlService(service, 1, &mut output) } != 0;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let Some(process) = process.as_ref() else {
            return Err(
                "service exit could not be confirmed; protected staging is retained".into(),
            );
        };
        check(
            unsafe { TerminateProcess(process.0, 1) },
            "terminate validated service after cleanup grace",
        )?;
        if unsafe { WaitForSingleObject(process.0, 5000) } != 0 {
            return Err(
                "service remained alive after cleanup grace; protected staging is retained".into(),
            );
        }
        Err("service required forced termination; workspace handback may be incomplete".into())
    }

    pub fn run_bootstrap(
        session: &str,
        parent_pid: u32,
        parent_created: u64,
        expected_user: &str,
    ) -> Result<(), String> {
        const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
        if session.len() != 26
            || !session.bytes().all(|b| ALPHABET.contains(&b))
            || session.as_bytes()[0] > b'7'
            || parent_pid == 0
            || parent_created == 0
            || !expected_user.starts_with("S-1-")
            || !expected_user
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'S' || b == b'-')
        {
            return Err("invalid fixed bootstrap identity arguments".into());
        }
        let own_token = token_of(unsafe { GetCurrentProcess() })?;
        if !token::is_elevated(own_token.0)?
            || token::is_restricted(own_token.0)?
            || token::is_app_container(own_token.0)?
        {
            return Err("bootstrap requires the explicitly elevated ordinary helper".into());
        }
        let parent = parent_handle(parent_pid, parent_created, expected_user)?;
        let mut staging = stage(session, expected_user)?;
        let mut service = None;
        let mut process = None;
        let mut attempted = false;
        let outcome = (|| {
            let manager = unsafe { OpenSCManagerW(null(), null(), 3) };
            if manager.is_null() {
                return Err(last_error("open local service manager"));
            }
            let manager = ServiceHandle(manager);
            let name = text(&format!("moyAI-Sandbox-{session}"))?;
            let command = text(&format!(
                "\"{}\" --service --session {session} --parent-pid {parent_pid} --parent-created {parent_created} --user {expected_user}",
                staging.executable.display()
            ))?;
            let handle = unsafe {
                CreateServiceW(
                    manager.0,
                    name.as_ptr(),
                    name.as_ptr(),
                    0xf01ff,
                    0x10,
                    3,
                    1,
                    command.as_ptr(),
                    null(),
                    null_mut(),
                    null(),
                    null(),
                    null(),
                )
            };
            if handle.is_null() {
                return Err(last_error("create new demand-start sandbox service"));
            }
            service = Some(ServiceHandle(handle));
            let sd = descriptor("D:P(A;;GA;;;SY)(A;;GA;;;BA)")?;
            check(
                unsafe { SetServiceObjectSecurity(handle, 4, sd.0) },
                "protect service controls",
            )?;
            if unsafe { WaitForSingleObject(parent.0, 0) } != WAIT_TIMEOUT {
                return Err("application exited before service start".into());
            }
            attempted = true;
            check(
                unsafe { StartServiceW(handle, 0, null()) },
                "start owned sandbox service",
            )?;
            let started = Instant::now();
            let mut running = false;
            while started.elapsed() < Duration::from_secs(30) {
                let status = service_status(handle)?;
                if process.is_none() && status.pid != 0 {
                    process = Some(service_process(status.pid, &staging.executable)?);
                }
                if status.basic.state == STOPPED {
                    return Err("sandbox service stopped during startup".into());
                }
                if unsafe { WaitForSingleObject(parent.0, 0) } != WAIT_TIMEOUT {
                    return Err("application exited during service startup".into());
                }
                if status.basic.state == RUNNING && process.is_some() {
                    running = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let owned = process
                .as_ref()
                .ok_or("sandbox service PID was not established")?;
            if !running {
                return Err("sandbox service did not reach running state".into());
            }
            // Marking a started service for deletion prevents its registration surviving a crash.
            check(
                unsafe { DeleteService(handle) },
                "mark transient service for deletion",
            )?;
            // The service owns explicit Desktop/Runner leases; the original PID may exit first.
            if unsafe { WaitForSingleObject(owned.0, u32::MAX) } != 0 {
                return Err(last_error("wait for sandbox app-family lifetime"));
            }
            let mut code = 0;
            check(
                unsafe { GetExitCodeProcess(owned.0, &mut code) },
                "query sandbox service exit",
            )?;
            if code != 0 {
                Err(format!("sandbox service exited with code {code}"))
            } else {
                Ok(())
            }
        })();
        let mut failures = Vec::new();
        if let Err(error) = outcome {
            failures.push(error);
        }
        let mut exited = !attempted;
        if let Some(service) = service.as_ref() {
            if let Err(error) =
                settle_service(service.0, &mut process, &staging.executable, attempted)
            {
                failures.push(error);
            }
            if let Some(process) = process.as_ref() {
                exited = unsafe { WaitForSingleObject(process.0, 0) } == 0;
            }
            if unsafe { DeleteService(service.0) } == 0
                && std::io::Error::last_os_error().raw_os_error() != Some(1072)
            {
                failures.push(last_error("delete owned transient service"));
            }
        }
        drop(process);
        drop(service);
        if exited {
            staging.images.clear();
            for file in &staging.files {
                if let Err(error) = std::fs::remove_file(file) {
                    failures.push(format!("remove protected helper component: {error}"));
                }
            }
            staging.pins.clear();
            if let Err(error) = std::fs::remove_dir(&staging.instance) {
                failures.push(format!("remove owned staging directory: {error}"));
            }
        } else {
            failures.push(
                "service exit is unconfirmed; the protected instance directory was not removed"
                    .into(),
            );
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}
