//! Validates and starts the administrator-installed, demand-start helper.
//! Normal application startup never installs, copies, or elevates a program.

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use super::client::{pin_bundled_runtime, pin_helper, pin_source_ancestors};
use super::native;

type RawHandle = *mut c_void;
const STOPPED: u32 = 1;
const START_PENDING: u32 = 2;
const STOP_PENDING: u32 = 3;
const RUNNING: u32 = 4;
const SY: &str = "S-1-5-18";
const BA: &str = "S-1-5-32-544";
const WRITE_MASK: u32 = 0x000d_0156 | 0x4000_0000 | 0x1000_0000;

#[repr(C)]
struct Guid {
    a: u32,
    b: u16,
    c: u16,
    d: [u8; 8],
}
#[repr(C)]
struct Config {
    kind: u32,
    start: u32,
    errors: u32,
    binary: *mut u16,
    group: *mut u16,
    tag: u32,
    dependencies: *mut u16,
    account: *mut u16,
    display: *mut u16,
}
#[repr(C)]
#[derive(Default)]
struct Status {
    kind: u32,
    state: u32,
    controls: u32,
    win32_exit: u32,
    specific_exit: u32,
    checkpoint: u32,
    wait_hint: u32,
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

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> RawHandle;
    fn OpenServiceW(manager: RawHandle, name: *const u16, access: u32) -> RawHandle;
    fn QueryServiceConfigW(
        service: RawHandle,
        config: *mut Config,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn QueryServiceStatusEx(
        service: RawHandle,
        kind: i32,
        buffer: *mut u8,
        size: u32,
        needed: *mut u32,
    ) -> i32;
    fn StartServiceW(service: RawHandle, count: u32, arguments: *const *const u16) -> i32;
    fn CloseServiceHandle(service: RawHandle) -> i32;
    fn GetSecurityInfo(
        object: RawHandle,
        kind: u32,
        info: u32,
        owner: *mut RawHandle,
        group: *mut RawHandle,
        dacl: *mut *mut Acl,
        sacl: *mut RawHandle,
        descriptor: *mut RawHandle,
    ) -> u32;
    fn GetAce(acl: *const Acl, index: u32, ace: *mut RawHandle) -> i32;
    fn ConvertSidToStringSidW(sid: RawHandle, text: *mut *mut u16) -> i32;
}
#[link(name = "shell32")]
unsafe extern "system" {
    fn SHGetKnownFolderPath(
        folder: *const Guid,
        flags: u32,
        token: RawHandle,
        path: *mut *mut u16,
    ) -> i32;
}
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemFree(memory: RawHandle);
}
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LocalFree(memory: RawHandle) -> RawHandle;
}

struct ServiceHandle(RawHandle);
// The SCM handle is immutable and Windows permits using it from another thread.
unsafe impl Send for ServiceHandle {}
unsafe impl Sync for ServiceHandle {}
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}
struct Allocation(RawHandle);
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn setup_error(detail: impl std::fmt::Display) -> String {
    format!(
        "Windows実行補助サービスを確認できません。Setup-moyAI.ps1で導入・更新してください。通常起動で管理者承認や制限外実行へ切り替えることはありません。{detail}"
    )
}

fn wide(value: &std::ffi::OsStr) -> Result<Vec<u16>, String> {
    let mut result: Vec<_> = value.encode_wide().collect();
    if result.contains(&0) {
        return Err("Windows service path contains NUL".into());
    }
    result.push(0);
    Ok(result)
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
    if status < 0 || value.is_null() {
        return Err(format!("resolve OS ProgramFiles: HRESULT {status:#x}"));
    }
    let mut length = 0;
    while unsafe { *value.add(length) } != 0 {
        length += 1;
    }
    let result = String::from_utf16(unsafe { std::slice::from_raw_parts(value, length) })
        .map(PathBuf::from)
        .map_err(|error| error.to_string());
    unsafe {
        CoTaskMemFree(value.cast());
    }
    let result = result?;
    let units: Vec<_> = result.as_os_str().encode_wide().collect();
    if units.len() < 3
        || !matches!(units[0], 65..=90 | 97..=122)
        || units[1] != u16::from(b':')
        || units[2] != u16::from(b'\\')
    {
        return Err("OS ProgramFiles must be a local absolute drive path".into());
    }
    Ok(result)
}

fn sid_text(sid: RawHandle) -> Result<String, String> {
    if sid.is_null() {
        return Err("installed helper owner SID is missing".into());
    }
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(native::last("format installed helper SID"));
    }
    let _allocation = Allocation(text.cast());
    let mut length = 0;
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) })
        .map_err(|error| error.to_string())
}

