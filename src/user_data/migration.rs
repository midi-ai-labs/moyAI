use std::fs::{self, OpenOptions};
use std::io::Write;

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::json;

use super::{DesktopPaths, assert_regular_tree};
use crate::runtime::resource_admission::ProfileMaintenanceGuard;

mod references;

enum MigrationFailure {
    Operation(String),
    References(references::RelocationFailure),
}

impl From<String> for MigrationFailure {
    fn from(message: String) -> Self {
        Self::Operation(message)
    }
}

impl From<references::RelocationFailure> for MigrationFailure {
    fn from(failure: references::RelocationFailure) -> Self {
        Self::References(failure)
    }
}

impl std::fmt::Display for MigrationFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Operation(message) => formatter.write_str(message),
            Self::References(failure) => std::fmt::Display::fmt(failure, formatter),
        }
    }
}

fn marker_paths(paths: &DesktopPaths) -> Result<(Utf8PathBuf, Utf8PathBuf), String> {
    let parent = paths
        .root
        .parent()
        .ok_or("Desktop data directory has no parent")?;
    Ok((
        parent.join("migrations/desktop.json"),
        parent.join("migrations/desktop.pending.json"),
    ))
}

pub(crate) fn mark_reset(paths: &DesktopPaths) -> Result<(), String> {
    let (marker, pending) = marker_paths(paths)?;
    if pending.exists() {
        return Err(format!(
            "移行が未完了です。保存先の状態を確認してください: {pending}"
        ));
    }
    write_marker(&marker, "reset")
}

pub(crate) fn check_cleanup_ready(paths: &DesktopPaths) -> Result<(), String> {
    let (_, pending) = marker_paths(paths)?;
    if pending.exists() {
        return Err(format!(
            "移行が未完了のため削除を中止しました。移行中のデータを先に確認してください: {pending}"
        ));
    }
    Ok(())
}

/// An explicit path override must not open a partially moved or retired default
/// profile. Unrelated custom profiles do not participate in this migration.
pub(crate) fn check_explicit_profile(
    paths: &DesktopPaths,
    legacy_roots: &[Utf8PathBuf],
    webview: Option<&Utf8Path>,
    selected: &[Utf8PathBuf],
) -> Result<(), String> {
    let (marker, pending) = marker_paths(paths)?;
    if !marker.exists() && !pending.exists() {
        return Ok(());
    }
    let mut old = legacy_roots.to_vec();
    if let Some(webview) = webview {
        old.push(webview.to_owned());
    }
    for selected_path in selected {
        let cwd =
            Utf8PathBuf::from_path_buf(std::env::current_dir().map_err(|error| error.to_string())?)
                .map_err(|_| "Current directory is not UTF-8")?;
        let path = crate::workspace::project::normalize_path(&cwd, selected_path)
            .map_err(|error| error.to_string())?;
        let legacy = old.iter().try_fold(false, |inside, root| {
            crate::workspace::PathGuard::security_path_is_within(&path, root)
                .map(|matched| inside || matched)
                .map_err(|error| error.to_string())
        })?;
        let current = crate::workspace::PathGuard::security_path_is_within(&path, &paths.root)
            .map_err(|error| error.to_string())?;
        if pending.exists() && (legacy || current) {
            return Err(format!(
                "Desktop保存先の移行中または移行未完了のため、明示指定した保存先も起動できません。データを生成・上書きしていません。移行記録: {pending}"
            ));
        }
        if legacy && marker.exists() {
            return Err(format!(
                "この旧Desktop保存先は移行または初期化済みです。旧データを再作成せず中止しました: {path}。新保存先: {}",
                paths.root
            ));
        }
    }
    Ok(())
}

