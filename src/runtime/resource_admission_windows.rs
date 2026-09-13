//! Machine policy is read by every Windows account. Only its installing account and OS
//! administrators can change it. There is no environment-variable location override.
use super::resource_admission::error;
use crate::runner::RunnerError;
use camino::Utf8PathBuf;
use std::{
    ffi::c_void,
    fs::OpenOptions,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    ptr::null_mut,
};
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::Com::CoTaskMemFree,
    UI::Shell::*,
};

struct Allocation(*mut c_void);
impl Drop for Allocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
unsafe fn sid_text(sid: PSID) -> Result<String, RunnerError> {
    let mut text = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(error(std::io::Error::last_os_error()));
    }
    let _allocation = Allocation(text.cast());
    let mut length = 0;
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    String::from_utf16(unsafe { std::slice::from_raw_parts(text, length) }).map_err(error)
}
pub(super) fn directory() -> Result<Utf8PathBuf, RunnerError> {
    let mut value = null_mut();
    let status = unsafe { SHGetKnownFolderPath(&FOLDERID_ProgramData, 0, null_mut(), &mut value) };
    if status < 0 {
        return Err(error(format!(
            "Windows ProgramData discovery failed ({status})"
        )));
    }
    let mut length = 0;
    while unsafe { *value.add(length) } != 0 {
        length += 1;
    }
    let decoded = String::from_utf16(unsafe { std::slice::from_raw_parts(value, length) });
    unsafe {
        CoTaskMemFree(value.cast());
    }
    Ok(Utf8PathBuf::from(decoded.map_err(error)?).join("moyAI-resource-admission"))
}
pub(super) fn ensure(directory: &camino::Utf8Path) -> Result<(), RunnerError> {
    let sid = crate::runner::windows::current_user_sid()?;
    let sddl = wide(&format!(
        "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})(A;OICI;GRGX;;;BU)"
    ));
    let mut descriptor = null_mut();
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(error(std::io::Error::last_os_error()));
    }
    let _descriptor = Allocation(descriptor);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    if unsafe { CreateDirectoryW(wide(directory.as_str()).as_ptr(), &attributes) } == 0 {
        let code = unsafe { GetLastError() };
        if code != ERROR_ALREADY_EXISTS {
            return Err(error(format!(
                "Machine resource policy cannot be installed without administrator setup: {}",
                std::io::Error::from_raw_os_error(code as i32)
            )));
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(directory)
        .map_err(error)?;
    if file.metadata().map_err(error)?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(error(
            "Machine policy directory must not be a reparse point",
        ));
    }
    use std::os::windows::io::AsRawHandle;
    let mut owner = null_mut();
    let mut dacl = null_mut();
    let mut descriptor = null_mut();
    let result = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(error(std::io::Error::from_raw_os_error(result as i32)));
    }
    let _descriptor = Allocation(descriptor);
    let mut control = 0;
    let mut revision = 0;
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
        || dacl.is_null()
    {
        return Err(error(
            "Machine policy requires a protected owner/administrators/System write ACL",
        ));
    }
    let owner = unsafe { sid_text(owner) }?;
    if !owner.starts_with("S-1-5-21-") && owner != "S-1-5-32-544" && owner != "S-1-5-18" {
        return Err(error(
            "Machine policy owner is not an operator or OS administrator",
        ));
    }
    let mut users_read = false;
    for index in 0..unsafe { (*dacl).AceCount } as u32 {
        let mut raw = null_mut();
        if unsafe { GetAce(dacl, index, &mut raw) } == 0 {
            return Err(error(std::io::Error::last_os_error()));
        }
        let ace = unsafe { &*raw.cast::<ACCESS_ALLOWED_ACE>() };
        if ace.Header.AceType != ACCESS_ALLOWED_ACE_TYPE as u8 {
            return Err(error("Machine policy has unsupported access rules"));
        }
        let trustee = unsafe { sid_text((&ace.SidStart as *const u32).cast_mut().cast()) }?;
        if trustee == owner || trustee == "S-1-5-18" || trustee == "S-1-5-32-544" {
            continue;
        }
        if trustee != "S-1-5-32-545"
            || ace.Mask
                & !(FILE_GENERIC_READ | FILE_GENERIC_EXECUTE | GENERIC_READ | GENERIC_EXECUTE)
                != 0
        {
            return Err(error(
                "Machine policy grants modification to an untrusted account",
            ));
        }
        users_read = true;
    }
    if !users_read {
        return Err(error(
            "Machine policy must be readable by every Windows user",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn machine_policy_acl_is_created_protected_and_read_only_handles_can_lock() {
        let path = camino::Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("project_sandbox/shared-work-completion-20260913/machine-acl-focused");
        std::fs::create_dir_all(&path).unwrap();
        let fixture = tempfile::tempdir_in(path).unwrap();
        let root = camino::Utf8Path::from_path(fixture.path()).unwrap();
        let policy = root.join("machine-policy");
        ensure(&policy).unwrap();
        ensure(&policy).unwrap();
        let barrier = policy.join("publication.lock");
        std::fs::write(&barrier, []).unwrap();
        let read = std::fs::File::open(&barrier).unwrap();
        let another = std::fs::File::open(&barrier).unwrap();
        fs2::FileExt::try_lock_shared(&read).unwrap();
        assert!(fs2::FileExt::try_lock_exclusive(&another).is_err());
        drop(read);
        fs2::FileExt::try_lock_exclusive(&another).unwrap();
        let machine = directory().unwrap();
        assert!(machine.is_absolute());
        assert_eq!(machine.file_name(), Some("moyAI-resource-admission"));
        // Only discover the actual default location; never replace its existing policy.
    }
    #[test]
    fn existing_unprotected_machine_policy_is_not_silently_rewritten() {
        let path = camino::Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("project_sandbox/shared-work-completion-20260913/machine-acl-focused");
        std::fs::create_dir_all(&path).unwrap();
        let fixture = tempfile::tempdir_in(path).unwrap();
        let root = camino::Utf8Path::from_path(fixture.path()).unwrap();
        let policy = root.join("existing");
        std::fs::create_dir(&policy).unwrap();
        assert!(ensure(&policy).is_err());
        assert!(policy.is_dir());
    }
}
