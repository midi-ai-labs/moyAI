use camino::{Utf8Path, Utf8PathBuf};

use crate::error::WorkspaceError;
use crate::workspace::Workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    List,
    Search,
    Read,
    Edit,
    Shell,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PathPolicy {
    pub workspace_root: Utf8PathBuf,
    pub additional_read_roots: Vec<Utf8PathBuf>,
    pub additional_write_roots: Vec<Utf8PathBuf>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GuardedPath {
    pub absolute: Utf8PathBuf,
    pub relative_to_root: Utf8PathBuf,
    pub inside_workspace: bool,
    pub trusted_external: bool,
}

pub struct PathGuard;

impl PathGuard {
    pub fn require_path(
        workspace: &Workspace,
        requested: &Utf8Path,
        access: AccessKind,
        allow_external: bool,
    ) -> Result<GuardedPath, WorkspaceError> {
        let mut absolute = crate::workspace::project::normalize_path(&workspace.cwd, requested)?;
        if !absolute.starts_with(&workspace.root) {
            if let Some(remapped) = remap_absolute_path_into_workspace(&absolute, &workspace.root) {
                absolute = remapped;
            }
        }
        if workspace
            .protected_paths
            .iter()
            .any(|path| absolute.starts_with(path))
        {
            return Err(WorkspaceError::Message(format!(
                "path `{absolute}` is protected"
            )));
        }

        let inside_workspace = absolute.starts_with(&workspace.root);
        let trusted_external = if inside_workspace {
            false
        } else {
            let allow_roots = match access {
                AccessKind::List | AccessKind::Search | AccessKind::Read => {
                    &workspace.path_policy.additional_read_roots
                }
                AccessKind::Edit | AccessKind::Shell => {
                    &workspace.path_policy.additional_write_roots
                }
            };
            allow_roots.iter().any(|root| absolute.starts_with(root))
        };
        let is_allowed_external = inside_workspace || trusted_external || allow_external;

        if !is_allowed_external {
            return Err(WorkspaceError::Message(format!(
                "path `{absolute}` is outside the allowed roots"
            )));
        }

        let relative_to_root = if inside_workspace {
            absolute
                .strip_prefix(&workspace.root)
                .unwrap_or(Utf8Path::new(""))
                .to_path_buf()
        } else {
            absolute.clone()
        };

        Ok(GuardedPath {
            absolute,
            relative_to_root,
            inside_workspace,
            trusted_external,
        })
    }
}

fn remap_absolute_path_into_workspace(
    requested: &Utf8Path,
    workspace_root: &Utf8Path,
) -> Option<Utf8PathBuf> {
    if !requested.is_absolute() {
        return None;
    }
    let root_name = workspace_root.file_name()?;
    let components = requested.iter().collect::<Vec<_>>();
    let root_index = components
        .iter()
        .rposition(|component| *component == root_name)?;
    let mut candidate = workspace_root.to_path_buf();
    for component in &components[root_index + 1..] {
        candidate.push(component);
    }
    let parent_exists = candidate
        .parent()
        .is_some_and(|parent| parent.exists() || parent == workspace_root);
    (candidate.exists() || parent_exists).then_some(candidate)
}
