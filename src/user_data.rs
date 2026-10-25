//! Product-owned user data paths. Discovering a path never migrates or creates data.
use camino::{Utf8Path, Utf8PathBuf};
#[cfg(windows)]
use directories_next::BaseDirs;
use directories_next::ProjectDirs;

mod migration;

#[derive(Clone, Debug)]
pub struct DesktopPaths {
    pub root: Utf8PathBuf,
    pub config: Utf8PathBuf,
    pub data: Utf8PathBuf,
    pub cache: Utf8PathBuf,
}

impl DesktopPaths {
    pub fn discover() -> Result<Self, String> {
        #[cfg(windows)]
        {
            let base = BaseDirs::new().ok_or("Windows user directories are unavailable")?;
            let local = utf8(base.data_local_dir())?;
            Ok(Self::at(local.join("midi-ai-labs/moyai-data/desktop")))
        }
        #[cfg(not(windows))]
        {
            let dirs = legacy_dirs()?;
            Ok(Self {
                root: utf8(dirs.data_dir())?,
                config: utf8(dirs.config_dir())?,
                data: utf8(dirs.data_dir())?,
                cache: utf8(dirs.cache_dir())?,
            })
        }
    }

    pub(crate) fn at(root: Utf8PathBuf) -> Self {
        Self {
            config: root.join("config"),
            data: root.join("data"),
            cache: root.join("cache"),
            root,
        }
    }
}

fn utf8(path: &std::path::Path) -> Result<Utf8PathBuf, String> {
    Utf8PathBuf::from_path_buf(path.to_owned()).map_err(|_| "User data path is not UTF-8".into())
}

fn legacy_dirs() -> Result<ProjectDirs, String> {
    ProjectDirs::from("net", "midi-ai-labs", "moyai")
        .ok_or_else(|| "Legacy moyAI user directories are unavailable".into())
}

pub(crate) fn legacy_roots() -> Result<Vec<Utf8PathBuf>, String> {
    let dirs = legacy_dirs()?;
    let mut roots = Vec::new();
    for path in [dirs.config_dir(), dirs.data_dir(), dirs.cache_dir()] {
        let path = utf8(path)?;
        #[cfg(windows)]
        let root = path
            .ancestors()
            .find(|ancestor| {
                ancestor
                    .file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case("moyai"))
            })
            .ok_or("Legacy moyAI directory cannot be identified")?
            .to_owned();
        #[cfg(not(windows))]
        let root = path;
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    Ok(roots)
}

pub(crate) fn legacy_profile() -> Result<(Utf8PathBuf, Utf8PathBuf), String> {
    let dirs = legacy_dirs()?;
    Ok((
        utf8(dirs.config_dir())?.join("config.toml"),
        utf8(dirs.data_dir())?,
    ))
}

pub(crate) fn legacy_webview() -> Result<Option<Utf8PathBuf>, String> {
    #[cfg(windows)]
    {
        let base = BaseDirs::new().ok_or("Windows user directories are unavailable")?;
        let config: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
            .map_err(|error| error.to_string())?;
        let identifier = config["identifier"]
            .as_str()
            .ok_or("Desktop identifier is missing")?;
        Ok(Some(utf8(base.data_local_dir())?.join(identifier)))
    }
    #[cfg(not(windows))]
    Ok(None)
}

/// Called by executable startup, before creating either the Desktop or Runner lease.
pub fn prepare_default_profile() -> Result<(), String> {
    #[cfg(all(windows, not(test)))]
    {
        let paths = DesktopPaths::discover()?;
        let legacy = legacy_roots()?;
        let webview = legacy_webview()?;
        if [
            "MOYAI_CONFIG_PATH",
            "MOYAI_DATA_DIR",
            "MOYAI_DESKTOP_PREFS_PATH",
        ]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
        {
            let selected = [
                crate::config::loader::global_config_path().map_err(|error| error.to_string())?,
                crate::storage::StoragePaths::discover()
                    .map_err(|error| error.to_string())?
                    .data_dir,
            ];
            migration::check_explicit_profile(&paths, &legacy, webview.as_deref(), &selected)?;
            return Ok(());
        }
        migration::prepare(&paths, &legacy, &legacy_profile()?, webview.as_deref())?;
    }
    Ok(())
}

pub fn prepare_desktop_profile() -> Result<(), String> {
    prepare_default_profile()?;
    #[cfg(all(windows, not(test)))]
    {
        let paths = DesktopPaths::discover()?;
        let selected = [
            std::env::var("MOYAI_DESKTOP_PREFS_PATH")
                .map(Utf8PathBuf::from)
                .unwrap_or_else(|_| paths.config.join("desktop.toml")),
            std::env::var("WEBVIEW2_USER_DATA_FOLDER")
                .map(Utf8PathBuf::from)
                .unwrap_or_else(|_| paths.cache.join("webview")),
        ];
        migration::check_explicit_profile(
            &paths,
            &legacy_roots()?,
            legacy_webview()?.as_deref(),
            &selected,
        )?;
    }
    Ok(())
}

