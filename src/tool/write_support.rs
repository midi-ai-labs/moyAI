use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::UNIX_EPOCH;

use camino::{Utf8Path, Utf8PathBuf};

use crate::edit::{ChangeSummary, FileChange, FileContentIdentity, ensure_edit_read_limit};
use crate::error::EditError;
use crate::workspace::{GuardedPath, PathGuard};

const MAX_UNIQUE_WRITE_NAMES: usize = 16;
pub(crate) const MAX_EDIT_RECOVERY_PATHS: usize = 512;
pub(crate) const MAX_EDIT_RECOVERY_REASON_BYTES: usize = 4 * 1024;
const PRESERVED_CONFLICT_RESTORE_LABEL: &str = "; restore failed: ";
const STAGED_CLEANUP_FAILURE_LABEL: &str = "; staged-file cleanup also failed: ";
const STAGED_IDENTITY_FAILURE_LABEL: &str = "; stable identity verification failed: ";

#[cfg(unix)]
const UNIX_FILE_TYPE_MASK: u32 = libc::S_IFMT as u32;
#[cfg(unix)]
const UNIX_REGULAR_FILE_TYPE: u32 = libc::S_IFREG as u32;

#[cfg(target_vendor = "apple")]
const _: [u32; 2] = [libc::S_IFMT as u32, libc::S_IFREG as u32];

pub(crate) fn write_text_file_conditionally(
    guarded: &GuardedPath,
    text: &str,
    expected_identity: Option<&FileContentIdentity>,
    validate_temporary_file: impl FnOnce(&File) -> Result<(), EditError>,
) -> Result<FileContentIdentity, EditError> {
    let path = guarded.absolute.as_path();
    let parent = path
        .parent()
        .ok_or_else(|| EditError::Message("file path has no parent".to_string()))?;
    let target_name = path
        .file_name()
        .ok_or_else(|| EditError::Message(format!("file path `{path}` has no name")))?;
    let stable_parent = StableWriteParent::open(parent, guarded)?;
    let mut staged = stable_parent.create_entry("stage", path)?;
    if let Err(error) = validate_temporary_file(&staged.file) {
        return Err(cleanup_staged_entry_after_failure(
            &stable_parent,
            &staged,
            path,
            error,
        ));
    }
    let prepared = (|| {
        staged.file.write_all(text.as_bytes())?;
        staged.file.flush()?;
        written_text_identity(&staged.file, text)
    })();
    let committed_identity = match prepared {
        Ok(identity) => identity,
        Err(error) => {
            return Err(cleanup_staged_entry_after_failure(
                &stable_parent,
                &staged,
                path,
                error,
            ));
        }
    };

    let Some(expected_identity) = expected_identity else {
        if let Err(error) = stable_parent.move_entry_noclobber(&mut staged, target_name) {
            let primary_error = if is_noclobber_conflict(&error) {
                EditError::CommitConflict {
                    path: path.to_path_buf(),
                }
            } else {
                EditError::Io(error)
            };
            return Err(cleanup_staged_entry_after_failure(
                &stable_parent,
                &staged,
                path,
                primary_error,
            ));
        }
        return Ok(committed_identity);
    };

    let mut backup =
        match stable_parent.take_expected_target(guarded, target_name, expected_identity) {
            Ok(backup) => backup,
            Err(error) => {
                return Err(cleanup_staged_entry_after_failure(
                    &stable_parent,
                    &staged,
                    path,
                    error,
                ));
            }
        };
    if let Err(error) = stable_parent.move_entry_noclobber(&mut staged, target_name) {
        let primary_error = if is_noclobber_conflict(&error) {
            EditError::CommitConflict {
                path: path.to_path_buf(),
            }
        } else {
            EditError::Io(error)
        };
        let primary_error = match stable_parent.move_entry_noclobber(&mut backup, target_name) {
            Ok(()) => primary_error,
            Err(restore_error) => EditError::CommitConflictPreserved {
                path: path.to_path_buf(),
                preserved_path: stable_parent.display_entry(&backup),
                reason: bounded_preserved_failure_reason(
                    &primary_error.to_string(),
                    PRESERVED_CONFLICT_RESTORE_LABEL,
                    &restore_error.to_string(),
                ),
            },
        };
        return Err(cleanup_staged_entry_after_failure(
            &stable_parent,
            &staged,
            path,
            primary_error,
        ));
    }

    if let Err(error) = stable_parent.retire_detached_target(&backup) {
        return Err(EditError::PartialCommit {
            path: path.to_path_buf(),
            preserved_path: stable_parent.display_entry(&backup),
            reason: error.to_string(),
        });
    }
    Ok(committed_identity)
}

fn cleanup_staged_entry_after_failure(
    parent: &StableWriteParent,
    staged: &StableFileEntry,
    target_path: &Utf8Path,
    primary_error: EditError,
) -> EditError {
    let Err(cleanup_error) = parent.delete_entry_if_same(staged) else {
        return primary_error;
    };
    staged_cleanup_failure(
        target_path,
        parent.display_entry(staged),
        primary_error,
        cleanup_error,
    )
}

fn staged_cleanup_failure(
    target_path: &Utf8Path,
    staged_path: Utf8PathBuf,
    primary_error: EditError,
    cleanup_error: std::io::Error,
) -> EditError {
    let mut preserved_paths = edit_error_preserved_paths(&primary_error);
    push_unique_recovery_path(&mut preserved_paths, staged_path);
    let reason = bounded_preserved_failure_reason(
        &primary_error.to_string(),
        STAGED_CLEANUP_FAILURE_LABEL,
        &cleanup_error.to_string(),
    );
    if preserved_paths.len() == 1 {
        return EditError::PartialCommit {
            path: target_path.to_path_buf(),
            preserved_path: preserved_paths.pop().expect("one preserved path"),
            reason,
        };
    }
    EditError::RecoveryFilesPreserved {
        path: target_path.to_path_buf(),
        preserved_paths,
        reason,
    }
}

fn edit_error_preserved_paths(error: &EditError) -> Vec<Utf8PathBuf> {
    match error {
        EditError::CommitConflictPreserved { preserved_path, .. }
        | EditError::PartialCommit { preserved_path, .. }
        | EditError::RollbackConflictPreserved { preserved_path, .. } => {
            vec![preserved_path.clone()]
        }
        EditError::RecoveryFilesPreserved {
            preserved_paths, ..
        } => preserved_paths.clone(),
        _ => Vec::new(),
    }
}

fn push_unique_recovery_path(paths: &mut Vec<Utf8PathBuf>, path: Utf8PathBuf) {
    if paths.contains(&path) {
        return;
    }
    assert!(
        paths.len() < MAX_EDIT_RECOVERY_PATHS,
        "bounded edit recovery path invariant exceeded"
    );
    paths.push(path);
}

pub(crate) fn delete_file_conditionally(
    guarded: &GuardedPath,
    expected_identity: &FileContentIdentity,
) -> Result<(), EditError> {
    let path = guarded.absolute.as_path();
    let parent = path
        .parent()
        .ok_or_else(|| EditError::Message("file path has no parent".to_string()))?;
    let target_name = path
        .file_name()
        .ok_or_else(|| EditError::Message(format!("file path `{path}` has no name")))?;
    let stable_parent = StableWriteParent::open(parent, guarded)?;
    let backup = stable_parent.take_expected_target(guarded, target_name, expected_identity)?;
    stable_parent
        .retire_detached_target(&backup)
        .map_err(|error| EditError::PartialCommit {
            path: path.to_path_buf(),
            preserved_path: stable_parent.display_entry(&backup),
            reason: error.to_string(),
        })
}

struct StableWriteParent {
    path: Utf8PathBuf,
    file: File,
}

