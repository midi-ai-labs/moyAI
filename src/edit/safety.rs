use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::io::Read as _;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::EditError;
use crate::session::SessionId;

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

#[derive(Clone, Default)]
pub struct EditSafety {
    read_stamps: Arc<Mutex<HashMap<SessionId, HashMap<Utf8PathBuf, FileReadStamp>>>>,
    file_locks: Arc<Mutex<HashMap<Utf8PathBuf, Arc<tokio::sync::Mutex<()>>>>>,
}

impl EditSafety {
    pub fn record_read(
        &self,
        session_id: SessionId,
        stamp: FileReadStamp,
    ) -> Result<(), EditError> {
        let mut store = self.read_stamps.lock().expect("edit safety mutex poisoned");
        store
            .entry(session_id)
            .or_default()
            .insert(stamp.path.clone(), stamp);
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
        let store = self.read_stamps.lock().expect("edit safety mutex poisoned");
        let entries = store.get(&session_id);
        let mut unique_paths = paths.to_vec();
        unique_paths.sort();
        unique_paths.dedup();
        unique_paths
            .into_iter()
            .map(|path| {
                let stamp = entries.and_then(|value| value.get(&path)).cloned();
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
        let entries = store.entry(session_id).or_default();
        for (path, stamp) in snapshot {
            if let Some(stamp) = stamp {
                entries.insert(path.clone(), stamp.clone());
            } else {
                entries.remove(path);
            }
        }
        Ok(())
    }

    pub fn get_stamp(&self, session_id: SessionId, path: &Utf8Path) -> Option<FileReadStamp> {
        let store = self.read_stamps.lock().expect("edit safety mutex poisoned");
        store
            .get(&session_id)
            .and_then(|value| value.get(path))
            .cloned()
    }

    pub fn assert_fresh_write(
        &self,
        session_id: SessionId,
        path: &Utf8Path,
        current: &FileContentIdentity,
    ) -> Result<(), EditError> {
        let stamp = self.get_stamp(session_id, path).ok_or_else(|| {
            EditError::Message(format!(
                "path `{path}` was not read in this session. Read the file with the `read` tool before the first replacement, update, or delete of an existing file in this session."
            ))
        })?;
        if stamp.mtime_ms != current.mtime_ms
            || stamp.size_bytes != Some(current.size_bytes)
            || stamp.content_sha256.as_deref() != Some(current.content_sha256.as_str())
        {
            return Err(EditError::Message(format!(
                "path `{path}` changed since its last confirmed contents in this session. Read the current contents again before another replacement, update, or delete."
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
                        "path `{path}` changed before commit and could not be revalidated: {error}"
                    ))
                })?;
                if &current != expected {
                    return Err(EditError::Message(format!(
                        "path `{path}` changed while the edit was being prepared. The external contents were not overwritten; read the file again before retrying."
                    )));
                }
            }
            None => match fs::symlink_metadata(path) {
                Ok(_) => {
                    return Err(EditError::Message(format!(
                        "path `{path}` was created while the edit was being prepared. The external file was not overwritten; read it before retrying."
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
        let lock = {
            let mut locks = self.file_locks.lock().expect("edit safety mutex poisoned");
            locks
                .entry(path.to_path_buf())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
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
        let locks = {
            let mut store = self.file_locks.lock().expect("edit safety mutex poisoned");
            ordered_paths
                .iter()
                .map(|path| {
                    store
                        .entry(path.clone())
                        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                        .clone()
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
        if let Some(entries) = store.get_mut(&session_id) {
            for path in paths {
                entries.remove(path);
            }
        }
        Ok(())
    }

    pub fn invalidate_roots(
        &self,
        session_id: SessionId,
        roots: &[Utf8PathBuf],
    ) -> Result<(), EditError> {
        let mut store = self.read_stamps.lock().expect("edit safety mutex poisoned");
        if let Some(entries) = store.get_mut(&session_id) {
            entries.retain(|path, _| !roots.iter().any(|root| path.starts_with(root)));
        }
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

    use super::{EditSafety, read_file_with_identity};

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
        assert!(
            error
                .to_string()
                .contains("external contents were not overwritten")
        );
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
}
