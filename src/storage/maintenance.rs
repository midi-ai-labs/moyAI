//! Keep existing Desktop and session owners out of an explicit user-data reset.
use std::fs::{File, OpenOptions};
use std::io;

use camino::Utf8Path;
use fs2::FileExt;

pub(crate) const DESKTOP_INSTANCE_LOCK: &str = "desktop-instance.lock";

pub(crate) struct UserDataQuiescence {
    _leases: Vec<File>,
}

impl UserDataQuiescence {
    /// The caller holds exclusive publication ownership before entering, so new
    /// Desktop startups and execution admission cannot pass this snapshot.
    pub(crate) fn acquire(data: &Utf8Path) -> Result<Self, String> {
        let mut leases = Vec::new();
        lock_existing(
            &data.join(DESKTOP_INSTANCE_LOCK),
            "moyAI Desktopを「ファイル → moyAIを終了」で終了してください。×ボタンはトレイへ格納します。",
            &mut leases,
        )?;
        let entries = match std::fs::read_dir(data.join("run-leases")) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("実行状態を確認できません: {error}")),
        };
        if let Some(entries) = entries {
            for entry in entries {
                let entry = entry.map_err(|error| error.to_string())?;
                let path = camino::Utf8PathBuf::from_path_buf(entry.path())
                    .map_err(|_| "実行状態の保存先がUTF-8ではありません。".to_string())?;
                if path.extension() != Some("lock") {
                    continue;
                }
                lock_existing(
                    &path,
                    "moyAIの仕事が実行中です。CLI・TUI・Runnerの仕事を終了してからやり直してください。",
                    &mut leases,
                )?;
            }
        }
        #[cfg(windows)]
        lock_database(&data.join("moyai.sqlite3"), &mut leases)?;
        Ok(Self { _leases: leases })
    }
}

#[cfg(windows)]
fn lock_database(path: &Utf8Path, leases: &mut Vec<File>) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE;

    if !path.exists() {
        return Ok(());
    }
    // Deny new readers/writers and reject a database already open by an older
    // process. DELETE sharing lets this same guard survive the confirmed reset.
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_DELETE)
        .open(path)
        .map_err(|error| format!("会話データが使用中か、終了状態を確認できません。すべてのmoyAIを終了してください。詳細: {error}"))?;
    leases.push(file);
    Ok(())
}

fn lock_existing(path: &Utf8Path, busy: &str, leases: &mut Vec<File>) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("終了状態を確認できません: {error}")),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("終了状態の保存先が通常のファイルではありません。削除していません。".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("終了状態を確認できません: {error}"))?;
    file.try_lock_exclusive().map_err(|_| busy.to_string())?;
    leases.push(file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RunProcessLease;
    use crate::session::SessionId;

    #[test]
    fn refuses_live_desktop_and_preserves_data() {
        let directory = tempfile::tempdir().unwrap();
        let data = Utf8Path::from_path(directory.path()).unwrap();
        let saved = data.join("saved.txt");
        std::fs::write(&saved, b"keep").unwrap();
        let desktop = File::create(data.join(DESKTOP_INSTANCE_LOCK)).unwrap();
        desktop.try_lock_exclusive().unwrap();
        assert!(UserDataQuiescence::acquire(data).is_err());
        assert_eq!(std::fs::read(saved).unwrap(), b"keep");
        drop(desktop);
        assert!(UserDataQuiescence::acquire(data).is_ok());
    }

    #[test]
    fn refuses_live_session_and_accepts_released_persistent_lease() {
        let directory = tempfile::tempdir().unwrap();
        let data = Utf8Path::from_path(directory.path()).unwrap();
        let session = SessionId::new();
        let running = RunProcessLease::try_acquire(data, session).unwrap();
        assert!(UserDataQuiescence::acquire(data).is_err());
        drop(running);
        let maintenance = UserDataQuiescence::acquire(data).unwrap();
        assert!(RunProcessLease::try_acquire(data, session).is_err());
        drop(maintenance);
        assert!(RunProcessLease::try_acquire(data, session).is_ok());
    }

    #[test]
    fn nonexistent_data_is_not_created_by_preflight() {
        let directory = tempfile::tempdir().unwrap();
        let data = Utf8Path::from_path(directory.path())
            .unwrap()
            .join("absent");
        let _maintenance = UserDataQuiescence::acquire(&data).unwrap();
        assert!(!data.exists());
    }

    #[cfg(windows)]
    #[test]
    fn refuses_open_database_then_allows_deletion_while_guard_blocks_new_readers() {
        let directory = tempfile::tempdir().unwrap();
        let data = Utf8Path::from_path(directory.path()).unwrap().join("data");
        std::fs::create_dir(&data).unwrap();
        let database = data.join("moyai.sqlite3");
        let reader = rusqlite::Connection::open(&database).unwrap();
        assert!(UserDataQuiescence::acquire(&data).is_err());
        drop(reader);
        let _guard = UserDataQuiescence::acquire(&data).unwrap();
        assert!(File::open(&database).is_err());
        std::fs::remove_dir_all(&data).unwrap();
        assert!(!data.exists());
    }
}