struct StableFileEntry {
    name: String,
    file: File,
    #[cfg_attr(windows, allow(dead_code))]
    identity: StableFileIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StableFileIdentity {
    #[cfg(windows)]
    volume_serial_number: u32,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    file_type: u32,
    #[cfg(not(any(unix, windows)))]
    unsupported: (),
}

#[cfg(unix)]
impl StableFileIdentity {
    fn is_regular_file(&self) -> bool {
        self.file_type == UNIX_REGULAR_FILE_TYPE
    }
}

#[cfg(unix)]
fn unix_file_type(mode: u32) -> u32 {
    mode & UNIX_FILE_TYPE_MASK
}

impl StableWriteParent {
    fn open(parent: &Utf8Path, guarded: &GuardedPath) -> Result<Self, EditError> {
        let file = open_stable_parent(parent.as_std_path()).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                EditError::MissingParent {
                    path: guarded.absolute.clone(),
                    parent: parent.to_path_buf(),
                }
            } else {
                EditError::Io(error)
            }
        })?;
        PathGuard::validate_open_parent(guarded, &file)
            .map_err(|error| EditError::Message(error.to_string()))?;
        Ok(Self {
            path: parent.to_path_buf(),
            file,
        })
    }

    fn create_entry(
        &self,
        purpose: &str,
        target_path: &Utf8Path,
    ) -> Result<StableFileEntry, EditError> {
        for _ in 0..MAX_UNIQUE_WRITE_NAMES {
            let name = unique_write_name(purpose);
            match create_relative_file(self, &name) {
                Ok(file) => {
                    let identity = stable_file_identity(&file).map_err(|error| {
                        EditError::PartialCommit {
                            path: target_path.to_path_buf(),
                            preserved_path: self.path.join(&name),
                            reason: bounded_preserved_failure_reason(
                                "a private staged file was created but its stable identity could not be verified, so exact cleanup was not safe",
                                STAGED_IDENTITY_FAILURE_LABEL,
                                &error.to_string(),
                            ),
                        }
                    })?;
                    return Ok(StableFileEntry {
                        name,
                        file,
                        identity,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(EditError::Message(format!(
            "could not allocate a unique staged file in `{}`",
            self.path
        )))
    }

    fn take_expected_target(
        &self,
        guarded: &GuardedPath,
        target_name: &str,
        expected_identity: &FileContentIdentity,
    ) -> Result<StableFileEntry, EditError> {
        take_expected_target(self, guarded, target_name, expected_identity)
    }

    fn move_entry_noclobber(
        &self,
        entry: &mut StableFileEntry,
        destination_name: &str,
    ) -> std::io::Result<()> {
        move_entry_noclobber(self, entry, destination_name)?;
        entry.name = destination_name.to_string();
        Ok(())
    }

    fn delete_entry_if_same(&self, entry: &StableFileEntry) -> std::io::Result<()> {
        delete_entry_if_same(self, entry)
    }

    fn retire_detached_target(&self, entry: &StableFileEntry) -> std::io::Result<()> {
        retire_detached_target(self, entry)
    }

    fn display_entry(&self, entry: &StableFileEntry) -> Utf8PathBuf {
        self.path.join(&entry.name)
    }
}

fn unique_write_name(purpose: &str) -> String {
    format!(".moyai-write-{purpose}-{}.tmp", ulid::Ulid::new())
}

fn is_noclobber_conflict(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::AlreadyExists {
        return true;
    }
    #[cfg(windows)]
    {
        matches!(error.raw_os_error(), Some(80) | Some(183))
    }
    #[cfg(not(windows))]
    false
}

#[cfg(windows)]
fn take_expected_target(
    parent: &StableWriteParent,
    guarded: &GuardedPath,
    _target_name: &str,
    expected_identity: &FileContentIdentity,
) -> Result<StableFileEntry, EditError> {
    let file = open_windows_mutation_file(guarded.absolute.as_std_path()).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(32) {
            commit_conflict(&guarded.absolute, None)
        } else {
            EditError::Io(error)
        }
    })?;
    PathGuard::validate_open_file(guarded, &file)
        .map_err(|error| EditError::Message(error.to_string()))?;
    let current =
        file_content_identity_from_handle(&file, &guarded.absolute, expected_identity.size_bytes)?;
    if &current != expected_identity {
        return Err(commit_conflict(&guarded.absolute, None));
    }
    let identity = stable_file_identity(&file)?;
    for _ in 0..MAX_UNIQUE_WRITE_NAMES {
        let name = unique_write_name("backup");
        match windows_rename_by_handle(&file, &parent.file, &name) {
            Ok(()) => {
                return Ok(StableFileEntry {
                    name,
                    file,
                    identity,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(EditError::Message(format!(
        "could not allocate a unique rollback name for `{}`",
        guarded.absolute
    )))
}

#[cfg(unix)]
fn take_expected_target(
    parent: &StableWriteParent,
    guarded: &GuardedPath,
    target_name: &str,
    expected_identity: &FileContentIdentity,
) -> Result<StableFileEntry, EditError> {
    let mut backup_name = None;
    for _ in 0..MAX_UNIQUE_WRITE_NAMES {
        let candidate = unique_write_name("backup");
        match unix_rename_noreplace(parent, target_name, &candidate) {
            Ok(()) => {
                backup_name = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(commit_conflict(&guarded.absolute, None));
            }
            Err(error) => {
                return Err(EditError::Message(format!(
                    "path `{}` could not be detached for a conditional commit: {error}",
                    guarded.absolute
                )));
            }
        }
    }
    let backup_name = backup_name.ok_or_else(|| {
        EditError::Message(format!(
            "could not allocate a unique rollback name for `{}`",
            guarded.absolute
        ))
    })?;
    let file = match open_relative_read(parent, &backup_name) {
        Ok(file) => file,
        Err(error) => {
            return Err(preserve_detached_name_after_validation_failure(
                parent,
                guarded,
                &backup_name,
                EditError::Io(error),
            ));
        }
    };
    validate_detached_target(
        parent,
        guarded,
        target_name,
        backup_name,
        file,
        expected_identity,
    )
}

#[cfg(unix)]
fn validate_detached_target(
    parent: &StableWriteParent,
    guarded: &GuardedPath,
    target_name: &str,
    backup_name: String,
    file: File,
    expected_identity: &FileContentIdentity,
) -> Result<StableFileEntry, EditError> {
    let identity = match stable_opened_entry_identity(&file) {
        Ok(identity) => identity,
        Err(error) => {
            return Err(preserve_detached_name_after_validation_failure(
                parent,
                guarded,
                &backup_name,
                EditError::Io(error),
            ));
        }
    };
    let mut backup = StableFileEntry {
        identity,
        name: backup_name,
        file,
    };
    if !backup.identity.is_regular_file() {
        return Err(restore_detached_target_after_validation_failure(
            parent,
            guarded,
            target_name,
            &mut backup,
            EditError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "conditional file mutation requires a regular file",
            )),
        ));
    }
    let current = match file_content_identity_from_handle(
        &backup.file,
        &guarded.absolute,
        expected_identity.size_bytes,
    ) {
        Ok(current) => current,
        Err(error) => {
            return Err(restore_detached_target_after_validation_failure(
                parent,
                guarded,
                target_name,
                &mut backup,
                error,
            ));
        }
    };
    if &current != expected_identity {
        return Err(restore_detached_target_after_validation_failure(
            parent,
            guarded,
            target_name,
            &mut backup,
            commit_conflict(&guarded.absolute, None),
        ));
    }
    Ok(backup)
}

#[cfg(unix)]
fn preserve_detached_name_after_validation_failure(
    parent: &StableWriteParent,
    guarded: &GuardedPath,
    backup_name: &str,
    primary_error: EditError,
) -> EditError {
    EditError::CommitConflictPreserved {
        path: guarded.absolute.clone(),
        preserved_path: parent.path.join(backup_name),
        reason: bounded_preserved_conflict_reason(
            &primary_error.to_string(),
            "detached entry identity was unavailable, so name-only restore was refused",
        ),
    }
}

#[cfg(unix)]
fn restore_detached_target_after_validation_failure(
    parent: &StableWriteParent,
    guarded: &GuardedPath,
    target_name: &str,
    backup: &mut StableFileEntry,
    primary_error: EditError,
) -> EditError {
    match parent.move_entry_noclobber(backup, target_name) {
        Ok(()) => commit_conflict(&guarded.absolute, None),
        Err(restore_error) => EditError::CommitConflictPreserved {
            path: guarded.absolute.clone(),
            preserved_path: parent.display_entry(backup),
            reason: bounded_preserved_conflict_reason(
                &primary_error.to_string(),
                &restore_error.to_string(),
            ),
        },
    }
}

#[cfg(unix)]
fn bounded_preserved_conflict_reason(primary: &str, restore: &str) -> String {
    bounded_preserved_failure_reason(primary, PRESERVED_CONFLICT_RESTORE_LABEL, restore)
}

fn bounded_preserved_failure_reason(primary: &str, label: &str, secondary: &str) -> String {
    let detail_bytes = MAX_EDIT_RECOVERY_REASON_BYTES.saturating_sub(label.len());
    let primary_budget = detail_bytes / 2;
    let primary = truncate_utf8_bytes(primary, primary_budget);
    let secondary_budget = detail_bytes.saturating_sub(primary.len());
    let secondary = truncate_utf8_bytes(secondary, secondary_budget);
    format!("{primary}{label}{secondary}")
}

fn truncate_utf8_bytes(value: &str, maximum_bytes: usize) -> String {
    const TRUNCATED: &str = "...[truncated]";
    if value.len() <= maximum_bytes {
        return value.to_string();
    }
    if maximum_bytes <= TRUNCATED.len() {
        let mut end = maximum_bytes.min(value.len());
        while !value.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        return value[..end].to_string();
    }
    let mut end = maximum_bytes.saturating_sub(TRUNCATED.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let mut bounded = String::with_capacity(maximum_bytes);
    bounded.push_str(&value[..end]);
    bounded.push_str(TRUNCATED);
    bounded
}

#[cfg(not(any(unix, windows)))]
fn take_expected_target(
    _parent: &StableWriteParent,
    guarded: &GuardedPath,
    _target_name: &str,
    _expected_identity: &FileContentIdentity,
) -> Result<StableFileEntry, EditError> {
    Err(EditError::Message(format!(
        "conditional file replacement is unsupported on this platform for `{}`",
        guarded.absolute
    )))
}

fn commit_conflict(path: &Utf8Path, preserved: Option<(Utf8PathBuf, std::io::Error)>) -> EditError {
    match preserved {
        Some((preserved_path, error)) => EditError::CommitConflictPreserved {
            path: path.to_path_buf(),
            preserved_path,
            reason: error.to_string(),
        },
        None => EditError::CommitConflict {
            path: path.to_path_buf(),
        },
    }
}

fn file_content_identity_from_handle(
    file: &File,
    path: &Utf8Path,
    expected_size_bytes: u64,
) -> Result<FileContentIdentity, EditError> {
    use sha2::{Digest as _, Sha256};

    let before = file.metadata()?;
    if !before.is_file() {
        return Err(EditError::Message(
            "conditional file mutation requires a regular file".to_string(),
        ));
    }
    if before.len() != expected_size_bytes {
        return Err(EditError::CommitConflict {
            path: path.to_path_buf(),
        });
    }
    let maximum_read_bytes = expected_size_bytes.checked_add(1).ok_or_else(|| {
        EditError::Message(format!(
            "path `{path}` exceeded the supported identity byte count"
        ))
    })?;
    let before_mtime_ms = metadata_mtime_ms(&before);
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let mut reader = reader.take(maximum_read_bytes);
    let mut digest = Sha256::new();
    let mut bytes_read = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes_read = bytes_read.checked_add(count as u64).ok_or_else(|| {
            EditError::Message(format!(
                "path `{path}` exceeded the supported identity byte count"
            ))
        })?;
        if bytes_read > expected_size_bytes {
            return Err(EditError::CommitConflict {
                path: path.to_path_buf(),
            });
        }
        digest.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    if bytes_read != expected_size_bytes
        || after.len() != expected_size_bytes
        || metadata_mtime_ms(&after) != before_mtime_ms
    {
        return Err(EditError::CommitConflict {
            path: path.to_path_buf(),
        });
    }
    Ok(FileContentIdentity {
        mtime_ms: before_mtime_ms,
        size_bytes: before.len(),
        content_sha256: format!("{:x}", digest.finalize()),
    })
}

fn metadata_mtime_ms(metadata: &fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as i64)
}

#[cfg(windows)]
fn open_stable_parent(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_APPEND_DATA, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, FILE_WRITE_DATA,
        SYNCHRONIZE,
    };

    fs::OpenOptions::new()
        .access_mode(
            FILE_READ_ATTRIBUTES | FILE_TRAVERSE | FILE_WRITE_DATA | FILE_APPEND_DATA | SYNCHRONIZE,
        )
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(unix)]
fn open_stable_parent(path: &Path) -> std::io::Result<File> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::io::FromRawFd as _;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "file parent path contains NUL",
        )
    })?;
    // SAFETY: `path` is a live C string. O_NOFOLLOW pins the final directory object and the
    // returned descriptor becomes the owner for every subsequent relative mutation.
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: ownership of the newly returned descriptor transfers exactly once.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

#[cfg(not(any(unix, windows)))]
fn open_stable_parent(_path: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stable file mutation handles are unsupported on this platform",
    ))
}

