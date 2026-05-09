use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::harness::HarnessRunId;
use crate::session::SessionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessRunStatus {
    Started,
    Pass,
    Fail,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessRunRecord {
    pub id: HarnessRunId,
    pub session_id: Option<SessionId>,
    pub workspace_root: Utf8PathBuf,
    pub artifact_root: Utf8PathBuf,
    pub mode: String,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub status: HarnessRunStatus,
}

pub trait HarnessRunStore {
    fn upsert_run(&self, run: &HarnessRunRecord) -> Result<(), StorageError>;
    fn get_run(&self, run_id: HarnessRunId) -> Result<Option<HarnessRunRecord>, StorageError>;
}

#[derive(Clone)]
pub struct SqliteHarnessRunStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteHarnessRunStore {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }
}

impl HarnessRunStore for SqliteHarnessRunStore {
    fn upsert_run(&self, run: &HarnessRunRecord) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT OR REPLACE INTO harness_runs
             (id, session_id, workspace_root, artifact_root, mode, started_at_ms, completed_at_ms, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run.id.to_string(),
                run.session_id.map(|id| id.to_string()),
                run.workspace_root.as_str(),
                run.artifact_root.as_str(),
                run.mode,
                run.started_at_ms,
                run.completed_at_ms,
                serde_json::to_string(&run.status)?,
            ],
        )?;
        Ok(())
    }

    fn get_run(&self, run_id: HarnessRunId) -> Result<Option<HarnessRunRecord>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection
            .query_row(
                "SELECT session_id, workspace_root, artifact_root, mode, started_at_ms, completed_at_ms, status
                 FROM harness_runs WHERE id = ?1",
                params![run_id.to_string()],
                |row| {
                    let session_id: Option<String> = row.get(0)?;
                    let status_json: String = row.get(6)?;
                    Ok((
                        session_id,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                        status_json,
                    ))
                },
            )
            .optional()?
            .map(
                |(
                    session_id,
                    workspace_root,
                    artifact_root,
                    mode,
                    started_at_ms,
                    completed_at_ms,
                    status_json,
                )| {
                    Ok(HarnessRunRecord {
                        id: run_id,
                        session_id: session_id
                            .map(|value| {
                                value.parse::<SessionId>().map_err(|error| {
                                    StorageError::Message(format!(
                                        "invalid harness session id `{value}`: {error}"
                                    ))
                                })
                            })
                            .transpose()?,
                        workspace_root: workspace_root.into(),
                        artifact_root: artifact_root.into(),
                        mode,
                        started_at_ms,
                        completed_at_ms,
                        status: serde_json::from_str(&status_json)?,
                    })
                },
            )
            .transpose()
    }
}
