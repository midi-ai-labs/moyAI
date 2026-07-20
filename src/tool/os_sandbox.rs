use std::fmt::{Display, Formatter};
use std::fs::OpenOptions;
use std::io::{Read, Seek};

use camino::{Utf8Path, Utf8PathBuf};
use sha2::{Digest, Sha256};

use crate::config::{AccessMode, ResolvedConfig};
use crate::workspace::{PathGuard, Workspace};

const MAX_GITDIR_FILE_BYTES: u64 = 16 * 1024;
pub(crate) const MAX_PINNED_PROTECTED_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_AUTHORITY_DIRECTORY_ENTRIES: usize = 4_096;

/// The process isolation selected after the current access mode has been read.
///
/// This type describes the intended enforcement boundary. Platform launch code
/// must still fail closed if it cannot enforce a `WorkspaceWrite` plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProcessSandboxPlan {
    /// This admission does not authorize a child process. Keeping this distinct
    /// from `Unrestricted` prevents a future process-bearing consumer from
    /// silently inheriting authority that was never reviewed.
    NoProcess,
    WorkspaceWrite(WorkspaceWriteSandboxProfile),
    Unrestricted,
}

impl ProcessSandboxPlan {
    #[cfg(test)]
    pub(crate) fn for_access_mode(
        access_mode: AccessMode,
        workspace: &Workspace,
    ) -> Result<Self, SandboxProfileError> {
        Self::for_access_mode_with_config(access_mode, workspace, &ResolvedConfig::default())
    }

    pub(crate) fn for_access_mode_with_config(
        access_mode: AccessMode,
        workspace: &Workspace,
        config: &ResolvedConfig,
    ) -> Result<Self, SandboxProfileError> {
        match access_mode {
            AccessMode::Default | AccessMode::AutoReview => Ok(Self::WorkspaceWrite(
                WorkspaceWriteSandboxProfile::from_workspace(workspace, config)?,
            )),
            AccessMode::FullAccess => Ok(Self::Unrestricted),
        }
    }

    pub(crate) fn audit_description(&self) -> String {
        match self {
            Self::NoProcess => "no_process_authorized".to_string(),
            Self::WorkspaceWrite(profile) => profile.audit_description(),
            Self::Unrestricted => "unrestricted".to_string(),
        }
    }
}

/// What the native restricted-token fallback can truthfully claim about
/// network isolation. Environment hints can reduce accidental access, but do
/// not constitute an OS-enforced network boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SandboxNetworkPolicy {
    AdvisoryOfflineEnvironment,
}

