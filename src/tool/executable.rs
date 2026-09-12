use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::fs::OpenOptions;
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};

use crate::workspace::PathGuard;

#[derive(Debug, Clone)]
pub(crate) struct ResolvedExecutable {
    inner: Arc<ResolvedExecutableInner>,
}

#[derive(Debug)]
struct ResolvedExecutableInner {
    requested: Utf8PathBuf,
    canonical: Utf8PathBuf,
    #[cfg(windows)]
    identity: WindowsExecutableIdentity,
    #[cfg(unix)]
    identity: UnixExecutableIdentity,
    #[cfg(not(any(windows, unix)))]
    length: u64,
    // On Windows this handle deliberately denies write/delete sharing. It keeps
    // the admitted executable entry immutable until every clone used by the
    // effect has either spawned or been dropped.
    _pin: Option<std::fs::File>,
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
enum WindowsExecutableIdentity {
    Extended {
        volume_serial_number: u64,
        file_id: [u8; 16],
    },
    Legacy {
        volume_serial_number: u32,
        file_index: u64,
    },
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnixExecutableIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

#[derive(Debug)]
struct CapturedSearchDirectory {
    canonical: Utf8PathBuf,
    #[cfg(unix)]
    handle: std::fs::File,
    #[cfg(unix)]
    identity: UnixDirectoryIdentity,
}

#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UnixDirectoryIdentity {
    device: u64,
    inode: u64,
}

impl ResolvedExecutable {
    pub(crate) fn resolve(
        program: &str,
        cwd: &Utf8Path,
        environment: &HashMap<String, String>,
    ) -> Result<Self, ExecutableIdentityError> {
        Self::resolve_with_search_policy(program, cwd, environment, true, false, None, false)
    }

    pub(crate) fn resolve_from_captured_search_path(
        program: &str,
        environment: &HashMap<String, String>,
        excluded_root: &Utf8Path,
    ) -> Result<Self, ExecutableIdentityError> {
        Self::resolve_captured_search_path(program, environment, excluded_root, false)
    }

    pub(crate) fn discover_shell_from_captured_search_path(
        program: &str,
        environment: &HashMap<String, String>,
        excluded_root: &Utf8Path,
    ) -> Result<Self, ExecutableIdentityError> {
        Self::resolve_captured_search_path(program, environment, excluded_root, true)
    }

    fn resolve_captured_search_path(
        program: &str,
        environment: &HashMap<String, String>,
        excluded_root: &Utf8Path,
        skip_reparse_candidates: bool,
    ) -> Result<Self, ExecutableIdentityError> {
        let path = Utf8Path::new(program);
        if path.is_absolute() || program.contains(['/', '\\']) {
            return Err(ExecutableIdentityError::InvalidProgram(program.to_string()));
        }
        Self::resolve_with_search_policy(
            program,
            excluded_root,
            environment,
            false,
            true,
            Some(excluded_root),
            skip_reparse_candidates,
        )
    }

