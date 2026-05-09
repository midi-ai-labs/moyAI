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
}
