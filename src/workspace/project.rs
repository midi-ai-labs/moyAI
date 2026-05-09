use std::path::{Component, Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::WorkspaceError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VcsKind {
    Git,
    None,
}

pub fn find_workspace_root(start_dir: &Utf8Path) -> Result<Option<Utf8PathBuf>, WorkspaceError> {
    let mut current = start_dir.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Ok(Some(current));
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => return Ok(None),
        }
    }
}

pub fn normalize_path(
    base: &Utf8Path,
    requested: &Utf8Path,
) -> Result<Utf8PathBuf, WorkspaceError> {
    let raw = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        base.join(requested)
    };

    let mut normalized = PathBuf::new();
    for component in Path::new(raw.as_str()).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    Utf8PathBuf::from_path_buf(normalized)
        .map_err(|_| WorkspaceError::Message("path is not valid UTF-8".to_string()))
}
