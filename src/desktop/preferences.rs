use std::collections::BTreeMap;
use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use directories_next::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::config::model::PartialResolvedConfig;

const DESKTOP_PREFS_ENV: &str = "MOYAI_DESKTOP_PREFS_PATH";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DesktopPreferences {
    pub last_workspace: Option<Utf8PathBuf>,
    pub window_opacity_percent: Option<i32>,
    #[serde(default)]
    pub deleted_project_roots: Vec<Utf8PathBuf>,
    #[serde(default)]
    pub workspaces: BTreeMap<String, DesktopWorkspacePreferences>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DesktopWorkspacePreferences {
    #[serde(default)]
    pub session_override: PartialResolvedConfig,
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
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|error| error.to_string())?;
        let temp_path = path.with_extension("tmp");
        fs::write(&temp_path, text).map_err(|error| error.to_string())?;
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
        fs::rename(&temp_path, &path).map_err(|error| error.to_string())
    }

    pub fn workspace_override(&self, root: &Utf8Path) -> Option<PartialResolvedConfig> {
        self.workspaces
            .get(root.as_str())
            .map(|prefs| prefs.session_override.clone())
    }

    pub fn set_workspace_override(&mut self, root: &Utf8Path, patch: PartialResolvedConfig) {
        self.workspaces.insert(
            root.as_str().to_string(),
            DesktopWorkspacePreferences {
                session_override: patch,
            },
        );
    }

    pub fn clear_workspace_override(&mut self, root: &Utf8Path) {
        self.workspaces.remove(root.as_str());
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
        self.clear_workspace_override(root);
    }

    pub fn unmark_project_deleted(&mut self, root: &Utf8Path) {
        self.deleted_project_roots.retain(|path| path != root);
    }

    pub fn is_project_deleted(&self, root: &Utf8Path) -> bool {
        self.deleted_project_roots.iter().any(|path| path == root)
    }
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
    fn project_delete_tombstone_clears_restore_state_and_override() {
        let root = Utf8Path::new("C:/workspace/deleted");
        let mut preferences = DesktopPreferences {
            last_workspace: Some(root.to_path_buf()),
            window_opacity_percent: Some(95),
            deleted_project_roots: Vec::new(),
            workspaces: BTreeMap::new(),
        };
        preferences.set_workspace_override(root, PartialResolvedConfig::default());

        preferences.mark_project_deleted(root);
        preferences.mark_project_deleted(root);

        assert_eq!(preferences.deleted_project_roots, vec![root.to_path_buf()]);
        assert!(preferences.last_workspace.is_none());
        assert!(preferences.workspace_override(root).is_none());
        assert!(preferences.is_project_deleted(root));

        preferences.unmark_project_deleted(root);

        assert!(!preferences.is_project_deleted(root));
    }
}
