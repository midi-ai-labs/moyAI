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
    effective_workspace_root: Option<Utf8PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExistingObjectIdentity(ExistingObjectIdentityKind);

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExistingObjectIdentityKind {
    #[cfg(windows)]
    Windows(WindowsFileIdentity),
    #[cfg(unix)]
    Unix { device: u64, inode: u64 },
    #[cfg(not(any(windows, unix)))]
    CanonicalPath(Utf8PathBuf),
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
        if is_protected_path(workspace, &absolute, &effective_absolute)? {
            return Err(WorkspaceError::Message(format!(
                "path `{absolute}` is protected"
            )));
        }

        let inside_workspace = boundary_path_is_within(
            &absolute,
            &workspace.root,
            &effective_absolute,
            &effective_workspace_root,
        )?;
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
            let mut trusted_root = None;
            for root in allow_roots {
                let effective_root = effective_path_for_boundary(root)?;
                if boundary_path_is_within(&absolute, root, &effective_absolute, &effective_root)? {
                    trusted_root = Some(effective_root);
                    break;
                }
            }
            trusted_root
        };
        let trusted_external = trusted_external_root.is_some();
        let is_allowed_external = inside_workspace || trusted_external;

        if !is_allowed_external {
            return Err(WorkspaceError::Message(format!(
                "path `{absolute}` is outside the allowed roots"
            )));
        }

        let relative_to_root = if inside_workspace {
            boundary_relative_path_from_root(
                &absolute,
                &workspace.root,
                &effective_absolute,
                &effective_workspace_root,
            )
            .ok_or_else(|| {
                WorkspaceError::Message(format!(
                    "path `{absolute}` could not be projected relative to workspace root `{}`",
                    workspace.root
                ))
            })?
        } else {
            absolute.clone()
        };

        Ok(GuardedPath {
            absolute,
            relative_to_root,
            inside_workspace,
            trusted_external,
            boundary_root: trusted_external_root
                .unwrap_or_else(|| effective_workspace_root.clone()),
            effective_absolute,
            effective_workspace_root: Some(effective_workspace_root),
        })
    }

    /// Rechecks an already-guarded search root and derives an exact guard for
    /// one path enumerated beneath it. The returned guard can be passed to
    /// [`Self::open_validated_read_file`] so path discovery and file reads do
    /// not become separate boundary owners.
    pub(crate) fn require_descendant(
        workspace: &Workspace,
        guarded_root: &GuardedPath,
        candidate: &Utf8Path,
    ) -> Result<GuardedPath, WorkspaceError> {
        Self::revalidate(guarded_root)?;
        let absolute = crate::workspace::project::normalize_path(&workspace.cwd, candidate)?;
        let effective_absolute = effective_path_for_boundary(&absolute)?;
        let effective_workspace_root = effective_path_for_boundary(&workspace.root)?;
        if !boundary_path_is_within(
            &absolute,
            &guarded_root.absolute,
            &effective_absolute,
            &guarded_root.effective_absolute,
        )? || is_protected_path(workspace, &absolute, &effective_absolute)?
        {
            return Err(WorkspaceError::Message(format!(
                "path `{absolute}` is outside its boundary-checked search root"
            )));
        }

        let relative_to_root = if guarded_root.inside_workspace {
            boundary_relative_path_from_root(
                &absolute,
                &workspace.root,
                &effective_absolute,
                &effective_workspace_root,
            )
            .ok_or_else(|| {
                WorkspaceError::Message(format!(
                    "path `{absolute}` could not be projected relative to workspace root `{}`",
                    workspace.root
                ))
            })?
        } else {
            absolute.clone()
        };
        Ok(GuardedPath {
            absolute,
            relative_to_root,
            inside_workspace: guarded_root.inside_workspace,
            trusted_external: guarded_root.trusted_external,
            boundary_root: guarded_root.effective_absolute.clone(),
            effective_absolute,
            effective_workspace_root: Some(effective_workspace_root),
        })
    }

    pub(crate) fn trusted_internal_path(
        path: &Utf8Path,
        trusted_root: &Utf8Path,
    ) -> Result<GuardedPath, WorkspaceError> {
        let effective_absolute = effective_path_for_boundary(path)?;
        let boundary_root = effective_path_for_boundary(trusted_root)?;
        if !boundary_path_is_within(path, trusted_root, &effective_absolute, &boundary_root)? {
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
            effective_workspace_root: None,
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
            effective_workspace_root: None,
        })
    }

    pub(crate) fn same_path_identity(left: &Utf8Path, right: &Utf8Path) -> bool {
        path_is_same(left, right)
    }

    pub(crate) fn opened_file_identity_path(
        file: &std::fs::File,
    ) -> Result<Utf8PathBuf, WorkspaceError> {
        let opened = final_path_for_file(file)?;
        #[cfg(windows)]
        {
            Ok(Utf8PathBuf::from(windows_path_without_extended_namespace(
                &opened,
            )))
        }
        #[cfg(not(windows))]
        {
            Ok(opened)
        }
    }

    pub(crate) fn opened_object_identity(
        file: &std::fs::File,
    ) -> Result<ExistingObjectIdentity, WorkspaceError> {
        #[cfg(windows)]
        {
            Ok(ExistingObjectIdentity(ExistingObjectIdentityKind::Windows(
                windows_file_identity(file)?,
            )))
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            let metadata = file.metadata()?;
            Ok(ExistingObjectIdentity(ExistingObjectIdentityKind::Unix {
                device: metadata.dev(),
                inode: metadata.ino(),
            }))
        }
        #[cfg(not(any(windows, unix)))]
        {
            Ok(ExistingObjectIdentity(
                ExistingObjectIdentityKind::CanonicalPath(Self::opened_file_identity_path(file)?),
            ))
        }
    }

    pub(crate) fn same_existing_object_identity(
        left: &Utf8Path,
        right: &Utf8Path,
    ) -> Result<bool, WorkspaceError> {
        #[cfg(windows)]
        {
            let left = WindowsSecurityAnchor::open(left).map_err(|error| {
                WorkspaceError::Message(format!(
                    "failed to open existing identity path `{left}`: {error}"
                ))
            })?;
            let right = WindowsSecurityAnchor::open(right).map_err(|error| {
                WorkspaceError::Message(format!(
                    "failed to open existing identity path `{right}`: {error}"
                ))
            })?;
            Ok(left.identity == right.identity)
        }
        #[cfg(not(windows))]
        {
            Ok(Self::canonical_existing_identity_path(left)?
                == Self::canonical_existing_identity_path(right)?)
        }
    }

    pub(crate) fn same_existing_namespace_entry(
        left: &Utf8Path,
        right: &Utf8Path,
    ) -> Result<bool, WorkspaceError> {
        let left_parent = left.parent().ok_or_else(|| {
            WorkspaceError::Message(format!("namespace entry `{left}` has no parent"))
        })?;
        let right_parent = right.parent().ok_or_else(|| {
            WorkspaceError::Message(format!("namespace entry `{right}` has no parent"))
        })?;
        let left_name = left.file_name().ok_or_else(|| {
            WorkspaceError::Message(format!("namespace entry `{left}` has no final component"))
        })?;
        let right_name = right.file_name().ok_or_else(|| {
            WorkspaceError::Message(format!("namespace entry `{right}` has no final component"))
        })?;

        #[cfg(windows)]
        {
            let left_parent = WindowsSecurityAnchor::open(left_parent).map_err(|error| {
                WorkspaceError::Message(format!(
                    "failed to open namespace parent `{left_parent}`: {error}"
                ))
            })?;
            let right_parent = WindowsSecurityAnchor::open(right_parent).map_err(|error| {
                WorkspaceError::Message(format!(
                    "failed to open namespace parent `{right_parent}`: {error}"
                ))
            })?;
            if left_parent.identity != right_parent.identity {
                return Ok(false);
            }
            windows_missing_component_matches(&left_parent, left_name, right_name)
        }
        #[cfg(not(windows))]
        {
            Ok(Self::canonical_existing_identity_path(left_parent)?
                == Self::canonical_existing_identity_path(right_parent)?
                && left_name == right_name)
        }
    }

    #[cfg(not(windows))]
    fn canonical_existing_identity_path(path: &Utf8Path) -> Result<Utf8PathBuf, WorkspaceError> {
        let canonical = std::fs::canonicalize(path)?;
        Utf8PathBuf::from_path_buf(canonical).map_err(|path| {
            WorkspaceError::Message(format!(
                "existing identity path `{}` is not valid UTF-8",
                path.display()
            ))
        })
    }

    pub(crate) fn security_path_is_within(
        candidate: &Utf8Path,
        root: &Utf8Path,
    ) -> Result<bool, WorkspaceError> {
        #[cfg(windows)]
        {
            windows_security_path_is_within(candidate, root)
        }
        #[cfg(not(windows))]
        {
            Ok(path_is_within(candidate, root))
        }
    }

    pub(crate) fn stable_identity_key(path: &Utf8Path) -> String {
        #[cfg(windows)]
        {
            normalized_windows_path(path).to_ascii_lowercase()
        }
        #[cfg(not(windows))]
        {
            path.as_str().to_string()
        }
    }

    pub(crate) fn compare_path_identity(left: &Utf8Path, right: &Utf8Path) -> std::cmp::Ordering {
        #[cfg(windows)]
        {
            Self::stable_identity_key(left).cmp(&Self::stable_identity_key(right))
        }
        #[cfg(not(windows))]
        {
            left.cmp(right)
        }
    }

    pub(crate) fn relative_path_from_root(
        candidate: &Utf8Path,
        root: &Utf8Path,
    ) -> Option<Utf8PathBuf> {
        relative_path_from_root(candidate, root)
    }

    /// Classifies a permission target from the same lexical and effective
    /// filesystem snapshot that established its workspace boundary.
    pub fn targets_protected_workspace_authority(
        workspace_root: &Utf8Path,
        guarded: &GuardedPath,
    ) -> bool {
        let lexical_target = crate::workspace::special_paths::is_protected_workspace_authority_path(
            workspace_root,
            &guarded.absolute,
        );
        let effective_root = guarded
            .effective_workspace_root
            .as_deref()
            .unwrap_or(workspace_root);
        lexical_target
            || crate::workspace::special_paths::is_protected_workspace_authority_path(
                effective_root,
                &guarded.effective_absolute,
            )
    }

    /// Rechecks the lexical target immediately before a path-based operation.
    /// Opened files must additionally pass [`Self::validate_open_file`], which
    /// validates the filesystem object behind the stable handle.
    pub fn revalidate(guarded: &GuardedPath) -> Result<(), WorkspaceError> {
        let current = effective_path_for_boundary(&guarded.absolute)?;
        if !security_canonical_path_is_same(&current, &guarded.effective_absolute)
            || !security_canonical_path_is_within(&current, &guarded.boundary_root)
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
        let opened = Self::opened_file_identity_path(file)?;
        if !security_canonical_path_is_same(&opened, &guarded.effective_absolute)
            || !security_canonical_path_is_within(&opened, &guarded.boundary_root)
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
        #[cfg(unix)]
        let file = open_unix_nonblocking_handle(guarded)?;
        #[cfg(not(unix))]
        let file = std::fs::File::open(&guarded.absolute)?;
        Self::validate_open_file(guarded, &file)?;
        Ok(file)
    }

    pub(crate) fn open_validated_metadata_handle(
        guarded: &GuardedPath,
    ) -> Result<std::fs::File, WorkspaceError> {
        Self::revalidate(guarded)?;
        #[cfg(windows)]
        let file = open_windows_security_path(&guarded.absolute)?;
        #[cfg(unix)]
        let file = open_unix_nonblocking_handle(guarded)?;
        #[cfg(not(any(windows, unix)))]
        let file = std::fs::File::open(&guarded.absolute)?;
        Self::validate_open_file(guarded, &file)?;
        Ok(file)
    }

    pub fn validate_open_file_within_boundary(
        guarded: &GuardedPath,
        file: &std::fs::File,
    ) -> Result<(), WorkspaceError> {
        let opened = Self::opened_file_identity_path(file)?;
        if !security_canonical_path_is_within(&opened, &guarded.boundary_root) {
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
        let opened = Self::opened_file_identity_path(directory)?;
        let expected = guarded.effective_absolute.parent().ok_or_else(|| {
            WorkspaceError::Message(format!(
                "path `{}` has no boundary-checked parent",
                guarded.absolute
            ))
        })?;
        if !security_canonical_path_is_same(&opened, expected)
            || !security_canonical_path_is_within(&opened, &guarded.boundary_root)
        {
            return Err(WorkspaceError::Message(format!(
                "opened parent directory for `{}` does not match the boundary-checked filesystem object",
                guarded.absolute
            )));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn open_unix_nonblocking_handle(guarded: &GuardedPath) -> Result<std::fs::File, WorkspaceError> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::io::{AsRawFd as _, FromRawFd as _};

    let target = guarded.effective_absolute.as_path();
    let Some(target_name) = target.file_name() else {
        return open_unix_direct_nonblocking_handle(target);
    };
    let parent_path = target.parent().ok_or_else(|| {
        WorkspaceError::Message(format!("Unix read path `{target}` has no parent"))
    })?;
    let parent_path = std::ffi::CString::new(parent_path.as_std_path().as_os_str().as_bytes())
        .map_err(|_| WorkspaceError::Message("Unix read parent contains NUL".to_string()))?;
    // SAFETY: `parent_path` is a live C string. O_DIRECTORY and O_NOFOLLOW pin the canonical
    // effective parent object before any final-component open is attempted.
    let parent_descriptor = unsafe {
        libc::open(
            parent_path.as_ptr(),
            unix_parent_directory_access_flag()
                | libc::O_DIRECTORY
                | libc::O_CLOEXEC
                | libc::O_NOFOLLOW,
        )
    };
    if parent_descriptor < 0 {
        let error = std::io::Error::last_os_error();
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            // POSIX does not expose one portable search-only directory flag. Preserve reads from
            // searchable but non-readable parents by opening the canonical target directly;
            // O_NOFOLLOW/O_NONBLOCK and the caller's same-handle boundary validation still apply.
            return open_unix_direct_nonblocking_handle(target);
        }
        return Err(WorkspaceError::Io(error));
    }
    // SAFETY: ownership of the newly returned descriptor transfers exactly once.
    let parent = unsafe { std::fs::File::from_raw_fd(parent_descriptor) };
    let opened_parent = PathGuard::opened_file_identity_path(&parent)?;
    let expected_parent = guarded.effective_absolute.parent().ok_or_else(|| {
        WorkspaceError::Message(format!(
            "path `{}` has no boundary-checked parent",
            guarded.absolute
        ))
    })?;
    if !security_canonical_path_is_same(&opened_parent, expected_parent) {
        return Err(WorkspaceError::Message(format!(
            "opened parent directory for `{}` does not match the boundary-checked filesystem object",
            guarded.absolute
        )));
    }

    let target_name = std::ffi::CString::new(target_name.as_bytes())
        .map_err(|_| WorkspaceError::Message("Unix read entry contains NUL".to_string()))?;
    // SAFETY: the stable parent descriptor and single NUL-terminated component are live.
    // O_NONBLOCK prevents a raced FIFO or device from stalling before same-handle validation.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            target_name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        Err(WorkspaceError::Io(std::io::Error::last_os_error()))
    } else {
        // SAFETY: ownership of the newly returned descriptor transfers exactly once.
        Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn unix_parent_directory_access_flag() -> libc::c_int {
    libc::O_PATH
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn unix_parent_directory_access_flag() -> libc::c_int {
    libc::O_RDONLY
}

#[cfg(unix)]
fn open_unix_direct_nonblocking_handle(target: &Utf8Path) -> Result<std::fs::File, WorkspaceError> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::io::FromRawFd as _;

    let target = std::ffi::CString::new(target.as_std_path().as_os_str().as_bytes())
        .map_err(|_| WorkspaceError::Message("Unix read path contains NUL".to_string()))?;
    // SAFETY: `target` is a live canonical C string. O_NOFOLLOW prevents a raced final symlink,
    // O_NONBLOCK bounds special-file opens, and the caller validates the returned handle itself.
    let descriptor = unsafe {
        libc::open(
            target.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        Err(WorkspaceError::Io(std::io::Error::last_os_error()))
    } else {
        // SAFETY: ownership of the newly returned descriptor transfers exactly once.
        Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum WindowsFileIdentity {
    Extended {
        volume_serial_number: u64,
        file_id: [u8; 16],
    },
    Legacy {
        volume_serial_number: u32,
        file_index: u64,
    },
}

#[cfg(windows)]
fn windows_file_identity(file: &std::fs::File) -> Result<WindowsFileIdentity, WorkspaceError> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx,
    };

    let handle = file.as_raw_handle() as HANDLE;
    let mut extended = FILE_ID_INFO::default();
    let extended_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut extended as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if extended_result != 0 {
        return Ok(WindowsFileIdentity::Extended {
            volume_serial_number: extended.VolumeSerialNumber,
            file_id: extended.FileId.Identifier,
        });
    }

    let mut legacy = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let legacy_result = unsafe { GetFileInformationByHandle(handle, &mut legacy) };
    if legacy_result == 0 {
        return Err(WorkspaceError::Io(std::io::Error::last_os_error()));
    }
    Ok(WindowsFileIdentity::Legacy {
        volume_serial_number: legacy.dwVolumeSerialNumber,
        file_index: ((legacy.nFileIndexHigh as u64) << 32) | legacy.nFileIndexLow as u64,
    })
}

#[cfg(windows)]
fn open_windows_security_path(path: &Utf8Path) -> Result<std::fs::File, std::io::Error> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };

    let mut options = std::fs::OpenOptions::new();
    options
        // Attribute-only opens do not participate in every Windows share-mode
        // conflict. Request the file-data/directory-list bit as well so omitting
        // FILE_SHARE_DELETE actually pins the namespace entry against rename.
        .access_mode(FILE_READ_ATTRIBUTES | FILE_LIST_DIRECTORY)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    options.open(path)
}

#[cfg(windows)]
struct WindowsSecurityAnchor {
    file: std::fs::File,
    identity: WindowsFileIdentity,
    final_path: Utf8PathBuf,
    is_directory: bool,
}

#[cfg(windows)]
impl WindowsSecurityAnchor {
    fn open(path: &Utf8Path) -> Result<Self, std::io::Error> {
        let file = open_windows_security_path(path)?;
        let metadata = file.metadata()?;
        let final_path = PathGuard::opened_file_identity_path(&file).map_err(workspace_to_io)?;
        let identity = windows_file_identity(&file).map_err(workspace_to_io)?;
        Ok(Self {
            file,
            identity,
            final_path,
            is_directory: metadata.is_dir(),
        })
    }

    fn case_sensitive_directory(&self) -> Result<bool, WorkspaceError> {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_CASE_SENSITIVE_INFO, FileCaseSensitiveInfo, GetFileInformationByHandleEx,
        };
        use windows_sys::Win32::System::SystemServices::FILE_CS_FLAG_CASE_SENSITIVE_DIR;

        if !self.is_directory {
            return Err(WorkspaceError::Message(format!(
                "case-sensitivity policy requested for non-directory `{}`",
                self.final_path
            )));
        }
        let mut info = FILE_CASE_SENSITIVE_INFO::default();
        let result = unsafe {
            GetFileInformationByHandleEx(
                self.file.as_raw_handle() as HANDLE,
                FileCaseSensitiveInfo,
                (&mut info as *mut FILE_CASE_SENSITIVE_INFO).cast(),
                std::mem::size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
            )
        };
        if result == 0 {
            return Err(WorkspaceError::Message(format!(
                "failed to query case-sensitivity policy for `{}`: {}",
                self.final_path,
                std::io::Error::last_os_error()
            )));
        }
        Ok(info.Flags & FILE_CS_FLAG_CASE_SENSITIVE_DIR != 0)
    }
}

