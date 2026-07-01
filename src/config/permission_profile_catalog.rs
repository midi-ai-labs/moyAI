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
            summary: "Automatically allows low-risk in-workspace operations only.".to_string(),
            auto_allowed: strings(&["workspace list/search/read", "risk-free workspace edits"]),
            requires_review: vec![
                "shell",
                "destructive edits",
                "workspace authority files",
                "outside-workspace targets",
                "network or external connections",
            ]
            .into_iter()
            .map(ToString::to_string)
            .collect(),
            selected: false,
            available: true,
        },
        PermissionProfileEntry {
            mode: AccessMode::AutoReview,
            id: "auto_review".to_string(),
            label: "Auto Review".to_string(),
            summary: "Automatically allows workspace read/search/list and non-risky edits."
                .to_string(),
            auto_allowed: strings(&["workspace list/search/read", "non-risky workspace edits"]),
            requires_review: vec![
                "shell",
                "destructive edits",
                "workspace authority files",
                "outside-workspace targets",
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
            summary: "Automatically allows configured-boundary operations except protected or external-risk actions."
                .to_string(),
            auto_allowed: strings(&["workspace list/search/read", "workspace edits", "shell"]),
            requires_review: vec![
                "workspace authority files",
                "outside configured boundary",
                "network or external connections",
            ]
            .into_iter()
            .map(ToString::to_string)
            .collect(),
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
        let catalog = super::PermissionProfileCatalog::for_current(AccessMode::AutoReview);

        assert_eq!(catalog.profiles.len(), 3);
        assert_eq!(
            catalog
                .selected_profile()
                .map(|profile| profile.id.as_str()),
            Some("auto_review")
        );
    }
}
