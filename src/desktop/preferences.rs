use std::fs;
use std::io::Write;

use camino::{Utf8Path, Utf8PathBuf};
use directories_next::ProjectDirs;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

const DESKTOP_PREFS_ENV: &str = "MOYAI_DESKTOP_PREFS_PATH";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DesktopPreferences {
    pub last_workspace: Option<Utf8PathBuf>,
    pub window_opacity_percent: Option<i32>,
    #[serde(default)]
    pub deleted_project_roots: Vec<Utf8PathBuf>,
}

impl DesktopPreferences {
    pub fn load_or_default() -> Self {
        Self::load().unwrap_or_default()
    }

    pub fn load() -> Result<Self, String> {
        let path = preferences_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path).map_err(|error| error.to_string())?;
        toml::from_str(&text).map_err(|error| error.to_string())
    }

    pub fn save(&self) -> Result<(), String> {
        let path = preferences_path()?;
        self.save_to_path(&path)
    }

    fn save_to_path(&self, path: &Utf8Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|error| error.to_string())?;
        persist_desktop_preferences_tempfile(path, &text)
    }

    pub fn mark_project_deleted(&mut self, root: &Utf8Path) {
        if !self.deleted_project_roots.iter().any(|path| path == root) {
            self.deleted_project_roots.push(root.to_path_buf());
        }
        if self
            .last_workspace
            .as_ref()
            .is_some_and(|workspace| workspace == root)
        {
            self.last_workspace = None;
        }
    }

    pub fn unmark_project_deleted(&mut self, root: &Utf8Path) {
        self.deleted_project_roots.retain(|path| path != root);
    }

    pub fn is_project_deleted(&self, root: &Utf8Path) -> bool {
        self.deleted_project_roots.iter().any(|path| path == root)
    }
}

fn persist_desktop_preferences_tempfile(path: &Utf8Path, text: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("desktop preferences path `{path}` has no parent directory"))?;
    let mut temp =
        NamedTempFile::new_in(parent.as_std_path()).map_err(|error| error.to_string())?;
    temp.write_all(text.as_bytes())
        .map_err(|error| error.to_string())?;
    temp.as_file_mut()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temp.persist(path.as_std_path())
        .map(|_| ())
        .map_err(|error| error.error.to_string())
}

pub(crate) fn desktop_preferences_save_atomic_commit_fixture_passes() -> bool {
    let Ok(temp_dir) = tempfile::tempdir() else {
        return false;
    };
    let Ok(path) = Utf8PathBuf::from_path_buf(temp_dir.path().join("desktop.toml")) else {
        return false;
    };
    if fs::write(&path, "last_workspace = \"C:/old\"\n").is_err() {
        return false;
    }
    let root = Utf8PathBuf::from("C:/workspace/deleted");
    let preferences = DesktopPreferences {
        last_workspace: None,
        window_opacity_percent: Some(91),
        deleted_project_roots: vec![root.clone()],
    };
    if preferences.save_to_path(&path).is_err() {
        return false;
    }
    let Ok(saved) = fs::read_to_string(&path) else {
        return false;
    };
    saved.contains("window_opacity_percent = 91")
        && saved.contains("deleted_project_roots")
        && saved.contains(root.as_str())
}

fn preferences_path() -> Result<Utf8PathBuf, String> {
    if let Ok(value) = std::env::var(DESKTOP_PREFS_ENV) {
        return Ok(Utf8PathBuf::from(value));
    }
    let dirs = ProjectDirs::from("net", "midi-ai-labs", "moyai")
        .ok_or_else(|| "failed to resolve desktop preferences directory".to_string())?;
    let config_dir = Utf8PathBuf::from_path_buf(dirs.config_dir().to_path_buf())
        .map_err(|_| "desktop preferences directory is not valid UTF-8".to_string())?;
    Ok(config_dir.join("desktop.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_delete_tombstone_clears_restore_state() {
        let root = Utf8Path::new("C:/workspace/deleted");
        let mut preferences = DesktopPreferences {
            last_workspace: Some(root.to_path_buf()),
            window_opacity_percent: Some(95),
            deleted_project_roots: Vec::new(),
        };

        preferences.mark_project_deleted(root);
        preferences.mark_project_deleted(root);

        assert_eq!(preferences.deleted_project_roots, vec![root.to_path_buf()]);
        assert!(preferences.last_workspace.is_none());
        assert!(preferences.is_project_deleted(root));

        preferences.unmark_project_deleted(root);

        assert!(!preferences.is_project_deleted(root));
    }
}
