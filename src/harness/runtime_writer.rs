use std::collections::BTreeSet;

use camino::Utf8PathBuf;

use crate::error::RuntimeError;
use crate::harness::{
    ArtifactId, ArtifactKind, ArtifactManifest, ArtifactStore, ArtifactTag, ContractRef,
    HarnessEvent, HarnessEventId, HarnessEventKind, HarnessEventPayload, HarnessEventStore,
    HarnessRunId, HarnessRunRecord, HarnessRunStatus, HarnessRunStore, SqliteArtifactStore,
    SqliteHarnessEventStore, SqliteHarnessRunStore, artifact::hash_bytes,
};
use crate::protocol::TurnId;
use crate::runtime::{RunEventSink, SystemClock};
use crate::session::{RunEvent, SessionId};
use crate::storage::StoreBundle;

pub struct NativeHarnessRecorder {
    run_id: HarnessRunId,
    session_id: Option<SessionId>,
    run_store: SqliteHarnessRunStore,
    event_store: SqliteHarnessEventStore,
    artifact_store: SqliteArtifactStore,
    workspace_root: Utf8PathBuf,
    artifact_root: Utf8PathBuf,
    started_at_ms: i64,
    next_sequence_no: i64,
    protocol_turn_id: TurnId,
}

impl NativeHarnessRecorder {
    pub fn start(
        store: &StoreBundle,
        session_id: Option<SessionId>,
        workspace_root: Utf8PathBuf,
    ) -> Result<Self, RuntimeError> {
        Self::start_harness_only(store, session_id, workspace_root)
    }

    pub fn start_harness_only(
        store: &StoreBundle,
        session_id: Option<SessionId>,
        workspace_root: Utf8PathBuf,
    ) -> Result<Self, RuntimeError> {
        Self::start_harness_only_for_turn(store, session_id, workspace_root, TurnId::new())
    }

    pub fn start_harness_only_for_turn(
        store: &StoreBundle,
        session_id: Option<SessionId>,
        workspace_root: Utf8PathBuf,
        protocol_turn_id: TurnId,
    ) -> Result<Self, RuntimeError> {
        let run_id = HarnessRunId::new();
        let artifact_root = store
            .paths()
            .data_dir
            .join("harness")
            .join(run_id.to_string());
        let run_store = store.harness_run_store();
        let event_store = store.harness_event_store();
        let artifact_store = store.harness_artifact_store();
        let started_at_ms = SystemClock::now_ms();
        std::fs::create_dir_all(artifact_root.as_std_path()).map_err(runtime_error)?;
        run_store
            .upsert_run(&HarnessRunRecord {
                id: run_id,
                session_id,
                workspace_root: workspace_root.clone(),
                artifact_root: artifact_root.clone(),
                mode: "native_runtime".to_string(),
                started_at_ms,
                completed_at_ms: None,
                status: HarnessRunStatus::Started,
            })
            .map_err(runtime_error)?;
        Ok(Self {
            run_id,
            session_id,
            run_store,
            event_store,
            artifact_store,
            workspace_root,
            artifact_root,
            started_at_ms,
            next_sequence_no: 0,
            protocol_turn_id,
        })
    }

    pub fn run_id(&self) -> HarnessRunId {
        self.run_id
    }

    pub fn protocol_turn_id(&self) -> TurnId {
        self.protocol_turn_id
    }

    pub fn record_run_event(&mut self, event: &RunEvent) -> Result<(), RuntimeError> {
        let kind = harness_kind_for_run_event(event);
        let payload = payload_for_run_event(event, &self.workspace_root, &self.artifact_root)?;
        self.append(kind, payload)?;
        if let Some(status) = terminal_status_for_run_event(event) {
            self.run_store
                .upsert_run(&HarnessRunRecord {
                    id: self.run_id,
                    session_id: self.session_id,
                    workspace_root: self.workspace_root.clone(),
                    artifact_root: self.artifact_root.clone(),
                    mode: "native_runtime".to_string(),
                    started_at_ms: self.started_at_ms,
                    completed_at_ms: Some(SystemClock::now_ms()),
                    status,
                })
                .map_err(runtime_error)?;
        }
        Ok(())
    }