    fn resolve_with_search_policy(
        program: &str,
        cwd: &Utf8Path,
        environment: &HashMap<String, String>,
        include_working_directory: bool,
        absolute_search_directories_only: bool,
        excluded_root: Option<&Utf8Path>,
        skip_reparse_candidates: bool,
    ) -> Result<Self, ExecutableIdentityError> {
        if program.trim().is_empty() || program.contains('\0') {
            return Err(ExecutableIdentityError::InvalidProgram(program.to_string()));
        }
        let candidates = match executable_path_candidates(
            program,
            cwd,
            environment,
            include_working_directory,
            absolute_search_directories_only,
        ) {
            Ok(candidates) => candidates,
            Err(ExecutableIdentityError::InvalidPath { .. })
                if !include_working_directory && absolute_search_directories_only =>
            {
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        let mut last_error = None;
        let mut excluded_candidate_seen = false;
        let canonical_excluded_root = excluded_root
            .map(|root| canonical_excluded_root(root, program))
            .transpose()?;
        for candidate in candidates {
            if let (Some(excluded_root), Some(canonical_excluded_root)) =
                (excluded_root, canonical_excluded_root.as_deref())
            {
                let requested_is_excluded = requested_candidate_is_within_excluded_root(
                    &candidate,
                    excluded_root,
                    program,
                )? || requested_candidate_is_within_excluded_root(
                    &candidate,
                    canonical_excluded_root,
                    program,
                )?;
                if requested_is_excluded {
                    excluded_candidate_seen = true;
                    continue;
                }
            }
            let captured_search_directory =
                if let Some(canonical_excluded_root) = canonical_excluded_root.as_deref() {
                    let captured = match CapturedSearchDirectory::capture(&candidate, program) {
                        Ok(captured) => captured,
                        Err(ExecutableIdentityError::Unavailable { source, .. }) => {
                            last_error = Some(source);
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    if captured.is_within(canonical_excluded_root, program)? {
                        excluded_candidate_seen = true;
                        continue;
                    }
                    Some(captured)
                } else {
                    None
                };
            let pinned = if let Some(captured) = captured_search_directory.as_ref() {
                Self::pin_from_captured_search_directory(candidate.clone(), captured)
            } else {
                Self::pin(candidate.clone())
            };
            match pinned {
                Ok(resolved) => {
                    if let Some(canonical_excluded_root) = canonical_excluded_root.as_deref() {
                        let Some(captured_before_pin) = captured_search_directory.as_ref() else {
                            return Err(ExecutableIdentityError::InvalidPath {
                                program: program.to_string(),
                                reason: "captured executable search directory was not retained"
                                    .to_string(),
                            });
                        };
                        let captured_after_pin =
                            match CapturedSearchDirectory::capture(&candidate, program) {
                                Ok(captured) => captured,
                                Err(ExecutableIdentityError::Unavailable { source, .. }) => {
                                    last_error = Some(source);
                                    continue;
                                }
                                Err(error) => return Err(error),
                            };
                        if captured_after_pin.is_within(canonical_excluded_root, program)? {
                            excluded_candidate_seen = true;
                            continue;
                        }
                        if !captured_before_pin.same_identity(&captured_after_pin) {
                            last_error = Some(std::io::Error::other(
                                "captured executable search directory changed during identity pinning",
                            ));
                            continue;
                        }
                        let excluded = PathGuard::security_path_is_within(
                            resolved.path(),
                            canonical_excluded_root,
                        )
                        .map_err(|error| ExecutableIdentityError::InvalidPath {
                            program: program.to_string(),
                            reason: format!(
                                "failed to validate the captured executable search boundary: {error}"
                            ),
                        })?;
                        if excluded {
                            excluded_candidate_seen = true;
                            continue;
                        }
                    }
                    return Ok(resolved);
                }
                Err(ExecutableIdentityError::Unavailable { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    last_error = Some(source);
                }
                Err(ExecutableIdentityError::ReparsePoint { .. }) if skip_reparse_candidates => {
                    // Windows app execution aliases cannot be pinned as executables.
                    // Automatic shell discovery may try the next captured PATH entry;
                    // explicit programs and other admission failures remain strict.
                    last_error = Some(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "no regular executable was found after excluding reparse candidates",
                    ));
                }
                Err(error) => return Err(error),
            }
        }
        Err(ExecutableIdentityError::Unavailable {
            program: program.to_string(),
            source: last_error.unwrap_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    if excluded_candidate_seen {
                        "no captured executable candidate outside the workspace/project boundary was found"
                    } else {
                        "executable was not found"
                    },
                )
            }),
        })
    }

    fn pin(requested: Utf8PathBuf) -> Result<Self, ExecutableIdentityError> {
        let requested = crate::workspace::project::normalize_path(Utf8Path::new("."), &requested)
            .map_err(|error| ExecutableIdentityError::InvalidPath {
            program: requested.to_string(),
            reason: error.to_string(),
        })?;
        let pin =
            open_executable(&requested).map_err(|source| ExecutableIdentityError::Unavailable {
                program: requested.to_string(),
                source,
            })?;
        Self::from_opened_pin(requested, pin)
    }

    fn pin_from_captured_search_directory(
        requested: Utf8PathBuf,
        directory: &CapturedSearchDirectory,
    ) -> Result<Self, ExecutableIdentityError> {
        let requested = crate::workspace::project::normalize_path(Utf8Path::new("."), &requested)
            .map_err(|error| ExecutableIdentityError::InvalidPath {
            program: requested.to_string(),
            reason: error.to_string(),
        })?;
        let file_name =
            requested
                .file_name()
                .ok_or_else(|| ExecutableIdentityError::InvalidPath {
                    program: requested.to_string(),
                    reason: "captured executable candidate has no file name".to_string(),
                })?;
        #[cfg(unix)]
        let pin = open_executable_at(&directory.handle, file_name).map_err(|source| {
            ExecutableIdentityError::Unavailable {
                program: requested.to_string(),
                source,
            }
        })?;
        #[cfg(not(unix))]
        let pin = open_executable(&directory.canonical.join(file_name)).map_err(|source| {
            ExecutableIdentityError::Unavailable {
                program: requested.to_string(),
                source,
            }
        })?;
        Self::from_opened_pin(requested, pin)
    }

    fn from_opened_pin(
        requested: Utf8PathBuf,
        pin: std::fs::File,
    ) -> Result<Self, ExecutableIdentityError> {
        let metadata = pin
            .metadata()
            .map_err(|source| ExecutableIdentityError::Unavailable {
                program: requested.to_string(),
                source,
            })?;
        // OPEN_REPARSE_POINT exposes the link itself; its metadata can report
        // !is_file(). Classify that exact attribute before the regular-file gate.
        #[cfg(windows)]
        reject_windows_reparse_point(&requested, &metadata)?;
        if !metadata.is_file() {
            return Err(ExecutableIdentityError::InvalidPath {
                program: requested.to_string(),
                reason: "resolved executable is not a regular file".to_string(),
            });
        }
        let canonical = PathGuard::opened_file_identity_path(&pin).map_err(|error| {
            ExecutableIdentityError::InvalidPath {
                program: requested.to_string(),
                reason: error.to_string(),
            }
        })?;
        Ok(Self {
            inner: Arc::new(ResolvedExecutableInner {
                requested,
                canonical,
                #[cfg(windows)]
                identity: windows_executable_identity(&pin)?,
                #[cfg(unix)]
                identity: unix_executable_identity(&metadata),
                #[cfg(not(any(windows, unix)))]
                length: metadata.len(),
                _pin: Some(pin),
            }),
        })
    }

    pub(crate) fn path(&self) -> &Utf8Path {
        &self.inner.canonical
    }

    #[cfg(test)]
    fn requested_path(&self) -> &Utf8Path {
        &self.inner.requested
    }

    pub(crate) fn revalidate(&self) -> Result<(), ExecutableIdentityError> {
        let current = open_executable(&self.inner.requested).map_err(|source| {
            ExecutableIdentityError::Changed {
                program: self.inner.requested.to_string(),
                reason: source.to_string(),
            }
        })?;
        let metadata = current
            .metadata()
            .map_err(|source| ExecutableIdentityError::Changed {
                program: self.inner.requested.to_string(),
                reason: source.to_string(),
            })?;
        #[cfg(windows)]
        reject_windows_reparse_point(&self.inner.requested, &metadata).map_err(|error| {
            ExecutableIdentityError::Changed {
                program: self.inner.requested.to_string(),
                reason: error.to_string(),
            }
        })?;
        if !metadata.is_file() {
            return Err(ExecutableIdentityError::Changed {
                program: self.inner.requested.to_string(),
                reason: "the admitted executable is no longer a regular file".to_string(),
            });
        }
        let canonical = PathGuard::opened_file_identity_path(&current).map_err(|error| {
            ExecutableIdentityError::Changed {
                program: self.inner.requested.to_string(),
                reason: error.to_string(),
            }
        })?;
        if PathGuard::stable_identity_key(&canonical)
            != PathGuard::stable_identity_key(&self.inner.canonical)
        {
            return Err(ExecutableIdentityError::Changed {
                program: self.inner.requested.to_string(),
                reason: format!(
                    "final path changed from `{}` to `{canonical}`",
                    self.inner.canonical
                ),
            });
        }
        #[cfg(windows)]
        if windows_executable_identity(&current)? != self.inner.identity {
            return Err(ExecutableIdentityError::Changed {
                program: self.inner.requested.to_string(),
                reason: "volume/file identity changed".to_string(),
            });
        }
        #[cfg(unix)]
        if unix_executable_identity(&metadata) != self.inner.identity {
            return Err(ExecutableIdentityError::Changed {
                program: self.inner.requested.to_string(),
                reason: "device/inode/content metadata changed".to_string(),
            });
        }
        #[cfg(not(any(windows, unix)))]
        if metadata.len() != self.inner.length {
            return Err(ExecutableIdentityError::Changed {
                program: self.inner.requested.to_string(),
                reason: "file length changed".to_string(),
            });
        }
        Ok(())
    }

    #[cfg(test)]
    fn release_pin_for_replacement_test(&mut self) {
        Arc::get_mut(&mut self.inner)
            .expect("test executable identity must not have clones")
            ._pin
            .take();
    }
}

impl CapturedSearchDirectory {
    fn capture(candidate: &Utf8Path, program: &str) -> Result<Self, ExecutableIdentityError> {
        let candidate = crate::workspace::project::normalize_path(Utf8Path::new("."), candidate)
            .map_err(|error| ExecutableIdentityError::InvalidPath {
                program: program.to_string(),
                reason: format!("failed to normalize the captured executable candidate: {error}"),
            })?;
        let parent = candidate
            .parent()
            .ok_or_else(|| ExecutableIdentityError::InvalidPath {
                program: program.to_string(),
                reason: format!("captured executable candidate `{candidate}` has no parent"),
            })?;

        #[cfg(unix)]
        {
            let handle = open_search_directory(parent).map_err(|source| {
                ExecutableIdentityError::Unavailable {
                    program: candidate.to_string(),
                    source,
                }
            })?;
            let metadata =
                handle
                    .metadata()
                    .map_err(|source| ExecutableIdentityError::Unavailable {
                        program: candidate.to_string(),
                        source,
                    })?;
            if !metadata.is_dir() {
                return Err(ExecutableIdentityError::InvalidPath {
                    program: program.to_string(),
                    reason: format!(
                        "captured executable search path `{parent}` is not a directory"
                    ),
                });
            }
            let opened_path = PathGuard::opened_file_identity_path(&handle).map_err(|error| {
                ExecutableIdentityError::InvalidPath {
                    program: program.to_string(),
                    reason: format!(
                        "failed to capture executable search directory `{parent}`: {error}"
                    ),
                }
            })?;
            let canonical = std::fs::canonicalize(&opened_path).map_err(|error| {
                ExecutableIdentityError::InvalidPath {
                    program: program.to_string(),
                    reason: format!(
                        "failed to canonicalize captured executable search directory `{opened_path}`: {error}"
                    ),
                }
            })?;
            let canonical = Utf8PathBuf::from_path_buf(canonical).map_err(|path| {
                ExecutableIdentityError::InvalidPath {
                    program: program.to_string(),
                    reason: format!(
                        "captured executable search directory `{}` is not valid UTF-8",
                        path.display()
                    ),
                }
            })?;
            let canonical_metadata = std::fs::metadata(&canonical).map_err(|error| {
                ExecutableIdentityError::InvalidPath {
                    program: program.to_string(),
                    reason: format!(
                        "failed to revalidate captured executable search directory `{canonical}`: {error}"
                    ),
                }
            })?;
            let identity = unix_directory_identity(&metadata);
            if unix_directory_identity(&canonical_metadata) != identity {
                return Err(ExecutableIdentityError::InvalidPath {
                    program: program.to_string(),
                    reason: format!(
                        "captured executable search directory `{parent}` changed during canonicalization"
                    ),
                });
            }
            Ok(Self {
                canonical,
                identity,
                handle,
            })
        }

        #[cfg(not(unix))]
        {
            let canonical = std::fs::canonicalize(parent).map_err(|source| {
                ExecutableIdentityError::Unavailable {
                    program: candidate.to_string(),
                    source,
                }
            })?;
            let canonical = Utf8PathBuf::from_path_buf(canonical).map_err(|path| {
                ExecutableIdentityError::InvalidPath {
                    program: program.to_string(),
                    reason: format!(
                        "captured executable search directory `{}` is not valid UTF-8",
                        path.display()
                    ),
                }
            })?;
            Ok(Self { canonical })
        }
    }