#[cfg(windows)]
#[repr(C)]
struct NativeUnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[cfg(windows)]
#[repr(C)]
struct NativeObjectAttributes {
    length: u32,
    root_directory: *mut core::ffi::c_void,
    object_name: *mut NativeUnicodeString,
    attributes: u32,
    security_descriptor: *mut core::ffi::c_void,
    security_quality_of_service: *mut core::ffi::c_void,
}

#[cfg(windows)]
#[repr(C)]
struct NativeIoStatusBlock {
    status_or_pointer: *mut core::ffi::c_void,
    information: usize,
}

#[cfg(windows)]
#[link(name = "ntdll")]
unsafe extern "system" {
    #[link_name = "NtCreateFile"]
    fn nt_create_file(
        file_handle: *mut *mut core::ffi::c_void,
        desired_access: u32,
        object_attributes: *const NativeObjectAttributes,
        io_status_block: *mut NativeIoStatusBlock,
        allocation_size: *const i64,
        file_attributes: u32,
        share_access: u32,
        create_disposition: u32,
        create_options: u32,
        ea_buffer: *const core::ffi::c_void,
        ea_length: u32,
    ) -> i32;

    #[link_name = "RtlNtStatusToDosError"]
    fn rtl_nt_status_to_dos_error(status: i32) -> u32;
}

