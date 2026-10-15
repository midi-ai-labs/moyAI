use std::collections::BTreeSet;
use std::io::Read;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use super::super::RunnerError;
use crate::config::AccessMode;

/// Explicit execution authority installed by the operator of this Windows account.
/// Hub task input can select an advertised ID, but cannot alter its local authority.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedSettings {
    pub version: u32,
    pub hub_id: String,
    pub device_id: String,
    pub environments: Vec<EnvironmentMapping>,
    #[serde(default)]
    pub resource_scope: ResourceScope,
}

#[cfg(test)]
mod isolation_tests {
    use super::*;
    #[test]
    fn overlapping_physical_roots_cannot_be_declared_independent_resources() {
        let path = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("project_sandbox/shared-work-completion-20260913/isolation-focused");
        std::fs::create_dir_all(&path).unwrap();
        let temp = tempfile::tempdir_in(path).unwrap();
        let root = Utf8Path::from_path(temp.path()).unwrap();
        std::fs::create_dir(root.join("nested")).unwrap();
        let mapping = |id: &str, directory: Utf8PathBuf| EnvironmentMapping {
            environment_id: id.into(),
            directory,
            access_mode: AccessMode::Default,
            allowed_child_environments: vec![],
        };
        let mut settings = SharedSettings {
            version: 1,
            hub_id: "hub".into(),
            device_id: "device".into(),
            resource_scope: ResourceScope::WorkspaceIsolation { confirmed: true },
            environments: vec![
                mapping("outer", root.to_owned()),
                mapping("inner", root.join("nested")),
            ],
        };
        assert!(settings.resolve().is_err());
        settings.resource_scope = ResourceScope::Device;
        settings.resolve().unwrap();
    }

    #[test]
    fn unavailable_installed_desktop_folders_leave_other_environments_usable_after_restart() {
        for (template, replace_with_file) in [
            ("desktop-default", false),
            ("desktop-default", true),
            ("local-folder", false),
            ("local-folder", true),
        ] {
            let (_temp, mut settings, journal) = super::super::tests::fixture();
            let root = journal.parent().unwrap();
            let old = root.join("old-work");
            let moved = root.join("moved-work");
            let other = root.join("other-work");
            std::fs::create_dir(&old).unwrap();
            std::fs::create_dir(&other).unwrap();
            std::fs::write(old.join("ProjectBrief.md"), "unfinished application").unwrap();
            settings.environments[0].directory = old.clone();
            let mut second = settings.environments[0].clone();
            second.environment_id = "other".into();
            second.directory = other;
            settings.environments.push(second);
            settings.resolve().unwrap();
            let receipt = serde_json::from_value(serde_json::json!({
                "environment_id":"environment", "template_id":template,
                "generation":1, "success":true, "error":null
            }))
            .unwrap();
            let mut store = crate::runner::operations::OperationsStore::open(root).unwrap();
            let mut installed = store.installed.clone();
            installed.settings = Some(settings.clone());
            installed.provisions.push(receipt);
            store.update(installed).unwrap();
            std::fs::rename(&old, &moved).unwrap();
            if replace_with_file {
                std::fs::write(&old, "replacement file").unwrap();
            }
            assert!(settings.clone().resolve().is_err());
            let mut reopened = crate::runner::operations::OperationsStore::open(root).unwrap();
            let mut restored = reopened.installed.settings.clone().unwrap();
            restored
                .resolve_installed(&reopened.installed.provisions)
                .unwrap();
            assert!(restored.mapping("environment").is_err());
            assert!(restored.mapping("other").unwrap().directory.is_dir());
            let mut next = reopened.installed.clone();
            next.settings = Some(restored);
            reopened.update(next).unwrap();
            let reopened = crate::runner::operations::OperationsStore::open(root).unwrap();
            let mut restored = reopened.installed.settings.clone().unwrap();
            restored
                .resolve_installed(&reopened.installed.provisions)
                .unwrap();
            assert_eq!(reopened.installed.provisions.len(), 1);
            assert!(reopened.installed.provisions[0].desktop_environment("environment"));
            assert_eq!(restored.environments.len(), 1);
            assert_eq!(
                old.exists(),
                replace_with_file,
                "Unavailable folders must not be recreated"
            );
            if replace_with_file {
                assert_eq!(std::fs::read_to_string(&old).unwrap(), "replacement file");
            }
            assert_eq!(
                std::fs::read_to_string(moved.join("ProjectBrief.md")).unwrap(),
                "unfinished application"
            );
        }
    }