    fn is_within(
        &self,
        canonical_excluded_root: &Utf8Path,
        program: &str,
    ) -> Result<bool, ExecutableIdentityError> {
        PathGuard::security_path_is_within(&self.canonical, canonical_excluded_root).map_err(
            |error| ExecutableIdentityError::InvalidPath {
                program: program.to_string(),
                reason: format!(
                    "failed to validate the captured executable search directory boundary: {error}"
                ),
            },
        )
    }

    fn same_identity(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.identity == other.identity
        }
        #[cfg(not(unix))]
        {
            PathGuard::same_path_identity(&self.canonical, &other.canonical)
        }
    }
}

fn canonical_excluded_root(
    excluded_root: &Utf8Path,
    program: &str,
) -> Result<Utf8PathBuf, ExecutableIdentityError> {
    let excluded_root =
        crate::workspace::project::normalize_path(Utf8Path::new("."), excluded_root).map_err(
            |error| ExecutableIdentityError::InvalidPath {
                program: program.to_string(),
                reason: format!("failed to normalize the excluded executable root: {error}"),
            },
        )?;
    let canonical = std::fs::canonicalize(&excluded_root).map_err(|error| {
        ExecutableIdentityError::InvalidPath {
            program: program.to_string(),
            reason: format!(
                "failed to canonicalize the excluded executable root `{excluded_root}`: {error}"
            ),
        }
    })?;
    Utf8PathBuf::from_path_buf(canonical).map_err(|path| ExecutableIdentityError::InvalidPath {
        program: program.to_string(),
        reason: format!(
            "canonical excluded executable root `{}` is not valid UTF-8",
            path.display()
        ),
    })
}

fn requested_candidate_is_within_excluded_root(
    candidate: &Utf8Path,
    excluded_root: &Utf8Path,
    program: &str,
) -> Result<bool, ExecutableIdentityError> {
    let candidate = crate::workspace::project::normalize_path(Utf8Path::new("."), candidate)
        .map_err(|error| ExecutableIdentityError::InvalidPath {
            program: program.to_string(),
            reason: format!("failed to normalize the captured executable candidate: {error}"),
        })?;
    let excluded_root =
        crate::workspace::project::normalize_path(Utf8Path::new("."), excluded_root).map_err(
            |error| ExecutableIdentityError::InvalidPath {
                program: program.to_string(),
                reason: format!("failed to normalize the excluded executable root: {error}"),
            },
        )?;
    let candidate_key = PathGuard::stable_identity_key(&candidate);
    let root_key = PathGuard::stable_identity_key(&excluded_root);
    Ok(Utf8Path::new(&candidate_key).starts_with(Utf8Path::new(&root_key)))
}

impl PartialEq for ResolvedExecutable {
    fn eq(&self, other: &Self) -> bool {
        PathGuard::stable_identity_key(&self.inner.canonical)
            == PathGuard::stable_identity_key(&other.inner.canonical)
            && {
                #[cfg(windows)]
                {
                    self.inner.identity == other.inner.identity
                }
                #[cfg(unix)]
                {
                    self.inner.identity == other.inner.identity
                }
                #[cfg(not(any(windows, unix)))]
                {
                    self.inner.length == other.inner.length
                }
            }
    }
}

impl Eq for ResolvedExecutable {}

fn executable_path_candidates(
    program: &str,
    cwd: &Utf8Path,
    environment: &HashMap<String, String>,
    include_working_directory: bool,
    absolute_search_directories_only: bool,
) -> Result<Vec<Utf8PathBuf>, ExecutableIdentityError> {
    let path = Utf8Path::new(program);
    if path.is_absolute() {
        return Ok(vec![path.to_path_buf()]);
    }
    if program.contains(['/', '\\']) {
        return Ok(vec![cwd.join(path)]);
    }
    let extensions = executable_extensions(program, environment);
    let mut candidates = Vec::new();
    if cfg!(windows) && include_working_directory {
        push_executable_candidates(&mut candidates, cwd, program, &extensions);
    }
    if let Some(search_path) = environment_value(environment, "PATH") {
        for directory in std::env::split_paths(std::ffi::OsStr::new(search_path)) {
            let Some(directory) = Utf8PathBuf::from_path_buf(directory).ok() else {
                continue;
            };
            if absolute_search_directories_only && !directory.is_absolute() {
                continue;
            }
            push_executable_candidates(&mut candidates, &directory, program, &extensions);
        }
    }
    #[cfg(windows)]
    if program.eq_ignore_ascii_case("powershell") || program.eq_ignore_ascii_case("powershell.exe")
    {
        if let Some(system_root) = environment_value(environment, "SystemRoot") {
            let directory = Utf8Path::new(system_root)
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0");
            if !absolute_search_directories_only || directory.is_absolute() {
                push_executable_candidates(&mut candidates, &directory, program, &extensions);
            }
        }
    }
    if candidates.is_empty() {
        return Err(ExecutableIdentityError::InvalidPath {
            program: program.to_string(),
            reason: "captured process environment has no executable search authority".to_string(),
        });
    }
    let mut deduplicated: Vec<Utf8PathBuf> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if !deduplicated.iter().any(|existing| {
            PathGuard::stable_identity_key(existing) == PathGuard::stable_identity_key(&candidate)
        }) {
            deduplicated.push(candidate);
        }
    }
    Ok(deduplicated)
}

