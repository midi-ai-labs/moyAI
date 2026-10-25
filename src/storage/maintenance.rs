//! Keep existing Desktop and session owners out of an explicit user-data reset.
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};

use camino::{Utf8Path, Utf8PathBuf};
use fs2::FileExt;
use sha2::{Digest, Sha256};

pub(crate) const DESKTOP_INSTANCE_LOCK: &str = "desktop-instance.lock";

pub(crate) struct UserDataQuiescence {
    _leases: Vec<File>,
    #[cfg(windows)]
    database: Option<File>,
}

impl UserDataQuiescence {
    /// The caller holds exclusive publication ownership before entering, so new
    /// Desktop startups and execution admission cannot pass this snapshot.
    pub(crate) fn acquire(data: &Utf8Path) -> Result<Self, String> {
        Self::acquire_checked(data, None)
    }

    pub(crate) fn acquire_for_cleanup(
        data: &Utf8Path,
        targets: &[Utf8PathBuf],
    ) -> Result<Self, String> {
        Self::acquire_checked(data, Some(targets))
    }

    fn acquire_checked(
        data: &Utf8Path,
        cleanup_targets: Option<&[Utf8PathBuf]>,
    ) -> Result<Self, String> {
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
        let workspace_snapshot = cleanup_targets
            .map(|_| read_workspace_snapshot(data))
            .transpose()?;
        #[cfg(windows)]
        let mut database = lock_database(&data.join("moyai.sqlite3"))?;
        if let Some(snapshot) = workspace_snapshot {
            #[cfg(windows)]
            let pinned = signature_with_database_pin(data, database.as_mut())?;
            #[cfg(not(windows))]
            let pinned = database_signature(data)?;
            snapshot.validate(pinned, cleanup_targets.unwrap_or_default())?;
        }
        Ok(Self {
            _leases: leases,
            #[cfg(windows)]
            database,
        })
    }

    /// The directory has been moved out of the old namespace. The migration
    /// keeps publication and startup leases while updating its own SQLite paths.
    pub(crate) fn release_database_for_migration(&mut self) {
        #[cfg(windows)]
        self.database.take();
    }
}

#[derive(PartialEq, Eq)]
struct FileSignature {
    bytes: u64,
    sha256: [u8; 32],
}

#[derive(PartialEq, Eq)]
struct DatabaseSignature {
    database: Option<FileSignature>,
    wal: Option<FileSignature>,
}

struct WorkspaceSnapshot {
    signature: DatabaseSignature,
    workspaces: Vec<Utf8PathBuf>,
}

impl WorkspaceSnapshot {
    fn validate(self, pinned: DatabaseSignature, targets: &[Utf8PathBuf]) -> Result<(), String> {
        if self.signature != pinned {
            return Err(changed_database());
        }
        for workspace in self.workspaces {
            for target in targets {
                if crate::workspace::PathGuard::security_path_is_within(&workspace, target)
                    .map_err(|error| error.to_string())?
                {
                    return Err(format!(
                        "作業フォルダーの登録が削除対象に含まれるため初期化を中止しました: {workspace}。フォルダーを保存先の外へ移して元の登録を削除するか、保存データを残してアンインストールしてください。データは削除していません。"
                    ));
                }
            }
        }
        Ok(())
    }
}

fn changed_database() -> String {
    "作業フォルダーの確認中に保存データが変わりました。すべてのmoyAIを終了してからやり直してください。削除していません。".into()
}

fn file_signature(file: &mut File) -> Result<FileSignature, String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
        bytes += count as u64;
    }
    Ok(FileSignature {
        bytes,
        sha256: digest.finalize().into(),
    })
}

fn optional_signature(path: &Utf8Path) -> Result<Option<FileSignature>, String> {
    match File::open(path) {
        Ok(mut file) => file_signature(&mut file).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("保存データの静止を確認できません: {path}: {error}")),
    }
}

fn database_signature(data: &Utf8Path) -> Result<DatabaseSignature, String> {
    Ok(DatabaseSignature {
        database: optional_signature(&data.join("moyai.sqlite3"))?,
        wal: wal_signature(data)?,
    })
}