impl SandboxNetworkPolicy {
    pub(crate) fn audit_label(self) -> &'static str {
        match self {
            Self::AdvisoryOfflineEnvironment => "advisory_offline_environment",
        }
    }

    pub(crate) fn is_os_enforced(self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkspaceWriteSandboxProfile {
    pub(crate) writable_roots: Vec<SandboxPathSnapshot>,
    pub(crate) read_only_roots: Vec<SandboxPathSnapshot>,
    pub(crate) network: SandboxNetworkPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxPathSnapshot {
    /// The normalized path selected when the permission decision was made.
    pub(crate) requested: Utf8PathBuf,
    /// The final path observed through the opened object handle at that time.
    pub(crate) canonical: Utf8PathBuf,
    /// Exact content identity for a protected regular file. This closes the
    /// same-object in-place rewrite gap that a volume/file ID cannot detect.
    pub(crate) content_sha256: Option<[u8; 32]>,
    #[cfg(windows)]
    pub(crate) identity: WindowsSandboxObjectIdentity,
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WindowsSandboxObjectIdentity {
    Extended {
        volume_serial_number: u64,
        file_id: [u8; 16],
    },
    Legacy {
        volume_serial_number: u32,
        file_index: u64,
    },
}

impl WorkspaceWriteSandboxProfile {
    pub(crate) fn from_workspace(
        workspace: &Workspace,
        config: &ResolvedConfig,
    ) -> Result<Self, SandboxProfileError> {
        let mut temp_roots = Vec::new();
        if let Ok(path) = Utf8PathBuf::from_path_buf(std::env::temp_dir()) {
            if path.is_dir() {
                temp_roots.push(path);
            }
        }
        for key in ["TEMP", "TMP"] {
            let Some(path) =
                std::env::var_os(key).and_then(|value| Utf8PathBuf::from_os_string(value).ok())
            else {
                continue;
            };
            if path.is_dir() {
                temp_roots.push(path);
            }
        }

        Self::compile_with_instructions(
            workspace,
            temp_roots,
            &config.instructions.additional_files,
        )
    }

    #[cfg(test)]
    fn compile(
        workspace: &Workspace,
        temp_roots: impl IntoIterator<Item = Utf8PathBuf>,
    ) -> Result<Self, SandboxProfileError> {
        Self::compile_with_instructions(workspace, temp_roots, &[])
    }

    fn compile_with_instructions(
        workspace: &Workspace,
        temp_roots: impl IntoIterator<Item = Utf8PathBuf>,
        configured_instruction_files: &[Utf8PathBuf],
    ) -> Result<Self, SandboxProfileError> {
        let mut writable_roots = Vec::new();
        writable_roots.push(validate_writable_root(&workspace.root, "workspace")?);
        for path in &workspace.path_policy.additional_write_roots {
            writable_roots.push(validate_writable_root(path, "additional write")?);
        }
        for path in temp_roots {
            writable_roots.push(validate_writable_root(&path, "temporary")?);
        }
        sort_and_dedupe_snapshots(&mut writable_roots);
        let writable_paths = writable_roots
            .iter()
            .map(|root| root.requested.clone())
            .collect::<Vec<_>>();

        let mut protected_paths = Vec::new();
        let mut read_only_roots = Vec::new();
        for dot_git in existing_named_authority_paths(&workspace.root, &[".git"])? {
            if let Some((git_file, git_dir)) = resolve_git_directory(&dot_git, &workspace.root)? {
                read_only_roots.push(git_file);
                protected_paths.push(git_dir);
            } else {
                protected_paths.push(dot_git);
            }
        }
        protected_paths.extend(existing_named_authority_paths(
            &workspace.root,
            &[".moyai", ".agents", ".claude", ".codex"],
        )?);
        protected_paths.extend(existing_instruction_authority_paths(workspace)?);
        protected_paths.extend(existing_configured_instruction_authority_paths(
            workspace,
            configured_instruction_files,
        )?);
        for protected in &workspace.protected_paths {
            let protected = normalize_absolute(protected, "protected")?;
            if protected_overlaps_writable_root(&protected, &writable_paths)? {
                if !protected.exists() {
                    return Err(SandboxProfileError::new(format!(
                        "configured protected sandbox path `{protected}` is unavailable"
                    )));
                }
                protected_paths.push(protected);
            }
        }
        normalize_sort_and_dedupe(&mut protected_paths);
        reject_writable_roots_inside_protected_paths(&writable_paths, &protected_paths)?;
        read_only_roots.extend(
            protected_paths
                .iter()
                .map(|path| snapshot_existing_path(path, "protected path", false))
                .collect::<Result<Vec<_>, _>>()?,
        );
        sort_and_dedupe_snapshots(&mut read_only_roots);

        Ok(Self {
            writable_roots,
            read_only_roots,
            network: SandboxNetworkPolicy::AdvisoryOfflineEnvironment,
        })
    }

    pub(crate) fn audit_description(&self) -> String {
        format!(
            "workspace_write(writable_roots={}, read_only_roots={}, network={}, network_os_enforced={}, world_writable_audit=bounded_best_effort)",
            self.writable_roots.len(),
            self.read_only_roots.len(),
            self.network.audit_label(),
            self.network.is_os_enforced(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxProfileError {
    message: String,
}

impl SandboxProfileError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for SandboxProfileError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SandboxProfileError {}

fn validate_writable_root(
    path: &Utf8Path,
    root_kind: &'static str,
) -> Result<SandboxPathSnapshot, SandboxProfileError> {
    snapshot_existing_path(path, root_kind, true)
}

fn snapshot_existing_path(
    path: &Utf8Path,
    path_kind: &'static str,
    require_directory: bool,
) -> Result<SandboxPathSnapshot, SandboxProfileError> {
    let requested = normalize_absolute(path, path_kind)?;
    let file = open_snapshot_path(&requested).map_err(|error| {
        SandboxProfileError::new(format!(
            "{path_kind} sandbox path `{requested}` is unavailable: {error}"
        ))
    })?;
    let metadata = file.metadata().map_err(|error| {
        SandboxProfileError::new(format!(
            "failed to inspect opened {path_kind} sandbox path `{requested}`: {error}"
        ))
    })?;
    snapshot_opened_path(
        requested,
        &file,
        &metadata,
        path_kind,
        require_directory,
        None,
    )
}

fn snapshot_opened_path(
    requested: Utf8PathBuf,
    file: &std::fs::File,
    metadata: &std::fs::Metadata,
    path_kind: &'static str,
    require_directory: bool,
    pinned_content: Option<&[u8]>,
) -> Result<SandboxPathSnapshot, SandboxProfileError> {
    if require_directory && !metadata.is_dir() {
        return Err(SandboxProfileError::new(format!(
            "{path_kind} sandbox path `{requested}` is not a directory"
        )));
    }
    validate_windows_root(&requested, &metadata)?;
    let canonical = PathGuard::opened_file_identity_path(&file).map_err(|error| {
        SandboxProfileError::new(format!(
            "failed to resolve opened {path_kind} sandbox path `{requested}`: {error}"
        ))
    })?;
    validate_windows_canonical_local_path(&canonical)?;
    let content_sha256 = if metadata.is_file() {
        let content = match pinned_content {
            Some(content) => {
                if u64::try_from(content.len()).unwrap_or(u64::MAX)
                    > MAX_PINNED_PROTECTED_FILE_BYTES
                {
                    return Err(SandboxProfileError::new(format!(
                        "protected sandbox file `{requested}` exceeds {MAX_PINNED_PROTECTED_FILE_BYTES} bytes"
                    )));
                }
                content.to_vec()
            }
            None => read_opened_file_bounded(file, MAX_PINNED_PROTECTED_FILE_BYTES, &requested)?,
        };
        Some(Sha256::digest(&content).into())
    } else {
        None
    };

    Ok(SandboxPathSnapshot {
        requested,
        canonical,
        content_sha256,
        #[cfg(windows)]
        identity: windows_object_identity(&file)?,
    })
}

fn read_opened_file_bounded(
    file: &std::fs::File,
    maximum_bytes: u64,
    path: &Utf8Path,
) -> Result<Vec<u8>, SandboxProfileError> {
    let mut reader = file.try_clone().map_err(|error| {
        SandboxProfileError::new(format!(
            "failed to clone protected sandbox file handle `{path}`: {error}"
        ))
    })?;
    reader.rewind().map_err(|error| {
        SandboxProfileError::new(format!(
            "failed to rewind protected sandbox file `{path}`: {error}"
        ))
    })?;
    let mut bytes = Vec::new();
    reader
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| {
            SandboxProfileError::new(format!(
                "failed to read protected sandbox file `{path}`: {error}"
            ))
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_bytes {
        return Err(SandboxProfileError::new(format!(
            "protected sandbox file `{path}` exceeds {maximum_bytes} bytes"
        )));
    }
    Ok(bytes)
}

fn open_snapshot_path(path: &Utf8Path) -> Result<std::fs::File, std::io::Error> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        };
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(windows)]
fn windows_object_identity(
    file: &std::fs::File,
) -> Result<WindowsSandboxObjectIdentity, SandboxProfileError> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandle,
        GetFileInformationByHandleEx,
    };

    let handle = file.as_raw_handle() as HANDLE;
    let mut extended: FILE_ID_INFO = unsafe { std::mem::zeroed() };
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut extended as *mut FILE_ID_INFO).cast(),
            u32::try_from(std::mem::size_of::<FILE_ID_INFO>())
                .expect("file identity size fits u32"),
        )
    } != 0
    {
        return Ok(WindowsSandboxObjectIdentity::Extended {
            volume_serial_number: extended.VolumeSerialNumber,
            file_id: extended.FileId.Identifier,
        });
    }

    let mut legacy: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle, &mut legacy) } == 0 {
        return Err(SandboxProfileError::new(format!(
            "failed to read opened sandbox object identity: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(WindowsSandboxObjectIdentity::Legacy {
        volume_serial_number: legacy.dwVolumeSerialNumber,
        file_index: ((legacy.nFileIndexHigh as u64) << 32) | legacy.nFileIndexLow as u64,
    })
}

fn normalize_absolute(
    path: &Utf8Path,
    path_kind: &'static str,
) -> Result<Utf8PathBuf, SandboxProfileError> {
    if !path.is_absolute() {
        return Err(SandboxProfileError::new(format!(
            "{path_kind} sandbox path `{path}` is not absolute"
        )));
    }
    crate::workspace::project::normalize_path(Utf8Path::new("."), path).map_err(|error| {
        SandboxProfileError::new(format!(
            "failed to normalize {path_kind} sandbox path `{path}`: {error}"
        ))
    })
}

#[cfg(windows)]
fn validate_windows_root(
    path: &Utf8Path,
    metadata: &std::fs::Metadata,
) -> Result<(), SandboxProfileError> {
    use std::os::windows::fs::MetadataExt;
    use std::path::{Component, Prefix};

    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    if matches!(
        path.as_std_path().components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _))
    ) {
        return Err(SandboxProfileError::new(format!(
            "sandbox writable root `{path}` cannot be a UNC path"
        )));
    }
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(SandboxProfileError::new(format!(
            "sandbox writable root `{path}` cannot be a reparse point"
        )));
    }
    Ok(())
}

