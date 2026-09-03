use crate::error::{RuntimeError, StorageError};
use crate::harness::SqliteHarnessRunStore;
use crate::protocol::{
    MAX_PROTOCOL_PAGE_LIMIT, ProtocolEventStore, RuntimeEvent, RuntimeEventMsg,
    SqliteProtocolEventStore, TurnId, project_protocol_run_event,
};
use crate::runtime::event_bus::validate_run_event_durability;
use crate::runtime::{RunEventSink, SessionRuntimeEventPublisher};
use crate::session::{AdmissionId, RunEvent, RunEventDurability, SessionId};

/// Replays committed protocol events into process-local observers.
///
/// SQLite is the only semantic owner. Harness and live UI delivery are
/// idempotent projections of that canonical stream and can never reverse a
/// committed turn outcome.
#[derive(Clone)]
pub struct CanonicalRuntimeEventProjector {
    protocol_store: SqliteProtocolEventStore,
    harness_store: SqliteHarnessRunStore,
    publisher: SessionRuntimeEventPublisher,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CanonicalRuntimeProjectionReport {
    pub event_count: usize,
    pub harness_projection_errors: Vec<String>,
    pub live_publish_errors: Vec<String>,
}

impl CanonicalRuntimeProjectionReport {
    pub fn is_clean(&self) -> bool {
        self.harness_projection_errors.is_empty() && self.live_publish_errors.is_empty()
    }

    pub fn log_failures(&self, context: &str) {
        for error in &self.harness_projection_errors {
            eprintln!(
                "warning: {context}: committed terminal could not reach the durable harness projection: {error}"
            );
        }
        for error in &self.live_publish_errors {
            eprintln!(
                "warning: {context}: committed runtime event could not reach live subscribers: {error}"
            );
        }
    }
}

impl CanonicalRuntimeEventProjector {
    pub fn new(
        protocol_store: SqliteProtocolEventStore,
        harness_store: SqliteHarnessRunStore,
        publisher: SessionRuntimeEventPublisher,
    ) -> Self {
        Self {
            protocol_store,
            harness_store,
            publisher,
        }
    }

    pub fn latest_cursor(&self, session_id: SessionId) -> Result<Option<i64>, StorageError> {
        Ok(self
            .protocol_store
            .latest_runtime_event_page_for_session(session_id, 1)?
            .next_cursor)
    }

    pub fn project_after_cursor(
        &self,
        session_id: SessionId,
        mut cursor: Option<i64>,
    ) -> Result<CanonicalRuntimeProjectionReport, StorageError> {
        let mut report = CanonicalRuntimeProjectionReport::default();
        loop {
            let page = self.protocol_store.runtime_event_cursor_page_for_session(
                session_id,
                cursor,
                MAX_PROTOCOL_PAGE_LIMIT,
            )?;
            if page.items.is_empty() {
                break;
            }
            let page_len = page.items.len();
            for event in page.items {
                report.merge(self.project_event(&event));
            }
            let next_cursor = page.next_cursor;
            if page_len < MAX_PROTOCOL_PAGE_LIMIT {
                break;
            }
            if next_cursor == cursor {
                return Err(StorageError::Message(format!(
                    "canonical runtime projection cursor did not advance for session {session_id}"
                )));
            }
            cursor = next_cursor;
        }
        Ok(report)
    }