fn push_executable_candidates(
    candidates: &mut Vec<Utf8PathBuf>,
    directory: &Utf8Path,
    program: &str,
    extensions: &[String],
) {
    for extension in extensions {
        candidates.push(directory.join(format!("{program}{extension}")));
    }
}

fn environment_value<'a>(environment: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    environment
        .iter()
        .find_map(|(name, value)| name.eq_ignore_ascii_case(key).then_some(value.as_str()))
}

#[cfg(windows)]
fn executable_extensions(program: &str, environment: &HashMap<String, String>) -> Vec<String> {
    if Utf8Path::new(program).extension().is_some() {
        return vec![String::new()];
    }
    environment_value(environment, "PATHEXT")
        .unwrap_or(".COM;.EXE;.BAT;.CMD")
        .split(';')
        .filter(|extension| !extension.is_empty())
        .map(|extension| extension.to_ascii_lowercase())
        .collect()
}

#[cfg(not(windows))]
fn executable_extensions(_program: &str, _environment: &HashMap<String, String>) -> Vec<String> {
    vec![String::new()]
}

fn open_executable(path: &Utf8Path) -> Result<std::fs::File, std::io::Error> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        };
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(unix)]
fn open_search_directory(path: &Utf8Path) -> Result<std::fs::File, std::io::Error> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY);
    options.open(path)
}

