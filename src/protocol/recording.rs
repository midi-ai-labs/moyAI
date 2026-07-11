use crate::error::RuntimeError;
use crate::protocol::{ProtocolEventStore, TurnId, project_protocol_run_event};
use crate::runtime::{RunEventSink, SessionRuntimeEventPublisher};
use crate::session::{RunEvent, SessionId};

pub struct ProtocolRecordingSink<'a, S: RunEventSink + ?Sized> {
    store: crate::protocol::SqliteProtocolEventStore,
    fallback_session_id: Option<SessionId>,
    turn_id: TurnId,
    admission_id: Option<String>,
    next_sequence_no: i64,
    runtime_event_publisher: Option<SessionRuntimeEventPublisher>,
    inner: &'a mut S,
}

impl<'a, S: RunEventSink + ?Sized> ProtocolRecordingSink<'a, S> {
    pub fn new(
        store: crate::protocol::SqliteProtocolEventStore,
        fallback_session_id: Option<SessionId>,
        turn_id: TurnId,
        inner: &'a mut S,
    ) -> Self {
        Self {
            store,
            fallback_session_id,
            turn_id,
            admission_id: None,
            next_sequence_no: 0,
            runtime_event_publisher: None,
            inner,
        }
    }

    pub fn with_runtime_event_publisher(mut self, publisher: SessionRuntimeEventPublisher) -> Self {
        self.runtime_event_publisher = Some(publisher);
        self
    }

    pub fn with_admission_id(mut self, admission_id: impl Into<String>) -> Self {
        self.admission_id = Some(admission_id.into());
        self
    }

    pub fn reserve_sequence_no(&mut self) -> i64 {
        self.sync_next_sequence_no_from_store();
        let sequence_no = self.next_sequence_no;
        self.next_sequence_no += 1;
        sequence_no
    }

    fn sync_next_sequence_no_from_store(&mut self) {
        let Some(session_id) = self.fallback_session_id else {
            return;
        };
        let Ok(Some((turn_id, next_sequence_no))) =
            self.store.latest_turn_position_for_session(session_id)
        else {
            return;
        };
        if turn_id == self.turn_id && next_sequence_no > self.next_sequence_no {
            self.next_sequence_no = next_sequence_no;
        }
    }
}

impl<S: RunEventSink + ?Sized> RunEventSink for ProtocolRecordingSink<'_, S> {
    fn reserve_protocol_sequence_no(&mut self) -> Option<i64> {
        Some(self.reserve_sequence_no())
    }

    fn emit_pre_recorded(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
        self.inner.emit(event)
    }

    fn emit(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
        self.sync_next_sequence_no_from_store();
        if let Some(projection) = project_protocol_run_event(
            &event,
            self.fallback_session_id,
            self.turn_id,
            self.next_sequence_no,
        ) {
            let stored = if let Some(admission_id) = &self.admission_id {
                self.store
                    .append_admitted_event_bundle_allocating(
                        admission_id,
                        &projection.runtime_event,
                        projection.history_item.as_ref(),
                        projection.turn_item.as_ref(),
                    )
                    .map_err(runtime_error)?
                    .ok_or_else(|| {
                        RuntimeError::Message(format!(
                            "run admission {admission_id} no longer owns protocol turn {}",
                            self.turn_id
                        ))
                    })?
            } else {
                self.store
                    .append_event_bundle_allocating(
                        &projection.runtime_event,
                        projection.history_item.as_ref(),
                        projection.turn_item.as_ref(),
                    )
                    .map_err(runtime_error)?
            };
            if let Some(publisher) = &self.runtime_event_publisher {
                publisher.publish(stored.runtime_event.clone())?;
            }
            self.next_sequence_no = stored.runtime_event.sequence_no.saturating_add(1);
        }
        self.inner.emit(event)
    }
}

fn runtime_error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Message(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::StorageError;
    use crate::protocol::{RuntimeEvent, RuntimeEventId, RuntimeEventMsg};
    use crate::runtime::SystemClock;
    use crate::storage::{SqliteStore, StoragePaths};

    struct NullSink;

    impl RunEventSink for NullSink {
        fn emit(&mut self, _event: RunEvent) -> Result<(), RuntimeError> {
            Ok(())
        }
    }

    #[test]
    fn sequence_reservation_catches_up_to_external_turn_writes() -> Result<(), StorageError> {
        let temp = tempfile::tempdir()?;
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data"))
            .expect("temp path should be utf8");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let store = SqliteStore::open(&paths)?;
        store.migrate()?;
        let event_store = store.protocol_event_store();
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        event_store.append_runtime_event(&RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no: 0,
            created_at_ms: SystemClock::now_ms(),
            msg: RuntimeEventMsg::Warning {
                message: "external cancellation marker".to_string(),
            },
        })?;

        let mut inner = NullSink;
        let mut sink =
            ProtocolRecordingSink::new(event_store, Some(session_id), turn_id, &mut inner);

        assert_eq!(sink.reserve_sequence_no(), 1);
        Ok(())
    }
}