    pub fn project_event(&self, event: &RuntimeEvent) -> CanonicalRuntimeProjectionReport {
        let mut report = CanonicalRuntimeProjectionReport {
            event_count: 1,
            ..Default::default()
        };
        if matches!(event.msg, RuntimeEventMsg::TurnTerminal { .. })
            && let Err(error) = self.harness_store.project_canonical_terminal_event(event)
        {
            report.harness_projection_errors.push(error.to_string());
        }
        if let Err(error) = self.publisher.publish(event.clone()) {
            report.live_publish_errors.push(error.to_string());
        }
        report
    }
}

impl CanonicalRuntimeProjectionReport {
    fn merge(&mut self, mut other: Self) {
        self.event_count = self.event_count.saturating_add(other.event_count);
        self.harness_projection_errors
            .append(&mut other.harness_projection_errors);
        self.live_publish_errors
            .append(&mut other.live_publish_errors);
    }
}

pub struct ProtocolRecordingSink<'a, S: RunEventSink + ?Sized> {
    store: crate::protocol::SqliteProtocolEventStore,
    fallback_session_id: Option<SessionId>,
    turn_id: TurnId,
    admission_id: Option<AdmissionId>,
    next_sequence_no: i64,
    published_runtime_sequence_no: Option<i64>,
    committed_terminal_fanout_attempted: bool,
    runtime_event_publisher: Option<SessionRuntimeEventPublisher>,
    canonical_runtime_projector: Option<CanonicalRuntimeEventProjector>,
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
            published_runtime_sequence_no: None,
            committed_terminal_fanout_attempted: false,
            runtime_event_publisher: None,
            canonical_runtime_projector: None,
            inner,
        }
    }

    pub fn with_runtime_event_publisher(mut self, publisher: SessionRuntimeEventPublisher) -> Self {
        self.runtime_event_publisher = Some(publisher);
        self
    }

    pub fn with_canonical_runtime_projector(
        mut self,
        projector: CanonicalRuntimeEventProjector,
    ) -> Self {
        self.canonical_runtime_projector = Some(projector);
        self
    }

    pub fn with_admission_id(mut self, admission_id: AdmissionId) -> Self {
        self.admission_id = Some(admission_id.into());
        self
    }

    pub fn reserve_sequence_no(&mut self) -> i64 {
        self.sync_next_sequence_no_from_store();
        let sequence_no = self.next_sequence_no;
        self.next_sequence_no += 1;
        sequence_no
    }

    pub const fn committed_terminal_fanout_attempted(&self) -> bool {
        self.committed_terminal_fanout_attempted
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

    fn publish_committed_runtime_events(&mut self) -> Result<(), RuntimeError> {
        let Some(publisher) = self.runtime_event_publisher.clone() else {
            return Ok(());
        };
        let Some(session_id) = self.fallback_session_id else {
            return Ok(());
        };
        loop {
            let events = self
                .store
                .runtime_event_page_for_turn_after_sequence(
                    session_id,
                    self.turn_id,
                    self.published_runtime_sequence_no,
                    crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
                )
                .map_err(runtime_error)?;
            if events.is_empty() {
                break;
            }
            let page_len = events.len();
            for event in events {
                let sequence_no = event.sequence_no;
                self.next_sequence_no = self.next_sequence_no.max(sequence_no.saturating_add(1));
                if let Some(projector) = self.canonical_runtime_projector.as_ref() {
                    projector
                        .project_event(&event)
                        .log_failures("normal admitted-run fanout");
                } else {
                    publisher.publish(event)?;
                }
                self.published_runtime_sequence_no = Some(
                    self.published_runtime_sequence_no
                        .map_or(sequence_no, |current| current.max(sequence_no)),
                );
            }
            if page_len < crate::protocol::MAX_PROTOCOL_PAGE_LIMIT {
                break;
            }
        }
        Ok(())
    }
}