#[cfg(unix)]
fn open_executable_at(
    directory: &std::fs::File,
    file_name: &str,
) -> Result<std::fs::File, std::io::Error> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let file_name = CString::new(file_name).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "captured executable file name contains a NUL byte",
        )
    })?;
    // SAFETY: `file_name` is a live NUL-terminated single component and `directory` remains
    // open for the whole call, so lookup cannot be redirected by replacing its PATH alias.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            file_name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC,
        )
    };
    if descriptor == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: ownership of the newly returned descriptor transfers exactly once.
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
}

#[cfg(windows)]
fn reject_windows_reparse_point(
    path: &Utf8Path,
    metadata: &std::fs::Metadata,
) -> Result<(), ExecutableIdentityError> {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(ExecutableIdentityError::ReparsePoint {
            program: path.to_string(),
        });
    }
    Ok(())
}

#[cfg(windows)]
fn windows_executable_identity(
    file: &std::fs::File,
) -> Result<WindowsExecutableIdentity, ExecutableIdentityError> {
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
        return Ok(WindowsExecutableIdentity::Extended {
            volume_serial_number: extended.VolumeSerialNumber,
            file_id: extended.FileId.Identifier,
        });
    }
    let mut legacy: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle, &mut legacy) } == 0 {
        return Err(ExecutableIdentityError::InvalidPath {
            program: "opened executable".to_string(),
            reason: std::io::Error::last_os_error().to_string(),
        });
    }
    Ok(WindowsExecutableIdentity::Legacy {
        volume_serial_number: legacy.dwVolumeSerialNumber,
        file_index: ((legacy.nFileIndexHigh as u64) << 32) | legacy.nFileIndexLow as u64,
    })
}

