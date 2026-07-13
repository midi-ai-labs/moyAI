use std::collections::HashSet;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::StorageError;
use crate::storage::{
    SqliteChangeRepository, SqliteProjectRepository, SqliteSessionRepository, StoragePaths,
};

const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct SqliteStore {
    connection: Arc<Mutex<Connection>>,
    paths: StoragePaths,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageMaintenanceReport {
    pub orphan_harness_dirs_removed: usize,
    pub orphan_truncation_files_removed: usize,
}

impl SqliteStore {
    pub fn open(paths: &StoragePaths) -> Result<Self, StorageError> {
        std::fs::create_dir_all(&paths.data_dir)?;
        std::fs::create_dir_all(&paths.truncation_dir)?;
        let connection = Connection::open(&paths.database_path)?;
        connection.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
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

    pub fn cleanup_orphan_internal_files(&self) -> Result<StorageMaintenanceReport, StorageError> {
        let referenced_harness_runs = self.referenced_harness_run_ids()?;
        let referenced_truncation_paths = self.referenced_truncation_paths()?;
        let mut report = StorageMaintenanceReport::default();

        let harness_root = self.paths.data_dir.join("harness");
        if harness_root.exists() {
            for entry in fs::read_dir(harness_root.as_std_path())? {
                let entry = entry?;
                let path = camino::Utf8PathBuf::from_path_buf(entry.path()).map_err(|_| {
                    StorageError::Message("harness path is not valid UTF-8".to_string())
                })?;
                if !path.starts_with(&harness_root) || !entry.file_type()?.is_dir() {
                    continue;
                }
                let Some(name) = path.file_name() else {
                    continue;
                };
                if !referenced_harness_runs.contains(name) {
                    fs::remove_dir_all(path.as_std_path())?;
                    report.orphan_harness_dirs_removed += 1;
                }
            }
        }

        if self.paths.truncation_dir.exists() {
            for entry in fs::read_dir(self.paths.truncation_dir.as_std_path())? {
                let entry = entry?;
                let path = camino::Utf8PathBuf::from_path_buf(entry.path()).map_err(|_| {
                    StorageError::Message("truncation path is not valid UTF-8".to_string())
                })?;
                if !path.starts_with(&self.paths.truncation_dir) || !entry.file_type()?.is_file() {
                    continue;
                }
                if !referenced_truncation_paths.contains(path.as_str()) {
                    fs::remove_file(path.as_std_path())?;
                    report.orphan_truncation_files_removed += 1;
                }
            }
        }

        Ok(report)
    }

    pub fn checkpoint_and_vacuum(&self) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(())
    }

    pub fn paths(&self) -> &StoragePaths {
        &self.paths
    }

    fn referenced_harness_run_ids(&self) -> Result<HashSet<String>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare("SELECT id FROM harness_runs")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut ids = HashSet::new();
        for row in rows {
            ids.insert(row?);
        }
        Ok(ids)
    }

    fn referenced_truncation_paths(&self) -> Result<HashSet<String>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT truncated_output_path FROM tool_calls WHERE truncated_output_path IS NOT NULL",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut paths = HashSet::new();
        for row in rows {
            paths.insert(row?);
        }
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;

    #[test]
    fn production_connection_configures_a_busy_timeout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };

        let store = SqliteStore::open(&paths).expect("store");
        let timeout_ms = store
            .connection
            .lock()
            .expect("sqlite mutex poisoned")
            .query_row("PRAGMA busy_timeout", [], |row| row.get::<_, i64>(0))
            .expect("busy timeout");

        assert_eq!(timeout_ms, SQLITE_BUSY_TIMEOUT.as_millis() as i64);
    }

    #[test]
    fn second_production_connection_retries_a_busy_writer() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let first = SqliteStore::open(&paths).expect("first store");
        first.migrate().expect("migrate");
        let second = SqliteStore::open(&paths).expect("second store");
        first
            .connection
            .lock()
            .expect("first sqlite mutex")
            .execute_batch("BEGIN IMMEDIATE")
            .expect("hold writer transaction");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            started_tx.send(()).expect("writer started");
            second
                .connection
                .lock()
                .expect("second sqlite mutex")
                .execute(
                    "INSERT INTO projects
                     (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                     VALUES ('busy-project', 'C:/busy', 'busy', 'none', 1, 1)",
                    [],
                )
        });
        started_rx.recv().expect("writer start");
        std::thread::sleep(Duration::from_millis(100));
        first
            .connection
            .lock()
            .expect("first sqlite mutex")
            .execute_batch("COMMIT")
            .expect("release writer transaction");

        assert_eq!(
            writer.join().expect("writer thread").expect("busy retry"),
            1
        );
        let stored = first
            .connection
            .lock()
            .expect("first sqlite mutex")
            .query_row(
                "SELECT COUNT(*) FROM projects WHERE id = 'busy-project'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .expect("stored project");
        assert_eq!(stored, 1);
    }

    #[test]
    fn cleanup_orphan_internal_files_removes_only_unreferenced_appdata_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");

        let harness_root = data_dir.join("harness");
        let referenced_harness = harness_root.join("referenced-run");
        let orphan_harness = harness_root.join("orphan-run");
        fs::create_dir_all(referenced_harness.as_std_path()).expect("referenced harness");
        fs::create_dir_all(orphan_harness.as_std_path()).expect("orphan harness");
        fs::write(referenced_harness.join("event.json").as_std_path(), "{}").expect("write ref");
        fs::write(orphan_harness.join("event.json").as_std_path(), "{}").expect("write orphan");

        fs::create_dir_all(paths.truncation_dir.as_std_path()).expect("truncation dir");
        let referenced_truncation = paths.truncation_dir.join("referenced.txt");
        let orphan_truncation = paths.truncation_dir.join("orphan.txt");
        fs::write(referenced_truncation.as_std_path(), "referenced").expect("write trunc ref");
        fs::write(orphan_truncation.as_std_path(), "orphan").expect("write trunc orphan");

        {
            let connection = store.connection.lock().expect("sqlite mutex poisoned");
            connection
                .execute(
                    "INSERT INTO harness_runs
                     (id, session_id, workspace_root, artifact_root, mode, started_at_ms, completed_at_ms, status)
                     VALUES (?1, NULL, ?2, ?3, 'native_runtime', 1, NULL, 'started')",
                    params![
                        "referenced-run",
                        "C:/workspace",
                        referenced_harness.as_str()
                    ],
                )
                .expect("insert harness run");
            connection
                .execute(
                    "INSERT INTO projects
                     (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
                     VALUES ('project', 'C:/workspace', 'workspace', 'none', 1, 1)",
                    [],
                )
                .expect("insert project");
            connection
                .execute(
                    "INSERT INTO sessions
                     (id, project_id, title, status, cwd_path, model_name, base_url, created_at_ms, updated_at_ms, completed_at_ms)
                     VALUES ('session', 'project', 'session', 'completed', 'C:/workspace', 'model', 'http://localhost:1234', 1, 1, 1)",
                    [],
                )
                .expect("insert session");
            connection
                .execute(
                    "INSERT INTO messages
                     (id, session_id, parent_message_id, role, sequence_no, metadata_json, created_at_ms)
                     VALUES ('message', 'session', NULL, 'assistant', 1, '{}', 1)",
                    [],
                )
                .expect("insert message");
            connection
                .execute(
                    "INSERT INTO tool_calls
                     (id, session_id, message_id, tool_name, status, arguments_json, title, metadata_json, output_text, truncated_output_path, error_text, started_at_ms, finished_at_ms)
                     VALUES ('tool', 'session', 'message', 'shell', 'completed', '{}', NULL, '{}', 'preview', ?1, NULL, 1, 1)",
                    params![referenced_truncation.as_str()],
                )
                .expect("insert tool call");
        }

        let report = store
            .cleanup_orphan_internal_files()
            .expect("cleanup orphan files");

        assert_eq!(report.orphan_harness_dirs_removed, 1);
        assert_eq!(report.orphan_truncation_files_removed, 1);
        assert!(referenced_harness.exists());
        assert!(!orphan_harness.exists());
        assert!(referenced_truncation.exists());
        assert!(!orphan_truncation.exists());
    }
}
