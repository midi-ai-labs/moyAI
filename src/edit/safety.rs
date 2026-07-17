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
    ) -> Result<(), EditError> {
        let (_, identity) = read_file_with_identity(path)?;
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

    pub fn sync_file_mutations(
        &self,
        session_id: SessionId,
        removed_paths: &[Utf8PathBuf],
        current_paths: &[Utf8PathBuf],
    ) -> Result<(), EditError> {
        self.invalidate_paths(session_id, removed_paths)?;
        for path in current_paths {
            if path.exists() {
                self.record_current_file_state(session_id, path)?;
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
                let (_, current) = read_file_with_identity(path).map_err(|error| {
                    EditError::Message(format!(
                        "path `{path}` could not be revalidated before commit: {error}"
                    ))
                })?;
                if &current != expected {
                    return Err(EditError::Message(format!(
                        "path `{path}` changed while the edit was being prepared; the commit was not applied"
                    )));
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

pub fn read_file_with_identity(
    path: &Utf8Path,
) -> Result<(Vec<u8>, FileContentIdentity), EditError> {
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let metadata = file.metadata()?;
    let identity = FileContentIdentity {
        mtime_ms: metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map(|value| value.as_millis() as i64),
        size_bytes: metadata.len(),
        content_sha256: format!("{:x}", Sha256::digest(&bytes)),
    };
    Ok((bytes, identity))
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::{EditSafety, FileReadStamp, MAX_EDIT_READ_BASELINES, read_file_with_identity};

    #[test]
    fn commit_revalidation_rejects_external_same_size_change() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = Utf8PathBuf::from_path_buf(temp.path().join("source.txt")).expect("utf8 path");
        std::fs::write(&path, "alpha").expect("seed file");
        let (_, expected) = read_file_with_identity(&path).expect("capture identity");

        std::fs::write(&path, "bravo").expect("external rewrite");

        let error = EditSafety::default()
            .assert_path_unchanged(&path, Some(&expected))
            .expect_err("same-size external rewrite must be rejected");
        assert!(error.to_string().contains("commit was not applied"));
        assert_eq!(std::fs::read_to_string(&path).expect("read file"), "bravo");
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
            .record_current_file_state(session_id, &inside)
            .expect("record inside");
        safety
            .record_current_file_state(session_id, &outside)
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
                .record_current_file_state(session_id, path)
                .expect("record session baseline");
        }
        safety
            .record_current_file_state(other_session_id, &first)
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
