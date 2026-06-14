use camino::Utf8PathBuf;
use directories_next::ProjectDirs;

use crate::error::StorageError;

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
}

impl StoreBundle {
    pub fn new(store: SqliteStore) -> Self {
        Self { store }
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

    pub fn checkpoint_and_vacuum(&self) -> Result<(), StorageError> {
        self.store.checkpoint_and_vacuum()
    }

    pub fn paths(&self) -> &StoragePaths {
        self.store.paths()
    }
}
