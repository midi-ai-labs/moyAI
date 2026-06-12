use crate::error::RuntimeError;
use crate::protocol::{ProtocolEventStore, TurnId, project_protocol_run_event};
use crate::runtime::{RunEventSink, SessionRuntimeEventPublisher};
use crate::session::{RunEvent, SessionId};

pub struct ProtocolRecordingSink<'a, S: RunEventSink + ?Sized> {
    store: crate::protocol::SqliteProtocolEventStore,
    fallback_session_id: Option<SessionId>,
    turn_id: TurnId,
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
            next_sequence_no: 0,
            runtime_event_publisher: None,
            inner,
        }
    }

    pub fn with_runtime_event_publisher(mut self, publisher: SessionRuntimeEventPublisher) -> Self {
        self.runtime_event_publisher = Some(publisher);
        self
    }

    pub fn reserve_sequence_no(&mut self) -> i64 {
        let sequence_no = self.next_sequence_no;
        self.next_sequence_no += 1;
        sequence_no
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
        if let Some(projection) = project_protocol_run_event(
            &event,
            self.fallback_session_id,
            self.turn_id,
            self.next_sequence_no,
        ) {
            self.store
                .append_event_bundle(
                    &projection.runtime_event,
                    projection.history_item.as_ref(),
                    projection.turn_item.as_ref(),
                )
                .map_err(runtime_error)?;
            if let Some(publisher) = &self.runtime_event_publisher {
                publisher.publish(projection.runtime_event.clone())?;
            }
            self.next_sequence_no += 1;
        }
        self.inner.emit(event)
    }
}

fn runtime_error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Message(error.to_string())
}

pub(crate) fn pre_recorded_protocol_sequence_reservation_fixture_passes() -> bool {
    use crate::protocol::ProtocolEventStore;
    use crate::storage::{SqliteStore, StoragePaths};

    struct NoopSink;

    impl RunEventSink for NoopSink {
        fn emit(&mut self, _event: RunEvent) -> Result<(), RuntimeError> {
            Ok(())
        }
    }

    let unique = format!(
        "moyai-protocol-sequence-reservation-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or(0)
    );
    let root_path = std::env::temp_dir().join(unique);
    let Ok(data_dir) = camino::Utf8PathBuf::from_path_buf(root_path) else {
        return false;
    };
    let paths = StoragePaths {
        data_dir: data_dir.clone(),
        database_path: data_dir.join("moyai.sqlite3"),
        truncation_dir: data_dir.join("truncation"),
    };
    let result = (|| -> Result<bool, RuntimeError> {
        let store = SqliteStore::open(&paths).map_err(runtime_error)?;
        store.migrate().map_err(runtime_error)?;
        let protocol_store = store.protocol_event_store();
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let mut inner = NoopSink;
        let mut sink = ProtocolRecordingSink::new(
            protocol_store.clone(),
            Some(session_id),
            turn_id,
            &mut inner,
        );

        sink.emit(RunEvent::SessionStarted {
            session_id,
            title: "sequence reservation".to_string(),
        })?;

        let pre_recorded_event = RunEvent::SessionTitleUpdated {
            session_id,
            title: "pre-recorded title".to_string(),
        };
        let Some(sequence_no) = sink.reserve_protocol_sequence_no() else {
            return Ok(false);
        };
        let projection =
            project_protocol_run_event(&pre_recorded_event, Some(session_id), turn_id, sequence_no)
                .ok_or_else(|| {
                    RuntimeError::Message(
                        "pre-recorded event did not project to protocol".to_string(),
                    )
                })?;
        protocol_store
            .append_event_bundle(
                &projection.runtime_event,
                projection.history_item.as_ref(),
                projection.turn_item.as_ref(),
            )
            .map_err(runtime_error)?;

        sink.emit(RunEvent::SessionTitleUpdated {
            session_id,
            title: "side event before pre-recorded emit".to_string(),
        })?;
        sink.emit_pre_recorded(pre_recorded_event)?;

        let events = protocol_store
            .list_runtime_events(session_id, turn_id)
            .map_err(runtime_error)?;
        let sequences = events
            .iter()
            .map(|event| event.sequence_no)
            .collect::<Vec<_>>();
        Ok(sequences == vec![0, 1, 2])
    })();
    let _ = std::fs::remove_dir_all(data_dir.as_std_path());
    result.unwrap_or(false)
}
