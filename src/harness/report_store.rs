use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::StorageError;
use crate::harness::{HarnessRunId, ReplayReport};
use crate::runtime::SystemClock;
use crate::session::SessionId;

pub trait ReplayReportStore {
    fn save_report(&self, report: &ReplayReport) -> Result<(), StorageError>;
    fn get_report(&self, run_id: HarnessRunId) -> Result<Option<ReplayReport>, StorageError>;
    fn latest_report_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<ReplayReport>, StorageError>;
}

#[derive(Clone)]
pub struct SqliteReplayReportStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteReplayReportStore {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }
}

impl ReplayReportStore for SqliteReplayReportStore {
    fn save_report(&self, report: &ReplayReport) -> Result<(), StorageError> {
        let report_json = serde_json::to_string(report)?;
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT OR REPLACE INTO harness_replay_reports
             (run_id, schema_version, status, primary_owner, summary, restart_point, next_actions_json, report_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                report.run_id.to_string(),
                report.schema_version,
                serde_json::to_string(&report.status)?,
                report
                    .primary_owner
                    .map(|owner| serde_json::to_string(&owner))
                    .transpose()?,
                report.summary,
                report.restart_point,
                serde_json::to_string(&report.next_actions)?,
                report_json,
                SystemClock::now_ms(),
            ],
        )?;
        Ok(())
    }

    fn get_report(&self, run_id: HarnessRunId) -> Result<Option<ReplayReport>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let report_json: Option<String> = connection
            .query_row(
                "SELECT report_json FROM harness_replay_reports WHERE run_id = ?1",
                params![run_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        report_json
            .map(|json| serde_json::from_str(&json).map_err(StorageError::from))
            .transpose()
    }

    fn latest_report_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Option<ReplayReport>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let report_json: Option<String> = connection
            .query_row(
                "SELECT reports.report_json
                 FROM harness_replay_reports reports
                 JOIN harness_runs runs ON runs.id = reports.run_id
                 WHERE runs.session_id = ?1
                 ORDER BY COALESCE(runs.completed_at_ms, runs.started_at_ms) DESC,
                          runs.started_at_ms DESC,
                          reports.created_at_ms DESC,
                          reports.run_id DESC
                 LIMIT 1",
                params![session_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        report_json
            .map(|json| serde_json::from_str(&json).map_err(StorageError::from))
            .transpose()
    }
}
