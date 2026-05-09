use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};

use crate::error::StorageError;
use crate::harness::{ContractId, ContractRecord, HarnessRunId};

pub trait ContractStore {
    fn upsert_contract(
        &self,
        run_id: HarnessRunId,
        record: &ContractRecord,
    ) -> Result<(), StorageError>;
    fn list_contracts(&self, run_id: HarnessRunId) -> Result<Vec<ContractRecord>, StorageError>;
}

#[derive(Clone)]
pub struct SqliteContractStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteContractStore {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }
}

impl ContractStore for SqliteContractStore {
    fn upsert_contract(
        &self,
        run_id: HarnessRunId,
        record: &ContractRecord,
    ) -> Result<(), StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT OR REPLACE INTO harness_contracts (run_id, contract_id, kind, version, source_path, content_sha256, schema_ref, model_visible_summary)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run_id.to_string(),
                record.id.to_string(),
                serde_json::to_string(&record.kind)?,
                record.version,
                record.source_path.as_str(),
                record.content_sha256,
                record.schema_ref,
                record.model_visible_summary,
            ],
        )?;
        Ok(())
    }

    fn list_contracts(&self, run_id: HarnessRunId) -> Result<Vec<ContractRecord>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT contract_id, kind, version, source_path, content_sha256, schema_ref, model_visible_summary
             FROM harness_contracts WHERE run_id = ?1 ORDER BY contract_id ASC, version ASC",
        )?;
        let rows = statement.query_map(params![run_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (id, kind, version, source_path, content_sha256, schema_ref, summary) = row?;
            records.push(ContractRecord {
                id: ContractId::new(id),
                kind: serde_json::from_str(&kind)?,
                version,
                source_path: source_path.into(),
                content_sha256,
                schema_ref,
                model_visible_summary: summary,
            });
        }
        Ok(records)
    }
}
