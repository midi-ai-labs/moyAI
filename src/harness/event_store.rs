use std::sync::{Arc, Mutex};

use rusqlite::{Connection, params};

use crate::error::StorageError;
use crate::harness::{
    ArtifactManifest, HarnessEvent, HarnessEventId, HarnessRunId, artifact::hash_bytes,
    artifact_store::insert_artifact_in_connection,
};

const EVENT_ENVELOPE_HASH_PREFIX: &str = "envelope-v1:";

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

    pub(crate) fn append_event_with_artifact(
        &self,
        event: &HarnessEvent,
        artifact: Option<&ArtifactManifest>,
    ) -> Result<(), StorageError> {
        let payload_json = serde_json::to_string(&event.payload)?;
        let kind_json = serde_json::to_string(&event.kind)?;
        let contract_refs_json = serde_json::to_string(&event.contract_refs)?;
        let artifact_refs_json = serde_json::to_string(&event.artifact_refs)?;
        let parent_event_id = event.parent_event_id.map(|id| id.to_string());
        let envelope_sha256 = event_envelope_sha256(
            &event.id.to_string(),
            &event.run_id.to_string(),
            event.sequence_no,
            &kind_json,
            &payload_json,
            &contract_refs_json,
            &artifact_refs_json,
            parent_event_id.as_deref(),
            event.created_at_ms,
        )?;

        let mut connection = self.connection.lock().expect("sqlite mutex poisoned");
        let transaction = connection.transaction()?;
        transaction.execute(
            "INSERT INTO harness_events (id, run_id, sequence_no, kind, payload_json, contract_refs_json, artifact_refs_json, parent_event_id, payload_sha256, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                event.id.to_string(),
                event.run_id.to_string(),
                event.sequence_no,
                kind_json,
                payload_json,
                contract_refs_json,
                artifact_refs_json,
                parent_event_id,
                envelope_sha256,
                event.created_at_ms,
            ],
        )?;
        if let Some(artifact) = artifact {
            insert_artifact_in_connection(&transaction, artifact)?;
        }
        transaction.commit()?;
        Ok(())
    }
}

impl HarnessEventStore for SqliteHarnessEventStore {
    fn append_event(&self, event: &HarnessEvent) -> Result<(), StorageError> {
        self.append_event_with_artifact(event, None)
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
            if let Some(recorded_envelope_hash) =
                payload_sha256.strip_prefix(EVENT_ENVELOPE_HASH_PREFIX)
            {
                let expected = event_envelope_sha256(
                    &id,
                    &run_id.to_string(),
                    sequence_no,
                    &kind_json,
                    &payload_json,
                    &contract_refs_json,
                    &artifact_refs_json,
                    parent.as_deref(),
                    created_at_ms,
                )?;
                let expected = expected
                    .strip_prefix(EVENT_ENVELOPE_HASH_PREFIX)
                    .expect("current event hash prefix");
                if recorded_envelope_hash != expected {
                    return Err(StorageError::Message(format!(
                        "harness event envelope hash mismatch for event `{id}`"
                    )));
                }
            } else if hash_bytes(payload_json.as_bytes()) != payload_sha256 {
                // V14 artifacts used a payload-only hash. Keep those readable, while every
                // event written by the current store is protected by the versioned envelope.
                return Err(StorageError::Message(format!(
                    "legacy harness event payload hash mismatch for event `{id}`"
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

#[allow(clippy::too_many_arguments)]
fn event_envelope_sha256(
    id: &str,
    run_id: &str,
    sequence_no: i64,
    kind_json: &str,
    payload_json: &str,
    contract_refs_json: &str,
    artifact_refs_json: &str,
    parent_event_id: Option<&str>,
    created_at_ms: i64,
) -> Result<String, StorageError> {
    let envelope = serde_json::to_vec(&(
        id,
        run_id,
        sequence_no,
        kind_json,
        payload_json,
        contract_refs_json,
        artifact_refs_json,
        parent_event_id,
        created_at_ms,
    ))?;
    Ok(format!(
        "{EVENT_ENVELOPE_HASH_PREFIX}{}",
        hash_bytes(&envelope)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::{ArtifactId, ArtifactKind, HarnessEventKind, HarnessEventPayload};

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
                );
                CREATE TABLE harness_artifacts (
                    id TEXT PRIMARY KEY NOT NULL,
                    run_id TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    relative_path TEXT NOT NULL,
                    sha256 TEXT NOT NULL,
                    size_bytes INTEGER NOT NULL,
                    tags_json TEXT NOT NULL,
                    created_by_event_id TEXT,
                    contract_refs_json TEXT NOT NULL,
                    created_at_ms INTEGER NOT NULL
                );",
            )
            .expect("create harness_events");
        (SqliteHarnessEventStore::new(connection.clone()), connection)
    }

    #[test]
    fn rejects_any_envelope_tampering_in_a_current_event() {
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
        assert!(error.to_string().contains("envelope hash mismatch"));

        connection
            .lock()
            .expect("sqlite mutex")
            .execute(
                "UPDATE harness_events
                 SET payload_json = ?1, kind = ?2
                 WHERE id = ?3",
                params![
                    serde_json::to_string(&event.payload).expect("payload json"),
                    serde_json::to_string(&HarnessEventKind::ToolFailed).expect("kind json"),
                    event.id.to_string(),
                ],
            )
            .expect("tamper non-payload envelope field");
        let error = store
            .list_events(event.run_id)
            .expect_err("kind tampering must fail integrity validation");
        assert!(error.to_string().contains("envelope hash mismatch"));
    }

    #[test]
    fn event_and_artifact_metadata_commit_as_one_capture_unit() {
        let (store, connection) = test_store();
        connection
            .lock()
            .expect("sqlite mutex")
            .execute_batch(
                "CREATE TRIGGER reject_artifact_capture
                 BEFORE INSERT ON harness_artifacts
                 BEGIN
                     SELECT RAISE(FAIL, 'artifact capture rejected');
                 END;",
            )
            .expect("install failure trigger");
        let event = HarnessEvent {
            id: HarnessEventId::new(),
            run_id: HarnessRunId::new(),
            sequence_no: 0,
            created_at_ms: 1,
            kind: HarnessEventKind::StateSnapshotRecorded,
            payload: HarnessEventPayload::generic(serde_json::json!({"state": "captured"})),
            contract_refs: Vec::new(),
            artifact_refs: Vec::new(),
            parent_event_id: None,
        };
        let artifact_id = ArtifactId::new();
        let mut event = event;
        event.artifact_refs.push(artifact_id);
        let artifact = ArtifactManifest {
            id: artifact_id,
            run_id: event.run_id,
            kind: ArtifactKind::StateSnapshot,
            relative_path: "events/state.json".into(),
            sha256: "hash".to_string(),
            size_bytes: 4,
            tags: Default::default(),
            created_by_event: Some(event.id),
            contract_refs: Vec::new(),
        };

        store
            .append_event_with_artifact(&event, Some(&artifact))
            .expect_err("artifact failure must roll back the event");
        let connection = connection.lock().expect("sqlite mutex");
        let event_count = connection
            .query_row("SELECT COUNT(*) FROM harness_events", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("event count");
        let artifact_count = connection
            .query_row("SELECT COUNT(*) FROM harness_artifacts", [], |row| {
                row.get::<_, i64>(0)
            })
            .expect("artifact count");
        assert_eq!((event_count, artifact_count), (0, 0));
    }
}
