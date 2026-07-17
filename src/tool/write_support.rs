use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::UNIX_EPOCH;

use camino::{Utf8Path, Utf8PathBuf};

use crate::edit::{
    ChangeSummary, FileChange, FileContentIdentity, FileReadStamp, read_file_with_identity,
};
use crate::error::EditError;
use crate::runtime::SystemClock;
use crate::workspace::{GuardedPath, PathGuard};

const MAX_UNIQUE_WRITE_NAMES: usize = 16;

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
    let mut staged = stable_parent.create_entry("stage")?;
    if let Err(error) = validate_temporary_file(&staged.file) {
        let _ = stable_parent.delete_entry_if_same(&staged);
        return Err(error);
    }
    let prepared = (|| {
        staged.file.write_all(text.as_bytes())?;
        staged.file.flush()?;
        written_text_identity(&staged.file, text)
    })();
    let committed_identity = match prepared {
        Ok(identity) => identity,
        Err(error) => {
            let _ = stable_parent.delete_entry_if_same(&staged);
            return Err(error);
        }
    };

    let Some(expected_identity) = expected_identity else {
        if let Err(error) = stable_parent.move_entry_noclobber(&mut staged, target_name) {
            let _ = stable_parent.delete_entry_if_same(&staged);
            return if is_noclobber_conflict(&error) {
                Err(EditError::CommitConflict {
                    path: path.to_path_buf(),
                })
            } else {
                Err(EditError::Io(error))
            };
        }
        return Ok(committed_identity);
    };

    let mut backup =
        match stable_parent.take_expected_target(guarded, target_name, expected_identity) {
            Ok(backup) => backup,
            Err(error) => {
                let _ = stable_parent.delete_entry_if_same(&staged);
                return Err(error);
            }
        };
    if let Err(error) = stable_parent.move_entry_noclobber(&mut staged, target_name) {
        let _ = stable_parent.delete_entry_if_same(&staged);
        return match stable_parent.move_entry_noclobber(&mut backup, target_name) {
            Ok(()) if is_noclobber_conflict(&error) => Err(EditError::CommitConflict {
                path: path.to_path_buf(),
            }),
            Ok(()) => Err(EditError::Io(error)),
            Err(restore_error) => Err(EditError::CommitConflictPreserved {
                path: path.to_path_buf(),
                preserved_path: stable_parent.display_entry(&backup),
                reason: restore_error.to_string(),
            }),
        };
    }

    if let Err(error) = stable_parent.delete_entry_if_same(&backup) {
        return Err(EditError::PartialCommit {
            path: path.to_path_buf(),
            preserved_path: stable_parent.display_entry(&backup),
            reason: error.to_string(),
        });
    }
    Ok(committed_identity)
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
        .delete_entry_if_same(&backup)
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
    #[cfg(not(any(unix, windows)))]
    unsupported: (),
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

    fn create_entry(&self, purpose: &str) -> Result<StableFileEntry, EditError> {
        for _ in 0..MAX_UNIQUE_WRITE_NAMES {
            let name = unique_write_name(purpose);
            match create_relative_file(self, &name) {
                Ok(file) => {
                    let identity = stable_file_identity(&file)?;
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
    let current = file_content_identity_from_handle(&file, &guarded.absolute)?;
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
            return match unix_rename_noreplace(parent, &backup_name, target_name) {
                Ok(()) => Err(EditError::Message(format!(
                    "path `{}` was not a regular no-follow file at commit time; it was restored and not overwritten: {error}",
                    guarded.absolute
                ))),
                Err(restore_error) => Err(commit_conflict(
                    &guarded.absolute,
                    Some((parent.path.join(&backup_name), restore_error)),
                )),
            };
        }
    };
    let mut backup = StableFileEntry {
        identity: stable_file_identity(&file)?,
        name: backup_name,
        file,
    };
    let current = file_content_identity_from_handle(&backup.file, &guarded.absolute)?;
    if &current != expected_identity {
        return match parent.move_entry_noclobber(&mut backup, target_name) {
            Ok(()) => Err(commit_conflict(&guarded.absolute, None)),
            Err(restore_error) => Err(commit_conflict(
                &guarded.absolute,
                Some((parent.display_entry(&backup), restore_error)),
            )),
        };
    }
    Ok(backup)
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
) -> Result<FileContentIdentity, EditError> {
    use sha2::{Digest as _, Sha256};

    let before = file.metadata()?;
    if !before.is_file() {
        return Err(EditError::Message(
            "conditional file mutation requires a regular file".to_string(),
        ));
    }
    let before_mtime_ms = metadata_mtime_ms(&before);
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
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
        digest.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    if bytes_read != before.len()
        || after.len() != before.len()
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
fn create_relative_file(parent: &StableWriteParent, name: &str) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        FILE_SHARE_READ,
    };

    fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(parent.path.join(name))
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
    let descriptor = unsafe {
        libc::openat(
            parent.file.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
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
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "conditional file mutation requires a regular file",
        ));
    }
    Ok(StableFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
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
    if status.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "private file entry became non-regular",
        ));
    }
    Ok(Some(StableFileIdentity {
        device: status.st_dev as u64,
        inode: status.st_ino as u64,
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
    path: &Utf8Path,
) -> Result<(String, FileContentIdentity), EditError> {
    let (bytes, identity) = read_file_with_identity(path)?;
    let text = String::from_utf8(bytes).map_err(|error| {
        EditError::Message(format!("path `{path}` is not valid UTF-8 text: {error}"))
    })?;
    Ok((text, identity))
}

pub(crate) fn build_read_stamp(path: &Utf8Path) -> Result<FileReadStamp, EditError> {
    let (_, identity) = read_file_with_identity(path)?;
    Ok(FileReadStamp {
        path: path.to_path_buf(),
        read_at_ms: SystemClock::now_ms(),
        mtime_ms: identity.mtime_ms,
        size_bytes: Some(identity.size_bytes),
        content_sha256: Some(identity.content_sha256),
    })
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
        let (_, expected) = read_file_with_identity(&path).expect("baseline identity");
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
    fn conditional_replace_commits_when_the_expected_object_is_current() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "baseline").expect("seed baseline");
        let (_, expected) = read_file_with_identity(&path).expect("baseline identity");
        let guarded = guarded_path(&path);

        let committed =
            super::write_text_file_conditionally(&guarded, "agent", Some(&expected), |_| Ok(()))
                .expect("conditional replacement");

        let (bytes, actual) = read_file_with_identity(&path).expect("committed identity");
        assert_eq!(bytes, b"agent");
        assert_eq!(committed, actual);
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
