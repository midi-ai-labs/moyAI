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

#[derive(Debug, Clone)]
pub struct GuardedPath {
    pub absolute: Utf8PathBuf,
    pub relative_to_root: Utf8PathBuf,
    pub inside_workspace: bool,
    pub trusted_external: bool,
    boundary_root: Utf8PathBuf,
    effective_absolute: Utf8PathBuf,
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
            && path_is_within(&effective_absolute, &effective_workspace_root);
        let trusted_external_root = if inside_workspace {
            None
        } else {
            let allow_roots = match access {
                AccessKind::List | AccessKind::Search | AccessKind::Read => {
                    &workspace.path_policy.additional_read_roots
                }
                AccessKind::Edit | AccessKind::Shell => {
                    &workspace.path_policy.additional_write_roots
                }
            };
            allow_roots.iter().find_map(|root| {
                let effective_root = effective_path_for_boundary(root).ok()?;
                (absolute.starts_with(root) && path_is_within(&effective_absolute, &effective_root))
                    .then_some(effective_root)
            })
        };
        let trusted_external = trusted_external_root.is_some();
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
            boundary_root: trusted_external_root.unwrap_or(effective_workspace_root),
            effective_absolute,
        })
    }

    pub(crate) fn trusted_internal_path(
        path: &Utf8Path,
        trusted_root: &Utf8Path,
    ) -> Result<GuardedPath, WorkspaceError> {
        let effective_absolute = effective_path_for_boundary(path)?;
        let boundary_root = effective_path_for_boundary(trusted_root)?;
        if !path.starts_with(trusted_root) || !path_is_within(&effective_absolute, &boundary_root) {
            return Err(WorkspaceError::Message(format!(
                "path `{path}` is outside the trusted internal root"
            )));
        }
        Ok(GuardedPath {
            absolute: path.to_path_buf(),
            relative_to_root: path.to_path_buf(),
            inside_workspace: false,
            trusted_external: true,
            boundary_root,
            effective_absolute,
        })
    }

    pub(crate) fn trusted_exact_path(path: &Utf8Path) -> Result<GuardedPath, WorkspaceError> {
        let effective_absolute = effective_path_for_boundary(path)?;
        Ok(GuardedPath {
            absolute: path.to_path_buf(),
            relative_to_root: path.to_path_buf(),
            inside_workspace: false,
            trusted_external: true,
            boundary_root: effective_absolute.clone(),
            effective_absolute,
        })
    }

    pub(crate) fn same_path_identity(left: &Utf8Path, right: &Utf8Path) -> bool {
        path_is_same(left, right)
    }

    pub(crate) fn compare_path_identity(left: &Utf8Path, right: &Utf8Path) -> std::cmp::Ordering {
        #[cfg(windows)]
        {
            comparable_path(left).cmp(&comparable_path(right))
        }
        #[cfg(not(windows))]
        {
            left.cmp(right)
        }
    }

    /// Rechecks the lexical target immediately before a path-based operation.
    /// Opened files must additionally pass [`Self::validate_open_file`], which
    /// validates the filesystem object behind the stable handle.
    pub fn revalidate(guarded: &GuardedPath) -> Result<(), WorkspaceError> {
        let current = effective_path_for_boundary(&guarded.absolute)?;
        if !path_is_same(&current, &guarded.effective_absolute)
            || !path_is_within(&current, &guarded.boundary_root)
        {
            return Err(WorkspaceError::Message(format!(
                "path `{}` changed after its workspace boundary check",
                guarded.absolute
            )));
        }
        Ok(())
    }

    pub fn validate_open_file(
        guarded: &GuardedPath,
        file: &std::fs::File,
    ) -> Result<(), WorkspaceError> {
        let opened = final_path_for_file(file)?;
        if !path_is_same(&opened, &guarded.effective_absolute)
            || !path_is_within(&opened, &guarded.boundary_root)
        {
            return Err(WorkspaceError::Message(format!(
                "opened file for `{}` does not match the boundary-checked filesystem object",
                guarded.absolute
            )));
        }
        Ok(())
    }

    pub fn open_validated_read_file(
        guarded: &GuardedPath,
    ) -> Result<std::fs::File, WorkspaceError> {
        Self::revalidate(guarded)?;
        let file = std::fs::File::open(&guarded.absolute)?;
        Self::validate_open_file(guarded, &file)?;
        Ok(file)
    }

    pub fn validate_open_file_within_boundary(
        guarded: &GuardedPath,
        file: &std::fs::File,
    ) -> Result<(), WorkspaceError> {
        let opened = final_path_for_file(file)?;
        if !path_is_within(&opened, &guarded.boundary_root) {
            return Err(WorkspaceError::Message(format!(
                "temporary file for `{}` escaped its boundary-checked root",
                guarded.absolute
            )));
        }
        Ok(())
    }

    pub(crate) fn validate_open_parent(
        guarded: &GuardedPath,
        directory: &std::fs::File,
    ) -> Result<(), WorkspaceError> {
        let opened = final_path_for_file(directory)?;
        let expected = guarded.effective_absolute.parent().ok_or_else(|| {
            WorkspaceError::Message(format!(
                "path `{}` has no boundary-checked parent",
                guarded.absolute
            ))
        })?;
        if !path_is_same(&opened, expected) || !path_is_within(&opened, &guarded.boundary_root) {
            return Err(WorkspaceError::Message(format!(
                "opened parent directory for `{}` does not match the boundary-checked filesystem object",
                guarded.absolute
            )));
        }
        Ok(())
    }
}