fn existing_instruction_authority_paths(
    workspace: &Workspace,
) -> Result<Vec<Utf8PathBuf>, SandboxProfileError> {
    let root = normalize_absolute(&workspace.root, "workspace")?;
    let mut current = normalize_absolute(&workspace.cwd, "workspace cwd")?;
    if !PathGuard::security_path_is_within(&current, &root).map_err(|error| {
        SandboxProfileError::new(format!(
            "failed to validate workspace instruction authority path `{current}`: {error}"
        ))
    })? {
        return Err(SandboxProfileError::new(format!(
            "workspace cwd `{current}` is outside sandbox root `{root}`"
        )));
    }

    let mut paths = Vec::new();
    loop {
        paths.extend(existing_named_authority_paths(
            &current,
            crate::workspace::instruction_file_names(),
        )?);
        if PathGuard::stable_identity_key(&current) == PathGuard::stable_identity_key(&root) {
            break;
        }
        current = current.parent().map(Utf8Path::to_path_buf).ok_or_else(|| {
            SandboxProfileError::new(format!(
                "workspace cwd ancestry did not reach sandbox root `{root}`"
            ))
        })?;
    }
    Ok(paths)
}

fn existing_named_authority_paths(
    directory: &Utf8Path,
    names: &[&str],
) -> Result<Vec<Utf8PathBuf>, SandboxProfileError> {
    let directory = normalize_absolute(directory, "authority directory")?;
    if !PathGuard::directory_is_case_sensitive(&directory).map_err(|error| {
        SandboxProfileError::new(format!(
            "failed to inspect authority directory `{directory}` case policy: {error}"
        ))
    })? {
        return names
            .iter()
            .filter_map(|name| {
                let candidate = directory.join(name);
                candidate.exists().then_some(candidate)
            })
            .map(|path| normalize_absolute(&path, "authority path"))
            .collect();
    }

    let entries = std::fs::read_dir(&directory).map_err(|error| {
        SandboxProfileError::new(format!(
            "failed to enumerate case-sensitive authority directory `{directory}`: {error}"
        ))
    })?;
    let mut paths = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index >= MAX_AUTHORITY_DIRECTORY_ENTRIES {
            return Err(SandboxProfileError::new(format!(
                "case-sensitive authority directory `{directory}` exceeds the {MAX_AUTHORITY_DIRECTORY_ENTRIES} entry sandbox limit"
            )));
        }
        let entry = entry.map_err(|error| {
            SandboxProfileError::new(format!(
                "failed to enumerate case-sensitive authority directory `{directory}`: {error}"
            ))
        })?;
        let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if names
            .iter()
            .any(|candidate| file_name.eq_ignore_ascii_case(candidate))
        {
            let path = Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| {
                SandboxProfileError::new(format!(
                    "authority directory `{directory}` contains a non-UTF-8 path: {}",
                    path.display()
                ))
            })?;
            paths.push(normalize_absolute(&path, "authority path")?);
        }
    }
    Ok(paths)
}

