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
    ) -> Result<GuardedPath, WorkspaceError> {
        let absolute = crate::workspace::project::normalize_path(&workspace.cwd, requested)?;
        let effective_absolute = effective_path_for_boundary(&absolute)?;
        let effective_workspace_root = effective_path_for_boundary(&workspace.root)?;
        if workspace.protected_paths.iter().any(|path| {
            absolute.starts_with(path)
                || effective_path_for_boundary(path)
                    .map(|effective| effective_absolute.starts_with(effective))
                    .unwrap_or(false)
        }) {
            return Err(WorkspaceError::Message(format!(
                "path `{absolute}` is protected"
            )));
        }

        let inside_workspace = absolute.starts_with(&workspace.root)
            && effective_absolute.starts_with(&effective_workspace_root);
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
            allow_roots.iter().any(|root| {
                absolute.starts_with(root)
                    || effective_path_for_boundary(root)
                        .map(|effective| effective_absolute.starts_with(effective))
                        .unwrap_or(false)
            })
        };
        let is_allowed_external = inside_workspace || trusted_external;

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

fn effective_path_for_boundary(path: &Utf8Path) -> Result<Utf8PathBuf, WorkspaceError> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return Utf8PathBuf::from_path_buf(canonical).map_err(|path| {
            WorkspaceError::Message(format!("path `{}` is not valid UTF-8", path.display()))
        });
    }

    let mut missing = Vec::new();
    let mut cursor = path.as_std_path();
    while !cursor.exists() {
        if let Some(file_name) = cursor.file_name() {
            missing.push(file_name.to_os_string());
        }
        let Some(parent) = cursor.parent() else {
            break;
        };
        if parent == cursor {
            break;
        }
        cursor = parent;
    }

    let mut effective = if cursor.exists() {
        std::fs::canonicalize(cursor).map_err(|error| {
            WorkspaceError::Message(format!(
                "failed to canonicalize `{}`: {error}",
                cursor.display()
            ))
        })?
    } else {
        path.as_std_path().to_path_buf()
    };
    for component in missing.iter().rev() {
        effective.push(component);
    }
    Utf8PathBuf::from_path_buf(effective).map_err(|path| {
        WorkspaceError::Message(format!("path `{}` is not valid UTF-8", path.display()))
    })
}

pub(crate) fn path_guard_rejects_cross_workspace_absolute_remap_fixture_passes() -> bool {
    use crate::config::ResolvedConfig;
    use crate::workspace::WorkspaceDiscovery;

    let unique = format!(
        "moyai-pathguard-remap-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    );
    let root = std::env::temp_dir().join(unique);
    let workspace_root = root.join("real").join("workspace");
    let Ok(workspace_root) = Utf8PathBuf::from_path_buf(workspace_root) else {
        return false;
    };
    if std::fs::create_dir_all(workspace_root.join("docs")).is_err() {
        return false;
    }
    let Ok(outside_request) = Utf8PathBuf::from_path_buf(
        root.join("elsewhere")
            .join("workspace")
            .join("docs")
            .join("note.md"),
    ) else {
        let _ = std::fs::remove_dir_all(root);
        return false;
    };
    let workspace = match WorkspaceDiscovery::discover_fixed_root(
        &workspace_root,
        &ResolvedConfig::default(),
    ) {
        Ok(value) => value,
        Err(_) => {
            let _ = std::fs::remove_dir_all(root);
            return false;
        }
    };
    let rejected = PathGuard::require_path(&workspace, &outside_request, AccessKind::Read).is_err();
    let _ = std::fs::remove_dir_all(root);
    rejected
}

#[cfg(test)]
mod tests {
    #[test]
    fn path_guard_rejects_cross_workspace_absolute_remap() {
        assert!(super::path_guard_rejects_cross_workspace_absolute_remap_fixture_passes());
    }
}
