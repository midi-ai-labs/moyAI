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
            label: AccessMode::Default.label().to_string(),
            summary: "Automatically allows ordinary in-workspace file operations and asks the user when an operation crosses that boundary."
                .to_string(),
            auto_allowed: strings(&["ordinary workspace file operations"]),
            requires_review: vec![
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
            mode: AccessMode::AutoReview,
            id: "auto_review".to_string(),
            label: AccessMode::AutoReview.label().to_string(),
            summary: "Uses the same workspace boundary as Ask for approval, then sends boundary-crossing requests to an independent AI guardian."
                .to_string(),
            auto_allowed: strings(&[
                "ordinary workspace file operations",
                "operations approved by the AI reviewer",
            ]),
            requires_review: strings(&[
                "operations not approved by the AI reviewer",
                "reviewer unavailable or invalid reviewer responses",
            ]),
            selected: false,
            available: true,
        },
        PermissionProfileEntry {
            mode: AccessMode::FullAccess,
            id: "full_access".to_string(),
            label: AccessMode::FullAccess.label().to_string(),
            summary: "Does not ask for permission approval. Independent filesystem, ownership, and runtime integrity checks still fail closed."
                .to_string(),
            auto_allowed: strings(&["all permission requests"]),
            requires_review: Vec::new(),
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

        assert_eq!(catalog.profiles.len(), 3);
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

        assert_eq!(
            profiles[0].auto_allowed,
            vec!["ordinary workspace file operations"]
        );
        assert_eq!(profiles[1].mode, AccessMode::AutoReview);
        assert_eq!(profiles[2].auto_allowed, vec!["all permission requests"]);
        assert_eq!(profiles[0].requires_review[0], "shell");
        assert!(
            profiles[2]
                .summary
                .contains("integrity checks still fail closed")
        );
        assert!(profiles[2].requires_review.is_empty());
    }
}