fn permits_untrusted_write(mask: u32, sid: &str) -> bool {
    mask & WRITE_MASK != 0 && sid != SY && sid != BA
}

fn verify_protected(file: &File) -> Result<(), String> {
    let (mut owner, mut acl, mut descriptor) = (null_mut(), null_mut(), null_mut());
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle().cast(),
            1,
            5,
            &mut owner,
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(format!(
            "read installed helper security: Windows error {status}"
        ));
    }
    let _allocation = Allocation(descriptor);
    if acl.is_null() || !matches!(sid_text(owner)?.as_str(), SY | BA) {
        return Err("installed helper must be owned by SYSTEM/Administrators with a DACL".into());
    }
    for index in 0..u32::from(unsafe { (*acl).count }) {
        let mut ace = null_mut();
        if unsafe { GetAce(acl, index, &mut ace) } == 0 {
            return Err(native::last("inspect installed helper ACL"));
        }
        let bytes = ace.cast::<u8>();
        let kind = unsafe { *bytes };
        if kind == 1 {
            continue;
        }
        if kind != 0 {
            return Err("unsupported ACE in installed helper DACL".into());
        }
        let mask = unsafe { std::ptr::read_unaligned(bytes.add(4).cast::<u32>()) };
        let sid = sid_text(unsafe { bytes.add(8) }.cast())?;
        if permits_untrusted_write(mask, &sid) {
            return Err("installed helper permits non-administrator modification".into());
        }
    }
    Ok(())
}

fn config_string(buffer: &[usize], pointer: *const u16) -> Result<String, String> {
    let start = buffer.as_ptr() as usize;
    let end = start + std::mem::size_of_val(buffer);
    let address = pointer as usize;
    if address < start || address >= end || address % 2 != 0 {
        return Err("invalid service configuration string".into());
    }
    let units = unsafe { std::slice::from_raw_parts(pointer, (end - address) / 2) };
    let length = units
        .iter()
        .position(|value| *value == 0)
        .ok_or("unterminated service configuration string")?;
    String::from_utf16(&units[..length]).map_err(|error| error.to_string())
}

fn config_matches(kind: u32, start: u32, binary: &str, account: &str, image: &Path) -> bool {
    kind == 0x10
        && start == 3
        && account.eq_ignore_ascii_case("LocalSystem")
        && binary.eq_ignore_ascii_case(&format!("\"{}\" --service", image.display()))
}

pub(super) struct InstalledService {
    service: ServiceHandle,
    image: PathBuf,
    _ancestors: Vec<File>,
    _directories: Vec<File>,
    _image: File,
    _runtime: Vec<File>,
}

impl InstalledService {
    pub(super) fn open(expected_hash: &[u8; 32]) -> Result<Self, String> {
        Self::open_checked(expected_hash).map_err(setup_error)
    }

    fn open_checked(expected_hash: &[u8; 32]) -> Result<Self, String> {
        let base = program_files()?.join("moyAI");
        let directory = base.join("SandboxService");
        let image = directory.join("moyai-sandbox.exe");
        let ancestors = pin_source_ancestors(&image)?;
        let mut directories = Vec::new();
        for path in [&base, &directory] {
            let file = OpenOptions::new()
                .read(true)
                .share_mode(3)
                .custom_flags(0x0220_0000)
                .open(path)
                .map_err(|error| error.to_string())?;
            verify_protected(&file)?;
            directories.push(file);
        }
        let (pin, hash) = pin_helper(&image)?;
        verify_protected(&pin)?;
        if &hash != expected_hash {
            return Err("installed helper differs from this moyAI version".into());
        }
        let runtime = pin_bundled_runtime(&image)?;
        for file in &runtime {
            verify_protected(file)?;
        }
        let manager = unsafe { OpenSCManagerW(null(), null(), 1) };
        if manager.is_null() {
            return Err(native::last("open local service manager"));
        }
        let manager = ServiceHandle(manager);
        let name = wide(std::ffi::OsStr::new(super::SERVICE_NAME))?;
        let raw = unsafe { OpenServiceW(manager.0, name.as_ptr(), 0x15) };
        if raw.is_null() {
            return Err(native::last("open installed sandbox service"));
        }
        let result = Self {
            service: ServiceHandle(raw),
            image,
            _ancestors: ancestors,
            _directories: directories,
            _image: pin,
            _runtime: runtime,
        };
        result.verify_config()?;
        Ok(result)
    }