    fn append(
        &mut self,
        kind: HarnessEventKind,
        payload: HarnessEventPayload,
    ) -> Result<(), RuntimeError> {
        let event_id = HarnessEventId::new();
        let contract_refs = Vec::new();
        let artifact_refs =
            self.record_payload_artifact(event_id, kind, &payload, &contract_refs)?;
        let event = HarnessEvent {
            id: event_id,
            run_id: self.run_id,
            sequence_no: self.next_sequence_no,
            created_at_ms: SystemClock::now_ms(),
            kind,
            payload,
            contract_refs,
            artifact_refs,
            parent_event_id: None,
        };
        self.event_store
            .append_event(&event)
            .map_err(runtime_error)?;
        self.next_sequence_no += 1;
        Ok(())
    }

    fn record_payload_artifact(
        &self,
        event_id: HarnessEventId,
        kind: HarnessEventKind,
        payload: &HarnessEventPayload,
        contract_refs: &[ContractRef],
    ) -> Result<Vec<ArtifactId>, RuntimeError> {
        let Some(artifact_kind) = artifact_kind_for_event(kind) else {
            return Ok(Vec::new());
        };
        let payload_json = serde_json::to_string_pretty(payload).map_err(runtime_error)?;
        let relative_path = Utf8PathBuf::from(format!(
            "events/{:06}_{}.json",
            self.next_sequence_no,
            event_kind_file_label(kind)
        ));
        let full_path = self.artifact_root.join(&relative_path);
        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent.as_std_path()).map_err(runtime_error)?;
        }
        std::fs::write(full_path.as_std_path(), payload_json.as_bytes()).map_err(runtime_error)?;
        let artifact_id = ArtifactId::new();
        let mut tags = BTreeSet::new();
        match artifact_kind {
            ArtifactKind::StateSnapshot => {
                tags.insert(ArtifactTag::StateSnapshot);
            }
            ArtifactKind::RequestDiagnostics => {
                tags.insert(ArtifactTag::Diagnostics);
            }
            ArtifactKind::VerificationLog => {
                tags.insert(ArtifactTag::Verification);
            }
            _ => {}
        }
        self.artifact_store
            .insert_artifact(&ArtifactManifest {
                id: artifact_id,
                run_id: self.run_id,
                kind: artifact_kind,
                relative_path,
                sha256: hash_bytes(payload_json.as_bytes()),
                size_bytes: payload_json.len() as u64,
                tags,
                created_by_event: Some(event_id),
                contract_refs: contract_refs.to_vec(),
            })
            .map_err(runtime_error)?;
        Ok(vec![artifact_id])
    }
}

pub struct HarnessRecordingSink<'a, S: RunEventSink + ?Sized> {
    recorder: NativeHarnessRecorder,
    inner: &'a mut S,
}

impl<'a, S: RunEventSink + ?Sized> HarnessRecordingSink<'a, S> {
    pub fn new(recorder: NativeHarnessRecorder, inner: &'a mut S) -> Self {
        Self { recorder, inner }
    }

    pub fn run_id(&self) -> HarnessRunId {
        self.recorder.run_id()
    }
}

impl<S: RunEventSink + ?Sized> RunEventSink for HarnessRecordingSink<'_, S> {
    fn reserve_protocol_sequence_no(&mut self) -> Option<i64> {
        self.inner.reserve_protocol_sequence_no()
    }

    fn emit(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
        self.recorder.record_run_event(&event)?;
        self.inner.emit(event)
    }
}