#[cfg(windows)]
fn final_path_for_file(file: &std::fs::File) -> Result<Utf8PathBuf, WorkspaceError> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    let handle = file.as_raw_handle() as HANDLE;
    let mut buffer = vec![0u16; 512];
    loop {
        let length = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if length == 0 {
            return Err(WorkspaceError::Io(std::io::Error::last_os_error()));
        }
        if (length as usize) < buffer.len() {
            buffer.truncate(length as usize);
            let value = String::from_utf16(&buffer).map_err(|error| {
                WorkspaceError::Message(format!("opened path is not valid UTF-16: {error}"))
            })?;
            return Ok(Utf8PathBuf::from(value));
        }
        buffer.resize(length as usize + 1, 0);
    }
}

#[cfg(target_os = "linux")]
fn final_path_for_file(file: &std::fs::File) -> Result<Utf8PathBuf, WorkspaceError> {
    use std::os::fd::AsRawFd as _;

    let path = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    Utf8PathBuf::from_path_buf(path).map_err(|path| {
        WorkspaceError::Message(format!(
            "opened path `{}` is not valid UTF-8",
            path.display()
        ))
    })
}

#[cfg(not(any(windows, target_os = "linux")))]
fn final_path_for_file(_file: &std::fs::File) -> Result<Utf8PathBuf, WorkspaceError> {
    Err(WorkspaceError::Message(
        "stable opened-file boundary validation is unsupported on this platform".to_string(),
    ))
}

#[cfg(windows)]
fn comparable_path(path: &Utf8Path) -> String {
    let value = path.as_str().replace('/', "\\");
    let value = value
        .strip_prefix("\\\\?\\UNC\\")
        .map(|rest| format!("\\\\{rest}"))
        .or_else(|| value.strip_prefix("\\\\?\\").map(str::to_string))
        .unwrap_or(value);
    value.trim_end_matches('\\').to_ascii_lowercase()
}

#[cfg(windows)]
fn path_is_same(candidate: &Utf8Path, expected: &Utf8Path) -> bool {
    comparable_path(candidate) == comparable_path(expected)
}

#[cfg(not(windows))]
fn path_is_same(candidate: &Utf8Path, expected: &Utf8Path) -> bool {
    candidate == expected
}

#[cfg(windows)]
fn path_is_within(candidate: &Utf8Path, root: &Utf8Path) -> bool {
    let candidate = comparable_path(candidate);
    let root = comparable_path(root);
    candidate == root
        || candidate
            .strip_prefix(&root)
            .is_some_and(|suffix| suffix.starts_with('\\'))
}

#[cfg(not(windows))]
fn path_is_within(candidate: &Utf8Path, root: &Utf8Path) -> bool {
    candidate.starts_with(root)
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

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::PathGuard;

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn opened_file_handle_matches_the_checked_boundary_object() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let path = root.join("file.txt");
        std::fs::write(&path, "content").expect("seed file");
        let guarded = PathGuard::trusted_internal_path(&path, &root).expect("guard path");
        let file = std::fs::File::open(&path).expect("open file");

        PathGuard::validate_open_file(&guarded, &file).expect("same opened object");
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn validated_read_open_returns_the_boundary_checked_handle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let path = root.join("file.txt");
        std::fs::write(&path, "content").expect("seed file");
        let guarded = PathGuard::trusted_internal_path(&path, &root).expect("guard path");

        let file = PathGuard::open_validated_read_file(&guarded).expect("validated read open");

        PathGuard::validate_open_file(&guarded, &file).expect("same opened object");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_swap_is_rejected_before_io_and_by_the_open_handle() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("root")).expect("utf8 root");
        let external =
            Utf8PathBuf::from_path_buf(temp.path().join("external")).expect("utf8 external");
        std::fs::create_dir_all(root.join("slot")).expect("root slot");
        std::fs::create_dir_all(&external).expect("external root");
        std::fs::write(root.join("slot/file.txt"), "inside").expect("inside file");
        std::fs::write(external.join("file.txt"), "outside").expect("outside file");
        let target = root.join("slot/file.txt");
        let guarded = PathGuard::trusted_internal_path(&target, &root).expect("guard target");

        std::fs::remove_file(&target).expect("remove inside file");
        std::fs::remove_dir(root.join("slot")).expect("remove inside directory");
        symlink(&external, root.join("slot")).expect("swap symlink");

        assert!(PathGuard::revalidate(&guarded).is_err());
        let escaped = std::fs::File::open(&target).expect("open swapped target");
        assert!(PathGuard::validate_open_file(&guarded, &escaped).is_err());
    }
}