#[cfg(windows)]
fn signature_with_database_pin(
    data: &Utf8Path,
    file: Option<&mut File>,
) -> Result<DatabaseSignature, String> {
    let database = match file {
        Some(file) => Some(file_signature(file)?),
        None => match std::fs::symlink_metadata(data.join("moyai.sqlite3")) {
            Ok(_) => return Err(changed_database()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        },
    };
    Ok(DatabaseSignature {
        database,
        wal: wal_signature(data)?,
    })
}

fn wal_signature(data: &Utf8Path) -> Result<Option<FileSignature>, String> {
    // Opening a read-only WAL database may create an empty WAL. It has no
    // frames and does not change the snapshot; any nonempty WAL is compared.
    Ok(optional_signature(&data.join("moyai.sqlite3-wal"))?
        .filter(|signature| signature.bytes != 0))
}

fn read_workspace_snapshot(data: &Utf8Path) -> Result<WorkspaceSnapshot, String> {
    let before = database_signature(data)?;
    if before.database.is_none() {
        return Ok(WorkspaceSnapshot {
            signature: before,
            workspaces: Vec::new(),
        });
    }
    let reader = rusqlite::Connection::open_with_flags(
        data.join("moyai.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|error| format!("作業フォルダーを確認できません。削除していません: {error}"))?;
    let workspaces = crate::storage::project_repo::cleanup_preserved_workspace_roots(&reader, data)
        .map_err(|error| error.to_string())?;
    let after = database_signature(data)?;
    drop(reader);
    if before != after {
        return Err(changed_database());
    }
    Ok(WorkspaceSnapshot {
        signature: after,
        workspaces,
    })
}

#[cfg(windows)]
fn lock_database(path: &Utf8Path) -> Result<Option<File>, String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE;

    if !path.exists() {
        return Ok(None);
    }
    // Deny new readers/writers and reject a database already open by an older
    // process. DELETE sharing lets this same guard survive the confirmed reset.
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_DELETE)
        .open(path)
        .map_err(|error| format!("会話データが使用中か、終了状態を確認できません。すべてのmoyAIを終了してください。詳細: {error}"))?;
    Ok(Some(file))
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

    async fn save_projects(data: &Utf8Path, projects: &[(&Utf8Path, &str)]) {
        use crate::session::{ProjectId, ProjectRepository};
        let paths = crate::storage::StoragePaths {
            data_dir: data.to_owned(),
            database_path: data.join("moyai.sqlite3"),
            truncation_dir: data.join("truncation"),
        };
        let sqlite = crate::storage::SqliteStore::open(&paths).unwrap();
        sqlite.migrate().unwrap();
        let store = crate::storage::StoreBundle::new(sqlite);
        for (workspace, vcs) in projects {
            std::fs::create_dir_all(workspace).unwrap();
            store
                .project_repo()
                .upsert_project(
                    ProjectId::from_stable_input(workspace.as_str()),
                    workspace,
                    "workspace",
                    vcs,
                )
                .await
                .unwrap();
        }
        drop(store);
    }

    #[tokio::test]
    async fn cleanup_allows_internal_saved_work_and_keeps_external_and_ancestor_workspaces() {
        let temp = crate::user_data::test_directory();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let desktop = root.join("desktop");
        let data = desktop.join("data");
        let internal = crate::user_data::internal_desktop_project_roots(&data).remove(0);
        let external = root.join("external");
        save_projects(
            &data,
            &[(&internal, "none"), (&external, "none"), (root, "none")],
        )
        .await;
        std::fs::write(internal.join("saved.txt"), "saved chat output").unwrap();
        std::fs::write(external.join("keep.txt"), "external project").unwrap();
        std::fs::write(root.join("keep.txt"), "ancestor project").unwrap();
        let guard = UserDataQuiescence::acquire_for_cleanup(&data, &[desktop.clone()]).unwrap();
        std::fs::remove_dir_all(&desktop).unwrap();
        assert!(!internal.exists());
        assert_eq!(
            std::fs::read_to_string(external.join("keep.txt")).unwrap(),
            "external project"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("keep.txt")).unwrap(),
            "ancestor project"
        );
        drop(guard);
    }

    #[tokio::test]
    async fn cleanup_preserves_an_internal_workspace_when_it_is_a_git_project() {
        for recorded_vcs in ["git", "none"] {
            let temp = crate::user_data::test_directory();
            let root = Utf8Path::from_path(temp.path()).unwrap();
            let desktop = root.join("desktop");
            let data = desktop.join("data");
            let project = crate::user_data::internal_desktop_project_roots(&data).remove(0);
            save_projects(&data, &[(&project, recorded_vcs)]).await;
            std::fs::create_dir_all(project.join(".git")).unwrap();
            std::fs::write(project.join("keep.txt"), "project files").unwrap();
            let error = UserDataQuiescence::acquire_for_cleanup(&data, &[desktop])
                .err()
                .unwrap();
            assert!(error.contains(project.as_str()));
            assert_eq!(
                std::fs::read_to_string(project.join("keep.txt")).unwrap(),
                "project files"
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn cleanup_rejects_a_project_change_between_read_snapshot_and_database_pin() {
        let temp = crate::user_data::test_directory();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let desktop = root.join("desktop");
        let data = desktop.join("data");
        let external = root.join("external");
        save_projects(&data, &[(&external, "none")]).await;
        let snapshot = read_workspace_snapshot(&data).unwrap();
        let added = data.join("new-project");
        save_projects(&data, &[(&added, "none")]).await;
        let mut pin = lock_database(&data.join("moyai.sqlite3")).unwrap();
        let signature = signature_with_database_pin(&data, pin.as_mut()).unwrap();
        assert!(snapshot.validate(signature, &[desktop]).is_err());
        assert!(added.exists());
    }

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