fn harness_kind_for_run_event(event: &RunEvent) -> HarnessEventKind {
    match event {
        RunEvent::SessionStarted { .. } => HarnessEventKind::RunStarted,
        RunEvent::SessionTitleUpdated { .. } => HarnessEventKind::StateSnapshotRecorded,
        RunEvent::UserMessageStored { .. } | RunEvent::UserTurnStored { .. } => {
            HarnessEventKind::UserTurnAccepted
        }
        RunEvent::AssistantStarted { .. } => HarnessEventKind::ModelProjectionBuilt,
        RunEvent::ControlEnvelopePrepared { .. } => HarnessEventKind::ControlEnvelopePrepared,
        RunEvent::WorldStateUpdated { .. } => HarnessEventKind::StateSnapshotRecorded,
        RunEvent::ModelRequestPrepared { .. } => HarnessEventKind::ModelRequestSent,
        RunEvent::TextDelta { .. } | RunEvent::ReasoningDelta { .. } => {
            HarnessEventKind::ModelResponseReceived
        }
        RunEvent::ToolCallPending { .. } => HarnessEventKind::ToolDispatchRequested,
        RunEvent::ToolCallCompleted { .. } => HarnessEventKind::ToolExecuted,
        RunEvent::ToolCallDeclined { .. } => HarnessEventKind::ToolDeclined,
        RunEvent::ToolCallCancelled { .. } => HarnessEventKind::ToolCancelled,
        RunEvent::ToolCallFailed { .. } => HarnessEventKind::ToolFailed,
        RunEvent::ToolProposalRejected { .. } => HarnessEventKind::ToolDispatchDenied,
        RunEvent::CandidateRepairEditRecorded { .. } => HarnessEventKind::StateSnapshotRecorded,
        RunEvent::FileChangesRecorded { .. } => HarnessEventKind::ArtifactRegistered,
        RunEvent::CompactionCompleted { .. } => HarnessEventKind::StateSnapshotRecorded,
        RunEvent::PermissionRequested { .. } => HarnessEventKind::PermissionRequested,
        RunEvent::PermissionResolved { .. } => HarnessEventKind::PermissionResolved,
        RunEvent::RetryScheduled { .. } | RunEvent::RecoverableRuntimeFeedback { .. } => {
            HarnessEventKind::CorrectiveResultEmitted
        }
        RunEvent::StateUpdated { .. } | RunEvent::LifecycleGuardUpdated { .. } => {
            HarnessEventKind::StateSnapshotRecorded
        }
        RunEvent::SessionCompleted { .. }
        | RunEvent::SessionAwaitingUser { .. }
        | RunEvent::SessionInterrupted { .. }
        | RunEvent::SessionFailed { .. } => HarnessEventKind::RunTerminalized,
    }
}

fn payload_for_run_event(
    event: &RunEvent,
    workspace_root: &Utf8PathBuf,
    artifact_root: &Utf8PathBuf,
) -> Result<HarnessEventPayload, RuntimeError> {
    if matches!(event, RunEvent::SessionStarted { .. }) {
        return Ok(HarnessEventPayload::RunStarted {
            workspace_root: workspace_root.to_string(),
            artifact_root: artifact_root.to_string(),
            mode: "native_runtime".to_string(),
        });
    }
    serde_json::to_value(event)
        .map(HarnessEventPayload::generic)
        .map_err(runtime_error)
}

fn artifact_kind_for_event(kind: HarnessEventKind) -> Option<ArtifactKind> {
    match kind {
        HarnessEventKind::StateSnapshotRecorded | HarnessEventKind::StateTransitionRecorded => {
            Some(ArtifactKind::StateSnapshot)
        }
        HarnessEventKind::ControlEnvelopePrepared
        | HarnessEventKind::ModelProjectionBuilt
        | HarnessEventKind::ModelRequestSent => Some(ArtifactKind::RequestDiagnostics),
        HarnessEventKind::ToolDispatchDenied
        | HarnessEventKind::ToolDeclined
        | HarnessEventKind::ToolCancelled
        | HarnessEventKind::ToolFailed
        | HarnessEventKind::PermissionRequested
        | HarnessEventKind::PermissionResolved
        | HarnessEventKind::ToolExecuted
        | HarnessEventKind::ToolResultNormalized
        | HarnessEventKind::CorrectiveResultEmitted => Some(ArtifactKind::VerificationLog),
        HarnessEventKind::RunTerminalized => Some(ArtifactKind::ReplayReport),
        _ => None,
    }
}

