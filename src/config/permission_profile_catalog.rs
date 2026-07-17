use serde::{Deserialize, Serialize};

use crate::config::AccessMode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionProfileCatalog {
    pub current: AccessMode,
    pub profiles: Vec<PermissionProfileEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionProfileEntry {
    pub mode: AccessMode,
    pub id: String,
    pub label: String,
    pub summary: String,
    pub auto_allowed: Vec<String>,
    pub requires_review: Vec<String>,
    pub selected: bool,
    pub available: bool,
}

impl PermissionProfileCatalog {
    pub fn for_current(current: AccessMode) -> Self {
        let profiles = builtin_permission_profiles()
            .into_iter()
            .map(|mut profile| {
                profile.selected = profile.mode == current;
                profile
            })
            .collect();
        Self { current, profiles }
    }

    pub fn selected_profile(&self) -> Option<&PermissionProfileEntry> {
        self.profiles.iter().find(|profile| profile.selected)
    }
}

pub fn builtin_permission_profiles() -> Vec<PermissionProfileEntry> {
    vec![
        PermissionProfileEntry {
            mode: AccessMode::Default,
            id: "default".to_string(),
            label: "Default".to_string(),
            summary: "Automatically allows in-workspace list, search, and read operations only."
                .to_string(),
            auto_allowed: strings(&["workspace list/search/read"]),
            requires_review: vec![
                "workspace edits",
                "shell",
                "destructive edits",
                "workspace authority files",
                "detected outside configured boundary",
                "network or external connections",
            ]
            .into_iter()
            .map(ToString::to_string)
            .collect(),
            selected: false,
            available: true,
        },
        PermissionProfileEntry {
            mode: AccessMode::FullAccess,
            id: "full_access".to_string(),
            label: "Full Access".to_string(),
            summary: "Automatically allows operations accepted by the configured boundary, including detected shell risks. It is not an OS sandbox."
                .to_string(),
            auto_allowed: strings(&[
                "configured-boundary list/search/read",
                "configured-boundary edits",
                "configured-boundary shell including detected risks",
            ]),
            requires_review: strings(&["detected outside configured boundary"]),
            selected: false,
            available: true,
        },
    ]
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use crate::config::AccessMode;

    #[test]
    fn permission_profile_catalog_selects_current_mode() {
        let catalog = super::PermissionProfileCatalog::for_current(AccessMode::FullAccess);

        assert_eq!(catalog.profiles.len(), 2);
        assert_eq!(
            catalog
                .selected_profile()
                .map(|profile| profile.id.as_str()),
            Some("full_access")
        );
    }

    #[test]
    fn permission_profile_catalog_describes_monotonic_authority() {
        let profiles = super::builtin_permission_profiles();

        assert_eq!(profiles[0].auto_allowed, vec!["workspace list/search/read"]);
        assert_eq!(
            profiles[1].auto_allowed,
            vec![
                "configured-boundary list/search/read",
                "configured-boundary edits",
                "configured-boundary shell including detected risks"
            ]
        );
        assert_eq!(profiles[0].requires_review[0], "workspace edits");
        assert_eq!(profiles[0].requires_review[1], "shell");
        assert!(profiles[1].summary.contains("not an OS sandbox"));
        assert!(
            profiles[1]
                .requires_review
                .iter()
                .any(|value| value == "detected outside configured boundary")
        );
    }
}
