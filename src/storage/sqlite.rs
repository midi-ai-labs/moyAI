use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::error::StorageError;
use crate::storage::{
    SqliteChangeRepository, SqliteProjectRepository, SqliteSessionRepository, StoragePaths,
};

#[derive(Clone)]
pub struct SqliteStore {
    connection: Arc<Mutex<Connection>>,
    paths: StoragePaths,
}

impl SqliteStore {
    pub fn open(paths: &StoragePaths) -> Result<Self, StorageError> {
        std::fs::create_dir_all(&paths.data_dir)?;
        std::fs::create_dir_all(&paths.truncation_dir)?;
        let connection = Connection::open(&paths.database_path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            paths: paths.clone(),
        })
    }

    pub fn migrate(&self) -> Result<(), StorageError> {
        crate::storage::migration::run(&self.connection.lock().expect("sqlite mutex poisoned"))
    }

    pub fn session_repo(&self) -> SqliteSessionRepository {
        SqliteSessionRepository::new(self.connection.clone())
    }

    pub fn project_repo(&self) -> SqliteProjectRepository {
        SqliteProjectRepository::new(self.connection.clone())
    }

    pub fn change_repo(&self) -> SqliteChangeRepository {
        SqliteChangeRepository::new(self.connection.clone())
    }

    pub fn harness_event_store(&self) -> crate::harness::SqliteHarnessEventStore {
        crate::harness::SqliteHarnessEventStore::new(self.connection.clone())
    }

    pub fn harness_run_store(&self) -> crate::harness::SqliteHarnessRunStore {
        crate::harness::SqliteHarnessRunStore::new(self.connection.clone())
    }

    pub fn harness_artifact_store(&self) -> crate::harness::SqliteArtifactStore {
        crate::harness::SqliteArtifactStore::new(self.connection.clone())
    }

    pub fn harness_contract_store(&self) -> crate::harness::SqliteContractStore {
        crate::harness::SqliteContractStore::new(self.connection.clone())
    }

    pub fn harness_gate_result_store(&self) -> crate::harness::SqliteGateResultStore {
        crate::harness::SqliteGateResultStore::new(self.connection.clone())
    }

    pub fn harness_replay_report_store(&self) -> crate::harness::SqliteReplayReportStore {
        crate::harness::SqliteReplayReportStore::new(self.connection.clone())
    }

    pub fn protocol_event_store(&self) -> crate::protocol::SqliteProtocolEventStore {
        crate::protocol::SqliteProtocolEventStore::new(self.connection.clone())
    }

    pub fn paths(&self) -> &StoragePaths {
        &self.paths
    }
}
