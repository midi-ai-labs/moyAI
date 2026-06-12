use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::error::EditError;
use crate::session::SessionId;

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileReadStamp {
    pub path: Utf8PathBuf,
    pub read_at_ms: i64,
    pub mtime_ms: Option<i64>,
    pub size_bytes: Option<u64>,
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
        let metadata = fs::metadata(path)?;
        self.record_read(
            session_id,
            FileReadStamp {
                path: path.to_path_buf(),
                read_at_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|value| value.as_millis() as i64)
                    .unwrap_or_default(),
                mtime_ms: metadata
                    .modified()
                    .ok()
                    .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                    .map(|value| value.as_millis() as i64),
                size_bytes: Some(metadata.len()),
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
        current_mtime_ms: Option<i64>,
        current_size_bytes: Option<u64>,
    ) -> Result<(), EditError> {
        let stamp = self.get_stamp(session_id, path).ok_or_else(|| {
            EditError::Message(format!(
                "path `{path}` was not read in this session. Read the file with the `read` tool before the first replacement, update, or delete of an existing file in this session."
            ))
        })?;
        if stamp.mtime_ms != current_mtime_ms || stamp.size_bytes != current_size_bytes {
            return Err(EditError::Message(format!(
                "path `{path}` changed since its last confirmed contents in this session. Read the current contents again before another replacement, update, or delete."
            )));
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
}

#[cfg(test)]
mod tests {}
