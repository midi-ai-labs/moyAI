use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};

use crate::error::StorageError;
use crate::harness::{HarnessEvent, HarnessEventId, HarnessRunId, artifact::hash_bytes};

pub trait HarnessEventStore {
    fn append_event(&self, event: &HarnessEvent) -> Result<(), StorageError>;
    fn list_events(&self, run_id: HarnessRunId) -> Result<Vec<HarnessEvent>, StorageError>;
}

#[derive(Clone)]
pub struct SqliteHarnessEventStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteHarnessEventStore {
    pub fn new(connection: Arc<Mutex<Connection>>) -> Self {
        Self { connection }
    }
}

impl HarnessEventStore for SqliteHarnessEventStore {
    fn append_event(&self, event: &HarnessEvent) -> Result<(), StorageError> {
        let payload_json = serde_json::to_string(&event.payload)?;
        let contract_refs_json = serde_json::to_string(&event.contract_refs)?;
        let artifact_refs_json = serde_json::to_string(&event.artifact_refs)?;
        let payload_sha256 = hash_bytes(payload_json.as_bytes());
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        connection.execute(
            "INSERT INTO harness_events (id, run_id, sequence_no, kind, payload_json, contract_refs_json, artifact_refs_json, parent_event_id, payload_sha256, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                event.id.to_string(),
                event.run_id.to_string(),
                event.sequence_no,
                serde_json::to_string(&event.kind)?,
                payload_json,
                contract_refs_json,
                artifact_refs_json,
                event.parent_event_id.map(|id| id.to_string()),
                payload_sha256,
                event.created_at_ms,
            ],
        )?;
        Ok(())
    }

    fn list_events(&self, run_id: HarnessRunId) -> Result<Vec<HarnessEvent>, StorageError> {
        let connection = self.connection.lock().expect("sqlite mutex poisoned");
        let mut statement = connection.prepare(
            "SELECT id, sequence_no, kind, payload_json, contract_refs_json, artifact_refs_json, parent_event_id, created_at_ms
             FROM harness_events WHERE run_id = ?1 ORDER BY sequence_no ASC",
        )?;
        let rows = statement.query_map(params![run_id.to_string()], |row| {
            let id: String = row.get(0)?;
            let kind_json: String = row.get(2)?;
            let payload_json: String = row.get(3)?;
            let contract_refs_json: String = row.get(4)?;
            let artifact_refs_json: String = row.get(5)?;
            let parent: Option<String> = row.get(6)?;
            Ok((
                id,
                row.get::<_, i64>(1)?,
                kind_json,
                payload_json,
                contract_refs_json,
                artifact_refs_json,
                parent,
                row.get::<_, i64>(7)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (
                id,
                sequence_no,
                kind_json,
                payload_json,
                contract_refs_json,
                artifact_refs_json,
                parent,
                created_at_ms,
            ) = row?;
            events.push(HarnessEvent {
                id: id.parse::<HarnessEventId>().map_err(|error| {
                    StorageError::Message(format!("invalid harness event id `{id}`: {error}"))
                })?,
                run_id,
                sequence_no,
                created_at_ms,
                kind: serde_json::from_str(&kind_json)?,
                payload: serde_json::from_str(&payload_json)?,
                contract_refs: serde_json::from_str(&contract_refs_json)?,
                artifact_refs: serde_json::from_str(&artifact_refs_json)?,
                parent_event_id: parent
                    .map(|value| {
                        value.parse::<HarnessEventId>().map_err(|error| {
                            StorageError::Message(format!(
                                "invalid parent harness event id `{value}`: {error}"
                            ))
                        })
                    })
                    .transpose()?,
            });
        }
        Ok(events)
    }
}