impl<S: RunEventSink + ?Sized> RunEventSink for ProtocolRecordingSink<'_, S> {
    fn reserve_protocol_sequence_no(&mut self) -> Option<i64> {
        Some(self.reserve_sequence_no())
    }

    fn emit_committed(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
        validate_run_event_durability(
            &event,
            RunEventDurability::Committed,
            "ProtocolRecordingSink::emit_committed",
        )?;
        if matches!(event, RunEvent::TurnTerminal { .. }) {
            // Let the native recorder flush its buffered deltas and atomically project the exact
            // canonical terminal before a fallible renderer runs. The shared projector then
            // replays the same event idempotently to the live hub even when rendering fails.
            self.committed_terminal_fanout_attempted = true;
            let inner_result = self.inner.emit_committed(event);
            if let Err(error) = self.publish_committed_runtime_events() {
                eprintln!(
                    "warning: committed terminal could not be replayed to every observer: {error}"
                );
            }
            return inner_result;
        }
        self.publish_committed_runtime_events()?;
        self.inner.emit_committed(event)
    }

    fn emit_runtime_only(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
        validate_run_event_durability(
            &event,
            RunEventDurability::RuntimeOnly,
            "ProtocolRecordingSink::emit_runtime_only",
        )?;
        self.inner.emit_runtime_only(event)
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
                    .append_admitted_recording_projection_allocating(
                        *admission_id,
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
                    .append_recording_projection_allocating(
                        &projection.runtime_event,
                        projection.history_item.as_ref(),
                        projection.turn_item.as_ref(),
                    )
                    .map_err(runtime_error)?
            };
            self.next_sequence_no = stored.runtime_event.sequence_no.saturating_add(1);
            self.publish_committed_runtime_events()?;
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
    use crate::config::AccessMode;
    use crate::error::StorageError;
    use crate::protocol::{RuntimeEvent, RuntimeEventId, RuntimeEventMsg};
    use crate::runtime::SystemClock;
    use crate::session::{NewSession, ProjectId, ProjectRepository, SessionRepository};
    use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

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
        event_store.seed_runtime_event_for_test(&RuntimeEvent {
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

    #[test]
    fn explicit_sink_paths_reject_run_event_durability_mismatches() -> Result<(), StorageError> {
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
        let mut inner = NullSink;
        let mut sink = ProtocolRecordingSink::new(
            store.protocol_event_store(),
            Some(SessionId::new()),
            TurnId::new(),
            &mut inner,
        );

        let committed_on_runtime_path = sink
            .emit_runtime_only(RunEvent::RecoverableRuntimeFeedback {
                session_id: SessionId::new(),
                feedback: crate::session::DurableRuntimeFeedback::new(
                    crate::session::DurableFeedbackSeverity::Warning,
                    crate::session::DurableFeedbackCategory::Runtime,
                    "canonical warning",
                ),
            })
            .expect_err("committed facts cannot enter the lossy runtime-only path");
        assert!(
            committed_on_runtime_path
                .to_string()
                .contains("RuntimeOnly")
        );
        assert!(committed_on_runtime_path.to_string().contains("Committed"));

        let runtime_on_committed_path = sink
            .emit_committed(RunEvent::TextDelta {
                response_id: crate::protocol::ModelResponseId::new(),
                delta: "ephemeral".to_string(),
            })
            .expect_err("runtime telemetry cannot claim an existing durable commit");
        assert!(runtime_on_committed_path.to_string().contains("Committed"));
        assert!(
            runtime_on_committed_path
                .to_string()
                .contains("RuntimeOnly")
        );
        Ok(())
    }

    #[test]
    fn emit_rejects_events_owned_by_atomic_session_transactions() -> Result<(), StorageError> {
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
        let mut inner = NullSink;
        let mut sink =
            ProtocolRecordingSink::new(event_store.clone(), Some(session_id), turn_id, &mut inner);

        let error = sink
            .emit(RunEvent::AssistantMessageCommitted {
                response_id: crate::protocol::ModelResponseId::new(),
                text: "must already be committed by the model-response owner".to_string(),
            })
            .expect_err("recording sink must reject an uncommitted assistant projection");
        assert!(error.to_string().contains("atomic state owner"));
        assert!(
            event_store
                .list_runtime_events(session_id, turn_id)?
                .is_empty()
        );
        assert!(
            event_store
                .list_history_items(session_id, turn_id)?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn admitted_emit_records_one_coherent_durable_feedback_bundle() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data"))
            .expect("temp path should be utf8");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let sqlite = SqliteStore::open(&paths).expect("store");
        sqlite.migrate().expect("migrate");
        let store = StoreBundle::new(sqlite);
        let project_id = ProjectId::new();
        store
            .project_repo()
            .upsert_project(project_id, &data_dir, "test", "none")
            .await
            .expect("project");
        let session = store
            .session_repo()
            .create_session(NewSession {
                project_id,
                title: "test".to_string(),
                cwd: data_dir,
                model: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                access_mode: AccessMode::Default,
                provider_connection: None,
            })
            .await
            .expect("session");
        let session_id = session.id;
        let turn_id = TurnId::new();
        let admission_id = store
            .session_repo()
            .admit_session_turn(session_id, turn_id)
            .await
            .expect("admission")
            .expect("admitted")
            .admission_id;
        let event_store = store.protocol_event_store();
        let feedback = crate::session::DurableRuntimeFeedback::new(
            crate::session::DurableFeedbackSeverity::Warning,
            crate::session::DurableFeedbackCategory::Context,
            "semantic compaction remained unavailable",
        );
        let mut inner = NullSink;
        let mut sink =
            ProtocolRecordingSink::new(event_store.clone(), Some(session_id), turn_id, &mut inner)
                .with_admission_id(admission_id);

        sink.emit(RunEvent::RecoverableRuntimeFeedback {
            session_id,
            feedback: feedback.clone(),
        })
        .expect("durable feedback projection");

        let runtime_feedback = event_store
            .list_runtime_events(session_id, turn_id)
            .expect("runtime events")
            .into_iter()
            .filter_map(|event| match event.msg {
                RuntimeEventMsg::DurableFeedback { feedback } => Some(feedback),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(runtime_feedback, [feedback.clone()]);

        let history_feedback = event_store
            .list_history_items(session_id, turn_id)
            .expect("history items")
            .into_iter()
            .filter_map(|item| match item.payload {
                crate::protocol::HistoryItemPayload::DurableFeedback { feedback } => {
                    Some((item.id, feedback))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(history_feedback.len(), 1);
        assert_eq!(history_feedback[0].1, feedback);

        let turn_feedback = event_store
            .list_turn_items(session_id, turn_id)
            .expect("turn items")
            .into_iter()
            .filter_map(|item| match item.payload {
                crate::protocol::TurnItemPayload::DurableFeedback { feedback } => {
                    Some((item.source_item_id, feedback))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(turn_feedback, [(Some(history_feedback[0].0), feedback)]);
    }

    #[tokio::test]
    async fn committed_events_are_published_while_runtime_only_deltas_are_not_persisted() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data"))
            .expect("temp path should be utf8");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");
        let event_store = store.protocol_event_store();
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let committed = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no: 0,
            created_at_ms: SystemClock::now_ms(),
            msg: RuntimeEventMsg::Warning {
                message: "committed".to_string(),
            },
        };
        event_store
            .seed_runtime_event_for_test(&committed)
            .expect("committed runtime event");

        let hub = crate::runtime::SessionRuntimeEventHub::new(8);
        let mut subscription = hub.subscribe(session_id);
        let mut inner = NullSink;
        let mut sink =
            ProtocolRecordingSink::new(event_store.clone(), Some(session_id), turn_id, &mut inner)
                .with_runtime_event_publisher(hub.publisher());
        sink.emit_committed(RunEvent::RecoverableRuntimeFeedback {
            session_id,
            feedback: crate::session::DurableRuntimeFeedback::new(
                crate::session::DurableFeedbackSeverity::Warning,
                crate::session::DurableFeedbackCategory::Runtime,
                "committed",
            ),
        })
        .expect("publish committed event");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), subscription.recv())
                .await
                .expect("committed event timeout")
                .expect("committed event")
                .id,
            committed.id
        );
        sink.emit_committed(RunEvent::RecoverableRuntimeFeedback {
            session_id,
            feedback: crate::session::DurableRuntimeFeedback::new(
                crate::session::DurableFeedbackSeverity::Warning,
                crate::session::DurableFeedbackCategory::Runtime,
                "projection retry without a new durable row",
            ),
        })
        .expect("retry committed projection");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), subscription.recv())
                .await
                .is_err(),
            "an already published durable sequence must not be replayed"
        );

        sink.emit_runtime_only(RunEvent::TextDelta {
            response_id: crate::protocol::ModelResponseId::new(),
            delta: "live only".to_string(),
        })
        .expect("runtime-only delta");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), subscription.recv())
                .await
                .is_err(),
            "runtime-only delta must not synthesize or republish a durable runtime event"
        );
        assert_eq!(
            event_store
                .list_runtime_events(session_id, turn_id)
                .expect("stored events")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn committed_terminal_reaches_live_subscribers_even_when_the_renderer_fails() {
        struct RejectingSink;

        impl RunEventSink for RejectingSink {
            fn emit(&mut self, _event: RunEvent) -> Result<(), RuntimeError> {
                Err(RuntimeError::Message(
                    "injected renderer failure".to_string(),
                ))
            }
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = camino::Utf8PathBuf::from_path_buf(temp.path().join("data"))
            .expect("temp path should be utf8");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let store = SqliteStore::open(&paths).expect("store");
        store.migrate().expect("migrate");
        let event_store = store.protocol_event_store();
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let terminal = crate::session::DurableTurnTerminal {
            outcome: crate::protocol::TurnTerminalOutcome::Interrupted {
                cause: crate::protocol::TurnInterruptionCause::UserStop,
            },
            final_response_id: None,
            tool_call_count: 4,
            failed_tool_count: 1,
            change_count: 2,
            metrics: Default::default(),
        };
        let committed = RuntimeEvent {
            id: RuntimeEventId::new(),
            session_id,
            turn_id,
            sequence_no: 0,
            created_at_ms: SystemClock::now_ms(),
            msg: RuntimeEventMsg::TurnTerminal {
                terminal: Box::new(terminal.clone()),
            },
        };
        event_store
            .seed_runtime_event_for_test(&committed)
            .expect("committed terminal");

        let hub = crate::runtime::SessionRuntimeEventHub::new(8);
        let mut subscription = hub.subscribe(session_id);
        let mut inner = RejectingSink;
        let mut sink =
            ProtocolRecordingSink::new(event_store, Some(session_id), turn_id, &mut inner)
                .with_runtime_event_publisher(hub.publisher());

        let error = sink
            .emit_committed(RunEvent::TurnTerminal {
                session_id,
                terminal: Box::new(terminal),
            })
            .expect_err("renderer failure remains visible to its caller");
        assert!(error.to_string().contains("renderer failure"));
        assert!(sink.committed_terminal_fanout_attempted());
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), subscription.recv())
                .await
                .expect("committed terminal timeout")
                .expect("committed terminal")
                .id,
            committed.id
        );
    }
}
