use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};

use crate::error::StorageError;
use crate::harness::{HarnessRunId, QualityGateResult};
use crate::runtime::SystemClock;

pub trait GateResultStore {
    fn insert_gate_result(
        &self,
        run_id: HarnessRunId,
        result: &QualityGateResult,
    ) -> Result<(), StorageError>;
    fn list_gate_results(
        &self,
        run_id: HarnessRunId,
    ) -> Result<Vec<QualityGateResult>, StorageError>;
}

#[derive(Clone)]
pub struct SqliteGateResultStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteGateResultStore {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }
}

impl GateResultStore for SqliteGateResultStore {
    fn insert_gate_result(
        &self,
        run_id: HarnessRunId,
        result: &QualityGateResult,
    ) -> Result<(), StorageError> {
        let payload_json = serde_json::to_string(result)?;
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let sequence_no: i64 = connection.query_row(
            "SELECT COALESCE(MAX(sequence_no) + 1, 0) FROM harness_gate_results WHERE run_id = ?1",
            params![run_id.to_string()],
            |row| row.get(0),
        )?;
        connection.execute(
            "INSERT OR REPLACE INTO harness_gate_results
             (id, run_id, sequence_no, gate_kind, status, severity, owner, summary, payload_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                result.gate_id.to_string(),
                run_id.to_string(),
                sequence_no,
                serde_json::to_string(&result.gate_kind)?,
                serde_json::to_string(&result.status)?,
                serde_json::to_string(&result.severity)?,
                result
                    .owner
                    .map(|owner| serde_json::to_string(&owner))
                    .transpose()?,
                result.summary,
                payload_json,
                SystemClock::now_ms(),
            ],
        )?;
        Ok(())
    }

    fn list_gate_results(
        &self,
        run_id: HarnessRunId,
    ) -> Result<Vec<QualityGateResult>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT payload_json FROM harness_gate_results WHERE run_id = ?1 ORDER BY sequence_no ASC",
        )?;
        let rows =
            statement.query_map(params![run_id.to_string()], |row| row.get::<_, String>(0))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(serde_json::from_str(&row?)?);
        }
        Ok(results)
    }
}