#[cfg(windows)]
fn workspace_to_io(error: WorkspaceError) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

#[cfg(windows)]
struct WindowsResolvedSecurityPath {
    anchor: WindowsSecurityAnchor,
    missing_components: Vec<String>,
}

#[cfg(windows)]
fn resolve_windows_security_path(
    path: &Utf8Path,
) -> Result<WindowsResolvedSecurityPath, WorkspaceError> {
    if !path.is_absolute() {
        return Err(WorkspaceError::Message(format!(
            "security path `{path}` is not absolute"
        )));
    }

    let mut cursor = path.to_path_buf();
    let mut missing_components = Vec::new();
    loop {
        match WindowsSecurityAnchor::open(&cursor) {
            Ok(anchor) => {
                if !missing_components.is_empty() && !anchor.is_directory {
                    return Err(WorkspaceError::Message(format!(
                        "nearest existing path `{}` for `{path}` is not a directory",
                        anchor.final_path
                    )));
                }
                missing_components.reverse();
                return Ok(WindowsResolvedSecurityPath {
                    anchor,
                    missing_components,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = cursor.file_name().ok_or_else(|| {
                    WorkspaceError::Message(format!(
                        "no existing ancestor could be opened for `{path}`"
                    ))
                })?;
                missing_components.push(component.to_string());
                cursor = cursor
                    .parent()
                    .ok_or_else(|| {
                        WorkspaceError::Message(format!(
                            "no existing ancestor could be opened for `{path}`"
                        ))
                    })?
                    .to_path_buf();
            }
            Err(error) => {
                return Err(WorkspaceError::Message(format!(
                    "failed to open security path `{cursor}` while resolving `{path}`: {error}"
                )));
            }
        }
    }
}

#[cfg(windows)]
fn windows_exact_relative_components(candidate: &Utf8Path, root: &Utf8Path) -> Option<Vec<String>> {
    let candidate = windows_path_without_extended_namespace(candidate);
    let root = windows_path_without_extended_namespace(root);
    let mut candidate_components = Utf8Path::new(&candidate).components();
    for root_component in Utf8Path::new(&root).components() {
        let candidate_component = candidate_components.next()?;
        if candidate_component.as_str() != root_component.as_str() {
            return None;
        }
    }
    Some(
        candidate_components
            .map(|component| component.as_str().to_string())
            .collect(),
    )
}

#[cfg(windows)]
fn windows_ordinal_eq_ignore_case(left: &str, right: &str) -> Result<bool, WorkspaceError> {
    use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};

    let left: Vec<u16> = left.encode_utf16().collect();
    let right: Vec<u16> = right.encode_utf16().collect();
    let left_len = i32::try_from(left.len()).map_err(|_| {
        WorkspaceError::Message("Windows path component exceeds ordinal comparison limit".into())
    })?;
    let right_len = i32::try_from(right.len()).map_err(|_| {
        WorkspaceError::Message("Windows path component exceeds ordinal comparison limit".into())
    })?;
    let result =
        unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, 1) };
    if result == 0 {
        return Err(WorkspaceError::Message(format!(
            "failed to compare Windows path components: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(result == CSTR_EQUAL)
}

#[cfg(windows)]
fn windows_missing_component_matches(
    anchor: &WindowsSecurityAnchor,
    candidate: &str,
    root: &str,
) -> Result<bool, WorkspaceError> {
    if candidate == root {
        return Ok(true);
    }
    if anchor.case_sensitive_directory()? {
        return Ok(false);
    }
    windows_ordinal_eq_ignore_case(candidate, root)
}

#[cfg(windows)]
fn windows_security_path_is_within(
    candidate: &Utf8Path,
    root: &Utf8Path,
) -> Result<bool, WorkspaceError> {
    let root = resolve_windows_security_path(root)?;
    let candidate = resolve_windows_security_path(candidate)?;

    if root.missing_components.is_empty() {
        let Some(relative) = windows_exact_relative_components(
            &candidate.anchor.final_path,
            &root.anchor.final_path,
        ) else {
            return Ok(false);
        };
        if relative.is_empty() && candidate.anchor.identity != root.anchor.identity {
            return Err(WorkspaceError::Message(format!(
                "existing path identity changed while validating `{}`",
                candidate.anchor.final_path
            )));
        }
        if (!relative.is_empty() || !candidate.missing_components.is_empty())
            && !root.anchor.is_directory
        {
            return Ok(false);
        }
        return Ok(true);
    }

    if !root.anchor.is_directory {
        return Err(WorkspaceError::Message(format!(
            "missing root descends from non-directory `{}`",
            root.anchor.final_path
        )));
    }
    let Some(existing_relative) =
        windows_exact_relative_components(&candidate.anchor.final_path, &root.anchor.final_path)
    else {
        return Ok(false);
    };
    if !existing_relative.is_empty() {
        if windows_missing_component_matches(
            &root.anchor,
            &existing_relative[0],
            &root.missing_components[0],
        )? {
            return Err(WorkspaceError::Message(format!(
                "path namespace changed while validating missing root `{root_path}`",
                root_path = root.anchor.final_path.join(&root.missing_components[0])
            )));
        }
        return Ok(false);
    }
    if candidate.anchor.identity != root.anchor.identity {
        return Err(WorkspaceError::Message(format!(
            "nearest existing ancestor changed while validating `{}`",
            root.anchor.final_path
        )));
    }
    if candidate.missing_components.len() < root.missing_components.len() {
        return Ok(false);
    }
    for (index, root_component) in root.missing_components.iter().enumerate() {
        let candidate_component = &candidate.missing_components[index];
        if candidate_component == root_component {
            continue;
        }
        if index > 0 {
            return Err(WorkspaceError::Message(format!(
                "cannot prove case policy below missing path component `{}`",
                root.missing_components[index - 1]
            )));
        }
        if !windows_missing_component_matches(&root.anchor, candidate_component, root_component)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn boundary_path_is_within(
    candidate: &Utf8Path,
    root: &Utf8Path,
    effective_candidate: &Utf8Path,
    effective_root: &Utf8Path,
) -> Result<bool, WorkspaceError> {
    if !PathGuard::security_path_is_within(candidate, root)? {
        return Ok(false);
    }
    #[cfg(windows)]
    {
        let _ = (effective_candidate, effective_root);
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        Ok(path_is_within(effective_candidate, effective_root))
    }
}

fn boundary_relative_path_from_root(
    candidate: &Utf8Path,
    root: &Utf8Path,
    effective_candidate: &Utf8Path,
    effective_root: &Utf8Path,
) -> Option<Utf8PathBuf> {
    #[cfg(windows)]
    {
        let _ = (candidate, root);
        let components = windows_exact_relative_components(effective_candidate, effective_root)?;
        let mut relative = Utf8PathBuf::new();
        for component in components {
            relative.push(component);
        }
        Some(relative)
    }
    #[cfg(not(windows))]
    {
        let _ = (effective_candidate, effective_root);
        relative_path_from_root(candidate, root)
    }
}

fn security_canonical_path_is_same(candidate: &Utf8Path, expected: &Utf8Path) -> bool {
    #[cfg(windows)]
    {
        windows_exact_relative_components(candidate, expected).is_some_and(|value| value.is_empty())
            && windows_exact_relative_components(expected, candidate)
                .is_some_and(|value| value.is_empty())
    }
    #[cfg(not(windows))]
    {
        path_is_same(candidate, expected)
    }
}

fn security_canonical_path_is_within(candidate: &Utf8Path, root: &Utf8Path) -> bool {
    #[cfg(windows)]
    {
        windows_exact_relative_components(candidate, root).is_some()
    }
    #[cfg(not(windows))]
    {
        path_is_within(candidate, root)
    }
}

fn is_protected_path(
    workspace: &Workspace,
    absolute: &Utf8Path,
    effective_absolute: &Utf8Path,
) -> Result<bool, WorkspaceError> {
    for path in &workspace.protected_paths {
        #[cfg(not(windows))]
        let effective = effective_path_for_boundary(path)?;
        #[cfg(windows)]
        let protected = PathGuard::security_path_is_within(absolute, path)?;
        #[cfg(not(windows))]
        let protected =
            path_is_within(absolute, path) || path_is_within(effective_absolute, &effective);
        if protected {
            return Ok(true);
        }
    }
    #[cfg(windows)]
    let _ = effective_absolute;
    Ok(false)
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

#[cfg(target_os = "macos")]
fn final_path_for_file(file: &std::fs::File) -> Result<Utf8PathBuf, WorkspaceError> {
    use std::ffi::CStr;
    use std::os::fd::AsRawFd as _;

    let mut buffer = [0 as libc::c_char; libc::PATH_MAX as usize];
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) };
    if result == -1 {
        return Err(WorkspaceError::Io(std::io::Error::last_os_error()));
    }
    let value = unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_str()
        .map_err(|error| {
            WorkspaceError::Message(format!("opened path is not valid UTF-8: {error}"))
        })?;
    opened_handle_path_from_text(value)
}

#[cfg(any(test, target_os = "macos"))]
fn opened_handle_path_from_text(value: &str) -> Result<Utf8PathBuf, WorkspaceError> {
    let path = Utf8PathBuf::from(value);
    if !path.is_absolute() {
        return Err(WorkspaceError::Message(
            "opened handle reported a non-absolute path".to_string(),
        ));
    }
    Ok(path)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn final_path_for_file(_file: &std::fs::File) -> Result<Utf8PathBuf, WorkspaceError> {
    Err(WorkspaceError::Message(
        "stable opened-file boundary validation is unsupported on this platform".to_string(),
    ))
}

#[cfg(windows)]
fn normalized_windows_path(path: &Utf8Path) -> String {
    windows_path_without_extended_namespace(path)
        .trim_end_matches('\\')
        .to_string()
}

#[cfg(windows)]
fn windows_path_without_extended_namespace(path: &Utf8Path) -> String {
    let value = path.as_str().replace('/', "\\");
    strip_ascii_case_prefix(&value, "\\\\?\\UNC\\")
        .map(|rest| format!("\\\\{rest}"))
        .or_else(|| value.strip_prefix("\\\\?\\").map(str::to_string))
        .unwrap_or(value)
}

#[cfg(windows)]
fn strip_ascii_case_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
        .then(|| &value[prefix.len()..])
}

fn path_is_same(candidate: &Utf8Path, expected: &Utf8Path) -> bool {
    PathGuard::stable_identity_key(candidate) == PathGuard::stable_identity_key(expected)
}

#[cfg(not(windows))]
pub(crate) fn path_is_within(candidate: &Utf8Path, root: &Utf8Path) -> bool {
    candidate.starts_with(root)
}

#[cfg(windows)]
fn relative_path_from_root(candidate: &Utf8Path, root: &Utf8Path) -> Option<Utf8PathBuf> {
    let candidate = normalized_windows_path(candidate);
    let root = normalized_windows_path(root);
    let mut candidate_components = Utf8Path::new(&candidate).components();
    for root_component in Utf8Path::new(&root).components() {
        let candidate_component = candidate_components.next()?;
        if !candidate_component
            .as_str()
            .eq_ignore_ascii_case(root_component.as_str())
        {
            return None;
        }
    }
    Some(candidate_components.as_path().to_path_buf())
}

#[cfg(not(windows))]
fn relative_path_from_root(candidate: &Utf8Path, root: &Utf8Path) -> Option<Utf8PathBuf> {
    candidate.strip_prefix(root).ok().map(Utf8Path::to_path_buf)
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

    use crate::config::ResolvedConfig;
    use crate::workspace::WorkspaceDiscovery;

    #[cfg(unix)]
    use super::GuardedPath;
    use super::{AccessKind, PathGuard};

    #[cfg(windows)]
    fn enable_case_sensitive_directory(path: &camino::Utf8Path) {
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_CASE_SENSITIVE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FileCaseSensitiveInfo,
            SetFileInformationByHandle,
        };
        use windows_sys::Win32::System::SystemServices::FILE_CS_FLAG_CASE_SENSITIVE_DIR;

        let mut options = std::fs::OpenOptions::new();
        options
            .access_mode(FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
        let directory = options.open(path).expect("open case-sensitive parent");
        let info = FILE_CASE_SENSITIVE_INFO {
            Flags: FILE_CS_FLAG_CASE_SENSITIVE_DIR,
        };
        let result = unsafe {
            SetFileInformationByHandle(
                directory.as_raw_handle() as HANDLE,
                FileCaseSensitiveInfo,
                (&info as *const FILE_CASE_SENSITIVE_INFO).cast(),
                std::mem::size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
            )
        };
        assert_ne!(
            result,
            0,
            "enable per-directory case sensitivity: {}",
            std::io::Error::last_os_error()
        );
    }

    #[cfg(windows)]
    fn assert_sharing_violation(error: &std::io::Error, operation: &str) {
        use windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION;

        assert_eq!(
            error.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION as i32),
            "{operation} must fail because the security anchor denies delete sharing: {error}"
        );
    }

    #[cfg(unix)]
    fn create_directory_alias(target: &camino::Utf8Path, alias: &camino::Utf8Path) {
        std::os::unix::fs::symlink(target, alias).expect("create directory symlink");
    }

    #[cfg(windows)]
    fn create_directory_alias(target: &camino::Utf8Path, alias: &camino::Utf8Path) {
        if std::os::windows::fs::symlink_dir(target, alias).is_ok() {
            return;
        }

        let status = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J", alias.as_str(), target.as_str()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("create directory junction fallback");
        assert!(status.success(), "create directory symlink or junction");
    }

    #[cfg(unix)]
    fn remove_directory_alias(alias: &camino::Utf8Path) {
        std::fs::remove_file(alias).expect("remove directory symlink");
    }

    #[cfg(windows)]
    fn remove_directory_alias(alias: &camino::Utf8Path) {
        std::fs::remove_dir(alias).expect("remove directory symlink or junction");
    }

    #[cfg(unix)]
    fn raced_fifo_guard() -> (tempfile::TempDir, Utf8PathBuf, GuardedPath) {
        use std::os::unix::ffi::OsStrExt as _;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir(&root).expect("workspace root");
        let path = root.join("source.txt");
        std::fs::write(&path, "admitted regular file").expect("seed regular file");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let guarded = PathGuard::require_path(&workspace, &path, AccessKind::Read)
            .expect("guard regular file");

        std::fs::remove_file(&path).expect("remove admitted file");
        let fifo_path = std::ffi::CString::new(path.as_std_path().as_os_str().as_bytes())
            .expect("FIFO path without NUL");
        // SAFETY: `fifo_path` is a live NUL-terminated pathname and the target is absent.
        let created = unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600 as libc::mode_t) };
        assert_eq!(
            created,
            0,
            "create raced FIFO: {}",
            std::io::Error::last_os_error()
        );
        (temp, path, guarded)
    }

    #[cfg(unix)]
    fn assert_fifo_open_is_nonblocking(
        guarded: GuardedPath,
        path: Utf8PathBuf,
        open: fn(&GuardedPath) -> Result<std::fs::File, crate::error::WorkspaceError>,
        operation: &str,
    ) {
        use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
        use std::time::{Duration, Instant};

        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let outcome = open(&guarded)
                .and_then(|file| file.metadata().map_err(crate::error::WorkspaceError::from))
                .map(|metadata| metadata.file_type().is_fifo())
                .map_err(|error| error.to_string());
            sender.send(outcome).expect("send open outcome");
        });

        let mut blocked = false;
        let outcome = match receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                blocked = true;
                let release_deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&path)
                    {
                        Ok(writer) => {
                            drop(writer);
                            break;
                        }
                        Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                            assert!(
                                Instant::now() < release_deadline,
                                "blocked {operation} FIFO reader could not be released"
                            );
                            std::thread::yield_now();
                        }
                        Err(error) => panic!("release blocked {operation} FIFO reader: {error}"),
                    }
                }
                receiver
                    .recv_timeout(Duration::from_secs(2))
                    .expect("released FIFO open must return")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("{operation} worker disconnected before publishing its result")
            }
        };
        worker.join().expect("join FIFO open worker");

        assert!(!blocked, "{operation} blocked while opening a raced FIFO");
        assert_eq!(outcome, Ok(true), "{operation} must retain the FIFO handle");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn ancestor_directory_aliases_preserve_effective_authority_classification() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir_all(root.join(".moyai/rules/team")).expect("rule directory");
        std::fs::create_dir_all(root.join(".agents/skills/example")).expect("skill directory");
        std::fs::create_dir_all(root.join("src")).expect("normal directory");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let rules_alias = root.join("rules-alias");
        let skills_alias = root.join("skills-alias");
        create_directory_alias(&root.join(".moyai/rules"), &rules_alias);
        create_directory_alias(&root.join(".agents/skills"), &skills_alias);

        for requested in [
            rules_alias.join("team/policy.md"),
            skills_alias.join("example/SKILL.md"),
        ] {
            assert!(
                !crate::workspace::special_paths::is_protected_workspace_authority_path(
                    &root, &requested
                ),
                "the lexical alias must not independently reveal its authority target"
            );
            let guarded = PathGuard::require_path(&workspace, &requested, AccessKind::Edit)
                .expect("guard aliased authority target");
            assert!(PathGuard::targets_protected_workspace_authority(
                &root, &guarded
            ));
        }

        let normal =
            PathGuard::require_path(&workspace, &root.join("src/generated.rs"), AccessKind::Edit)
                .expect("guard normal target");
        assert!(!PathGuard::targets_protected_workspace_authority(
            &root, &normal
        ));

        remove_directory_alias(&rules_alias);
        remove_directory_alias(&skills_alias);
    }

    #[cfg(windows)]
    #[test]
    fn missing_case_variant_descendant_of_protected_path_is_rejected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let mut workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
                .expect("workspace");
        workspace.protected_paths.push(root.join("protected"));

        let error = PathGuard::require_path(
            &workspace,
            &root.join("PROTECTED/new-file.txt"),
            AccessKind::Read,
        )
        .expect_err("case variant must remain protected before the path exists");

        assert!(error.to_string().contains("is protected"));

        workspace.protected_paths.push(root.join("Prötected"));
        let unicode_error = PathGuard::require_path(
            &workspace,
            &root.join("PRÖTECTED/new-file.txt"),
            AccessKind::Read,
        )
        .expect_err("Unicode case alias must follow the directory policy");
        assert!(unicode_error.to_string().contains("is protected"));
    }

    #[cfg(windows)]
    #[test]
    fn unknown_case_policy_below_missing_component_fails_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let mut workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
                .expect("workspace");
        workspace.protected_paths.push(root.join("missing/Child"));

        let error = PathGuard::require_path(
            &workspace,
            &root.join("missing/CHILD/file.txt"),
            AccessKind::Read,
        )
        .expect_err("missing parent policy is unknowable");

        assert!(
            error
                .to_string()
                .contains("cannot prove case policy below missing path component")
        );
    }

    #[cfg(unix)]
    #[test]
    fn missing_case_variant_of_protected_path_remains_distinct() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let mut workspace =
            WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
                .expect("workspace");
        workspace.protected_paths.push(root.join("protected"));

        PathGuard::require_path(
            &workspace,
            &root.join("PROTECTED/new-file.txt"),
            AccessKind::Read,
        )
        .expect("Unix path components remain case-sensitive");
    }

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
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

    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
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

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn validated_opens_preserve_search_only_parent_access() {
        use std::os::unix::fs::PermissionsExt as _;

        struct PermissionRestore {
            path: Utf8PathBuf,
            permissions: Option<std::fs::Permissions>,
        }

        impl Drop for PermissionRestore {
            fn drop(&mut self) {
                if let Some(permissions) = self.permissions.take() {
                    let _ = std::fs::set_permissions(&self.path, permissions);
                }
            }
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        let parent = root.join("search-only");
        std::fs::create_dir_all(&parent).expect("create parent");
        let path = parent.join("file.txt");
        std::fs::write(&path, "content").expect("seed file");
        let guarded = PathGuard::trusted_internal_path(&path, &root).expect("guard path");

        let original_permissions = std::fs::metadata(&parent)
            .expect("parent metadata")
            .permissions();
        let restore = PermissionRestore {
            path: parent.clone(),
            permissions: Some(original_permissions.clone()),
        };
        let mut search_only = original_permissions;
        search_only.set_mode(0o111);
        std::fs::set_permissions(&parent, search_only).expect("make parent search-only");

        std::fs::File::open(&path).expect("search permission must allow a direct file open");
        let parent_error = match std::fs::File::open(&parent) {
            Ok(_) => {
                // Privileged test processes can bypass the read-permission distinction.
                drop(restore);
                return;
            }
            Err(error) => error,
        };
        assert_eq!(
            parent_error.kind(),
            std::io::ErrorKind::PermissionDenied,
            "the fixture must isolate directory read permission"
        );

        let read = PathGuard::open_validated_read_file(&guarded);
        let metadata = PathGuard::open_validated_metadata_handle(&guarded);
        drop(restore);

        let read = read.expect("validated read through search-only parent");
        PathGuard::validate_open_file(&guarded, &read).expect("validated read identity");
        let metadata = metadata.expect("validated metadata through search-only parent");
        PathGuard::validate_open_file(&guarded, &metadata).expect("validated metadata identity");
    }

    #[cfg(unix)]
    #[test]
    fn validated_read_open_does_not_block_on_a_raced_fifo() {
        let (_temp, path, guarded) = raced_fifo_guard();

        assert_fifo_open_is_nonblocking(
            guarded,
            path,
            PathGuard::open_validated_read_file,
            "validated read open",
        );
    }

    #[cfg(unix)]
    #[test]
    fn validated_metadata_open_does_not_block_on_a_raced_fifo() {
        let (_temp, path, guarded) = raced_fifo_guard();

        assert_fifo_open_is_nonblocking(
            guarded,
            path,
            PathGuard::open_validated_metadata_handle,
            "validated metadata open",
        );
    }

    #[test]
    fn opened_handle_path_conversion_does_not_reopen_a_removed_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let reported = root.join("reported.txt");
        std::fs::write(&reported, "opened object").expect("seed reported path");
        std::fs::remove_file(&reported).expect("remove reported namespace entry");

        let converted = super::opened_handle_path_from_text(reported.as_str())
            .expect("handle-derived path must not be reopened by name");

        assert_eq!(converted, reported);
        assert!(super::opened_handle_path_from_text("relative/reported.txt").is_err());
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

    #[cfg(windows)]
    #[test]
    fn case_variants_keep_workspace_and_additional_root_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("Workspace")).expect("utf8 root");
        let external =
            Utf8PathBuf::from_path_buf(temp.path().join("External")).expect("utf8 external");
        std::fs::create_dir_all(root.join("Nested")).expect("workspace tree");
        std::fs::create_dir_all(&external).expect("external root");
        std::fs::write(root.join("Nested/file.txt"), "workspace").expect("workspace file");
        std::fs::write(external.join("external.txt"), "external").expect("external file");

        let mut config = ResolvedConfig::default();
        config.permissions.additional_read_roots =
            vec![Utf8PathBuf::from(external.as_str().to_ascii_uppercase())];
        config.permissions.additional_write_roots =
            vec![Utf8PathBuf::from(external.as_str().to_ascii_uppercase())];
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");

        let workspace_request =
            Utf8PathBuf::from(root.join("Nested/file.txt").as_str().to_ascii_uppercase());
        let workspace_guard =
            PathGuard::require_path(&workspace, &workspace_request, AccessKind::Read)
                .expect("case-insensitive workspace path");
        assert!(workspace_guard.inside_workspace);
        assert!(
            workspace_guard
                .relative_to_root
                .as_str()
                .replace('\\', "/")
                .eq_ignore_ascii_case("Nested/file.txt")
        );

        let external_request =
            Utf8PathBuf::from(external.join("external.txt").as_str().to_ascii_lowercase());
        for access in [AccessKind::Read, AccessKind::Edit] {
            let external_guard = PathGuard::require_path(&workspace, &external_request, access)
                .expect("case-insensitive additional root");
            assert!(!external_guard.inside_workspace);
            assert!(external_guard.trusted_external);
        }

        let extended_workspace = Utf8PathBuf::from(format!(
            r"\\?\{}",
            root.join("Nested/file.txt")
                .as_str()
                .replace('/', "\\")
                .to_ascii_uppercase()
        ));
        PathGuard::require_path(&workspace, &extended_workspace, AccessKind::Read)
            .expect("extended DOS alias remains inside the workspace");
        assert!(
            PathGuard::same_existing_namespace_entry(
                &root.join("Nested/file.txt"),
                &root.join("NESTED/FILE.TXT"),
            )
            .expect("namespace comparison")
        );
    }

    #[cfg(windows)]
    #[test]
    fn unicode_existing_alias_follows_windows_directory_policy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("UnicodeÄrea")).expect("utf8 root");
        std::fs::create_dir(&root).expect("workspace root");
        std::fs::write(root.join("Ünicode.txt"), "content").expect("workspace file");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");
        let alias = Utf8PathBuf::from(
            root.join("Ünicode.txt")
                .as_str()
                .replace("UnicodeÄrea", "unicodeäREA")
                .replace("Ünicode.txt", "ünicode.TXT"),
        );

        let guarded = PathGuard::require_path(&workspace, &alias, AccessKind::Read)
            .expect("Unicode case alias follows the filesystem policy");
        assert_eq!(guarded.relative_to_root, "Ünicode.txt");
    }

    #[cfg(windows)]
    #[test]
    fn case_sensitive_sibling_is_not_inside_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent =
            Utf8PathBuf::from_path_buf(temp.path().join("case-parent")).expect("utf8 parent");
        std::fs::create_dir(&parent).expect("case parent");
        enable_case_sensitive_directory(&parent);
        let root = parent.join("Root");
        let sibling = parent.join("root");
        std::fs::create_dir(&root).expect("workspace root");
        std::fs::create_dir(&sibling).expect("case-distinct sibling");
        std::fs::write(sibling.join("secret.txt"), "outside").expect("sibling file");
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &ResolvedConfig::default())
            .expect("workspace");

        let error =
            PathGuard::require_path(&workspace, &sibling.join("secret.txt"), AccessKind::Read)
                .expect_err("case-distinct sibling must remain outside");

        assert!(error.to_string().contains("outside the allowed roots"));
        assert!(
            !PathGuard::same_existing_namespace_entry(
                &root.join("entry.txt"),
                &sibling.join("entry.txt"),
            )
            .expect("namespace comparison")
        );
    }

    #[cfg(windows)]
    #[test]
    fn missing_additional_root_uses_the_existing_parent_case_policy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let parent =
            Utf8PathBuf::from_path_buf(temp.path().join("case-parent")).expect("utf8 parent");
        std::fs::create_dir(&parent).expect("case parent");
        enable_case_sensitive_directory(&parent);
        let root = parent.join("Workspace");
        std::fs::create_dir(&root).expect("workspace root");

        let missing_additional_root = parent.join("Allowed");
        let mut config = ResolvedConfig::default();
        config.permissions.additional_read_roots = vec![missing_additional_root.clone()];
        let workspace = WorkspaceDiscovery::discover_fixed_root(&root, &config).expect("workspace");

        let allowed = PathGuard::require_path(
            &workspace,
            &missing_additional_root.join("new-file.txt"),
            AccessKind::Read,
        )
        .expect("correct-case path under missing additional root");
        assert!(!allowed.inside_workspace);
        assert!(allowed.trusted_external);

        let error = PathGuard::require_path(
            &workspace,
            &parent.join("allowed/new-file.txt"),
            AccessKind::Read,
        )
        .expect_err("case-distinct missing root must remain outside");
        let crate::error::WorkspaceError::Message(message) = error else {
            panic!("outside-root rejection must remain a typed workspace message");
        };
        assert!(message.contains("outside the allowed roots"));
    }

    #[cfg(windows)]
    #[test]
    fn security_anchor_blocks_directory_namespace_mutation_until_drop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");

        let rename_source = root.join("rename-source");
        let rename_target = root.join("rename-target");
        std::fs::create_dir(&rename_source).expect("rename source");
        let rename_anchor =
            super::WindowsSecurityAnchor::open(&rename_source).expect("rename security anchor");
        assert_eq!(
            rename_anchor.final_path,
            PathGuard::opened_file_identity_path(&rename_anchor.file)
                .expect("same-handle final path snapshot")
        );
        assert_eq!(
            rename_anchor.identity,
            super::windows_file_identity(&rename_anchor.file)
                .expect("same-handle file identity snapshot")
        );

        let rename_error = std::fs::rename(&rename_source, &rename_target)
            .expect_err("held security anchor must block directory rename");
        assert_sharing_violation(&rename_error, "directory rename");
        drop(rename_anchor);
        std::fs::rename(&rename_source, &rename_target)
            .expect("directory rename after security anchor drop");

        let delete_source = root.join("delete-source");
        std::fs::create_dir(&delete_source).expect("delete source");
        let delete_anchor =
            super::WindowsSecurityAnchor::open(&delete_source).expect("delete security anchor");
        let delete_error = std::fs::remove_dir(&delete_source)
            .expect_err("held security anchor must block directory deletion");
        assert_sharing_violation(&delete_error, "directory delete");
        drop(delete_anchor);
        std::fs::remove_dir(&delete_source).expect("directory deletion after security anchor drop");

        let file_source = root.join("file-source.txt");
        let file_target = root.join("file-target.txt");
        std::fs::write(&file_source, "entry").expect("file source");
        let file_anchor =
            super::WindowsSecurityAnchor::open(&file_source).expect("file security anchor");
        let file_rename_error = std::fs::rename(&file_source, &file_target)
            .expect_err("held security anchor must block file rename");
        assert_sharing_violation(&file_rename_error, "file rename");
        let file_delete_error = std::fs::remove_file(&file_source)
            .expect_err("held security anchor must block file deletion");
        assert_sharing_violation(&file_delete_error, "file delete");
        drop(file_anchor);
        std::fs::rename(&file_source, &file_target)
            .expect("file rename after security anchor drop");
        std::fs::remove_file(&file_target).expect("renamed file cleanup");

        std::fs::remove_dir(&rename_target).expect("renamed directory cleanup");
    }

    #[cfg(windows)]
    #[test]
    fn windows_relative_projection_normalizes_extended_and_unc_prefix_forms() {
        let drive_relative = PathGuard::relative_path_from_root(
            camino::Utf8Path::new(r"\\?\C:\Workspace\Nested\File.txt"),
            camino::Utf8Path::new(r"c:\workspace"),
        )
        .expect("extended DOS path projection");
        assert_eq!(drive_relative, "Nested/File.txt");

        let unc_relative = PathGuard::relative_path_from_root(
            camino::Utf8Path::new(r"\\?\UNC\Server\Share\Workspace\File.txt"),
            camino::Utf8Path::new(r"\\server\share\workspace"),
        )
        .expect("extended UNC path projection");
        assert_eq!(unc_relative, "File.txt");

        let mixed_case_unc_relative = PathGuard::relative_path_from_root(
            camino::Utf8Path::new(r"\\?\uNc\SERVER\SHARE\Workspace\Nested\File.txt"),
            camino::Utf8Path::new(r"\\server\share\workspace"),
        )
        .expect("mixed-case extended UNC path projection");
        assert_eq!(mixed_case_unc_relative, "Nested/File.txt");
        assert!(PathGuard::same_path_identity(
            camino::Utf8Path::new(r"\\?\unc\Server\Share\Workspace"),
            camino::Utf8Path::new(r"\\server\share\workspace"),
        ));
        assert_eq!(
            PathGuard::stable_identity_key(camino::Utf8Path::new(r"\\?\C:\Workspace")),
            PathGuard::stable_identity_key(camino::Utf8Path::new(r"c:\workspace")),
        );
        assert_eq!(
            PathGuard::stable_identity_key(camino::Utf8Path::new(
                r"\\?\uNc\SERVER\SHARE\Workspace"
            )),
            PathGuard::stable_identity_key(camino::Utf8Path::new(r"\\server\share\workspace")),
        );
        assert!(
            super::windows_exact_relative_components(
                camino::Utf8Path::new(r"\\?\UNC\Server\Share\Workspace\File.txt"),
                camino::Utf8Path::new(r"\\Server\Share\Workspace"),
            )
            .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_path_identity_remains_case_sensitive() {
        let upper = camino::Utf8Path::new("/tmp/Workspace/File.txt");
        let lower = camino::Utf8Path::new("/tmp/workspace/file.txt");

        assert!(!PathGuard::same_path_identity(upper, lower));
        assert_ne!(
            PathGuard::stable_identity_key(upper),
            PathGuard::stable_identity_key(lower)
        );
        assert_eq!(PathGuard::stable_identity_key(upper), upper.as_str());
        assert!(!super::path_is_within(
            upper,
            camino::Utf8Path::new("/tmp/workspace")
        ));
    }
}
