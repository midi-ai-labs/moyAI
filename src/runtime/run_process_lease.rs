use std::fs::{File, OpenOptions};

use camino::Utf8Path;
use fs2::FileExt;

use crate::error::StorageError;
use crate::session::SessionId;

pub struct RunProcessLease {
    file: File,
}

impl RunProcessLease {
    pub fn try_acquire(data_dir: &Utf8Path, session_id: SessionId) -> Result<Self, StorageError> {
        let lease_dir = data_dir.join("run-leases");
        std::fs::create_dir_all(&lease_dir)?;
        let lease_path = lease_dir.join(format!("{session_id}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lease_path.as_std_path())?;
        FileExt::try_lock_exclusive(&file).map_err(|error| {
            StorageError::Message(format!(
                "session {session_id} is owned by another live process: {error}"
            ))
        })?;
        Ok(Self { file })
    }
}

impl Drop for RunProcessLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_process_lock_blocks_same_session_until_guard_drops() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");
        let session_id = SessionId::new();
        let first = RunProcessLease::try_acquire(&data_dir, session_id).expect("first lock");

        assert!(RunProcessLease::try_acquire(&data_dir, session_id).is_err());
        drop(first);
        RunProcessLease::try_acquire(&data_dir, session_id).expect("lock after owner exit");
    }

    #[test]
    fn process_locks_are_scoped_per_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir =
            camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir");

        let _first =
            RunProcessLease::try_acquire(&data_dir, SessionId::new()).expect("first session lock");
        let _second =
            RunProcessLease::try_acquire(&data_dir, SessionId::new()).expect("second session lock");
    }
}