fn write_marker(path: &Utf8Path, state: &str) -> Result<(), String> {
    assert_regular_tree(path)?;
    let parent = path.parent().ok_or("Migration marker has no parent")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .write_all(
            &serde_json::to_vec(&json!({"version":1,"state":state}))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary.persist(path).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg_attr(test, allow(dead_code))]
pub(crate) fn prepare(
    paths: &DesktopPaths,
    legacy_roots: &[Utf8PathBuf],
    legacy_profile: &(Utf8PathBuf, Utf8PathBuf),
    webview: Option<&Utf8Path>,
) -> Result<(), String> {
    prepare_with(
        paths,
        legacy_roots,
        legacy_profile,
        webview,
        |profiles, targets| {
            ProfileMaintenanceGuard::acquire(profiles, targets).map_err(|error| error.message)
        },
    )
}

fn prepare_with(
    paths: &DesktopPaths,
    legacy_roots: &[Utf8PathBuf],
    legacy_profile: &(Utf8PathBuf, Utf8PathBuf),
    webview: Option<&Utf8Path>,
    acquire: impl FnOnce(
        &[(Utf8PathBuf, Utf8PathBuf)],
        &[Utf8PathBuf],
    ) -> Result<ProfileMaintenanceGuard, String>,
) -> Result<(), String> {
    prepare_with_relocation(
        paths,
        legacy_roots,
        legacy_profile,
        webview,
        acquire,
        references::relocate,
    )
}

fn prepare_with_relocation(
    paths: &DesktopPaths,
    legacy_roots: &[Utf8PathBuf],
    legacy_profile: &(Utf8PathBuf, Utf8PathBuf),
    webview: Option<&Utf8Path>,
    acquire: impl FnOnce(
        &[(Utf8PathBuf, Utf8PathBuf)],
        &[Utf8PathBuf],
    ) -> Result<ProfileMaintenanceGuard, String>,
    relocate: impl FnOnce(
        &Utf8Path,
        &Utf8Path,
        &[(Utf8PathBuf, Utf8PathBuf)],
    ) -> Result<references::Undo, references::RelocationFailure>,
) -> Result<(), String> {
    let (marker, pending) = marker_paths(paths)?;
    if pending.exists() {
        return Err(format!(
            "Desktopデータの移行が未完了です。上書きせずに中止しました。移行記録: {pending}"
        ));
    }
    let mut moves = Vec::new();
    for root in legacy_roots {
        assert_regular_tree(root)?;
        match fs::read_dir(root) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|error| error.to_string())?;
                    let source = Utf8PathBuf::from_path_buf(entry.path())
                        .map_err(|_| "Legacy data path is not UTF-8")?;
                    let name = source.file_name().ok_or("Legacy data entry has no name")?;
                    if moves
                        .iter()
                        .any(|(_, destination): &(Utf8PathBuf, Utf8PathBuf)| {
                            destination.file_name() == Some(name)
                        })
                    {
                        return Err(format!(
                            "旧保存先に同名のデータがあります。自動統合せずに中止しました: {source}"
                        ));
                    }
                    let destination = paths.root.join(name);
                    moves.push((source, destination));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("旧保存先を確認できません: {root}: {error}")),
        }
    }
    if let Some(webview) = webview {
        assert_regular_tree(webview)?;
        if webview.exists() {
            moves.push((webview.to_owned(), paths.cache.join("webview")));
        }
    }
    if moves.is_empty() {
        if !marker.exists() {
            write_marker(&marker, "fresh")?;
        }
        return Ok(());
    }
    if marker.exists() || paths.root.exists() {
        return Err(format!(
            "Desktopの新旧両方の保存先にデータがあります。古いデータを再読込・統合せずに中止しました。新保存先: {}。旧保存先: {}。旧版を終了し、保存先を確認してください。",
            paths.root,
            moves
                .iter()
                .map(|(source, _)| source.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let profiles = [
        legacy_profile.clone(),
        (paths.config.join("config.toml"), paths.data.clone()),
    ];
    let mut targets = legacy_roots.to_vec();
    targets.push(paths.root.clone());
    if let Some(webview) = webview {
        targets.push(webview.to_owned());
    }
    let mut guard = acquire(&profiles, &targets)?;
    assert_regular_tree(&paths.root)?;
    let parent = paths
        .root
        .parent()
        .ok_or("Desktop data directory has no parent")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    fs::create_dir_all(pending.parent().unwrap()).map_err(|error| error.to_string())?;
    let mut receipt = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&pending)
        .map_err(|error| error.to_string())?;
    receipt
        .write_all(
            &serde_json::to_vec(&json!({"version":1,"destination":paths.root,"moves":moves}))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    receipt.sync_all().map_err(|error| error.to_string())?;
    drop(receipt);
    let mut moved = Vec::new();
    let mut reference_undo = None;
    let mut published = false;
    let mut created_destination = false;
    let mut released_database = false;
    let old_database = legacy_profile.1.join("moyai.sqlite3");
    let database_was_absent = !old_database.exists();
    let old_data_was_absent = !legacy_profile.1.exists();
    let mut blockers = Vec::new();
    let result: Result<(), MigrationFailure> = (|| {
        fs::create_dir(&paths.root).map_err(|error| error.to_string())?;
        created_destination = true;
        if database_was_absent {
            fs::create_dir_all(&legacy_profile.1).map_err(|error| error.to_string())?;
            blockers.push(SourceBlocker::new(&old_database)?);
        }
        for (source, destination) in &moves {
            move_tree(
                source,
                destination,
                &old_database,
                &mut blockers,
                &mut moved,
            )?;
        }
        guard.release_database_for_migration();
        released_database = true;
        reference_undo = Some(relocate(&paths.root, &paths.root, &moves)?);
        guard.relocate(&moves).map_err(|error| error.message)?;
        published = true;
        write_marker(&marker, "migrated")?;
        blockers.clear();
        for root in legacy_roots {
            remove_empty_tree(root)?;
        }
        if let Some(webview) = webview {
            remove_empty_tree(webview)?;
        }
        fs::remove_file(&pending).map_err(|error| error.to_string())?;
        Ok(())
    })();
    if let Err(failure) = result {
        if matches!(
            &failure,
            MigrationFailure::References(references::RelocationFailure::RecoveryRequired { .. })
        ) {
            return Err(format!(
                "参照の復元を確認できないため、旧保存先への移動を行っていません。移動先のデータと移行記録を保持しました。保存データを削除せずに確認してください。移動先: {}。旧保存先: {}。移行記録: {pending}。原因: {failure}",
                paths.root,
                legacy_roots
                    .iter()
                    .map(|path| path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if published {
            return Err(format!(
                "データの移動と参照更新は完了しましたが、移行記録の確定を完了できませんでした。データを削除せずに確認してください: {pending}: {failure}"
            ));
        }
        let mut rollback_errors = Vec::new();
        if let Some(undo) = reference_undo {
            if let Err(error) = references::restore(&paths.root, undo) {
                rollback_errors.push(error);
            }
        }
        blockers.clear();
        if rollback_errors.is_empty() {
            for (source, destination) in moved.into_iter().rev() {
                // SQLite may checkpoint and remove its own WAL/SHM when the
                // moved database is opened to update path metadata.
                if released_database
                    && !destination.exists()
                    && [format!("{old_database}-wal"), format!("{old_database}-shm")]
                        .iter()
                        .any(|sidecar| source == Utf8Path::new(sidecar))
                {
                    continue;
                }
                if let Err(error) = rename_noreplace(&destination, &source) {
                    rollback_errors.push(format!("{destination} -> {source}: {error}"));
                }
            }
        }
        if rollback_errors.is_empty() {
            if created_destination {
                if let Err(error) = remove_empty_tree(&paths.root) {
                    rollback_errors.push(error);
                }
            }
            if old_data_was_absent {
                if let Err(error) = remove_empty_tree(&legacy_profile.1) {
                    rollback_errors.push(error);
                }
            }
            if rollback_errors.is_empty() {
                fs::remove_file(&pending).map_err(|error| error.to_string())?;
                return Err(format!("移行を中止し、旧保存先へ戻しました: {failure}"));
            }
        }
        return Err(format!(
            "移行の中断後、旧保存先への復元を完了できませんでした。データを削除しないでください。移行記録: {pending}。原因: {failure}。復元: {}",
            rollback_errors.join("; ")
        ));
    }
    Ok(())
}

struct SourceBlocker {
    path: Utf8PathBuf,
    file: Option<std::fs::File>,
}

impl SourceBlocker {
    fn new(path: &Utf8Path) -> Result<Self, String> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::{
                Foundation::{GENERIC_READ, GENERIC_WRITE},
                Storage::FileSystem::{DELETE, FILE_FLAG_DELETE_ON_CLOSE},
            };
            options
                .share_mode(0)
                .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
                .custom_flags(FILE_FLAG_DELETE_ON_CLOSE);
        }
        let file = options.open(path).map_err(|error| {
            format!("旧保存先の再作成を排除できません。移行を中止します: {path}: {error}")
        })?;
        Ok(Self {
            path: path.to_owned(),
            file: Some(file),
        })
    }
}

impl Drop for SourceBlocker {
    fn drop(&mut self) {
        self.file.take();
        #[cfg(not(windows))]
        let _ = fs::remove_file(&self.path);
    }
}

fn move_tree(
    source: &Utf8Path,
    destination: &Utf8Path,
    database: &Utf8Path,
    blockers: &mut Vec<SourceBlocker>,
    moved: &mut Vec<(Utf8PathBuf, Utf8PathBuf)>,
) -> Result<(), String> {
    if blockers.iter().any(|blocker| blocker.path == source) {
        return Ok(());
    }
    assert_regular_tree(source)?;
    if destination.exists() {
        return Err(format!(
            "移行先に同名のデータが存在します。上書きしません: {destination}"
        ));
    }
    if source.is_dir() {
        fs::create_dir_all(
            destination
                .parent()
                .ok_or("Migration destination has no parent")?,
        )
        .map_err(|error| error.to_string())?;
        fs::create_dir(destination).map_err(|error| error.to_string())?;
        let children = fs::read_dir(source)
            .map_err(|error| error.to_string())?
            .map(|entry| {
                Utf8PathBuf::from_path_buf(entry.map_err(|error| error.to_string())?.path())
                    .map_err(|_| "Migration source is not UTF-8".to_string())
            })
            .collect::<Result<Vec<_>, String>>()?;
        for child in children {
            move_tree(
                &child,
                &destination.join(child.file_name().ok_or("Migration source has no name")?),
                database,
                blockers,
                moved,
            )?;
        }
    } else {
        fs::create_dir_all(
            destination
                .parent()
                .ok_or("Migration destination has no parent")?,
        )
        .map_err(|error| error.to_string())?;
        rename_noreplace(source, destination)
            .map_err(|error| format!("保存データを移動できません: {source}: {error}"))?;
        moved.push((source.to_owned(), destination.to_owned()));
        if source == database {
            blockers.push(SourceBlocker::new(source)?);
        }
    }
    Ok(())
}

fn rename_noreplace(source: &Utf8Path, destination: &Utf8Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        let source = source
            .as_str()
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let destination = destination
            .as_str()
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                0,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        if destination.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "Migration destination already exists",
            ));
        }
        fs::rename(source, destination)
    }
}

fn remove_empty_tree(path: &Utf8Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    assert_regular_tree(path)?;
    for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let child =
            Utf8PathBuf::from_path_buf(entry.path()).map_err(|_| "Staging path is not UTF-8")?;
        if !child.is_dir() {
            return Err(format!(
                "移行先に未確認のファイルが残っています。削除していません: {child}"
            ));
        }
        remove_empty_tree(&child)?;
    }
    fs::remove_dir(path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn migrated_webview_normal_profile_survives_cold_restart_and_recreated_old_cache_is_preserved()
    {
        let temp = crate::user_data::test_directory();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let paths = DesktopPaths::at(root.join("new/desktop"));
        let old = root.join("old/moyai");
        let webview = root.join("old/local.moyai.desktop");
        fs::create_dir_all(&webview).unwrap();
        fs::create_dir_all(root.join("machine")).unwrap();
        fs::write(webview.join("Local State"), b"saved browser state").unwrap();
        let profile = (old.join("config/config.toml"), old.join("data"));
        prepare_with(
            &paths,
            &[old.clone()],
            &profile,
            Some(&webview),
            |profiles, targets| {
                ProfileMaintenanceGuard::for_test(&root.join("machine"), profiles, targets)
                    .map_err(|error| error.message)
            },
        )
        .unwrap();
        let current = crate::user_data::resolve_webview_directory(&paths, None).unwrap();
        assert_eq!(current, paths.cache.join("webview"));
        assert_eq!(
            fs::read(current.join("Local State")).unwrap(),
            b"saved browser state"
        );
        // The normal consumer has no WEBVIEW2 override. Its next write must stay
        // in the migrated profile so the following ordinary startup can reopen it.
        fs::write(current.join("new storage"), b"normal launch state").unwrap();
        prepare_with(&paths, &[old.clone()], &profile, Some(&webview), |_, _| {
            panic!("A cold restart must not attempt another migration")
        })
        .unwrap();
        assert!(!webview.exists());
        assert_eq!(
            fs::read(current.join("new storage")).unwrap(),
            b"normal launch state"
        );
        let explicit = root.join("isolated/webview");
        assert_eq!(
            crate::user_data::resolve_webview_directory(
                &paths,
                Some(explicit.clone().into_std_path_buf().into_os_string())
            )
            .unwrap(),
            explicit
        );

        // Data created by an older/broken consumer is a real conflict even when
        // migration has a completion marker. Never merge or delete its storage.
        fs::create_dir_all(&webview).unwrap();
        fs::write(webview.join("Local State"), b"recreated browser state").unwrap();
        let failure = prepare_with(&paths, &[old], &profile, Some(&webview), |_, _| {
            panic!("A conflicting browser profile must stop before maintenance")
        })
        .unwrap_err();
        assert!(failure.contains(webview.as_str()));
        assert_eq!(
            fs::read(webview.join("Local State")).unwrap(),
            b"recreated browser state"
        );
        assert_eq!(
            fs::read(current.join("Local State")).unwrap(),
            b"saved browser state"
        );
        assert!(!marker_paths(&paths).unwrap().1.exists());
    }

    #[tokio::test]
    async fn migrated_projects_keep_canonical_identity_and_history_after_cold_bootstrap() {
        use crate::app::AppBootstrap;
        use crate::config::ResolvedConfig;
        use crate::protocol::{ModeKind, ProtocolEventStore};
        use crate::session::{
            NewSession, ProjectRepository, SessionProviderConnection, SessionRepository,
            SessionSelector, SessionStartRequest,
        };
        use crate::storage::{SqliteStore, StoragePaths, StoreBundle};
        use crate::workspace::{VcsKind, WorkspaceDiscovery};

        for git in [false, true] {
            let temp = crate::user_data::test_directory();
            let root = Utf8Path::from_path(temp.path()).unwrap();
            let paths = DesktopPaths::at(root.join("new/desktop"));
            let old = root.join("old/moyai");
            let old_workspace = old.join("data/quick-chat-workspace");
            fs::create_dir_all(old.join("config")).unwrap();
            fs::create_dir_all(&old_workspace).unwrap();
            fs::create_dir_all(root.join("machine")).unwrap();
            fs::write(old.join("config/config.toml"), "").unwrap();
            fs::write(
                old_workspace.join("saved-result.txt"),
                "preserved workspace file",
            )
            .unwrap();
            if git {
                fs::create_dir(old_workspace.join(".git")).unwrap();
            }
            let old_paths = StoragePaths {
                data_dir: old.join("data"),
                database_path: old.join("data/moyai.sqlite3"),
                truncation_dir: old.join("data/truncation"),
            };
            let sqlite = SqliteStore::open(&old_paths).unwrap();
            sqlite.migrate().unwrap();
            let store = StoreBundle::new(sqlite);
            let workspace = if git {
                WorkspaceDiscovery::discover(&old_workspace, &ResolvedConfig::default()).unwrap()
            } else {
                WorkspaceDiscovery::discover_fixed_root(&old_workspace, &ResolvedConfig::default())
                    .unwrap()
            };
            let project_id = workspace.project_id;
            store
                .project_repo()
                .upsert_project(
                    project_id,
                    &workspace.root,
                    "saved project",
                    if git { "git" } else { "none" },
                )
                .await
                .unwrap();
            let saved = store
                .session_repo()
                .create_session(NewSession {
                    project_id,
                    title: "saved chat".into(),
                    cwd: workspace.cwd,
                    model: "model".into(),
                    base_url: "http://127.0.0.1:1234".into(),
                    access_mode: crate::config::AccessMode::Default,
                    provider_connection: None,
                })
                .await
                .unwrap();
            store
                .protocol_event_store()
                .set_collaboration_mode(saved.id, ModeKind::Plan)
                .unwrap();
            let history = serde_json::to_value(
                store
                    .protocol_event_store()
                    .list_history_items_for_session(saved.id)
                    .unwrap(),
            )
            .unwrap();
            assert!(!history.as_array().unwrap().is_empty());
            drop(store);
            prepare_with(
                &paths,
                &[old.clone()],
                &(old.join("config/config.toml"), old.join("data")),
                None,
                |profiles, targets| {
                    ProfileMaintenanceGuard::for_test(&root.join("machine"), profiles, targets)
                        .map_err(|error| error.message)
                },
            )
            .unwrap();

            let new_workspace = paths.data.join("quick-chat-workspace");
            let new_paths = StoragePaths {
                data_dir: paths.data.clone(),
                database_path: paths.data.join("moyai.sqlite3"),
                truncation_dir: paths.data.join("truncation"),
            };
            let sqlite = SqliteStore::open(&new_paths).unwrap();
            sqlite.migrate().unwrap();
            let store = StoreBundle::new(sqlite);
            let config = ResolvedConfig::default();
            let app = AppBootstrap::rebuild_for_directory_as_workspace_root_with_config(
                &new_workspace,
                store,
                config,
            )
            .await
            .unwrap();
            assert_eq!(app.workspace.project_id, project_id);
            assert_eq!(
                app.workspace.vcs,
                if git { VcsKind::Git } else { VcsKind::None }
            );
            assert_eq!(
                app.store
                    .project_repo()
                    .list_projects(10)
                    .await
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                fs::read_to_string(new_workspace.join("saved-result.txt")).unwrap(),
                "preserved workspace file"
            );
            let current = app.session_service.get_session(saved.id).await.unwrap();
            assert_eq!(current.project_id, project_id);
            assert!(crate::workspace::PathGuard::same_path_identity(
                &current.cwd,
                &new_workspace
            ));
            assert_eq!(
                serde_json::to_value(
                    app.store
                        .protocol_event_store()
                        .list_history_items_for_session(saved.id)
                        .unwrap()
                )
                .unwrap(),
                history
            );
            for selector in [
                SessionSelector::ById(saved.id),
                SessionSelector::Latest,
                SessionSelector::New,
            ] {
                let creates_session = matches!(selector, SessionSelector::New);
                let resumed = app
                    .session_service
                    .start_or_resume(
                        SessionStartRequest {
                            selector,
                            title: None,
                            cwd: app.workspace.cwd.clone(),
                            model: app.config.model.model.clone(),
                            base_url: app.config.model.base_url.clone(),
                            access_mode: app.config.permissions.access_mode,
                            provider_connection: Some(
                                SessionProviderConnection::from_model_config(&app.config.model),
                            ),
                        },
                        app.workspace.clone(),
                    )
                    .await
                    .unwrap();
                assert_eq!(resumed.session.project_id, project_id);
                if !creates_session {
                    assert_eq!(resumed.session.id, saved.id);
                }
            }
        }
    }

    #[test]
    fn failed_reference_restore_preserves_moved_data_pending_and_blocks_both_startup_paths() {
        let temp = crate::user_data::test_directory();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let paths = DesktopPaths::at(root.join("new/desktop"));
        let old = root.join("old/moyai");
        let registry = root.join("machine");
        for directory in [old.join("config"), old.join("data"), registry.clone()] {
            fs::create_dir_all(directory).unwrap();
        }
        let original = format!(
            "[shell]\nprogram = '{}'\n",
            old.join("config/bin/shell.exe")
        );
        fs::write(old.join("config/config.toml"), &original).unwrap();
        fs::write(old.join("config/identity.key"), b"fixture-only-key").unwrap();
        fs::write(old.join("data/runner-operations.json"), b"invalid JSON").unwrap();
        let profile = (old.join("config/config.toml"), old.join("data"));
        let mut persistence_attempts = 0;
        let failure = prepare_with_relocation(
            &paths,
            &[old.clone()],
            &profile,
            None,
            |profiles, targets| {
                ProfileMaintenanceGuard::for_test(&registry, profiles, targets)
                    .map_err(|error| error.message)
            },
            |physical, destination, mappings| {
                references::relocate_with_writer(physical, destination, mappings, |path, bytes| {
                    persistence_attempts += 1;
                    // The first replacement changes a real file. Fail the later
                    // restoration without blocking filesystem rename: flattening
                    // this failure would otherwise report a successful old-root
                    // rollback while leaving its references changed.
                    if persistence_attempts == 2 {
                        return Err("injected restoration persistence failure".into());
                    }
                    references::write_atomic(path, bytes)
                })
            },
        )
        .unwrap_err();
        assert_eq!(persistence_attempts, 2);
        assert!(failure.contains("参照の復元を確認できない"));
        assert!(!failure.contains("旧保存先へ戻しました"));
        let (marker, pending) = marker_paths(&paths).unwrap();
        assert!(pending.exists());
        assert!(!marker.exists());
        assert!(!old.join("config/config.toml").exists());
        assert!(!old.join("config/identity.key").exists());
        assert_eq!(
            fs::read(paths.config.join("identity.key")).unwrap(),
            b"fixture-only-key"
        );
        let relocated: toml::Value =
            toml::from_str(&fs::read_to_string(paths.config.join("config.toml")).unwrap()).unwrap();
        assert_eq!(
            Utf8Path::new(relocated["shell"]["program"].as_str().unwrap()),
            paths.config.join("bin/shell.exe")
        );
        assert_ne!(
            fs::read_to_string(paths.config.join("config.toml")).unwrap(),
            original
        );
        assert!(
            prepare_with(&paths, &[old.clone()], &profile, None, |_, _| panic!(
                "Pending recovery must stop before acquiring migration authority"
            ))
            .is_err()
        );
        for selected in [
            profile.0,
            paths.config.join("config.toml"),
            paths.data.clone(),
        ] {
            assert!(check_explicit_profile(&paths, &[old.clone()], None, &[selected]).is_err());
        }
        assert!(check_cleanup_ready(&paths).is_err());
        assert!(pending.exists());
    }

    #[test]
    fn explicit_known_roots_cannot_bypass_pending_or_retired_profile_guards() {
        let temp = crate::user_data::test_directory();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let paths = DesktopPaths::at(root.join("new/desktop"));
        let old = root.join("old/moyai");
        let (_, pending) = marker_paths(&paths).unwrap();
        fs::create_dir_all(pending.parent().unwrap()).unwrap();
        fs::write(&pending, "{}").unwrap();
        for selected in [
            paths.config.join("config.toml"),
            old.join("data"),
            root.join("new/desktop/../desktop/data"),
        ] {
            assert!(check_explicit_profile(&paths, &[old.clone()], None, &[selected]).is_err());
        }
        assert!(
            check_explicit_profile(&paths, &[old.clone()], None, &[root.join("custom/data")])
                .is_ok()
        );
        fs::remove_file(pending).unwrap();
        mark_reset(&paths).unwrap();
        assert!(
            check_explicit_profile(
                &paths,
                &[old.clone()],
                None,
                &[old.join("config/config.toml")]
            )
            .is_err()
        );
        assert!(check_explicit_profile(&paths, &[old], None, &[paths.data.clone()]).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn file_move_keeps_open_handle_and_source_blocker_excludes_recreation_without_overwrite() {
        use std::io::{Read, Seek, SeekFrom};
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE;
        let temp = crate::user_data::test_directory();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let source = root.join("source.db");
        let destination = root.join("destination.db");
        fs::write(&source, b"fixture data").unwrap();
        let mut handle = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_DELETE)
            .open(&source)
            .unwrap();
        rename_noreplace(&source, &destination).unwrap();
        let blocker = SourceBlocker::new(&source).unwrap();
        assert!(std::fs::File::open(&source).is_err());
        assert!(
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&source)
                .is_err()
        );
        handle.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = Vec::new();
        handle.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"fixture data");
        drop(handle);
        drop(blocker);
        assert!(!source.exists());
        fs::write(&source, b"new source").unwrap();
        assert!(rename_noreplace(&source, &destination).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"new source");
        assert_eq!(fs::read(destination).unwrap(), b"fixture data");
    }

    #[test]
    fn stopped_profile_moves_wal_credentials_and_webview_without_copying_or_touching_hub() {
        let temp = crate::user_data::test_directory();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let paths = DesktopPaths::at(root.join("new/desktop"));
        let old = root.join("Roaming/midi-ai-labs/moyai");
        let old_local = root.join("Local/midi-ai-labs/moyai");
        let webview = root.join("Local/local.moyai.desktop");
        let hub = root.join("new/hub");
        let registry = root.join("machine");
        for directory in [
            old.join("config/device-network"),
            old.join("data"),
            old_local.join("cache"),
            webview.clone(),
            hub.clone(),
            registry.clone(),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::write(
            old.join("config/config.toml"),
            "[model]\nbase_url = 'http://127.0.0.1:1234'\n",
        )
        .unwrap();
        let identity = b"{\"private_key\":\"fixture-only-secret\",\"device_id\":\"device\"}";
        fs::write(old.join("config/device-network/identity.json"), identity).unwrap();
        fs::write(old_local.join("cache/keep.bin"), b"cache").unwrap();
        fs::write(webview.join("browser.bin"), b"browser").unwrap();
        fs::write(hub.join("keep.bin"), b"Hub state").unwrap();
        let db = rusqlite::Connection::open(old.join("data/moyai.sqlite3")).unwrap();
        db.pragma_update(None, "journal_mode", "WAL").unwrap();
        db.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
        db.execute_batch("CREATE TABLE projects(root_path TEXT);")
            .unwrap();
        db.execute(
            "INSERT INTO projects VALUES(?1)",
            [old.join("data/quick-chat-workspace").as_str()],
        )
        .unwrap();
        drop(db);
        assert!(
            old.join("data/moyai.sqlite3-wal").exists(),
            "Fixture must leave a real closed WAL to test relocation"
        );
        let profile = (old.join("config/config.toml"), old.join("data"));
        prepare_with(
            &paths,
            &[old.clone(), old_local.clone()],
            &profile,
            Some(&webview),
            |profiles, targets| {
                ProfileMaintenanceGuard::for_test(&registry, profiles, targets)
                    .map_err(|error| error.message)
            },
        )
        .unwrap();
        assert!(!old.exists());
        assert!(!old_local.exists());
        assert!(!webview.exists());
        assert_eq!(
            fs::read(paths.config.join("device-network/identity.json")).unwrap(),
            identity
        );
        assert_eq!(fs::read(paths.cache.join("keep.bin")).unwrap(), b"cache");
        assert_eq!(
            fs::read(paths.cache.join("webview/browser.bin")).unwrap(),
            b"browser"
        );
        assert_eq!(fs::read(hub.join("keep.bin")).unwrap(), b"Hub state");
        let db = rusqlite::Connection::open(paths.data.join("moyai.sqlite3")).unwrap();
        assert_eq!(
            db.query_row("SELECT root_path FROM projects", [], |row| row
                .get::<_, String>(0))
                .unwrap(),
            paths.data.join("quick-chat-workspace").as_str()
        );
        drop(db);
        assert!(!marker_paths(&paths).unwrap().1.exists());
        prepare_with(
            &paths,
            &[old, old_local],
            &profile,
            Some(&webview),
            |_, _| panic!("A finished migration must not acquire relocation ownership again"),
        )
        .unwrap();
    }

    #[test]
    fn failed_reference_update_restores_original_credentials_and_directories() {
        let temp = crate::user_data::test_directory();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let paths = DesktopPaths::at(root.join("new/desktop"));
        let old = root.join("old/moyai");
        fs::create_dir_all(old.join("config")).unwrap();
        fs::create_dir_all(root.join("machine")).unwrap();
        fs::write(old.join("config/config.toml"), "this is not TOML = [").unwrap();
        fs::write(old.join("config/identity.key"), b"fixture-only-key").unwrap();
        let profile = (old.join("config/config.toml"), old.join("data"));
        let error = prepare_with(
            &paths,
            &[old.clone()],
            &profile,
            None,
            |profiles, targets| {
                ProfileMaintenanceGuard::for_test(&root.join("machine"), profiles, targets)
                    .map_err(|error| error.message)
            },
        )
        .unwrap_err();
        assert!(error.contains("旧保存先へ戻しました"));
        assert!(!paths.root.exists());
        assert!(!marker_paths(&paths).unwrap().1.exists());
        assert_eq!(
            fs::read(old.join("config/identity.key")).unwrap(),
            b"fixture-only-key"
        );
    }

    #[test]
    fn new_and_legacy_data_are_never_merged() {
        let temp = crate::user_data::test_directory();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let paths = DesktopPaths::at(root.join("new/desktop"));
        let old = root.join("old/moyai");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&paths.root).unwrap();
        fs::write(old.join("saved.txt"), "old").unwrap();
        fs::write(paths.root.join("saved.txt"), "new").unwrap();
        assert!(
            prepare_with(
                &paths,
                &[old.clone()],
                &(old.join("config/config.toml"), old.join("data")),
                None,
                |_, _| panic!("Conflict must be detected before taking maintenance ownership")
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(old.join("saved.txt")).unwrap(), "old");
        assert_eq!(
            fs::read_to_string(paths.root.join("saved.txt")).unwrap(),
            "new"
        );
    }

    #[test]
    fn reset_marker_blocks_later_old_data_resurrection() {
        let temp = crate::user_data::test_directory();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        let paths = DesktopPaths::at(root.join("new/desktop"));
        mark_reset(&paths).unwrap();
        let old = root.join("old/moyai");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("config.toml"), "do not import").unwrap();
        assert!(
            prepare_with(
                &paths,
                &[old.clone()],
                &(old.join("config.toml"), old.join("data")),
                None,
                |_, _| panic!("Must not import retired old data")
            )
            .is_err()
        );
        assert!(!paths.root.exists());
    }
}
