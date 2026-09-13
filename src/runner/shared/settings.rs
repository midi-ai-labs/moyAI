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
        for mapping in &mut self.environments {
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
                || !mapping.directory.is_dir()
            {
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

    pub(crate) fn mapping(&self, id: &str) -> Result<&EnvironmentMapping, RunnerError> {
        self.environments
            .iter()
            .find(|mapping| mapping.environment_id == id)
            .ok_or_else(|| RunnerError::new("This environment has no local execution mapping"))
    }
}
