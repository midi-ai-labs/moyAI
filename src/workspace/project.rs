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

pub(crate) fn normalize_path_separators(path: &str) -> String {
    let slash_normalized = path.replace('\\', "/");
    collapse_repeated_path_separators(&slash_normalized)
}

pub(crate) fn path_key_for_workspace_match(path: &str) -> String {
    normalize_path_separators(path.trim().trim_matches('`'))
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

pub(crate) fn workspace_relative_key_for_match(path: &str, workspace_root: &str) -> Option<String> {
    let path_key = path_key_for_workspace_match(path);
    let root_key = path_key_for_workspace_match(workspace_root);
    if root_key.is_empty() {
        return None;
    }
    if path_key == root_key {
        return Some(String::new());
    }
    let prefix = format!("{root_key}/");
    path_key
        .strip_prefix(&prefix)
        .map(|relative| relative.trim_start_matches('/').to_string())
        .filter(|relative| !relative.is_empty())
}

fn collapse_repeated_path_separators(path: &str) -> String {
    let mut collapsed = String::with_capacity(path.len());
    let mut previous_was_separator = false;
    for (index, ch) in path.chars().enumerate() {
        if ch == '/' {
            let preserve_unc_prefix =
                index < 2 && path.starts_with("//") && !path.starts_with("///");
            if preserve_unc_prefix || !previous_was_separator {
                collapsed.push(ch);
            }
            previous_was_separator = true;
        } else {
            collapsed.push(ch);
            previous_was_separator = false;
        }
    }
    collapsed
}

pub(crate) fn path_separator_normalization_fixture_passes() -> bool {
    normalize_path_separators(r"C:\\workspace\\route\\docs\\design.md")
        == "C:/workspace/route/docs/design.md"
        && normalize_path_separators(r"\\server\\share\\workspace") == "//server/share/workspace"
        && path_key_for_workspace_match(r"`.\docs\\design.md`") == "docs/design.md"
        && workspace_relative_key_for_match(
            r"C:\\workspace\\route\\docs\\design.md",
            "C:/workspace/route",
        )
        .as_deref()
            == Some("docs/design.md")
        && workspace_relative_key_for_match(
            r"C:\\workspace\\route2\\docs\\design.md",
            "C:/workspace/route",
        )
        .is_none()
}
