use std::fs::{File, OpenOptions};
use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use directories_next::ProjectDirs;
use fs2::FileExt;

const LOCK_FILE_NAME: &str = "desktop-instance.lock";

/// A process-scoped lease that must be held for the complete Desktop lifetime.
///
/// The lock file is deliberately persistent. The operating system owns the
/// actual lease and releases it when the process exits, including after a
/// crash, so a leftover file never constitutes a stale lock.
pub struct DesktopInstanceGuard {
    file: File,
}

impl DesktopInstanceGuard {
    /// Acquires the Desktop lease, or asks the already-running Desktop to
    /// restore its main window and returns `None` to stop this launch.
    pub fn acquire_or_notify() -> Result<Option<Self>, String> {
        let lock_path = desktop_lock_path()?;
        match Self::try_acquire_at(&lock_path) {
            Ok(guard) => {
                // The file lease was introduced after the Tauri listener.
                // Probe once for an already-running older Desktop whose Tauri
                // listener is already ready but which does not own this lease.
                // Simultaneous mixed-version cold starts cannot be coordinated
                // pre-bootstrap by the new binary alone; current builds use the
                // file lease below as their strict launch boundary.
                if notify_existing_instance_once() {
                    return Ok(None);
                }
                Ok(Some(guard))
            }
            Err(lock_error) => {
                if notify_existing_instance() {
                    return Ok(None);
                }

                // The first process may have exited while we waited for its
                // Tauri notification endpoint. In that case this process can
                // safely become the sole Desktop owner.
                if let Ok(guard) = Self::try_acquire_at(&lock_path) {
                    return Ok(Some(guard));
                }

                show_launch_blocked_message();
                Err(format!(
                    "moyAI Desktop could not acquire its single-instance lease and no running window responded: {lock_error}"
                ))
            }
        }
    }

    fn try_acquire_at(path: &Utf8Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent.as_std_path())?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.as_std_path())?;
        FileExt::try_lock_exclusive(&file)?;
        Ok(Self { file })
    }
}

impl Drop for DesktopInstanceGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn desktop_lock_path() -> Result<Utf8PathBuf, String> {
    let dirs = ProjectDirs::from("net", "midi-ai-labs", "moyai")
        .ok_or_else(|| "failed to resolve the moyAI data directory".to_string())?;
    let data_dir = Utf8PathBuf::from_path_buf(dirs.data_dir().to_path_buf())
        .map_err(|_| "moyAI data directory is not valid UTF-8".to_string())?;
    Ok(data_dir.join(LOCK_FILE_NAME))
}

#[cfg(target_os = "windows")]
fn notify_existing_instance() -> bool {
    windows::notify_existing_instance()
}

#[cfg(target_os = "windows")]
fn notify_existing_instance_once() -> bool {
    windows::notify_existing_instance_once()
}

#[cfg(not(target_os = "windows"))]
fn notify_existing_instance() -> bool {
    false
}

#[cfg(not(target_os = "windows"))]
fn notify_existing_instance_once() -> bool {
    false
}

fn show_launch_blocked_message() {
    let _ = rfd::MessageDialog::new()
        .set_title("moyAI")
        .set_description(
            "moyAI は既に起動している可能性があります。\n既存ウィンドウを前面に戻せなかったため、新しい起動を中止しました。",
        )
        .set_level(rfd::MessageLevel::Warning)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(target_os = "windows")]
mod windows {
    use std::ffi::{OsStr, c_void};
    use std::os::windows::ffi::OsStrExt;
    use std::time::{Duration, Instant};

    const WM_COPYDATA: u32 = 0x004A;
    const SINGLE_INSTANCE_DATA_ID: usize = 1542;
    const SMTO_ABORT_IF_HUNG: u32 = 0x0002;
    const SEND_TIMEOUT_MS: u32 = 1_000;
    const LISTENER_WAIT: Duration = Duration::from_secs(10);
    const RETRY_INTERVAL: Duration = Duration::from_millis(50);

    #[repr(C)]
    struct CopyDataStruct {
        data_id: usize,
        byte_count: u32,
        data: *const c_void,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn FindWindowW(class_name: *const u16, window_name: *const u16) -> *mut c_void;
        fn SendMessageTimeoutW(
            window: *mut c_void,
            message: u32,
            w_param: usize,
            l_param: isize,
            flags: u32,
            timeout_ms: u32,
            result: *mut usize,
        ) -> isize;
    }

    pub(super) fn notify_existing_instance() -> bool {
        let Some(identifier) = tauri_identifier() else {
            return false;
        };
        let class_name = encode_wide(format!("{identifier}-sic"));
        let window_name = encode_wide(format!("{identifier}-siw"));
        let deadline = Instant::now() + LISTENER_WAIT;

        loop {
            if send_restore_request(&class_name, &window_name) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(RETRY_INTERVAL);
        }
    }

    pub(super) fn notify_existing_instance_once() -> bool {
        let Some(identifier) = tauri_identifier() else {
            return false;
        };
        let class_name = encode_wide(format!("{identifier}-sic"));
        let window_name = encode_wide(format!("{identifier}-siw"));
        send_restore_request(&class_name, &window_name)
    }

    fn tauri_identifier() -> Option<String> {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).ok()?;
        config
            .get("identifier")?
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }

    fn send_restore_request(class_name: &[u16], window_name: &[u16]) -> bool {
        let window = unsafe { FindWindowW(class_name.as_ptr(), window_name.as_ptr()) };
        if window.is_null() {
            return false;
        }

        // Match tauri-plugin-single-instance's Windows wire format so the
        // existing callback remains the sole owner of window restoration.
        let cwd = std::env::current_dir().unwrap_or_default();
        let cwd = cwd.to_string_lossy();
        let args = std::env::args_os()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("|");
        let payload = format!("{cwd}|{args}\0");
        let data = CopyDataStruct {
            data_id: SINGLE_INSTANCE_DATA_ID,
            byte_count: payload.len() as u32,
            data: payload.as_ptr().cast(),
        };
        let mut result = 0usize;
        let sent = unsafe {
            SendMessageTimeoutW(
                window,
                WM_COPYDATA,
                0,
                (&raw const data).cast::<c_void>() as isize,
                SMTO_ABORT_IF_HUNG,
                SEND_TIMEOUT_MS,
                &raw mut result,
            )
        };
        sent != 0 && result != 0
    }

    fn encode_wide(value: impl AsRef<OsStr>) -> Vec<u16> {
        value
            .as_ref()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_desktop_lease_blocks_a_second_owner_until_drop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(temp.path().join(LOCK_FILE_NAME)).expect("utf8 lock path");
        let first = DesktopInstanceGuard::try_acquire_at(&path).expect("first lease");

        assert!(DesktopInstanceGuard::try_acquire_at(&path).is_err());
        drop(first);
        DesktopInstanceGuard::try_acquire_at(&path).expect("lease after owner drop");
    }

    #[test]
    fn leftover_lock_file_is_not_treated_as_a_live_owner() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path =
            Utf8PathBuf::from_path_buf(temp.path().join(LOCK_FILE_NAME)).expect("utf8 lock path");
        std::fs::write(path.as_std_path(), b"stale metadata").expect("stale lock file");

        DesktopInstanceGuard::try_acquire_at(&path).expect("lease over stale file");
    }
}