fn event_kind_file_label(kind: HarnessEventKind) -> &'static str {
    match kind {
        HarnessEventKind::RunStarted => "run_started",
        HarnessEventKind::ProviderPreflightChecked => "provider_preflight_checked",
        HarnessEventKind::UserTurnAccepted => "user_turn_accepted",
        HarnessEventKind::AttachmentRegistered => "attachment_registered",
        HarnessEventKind::ContractVersionSelected => "contract_version_selected",
        HarnessEventKind::StateSnapshotRecorded => "state_snapshot_recorded",
        HarnessEventKind::ActiveWorkContractSelected => "active_work_contract_selected",
        HarnessEventKind::ControlEnvelopePrepared => "turn_control_envelope",
        HarnessEventKind::ModelProjectionBuilt => "model_projection_built",
        HarnessEventKind::ModelRequestSent => "model_request_sent",
        HarnessEventKind::ModelResponseReceived => "model_response_received",
        HarnessEventKind::ModelNoToolStop => "model_no_tool_stop",
        HarnessEventKind::ToolDispatchRequested => "tool_dispatch_requested",
        HarnessEventKind::ToolDispatchDenied => "tool_dispatch_denied",
        HarnessEventKind::ToolDeclined => "tool_declined",
        HarnessEventKind::ToolCancelled => "tool_cancelled",
        HarnessEventKind::ToolFailed => "tool_failed",
        HarnessEventKind::PermissionRequested => "permission_requested",
        HarnessEventKind::PermissionResolved => "permission_resolved",
        HarnessEventKind::ToolExecuted => "tool_executed",
        HarnessEventKind::ToolResultNormalized => "tool_result_normalized",
        HarnessEventKind::CorrectiveResultEmitted => "corrective_result_emitted",
        HarnessEventKind::StateTransitionRecorded => "state_transition_recorded",
        HarnessEventKind::ArtifactRegistered => "artifact_registered",
        HarnessEventKind::QualityGateEvaluated => "quality_gate_evaluated",
        HarnessEventKind::ScenarioGateEvaluated => "scenario_gate_evaluated",
        HarnessEventKind::RunTerminalized => "run_terminalized",
    }
}

fn terminal_status_for_run_event(event: &RunEvent) -> Option<HarnessRunStatus> {
    match event {
        RunEvent::SessionCompleted { .. } => Some(HarnessRunStatus::Pass),
        RunEvent::SessionAwaitingUser { .. } => Some(HarnessRunStatus::Blocked),
        RunEvent::SessionInterrupted { .. } => Some(HarnessRunStatus::Blocked),
        RunEvent::SessionFailed { .. } => Some(HarnessRunStatus::Fail),
        _ => None,
    }
}

fn runtime_error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Message(format!("native harness event writer failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HarnessEventStore;
    use crate::session::{MessageId, ToolCallId};
    use crate::storage::{SqliteStore, StoragePaths};
    use crate::tool::ToolName;

    #[test]
    fn tool_terminal_outcomes_keep_distinct_harness_kinds() {
        let call_id = ToolCallId::new();
        let metadata = serde_json::Value::Null;
        let cases = [
            (
                RunEvent::ToolCallDeclined {
                    tool_call_id: call_id,
                    tool: ToolName::Shell,
                    reason: "user declined".to_string(),
                    metadata: metadata.clone(),
                },
                HarnessEventKind::ToolDeclined,
            ),
            (
                RunEvent::ToolCallCancelled {
                    tool_call_id: call_id,
                    tool: ToolName::Shell,
                    reason: "tree stopped".to_string(),
                    metadata: metadata.clone(),
                },
                HarnessEventKind::ToolCancelled,
            ),
            (
                RunEvent::ToolCallFailed {
                    tool_call_id: call_id,
                    tool: ToolName::Shell,
                    error: "transport failed".to_string(),
                    metadata,
                },
                HarnessEventKind::ToolFailed,
            ),
        ];

        for (event, expected) in cases {
            assert_eq!(harness_kind_for_run_event(&event), expected);
        }
    }

    #[test]
    fn records_only_the_runtime_event_without_synthetic_contracts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 path");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir,
        };
        let store = SqliteStore::open(&paths).expect("open store");
        store.migrate().expect("migrate store");
        let bundle = StoreBundle::new(store);
        let workspace = Utf8PathBuf::from("C:/workspace");
        let mut recorder =
            NativeHarnessRecorder::start_harness_only(&bundle, None, workspace).expect("recorder");
        let run_id = recorder.run_id();
        let event = RunEvent::TextDelta {
            message_id: MessageId::new(),
            delta: "visible text".to_string(),
        };

        recorder.record_run_event(&event).expect("record event");

        let events = bundle
            .harness_event_store()
            .list_events(run_id)
            .expect("list events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, HarnessEventKind::ModelResponseReceived);
        assert!(events[0].contract_refs.is_empty());
        assert!(events[0].artifact_refs.is_empty());
        assert_eq!(
            events[0].payload,
            HarnessEventPayload::generic(serde_json::to_value(event).expect("serialize event"))
        );
    }
}