    #[test]
    fn unavailable_folder_recovery_cannot_hide_invalid_or_unapproved_authority() {
        let (_temp, mut settings, journal) = super::super::tests::fixture();
        settings.environments[0].directory = journal.parent().unwrap().join("missing");
        let receipt = serde_json::json!({"environment_id":"environment", "template_id":"desktop-default", "generation":1, "success":true, "error":null});
        let receipts = vec![serde_json::from_value(receipt.clone()).unwrap()];
        for scenario in 0..5 {
            let mut invalid = settings.clone();
            match scenario {
                0 => invalid.environments[0].environment_id = "invalid/id".into(),
                1 => invalid.environments.push(invalid.environments[0].clone()),
                2 => invalid.environments[0].allowed_child_environments = vec!["invalid/id".into()],
                3 => {
                    invalid.environments[0].allowed_child_environments =
                        vec!["child".into(), "child".into()]
                }
                _ => invalid.environments[0].directory = "relative-folder".into(),
            }
            let before = invalid.environments.clone();
            assert!(
                invalid.resolve_installed(&receipts).is_err(),
                "scenario {scenario}"
            );
            assert_eq!(invalid.environments, before);
        }
        for (template, success) in [("manual", true), ("desktop-default", false)] {
            let mut receipt = receipt.clone();
            receipt["template_id"] = template.into();
            receipt["success"] = success.into();
            assert!(
                settings
                    .clone()
                    .resolve_installed(&[serde_json::from_value(receipt).unwrap()])
                    .is_err()
            );
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceScope {
    #[default]
    Device,
    WorkspaceIsolation {
        confirmed: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentMapping {
    pub environment_id: String,
    pub directory: Utf8PathBuf,
    pub access_mode: AccessMode,
    #[serde(default)]
    pub allowed_child_environments: Vec<String>,
}

impl EnvironmentMapping {
    pub(super) fn directory_is_current(&self) -> bool {
        self.directory.is_dir()
            && std::fs::canonicalize(&self.directory)
                .ok()
                .and_then(|path| Utf8PathBuf::from_path_buf(path).ok())
                .as_ref()
                == Some(&self.directory)
    }
}

impl SharedSettings {
    pub fn load(path: &Utf8Path) -> Result<Self, RunnerError> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(256 * 1024 + 1).read_to_end(&mut bytes))
            .map_err(|_| RunnerError::new("Cannot read shared Runner settings"))?;
        if bytes.len() > 256 * 1024 {
            return Err(RunnerError::new("Shared Runner settings exceed 256 KiB"));
        }
        let mut settings: Self = serde_json::from_slice(&bytes)
            .map_err(|_| RunnerError::new("Invalid shared Runner settings"))?;
        settings.resolve()?;
        Ok(settings)
    }

    pub(crate) fn resolve(&mut self) -> Result<(), RunnerError> {
        self.validate_structure()?;
        for mapping in &mut self.environments {
            if !mapping.directory.is_dir() {
                return Err(RunnerError::new(
                    "Each shared environment needs a unique ID and an existing absolute directory",
                ));
            }
            mapping.directory =
                Utf8PathBuf::from_path_buf(std::fs::canonicalize(&mapping.directory).map_err(
                    |_| RunnerError::new("Cannot resolve the shared environment directory"),
                )?)
                .map_err(|_| RunnerError::new("Shared directory is not UTF-8"))?;
        }
        if matches!(
            self.resource_scope,
            ResourceScope::WorkspaceIsolation { .. }
        ) {
            for (index, left) in self.environments.iter().enumerate() {
                for right in &self.environments[index + 1..] {
                    let overlap = crate::workspace::PathGuard::security_path_is_within(
                        &left.directory,
                        &right.directory,
                    )
                    .and_then(|inside| {
                        if inside {
                            Ok(true)
                        } else {
                            crate::workspace::PathGuard::security_path_is_within(
                                &right.directory,
                                &left.directory,
                            )
                        }
                    })
                    .map_err(|e| RunnerError::new(e.to_string()))?;
                    if overlap {
                        return Err(RunnerError::new(
                            "Physically overlapping directories cannot be configured as independent resources",
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Unavailable Desktop folders lose only their execution mapping. The receipt
    /// remains the authority for an explicit rebind; it never recreates a folder.
    pub(super) fn resolve_installed(
        &mut self,
        receipts: &[super::provisioning::ProvisionDelivery],
    ) -> Result<(), RunnerError> {
        // Do not hide malformed settings by discarding their unavailable paths.
        self.validate_structure()?;
        self.environments.retain(|mapping| {
            !receipts
                .iter()
                .any(|receipt| receipt.desktop_environment(&mapping.environment_id))
                || mapping.directory_is_current()
        });
        self.resolve()
    }

    fn validate_structure(&self) -> Result<(), RunnerError> {
        if matches!(
            self.resource_scope,
            ResourceScope::WorkspaceIsolation { confirmed: false }
        ) {
            return Err(RunnerError::new(
                "Resource isolation requires explicit confirmation by the local operator",
            ));
        }
        if self.version != 1
            || !crate::device_network::stable_id(&self.hub_id)
            || !crate::device_network::stable_id(&self.device_id)
            || self.environments.len() > 128
        {
            return Err(RunnerError::new(
                "Shared settings require version 1, the enrolled Hub/device identity, and at most 128 environments",
            ));
        }
        let mut ids = BTreeSet::new();
        for mapping in &self.environments {
            let children = mapping
                .allowed_child_environments
                .iter()
                .collect::<BTreeSet<_>>();
            if !crate::device_network::stable_id(&mapping.environment_id)
                || !ids.insert(mapping.environment_id.clone())
                || children.len() != mapping.allowed_child_environments.len()
                || children.len() > 128
                || children
                    .iter()
                    .any(|id| !crate::device_network::stable_id(id))
                || !mapping.directory.is_absolute()
            {
                return Err(RunnerError::new(
                    "Each shared environment needs a unique ID and an existing absolute directory",
                ));
            }
        }
        Ok(())
    }

    pub(crate) fn mapping(&self, id: &str) -> Result<&EnvironmentMapping, RunnerError> {
        self.environments
            .iter()
            .find(|mapping| mapping.environment_id == id)
            .ok_or_else(|| RunnerError::new("This environment has no local execution mapping"))
    }
}