fn existing_configured_instruction_authority_paths(
    workspace: &Workspace,
    configured_files: &[Utf8PathBuf],
) -> Result<Vec<Utf8PathBuf>, SandboxProfileError> {
    let root = normalize_absolute(&workspace.root, "workspace")?;
    let mut paths = Vec::new();
    for configured in configured_files {
        let candidate = if configured.is_absolute() {
            configured.clone()
        } else {
            root.join(configured)
        };
        let candidate = normalize_absolute(&candidate, "configured instruction")?;
        if !configured.is_absolute()
            && !PathGuard::security_path_is_within(&candidate, &root).map_err(|error| {
                SandboxProfileError::new(format!(
                    "failed to validate configured instruction path `{candidate}`: {error}"
                ))
            })?
        {
            return Err(SandboxProfileError::new(format!(
                "relative configured instruction `{configured}` escapes workspace root `{root}`"
            )));
        }
        let metadata = match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(SandboxProfileError::new(format!(
                    "failed to inspect configured instruction `{candidate}`: {error}"
                )));
            }
        };
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(SandboxProfileError::new(format!(
                    "configured instruction `{candidate}` is a reparse point and cannot be safely pinned"
                )));
            }
        }
        #[cfg(not(windows))]
        if metadata.file_type().is_symlink() {
            return Err(SandboxProfileError::new(format!(
                "configured instruction `{candidate}` is a symlink and cannot be safely pinned"
            )));
        }
        if metadata.is_file() {
            paths.push(candidate);
        }
    }
    Ok(paths)
}

