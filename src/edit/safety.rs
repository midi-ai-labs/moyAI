use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::io::Read as _;
use std::sync::{Arc, Mutex, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::EditError;
use crate::session::SessionId;

const MAX_EDIT_READ_BASELINES: usize = 4_096;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileReadStamp {
    pub path: Utf8PathBuf,
    pub read_at_ms: i64,
    pub mtime_ms: Option<i64>,
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub content_sha256: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct FileContentIdentity {
    pub mtime_ms: Option<i64>,
    pub size_bytes: u64,
    pub content_sha256: String,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum CommittedFileMutation {
    Present {
        path: Utf8PathBuf,
        identity: FileContentIdentity,
    },
    Absent {
        path: Utf8PathBuf,
    },
}

impl CommittedFileMutation {
    pub(crate) fn present(path: Utf8PathBuf, identity: FileContentIdentity) -> Self {
        Self::Present { path, identity }
    }

    pub(crate) fn absent(path: Utf8PathBuf) -> Self {
        Self::Absent { path }
    }
}

#[derive(Debug, Clone)]
struct CachedReadStamp {
    stamp: FileReadStamp,
    last_access: u64,
}

#[derive(Debug, Default)]
struct ReadStampCache {
    entries: HashMap<(SessionId, Utf8PathBuf), CachedReadStamp>,
    access_clock: u64,
}

impl ReadStampCache {
    fn next_access(&mut self) -> u64 {
        self.access_clock = self.access_clock.saturating_add(1);
        self.access_clock
    }

    fn insert(&mut self, session_id: SessionId, stamp: FileReadStamp) {
        let key = (session_id, stamp.path.clone());
        let last_access = self.next_access();
        if !self.entries.contains_key(&key) && self.entries.len() >= MAX_EDIT_READ_BASELINES {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_access)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.entries
            .insert(key, CachedReadStamp { stamp, last_access });
    }

    fn get(&mut self, session_id: SessionId, path: &Utf8Path) -> Option<FileReadStamp> {
        let key = (session_id, path.to_path_buf());
        let last_access = self.next_access();
        let entry = self.entries.get_mut(&key)?;
        entry.last_access = last_access;
        Some(entry.stamp.clone())
    }

    fn remove(&mut self, session_id: SessionId, path: &Utf8Path) {
        self.entries.remove(&(session_id, path.to_path_buf()));
    }
}

#[derive(Clone, Default)]
pub struct EditSafety {
    read_stamps: Arc<Mutex<ReadStampCache>>,
    file_locks: Arc<Mutex<HashMap<Utf8PathBuf, Weak<tokio::sync::Mutex<()>>>>>,
}

struct FileLockRegistryCleanup {
    registry: Arc<Mutex<HashMap<Utf8PathBuf, Weak<tokio::sync::Mutex<()>>>>>,
    paths: Vec<Utf8PathBuf>,
}

impl Drop for FileLockRegistryCleanup {
    fn drop(&mut self) {
        let Ok(mut registry) = self.registry.lock() else {
            return;
        };
        for path in &self.paths {
            if registry
                .get(path)
                .is_some_and(|lock| lock.strong_count() == 0)
            {
                registry.remove(path);
            }
        }
    }
}

impl EditSafety {
    pub fn record_read(
        &self,
        session_id: SessionId,
        stamp: FileReadStamp,
    ) -> Result<(), EditError> {
        let mut store = self.read_stamps.lock().expect("edit safety mutex poisoned");
        store.insert(session_id, stamp);
        Ok(())
    }

    pub fn record_current_file_state(
        &self,
        session_id: SessionId,
        path: &Utf8Path,
        maximum_bytes: u64,
    ) -> Result<(), EditError> {
        let identity = read_file_identity_bounded(path, maximum_bytes)?;
        self.record_read(
            session_id,
            FileReadStamp {
                path: path.to_path_buf(),
                read_at_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|value| value.as_millis() as i64)
                    .unwrap_or_default(),
                mtime_ms: identity.mtime_ms,
                size_bytes: Some(identity.size_bytes),
                content_sha256: Some(identity.content_sha256),
            },
        )
    }

    pub(crate) fn sync_file_mutations(
        &self,
        session_id: SessionId,
        mutations: &[CommittedFileMutation],
        maximum_bytes: u64,
    ) -> Result<(), EditError> {
        let read_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_millis() as i64)
            .unwrap_or_default();
        let mut verified = Vec::with_capacity(mutations.len());
        for mutation in mutations {
            verified.push(verify_committed_file_mutation(
                mutation,
                maximum_bytes,
                read_at_ms,
            )?);
        }

        let mut store = self.read_stamps.lock().expect("edit safety mutex poisoned");
        for (path, stamp) in verified {
            match stamp {
                Some(stamp) => store.insert(session_id, stamp),
                None => store.remove(session_id, &path),
            }
        }
        Ok(())
    }

    pub fn snapshot_path_stamps(
        &self,
        session_id: SessionId,
        paths: &[Utf8PathBuf],
    ) -> Vec<(Utf8PathBuf, Option<FileReadStamp>)> {
        let mut unique_paths = paths.to_vec();
        unique_paths.sort();
        unique_paths.dedup();
        let mut store = self.read_stamps.lock().expect("edit safety mutex poisoned");
        unique_paths
            .into_iter()
            .map(|path| {
                let stamp = store.get(session_id, &path);
                (path, stamp)
            })
            .collect()
    }

    pub fn restore_path_stamps(
        &self,
        session_id: SessionId,
        snapshot: &[(Utf8PathBuf, Option<FileReadStamp>)],
    ) -> Result<(), EditError> {
        let mut store = self.read_stamps.lock().expect("edit safety mutex poisoned");
        for (path, stamp) in snapshot {
            if let Some(stamp) = stamp {
                store.insert(session_id, stamp.clone());
            } else {
                store.remove(session_id, path);
            }
        }
        Ok(())
    }

    pub fn get_stamp(&self, session_id: SessionId, path: &Utf8Path) -> Option<FileReadStamp> {
        self.read_stamps
            .lock()
            .expect("edit safety mutex poisoned")
            .get(session_id, path)
    }

    pub fn assert_fresh_write(
        &self,
        session_id: SessionId,
        path: &Utf8Path,
        current: &FileContentIdentity,
    ) -> Result<(), EditError> {
        let stamp = self.get_stamp(session_id, path).ok_or_else(|| {
            EditError::Message(format!(
                "no edit baseline exists for path `{path}` in this session"
            ))
        })?;
        if stamp.mtime_ms != current.mtime_ms
            || stamp.size_bytes != Some(current.size_bytes)
            || stamp.content_sha256.as_deref() != Some(current.content_sha256.as_str())
        {
            return Err(EditError::Message(format!(
                "the edit baseline for path `{path}` does not match its current contents"
            )));
        }
        Ok(())
    }

    pub fn assert_path_unchanged(
        &self,
        path: &Utf8Path,
        expected: Option<&FileContentIdentity>,
    ) -> Result<(), EditError> {
        match expected {
            Some(expected) => {
                let metadata = fs::metadata(path).map_err(|error| {
                    EditError::Message(format!(
                        "path `{path}` could not be revalidated before commit: {error}"
                    ))
                })?;
                if metadata.len() != expected.size_bytes {
                    return Err(path_changed_before_commit(path));
                }
                let current =
                    read_file_identity_bounded(path, expected.size_bytes).map_err(|error| {
                        EditError::Message(format!(
                            "path `{path}` could not be revalidated before commit: {error}"
                        ))
                    })?;
                if &current != expected {
                    return Err(path_changed_before_commit(path));
                }
            }
            None => match fs::symlink_metadata(path) {
                Ok(_) => {
                    return Err(EditError::Message(format!(
                        "path `{path}` was created while the edit was being prepared; the commit was not applied"
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(EditError::Io(error)),
            },
        }
        Ok(())
    }

    pub async fn with_file_lock<T, E, F>(&self, path: &Utf8Path, op: F) -> Result<T, E>
    where
        F: Future<Output = Result<T, E>>,
    {
        let _cleanup = FileLockRegistryCleanup {
            registry: Arc::clone(&self.file_locks),
            paths: vec![path.to_path_buf()],
        };
        let lock = {
            let mut locks = self.file_locks.lock().expect("edit safety mutex poisoned");
            match locks.get(path).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(tokio::sync::Mutex::new(()));
                    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        let _guard = lock.lock().await;
        op.await
    }

    pub async fn with_file_locks<T, E, F>(&self, paths: &[Utf8PathBuf], op: F) -> Result<T, E>
    where
        F: Future<Output = Result<T, E>>,
    {
        let mut ordered_paths = paths.to_vec();
        ordered_paths.sort();
        ordered_paths.dedup();
        let _cleanup = FileLockRegistryCleanup {
            registry: Arc::clone(&self.file_locks),
            paths: ordered_paths.clone(),
        };
        let locks = {
            let mut store = self.file_locks.lock().expect("edit safety mutex poisoned");
            ordered_paths
                .iter()
                .map(|path| match store.get(path).and_then(Weak::upgrade) {
                    Some(lock) => lock,
                    None => {
                        let lock = Arc::new(tokio::sync::Mutex::new(()));
                        store.insert(path.clone(), Arc::downgrade(&lock));
                        lock
                    }
                })
                .collect::<Vec<_>>()
        };
        let mut guards = Vec::with_capacity(locks.len());
        for lock in &locks {
            guards.push(lock.lock().await);
        }
        op.await
    }

    pub fn invalidate_paths(
        &self,
        session_id: SessionId,
        paths: &[Utf8PathBuf],
    ) -> Result<(), EditError> {
        let mut store = self.read_stamps.lock().expect("edit safety mutex poisoned");
        for path in paths {
            store.remove(session_id, path);
        }
        Ok(())
    }

    pub fn invalidate_roots(
        &self,
        session_id: SessionId,
        roots: &[Utf8PathBuf],
    ) -> Result<(), EditError> {
        let mut store = self.read_stamps.lock().expect("edit safety mutex poisoned");
        store.entries.retain(|(entry_session_id, path), _| {
            *entry_session_id != session_id || !roots.iter().any(|root| path.starts_with(root))
        });
        Ok(())
    }

    pub fn invalidate_session(&self, session_id: SessionId) -> Result<(), EditError> {
        let mut store = self.read_stamps.lock().expect("edit safety mutex poisoned");
        store
            .entries
            .retain(|(entry_session_id, _), _| *entry_session_id != session_id);
        Ok(())
    }
}

fn verify_committed_file_mutation(
    mutation: &CommittedFileMutation,
    maximum_bytes: u64,
    read_at_ms: i64,
) -> Result<(Utf8PathBuf, Option<FileReadStamp>), EditError> {
    match mutation {
        CommittedFileMutation::Present { path, identity } => {
            ensure_edit_read_limit(path, identity.size_bytes, maximum_bytes)?;
            let current = match read_file_identity_bounded(path, maximum_bytes) {
                Ok(current) => current,
                Err(EditError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(EditError::CommitConflict { path: path.clone() });
                }
                Err(EditError::Message(_)) => {
                    return Err(EditError::CommitConflict { path: path.clone() });
                }
                Err(error) => return Err(error),
            };
            if &current != identity {
                return Err(EditError::CommitConflict { path: path.clone() });
            }
            Ok((
                path.clone(),
                Some(FileReadStamp {
                    path: path.clone(),
                    read_at_ms,
                    mtime_ms: identity.mtime_ms,
                    size_bytes: Some(identity.size_bytes),
                    content_sha256: Some(identity.content_sha256.clone()),
                }),
            ))
        }
        CommittedFileMutation::Absent { path } => match fs::symlink_metadata(path) {
            Ok(_) => Err(EditError::CommitConflict { path: path.clone() }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((path.clone(), None)),
            Err(error) => Err(EditError::Io(error)),
        },
    }
}

pub fn read_file_with_identity(
    path: &Utf8Path,
    maximum_bytes: u64,
) -> Result<(Vec<u8>, FileContentIdentity), EditError> {
    let mut file = open_file_for_bounded_identity_read(path)?;
    let before = checked_file_metadata(path, &file, maximum_bytes)?;
    let capacity = usize::try_from(before.len()).unwrap_or_default();
    let mut bytes = Vec::with_capacity(capacity);
    file.by_ref()
        .take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    ensure_edit_read_limit(path, bytes.len() as u64, maximum_bytes)?;
    let after = file.metadata()?;
    ensure_stable_file_read(path, &before, &after, bytes.len() as u64)?;
    let identity = identity_from_metadata_and_hash(&after, format!("{:x}", Sha256::digest(&bytes)));
    Ok((bytes, identity))
}

fn read_file_identity_bounded(
    path: &Utf8Path,
    maximum_bytes: u64,
) -> Result<FileContentIdentity, EditError> {
    let mut file = open_file_for_bounded_identity_read(path)?;
    let before = checked_file_metadata(path, &file, maximum_bytes)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.saturating_add(read as u64);
        ensure_edit_read_limit(path, total, maximum_bytes)?;
        hasher.update(&buffer[..read]);
    }
    let after = file.metadata()?;
    ensure_stable_file_read(path, &before, &after, total)?;
    Ok(identity_from_metadata_and_hash(
        &after,
        format!("{:x}", hasher.finalize()),
    ))
}

#[cfg(unix)]
fn open_file_for_bounded_identity_read(path: &Utf8Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    // A raced FIFO or other non-regular entry must not block before the metadata check on this
    // same handle rejects it. O_NONBLOCK has no effect on ordinary regular-file reads.
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}

#[cfg(not(unix))]
fn open_file_for_bounded_identity_read(path: &Utf8Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

fn checked_file_metadata(
    path: &Utf8Path,
    file: &fs::File,
    maximum_bytes: u64,
) -> Result<fs::Metadata, EditError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(EditError::Message(format!(
            "path `{path}` is not a regular file"
        )));
    }
    ensure_edit_read_limit(path, metadata.len(), maximum_bytes)?;
    Ok(metadata)
}

pub(crate) fn ensure_edit_read_limit(
    path: &Utf8Path,
    actual: u64,
    maximum: u64,
) -> Result<(), EditError> {
    if actual > maximum {
        return Err(EditError::Message(format!(
            "path `{path}` is {actual} bytes, exceeding the configured edit read limit of {maximum} bytes"
        )));
    }
    Ok(())
}

fn ensure_stable_file_read(
    path: &Utf8Path,
    before: &fs::Metadata,
    after: &fs::Metadata,
    bytes_read: u64,
) -> Result<(), EditError> {
    if before.len() != after.len()
        || after.len() != bytes_read
        || before.modified().ok() != after.modified().ok()
    {
        return Err(EditError::Message(format!(
            "path `{path}` changed while its contents were being read"
        )));
    }
    Ok(())
}

fn identity_from_metadata_and_hash(
    metadata: &fs::Metadata,
    content_sha256: String,
) -> FileContentIdentity {
    FileContentIdentity {
        mtime_ms: metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_millis() as i64),
        size_bytes: metadata.len(),
        content_sha256,
    }
}

fn path_changed_before_commit(path: &Utf8Path) -> EditError {
    EditError::Message(format!(
        "path `{path}` changed while the edit was being prepared; the commit was not applied"
    ))
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use crate::error::EditError;

    use super::{
        CommittedFileMutation, EditSafety, FileContentIdentity, FileReadStamp,
        MAX_EDIT_READ_BASELINES, read_file_with_identity,
    };

    #[test]
    fn commit_revalidation_rejects_external_same_size_change() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "alpha").expect("seed file");
        let (_, expected) = read_file_with_identity(&path, 1_024).expect("capture identity");

        std::fs::write(&path, "bravo").expect("external rewrite");

        let error = EditSafety::default()
            .assert_path_unchanged(&path, Some(&expected))
            .expect_err("same-size external rewrite must be rejected");
        assert!(error.to_string().contains("commit was not applied"));
        assert_eq!(std::fs::read_to_string(&path).expect("read file"), "bravo");
    }

    #[cfg(unix)]
    #[test]
    fn zero_length_baseline_fifo_replacement_is_rejected_without_blocking() {
        use std::os::unix::ffi::OsStrExt as _;
        use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
        use std::time::{Duration, Instant};

        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::File::create(&path).expect("seed empty baseline");
        let (_, expected) = read_file_with_identity(&path, 1_024).expect("capture empty identity");
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

        let worker_path = path.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let outcome =
                EditSafety::default().assert_path_unchanged(&worker_path, Some(&expected));
            sender.send(outcome).expect("send revalidation outcome");
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
                                "blocked FIFO reader could not be released"
                            );
                            std::thread::yield_now();
                        }
                        Err(error) => panic!("release blocked FIFO reader: {error}"),
                    }
                }
                receiver
                    .recv_timeout(Duration::from_secs(2))
                    .expect("released revalidation must return its typed conflict")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("revalidation worker disconnected before publishing its result")
            }
        };
        worker.join().expect("join revalidation worker");

        assert!(!blocked, "edit safety blocked while opening a raced FIFO");
        let error = outcome.expect_err("FIFO replacement must fail revalidation");
        assert!(matches!(&error, EditError::Message(_)));
        assert!(
            error
                .to_string()
                .contains("could not be revalidated before commit")
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .expect("FIFO metadata")
                .file_type()
                .is_fifo()
        );
    }

    #[test]
    fn configured_limit_rejects_sparse_file_before_materializing_contents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("large.txt")).expect("utf8 path");
        let file = std::fs::File::create(&path).expect("create sparse file");
        file.set_len(1024 * 1024 * 1024).expect("set sparse length");

        let error = read_file_with_identity(&path, 8)
            .expect_err("metadata over the configured limit must fail before allocation");

        assert!(
            error
                .to_string()
                .contains("configured edit read limit of 8 bytes")
        );
    }

    #[test]
    fn post_commit_baseline_sync_respects_the_configured_read_limit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("large.txt")).expect("utf8 path");
        let file = std::fs::File::create(&path).expect("create sparse file");
        file.set_len(1024 * 1024 * 1024).expect("set sparse length");
        let session_id = crate::session::SessionId::new();
        let safety = EditSafety::default();

        let error = safety
            .sync_file_mutations(
                session_id,
                &[CommittedFileMutation::present(
                    path.clone(),
                    FileContentIdentity {
                        mtime_ms: None,
                        size_bytes: 1024 * 1024 * 1024,
                        content_sha256: "unread-large-file".to_string(),
                    },
                )],
                8,
            )
            .expect_err("baseline sync must not bypass the configured read limit");

        assert!(
            error
                .to_string()
                .contains("configured edit read limit of 8 bytes")
        );
        assert!(safety.get_stamp(session_id, &path).is_none());
    }

    #[test]
    fn post_commit_sync_is_all_or_nothing_and_never_adopts_external_replacement() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = Utf8PathBuf::from_path_buf(temp.path().join("first.txt")).expect("utf8 path");
        let second = Utf8PathBuf::from_path_buf(temp.path().join("second.txt")).expect("utf8 path");
        std::fs::write(&first, "first-before").expect("seed first");
        std::fs::write(&second, "second-before").expect("seed second");
        let session_id = crate::session::SessionId::new();
        let safety = EditSafety::default();
        for path in [&first, &second] {
            safety
                .record_current_file_state(session_id, path, 1_024)
                .expect("record original baseline");
        }
        let first_baseline = safety
            .get_stamp(session_id, &first)
            .expect("first baseline");
        let second_baseline = safety
            .get_stamp(session_id, &second)
            .expect("second baseline");

        std::fs::write(&first, "first-agent").expect("commit first");
        std::fs::write(&second, "second-agent").expect("commit second");
        let (_, first_committed) =
            read_file_with_identity(&first, 1_024).expect("first committed identity");
        let (_, second_committed) =
            read_file_with_identity(&second, 1_024).expect("second committed identity");
        std::fs::write(&second, "external-after-commit").expect("external replacement");

        let error = safety
            .sync_file_mutations(
                session_id,
                &[
                    CommittedFileMutation::present(first.clone(), first_committed),
                    CommittedFileMutation::present(second.clone(), second_committed),
                ],
                1_024,
            )
            .expect_err("external replacement must reject the whole baseline sync");

        assert!(matches!(
            error,
            EditError::CommitConflict { path } if path == second
        ));
        assert_eq!(safety.get_stamp(session_id, &first), Some(first_baseline));
        assert_eq!(safety.get_stamp(session_id, &second), Some(second_baseline));
    }

    #[test]
    fn post_commit_sync_rejects_missing_present_and_recreated_absent_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let present =
            Utf8PathBuf::from_path_buf(temp.path().join("present.txt")).expect("utf8 path");
        let absent = Utf8PathBuf::from_path_buf(temp.path().join("absent.txt")).expect("utf8 path");
        let session_id = crate::session::SessionId::new();
        let safety = EditSafety::default();

        std::fs::write(&present, "agent").expect("commit present path");
        let (_, committed) = read_file_with_identity(&present, 1_024).expect("committed identity");
        std::fs::remove_file(&present).expect("external delete");
        let missing = safety
            .sync_file_mutations(
                session_id,
                &[CommittedFileMutation::present(present.clone(), committed)],
                1_024,
            )
            .expect_err("missing committed path must fail closed");
        assert!(matches!(
            missing,
            EditError::CommitConflict { path } if path == present
        ));

        std::fs::write(&absent, "external recreation").expect("external recreation");
        let recreated = safety
            .sync_file_mutations(
                session_id,
                &[CommittedFileMutation::absent(absent.clone())],
                1_024,
            )
            .expect_err("recreated deleted path must fail closed");
        assert!(matches!(
            recreated,
            EditError::CommitConflict { path } if path == absent
        ));
        assert!(safety.get_stamp(session_id, &present).is_none());
        assert!(safety.get_stamp(session_id, &absent).is_none());
    }

    #[test]
    fn commit_revalidation_rejects_huge_replacement_by_expected_size() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "small").expect("seed file");
        let (_, expected) = read_file_with_identity(&path, 1_024).expect("capture identity");
        let replacement = std::fs::File::create(&path).expect("replace file");
        replacement
            .set_len(1024 * 1024 * 1024)
            .expect("set sparse replacement length");

        let error = EditSafety::default()
            .assert_path_unchanged(&path, Some(&expected))
            .expect_err("size-mismatched replacement must be rejected before hashing");

        assert!(error.to_string().contains("commit was not applied"));
    }

    #[test]
    fn commit_revalidation_rejects_new_external_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("new.txt")).expect("utf8 path");
        std::fs::write(&path, "external").expect("external create");

        let error = EditSafety::default()
            .assert_path_unchanged(&path, None)
            .expect_err("external creation must be rejected");
        assert!(error.to_string().contains("was created while"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read file"),
            "external"
        );
    }

    #[test]
    fn invalidating_snapshot_roots_removes_only_affected_baselines() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().join("scope")).expect("utf8 root");
        let outside =
            Utf8PathBuf::from_path_buf(temp.path().join("outside.txt")).expect("utf8 outside");
        std::fs::create_dir_all(&root).expect("create scope");
        let inside = root.join("inside.txt");
        std::fs::write(&inside, "inside").expect("write inside");
        std::fs::write(&outside, "outside").expect("write outside");
        let session_id = crate::session::SessionId::new();
        let safety = EditSafety::default();
        safety
            .record_current_file_state(session_id, &inside, 1_024)
            .expect("record inside");
        safety
            .record_current_file_state(session_id, &outside, 1_024)
            .expect("record outside");

        safety
            .invalidate_roots(session_id, std::slice::from_ref(&root))
            .expect("invalidate scope");

        assert!(safety.get_stamp(session_id, &inside).is_none());
        assert!(safety.get_stamp(session_id, &outside).is_some());
    }

    #[test]
    fn invalidating_a_session_removes_every_edit_baseline() {
        let temp = tempfile::tempdir().expect("tempdir");
        let first = Utf8PathBuf::from_path_buf(temp.path().join("first.txt")).expect("utf8 path");
        let second = Utf8PathBuf::from_path_buf(temp.path().join("second.txt")).expect("utf8 path");
        std::fs::write(&first, "first").expect("write first");
        std::fs::write(&second, "second").expect("write second");
        let session_id = crate::session::SessionId::new();
        let other_session_id = crate::session::SessionId::new();
        let safety = EditSafety::default();
        for path in [&first, &second] {
            safety
                .record_current_file_state(session_id, path, 1_024)
                .expect("record session baseline");
        }
        safety
            .record_current_file_state(other_session_id, &first, 1_024)
            .expect("record other baseline");

        safety
            .invalidate_session(session_id)
            .expect("invalidate session");

        assert!(safety.get_stamp(session_id, &first).is_none());
        assert!(safety.get_stamp(session_id, &second).is_none());
        assert!(safety.get_stamp(other_session_id, &first).is_some());
    }

    #[test]
    fn read_baseline_cache_is_globally_bounded_and_evicts_least_recently_used() {
        let session_id = crate::session::SessionId::new();
        let safety = EditSafety::default();
        for index in 0..MAX_EDIT_READ_BASELINES {
            safety
                .record_read(
                    session_id,
                    FileReadStamp {
                        path: Utf8PathBuf::from(format!("generated/{index}.txt")),
                        read_at_ms: index as i64,
                        mtime_ms: Some(index as i64),
                        size_bytes: Some(index as u64),
                        content_sha256: Some(format!("hash-{index}")),
                    },
                )
                .expect("record baseline");
        }
        let retained = Utf8PathBuf::from("generated/0.txt");
        assert!(safety.get_stamp(session_id, &retained).is_some());

        safety
            .record_read(
                crate::session::SessionId::new(),
                FileReadStamp {
                    path: Utf8PathBuf::from("other/new.txt"),
                    read_at_ms: 9_999,
                    mtime_ms: Some(9_999),
                    size_bytes: Some(1),
                    content_sha256: Some("new".to_string()),
                },
            )
            .expect("record over capacity");

        let cache = safety.read_stamps.lock().expect("edit safety cache");
        assert_eq!(cache.entries.len(), MAX_EDIT_READ_BASELINES);
        assert!(cache.entries.contains_key(&(session_id, retained)));
        assert!(
            !cache
                .entries
                .contains_key(&(session_id, Utf8PathBuf::from("generated/1.txt")))
        );
    }

    #[tokio::test]
    async fn unique_file_lock_churn_does_not_grow_the_registry() {
        let safety = EditSafety::default();
        for index in 0..1_000 {
            let path = Utf8PathBuf::from(format!("generated/{index}.txt"));
            safety
                .with_file_lock(&path, async { Ok::<_, ()>(()) })
                .await
                .expect("single lock operation");
        }

        assert!(
            safety
                .file_locks
                .lock()
                .expect("file lock registry")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn multi_file_lock_cleanup_removes_only_expired_weak_entries() {
        let safety = EditSafety::default();
        let paths = vec![
            Utf8PathBuf::from("generated/first.txt"),
            Utf8PathBuf::from("generated/second.txt"),
        ];
        safety
            .with_file_locks(&paths, async { Ok::<_, ()>(()) })
            .await
            .expect("multi lock operation");

        assert!(
            safety
                .file_locks
                .lock()
                .expect("file lock registry")
                .is_empty()
        );
    }
}
