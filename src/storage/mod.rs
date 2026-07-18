use std::fs::{File, OpenOptions};
use std::sync::Arc;

use camino::Utf8PathBuf;
use directories_next::ProjectDirs;
use fs2::FileExt;

use crate::error::StorageError;
use crate::runtime::{ActiveRunRegistry, RunProcessLease};

pub mod change_repo;
pub mod migration;
pub mod project_repo;
pub mod session_repo;
pub mod sqlite;

pub use change_repo::SqliteChangeRepository;
pub use project_repo::SqliteProjectRepository;
pub use session_repo::SqliteSessionRepository;
pub use sqlite::{SqliteStore, StorageMaintenanceReport};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoragePaths {
    pub data_dir: Utf8PathBuf,
    pub database_path: Utf8PathBuf,
    pub truncation_dir: Utf8PathBuf,
}

const INTERNAL_FILE_MAINTENANCE_LOCK: &str = ".internal-files.lock";

/// A cross-process shared lease held from creation of an internal file until its
/// durable database reference has committed. Maintenance takes the exclusive
/// counterpart, so it cannot observe the producer's in-between state.
#[derive(Clone)]
pub(crate) struct InternalFileProducerLease {
    _inner: Arc<InternalFileProducerLeaseInner>,
}

struct InternalFileProducerLeaseInner {
    file: File,
}

impl std::fmt::Debug for InternalFileProducerLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InternalFileProducerLease")
            .finish_non_exhaustive()
    }
}

impl InternalFileProducerLease {
    pub(crate) fn acquire(paths: &StoragePaths) -> Result<Self, StorageError> {
        let file = open_internal_file_maintenance_lock(paths)?;
        FileExt::lock_shared(&file)?;
        Ok(Self {
            _inner: Arc::new(InternalFileProducerLeaseInner { file }),
        })
    }
}

impl Drop for InternalFileProducerLeaseInner {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub(crate) struct InternalFileMaintenanceLease {
    file: File,
}

impl InternalFileMaintenanceLease {
    pub(crate) fn acquire(paths: &StoragePaths) -> Result<Self, StorageError> {
        let file = open_internal_file_maintenance_lock(paths)?;
        FileExt::lock_exclusive(&file)?;
        Ok(Self { file })
    }
}

impl Drop for InternalFileMaintenanceLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn open_internal_file_maintenance_lock(paths: &StoragePaths) -> Result<File, StorageError> {
    std::fs::create_dir_all(&paths.data_dir)?;
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(
            paths
                .data_dir
                .join(INTERNAL_FILE_MAINTENANCE_LOCK)
                .as_std_path(),
        )?)
}

impl StoragePaths {
    pub fn discover() -> Result<Self, StorageError> {
        if let Ok(value) = std::env::var("MOYAI_DATA_DIR") {
            let data_dir = Utf8PathBuf::from(value);
            let truncation_dir = data_dir.join("truncation");
            let database_path = data_dir.join("moyai.sqlite3");
            return Ok(Self {
                data_dir,
                database_path,
                truncation_dir,
            });
        }
        let dirs = ProjectDirs::from("net", "midi-ai-labs", "moyai")
            .ok_or_else(|| StorageError::Message("failed to resolve data directory".to_string()))?;
        let data_dir = Utf8PathBuf::from_path_buf(dirs.data_dir().to_path_buf())
            .map_err(|_| StorageError::Message("data directory is not valid UTF-8".to_string()))?;
        let truncation_dir = data_dir.join("truncation");
        let database_path = data_dir.join("moyai.sqlite3");
        Ok(Self {
            data_dir,
            database_path,
            truncation_dir,
        })
    }
}

#[derive(Clone)]
pub struct StoreBundle {
    store: SqliteStore,
    active_runs: ActiveRunRegistry,
}

impl StoreBundle {
    pub fn new(store: SqliteStore) -> Self {
        Self {
            store,
            active_runs: ActiveRunRegistry::default(),
        }
    }

    pub fn active_runs(&self) -> &ActiveRunRegistry {
        &self.active_runs
    }

    pub fn try_acquire_run_process_lease(
        &self,
        session_id: crate::session::SessionId,
    ) -> Result<RunProcessLease, StorageError> {
        RunProcessLease::try_acquire(&self.store.paths().data_dir, session_id)
    }

    pub fn session_repo(&self) -> SqliteSessionRepository {
        self.store.session_repo()
    }

    pub fn project_repo(&self) -> SqliteProjectRepository {
        self.store.project_repo()
    }

    pub fn change_repo(&self) -> SqliteChangeRepository {
        self.store.change_repo()
    }

    pub fn harness_event_store(&self) -> crate::harness::SqliteHarnessEventStore {
        self.store.harness_event_store()
    }

    pub fn harness_run_store(&self) -> crate::harness::SqliteHarnessRunStore {
        self.store.harness_run_store()
    }

    pub fn harness_artifact_store(&self) -> crate::harness::SqliteArtifactStore {
        self.store.harness_artifact_store()
    }

    pub fn harness_contract_store(&self) -> crate::harness::SqliteContractStore {
        self.store.harness_contract_store()
    }

    pub fn harness_gate_result_store(&self) -> crate::harness::SqliteGateResultStore {
        self.store.harness_gate_result_store()
    }

    pub fn harness_replay_report_store(&self) -> crate::harness::SqliteReplayReportStore {
        self.store.harness_replay_report_store()
    }

    pub fn protocol_event_store(&self) -> crate::protocol::SqliteProtocolEventStore {
        self.store.protocol_event_store()
    }

    pub fn cleanup_orphan_internal_files(&self) -> Result<StorageMaintenanceReport, StorageError> {
        self.store.cleanup_orphan_internal_files()
    }

    pub fn paths(&self) -> &StoragePaths {
        self.store.paths()
    }
}
