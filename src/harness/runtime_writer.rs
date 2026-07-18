use std::collections::BTreeSet;

use camino::Utf8PathBuf;

use crate::error::RuntimeError;
use crate::harness::{
    ArtifactId, ArtifactKind, ArtifactManifest, ArtifactTag, ContractRef, HarnessEvent,
    HarnessEventId, HarnessEventKind, HarnessEventPayload, HarnessRunId, HarnessRunRecord,
    HarnessRunStatus, HarnessRunStore, SqliteHarnessEventStore, SqliteHarnessRunStore,
    artifact::hash_bytes,
};
use crate::protocol::{ModelResponseId, TurnId};
use crate::runtime::{RunEventSink, SystemClock};
use crate::session::{RunEvent, SessionId};
use crate::storage::{InternalFileProducerLease, StoragePaths, StoreBundle};

const MAX_RECORDED_EVENTS_PER_RUN: i64 = 4_096;
const MAX_RECORDED_PAYLOAD_BYTES: usize = 256 * 1024;
const MAX_COALESCED_DELTA_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HarnessRecordingStatus {
    pub disabled: bool,
    pub failure_count: u64,
    pub dropped_event_count: u64,
    pub dropped_delta_bytes: u64,
    pub last_error: Option<String>,
}

#[derive(Debug)]
enum PendingDelta {
    Text {
        response_id: ModelResponseId,
        delta: String,
    },
    ReasoningSummary {
        response_id: ModelResponseId,
        delta: String,
    },
}

