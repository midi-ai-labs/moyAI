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
            "SELECT id, sequence_no, kind, payload_json, contract_refs_json, artifact_refs_json, parent_event_id, payload_sha256, created_at_ms
             FROM harness_events WHERE run_id = ?1 ORDER BY sequence_no ASC",
        )?;
        let rows = statement.query_map(params![run_id.to_string()], |row| {
            let id: String = row.get(0)?;
            let kind_json: String = row.get(2)?;
            let payload_json: String = row.get(3)?;
            let contract_refs_json: String = row.get(4)?;
            let artifact_refs_json: String = row.get(5)?;
            let parent: Option<String> = row.get(6)?;
            let payload_sha256: String = row.get(7)?;
            Ok((
                id,
                row.get::<_, i64>(1)?,
                kind_json,
                payload_json,
                contract_refs_json,
                artifact_refs_json,
                parent,
                payload_sha256,
                row.get::<_, i64>(8)?,
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
                payload_sha256,
                created_at_ms,
            ) = row?;
            let actual_payload_sha256 = hash_bytes(payload_json.as_bytes());
            if actual_payload_sha256 != payload_sha256 {
                return Err(StorageError::Message(format!(
                    "harness event payload hash mismatch for event `{id}`"
                )));
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{HarnessEventKind, HarnessEventPayload};

    fn test_store() -> (SqliteHarnessEventStore, Arc<Mutex<Connection>>) {
        let connection = Arc::new(Mutex::new(
            Connection::open_in_memory().expect("open sqlite"),
        ));
        connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch(
                "CREATE TABLE harness_events (
                    id TEXT PRIMARY KEY NOT NULL,
                    run_id TEXT NOT NULL,
                    sequence_no INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    contract_refs_json TEXT NOT NULL,
                    artifact_refs_json TEXT NOT NULL,
                    parent_event_id TEXT,
                    payload_sha256 TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL
                );",
            )
            .expect("create harness_events");
        (SqliteHarnessEventStore::new(connection.clone()), connection)
    }

    #[test]
    fn rejects_payload_tampering_in_persisted_event() {
        let (store, connection) = test_store();
        let event = HarnessEvent {
            id: HarnessEventId::new(),
            run_id: HarnessRunId::new(),
            sequence_no: 0,
            created_at_ms: 1,
            kind: HarnessEventKind::UserTurnAccepted,
            payload: HarnessEventPayload::generic(serde_json::json!({"message": "original"})),
            contract_refs: Vec::new(),
            artifact_refs: Vec::new(),
            parent_event_id: None,
        };
        store.append_event(&event).expect("append event");
        connection
            .lock()
            .expect("sqlite mutex")
            .execute(
                "UPDATE harness_events SET payload_json = ?1 WHERE id = ?2",
                params![
                    "{\"type\":\"generic\",\"data\":{\"message\":\"tampered\"}}",
                    event.id.to_string()
                ],
            )
            .expect("tamper event");

        let error = store
            .list_events(event.run_id)
            .expect_err("tampered event must fail integrity validation");
        assert!(error.to_string().contains("payload hash mismatch"));
    }
}