#[cfg(not(windows))]
fn validate_windows_root(
    _path: &Utf8Path,
    _metadata: &std::fs::Metadata,
) -> Result<(), SandboxProfileError> {
    Ok(())
}

#[cfg(windows)]
fn validate_windows_canonical_local_path(path: &Utf8Path) -> Result<(), SandboxProfileError> {
    use std::path::{Component, Prefix};

    if matches!(
        path.as_std_path().components().next(),
        Some(Component::Prefix(prefix))
            if matches!(
                prefix.kind(),
                Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) | Prefix::DeviceNS(_)
            )
    ) {
        return Err(SandboxProfileError::new(format!(
            "sandbox path `{path}` resolves to a remote or device namespace"
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn validate_windows_canonical_local_path(_path: &Utf8Path) -> Result<(), SandboxProfileError> {
    Ok(())
}

fn resolve_git_directory(
    dot_git: &Utf8Path,
    workspace_root: &Utf8Path,
) -> Result<Option<(SandboxPathSnapshot, Utf8PathBuf)>, SandboxProfileError> {
    let dot_git = normalize_absolute(dot_git, "protected git path")?;
    let file = match open_snapshot_path(&dot_git) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(SandboxProfileError::new(format!(
                "failed to open protected git path `{dot_git}`: {error}"
            )));
        }
    };
    let metadata = file.metadata().map_err(|error| {
        SandboxProfileError::new(format!(
            "failed to inspect opened protected git path `{dot_git}`: {error}"
        ))
    })?;
    if !metadata.is_file() {
        return Ok(None);
    }
    if metadata.len() > MAX_GITDIR_FILE_BYTES {
        return Err(SandboxProfileError::new(format!(
            "protected gitdir file `{dot_git}` exceeds {MAX_GITDIR_FILE_BYTES} bytes"
        )));
    }

    let content = read_opened_file_bounded(&file, MAX_GITDIR_FILE_BYTES, &dot_git)?;
    let text = std::str::from_utf8(&content).map_err(|error| {
        SandboxProfileError::new(format!(
            "protected gitdir file `{dot_git}` is not UTF-8: {error}"
        ))
    })?;
    let git_dir = text
        .lines()
        .next()
        .and_then(|line| line.trim().strip_prefix("gitdir:"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            SandboxProfileError::new(format!(
                "protected gitdir file `{dot_git}` has an invalid gitdir record"
            ))
        })?;
    let git_dir = Utf8Path::new(git_dir);
    let resolved = if git_dir.is_absolute() {
        git_dir.to_path_buf()
    } else {
        workspace_root.join(git_dir)
    };
    let resolved = normalize_absolute(&resolved, "protected gitdir")?;
    let snapshot = snapshot_opened_path(
        dot_git,
        &file,
        &metadata,
        "protected gitdir file",
        false,
        Some(&content),
    )?;
    Ok(Some((snapshot, resolved)))
}

fn protected_overlaps_writable_root(
    protected: &Utf8Path,
    writable_roots: &[Utf8PathBuf],
) -> Result<bool, SandboxProfileError> {
    for writable in writable_roots {
        let protected_inside_writable = PathGuard::security_path_is_within(protected, writable)
            .map_err(|error| {
                SandboxProfileError::new(format!(
                    "failed to compare protected path `{protected}` with writable root `{writable}`: {error}"
                ))
            })?;
        let writable_inside_protected = PathGuard::security_path_is_within(writable, protected)
            .map_err(|error| {
                SandboxProfileError::new(format!(
                    "failed to compare writable root `{writable}` with protected path `{protected}`: {error}"
                ))
            })?;
        if protected_inside_writable || writable_inside_protected {
            return Ok(true);
        }
    }
    Ok(false)
}

fn reject_writable_roots_inside_protected_paths(
    writable_roots: &[Utf8PathBuf],
    protected_paths: &[Utf8PathBuf],
) -> Result<(), SandboxProfileError> {
    for writable in writable_roots {
        for protected in protected_paths {
            let writable_inside_protected = PathGuard::security_path_is_within(writable, protected)
                .map_err(|error| {
                    SandboxProfileError::new(format!(
                        "failed to compare writable root `{writable}` with protected path `{protected}`: {error}"
                    ))
                })?;
            if writable_inside_protected {
                return Err(SandboxProfileError::new(format!(
                    "sandbox writable root `{writable}` is inside protected path `{protected}`"
                )));
            }
        }
    }
    Ok(())
}

fn normalize_sort_and_dedupe(paths: &mut Vec<Utf8PathBuf>) {
    paths.sort();
    paths.dedup();
}

fn sort_and_dedupe_snapshots(paths: &mut Vec<SandboxPathSnapshot>) {
    paths
        .sort_by(|left, right| PathGuard::compare_path_identity(&left.requested, &right.requested));
    #[cfg(windows)]
    paths.dedup_by(|left, right| left.identity == right.identity);
    #[cfg(not(windows))]
    paths.dedup_by(|left, right| left.canonical == right.canonical);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ResolvedConfig;
    use crate::workspace::WorkspaceDiscovery;

    fn utf8(path: std::path::PathBuf) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(path).expect("test path must be UTF-8")
    }

    fn workspace(root: &Utf8Path) -> Workspace {
        WorkspaceDiscovery::discover_fixed_root(root, &ResolvedConfig::default())
            .expect("workspace")
    }

    #[cfg(windows)]
    fn enable_case_sensitive_directory(path: &Utf8Path) {
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Foundation::HANDLE;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_CASE_SENSITIVE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FileCaseSensitiveInfo,
            SetFileInformationByHandle,
        };
        use windows_sys::Win32::System::SystemServices::FILE_CS_FLAG_CASE_SENSITIVE_DIR;

        let directory = std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .expect("open case-sensitive directory");
        let info = FILE_CASE_SENSITIVE_INFO {
            Flags: FILE_CS_FLAG_CASE_SENSITIVE_DIR,
        };
        assert_ne!(
            unsafe {
                SetFileInformationByHandle(
                    directory.as_raw_handle() as HANDLE,
                    FileCaseSensitiveInfo,
                    (&info as *const FILE_CASE_SENSITIVE_INFO).cast(),
                    std::mem::size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
                )
            },
            0,
            "enable case-sensitive directory: {}",
            std::io::Error::last_os_error()
        );
    }

    #[test]
    fn access_modes_map_to_codex_style_process_plans() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = utf8(temp.path().join("workspace"));
        let scratch = utf8(temp.path().join("scratch"));
        std::fs::create_dir_all(&root).expect("workspace root");
        std::fs::create_dir_all(&scratch).expect("scratch root");
        let workspace = workspace(&root);

        for mode in [AccessMode::Default, AccessMode::AutoReview] {
            let plan = ProcessSandboxPlan::for_access_mode(mode, &workspace).expect("plan");
            assert!(matches!(plan, ProcessSandboxPlan::WorkspaceWrite(_)));
            assert!(plan.audit_description().starts_with("workspace_write("));
        }
        let unrestricted =
            ProcessSandboxPlan::for_access_mode(AccessMode::FullAccess, &workspace).expect("plan");
        assert_eq!(unrestricted, ProcessSandboxPlan::Unrestricted);
        assert_eq!(unrestricted.audit_description(), "unrestricted");

        let profile = WorkspaceWriteSandboxProfile::compile(&workspace, [scratch])
            .expect("workspace-write profile");
        assert!(!profile.network.is_os_enforced());
        assert!(
            profile
                .audit_description()
                .contains("network=advisory_offline_environment")
        );
        assert!(
            profile
                .audit_description()
                .contains("world_writable_audit=bounded_best_effort")
        );
    }

    #[test]
    fn writable_roots_are_normalized_sorted_and_deduplicated() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = utf8(temp.path().join("workspace"));
        let scratch = utf8(temp.path().join("scratch"));
        std::fs::create_dir_all(&root).expect("workspace root");
        std::fs::create_dir_all(&scratch).expect("scratch root");
        let mut workspace = workspace(&root);
        workspace.path_policy.additional_write_roots =
            vec![scratch.join("child/.."), scratch.clone()];

        let profile =
            WorkspaceWriteSandboxProfile::compile(&workspace, [root.join("."), scratch.clone()])
                .expect("profile");

        assert_eq!(profile.writable_roots.len(), 2);
        assert!(
            profile
                .writable_roots
                .iter()
                .any(|snapshot| snapshot.requested == root)
        );
        assert!(
            profile
                .writable_roots
                .iter()
                .any(|snapshot| snapshot.requested == scratch)
        );
    }

    #[test]
    fn protected_carveouts_include_authority_paths_gitdir_and_overlapping_config() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = utf8(temp.path().join("workspace"));
        let git_dir = utf8(temp.path().join("git-meta"));
        let outside = utf8(temp.path().join("outside-protected"));
        std::fs::create_dir_all(&root).expect("workspace root");
        std::fs::create_dir_all(&git_dir).expect("git dir");
        std::fs::create_dir_all(&outside).expect("outside protected");
        std::fs::create_dir_all(root.join(".moyai/rules")).expect("moyai authority");
        std::fs::create_dir_all(root.join(".agents")).expect("agents authority");
        std::fs::create_dir_all(root.join(".claude/skills/example")).expect("claude authority");
        std::fs::create_dir_all(root.join(".codex")).expect("codex authority");
        std::fs::write(root.join("AGENTS.md"), "workspace authority\n")
            .expect("agents instruction");
        std::fs::write(root.join("CLAUDE.md"), "workspace authority\n")
            .expect("claude instruction");
        std::fs::write(root.join(".git"), "gitdir: ../git-meta\n").expect("gitdir file");
        let configured = root.join("configured-protected");
        std::fs::create_dir_all(&configured).expect("configured protected");
        let mut workspace = workspace(&root);
        workspace.protected_paths = vec![configured.clone(), outside.clone()];

        let profile =
            WorkspaceWriteSandboxProfile::compile(&workspace, std::iter::empty()).expect("profile");

        for expected in [
            root.join(".git"),
            root.join(".moyai"),
            root.join(".agents"),
            root.join(".claude"),
            root.join(".codex"),
            root.join("AGENTS.md"),
            root.join("CLAUDE.md"),
            git_dir,
            configured,
        ] {
            assert!(
                profile
                    .read_only_roots
                    .iter()
                    .any(|snapshot| snapshot.requested == expected),
                "missing protected carveout {expected}"
            );
        }
        assert!(
            profile
                .read_only_roots
                .iter()
                .all(|snapshot| snapshot.requested != outside)
        );
        for protected_file in [
            root.join(".git"),
            root.join("AGENTS.md"),
            root.join("CLAUDE.md"),
        ] {
            assert!(
                profile
                    .read_only_roots
                    .iter()
                    .find(|snapshot| snapshot.requested == protected_file)
                    .is_some_and(|snapshot| snapshot.content_sha256.is_some()),
                "protected file content was not pinned: {protected_file}"
            );
        }
    }

    #[test]
    fn configured_instruction_files_are_pinned_as_exact_protected_carveouts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = utf8(temp.path().join("workspace"));
        let external = utf8(temp.path().join("external-policy.md"));
        std::fs::create_dir_all(&root).expect("workspace root");
        std::fs::write(root.join("policy.md"), "workspace policy\n")
            .expect("workspace configured instruction");
        std::fs::write(&external, "external policy\n").expect("external configured instruction");
        let workspace = workspace(&root);
        let configured = [Utf8PathBuf::from("policy.md"), external.clone()];

        let profile = WorkspaceWriteSandboxProfile::compile_with_instructions(
            &workspace,
            std::iter::empty(),
            &configured,
        )
        .expect("profile");

        for expected in [root.join("policy.md"), external] {
            assert!(
                profile.read_only_roots.iter().any(|snapshot| {
                    snapshot.requested == expected && snapshot.content_sha256.is_some()
                }),
                "configured instruction was not content-pinned: {expected}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn case_sensitive_authority_variants_remain_distinct_protected_objects() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = utf8(temp.path().join("workspace"));
        std::fs::create_dir_all(&root).expect("workspace root");
        enable_case_sensitive_directory(&root);
        std::fs::create_dir(root.join(".moyai")).expect("lowercase authority");
        std::fs::create_dir(root.join(".MOYAI")).expect("uppercase authority");
        std::fs::write(root.join("AGENTS.md"), "upper\n").expect("upper instruction");
        std::fs::write(root.join("agents.md"), "lower\n").expect("lower instruction");
        let workspace = workspace(&root);

        let profile =
            WorkspaceWriteSandboxProfile::compile(&workspace, std::iter::empty()).expect("profile");

        for expected in [
            root.join(".moyai"),
            root.join(".MOYAI"),
            root.join("AGENTS.md"),
            root.join("agents.md"),
        ] {
            assert!(
                profile
                    .read_only_roots
                    .iter()
                    .any(|snapshot| snapshot.requested == expected),
                "case-distinct authority was dropped: {expected}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn remote_and_device_final_paths_are_not_admitted_as_local_sandbox_roots() {
        assert!(validate_windows_canonical_local_path(Utf8Path::new("C:/workspace")).is_ok());
        for path in [
            r"\\server\share\workspace",
            r"\\?\UNC\server\share\workspace",
            r"\\.\pipe\moyai",
        ] {
            assert!(
                validate_windows_canonical_local_path(Utf8Path::new(path)).is_err(),
                "remote/device path was admitted: {path}"
            );
        }
    }

    #[test]
    fn invalid_writable_roots_fail_profile_compilation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = utf8(temp.path().join("workspace"));
        std::fs::create_dir_all(&root).expect("workspace root");
        let mut workspace = workspace(&root);
        workspace.path_policy.additional_write_roots = vec![utf8(temp.path().join("missing"))];

        let error = WorkspaceWriteSandboxProfile::compile(&workspace, std::iter::empty())
            .expect_err("missing root must fail closed");
        assert!(error.to_string().contains("is unavailable"));
    }

    #[test]
    fn missing_or_ancestor_protected_paths_fail_process_profile_compilation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = utf8(temp.path().join("workspace"));
        std::fs::create_dir_all(&root).expect("workspace root");
        let mut workspace = workspace(&root);
        workspace.protected_paths = vec![root.join("missing-protected")];
        let error = WorkspaceWriteSandboxProfile::compile(&workspace, std::iter::empty())
            .expect_err("missing configured protection must fail closed");
        assert!(error.to_string().contains("is unavailable"));

        let protected = root.join("protected");
        let nested_write = protected.join("scratch");
        std::fs::create_dir_all(&nested_write).expect("nested writable root");
        workspace.protected_paths = vec![protected.clone()];
        workspace.path_policy.additional_write_roots = vec![nested_write.clone()];
        let error = WorkspaceWriteSandboxProfile::compile(&workspace, std::iter::empty())
            .expect_err("writable root inside protected path must fail closed");
        assert!(error.to_string().contains("inside protected path"));
        assert!(error.to_string().contains(nested_write.as_str()));
    }
}
