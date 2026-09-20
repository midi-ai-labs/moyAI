use std::fs;
use std::io::Write;

use camino::{Utf8Path, Utf8PathBuf};
use directories_next::ProjectDirs;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

const DESKTOP_PREFS_ENV: &str = "MOYAI_DESKTOP_PREFS_PATH";

/// The user's unfinished entry route, not connection or permission readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopOnboardingIntent {
    Welcome,
    Personal,
    Execution,
    Team,
    Hosting,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DesktopPreferences {
    pub last_workspace: Option<Utf8PathBuf>,
    pub window_opacity_percent: Option<i32>,
    #[serde(default)]
    pub deleted_project_roots: Vec<Utf8PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onboarding_intent: Option<DesktopOnboardingIntent>,
}

impl DesktopPreferences {
    /// Persist before creating default config so an interrupted first launch
    /// cannot be mistaken for an existing user's completed setup.
    pub fn prepare_initial_setup(global_config_exists: bool) -> Result<(), String> {
        if global_config_exists {
            return Ok(());
        }
        let mut preferences = Self::load()?;
        if preferences.begin_initial_setup(global_config_exists) {
            preferences.save()?;
        }
        Ok(())
    }

    fn begin_initial_setup(&mut self, global_config_exists: bool) -> bool {
        if global_config_exists || self.onboarding_intent.is_some() {
            return false;
        }
        self.onboarding_intent = Some(DesktopOnboardingIntent::Welcome);
        true
    }

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
            .is_some_and(|workspace| workspace.starts_with(root))
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
    fn interrupted_initial_setup_survives_default_config_creation_and_restart() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("desktop.toml")).unwrap();
        let mut first = DesktopPreferences::default();
        assert!(first.begin_initial_setup(false));
        first
            .save_to_path(&path)
            .expect("persist before bootstrap creates config");
        let mut restarted: DesktopPreferences =
            toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!restarted.begin_initial_setup(true));
        assert_eq!(
            restarted.onboarding_intent,
            Some(DesktopOnboardingIntent::Welcome)
        );
        for intent in [
            DesktopOnboardingIntent::Personal,
            DesktopOnboardingIntent::Execution,
            DesktopOnboardingIntent::Team,
            DesktopOnboardingIntent::Hosting,
        ] {
            restarted.onboarding_intent = Some(intent);
            restarted.save_to_path(&path).unwrap();
            let loaded: DesktopPreferences =
                toml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            assert_eq!(loaded.onboarding_intent, Some(intent));
        }
    }

    #[test]
    fn existing_users_are_not_returned_to_onboarding() {
        let mut legacy: DesktopPreferences =
            toml::from_str("window_opacity_percent = 95\n").unwrap();
        assert!(!legacy.begin_initial_setup(true));
        assert_eq!(legacy.onboarding_intent, None);
        let loaded: DesktopPreferences =
            toml::from_str(&toml::to_string(&legacy).unwrap()).unwrap();
        assert_eq!(loaded.onboarding_intent, None);
        assert_eq!(loaded.window_opacity_percent, Some(95));
    }

    #[test]
    fn project_delete_tombstone_clears_nested_restore_state() {
        let root = Utf8Path::new("C:/workspace/deleted");
        let nested_workspace = root.join("bbb");
        let mut preferences = DesktopPreferences {
            last_workspace: Some(nested_workspace),
            window_opacity_percent: Some(95),
            deleted_project_roots: Vec::new(),
            onboarding_intent: None,
        };

        preferences.mark_project_deleted(root);
        preferences.mark_project_deleted(root);

        assert_eq!(preferences.deleted_project_roots, vec![root.to_path_buf()]);
        assert!(preferences.last_workspace.is_none());
        assert!(preferences.is_project_deleted(root));

        preferences.unmark_project_deleted(root);

        assert!(!preferences.is_project_deleted(root));
    }

    #[test]
    fn project_delete_tombstone_preserves_sibling_restore_state() {
        let root = Utf8Path::new("C:/workspace/deleted");
        let sibling = Utf8PathBuf::from("C:/workspace/deleted-sibling");
        let mut preferences = DesktopPreferences {
            last_workspace: Some(sibling.clone()),
            window_opacity_percent: Some(95),
            deleted_project_roots: Vec::new(),
            onboarding_intent: None,
        };

        preferences.mark_project_deleted(root);

        assert_eq!(preferences.last_workspace, Some(sibling));
    }
}
