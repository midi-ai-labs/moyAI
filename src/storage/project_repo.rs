use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use camino::Utf8Path;
use rusqlite::{Connection, params};

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
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT INTO projects (id, root_path, display_name, vcs_kind, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(root_path) DO UPDATE SET display_name=excluded.display_name, vcs_kind=excluded.vcs_kind, updated_at_ms=excluded.updated_at_ms",
            params![id.to_string(), root_path.as_str(), display_name, vcs_kind, now, now],
        )?;
        drop(connection);
        self.get_project(id).await
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
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let tx = connection.unchecked_transaction()?;
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
            "DELETE FROM file_changes
             WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM tool_calls
             WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM message_parts
             WHERE message_id IN (
                 SELECT id FROM messages
                 WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)
             )",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM messages
             WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM session_todos
             WHERE session_id IN (SELECT id FROM sessions WHERE project_id = ?1)",
            params![id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM session_state
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
mod tests {}