    fn verify_config(&self) -> Result<(), String> {
        let mut needed = 0;
        unsafe {
            QueryServiceConfigW(self.service.0, null_mut(), 0, &mut needed);
        }
        if needed < std::mem::size_of::<Config>() as u32 || needed > 1024 * 1024 {
            return Err(native::last("size installed service configuration"));
        }
        let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        if unsafe {
            QueryServiceConfigW(
                self.service.0,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(native::last("query installed service configuration"));
        }
        let config = unsafe { &*buffer.as_ptr().cast::<Config>() };
        let binary = config_string(&buffer, config.binary)?;
        let account = config_string(&buffer, config.account)?;
        if !config_matches(config.kind, config.start, &binary, &account, &self.image) {
            return Err(
                "installed service has an unexpected image, account, type, or start mode".into(),
            );
        }
        Ok(())
    }

    fn status(&self) -> Result<Status, String> {
        self.verify_config()?;
        let mut status = Status::default();
        let mut needed = 0;
        if unsafe {
            QueryServiceStatusEx(
                self.service.0,
                0,
                (&mut status as *mut Status).cast(),
                std::mem::size_of::<Status>() as u32,
                &mut needed,
            )
        } == 0
        {
            return Err(native::last("query installed sandbox service state"));
        }
        if status.kind != 0x10 {
            return Err("installed service changed process type".into());
        }
        Ok(status)
    }

    /// Used only before a new application family has performed any effect.
    pub(super) fn start_if_stopped(&self) -> Result<Option<u32>, String> {
        let status = self.status()?;
        match status.state {
            STOPPED => {
                if unsafe { StartServiceW(self.service.0, 0, null()) } == 0
                    && std::io::Error::last_os_error().raw_os_error() != Some(1056)
                {
                    return Err(native::last("start installed sandbox service"));
                }
                Ok(None)
            }
            START_PENDING | STOP_PENDING => Ok(None),
            RUNNING if status.pid != 0 => Ok(Some(status.pid)),
            _ => Err(format!(
                "sandbox service is in an unexpected state ({})",
                status.state
            )),
        }
    }

    pub(super) fn running_pid(&self) -> Result<u32, String> {
        let status = self.status()?;
        if status.state != RUNNING || status.pid == 0 {
            return Err("sandbox service is no longer running; no command was replayed".into());
        }
        Ok(status.pid)
    }

    pub(super) fn is_image(&self, path: &Path) -> bool {
        normalize(path) == normalize(&self.image)
    }
}

fn normalize(path: &Path) -> String {
    path.to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('/', "\\")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fixed_demand_system_own_process_configuration_is_accepted() {
        let image = Path::new(r"C:\Program Files\moyAI\SandboxService\moyai-sandbox.exe");
        let command = format!("\"{}\" --service", image.display());
        assert!(config_matches(0x10, 3, &command, "LocalSystem", image));
        for (kind, start, binary, account) in [
            (0x20, 3, command.clone(), "LocalSystem"),
            (0x10, 2, command.clone(), "LocalSystem"),
            (0x10, 3, command.clone(), "LocalService"),
            (0x10, 3, format!("{command} --session 1"), "LocalSystem"),
            (
                0x10,
                3,
                command.replace("Program Files", "Users"),
                "LocalSystem",
            ),
            (
                0x10,
                3,
                format!("{} --service", image.display()),
                "LocalSystem",
            ),
        ] {
            assert!(!config_matches(kind, start, &binary, account, image));
        }
    }

    #[test]
    fn installed_payload_rejects_every_untrusted_write_right() {
        for mask in [
            2, 4, 0x10, 0x40, 0x100, 0x10000, 0x40000, 0x80000, 0x40000000, 0x10000000,
        ] {
            assert!(permits_untrusted_write(mask, "S-1-5-11"));
            assert!(!permits_untrusted_write(mask, SY));
            assert!(!permits_untrusted_write(mask, BA));
        }
        assert!(!permits_untrusted_write(0x0012_00a9, "S-1-5-11"));
    }

    #[test]
    fn service_configuration_strings_are_bounded_by_the_returned_buffer() {
        let mut buffer = vec![0usize; 16];
        let pointer = buffer.as_mut_ptr().cast::<u16>();
        unsafe {
            *pointer = b'A' as u16;
        }
        assert_eq!(config_string(&buffer, pointer).unwrap(), "A");
        assert!(config_string(&buffer, null()).is_err());
        assert!(config_string(&buffer, pointer.wrapping_add(128)).is_err());
        buffer.fill(usize::MAX);
        assert!(config_string(&buffer, pointer).is_err());
    }
}