#[cfg(windows)]
fn create_relative_file(parent: &StableWriteParent, name: &str) -> std::io::Result<File> {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ,
    };

    const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;
    const FILE_CREATE: u32 = 2;
    const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x0000_0020;
    const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
    const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    if name.is_empty()
        || matches!(name, "." | "..")
        || name
            .chars()
            .any(|character| matches!(character, '\0' | '/' | '\\' | ':'))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "stable staged file name must be one non-empty path component",
        ));
    }

    let mut wide_name = name.encode_utf16().collect::<Vec<_>>();
    let length_bytes = wide_name
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "stable staged file name is too long for a native relative open",
            )
        })?;
    wide_name.push(0);
    let maximum_length_bytes = wide_name
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "stable staged file name is too long for a native relative open",
            )
        })?;
    let mut object_name = NativeUnicodeString {
        length: length_bytes,
        maximum_length: maximum_length_bytes,
        buffer: wide_name.as_mut_ptr(),
    };
    let object_attributes = NativeObjectAttributes {
        length: std::mem::size_of::<NativeObjectAttributes>() as u32,
        root_directory: parent.file.as_raw_handle(),
        object_name: &mut object_name,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: std::ptr::null_mut(),
        security_quality_of_service: std::ptr::null_mut(),
    };
    let mut io_status = NativeIoStatusBlock {
        status_or_pointer: std::ptr::null_mut(),
        information: 0,
    };
    let mut handle = std::ptr::null_mut();
    // SAFETY: every native structure and the UTF-16 component remain live for the call. The
    // stable directory handle is the root owner, FILE_CREATE provides no-clobber semantics, and
    // FILE_OPEN_REPARSE_POINT prevents the new entry from being followed through a reparse point.
    let status = unsafe {
        nt_create_file(
            &mut handle,
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE,
            &object_attributes,
            &mut io_status,
            std::ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            FILE_SHARE_READ,
            FILE_CREATE,
            FILE_SYNCHRONOUS_IO_NONALERT | FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        if !handle.is_null() && handle != (-1_isize as *mut core::ffi::c_void) {
            // SAFETY: a failing native call should not return an owned handle, but if it does,
            // transfer it exactly once so it is closed before the error leaves this function.
            drop(unsafe { File::from_raw_handle(handle) });
        }
        // SAFETY: conversion has no pointer arguments and accepts the status just returned by
        // NtCreateFile.
        let windows_error = unsafe { rtl_nt_status_to_dos_error(status) };
        return Err(std::io::Error::from_raw_os_error(windows_error as i32));
    }
    if handle.is_null() || handle == (-1_isize as *mut core::ffi::c_void) {
        return Err(std::io::Error::other(
            "native relative create succeeded without a valid file handle",
        ));
    }
    // SAFETY: NtCreateFile returned a unique owned handle and ownership transfers once to File.
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(unix)]
fn create_relative_file(parent: &StableWriteParent, name: &str) -> std::io::Result<File> {
    use std::os::unix::io::{AsRawFd as _, FromRawFd as _};

    let name = unix_relative_name(name)?;
    // SAFETY: the parent descriptor and name are live, the name is one component, and O_EXCL
    // prevents an existing directory entry from becoming the staged file owner.
    let descriptor = unsafe {
        libc::openat(
            parent.file.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600 as libc::mode_t,
        )
    };
    if descriptor < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: ownership of the newly returned descriptor transfers exactly once.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

#[cfg(not(any(unix, windows)))]
fn create_relative_file(_parent: &StableWriteParent, _name: &str) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stable staged file creation is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn open_windows_mutation_file(path: &Path) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_SHARE_READ,
    };

    fs::OpenOptions::new()
        .access_mode(FILE_GENERIC_READ | DELETE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(unix)]
fn open_relative_read(parent: &StableWriteParent, name: &str) -> std::io::Result<File> {
    use std::os::unix::io::{AsRawFd as _, FromRawFd as _};

    let name = unix_relative_name(name)?;
    // SAFETY: the stable parent descriptor and NUL-terminated single component are live.
    // O_NONBLOCK ensures that a raced non-regular entry such as a FIFO cannot stall before the
    // same opened handle is rejected by the regular-file and content-identity validation.
    let descriptor = unsafe {
        libc::openat(
            parent.file.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: ownership of the newly returned descriptor transfers exactly once.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

#[cfg(windows)]
fn move_entry_noclobber(
    parent: &StableWriteParent,
    entry: &StableFileEntry,
    destination_name: &str,
) -> std::io::Result<()> {
    windows_rename_by_handle(&entry.file, &parent.file, destination_name)
}

#[cfg(unix)]
fn move_entry_noclobber(
    parent: &StableWriteParent,
    entry: &StableFileEntry,
    destination_name: &str,
) -> std::io::Result<()> {
    match relative_file_identity(parent, &entry.name)? {
        Some(identity) if identity == entry.identity => {
            unix_rename_noreplace(parent, &entry.name, destination_name)
        }
        Some(_) => Err(std::io::Error::new(
            std::io::ErrorKind::Other,
            "staged file entry changed before its conditional rename",
        )),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "staged file entry disappeared before its conditional rename",
        )),
    }
}

#[cfg(not(any(unix, windows)))]
fn move_entry_noclobber(
    _parent: &StableWriteParent,
    _entry: &StableFileEntry,
    _destination_name: &str,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "conditional file rename is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn delete_entry_if_same(
    _parent: &StableWriteParent,
    entry: &StableFileEntry,
) -> std::io::Result<()> {
    windows_delete_by_handle(&entry.file)
}

#[cfg(unix)]
fn delete_entry_if_same(
    parent: &StableWriteParent,
    entry: &StableFileEntry,
) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd as _;

    match relative_file_identity(parent, &entry.name)? {
        Some(identity) if identity == entry.identity => {}
        Some(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "private file entry changed before cleanup",
            ));
        }
        None => return Ok(()),
    }
    let name = unix_relative_name(&entry.name)?;
    // SAFETY: the stable parent descriptor and single component are live; the immediately prior
    // identity check limits cleanup to the private entry allocated by this operation.
    let result = unsafe { libc::unlinkat(parent.file.as_raw_fd(), name.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(unix, windows)))]
fn delete_entry_if_same(
    _parent: &StableWriteParent,
    _entry: &StableFileEntry,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stable file deletion is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn retire_detached_target(
    parent: &StableWriteParent,
    entry: &StableFileEntry,
) -> std::io::Result<()> {
    delete_entry_if_same(parent, entry)
}

#[cfg(unix)]
fn retire_detached_target(
    _parent: &StableWriteParent,
    _entry: &StableFileEntry,
) -> std::io::Result<()> {
    // Unix cannot atomically prove that no writable descriptor opened before the rename still
    // references this inode. An identity check, advisory lock, or breakable Linux lease cannot
    // close the final check-to-unlink race, so keep the private name as the recovery path.
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "detached target cannot be safely removed on Unix because a previously opened writable file descriptor may still modify it; the backup was preserved",
    ))
}

#[cfg(not(any(unix, windows)))]
fn retire_detached_target(
    _parent: &StableWriteParent,
    _entry: &StableFileEntry,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "safe detached target removal is unsupported on this platform; the backup was preserved",
    ))
}

