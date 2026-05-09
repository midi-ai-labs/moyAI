use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

use crate::error::EditError;
use crate::session::SessionId;

#[derive(Debug, Clone, Serialize, Deserialize)]
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

    pub async fn with_file_lock<T, F>(&self, path: &Utf8Path, op: F) -> Result<T, EditError>
    where
        F: Future<Output = Result<T, EditError>>,
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
