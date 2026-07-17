use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use camino::Utf8Path;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use crate::error::StorageError;
use crate::runtime::{Clock, SystemClock};
use crate::session::{ProjectId, ProjectRecord, ProjectRepository};

#[derive(Clone)]
pub struct SqliteProjectRepository {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteProjectRepository {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }
}

#[async_trait(?Send)]
impl ProjectRepository for SqliteProjectRepository {
    async fn upsert_project(
        &self,
        id: ProjectId,
        root_path: &Utf8Path,
        display_name: &str,
        vcs_kind: &str,
    ) -> Result<ProjectRecord, StorageError> {
        let now = SystemClock.now_ms();
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing_id_for_root = transaction
            .query_row(
                "SELECT id FROM projects WHERE root_path = ?1",
                params![root_path.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if existing_id_for_root
            .as_deref()
            .is_some_and(|existing| existing != id.to_string())
        {
            return Err(StorageError::Message(format!(
                "project root `{root_path}` is already owned by project {}",
                existing_id_for_root.expect("checked existing project id")
            )));
        }
        let existing_root_for_id = transaction
            .query_row(
                "SELECT root_path FROM projects WHERE id = ?1",
                params![id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if existing_root_for_id
            .as_deref()
            .is_some_and(|existing| existing != root_path.as_str())
        {
            return Err(StorageError::Message(format!(
                "project {id} is already bound to root `{}`",
                existing_root_for_id.expect("checked existing project root")
            )));
        }
        transaction.execute(
            "INSERT INTO projects (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                 display_name=excluded.display_name,
                 vcs_kind=excluded.vcs_kind,
                 updated_at_ms=excluded.updated_at_ms",
            params![id.to_string(), root_path.as_str(), display_name, vcs_kind, now, now],
        )?;
        let project = transaction.query_row(
            "SELECT root_path, display_name, vcs_kind, created_at_ms, updated_at_ms
             FROM projects WHERE id = ?1",
            params![id.to_string()],
            |row| {
                Ok(ProjectRecord {
                    id,
                    root_path: row.get::<_, String>(0)?.into(),
                    display_name: row.get(1)?,
                    vcs_kind: row.get(2)?,
                    created_at_ms: row.get(3)?,
                    updated_at_ms: row.get(4)?,
                })
            },
        )?;
        transaction.commit()?;
        Ok(project)
    }

    async fn get_project(&self, id: ProjectId) -> Result<ProjectRecord, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.query_row(
            "SELECT id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms FROM projects WHERE id = ?1",
            params![id.to_string()],
            |row| {
                Ok(ProjectRecord {
                    id,
                    root_path: row.get::<_, String>(1)?.into(),
                    display_name: row.get(2)?,
                    vcs_kind: row.get(3)?,
                    created_at_ms: row.get(4)?,
                    updated_at_ms: row.get(5)?,
                })
            },
        ).map_err(StorageError::from)
    }

    async fn list_projects(&self, limit: usize) -> Result<Vec<ProjectRecord>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT projects.id,
                    projects.root_path,
                    projects.display_name,
                    projects.vcs_kind,
                    projects.created_at_ms,
                    projects.updated_at_ms,
                    COALESCE(MAX(sessions.updated_at_ms), projects.updated_at_ms) AS last_activity_ms
             FROM projects
             LEFT JOIN sessions ON sessions.project_id = projects.id
             GROUP BY projects.id
             ORDER BY projects.created_at_ms ASC, lower(projects.display_name) ASC, lower(projects.root_path) ASC
             LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok(ProjectRecord {
                id: row.get::<_, String>(0)?.parse().map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                root_path: row.get::<_, String>(1)?.into(),
                display_name: row.get(2)?,
                vcs_kind: row.get(3)?,
                created_at_ms: row.get(4)?,
                updated_at_ms: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    async fn delete_project(&self, id: ProjectId) -> Result<(), StorageError> {
        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = SystemClock.now_ms();
        let active_session_id = tx
            .query_row(
                "SELECT id
                 FROM sessions
                 WHERE project_id = ?1
                   AND (
                        status = 'running'
                        OR (
                            active_run_id IS NOT NULL
                            AND active_run_lease_expires_at_ms > ?2
                        )
                   )
                 ORDER BY id ASC
                 LIMIT 1",
                params![id.to_string(), now],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(active_session_id) = active_session_id {
            return Err(StorageError::Message(format!(
                "project {id} contains active session {active_session_id}; stop it before deleting the project"
            )));
        }
        tx.execute(
            "DELETE FROM harness_replay_reports
             WHERE run_id IN (
                 SELECT id FROM harness_runs
                 WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)
             )",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM harness_gate_results
             WHERE run_id IN (
                 SELECT id FROM harness_runs
                 WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)
             )",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM harness_contracts
             WHERE run_id IN (
                 SELECT id FROM harness_runs
                 WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)
             )",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM harness_artifacts
             WHERE run_id IN (
                 SELECT id FROM harness_runs
                 WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)
             )",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM harness_events
             WHERE run_id IN (
                 SELECT id FROM harness_runs
                 WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)
             )",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM harness_runs
             WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM protocol_turn_items
             WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM protocol_history_items
             WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM protocol_runtime_events
             WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM protocol_item_append_order
             WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM protocol_turn_sequence_allocators
             WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM sessions WHERE project_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM projects WHERE id = ?1",
            params![id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::config::AccessMode;
    use crate::protocol::TurnId;
    use crate::session::{NewSession, SessionRepository};
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

    use super::*;

    fn project_repo_fixture() -> (SqliteProjectRepository, camino::Utf8PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.keep().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        (StoreBundle::new(sqlite).project_repo(), data_dir)
    }

    #[tokio::test]
    async fn root_identity_collision_is_rejected_without_mutating_the_existing_project() {
        let (repo, root) = project_repo_fixture();
        let owner_id = ProjectId::new();
        repo.upsert_project(owner_id, &root, "original", "git")
            .await
            .expect("seed project");

        let error = repo
            .upsert_project(ProjectId::new(), &root, "must not commit", "none")
            .await
            .expect_err("same root with a different id must fail before mutation");

        assert!(error.to_string().contains("already owned"));
        let preserved = repo.get_project(owner_id).await.expect("preserved project");
        assert_eq!(preserved.display_name, "original");
        assert_eq!(preserved.vcs_kind, "git");
    }

    #[tokio::test]
    async fn project_id_collision_is_rejected_without_rebinding_its_root() {
        let (repo, root) = project_repo_fixture();
        let owner_id = ProjectId::new();
        repo.upsert_project(owner_id, &root, "original", "git")
            .await
            .expect("seed project");
        let other_root = root.join("other");

        let error = repo
            .upsert_project(owner_id, &other_root, "must not commit", "none")
            .await
            .expect_err("same id with a different root must fail before mutation");

        assert!(error.to_string().contains("already bound"));
        let preserved = repo.get_project(owner_id).await.expect("preserved project");
        assert_eq!(preserved.root_path, root);
        assert_eq!(preserved.display_name, "original");
        assert_eq!(preserved.vcs_kind, "git");
    }

    #[tokio::test]
    async fn delete_project_removes_protocol_turn_sequence_allocators() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &data_dir, "project", "none")
            .await
            .expect("project");
        let session = store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: "session".to_string(),
                cwd: data_dir,
                model: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                access_mode: AccessMode::Default,
            })
            .await
            .expect("session");
        let repo = store.project_repo();
        repo.connection
            .lock()
            .expect("sqlite mutex poisoned")
            .execute(
                "INSERT INTO protocol_turn_sequence_allocators
                 (session_id, turn_id, next_sequence_no) VALUES (?1, 'turn', 1)",
                params![session.id.to_string()],
            )
            .expect("allocator");

        repo.delete_project(project_id)
            .await
            .expect("delete project");

        let allocator_count = repo
            .connection
            .lock()
            .expect("sqlite mutex poisoned")
            .query_row(
                "SELECT COUNT(*) FROM protocol_turn_sequence_allocators WHERE session_id = ?1",
                params![session.id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .expect("allocator count");
        assert_eq!(allocator_count, 0);
    }

    #[tokio::test]
    async fn repository_project_delete_rejects_durable_active_sessions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8");
        let paths = StoragePaths {
            data_dir: data_dir.clone(),
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &data_dir, "project", "none")
            .await
            .expect("project");
        let session = store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: "session".to_string(),
                cwd: data_dir,
                model: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                access_mode: AccessMode::Default,
            })
            .await
            .expect("session");
        store
            .session_repo()
            .admit_session_turn(session.id, TurnId::new())
            .await
            .expect("admission")
            .expect("admitted");

        let error = store
            .project_repo()
            .delete_project(project_id)
            .await
            .expect_err("active session must block direct repository deletion");
        assert!(error.to_string().contains(&session.id.to_string()));
        assert!(store.session_repo().get_session(session.id).await.is_ok());
        assert!(store.project_repo().get_project(project_id).await.is_ok());
    }
}
