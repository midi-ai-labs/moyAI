use std::path::{Component, Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};

use crate::error::WorkspaceError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VcsKind {
    Git,
    None,
}

pub(crate) fn project_display_name(root: &Utf8Path) -> String {
    if let Some(name) = root.file_name().filter(|name| !name.is_empty()) {
        return name.to_string();
    }

    #[cfg(windows)]
    if let Some(name) = subst_backing_folder_name(root.as_str()) {
        return name;
    }

    root.to_string()
}

#[cfg(windows)]
fn subst_backing_folder_name(root: &str) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;

    let drive = windows_drive_device_name(root)?;
    let mut drive_wide = drive.encode_utf16().collect::<Vec<_>>();
    drive_wide.push(0);
    let mut target_wide = vec![0_u16; 32_768];
    let written = unsafe {
        QueryDosDeviceW(
            drive_wide.as_ptr(),
            target_wide.as_mut_ptr(),
            target_wide.len() as u32,
        )
    };
    if written == 0 {
        return None;
    }

    let target_end = target_wide
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(written as usize)
        .min(written as usize);
    let target = String::from_utf16(&target_wide[..target_end]).ok()?;
    subst_target_folder_name(&target)
}

#[cfg(windows)]
fn windows_drive_device_name(root: &str) -> Option<String> {
    let normalized = root.replace('/', "\\");
    let drive_root = normalized
        .strip_prefix("\\\\?\\")
        .unwrap_or(normalized.as_str());
    let bytes = drive_root.as_bytes();
    if bytes.len() != 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\'
    {
        return None;
    }
    Some(drive_root[..2].to_ascii_uppercase())
}

#[cfg(windows)]
fn subst_target_folder_name(target: &str) -> Option<String> {
    let path = target
        .strip_prefix("\\??\\")
        .or_else(|| target.strip_prefix("\\DosDevices\\"))
        .or_else(|| target.strip_prefix("\\\\?\\"))?;
    let bytes = path.as_bytes();
    if bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'\\' | b'/')
    {
        return None;
    }
    let path = path.trim_end_matches(|ch| ch == '\\' || ch == '/');
    if path.len() <= 3 {
        return None;
    }
    let name = path
        .rsplit(['\\', '/'])
        .next()
        .filter(|name| !name.is_empty() && !name.ends_with(':'))?;
    Some(name.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_display_name_uses_lexical_folder_name() {
        assert_eq!(
            project_display_name(Utf8Path::new("C:/workspace/project-alpha")),
            "project-alpha"
        );
    }

    #[cfg(windows)]
    #[test]
    fn subst_target_folder_name_uses_backing_folder_leaf() {
        assert_eq!(
            subst_target_folder_name("\\??\\C:\\Users\\example\\workspace\\MappedProjectFolder\\")
                .as_deref(),
            Some("MappedProjectFolder")
        );
        assert_eq!(
            subst_target_folder_name("\\DosDevices\\C:/workspace/project-beta").as_deref(),
            Some("project-beta")
        );
    }

    #[cfg(windows)]
    #[test]
    fn physical_volume_and_volume_root_are_not_display_aliases() {
        assert_eq!(subst_target_folder_name("\\Device\\HarddiskVolume4"), None);
        assert_eq!(subst_target_folder_name("\\??\\C:\\"), None);
        assert_eq!(subst_target_folder_name("\\??\\UNC\\server\\share"), None);
        assert_eq!(
            subst_target_folder_name("\\??\\Volume{01234567-89ab-cdef-0123-456789abcdef}\\folder"),
            None
        );
        assert_eq!(
            subst_target_folder_name("\\??\\GLOBALROOT\\Device\\HarddiskVolume4\\folder"),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn drive_device_name_accepts_only_exact_drive_roots() {
        assert_eq!(windows_drive_device_name("r:/").as_deref(), Some("R:"));
        assert_eq!(
            windows_drive_device_name("\\\\?\\r:\\").as_deref(),
            Some("R:")
        );
        assert_eq!(windows_drive_device_name("R:\\workspace"), None);
        assert_eq!(windows_drive_device_name("\\\\server\\share"), None);
    }
}