#[cfg(unix)]
fn unix_executable_identity(metadata: &std::fs::Metadata) -> UnixExecutableIdentity {
    use std::os::unix::fs::MetadataExt as _;
    UnixExecutableIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        length: metadata.len(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
    }
}

#[cfg(unix)]
fn unix_directory_identity(metadata: &std::fs::Metadata) -> UnixDirectoryIdentity {
    use std::os::unix::fs::MetadataExt as _;

    UnixDirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[derive(Debug)]
pub(crate) enum ExecutableIdentityError {
    InvalidProgram(String),
    InvalidPath {
        program: String,
        reason: String,
    },
    ReparsePoint {
        program: String,
    },
    Unavailable {
        program: String,
        source: std::io::Error,
    },
    Changed {
        program: String,
        reason: String,
    },
}

impl ExecutableIdentityError {
    pub(crate) fn is_not_found(&self) -> bool {
        matches!(self, Self::Unavailable { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }
}

impl Display for ExecutableIdentityError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidProgram(program) => {
                write!(formatter, "executable program `{program}` is invalid")
            }
            Self::InvalidPath { program, reason } => {
                write!(
                    formatter,
                    "executable `{program}` could not be pinned: {reason}"
                )
            }
            Self::ReparsePoint { program } => {
                write!(
                    formatter,
                    "executable `{program}` could not be pinned: resolved executable is a reparse point"
                )
            }
            Self::Unavailable { program, source } => {
                write!(formatter, "executable `{program}` is unavailable: {source}")
            }
            Self::Changed { program, reason } => {
                write!(
                    formatter,
                    "admitted executable `{program}` changed before spawn: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for ExecutableIdentityError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_executable_is_resolved_to_an_absolute_pinned_identity() {
        let executable = std::env::current_exe().expect("current test executable");
        let executable = Utf8PathBuf::from_path_buf(executable).expect("utf8 test executable");
        let directory = executable.parent().expect("test executable parent");
        let name = executable.file_name().expect("test executable name");
        let environment = HashMap::from([(
            "PATH".to_string(),
            directory
                .as_std_path()
                .as_os_str()
                .to_string_lossy()
                .into_owned(),
        )]);

        let resolved = ResolvedExecutable::resolve(name, directory, &environment)
            .expect("resolve test executable");

        assert!(resolved.path().is_absolute());
        assert_eq!(
            PathGuard::stable_identity_key(resolved.requested_path()),
            PathGuard::stable_identity_key(&executable)
        );
        resolved.revalidate().expect("stable executable identity");
    }

    #[cfg(windows)]
    #[test]
    fn captured_search_path_skips_workspace_executable_but_explicit_resolution_keeps_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace =
            Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 workspace");
        let missing = Utf8PathBuf::from_path_buf(temp.path().join("missing"))
            .expect("utf8 missing directory");
        let trusted = Utf8PathBuf::from_path_buf(temp.path().join("trusted"))
            .expect("utf8 trusted directory");
        std::fs::create_dir_all(&workspace).expect("workspace directory");
        std::fs::create_dir_all(&trusted).expect("trusted directory");
        let workspace_candidate = workspace.join("pwsh.exe");
        let trusted_candidate = trusted.join("pwsh.exe");
        std::fs::write(&workspace_candidate, b"workspace executable")
            .expect("workspace executable fixture");
        std::fs::write(&trusted_candidate, b"trusted executable")
            .expect("trusted executable fixture");
        let search_path = std::env::join_paths([
            missing.as_std_path(),
            workspace.as_std_path(),
            trusted.as_std_path(),
        ])
        .expect("captured search path")
        .to_string_lossy()
        .into_owned();
        let environment = HashMap::from([
            ("PATH".to_string(), search_path),
            ("PATHEXT".to_string(), ".EXE".to_string()),
        ]);

        let implicit =
            ResolvedExecutable::resolve_from_captured_search_path("pwsh", &environment, &workspace)
                .expect("implicit executable outside workspace");
        assert_eq!(
            PathGuard::stable_identity_key(implicit.requested_path()),
            PathGuard::stable_identity_key(&trusted_candidate)
        );

        let explicit = ResolvedExecutable::resolve("pwsh", &workspace, &environment)
            .expect("explicit override keeps working-directory semantics");
        assert_eq!(
            PathGuard::stable_identity_key(explicit.requested_path()),
            PathGuard::stable_identity_key(&workspace_candidate)
        );
    }

    #[cfg(unix)]
    #[test]
    fn captured_search_path_rejects_a_workspace_symlink_to_an_outside_executable() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let workspace =
            Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 workspace");
        std::fs::create_dir_all(&workspace).expect("workspace directory");
        let outside = Utf8PathBuf::from_path_buf(std::env::current_exe().expect("current exe"))
            .expect("utf8 current exe");
        symlink(&outside, workspace.join("formatter")).expect("workspace executable symlink");
        let environment = HashMap::from([(
            "PATH".to_string(),
            workspace
                .as_std_path()
                .as_os_str()
                .to_string_lossy()
                .into_owned(),
        )]);

        let error = ResolvedExecutable::resolve_from_captured_search_path(
            "formatter",
            &environment,
            &workspace,
        )
        .expect_err("workspace-requested symlink must not escape the excluded boundary");
        assert!(
            error
                .to_string()
                .contains("outside the workspace/project boundary")
        );
    }

    #[cfg(unix)]
    #[test]
    fn captured_search_path_rejects_physical_workspace_candidate_for_aliased_excluded_root() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let physical_workspace = Utf8PathBuf::from_path_buf(temp.path().join("physical-workspace"))
            .expect("utf8 physical workspace");
        let physical_bin = physical_workspace.join("bin");
        let workspace_alias = Utf8PathBuf::from_path_buf(temp.path().join("workspace-alias"))
            .expect("utf8 workspace alias");
        std::fs::create_dir_all(&physical_bin).expect("physical workspace bin directory");
        symlink(&physical_workspace, &workspace_alias).expect("workspace root alias");
        let workspace_candidate = physical_bin.join("formatter");
        std::fs::copy(
            std::env::current_exe().expect("current exe"),
            &workspace_candidate,
        )
        .expect("physical workspace executable fixture");
        let environment = HashMap::from([(
            "PATH".to_string(),
            physical_bin
                .as_std_path()
                .as_os_str()
                .to_string_lossy()
                .into_owned(),
        )]);

        let error = ResolvedExecutable::resolve_from_captured_search_path(
            "formatter",
            &environment,
            &workspace_alias,
        )
        .expect_err("physical workspace candidate must remain excluded through a root alias");
        assert!(
            error
                .to_string()
                .contains("outside the workspace/project boundary")
        );
    }

    #[cfg(unix)]
    #[test]
    fn captured_search_path_rejects_second_workspace_alias_with_outward_executable_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let physical_workspace = Utf8PathBuf::from_path_buf(temp.path().join("physical-workspace"))
            .expect("utf8 physical workspace");
        let physical_bin = physical_workspace.join("bin");
        let excluded_root_alias =
            Utf8PathBuf::from_path_buf(temp.path().join("excluded-root-alias"))
                .expect("utf8 excluded root alias");
        let search_path_alias = Utf8PathBuf::from_path_buf(temp.path().join("search-path-alias"))
            .expect("utf8 search path alias");
        std::fs::create_dir_all(&physical_bin).expect("physical workspace bin directory");
        symlink(&physical_workspace, &excluded_root_alias).expect("excluded workspace root alias");
        symlink(&physical_bin, &search_path_alias).expect("second workspace search alias");
        symlink("/bin/sh", physical_bin.join("formatter")).expect("outward executable symlink");
        let environment = HashMap::from([(
            "PATH".to_string(),
            search_path_alias
                .as_std_path()
                .as_os_str()
                .to_string_lossy()
                .into_owned(),
        )]);

        let error = ResolvedExecutable::resolve_from_captured_search_path(
            "formatter",
            &environment,
            &excluded_root_alias,
        )
        .expect_err("a second workspace alias must not launder an outward executable symlink");
        assert!(
            error
                .to_string()
                .contains("outside the workspace/project boundary")
        );

        let explicit = ResolvedExecutable::resolve(
            search_path_alias.join("formatter").as_str(),
            &search_path_alias,
            &environment,
        )
        .expect("explicit path-qualified executable keeps existing semantics");
        explicit
            .revalidate()
            .expect("explicit outward executable symlink remains stable");
    }

    #[cfg(windows)]
    #[test]
    fn windows_pin_prevents_in_place_executable_rewrite_until_effect_is_dropped() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("fixture.exe"))
            .expect("utf8 executable fixture");
        std::fs::write(&path, b"first executable bytes").expect("write fixture");
        let resolved = ResolvedExecutable::resolve(
            path.as_str(),
            path.parent().expect("fixture parent"),
            &HashMap::new(),
        )
        .expect("pin fixture");

        assert!(std::fs::write(&path, b"replacement executable bytes").is_err());
        resolved.revalidate().expect("pinned identity stays stable");
        drop(resolved);
        std::fs::write(&path, b"replacement executable bytes")
            .expect("pin release permits rewrite");
    }

    #[cfg(windows)]
    #[test]
    fn executable_replacement_is_rejected_by_spawn_time_identity_revalidation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("fixture.exe"))
            .expect("utf8 executable fixture");
        std::fs::write(&path, b"first executable bytes").expect("write fixture");
        let mut resolved = ResolvedExecutable::resolve(
            path.as_str(),
            path.parent().expect("fixture parent"),
            &HashMap::new(),
        )
        .expect("pin fixture");
        resolved.release_pin_for_replacement_test();
        std::fs::remove_file(&path).expect("remove admitted entry");
        std::fs::write(&path, b"replacement executable bytes").expect("replace executable entry");

        let error = resolved
            .revalidate()
            .expect_err("replacement identity must fail before spawn");
        assert!(error.to_string().contains("changed before spawn"));
    }
}
