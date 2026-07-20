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
            summary: "Runs ordinary typed in-workspace file operations directly and local shell/formatter processes in the native workspace-write OS sandbox profile when available (currently Windows), then asks the user before explicit or detected elevation. The Windows profile pins a finite set of existing objects; namespace, network, outside-path, and same-user host-process isolation are not exhaustive."
                .to_string(),
            auto_allowed: strings(&[
                "ordinary workspace file operations",
                "shell and formatter processes classified as local inside the workspace-write sandbox",
            ]),
            requires_review: vec![
                "destructive edits",
                "workspace authority files",
                "detected outside configured boundary",
                "explicitly requested or heuristically detected network/external elevation",
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
            summary: "Uses the same finite native workspace-write OS sandbox profile as Ask for approval when available (currently Windows), then sends explicit or detected elevation requests to an independent AI guardian. Namespace, network, outside-path, and same-user host-process isolation are not exhaustive."
                .to_string(),
            auto_allowed: strings(&[
                "ordinary workspace file operations",
                "shell and formatter processes classified as local inside the workspace-write sandbox",
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
            summary: "Runs process effects without the workspace OS sandbox and does not ask for permission approval. Typed file/in-process guards and process lifecycle checks still apply, but unrestricted child filesystem mutations do not pass through typed file guards."
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
            vec![
                "ordinary workspace file operations",
                "shell and formatter processes classified as local inside the workspace-write sandbox",
            ]
        );
        assert_eq!(profiles[1].mode, AccessMode::AutoReview);
        assert_eq!(profiles[2].auto_allowed, vec!["all permission requests"]);
        assert_eq!(profiles[0].requires_review[0], "destructive edits");
        assert!(
            profiles[0]
                .summary
                .contains("native workspace-write OS sandbox")
        );
        assert!(
            profiles[2]
                .summary
                .contains("unrestricted child filesystem mutations")
        );
        assert!(profiles[2].requires_review.is_empty());
    }
}