/// The compatibility cleanup executable uses this same product-specific scope.
pub fn cleanup_targets() -> Result<Vec<Utf8PathBuf>, String> {
    let paths = DesktopPaths::discover()?;
    migration::check_cleanup_ready(&paths)?;
    cleanup_profiles()?;
    let mut targets = vec![paths.root.clone()];
    for root in legacy_roots()? {
        if !targets.contains(&root) {
            targets.push(root);
        }
    }
    if let Some(webview) = legacy_webview()? {
        targets.push(webview);
    }
    let config = crate::config::loader::global_config_path().map_err(|error| error.to_string())?;
    let data = crate::storage::StoragePaths::discover()
        .map_err(|error| error.to_string())?
        .data_dir;
    let preferences = std::env::var("MOYAI_DESKTOP_PREFS_PATH")
        .map(Utf8PathBuf::from)
        .unwrap_or_else(|_| paths.config.join("desktop.toml"));
    for path in [&config, &data, &preferences] {
        if !targets.iter().any(|target| path.starts_with(target)) {
            return Err(format!(
                "独自の保存先 {path} は既定の初期化範囲外です。保存先の指定を確認してください。データは削除していません。"
            ));
        }
    }
    Ok(targets)
}

pub fn finish_cleanup() -> Result<(), String> {
    migration::mark_reset(&DesktopPaths::discover()?)
}

pub(crate) fn assert_cleanup_ready() -> Result<(), String> {
    migration::check_cleanup_ready(&DesktopPaths::discover()?)
}

pub(crate) fn assert_store_access_allowed(data: &Utf8Path) -> Result<(), String> {
    #[cfg(all(windows, not(test)))]
    {
        migration::check_explicit_profile(
            &DesktopPaths::discover()?,
            &legacy_roots()?,
            legacy_webview()?.as_deref(),
            &[data.to_owned()],
        )?;
    }
    #[cfg(any(not(windows), test))]
    let _ = data;
    Ok(())
}

pub(crate) fn internal_desktop_project_roots(data: &Utf8Path) -> Vec<Utf8PathBuf> {
    [
        "quick-chat-workspace",
        "desktop-workspace",
        "desktop-workspace-after-delete",
        "desktop-workspace-after-delete-2",
    ]
    .into_iter()
    .map(|name| data.join(name))
    .collect()
}

#[cfg(test)]
pub(crate) fn assert_store_access_at(
    data: &Utf8Path,
    paths: &DesktopPaths,
    legacy: &[Utf8PathBuf],
) -> Result<(), String> {
    migration::check_explicit_profile(paths, legacy, None, &[data.to_owned()])
}

#[cfg(test)]
pub(crate) fn test_directory() -> tempfile::TempDir {
    tempfile::tempdir().expect("isolated user-data fixture")
}

pub(crate) fn cleanup_profiles() -> Result<Vec<(Utf8PathBuf, Utf8PathBuf)>, String> {
    let current = (
        crate::config::loader::global_config_path().map_err(|error| error.to_string())?,
        crate::storage::StoragePaths::discover()
            .map_err(|error| error.to_string())?
            .data_dir,
    );
    let paths = DesktopPaths::discover()?;
    let default = (paths.config.join("config.toml"), paths.data);
    let legacy = legacy_profile()?;
    if current != default && current != legacy {
        return Err("独自のconfig/data保存先を指定した状態では既定データの初期化を実行できません。指定した保存先と旧登録を確認してください。削除していません。".into());
    }
    let mut profiles = vec![default];
    if !profiles.contains(&current) {
        profiles.push(current);
    }
    if !profiles.contains(&legacy) {
        profiles.push(legacy);
    }
    Ok(profiles)
}

/// Reject junctions and links before a recursive operation. Never follow a link
/// into a Hub, project, or another account's directory.
pub fn assert_regular_tree(path: &Utf8Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if link_like(&metadata) => {
                return Err(format!(
                    "リンクまたはjunctionを含む保存先は処理できません: {ancestor}"
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("保存先を確認できません: {ancestor}: {error}")),
        }
    }
    let mut pending = vec![path.to_owned()];
    while let Some(path) = pending.pop() {
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("保存先を確認できません: {path}: {error}")),
        };
        if link_like(&metadata) {
            return Err(format!(
                "リンクまたはjunctionを含む保存先は処理できません: {path}"
            ));
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(&path).map_err(|error| error.to_string())? {
                pending.push(utf8(&entry.map_err(|error| error.to_string())?.path())?);
            }
        }
    }
    Ok(())
}

fn link_like(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
    }
    #[cfg(not(windows))]
    false
}