#[cfg(windows)]
fn stable_file_identity(file: &File) -> std::io::Result<StableFileIdentity> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        GetFileInformationByHandle,
    };

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` keeps its handle live and `information` is the exact writable output type.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if information.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "conditional file mutation requires a regular non-reparse file",
        ));
    }
    Ok(StableFileIdentity {
        volume_serial_number: information.dwVolumeSerialNumber,
        file_index: (u64::from(information.nFileIndexHigh) << 32)
            | u64::from(information.nFileIndexLow),
    })
}

#[cfg(unix)]
fn stable_file_identity(file: &File) -> std::io::Result<StableFileIdentity> {
    let identity = stable_opened_entry_identity(file)?;
    if !identity.is_regular_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "conditional file mutation requires a regular file",
        ));
    }
    Ok(identity)
}

#[cfg(unix)]
fn stable_opened_entry_identity(file: &File) -> std::io::Result<StableFileIdentity> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata()?;
    Ok(StableFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        file_type: unix_file_type(metadata.mode()),
    })
}

#[cfg(not(any(unix, windows)))]
fn stable_file_identity(_file: &File) -> std::io::Result<StableFileIdentity> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stable file identity is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn relative_file_identity(
    parent: &StableWriteParent,
    name: &str,
) -> std::io::Result<Option<StableFileIdentity>> {
    use std::os::unix::io::AsRawFd as _;

    let name = unix_relative_name(name)?;
    // SAFETY: the stable descriptor, output structure, and single-component name are live;
    // AT_SYMLINK_NOFOLLOW observes the directory entry rather than a link target.
    let mut status = unsafe { std::mem::zeroed::<libc::stat>() };
    let result = unsafe {
        libc::fstatat(
            parent.file.as_raw_fd(),
            name.as_ptr(),
            &mut status,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(error);
    }
    Ok(Some(StableFileIdentity {
        device: status.st_dev as u64,
        inode: status.st_ino as u64,
        file_type: unix_file_type(status.st_mode as u32),
    }))
}

#[cfg(unix)]
fn unix_relative_name(name: &str) -> std::io::Result<std::ffi::CString> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "file entry name must be one normal component",
        ));
    }
    std::ffi::CString::new(name.as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "file entry name contains NUL",
        )
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn unix_rename_noreplace(
    parent: &StableWriteParent,
    source_name: &str,
    destination_name: &str,
) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd as _;

    let source_name = unix_relative_name(source_name)?;
    let destination_name = unix_relative_name(destination_name)?;
    // SAFETY: the stable directory descriptor and both single-component names are live.
    let result = unsafe {
        libc::renameat2(
            parent.file.as_raw_fd(),
            source_name.as_ptr(),
            parent.file.as_raw_fd(),
            destination_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_vendor = "apple")]
fn unix_rename_noreplace(
    parent: &StableWriteParent,
    source_name: &str,
    destination_name: &str,
) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd as _;

    let source_name = unix_relative_name(source_name)?;
    let destination_name = unix_relative_name(destination_name)?;
    // SAFETY: the stable directory descriptor and both single-component names are live.
    let result = unsafe {
        libc::renameatx_np(
            parent.file.as_raw_fd(),
            source_name.as_ptr(),
            parent.file.as_raw_fd(),
            destination_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn unix_rename_noreplace(
    _parent: &StableWriteParent,
    _source_name: &str,
    _destination_name: &str,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-clobber rename is unsupported on this Unix platform",
    ))
}

#[cfg(windows)]
fn windows_rename_by_handle(
    source: &File,
    destination_parent: &File,
    destination_name: &str,
) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_RENAME_INFORMATION, FILE_RENAME_INFORMATION_0, FileRenameInformation,
        NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

    let mut wide = std::ffi::OsStr::new(destination_name)
        .encode_wide()
        .collect::<Vec<_>>();
    let name_bytes = wide
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .and_then(|length| u32::try_from(length).ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "conditional rename destination is too long",
            )
        })?;
    wide.push(0);
    let buffer_bytes = std::mem::size_of::<FILE_RENAME_INFORMATION>()
        .checked_add(usize::try_from(name_bytes).expect("u32 name length fits usize"))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "conditional rename buffer is too large",
            )
        })?;
    let mut storage = vec![0usize; buffer_bytes.div_ceil(std::mem::size_of::<usize>())];
    let information = storage.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: the aligned buffer is large enough for the header and UTF-16 name; both handles
    // remain live for the synchronous call and ReplaceIfExists=false is the no-clobber boundary.
    unsafe {
        information.write(FILE_RENAME_INFORMATION {
            Anonymous: FILE_RENAME_INFORMATION_0 {
                ReplaceIfExists: false,
            },
            RootDirectory: destination_parent.as_raw_handle(),
            FileNameLength: name_bytes,
            FileName: [0],
        });
        std::ptr::copy_nonoverlapping(
            wide.as_ptr(),
            std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
            wide.len(),
        );
    }
    let mut io_status = IO_STATUS_BLOCK::default();
    // SAFETY: all handles and buffers remain live and have the exact native layout.
    let status = unsafe {
        NtSetInformationFile(
            source.as_raw_handle(),
            &mut io_status,
            information.cast(),
            u32::try_from(buffer_bytes).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "conditional rename buffer exceeds the native API limit",
                )
            })?,
            FileRenameInformation,
        )
    };
    if status >= 0 {
        Ok(())
    } else {
        // SAFETY: conversion accepts any NTSTATUS returned by NtSetInformationFile.
        let code = unsafe { RtlNtStatusToDosError(status) };
        Err(std::io::Error::from_raw_os_error(code as i32))
    }
}

#[cfg(windows)]
fn windows_delete_by_handle(entry: &File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };

    let information = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `entry` owns the exact file being deleted and the information structure has the
    // documented fixed layout.
    let succeeded = unsafe {
        SetFileInformationByHandle(
            entry.as_raw_handle(),
            FileDispositionInfo,
            std::ptr::from_ref(&information).cast(),
            u32::try_from(std::mem::size_of::<FILE_DISPOSITION_INFO>())
                .expect("FILE_DISPOSITION_INFO size fits u32"),
        )
    };
    if succeeded == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn written_text_identity(file: &File, text: &str) -> Result<FileContentIdentity, EditError> {
    use sha2::{Digest as _, Sha256};

    let metadata = file.metadata()?;
    Ok(FileContentIdentity {
        mtime_ms: metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_millis() as i64),
        size_bytes: metadata.len(),
        content_sha256: format!("{:x}", Sha256::digest(text.as_bytes())),
    })
}

pub(crate) fn read_text_file_with_identity(
    guarded: &GuardedPath,
    maximum_bytes: u64,
) -> Result<(String, FileContentIdentity), EditError> {
    use sha2::{Digest as _, Sha256};

    let path = guarded.absolute.as_path();
    let mut file = PathGuard::open_validated_read_file(guarded).map_err(|error| match error {
        crate::error::WorkspaceError::Io(error) => EditError::Io(error),
        crate::error::WorkspaceError::Message(message) => EditError::Message(message),
    })?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(EditError::Message(format!(
            "path `{path}` is not a regular file"
        )));
    }
    ensure_edit_read_limit(path, before.len(), maximum_bytes)?;
    let capacity = usize::try_from(before.len()).unwrap_or_default();
    let mut bytes = Vec::with_capacity(capacity);
    Read::by_ref(&mut file)
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let bytes_read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    ensure_edit_read_limit(path, bytes_read, maximum_bytes)?;
    let after = file.metadata()?;
    if before.len() != after.len()
        || after.len() != bytes_read
        || before.modified().ok() != after.modified().ok()
    {
        return Err(EditError::Message(format!(
            "path `{path}` changed while its contents were being read"
        )));
    }
    let identity = FileContentIdentity {
        mtime_ms: after
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_millis() as i64),
        size_bytes: after.len(),
        content_sha256: format!("{:x}", Sha256::digest(&bytes)),
    };
    let text = String::from_utf8(bytes).map_err(|error| {
        EditError::Message(format!("path `{path}` is not valid UTF-8 text: {error}"))
    })?;
    Ok((text, identity))
}

pub(crate) fn to_summary(change: &FileChange) -> ChangeSummary {
    ChangeSummary {
        change_id: change.id,
        kind: change.kind,
        path_before: change.path_before.clone(),
        path_after: change.path_after.clone(),
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;
    use std::io::Write as _;
    #[cfg(unix)]
    use std::io::{Seek as _, SeekFrom};

    use crate::edit::read_file_with_identity;
    use crate::error::EditError;
    use crate::workspace::{AccessKind, GuardedPath, PathGuard, WorkspaceDiscovery};

    fn guarded_path(path: &camino::Utf8Path) -> GuardedPath {
        let workspace = WorkspaceDiscovery::discover_fixed_root(
            path.parent().expect("target parent"),
            &crate::config::ResolvedConfig::default(),
        )
        .expect("test workspace");
        PathGuard::require_path(&workspace, path, AccessKind::Edit).expect("guarded target")
    }

    #[test]
    fn guarded_text_read_returns_content_and_identity_from_the_validated_handle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "bounded content").expect("seed source");
        let guarded = guarded_path(&path);

        let (text, identity) =
            super::read_text_file_with_identity(&guarded, 1_024).expect("guarded text read");

        assert_eq!(text, "bounded content");
        assert_eq!(identity.size_bytes, text.len() as u64);
        assert_eq!(identity.content_sha256.len(), 64);
    }

    #[cfg(unix)]
    #[test]
    fn guarded_text_read_rejects_a_namespace_swap_before_open() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("workspace")).expect("utf8 root");
        std::fs::create_dir(&root).expect("workspace root");
        let target = root.join("source.txt");
        let outside =
            Utf8PathBuf::from_path_buf(temp.path().join("outside.txt")).expect("utf8 outside");
        std::fs::write(&target, "approved object").expect("seed target");
        std::fs::write(&outside, "outside object").expect("seed outside");
        let workspace = WorkspaceDiscovery::discover_fixed_root(
            &root,
            &crate::config::ResolvedConfig::default(),
        )
        .expect("workspace");
        let guarded = PathGuard::require_path(&workspace, &target, AccessKind::Edit)
            .expect("guard original target");
        std::fs::remove_file(&target).expect("remove original target");
        std::os::unix::fs::symlink(&outside, &target).expect("replace target with outside symlink");

        let error = super::read_text_file_with_identity(&guarded, 1_024)
            .expect_err("namespace replacement must not be read");

        assert!(
            error
                .to_string()
                .contains("changed after its workspace boundary check")
        );
    }

    #[test]
    fn failed_stage_cleanup_returns_single_typed_recovery_path_with_bounded_reason() {
        let target = Utf8PathBuf::from("target.txt");
        let staged = Utf8PathBuf::from(".moyai-write-stage-test.tmp");
        let error = super::staged_cleanup_failure(
            &target,
            staged.clone(),
            EditError::Message("primary failure ".repeat(1_024)),
            std::io::Error::other("cleanup failure ".repeat(1_024)),
        );
        let EditError::PartialCommit {
            path,
            preserved_path,
            reason,
        } = error
        else {
            panic!("one failed staged cleanup must use the existing typed recovery error");
        };

        assert_eq!(path, target);
        assert_eq!(preserved_path, staged);
        assert!(reason.contains(super::STAGED_CLEANUP_FAILURE_LABEL));
        assert!(reason.len() <= super::MAX_EDIT_RECOVERY_REASON_BYTES);
    }

    #[test]
    fn failed_stage_cleanup_retains_existing_backup_and_stage_paths_in_stable_order() {
        let target = Utf8PathBuf::from("target.txt");
        let backup = Utf8PathBuf::from(".moyai-write-backup-test.tmp");
        let staged = Utf8PathBuf::from(".moyai-write-stage-test.tmp");
        let error = super::staged_cleanup_failure(
            &target,
            staged.clone(),
            EditError::CommitConflictPreserved {
                path: target.clone(),
                preserved_path: backup.clone(),
                reason: "restore failure ".repeat(1_024),
            },
            std::io::Error::other("cleanup failure ".repeat(1_024)),
        );
        let EditError::RecoveryFilesPreserved {
            path,
            preserved_paths,
            reason,
        } = error
        else {
            panic!("backup and staged paths must both remain typed");
        };

        assert_eq!(path, target);
        assert_eq!(preserved_paths, vec![backup, staged]);
        assert!(reason.contains(super::STAGED_CLEANUP_FAILURE_LABEL));
        assert!(reason.len() <= super::MAX_EDIT_RECOVERY_REASON_BYTES);
    }

    #[test]
    fn no_clobber_create_preserves_external_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("new.txt")).expect("utf8 path");
        let guarded = guarded_path(&path);
        std::fs::write(&path, "external").expect("seed external file");

        let error = super::write_text_file_conditionally(&guarded, "agent", None, |_| Ok(()))
            .expect_err("no-clobber write must reject an existing file");

        assert!(matches!(error, EditError::CommitConflict { .. }));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read file"),
            "external"
        );
    }

    #[test]
    fn conditional_replace_preserves_replacement_after_preparation() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "baseline").expect("seed baseline");
        let (_, expected) = read_file_with_identity(&path, 1_024).expect("baseline identity");
        let guarded = guarded_path(&path);
        let replacement_path = path.clone();

        let error =
            super::write_text_file_conditionally(&guarded, "agent", Some(&expected), move |_| {
                let mut replacement = tempfile::NamedTempFile::new_in(
                    replacement_path.parent().expect("replacement parent"),
                )?;
                replacement.write_all(b"external")?;
                replacement.flush()?;
                replacement
                    .persist(&replacement_path)
                    .map_err(|error| EditError::Io(error.error))?;
                Ok(())
            })
            .expect_err("replacement after preparation must win without being overwritten");

        assert!(matches!(error, EditError::CommitConflict { .. }));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read replacement"),
            "external"
        );
        let entries = std::fs::read_dir(path.parent().expect("target parent"))
            .expect("read target parent")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![std::ffi::OsString::from("source.txt")]);
    }

    #[test]
    fn handle_identity_rejects_content_larger_than_the_admitted_size() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "baseline").expect("seed baseline");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("open baseline handle");
        file.set_len(1024 * 1024 * 1024)
            .expect("grow sparse file after admission");

        let error = super::file_content_identity_from_handle(&file, &path, 8)
            .expect_err("oversized handle content must be rejected before hashing it");

        assert!(matches!(error, EditError::CommitConflict { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn unix_stable_identity_normalizes_file_type_bits_to_u32() {
        let regular = super::StableFileIdentity {
            device: 1,
            inode: 2,
            file_type: super::unix_file_type((libc::S_IFREG as u32) | 0o640),
        };
        let fifo = super::StableFileIdentity {
            device: 1,
            inode: 3,
            file_type: super::unix_file_type((libc::S_IFIFO as u32) | 0o600),
        };

        assert_eq!(regular.file_type, super::UNIX_REGULAR_FILE_TYPE);
        assert!(regular.is_regular_file());
        assert!(!fifo.is_regular_file());
    }

    #[cfg(unix)]
    #[test]
    fn detached_target_validation_conflict_restores_the_original_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "baseline").expect("seed baseline");
        let (_, expected) = read_file_with_identity(&path, 1_024).expect("baseline identity");
        let guarded = guarded_path(&path);
        let replacement = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open replacement writer");
        replacement.set_len(64).expect("grow replacement");
        drop(replacement);
        let parent =
            super::StableWriteParent::open(path.parent().expect("target parent"), &guarded)
                .expect("stable parent");

        let error = match super::take_expected_target(
            &parent,
            &guarded,
            path.file_name().expect("target name"),
            &expected,
        ) {
            Ok(_) => panic!("post-detach size conflict must restore the original name"),
            Err(error) => error,
        };

        assert!(matches!(error, EditError::CommitConflict { .. }));
        assert!(path.exists());
        assert_eq!(
            std::fs::metadata(&path).expect("restored metadata").len(),
            64
        );
        let entries = std::fs::read_dir(path.parent().expect("target parent"))
            .expect("read target parent")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![std::ffi::OsString::from("source.txt")]);
    }

    #[cfg(unix)]
    #[test]
    fn detached_fifo_replacement_is_rejected_without_blocking_and_restored() {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
        use std::time::{Duration, Instant};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "baseline").expect("seed baseline");
        let (_, expected) = read_file_with_identity(&path, 1_024).expect("baseline identity");
        let guarded = guarded_path(&path);
        let parent =
            super::StableWriteParent::open(path.parent().expect("target parent"), &guarded)
                .expect("stable parent");

        std::fs::remove_file(&path).expect("remove admitted target");
        let fifo_path = std::ffi::CString::new(path.as_std_path().as_os_str().as_bytes())
            .expect("fifo path without NUL");
        // SAFETY: `fifo_path` is a live NUL-terminated pathname and the target is absent.
        let created = unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600 as libc::mode_t) };
        assert_eq!(
            created,
            0,
            "create raced FIFO: {}",
            std::io::Error::last_os_error()
        );

        let target_name = path.file_name().expect("target name").to_string();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let outcome =
                super::take_expected_target(&parent, &guarded, &target_name, &expected).map(drop);
            sender.send(outcome).expect("send mutation outcome");
        });

        let mut blocked = false;
        let outcome = match receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                blocked = true;
                let discovery_deadline = Instant::now() + Duration::from_secs(2);
                let backup_path = loop {
                    let backup = std::fs::read_dir(path.parent().expect("target parent"))
                        .expect("read target parent")
                        .filter_map(Result::ok)
                        .map(|entry| entry.path())
                        .find(|candidate| {
                            std::fs::symlink_metadata(candidate)
                                .is_ok_and(|metadata| metadata.file_type().is_fifo())
                        });
                    if let Some(backup) = backup {
                        break backup;
                    }
                    assert!(
                        Instant::now() < discovery_deadline,
                        "blocked mutation did not publish its detached recovery entry"
                    );
                    std::thread::yield_now();
                };

                let release_deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&backup_path)
                    {
                        Ok(writer) => {
                            drop(writer);
                            break;
                        }
                        Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                            assert!(
                                Instant::now() < release_deadline,
                                "blocked FIFO reader could not be released"
                            );
                            std::thread::yield_now();
                        }
                        Err(error) => panic!("release blocked FIFO reader: {error}"),
                    }
                }
                receiver
                    .recv_timeout(Duration::from_secs(2))
                    .expect("released mutation must return its typed conflict")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("mutation worker disconnected before publishing its result")
            }
        };
        worker.join().expect("join mutation worker");

        assert!(
            !blocked,
            "conditional mutation blocked while opening a raced FIFO"
        );
        assert!(matches!(outcome, Err(EditError::CommitConflict { .. })));
        assert!(
            std::fs::symlink_metadata(&path)
                .expect("restored FIFO metadata")
                .file_type()
                .is_fifo()
        );
        let entries = std::fs::read_dir(path.parent().expect("target parent"))
            .expect("read restored parent")
            .map(|entry| entry.expect("directory entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![std::ffi::OsString::from("source.txt")]);
    }

    #[cfg(unix)]
    #[test]
    fn detached_fifo_source_swap_preserves_the_foreign_entry_without_name_only_restore() {
        use std::os::unix::ffi::OsStrExt as _;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "baseline").expect("seed baseline");
        let (_, expected) = read_file_with_identity(&path, 1_024).expect("baseline identity");
        let guarded = guarded_path(&path);
        let parent =
            super::StableWriteParent::open(path.parent().expect("target parent"), &guarded)
                .expect("stable parent");

        std::fs::remove_file(&path).expect("remove admitted target");
        let fifo_path = std::ffi::CString::new(path.as_std_path().as_os_str().as_bytes())
            .expect("fifo path without NUL");
        // SAFETY: `fifo_path` is a live NUL-terminated pathname and the target is absent.
        let created = unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600 as libc::mode_t) };
        assert_eq!(
            created,
            0,
            "create raced FIFO: {}",
            std::io::Error::last_os_error()
        );
        let backup_name = ".moyai-write-backup-source-swap.tmp";
        super::unix_rename_noreplace(&parent, path.file_name().expect("target name"), backup_name)
            .expect("detach raced FIFO");
        let detached = super::open_relative_read(&parent, backup_name)
            .expect("open detached FIFO without blocking");
        let backup_path = path.parent().expect("target parent").join(backup_name);
        std::fs::remove_file(&backup_path).expect("remove detached FIFO directory entry");
        std::fs::write(&backup_path, "foreign").expect("replace private backup source");

        let error = match super::validate_detached_target(
            &parent,
            &guarded,
            path.file_name().expect("target name"),
            backup_name.to_string(),
            detached,
            &expected,
        ) {
            Ok(_) => panic!("source-swapped detached entry must not validate"),
            Err(error) => error,
        };
        let EditError::CommitConflictPreserved {
            path: error_path,
            preserved_path,
            reason,
        } = error
        else {
            panic!("source identity mismatch must retain a typed recovery path");
        };

        assert_eq!(error_path, path);
        assert_eq!(preserved_path, backup_path);
        assert!(!path.exists(), "foreign entry must not be moved to target");
        assert_eq!(
            std::fs::read_to_string(&preserved_path).expect("read preserved foreign entry"),
            "foreign"
        );
        assert!(reason.contains("changed before its conditional rename"));
        assert!(reason.len() <= super::MAX_EDIT_RECOVERY_REASON_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn detached_symlink_open_failure_preserves_the_private_path_without_name_only_restore() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        let external =
            Utf8PathBuf::from_path_buf(temp.path().join("external.txt")).expect("utf8 path");
        std::fs::write(&path, "baseline").expect("seed baseline");
        std::fs::write(&external, "external").expect("seed link target");
        let (_, expected) = read_file_with_identity(&path, 1_024).expect("baseline identity");
        let guarded = guarded_path(&path);
        let parent =
            super::StableWriteParent::open(path.parent().expect("target parent"), &guarded)
                .expect("stable parent");
        std::fs::remove_file(&path).expect("remove admitted target");
        symlink(&external, &path).expect("replace target with symlink");

        let error = match super::take_expected_target(
            &parent,
            &guarded,
            path.file_name().expect("target name"),
            &expected,
        ) {
            Ok(_) => panic!("unopenable detached entry must not validate"),
            Err(error) => error,
        };
        let EditError::CommitConflictPreserved {
            path: error_path,
            preserved_path,
            reason,
        } = error
        else {
            panic!("open failure must retain a typed recovery path");
        };

        assert_eq!(error_path, path);
        assert!(
            !path.exists(),
            "unverified entry must not be restored by name"
        );
        assert_eq!(
            std::fs::read_link(&preserved_path).expect("read preserved symlink"),
            external.as_std_path()
        );
        assert!(reason.contains("name-only restore was refused"));
        assert!(reason.len() <= super::MAX_EDIT_RECOVERY_REASON_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn failed_detached_restore_keeps_the_primary_recovery_path_and_bounded_reason() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "baseline").expect("seed baseline");
        let guarded = guarded_path(&path);
        let parent =
            super::StableWriteParent::open(path.parent().expect("target parent"), &guarded)
                .expect("stable parent");
        let backup_name = ".moyai-write-backup-test.tmp";
        super::unix_rename_noreplace(&parent, path.file_name().expect("target name"), backup_name)
            .expect("detach target");
        let file = super::open_relative_read(&parent, backup_name).expect("open detached target");
        let identity = super::stable_file_identity(&file).expect("detached identity");
        let mut backup = super::StableFileEntry {
            name: backup_name.to_string(),
            file,
            identity,
        };
        std::fs::write(&path, "external").expect("occupy restore target");

        let error = super::restore_detached_target_after_validation_failure(
            &parent,
            &guarded,
            path.file_name().expect("target name"),
            &mut backup,
            EditError::CommitConflict { path: path.clone() },
        );
        let EditError::CommitConflictPreserved {
            path: error_path,
            preserved_path,
            reason,
        } = error
        else {
            panic!("failed restore must retain the typed recovery path");
        };

        assert_eq!(error_path, path);
        assert_eq!(
            preserved_path,
            path.parent().expect("parent").join(backup_name)
        );
        assert_eq!(
            std::fs::read_to_string(&preserved_path).expect("read preserved baseline"),
            "baseline"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read external target"),
            "external"
        );
        assert!(reason.contains("commit was not applied"));
        assert!(reason.contains("restore failed"));
        assert!(reason.len() <= super::MAX_EDIT_RECOVERY_REASON_BYTES);

        let bounded = super::bounded_preserved_conflict_reason(
            &"primary".repeat(2_048),
            &"restore".repeat(2_048),
        );
        assert!(bounded.contains(super::PRESERVED_CONFLICT_RESTORE_LABEL));
        assert!(bounded.len() <= super::MAX_EDIT_RECOVERY_REASON_BYTES);
    }

    #[cfg(windows)]
    #[test]
    fn conditional_replace_commits_when_the_expected_object_is_current() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "baseline").expect("seed baseline");
        let (_, expected) = read_file_with_identity(&path, 1_024).expect("baseline identity");
        let guarded = guarded_path(&path);

        let committed =
            super::write_text_file_conditionally(&guarded, "agent", Some(&expected), |_| Ok(()))
                .expect("conditional replacement");

        let (bytes, actual) = read_file_with_identity(&path, 1_024).expect("committed identity");
        assert_eq!(bytes, b"agent");
        assert_eq!(committed, actual);
    }

    #[cfg(unix)]
    #[test]
    fn conditional_replace_preserves_late_write_through_preopened_descriptor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "baseline").expect("seed baseline");
        let (_, expected) = read_file_with_identity(&path, 1_024).expect("baseline identity");
        let guarded = guarded_path(&path);
        let mut external_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("preopen independent writer");

        let error =
            super::write_text_file_conditionally(&guarded, "agent", Some(&expected), |_| Ok(()))
                .expect_err("Unix must not claim safe retirement of the detached target");
        let EditError::PartialCommit {
            path: error_path,
            preserved_path,
            reason,
        } = error
        else {
            panic!("expected a typed partial commit");
        };

        assert_eq!(error_path, path);
        assert!(reason.contains("previously opened writable file descriptor"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read installed target"),
            "agent"
        );
        assert_eq!(
            std::fs::read_to_string(&preserved_path).expect("read detached baseline"),
            "baseline"
        );

        external_writer
            .set_len(0)
            .expect("truncate detached target");
        external_writer
            .seek(SeekFrom::Start(0))
            .expect("seek detached target");
        external_writer
            .write_all(b"external-after-detach")
            .expect("late external write");
        external_writer.flush().expect("flush late external write");
        drop(external_writer);

        assert_ne!(preserved_path, path);
        assert_eq!(
            std::fs::read_to_string(&preserved_path).expect("read preserved detached target"),
            "external-after-detach"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("reread installed target"),
            "agent"
        );
    }

    #[cfg(unix)]
    #[test]
    fn conditional_delete_preserves_late_write_through_preopened_descriptor() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "baseline").expect("seed baseline");
        let (_, expected) = read_file_with_identity(&path, 1_024).expect("baseline identity");
        let guarded = guarded_path(&path);
        let mut external_writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("preopen independent writer");

        let error = super::delete_file_conditionally(&guarded, &expected)
            .expect_err("Unix must not claim safe retirement of the detached target");
        let EditError::PartialCommit {
            path: error_path,
            preserved_path,
            reason,
        } = error
        else {
            panic!("expected a typed partial commit");
        };

        assert_eq!(error_path, path);
        assert!(reason.contains("previously opened writable file descriptor"));
        assert!(!path.exists());
        assert_eq!(
            std::fs::read_to_string(&preserved_path).expect("read detached baseline"),
            "baseline"
        );

        external_writer
            .set_len(0)
            .expect("truncate detached target");
        external_writer
            .seek(SeekFrom::Start(0))
            .expect("seek detached target");
        external_writer
            .write_all(b"external-after-detach")
            .expect("late external write");
        external_writer.flush().expect("flush late external write");
        drop(external_writer);

        assert_ne!(preserved_path, path);
        assert_eq!(
            std::fs::read_to_string(&preserved_path).expect("read preserved detached target"),
            "external-after-detach"
        );
        assert!(!path.exists());
    }

    #[test]
    fn write_does_not_create_a_missing_parent_before_stable_admission() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let path = root.join("missing/child.txt");
        let workspace = WorkspaceDiscovery::discover_fixed_root(
            &root,
            &crate::config::ResolvedConfig::default(),
        )
        .expect("test workspace");
        let guarded =
            PathGuard::require_path(&workspace, &path, AccessKind::Edit).expect("guard target");

        let error = super::write_text_file_conditionally(&guarded, "agent", None, |_| Ok(()))
            .expect_err("missing parent must fail closed");

        assert!(matches!(error, EditError::MissingParent { .. }));
        assert!(!root.join("missing").exists());
    }

    #[cfg(windows)]
    #[test]
    fn staged_create_remains_relative_to_pinned_parent_after_ancestor_link_retarget() {
        use std::os::windows::fs::symlink_dir;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let original_root = root.join("original");
        let replacement_root = root.join("replacement");
        let original_parent = original_root.join("parent");
        let replacement_parent = replacement_root.join("parent");
        let link = root.join("current");
        std::fs::create_dir_all(&original_parent).expect("create original parent");
        std::fs::create_dir_all(&replacement_parent).expect("create replacement parent");
        match symlink_dir(original_root.as_std_path(), link.as_std_path()) {
            Ok(()) => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(1314) =>
            {
                return;
            }
            Err(error) => panic!("create original directory link: {error}"),
        }

        let target = link.join("parent/target.txt");
        let workspace = WorkspaceDiscovery::discover_fixed_root(
            &root,
            &crate::config::ResolvedConfig::default(),
        )
        .expect("test workspace");
        let guarded = PathGuard::require_path(&workspace, &target, AccessKind::Edit)
            .expect("guard target through original link");
        let stable_parent =
            super::StableWriteParent::open(target.parent().expect("target parent"), &guarded)
                .expect("pin original parent");

        std::fs::remove_dir(&link).expect("remove original directory link");
        symlink_dir(replacement_root.as_std_path(), link.as_std_path())
            .expect("retarget directory link");

        let mut staged = stable_parent
            .create_entry("retarget-test", &target)
            .expect("create relative staged entry");
        staged.file.write_all(b"agent").expect("write staged entry");
        staged.file.flush().expect("flush staged entry");

        assert_eq!(
            std::fs::read(original_parent.join(&staged.name)).expect("read original parent entry"),
            b"agent"
        );
        assert!(!replacement_parent.join(&staged.name).exists());
        stable_parent
            .delete_entry_if_same(&staged)
            .expect("delete staged entry through its handle");
    }

    #[cfg(windows)]
    #[test]
    fn pinned_windows_target_rejects_external_write_and_replace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "baseline").expect("seed baseline");
        let pinned = super::open_windows_mutation_file(path.as_std_path()).expect("pin target");

        std::fs::write(&path, "external-write")
            .expect_err("pinned target must deny a new external writer");
        let mut replacement =
            tempfile::NamedTempFile::new_in(path.parent().expect("target parent"))
                .expect("replacement temp");
        replacement
            .write_all(b"external-replacement")
            .expect("replacement content");
        replacement.flush().expect("flush replacement");
        replacement
            .persist(&path)
            .expect_err("pinned target must deny external replacement");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read pinned target"),
            "baseline"
        );
        drop(pinned);
    }
}
