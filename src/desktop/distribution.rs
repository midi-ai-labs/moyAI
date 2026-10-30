//! Cloud files are distribution input, never sandbox/runtime payload.

#[cfg(windows)]
fn cloud_tag(tag: u32) -> bool {
    tag & 0xffff_0fff == 0x9000_001a
}

#[cfg(windows)]
#[link(name = "ntdll")]
unsafe extern "system" {
    fn RtlSetThreadPlaceholderCompatibilityMode(mode: i8) -> i8;
}

#[cfg(windows)]
struct PlaceholderMode(i8);

#[cfg(windows)]
impl Drop for PlaceholderMode {
    fn drop(&mut self) {
        unsafe { RtlSetThreadPlaceholderCompatibilityMode(self.0) };
    }
}

#[cfg(windows)]
fn cloud_source(path: &std::path::Path) -> Result<bool, String> {
    use std::fs::OpenOptions;
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::*;

    // Windows can disguise Cloud Files as ordinary files for compatibility.
    // Inspect on this synchronous thread only, restoring its mode on all exits.
    let previous = unsafe { RtlSetThreadPlaceholderCompatibilityMode(2) };
    if previous < 0 {
        return Err("配布物のクラウド属性を確認できません。".into());
    }
    let _mode = PlaceholderMode(previous);
    let mut cloud = false;
    for ancestor in path.ancestors() {
        let file = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(ancestor)
            .map_err(|error| {
                format!(
                    "配布物の場所を確認できません: {}: {error}",
                    ancestor.display()
                )
            })?;
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileAttributeTagInfo,
                (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            if !cloud_tag(tag.ReparseTag) {
                return Err(format!(
                    "配布物の場所に対応していないリンクがあります。通常のフォルダーへ展開してください: {}",
                    ancestor.display()
                ));
            }
            cloud = true;
        }
    }
    Ok(cloud)
}

/// Hand off before profile locks or sandbox startup. The script hydrates and
/// verifies the package, installs if necessary, and opens the local Desktop.
pub fn handoff_if_cloud() -> Result<bool, String> {
    #[cfg(windows)]
    {
        use base64::Engine;
        use std::os::windows::process::CommandExt;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let directory = executable
            .parent()
            .ok_or("Desktopの配置を確認できません。")?;
        let mut cloud = cloud_source(&executable)?;
        // A sync provider may mark only individual payload files as placeholders.
        for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("dll")
            }) {
                cloud |= cloud_source(&path)?;
            }
        }
        if !cloud {
            return Ok(false);
        }
        let root = directory
            .parent()
            .and_then(|app| app.parent())
            .ok_or("配布物の構成を確認できません。")?;
        let script = root.join("scripts/Start-moyAI.ps1");
        if !directory.ends_with("app/bin")
            || !script.is_file()
            || !root.join("deployment.json").is_file()
        {
            return Err("同期フォルダーから起動するには、ZIP全体を展開し、Setup-moyAI.cmdを実行してください。".into());
        }
        let args = std::env::args_os()
            .skip(1)
            .map(|arg| arg.into_string().map_err(|_| "起動引数を読み取れません。"))
            .collect::<Result<Vec<_>, _>>()?;
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(serde_json::to_vec(&args).map_err(|error| error.to_string())?);
        let mut system = [0u16; 32768];
        let length = unsafe {
            windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
                system.as_mut_ptr(),
                system.len() as u32,
            )
        } as usize;
        if length == 0 || length >= system.len() {
            return Err("Windowsの起動コマンドを確認できません。".into());
        }
        let powershell = std::path::PathBuf::from(
            String::from_utf16(&system[..length]).map_err(|error| error.to_string())?,
        )
        .join("WindowsPowerShell/v1.0/powershell.exe");
        let status = std::process::Command::new(powershell)
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(script)
            .arg("-DesktopArguments")
            .arg(encoded)
            .arg("-WorkingDirectory")
            .arg(std::env::current_dir().map_err(|error| error.to_string())?)
            .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
            .status()
            .map_err(|error| error.to_string())?;
        if !status.success() {
            return Err("ローカルへの導入・起動を完了できませんでした。Setup-moyAI.cmdを実行し、表示された内容を確認してください。".into());
        }
        return Ok(true);
    }
    #[cfg(not(windows))]
    Ok(false)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn only_cloud_reparse_tags_are_distribution_sources() {
        for variant in 0..16 {
            assert!(cloud_tag(0x9000_001a | variant << 12));
        }
        for tag in [0, 0xa000_0003, 0xa000_000c, 0x8000_001b, 0x9001_001a] {
            assert!(!cloud_tag(tag));
        }
    }
    #[test]
    fn ordinary_executable_is_not_redirected() {
        assert!(!cloud_source(&std::env::current_exe().unwrap()).unwrap());
    }

    #[test]
    #[ignore = "requires an isolated hydrated Windows Cloud Files fixture"]
    fn disguised_cloud_file_is_detected_and_thread_mode_is_restored() {
        let path = std::path::PathBuf::from(
            std::env::var_os("MOYAI_TEST_CLOUD_FILE").expect("provide a real Cloud Files fixture"),
        );
        let previous = unsafe { RtlSetThreadPlaceholderCompatibilityMode(1) };
        assert!(previous >= 0);
        let _restore = PlaceholderMode(previous);
        assert!(cloud_source(&path).unwrap());
        assert_eq!(unsafe { RtlSetThreadPlaceholderCompatibilityMode(1) }, 1);
        assert!(cloud_source(&path.join("does-not-exist")).is_err());
        assert_eq!(unsafe { RtlSetThreadPlaceholderCompatibilityMode(1) }, 1);
    }
}
