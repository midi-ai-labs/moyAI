//! Local operator templates are the only authority for creating execution directories.
use super::{RunnerError, shared::EnvironmentMapping};
use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const DESKTOP_TEMPLATE_ID: &str = "desktop-default";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProvisionTemplate {
    pub id: String,
    pub label: String,
    pub base_root: Utf8PathBuf,
    pub access_mode: crate::config::AccessMode,
    #[serde(default)]
    pub allowed_child_environments: Vec<String>,
}

pub(crate) fn validate_templates(templates: &mut [ProvisionTemplate]) -> Result<(), RunnerError> {
    if templates.len() > 128 {
        return Err(RunnerError::new(
            "At most 128 provision templates are supported",
        ));
    }
    let mut ids = BTreeSet::new();
    for template in templates {
        if !crate::device_network::stable_id(&template.id)
            || !ids.insert(template.id.clone())
            || template.label.trim().is_empty()
            || template.label.len() > 256
            || !template.base_root.is_absolute()
            || !template.base_root.is_dir()
            || template.allowed_child_environments.len() > 128
            || template
                .allowed_child_environments
                .iter()
                .any(|id| !crate::device_network::stable_id(id))
        {
            return Err(RunnerError::new(
                "Templates need unique IDs, a label, and an existing absolute base directory",
            ));
        }
        template.base_root = canonical(&template.base_root)?;
    }
    Ok(())
}

fn canonical(path: &Utf8Path) -> Result<Utf8PathBuf, RunnerError> {
    Utf8PathBuf::from_path_buf(
        std::fs::canonicalize(path).map_err(|e| RunnerError::new(e.to_string()))?,
    )
    .map_err(|_| RunnerError::new("Provision path must be UTF-8"))
}

pub(crate) fn create(
    template: &ProvisionTemplate,
    environment: &str,
) -> Result<EnvironmentMapping, RunnerError> {
    if !crate::device_network::stable_id(environment) || environment.len() > 128 {
        return Err(RunnerError::new("Invalid environment ID"));
    }
    let base = canonical(&template.base_root)?;
    if base != template.base_root {
        return Err(RunnerError::new(
            "Template directory changed; approve its new location before provisioning",
        ));
    }
    use sha2::{Digest, Sha256};
    let directory = base.join(format!(
        "environment-{:x}",
        Sha256::digest(environment.as_bytes())
    ));
    let receipt = serde_json::to_string(
        &serde_json::json!({"version":1,"environment_id":environment,"template":template}),
    )
    .map_err(|e| RunnerError::new(e.to_string()))?;
    let guarded = crate::workspace::PathGuard::trusted_internal_path(&directory, &base)
        .map_err(|e| RunnerError::new(e.to_string()))?;
    if directory.exists() {
        // A crash after create-new but before saving settings may reuse only its exact receipt.
        // Existing arbitrary directories and changed template authority are never adopted.
        let actual = canonical(&directory)?;
        if actual != directory
            || std::fs::read_to_string(directory.join(".moyai-provision.json"))
                .ok()
                .as_deref()
                != Some(receipt.as_str())
        {
            return Err(RunnerError::new(
                "Existing provision data does not match the exact locally approved template receipt",
            ));
        }
    } else {
        crate::tool::write_support::create_new_text_tree(
            &guarded,
            &[(".moyai-provision.json".into(), receipt)],
        )
        .map_err(|e| {
            RunnerError::new(format!(
                "Provisioning retained existing or partially created data: {e}"
            ))
        })?;
    }
    Ok(EnvironmentMapping {
        environment_id: environment.into(),
        directory: canonical(&directory)?,
        access_mode: template.access_mode,
        allowed_child_environments: template.allowed_child_environments.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provision_reuses_exact_receipt_and_rejects_changed_template_or_existing_data() {
        let path = Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("project_sandbox/shared-work-completion-20260913/provision-focused");
        std::fs::create_dir_all(&path).unwrap();
        let temp = tempfile::tempdir_in(path).unwrap();
        let root = canonical(Utf8Path::from_path(temp.path()).unwrap()).unwrap();
        let template = ProvisionTemplate {
            id: "approved".into(),
            label: "Approved".into(),
            base_root: root,
            access_mode: crate::config::AccessMode::Default,
            allowed_child_environments: vec![],
        };
        let first = create(&template, "environment:one").unwrap();
        assert_eq!(first, create(&template, "environment:one").unwrap());
        assert!(!first.directory.file_name().unwrap().contains(':'));
        std::fs::write(first.directory.join("user.txt"), "keep").unwrap();
        let mut changed = template.clone();
        changed.access_mode = crate::config::AccessMode::FullAccess;
        assert!(create(&changed, "environment:one").is_err());
        assert_eq!(
            std::fs::read_to_string(first.directory.join("user.txt")).unwrap(),
            "keep"
        );
        std::fs::write(first.directory.join(".moyai-provision.json"), "unrelated").unwrap();
        assert!(create(&template, "environment:one").is_err());
    }
}
