//! Per-user logon startup. No service account, elevation, or shell is involved.
use super::RunnerError;

#[cfg(windows)]
mod windows {
    use super::*;
    use windows_sys::Win32::{Foundation::*, System::Registry::*};
    const KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const NAME: &str = "moyAI Runner";
    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }
    struct Key(HKEY);
    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }
    fn open(access: u32) -> Result<Key, RunnerError> {
        let mut key = std::ptr::null_mut();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(KEY).as_ptr(),
                0,
                std::ptr::null(),
                0,
                access,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(RunnerError::new(
                std::io::Error::from_raw_os_error(status as i32).to_string(),
            ));
        }
        Ok(Key(key))
    }
    pub(super) fn installed() -> Result<bool, RunnerError> {
        Ok(read_command()?.is_some())
    }
    fn read_command() -> Result<Option<String>, RunnerError> {
        let mut raw = std::ptr::null_mut();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(KEY).as_ptr(),
                0,
                KEY_QUERY_VALUE,
                &mut raw,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            return Err(RunnerError::new("Cannot inspect Runner autostart"));
        }
        let key = Key(raw);
        let mut size = 0;
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                wide(NAME).as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        match status {
            ERROR_SUCCESS => {
                if size > 4096 {
                    return Err(RunnerError::new(
                        "Existing Runner autostart exceeds its bound",
                    ));
                }
                let mut value = vec![0u16; (size as usize).div_ceil(2)];
                let mut kind = 0;
                if unsafe {
                    RegQueryValueExW(
                        key.0,
                        wide(NAME).as_ptr(),
                        std::ptr::null(),
                        &mut kind,
                        value.as_mut_ptr().cast(),
                        &mut size,
                    )
                } != ERROR_SUCCESS
                    || kind != REG_SZ
                {
                    return Err(RunnerError::new(
                        "Existing Runner autostart is not a command string",
                    ));
                }
                while value.last() == Some(&0) {
                    value.pop();
                }
                String::from_utf16(&value)
                    .map(Some)
                    .map_err(|_| RunnerError::new("Invalid Runner autostart string"))
            }
            ERROR_FILE_NOT_FOUND => Ok(None),
            _ => Err(RunnerError::new("Cannot inspect Runner autostart")),
        }
    }
    pub(super) fn install() -> Result<(), RunnerError> {
        let executable = std::env::current_exe().map_err(|e| RunnerError::new(e.to_string()))?;
        let executable = executable
            .to_str()
            .ok_or_else(|| RunnerError::new("Runner path must be UTF-8"))?;
        let command = format!("\"{executable}\" serve --background");
        if command.encode_utf16().count() > 260 || executable.contains(['"', '\r', '\n']) {
            return Err(RunnerError::new(
                "Autostart command exceeds the Windows Run-key limit",
            ));
        }
        // An overridden profile does not survive logon. Refuse to silently start another one.
        if std::env::var_os("MOYAI_CONFIG_PATH").is_some()
            || std::env::var_os("MOYAI_DATA_DIR").is_some()
        {
            return Err(RunnerError::new(
                "Logon autostart requires the normal OS-user profile; isolated profiles must be started explicitly",
            ));
        }
        if read_command()?.is_some_and(|existing| existing != command) {
            return Err(RunnerError::new(
                "Another Runner installation owns logon startup; remove it from that installation first",
            ));
        }
        let value = wide(&command);
        let key = open(KEY_SET_VALUE)?;
        let status = unsafe {
            RegSetValueExW(
                key.0,
                wide(NAME).as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(RunnerError::new("Cannot install Runner autostart"));
        }
        Ok(())
    }
    pub(super) fn remove() -> Result<(), RunnerError> {
        let executable = std::env::current_exe().map_err(|e| RunnerError::new(e.to_string()))?;
        remove_for(&executable)
    }
    pub(super) fn remove_for(executable: &std::path::Path) -> Result<(), RunnerError> {
        if std::env::var_os("MOYAI_CONFIG_PATH").is_some()
            || std::env::var_os("MOYAI_DATA_DIR").is_some()
        {
            return Err(RunnerError::new(
                "An isolated profile cannot change normal-profile logon startup",
            ));
        }
        let command = format!("\"{}\" serve --background", executable.display());
        let Some(existing) = read_command()? else {
            return Ok(());
        };
        if existing != command {
            return Err(RunnerError::new(
                "Another Runner installation owns logon startup",
            ));
        }
        let key = open(KEY_SET_VALUE)?;
        let status = unsafe { RegDeleteValueW(key.0, wide(NAME).as_ptr()) };
        if !matches!(status, ERROR_SUCCESS | ERROR_FILE_NOT_FOUND) {
            return Err(RunnerError::new("Cannot remove Runner autostart"));
        }
        Ok(())
    }
}

pub(crate) fn installed() -> Result<bool, RunnerError> {
    #[cfg(windows)]
    {
        windows::installed()
    }
    #[cfg(not(windows))]
    {
        Ok(false)
    }
}
pub(crate) fn install() -> Result<(), RunnerError> {
    #[cfg(windows)]
    {
        windows::install()
    }
    #[cfg(not(windows))]
    {
        Err(RunnerError::new(
            "Logon autostart currently supports Windows only",
        ))
    }
}
pub(crate) fn remove() -> Result<(), RunnerError> {
    #[cfg(windows)]
    {
        windows::remove()
    }
    #[cfg(not(windows))]
    {
        Err(RunnerError::new(
            "Logon autostart currently supports Windows only",
        ))
    }
}

pub(crate) fn remove_adjacent_runner() -> Result<(), RunnerError> {
    #[cfg(windows)]
    {
        let desktop = std::env::current_exe().map_err(|e| RunnerError::new(e.to_string()))?;
        windows::remove_for(&desktop.with_file_name("moyai-runner.exe"))
    }
    #[cfg(not(windows))]
    {
        Err(RunnerError::new(
            "Logon autostart currently supports Windows only",
        ))
    }
}