pub struct NativeHarnessRecorder {
    run_id: HarnessRunId,
    session_id: Option<SessionId>,
    run_store: SqliteHarnessRunStore,
    event_store: SqliteHarnessEventStore,
    storage_paths: StoragePaths,
    workspace_root: Utf8PathBuf,
    artifact_root: Utf8PathBuf,
    started_at_ms: i64,
    next_sequence_no: i64,
    protocol_turn_id: TurnId,
    recording_status: HarnessRecordingStatus,
    pending_delta: Option<PendingDelta>,
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
        let mut recorder = Self::unregistered(store, session_id, workspace_root, protocol_turn_id);
        recorder.initialize()?;
        Ok(recorder)
    }

    pub fn start_best_effort_for_turn(
        store: &StoreBundle,
        session_id: Option<SessionId>,
        workspace_root: Utf8PathBuf,
        protocol_turn_id: TurnId,
    ) -> Self {
        let mut recorder = Self::unregistered(store, session_id, workspace_root, protocol_turn_id);
        if let Err(error) = recorder.initialize() {
            recorder.disable(error);
        }
        recorder
    }

    fn unregistered(
        store: &StoreBundle,
        session_id: Option<SessionId>,
        workspace_root: Utf8PathBuf,
        protocol_turn_id: TurnId,
    ) -> Self {
        let run_id = HarnessRunId::new();
        let artifact_root = store
            .paths()
            .data_dir
            .join("harness")
            .join(run_id.to_string());
        let run_store = store.harness_run_store();
        let event_store = store.harness_event_store();
        let started_at_ms = SystemClock::now_ms();
        Self {
            run_id,
            session_id,
            run_store,
            event_store,
            storage_paths: store.paths().clone(),
            workspace_root,
            artifact_root,
            started_at_ms,
            next_sequence_no: 0,
            protocol_turn_id,
            recording_status: HarnessRecordingStatus::default(),
            pending_delta: None,
        }
    }

    fn initialize(&mut self) -> Result<(), RuntimeError> {
        let _producer_lease =
            InternalFileProducerLease::acquire(&self.storage_paths).map_err(runtime_error)?;
        let harness_root = self
            .artifact_root
            .parent()
            .ok_or_else(|| runtime_error("native harness artifact root has no parent directory"))?;
        std::fs::create_dir_all(harness_root.as_std_path()).map_err(runtime_error)?;
        std::fs::create_dir(self.artifact_root.as_std_path()).map_err(runtime_error)?;
        if let Err(error) = self.run_store.upsert_run(&HarnessRunRecord {
            id: self.run_id,
            session_id: self.session_id,
            workspace_root: self.workspace_root.clone(),
            artifact_root: self.artifact_root.clone(),
            mode: "native_runtime".to_string(),
            started_at_ms: self.started_at_ms,
            completed_at_ms: None,
            status: HarnessRunStatus::Started,
        }) {
            let _ = std::fs::remove_dir(self.artifact_root.as_std_path());
            return Err(runtime_error(error));
        }
        Ok(())
    }

    pub fn run_id(&self) -> HarnessRunId {
        self.run_id
    }

    pub fn protocol_turn_id(&self) -> TurnId {
        self.protocol_turn_id
    }

    pub fn recording_status(&self) -> &HarnessRecordingStatus {
        &self.recording_status
    }

    pub fn record_run_event(&mut self, event: &RunEvent) -> Result<(), RuntimeError> {
        if self.recording_status.disabled {
            self.recording_status.dropped_event_count =
                self.recording_status.dropped_event_count.saturating_add(1);
            return Ok(());
        }
        match event {
            RunEvent::TextDelta { response_id, delta } => {
                self.coalesce_delta(*response_id, delta, false)
            }
            RunEvent::ReasoningSummaryDelta { response_id, delta } => {
                self.coalesce_delta(*response_id, delta, true)
            }
            _ => {
                self.flush_pending_delta()?;
                self.record_event_immediate(event)
            }
        }
    }

    pub fn record_run_event_best_effort(&mut self, event: &RunEvent) {
        if self.recording_status.disabled {
            self.recording_status.dropped_event_count =
                self.recording_status.dropped_event_count.saturating_add(1);
            return;
        }
        if let Err(error) = self.record_run_event(event) {
            self.disable(error);
        }
    }

    pub fn flush(&mut self) -> Result<(), RuntimeError> {
        self.flush_pending_delta()
    }

    pub fn flush_best_effort(&mut self) {
        if let Err(error) = self.flush_pending_delta() {
            self.disable(error);
        }
    }

    fn disable(&mut self, error: RuntimeError) {
        self.recording_status.disabled = true;
        self.recording_status.failure_count = self.recording_status.failure_count.saturating_add(1);
        self.recording_status.last_error = Some(error.to_string());
        self.pending_delta = None;
    }

    fn coalesce_delta(
        &mut self,
        response_id: ModelResponseId,
        delta: &str,
        reasoning_summary: bool,
    ) -> Result<(), RuntimeError> {
        let compatible = matches!(
            &self.pending_delta,
            Some(PendingDelta::Text { response_id: current, .. })
                if !reasoning_summary && *current == response_id
        ) || matches!(
            &self.pending_delta,
            Some(PendingDelta::ReasoningSummary { response_id: current, .. })
                if reasoning_summary && *current == response_id
        );
        if self.pending_delta.is_some() && !compatible {
            self.flush_pending_delta()?;
        }
        if self.pending_delta.is_none() {
            self.pending_delta = Some(if reasoning_summary {
                PendingDelta::ReasoningSummary {
                    response_id,
                    delta: String::new(),
                }
            } else {
                PendingDelta::Text {
                    response_id,
                    delta: String::new(),
                }
            });
        }
        let buffer = match self.pending_delta.as_mut().expect("pending delta") {
            PendingDelta::Text { delta, .. } | PendingDelta::ReasoningSummary { delta, .. } => {
                delta
            }
        };
        let dropped = append_bounded_delta(buffer, delta);
        self.recording_status.dropped_delta_bytes = self
            .recording_status
            .dropped_delta_bytes
            .saturating_add(dropped as u64);
        Ok(())
    }

    fn flush_pending_delta(&mut self) -> Result<(), RuntimeError> {
        let Some(pending) = self.pending_delta.take() else {
            return Ok(());
        };
        let event = match pending {
            PendingDelta::Text { response_id, delta } => RunEvent::TextDelta { response_id, delta },
            PendingDelta::ReasoningSummary { response_id, delta } => {
                RunEvent::ReasoningSummaryDelta { response_id, delta }
            }
        };
        self.record_event_immediate(&event)
    }

    fn record_event_immediate(&mut self, event: &RunEvent) -> Result<(), RuntimeError> {
        let terminal_status = terminal_status_for_run_event(event);
        let event_capacity = if terminal_status.is_some() {
            MAX_RECORDED_EVENTS_PER_RUN
        } else {
            MAX_RECORDED_EVENTS_PER_RUN.saturating_sub(1)
        };
        let mut first_error = None;
        if self.next_sequence_no >= event_capacity {
            self.recording_status.dropped_event_count =
                self.recording_status.dropped_event_count.saturating_add(1);
        } else {
            let kind = harness_kind_for_run_event(event);
            match payload_for_run_event(event, &self.workspace_root, &self.artifact_root)
                .and_then(|payload| bounded_payload(kind, payload))
                .and_then(|payload| self.append(kind, payload))
            {
                Ok(()) => {}
                Err(error) => first_error = Some(error),
            }
        }
        if let Some(status) = terminal_status {
            let terminal_result = self
                .run_store
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
                .map_err(runtime_error);
            if first_error.is_none() {
                first_error = terminal_result.err();
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    fn append(
        &mut self,
        kind: HarnessEventKind,
        payload: HarnessEventPayload,
    ) -> Result<(), RuntimeError> {
        let event_id = HarnessEventId::new();
        let contract_refs = Vec::new();
        let prepared_artifact =
            self.prepare_payload_artifact(event_id, kind, &payload, &contract_refs)?;
        let artifact_refs = prepared_artifact
            .as_ref()
            .map(|prepared| vec![prepared.manifest.id])
            .unwrap_or_default();
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
        if let Err(error) = self.event_store.append_event_with_artifact(
            &event,
            prepared_artifact
                .as_ref()
                .map(|prepared| &prepared.manifest),
        ) {
            if let Some(prepared) = prepared_artifact {
                let _ = std::fs::remove_file(prepared.full_path.as_std_path());
            }
            return Err(runtime_error(error));
        }
        self.next_sequence_no += 1;
        Ok(())
    }

    fn prepare_payload_artifact(
        &self,
        event_id: HarnessEventId,
        kind: HarnessEventKind,
        payload: &HarnessEventPayload,
        contract_refs: &[ContractRef],
    ) -> Result<Option<PreparedPayloadArtifact>, RuntimeError> {
        let Some(artifact_kind) = artifact_kind_for_event(kind) else {
            return Ok(None);
        };
        let internal_file_lease =
            InternalFileProducerLease::acquire(&self.storage_paths).map_err(runtime_error)?;
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
        if let Err(error) = std::fs::write(full_path.as_std_path(), payload_json.as_bytes()) {
            let _ = std::fs::remove_file(full_path.as_std_path());
            return Err(runtime_error(error));
        }
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
        let manifest = ArtifactManifest {
            id: artifact_id,
            run_id: self.run_id,
            kind: artifact_kind,
            relative_path,
            sha256: hash_bytes(payload_json.as_bytes()),
            size_bytes: payload_json.len() as u64,
            tags,
            created_by_event: Some(event_id),
            contract_refs: contract_refs.to_vec(),
        };
        Ok(Some(PreparedPayloadArtifact {
            manifest,
            full_path,
            _internal_file_lease: internal_file_lease,
        }))
    }
}

struct PreparedPayloadArtifact {
    manifest: ArtifactManifest,
    full_path: Utf8PathBuf,
    _internal_file_lease: InternalFileProducerLease,
}

impl Drop for NativeHarnessRecorder {
    fn drop(&mut self) {
        self.flush_best_effort();
    }
}

fn append_bounded_delta(buffer: &mut String, delta: &str) -> usize {
    let remaining = MAX_COALESCED_DELTA_BYTES.saturating_sub(buffer.len());
    if remaining == 0 {
        return delta.len();
    }
    let mut accepted_bytes = 0usize;
    for (index, character) in delta.char_indices() {
        let end = index.saturating_add(character.len_utf8());
        if end > remaining {
            break;
        }
        accepted_bytes = end;
    }
    buffer.push_str(&delta[..accepted_bytes]);
    delta.len().saturating_sub(accepted_bytes)
}

fn bounded_payload(
    kind: HarnessEventKind,
    payload: HarnessEventPayload,
) -> Result<HarnessEventPayload, RuntimeError> {
    let serialized_bytes = serde_json::to_vec(&payload).map_err(runtime_error)?.len();
    if serialized_bytes <= MAX_RECORDED_PAYLOAD_BYTES {
        return Ok(payload);
    }
    Ok(HarnessEventPayload::generic(serde_json::json!({
        "recording": "omitted",
        "reason": "payload_exceeded_limit",
        "event_kind": event_kind_file_label(kind),
        "original_bytes": serialized_bytes,
        "limit_bytes": MAX_RECORDED_PAYLOAD_BYTES,
    })))
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

    pub fn recording_status(&self) -> &HarnessRecordingStatus {
        self.recorder.recording_status()
    }
}

impl<S: RunEventSink + ?Sized> RunEventSink for HarnessRecordingSink<'_, S> {
    fn reserve_protocol_sequence_no(&mut self) -> Option<i64> {
        self.inner.reserve_protocol_sequence_no()
    }

    fn emit(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
        self.inner.emit(event.clone())?;
        self.recorder.record_run_event_best_effort(&event);
        Ok(())
    }

    fn emit_runtime_only(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
        self.inner.emit_runtime_only(event.clone())?;
        self.recorder.record_run_event_best_effort(&event);
        Ok(())
    }
}

fn harness_kind_for_run_event(event: &RunEvent) -> HarnessEventKind {
    match event {
        RunEvent::SessionStarted { .. } => HarnessEventKind::RunStarted,
        RunEvent::SessionTitleUpdated { .. } => HarnessEventKind::StateSnapshotRecorded,
        RunEvent::UserTurnStored { .. } => HarnessEventKind::UserTurnAccepted,
        RunEvent::WorldStateUpdated { .. } => HarnessEventKind::StateSnapshotRecorded,
        RunEvent::ModelRequestPrepared { .. } => HarnessEventKind::ModelRequestPrepared,
        RunEvent::ProviderPhase { .. } => HarnessEventKind::StateTransitionRecorded,
        RunEvent::TextDelta { .. }
        | RunEvent::AssistantMessageCommitted { .. }
        | RunEvent::ReasoningSummaryDelta { .. } => HarnessEventKind::ModelResponseReceived,
        RunEvent::ToolCallPending { .. } => HarnessEventKind::ToolDispatchRequested,
        RunEvent::ToolCallCompleted { .. } => HarnessEventKind::ToolExecuted,
        RunEvent::ToolCallDeclined { .. } => HarnessEventKind::ToolDeclined,
        RunEvent::ToolCallCancelled { .. } => HarnessEventKind::ToolCancelled,
        RunEvent::ToolCallFailed { .. } => HarnessEventKind::ToolFailed,
        RunEvent::FileChangesRecorded { .. } => HarnessEventKind::ArtifactRegistered,
        RunEvent::CompactionCompleted { .. } => HarnessEventKind::StateSnapshotRecorded,
        RunEvent::PermissionRequested { .. } => HarnessEventKind::PermissionRequested,
        RunEvent::PermissionResolved { .. } => HarnessEventKind::PermissionResolved,
        RunEvent::RecoverableRuntimeFeedback { .. } => HarnessEventKind::CorrectiveResultEmitted,
        RunEvent::TurnTerminal { .. } => HarnessEventKind::RunTerminalized,
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
        | HarnessEventKind::ModelRequestPrepared
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
        HarnessEventKind::ControlEnvelopePrepared => "turn_control_envelope",
        HarnessEventKind::ModelProjectionBuilt => "model_projection_built",
        HarnessEventKind::ModelRequestPrepared => "model_request_prepared",
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
        RunEvent::TurnTerminal { terminal, .. } => Some(match &terminal.outcome {
            crate::protocol::TurnTerminalOutcome::Completed => HarnessRunStatus::Pass,
            crate::protocol::TurnTerminalOutcome::Interrupted { .. } => HarnessRunStatus::Blocked,
            crate::protocol::TurnTerminalOutcome::Failed { .. } => HarnessRunStatus::Fail,
        }),
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
    use crate::protocol::ModelResponseId;
    use crate::session::{RequestDiagnosticsPart, RunMetrics, SessionId, ToolCallId};
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
    fn prepared_request_evidence_does_not_claim_that_transport_started() {
        let event = RunEvent::ModelRequestPrepared {
            session_id: SessionId::new(),
            diagnostics: RequestDiagnosticsPart {
                provider: "openai_compatible".to_string(),
                model_name: "model".to_string(),
                base_url: "http://localhost:1234".to_string(),
                request_timeout_ms: 1_000,
                stream_idle_timeout_ms: 1_000,
                configured_max_output_tokens: None,
                effective_max_output_tokens: None,
                output_budget_reason: None,
                supports_tools: Some(true),
                supports_reasoning: None,
                supports_images: Some(false),
                system_prompt_chars: 0,
                tool_count: 0,
                tool_choice: None,
                parallel_tool_calls: None,
                provider_message_count: 0,
                image_count: 0,
                image_bytes: 0,
                tool_names: Vec::new(),
                tool_schemas: Vec::new(),
                context_window: None,
                messages: Vec::new(),
            },
        };

        assert_eq!(
            harness_kind_for_run_event(&event),
            HarnessEventKind::ModelRequestPrepared
        );
        assert_eq!(
            event_kind_file_label(HarnessEventKind::ModelRequestPrepared),
            "model_request_prepared"
        );
        assert_eq!(
            artifact_kind_for_event(HarnessEventKind::ModelRequestPrepared),
            Some(ArtifactKind::RequestDiagnostics)
        );
        assert_eq!(
            serde_json::from_str::<HarnessEventKind>("\"model_request_sent\"")
                .expect("persisted compatibility kind"),
            HarnessEventKind::ModelRequestSent
        );
    }

    #[test]
    fn durable_terminal_status_owns_harness_run_outcome() {
        let cases = [
            (
                crate::protocol::TurnTerminalOutcome::Completed,
                HarnessRunStatus::Pass,
            ),
            (
                crate::protocol::TurnTerminalOutcome::Interrupted {
                    cause: crate::protocol::TurnInterruptionCause::UserStop,
                },
                HarnessRunStatus::Blocked,
            ),
            (
                crate::protocol::TurnTerminalOutcome::Failed {
                    error: "failed".to_string(),
                },
                HarnessRunStatus::Fail,
            ),
        ];

        for (outcome, expected) in cases {
            let event = RunEvent::TurnTerminal {
                session_id: SessionId::new(),
                terminal: Box::new(crate::session::model::DurableTurnTerminal {
                    outcome,
                    final_response_id: None,
                    tool_call_count: 0,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: RunMetrics::default(),
                }),
            };

            assert_eq!(
                harness_kind_for_run_event(&event),
                HarnessEventKind::RunTerminalized
            );
            assert_eq!(terminal_status_for_run_event(&event), Some(expected));
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
            response_id: ModelResponseId::new(),
            delta: "visible text".to_string(),
        };

        recorder.record_run_event(&event).expect("record event");
        recorder.flush().expect("flush coalesced delta");

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

    #[test]
    fn coalesces_and_bounds_streaming_deltas() {
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
        let mut recorder = NativeHarnessRecorder::start_harness_only(
            &bundle,
            None,
            Utf8PathBuf::from("C:/workspace"),
        )
        .expect("recorder");
        let run_id = recorder.run_id();
        let response_id = ModelResponseId::new();
        for delta in ["alpha", "-", "beta"] {
            recorder
                .record_run_event(&RunEvent::TextDelta {
                    response_id,
                    delta: delta.to_string(),
                })
                .expect("coalesce delta");
        }
        recorder.flush().expect("flush delta");

        let events = bundle
            .harness_event_store()
            .list_events(run_id)
            .expect("events");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].payload,
            HarnessEventPayload::generic(serde_json::json!({
                "kind": "text_delta",
                "response_id": response_id,
                "delta": "alpha-beta",
            }))
        );

        recorder
            .record_run_event(&RunEvent::TextDelta {
                response_id,
                delta: "x".repeat(MAX_COALESCED_DELTA_BYTES * 2),
            })
            .expect("bounded delta");
        recorder.flush().expect("flush bounded delta");
        assert_eq!(
            recorder.recording_status().dropped_delta_bytes,
            MAX_COALESCED_DELTA_BYTES as u64
        );
    }

    #[test]
    fn oversized_payload_is_replaced_by_a_bounded_omission_record() {
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
        let mut recorder = NativeHarnessRecorder::start_harness_only(
            &bundle,
            None,
            Utf8PathBuf::from("C:/workspace"),
        )
        .expect("recorder");
        let run_id = recorder.run_id();
        recorder
            .record_run_event(&RunEvent::AssistantMessageCommitted {
                response_id: ModelResponseId::new(),
                text: "x".repeat(MAX_RECORDED_PAYLOAD_BYTES * 2),
            })
            .expect("bounded payload");

        let events = bundle
            .harness_event_store()
            .list_events(run_id)
            .expect("events");
        assert_eq!(events.len(), 1);
        let serialized = serde_json::to_vec(&events[0].payload).expect("serialize payload");
        assert!(serialized.len() < MAX_RECORDED_PAYLOAD_BYTES);
        assert!(
            serialized
                .windows(b"payload_exceeded_limit".len())
                .any(|window| { window == b"payload_exceeded_limit" })
        );
    }

    #[test]
    fn best_effort_sink_delivers_user_event_when_recording_fails() {
        #[derive(Default)]
        struct CollectingSink {
            events: Vec<RunEvent>,
        }

        impl RunEventSink for CollectingSink {
            fn emit(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
                self.events.push(event);
                Ok(())
            }
        }

        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 path");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let store = SqliteStore::open(&paths).expect("open store");
        store.migrate().expect("migrate store");
        let bundle = StoreBundle::new(store);
        let mut recorder = NativeHarnessRecorder::start_harness_only(
            &bundle,
            None,
            Utf8PathBuf::from("C:/workspace"),
        )
        .expect("recorder");
        let blocked_artifact_root = data_dir.join("blocked-artifact-root");
        std::fs::write(blocked_artifact_root.as_std_path(), b"not a directory")
            .expect("blocked artifact root");
        recorder.artifact_root = blocked_artifact_root;
        let session_id = SessionId::new();
        let event = RunEvent::TurnTerminal {
            session_id,
            terminal: Box::new(crate::session::model::DurableTurnTerminal {
                outcome: crate::protocol::TurnTerminalOutcome::Completed,
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: RunMetrics::default(),
            }),
        };
        let mut inner = CollectingSink::default();
        let mut sink = HarnessRecordingSink::new(recorder, &mut inner);

        sink.emit(event).expect("inner event delivery");
        assert!(sink.recording_status().disabled);
        assert_eq!(sink.recording_status().failure_count, 1);
        drop(sink);
        assert_eq!(inner.events.len(), 1);
    }

    #[test]
    fn failed_inner_delivery_does_not_become_semantic_harness_evidence() {
        #[derive(Default)]
        struct FailingSink {
            emit_calls: usize,
            runtime_only_calls: usize,
        }

        impl RunEventSink for FailingSink {
            fn emit(&mut self, _event: RunEvent) -> Result<(), RuntimeError> {
                self.emit_calls += 1;
                Err(RuntimeError::Message("inner emit failed".to_string()))
            }

            fn emit_runtime_only(&mut self, _event: RunEvent) -> Result<(), RuntimeError> {
                self.runtime_only_calls += 1;
                Err(RuntimeError::Message(
                    "inner runtime-only emit failed".to_string(),
                ))
            }
        }

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
        let recorder = NativeHarnessRecorder::start_harness_only(
            &bundle,
            None,
            Utf8PathBuf::from("C:/workspace"),
        )
        .expect("recorder");
        let run_id = recorder.run_id();
        let response_id = ModelResponseId::new();
        let mut inner = FailingSink::default();

        {
            let mut sink = HarnessRecordingSink::new(recorder, &mut inner);
            sink.emit(RunEvent::TextDelta {
                response_id,
                delta: "not delivered".to_string(),
            })
            .expect_err("failed delivery must remain failed");
            sink.emit_runtime_only(RunEvent::ReasoningSummaryDelta {
                response_id,
                delta: "also not delivered".to_string(),
            })
            .expect_err("failed runtime-only delivery must remain failed");
            assert!(!sink.recording_status().disabled);
            assert_eq!(sink.recording_status().failure_count, 0);
        }

        assert_eq!(inner.emit_calls, 1);
        assert_eq!(inner.runtime_only_calls, 1);
        let events = bundle
            .harness_event_store()
            .list_events(run_id)
            .expect("list events");
        assert!(events.is_empty());
    }

    #[test]
    fn best_effort_start_disables_only_the_recording_component() {
        let temp = tempfile::tempdir().expect("tempdir");
        let data_dir = Utf8PathBuf::from_path_buf(temp.path().join("data")).expect("utf8 path");
        let paths = StoragePaths {
            database_path: data_dir.join("moyai.sqlite3"),
            truncation_dir: data_dir.join("truncation"),
            data_dir: data_dir.clone(),
        };
        let store = SqliteStore::open(&paths).expect("open store");
        store.migrate().expect("migrate store");
        let bundle = StoreBundle::new(store);
        std::fs::write(data_dir.join("harness").as_std_path(), b"not a directory")
            .expect("block harness root");

        let recorder = NativeHarnessRecorder::start_best_effort_for_turn(
            &bundle,
            None,
            Utf8PathBuf::from("C:/workspace"),
            TurnId::new(),
        );

        assert!(recorder.recording_status().disabled);
        assert_eq!(recorder.recording_status().failure_count, 1);
    }
}
