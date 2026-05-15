use std::collections::BTreeSet;

use camino::Utf8PathBuf;
use serde_json::{Value, json};

use crate::error::RuntimeError;
use crate::harness::{
    ArtifactId, ArtifactKind, ArtifactManifest, ArtifactStore, ArtifactTag, ContractId,
    ContractRef, HarnessEvent, HarnessEventId, HarnessEventKind, HarnessEventPayload,
    HarnessEventStore, HarnessRunId, HarnessRunRecord, HarnessRunStatus, HarnessRunStore,
    SqliteArtifactStore, SqliteHarnessEventStore, SqliteHarnessRunStore, artifact::hash_bytes,
};
use crate::protocol::{ProtocolEventStore, SqliteProtocolEventStore, TurnId};
use crate::runtime::{RunEventSink, SystemClock};
use crate::session::{RunEvent, SessionId};
use crate::storage::StoreBundle;

pub struct NativeHarnessRecorder {
    run_id: HarnessRunId,
    session_id: Option<SessionId>,
    run_store: SqliteHarnessRunStore,
    event_store: SqliteHarnessEventStore,
    protocol_event_store: SqliteProtocolEventStore,
    artifact_store: SqliteArtifactStore,
    workspace_root: Utf8PathBuf,
    artifact_root: Utf8PathBuf,
    started_at_ms: i64,
    next_sequence_no: i64,
    protocol_turn_id: TurnId,
    next_protocol_sequence_no: i64,
    record_protocol_projection: bool,
}

impl NativeHarnessRecorder {
    pub fn start(
        store: &StoreBundle,
        session_id: Option<SessionId>,
        workspace_root: Utf8PathBuf,
    ) -> Result<Self, RuntimeError> {
        Self::start_with_protocol_projection(store, session_id, workspace_root, true)
    }

    pub fn start_harness_only(
        store: &StoreBundle,
        session_id: Option<SessionId>,
        workspace_root: Utf8PathBuf,
    ) -> Result<Self, RuntimeError> {
        Self::start_with_protocol_projection(store, session_id, workspace_root, false)
    }

    fn start_with_protocol_projection(
        store: &StoreBundle,
        session_id: Option<SessionId>,
        workspace_root: Utf8PathBuf,
        record_protocol_projection: bool,
    ) -> Result<Self, RuntimeError> {
        let run_id = HarnessRunId::new();
        let artifact_root = store
            .paths()
            .data_dir
            .join("harness")
            .join(run_id.to_string());
        let run_store = store.harness_run_store();
        let event_store = store.harness_event_store();
        let protocol_event_store = store.protocol_event_store();
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
            protocol_event_store,
            artifact_store,
            workspace_root,
            artifact_root,
            started_at_ms,
            next_sequence_no: 0,
            protocol_turn_id: TurnId::new(),
            next_protocol_sequence_no: 0,
            record_protocol_projection,
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
        self.record_protocol_projection(event)?;
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

    fn record_protocol_projection(&mut self, event: &RunEvent) -> Result<(), RuntimeError> {
        if !self.record_protocol_projection {
            return Ok(());
        }
        let Some(projection) = crate::protocol::project_protocol_run_event(
            event,
            self.session_id,
            self.protocol_turn_id,
            self.next_protocol_sequence_no,
        ) else {
            return Ok(());
        };
        self.protocol_event_store
            .append_event_bundle(
                &projection.runtime_event,
                projection.history_item.as_ref(),
                projection.turn_item.as_ref(),
            )
            .map_err(runtime_error)?;
        self.next_protocol_sequence_no += 1;
        Ok(())
    }

    fn append(
        &mut self,
        kind: HarnessEventKind,
        payload: HarnessEventPayload,
    ) -> Result<(), RuntimeError> {
        let event_id = HarnessEventId::new();
        let payload = enrich_payload(kind, payload)?;
        let contract_refs = contract_refs_for_kind(kind);
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
        RunEvent::ModelRequestPrepared { .. } => HarnessEventKind::ModelRequestSent,
        RunEvent::TextDelta { .. } | RunEvent::ReasoningDelta { .. } => {
            HarnessEventKind::ModelResponseReceived
        }
        RunEvent::ToolCallPending { .. } => HarnessEventKind::ToolDispatchRequested,
        RunEvent::ToolCallCompleted { .. } => HarnessEventKind::ToolExecuted,
        RunEvent::ToolCallFailed { .. } => HarnessEventKind::ToolDispatchDenied,
        RunEvent::ToolProposalRejected { .. } => HarnessEventKind::ToolDispatchDenied,
        RunEvent::CandidateRepairEditRecorded { .. } => HarnessEventKind::StateSnapshotRecorded,
        RunEvent::FileChangesRecorded { .. } => HarnessEventKind::ArtifactRegistered,
        RunEvent::CompactionCompleted { .. } => HarnessEventKind::StateSnapshotRecorded,
        RunEvent::PermissionRequested { .. } | RunEvent::PermissionResolved { .. } => {
            HarnessEventKind::ToolDispatchDenied
        }
        RunEvent::RetryScheduled { .. } | RunEvent::RecoverableRuntimeFeedback { .. } => {
            HarnessEventKind::CorrectiveResultEmitted
        }
        RunEvent::StateUpdated { .. } => HarnessEventKind::StateSnapshotRecorded,
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

fn enrich_payload(
    kind: HarnessEventKind,
    payload: HarnessEventPayload,
) -> Result<HarnessEventPayload, RuntimeError> {
    match payload {
        HarnessEventPayload::Generic(mut value) => {
            if let Value::Object(ref mut map) = value {
                let control_envelope = map.get("envelope").cloned();
                let state_projection = map.get("state").cloned();
                if let Some(state) = state_projection {
                    let turn_decision = state.get("turn_decision").cloned();
                    let allowed_surface_snapshot = turn_decision.as_ref().map(|turn_decision| {
                        json!({
                            "allowed_tools": turn_decision.get("allowed_tools").cloned().unwrap_or_else(|| json!([])),
                            "tool_choice": turn_decision.get("tool_choice").cloned().unwrap_or(Value::Null),
                            "required_next_action": turn_decision.get("required_next_action").cloned().unwrap_or(Value::Null),
                            "active_targets": turn_decision.get("active_targets").cloned().unwrap_or_else(|| json!([])),
                        })
                    });
                    let repair_lane = turn_decision
                        .as_ref()
                        .and_then(|turn_decision| turn_decision.get("repair_lane"))
                        .cloned();
                    let operation_template = repair_lane
                        .as_ref()
                        .and_then(|repair_lane| repair_lane.get("operation_template"))
                        .cloned();
                    let verification_cluster = repair_lane
                        .as_ref()
                        .and_then(|repair_lane| repair_lane.get("verification_cluster"))
                        .cloned();
                    if let Some(snapshot) = allowed_surface_snapshot {
                        map.insert("allowed_surface_snapshot".to_string(), snapshot);
                    }
                    if let Some(template) = operation_template {
                        map.insert("repair_operation_template".to_string(), template);
                    }
                    if let Some(cluster) = verification_cluster {
                        map.insert("verification_cluster".to_string(), cluster);
                    }
                    if !map.contains_key("allowed_surface_snapshot") {
                        map.insert(
                            "missing_control_projection".to_string(),
                            json!({
                                "status": "missing",
                                "reason": "StateUpdated event lacks a typed TurnControlEnvelope/RepairControlSnapshot projection; harness must report this instead of synthesizing authority from state",
                                "required_artifacts": [
                                    "turn_control_envelope",
                                    "allowed_surface_snapshot",
                                    "repair_operation_template",
                                    "verification_cluster"
                                ]
                            }),
                        );
                    }
                    map.insert(
                        "completed_todo_evidence_state".to_string(),
                        json!({
                            "status": "not_captured",
                            "contradicted_todos": [],
                            "missing_evidence_todos": [],
                            "evidence_refs": []
                        }),
                    );
                }
                if let Some(envelope) = control_envelope {
                    let authority = envelope.get("action_authority");
                    map.insert(
                        "allowed_surface_snapshot".to_string(),
                        json!({
                            "projection_id": envelope.get("projection_id").cloned().unwrap_or(Value::Null),
                            "allowed_tools": authority
                                .and_then(|authority| authority.get("allowed_tools"))
                                .cloned()
                                .unwrap_or_else(|| json!([])),
                            "forbidden_tools": authority
                                .and_then(|authority| authority.get("forbidden_tools"))
                                .cloned()
                                .unwrap_or_else(|| json!([])),
                            "tool_choice": authority
                                .and_then(|authority| authority.get("tool_choice"))
                                .cloned()
                                .unwrap_or(Value::Null),
                        }),
                    );
                }
                let typed_tool_projection_applied = apply_tool_lifecycle_metadata_projection(map);
                let should_build_tool_feedback_projection = typed_tool_projection_applied
                    && !matches!(kind, HarnessEventKind::ToolDispatchRequested);
                let stored_text_projection_seen =
                    map.get("metadata").is_some_and(stored_text_projection_seen);
                if should_build_tool_feedback_projection
                    || (stored_text_projection_seen
                        && matches!(
                            kind,
                            HarnessEventKind::ToolDispatchDenied
                                | HarnessEventKind::CorrectiveResultEmitted
                                | HarnessEventKind::ToolResultNormalized
                        ))
                {
                    if stored_text_projection_seen {
                        map.insert(
                            "text_projection_rejected".to_string(),
                            json!({
                                "source": "diagnostic_text",
                                "mode": map
                                    .get("metadata")
                                    .and_then(stored_text_projection_mode)
                                    .unwrap_or("explicit_text_projection"),
                                "status": "not_promoted_to_authority",
                                "reason": "typed projection is required; harness artifacts must not synthesize allowed surface or lifecycle policy from text",
                            }),
                        );
                    }
                    let signature_source = serde_json::to_string(&map).map_err(runtime_error)?;
                    let result_hash = map
                        .get("result_hash")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| hash_bytes(signature_source.as_bytes()));
                    map.insert(
                        "no_progress_signature".to_string(),
                        json!({
                            "result_hash": result_hash,
                            "tool": map.get("tool").cloned().unwrap_or(Value::Null),
                            "allowed_surface_snapshot": map.get("allowed_surface_snapshot").cloned().unwrap_or_else(|| json!([])),
                            "repeat_count": 1
                        }),
                    );
                }
            }
            Ok(HarnessEventPayload::Generic(value))
        }
        other => Ok(other),
    }
}

fn apply_tool_lifecycle_metadata_projection(map: &mut serde_json::Map<String, Value>) -> bool {
    let Some(metadata) = map.get("metadata").and_then(Value::as_object).cloned() else {
        return false;
    };
    let mut applied = false;

    if let Some(route) = metadata.get("tool_route").and_then(Value::as_object) {
        map.insert(
            "tool_route_decision".to_string(),
            Value::Object(route.clone()),
        );
        if !map.contains_key("allowed_surface_snapshot") {
            map.insert(
                "allowed_surface_snapshot".to_string(),
                json!({
                    "source": "tool_route_decision",
                    "allowed_tools": route.get("allowed_tools").cloned().unwrap_or_else(|| json!([])),
                    "tool_choice": route.get("tool_choice").cloned().unwrap_or(Value::Null),
                }),
            );
        }
        applied = true;
    }

    if let Some(control_projection) = metadata.get("control_projection") {
        map.insert("control_projection".to_string(), control_projection.clone());
        map.insert(
            "allowed_surface_snapshot".to_string(),
            json!({
                "source": "control_projection",
                "projection_id": control_projection.get("projection_id").cloned().unwrap_or(Value::Null),
                "surface": control_projection.get("surface").cloned().unwrap_or(Value::Null),
                "allowed_tools": control_projection.get("allowed_tools").cloned().unwrap_or_else(|| json!([])),
                "forbidden_tools": control_projection.get("forbidden_tools").cloned().unwrap_or_else(|| json!([])),
            }),
        );
        applied = true;
    }

    if let Some(feedback) = metadata.get("tool_feedback_envelope") {
        map.insert("tool_feedback_envelope".to_string(), feedback.clone());
        map.insert(
            "allowed_surface_snapshot".to_string(),
            json!({
                "source": "tool_feedback_envelope",
                "allowed_tools": feedback.get("allowed_surface_snapshot").cloned().unwrap_or_else(|| json!([])),
                "required_target": feedback.get("required_target").cloned().unwrap_or(Value::Null),
            }),
        );
        for key in [
            "required_target",
            "repair_operation_template",
            "verification_cluster",
            "repair_control_snapshot",
            "contract_reconciliation",
            "result_hash",
        ] {
            if let Some(value) = feedback.get(key).cloned() {
                map.insert(key.to_string(), value);
            }
        }
        applied = true;
    }

    applied
}

fn stored_text_projection_seen(metadata: &Value) -> bool {
    stored_text_projection_mode(metadata).is_some()
}

fn stored_text_projection_mode(metadata: &Value) -> Option<&'static str> {
    match metadata.get("projection_mode").and_then(Value::as_str) {
        Some("legacy_import") => Some("legacy_import"),
        Some("stored_artifact_replay") => Some("stored_artifact_replay"),
        Some("migration_backfill") => Some("migration_backfill"),
        _ => None,
    }
}

fn contract_refs_for_kind(kind: HarnessEventKind) -> Vec<ContractRef> {
    let ids: &[&str] = match kind {
        HarnessEventKind::RunStarted => &["runtime_contract"],
        HarnessEventKind::UserTurnAccepted => &["requested_work_contract"],
        HarnessEventKind::ControlEnvelopePrepared => &[
            "model_projection_schema",
            "active_work_contract",
            "turn_control_envelope",
        ],
        HarnessEventKind::ModelProjectionBuilt | HarnessEventKind::ModelRequestSent => {
            &["model_projection_schema", "active_work_contract"]
        }
        HarnessEventKind::ToolDispatchRequested
        | HarnessEventKind::ToolDispatchDenied
        | HarnessEventKind::ToolExecuted => &["tool_dispatch_contract"],
        HarnessEventKind::ToolResultNormalized | HarnessEventKind::CorrectiveResultEmitted => {
            &["tool_result_schema", "repair_operation_template"]
        }
        HarnessEventKind::StateSnapshotRecorded | HarnessEventKind::StateTransitionRecorded => {
            &["state_machine_contract"]
        }
        HarnessEventKind::QualityGateEvaluated | HarnessEventKind::ScenarioGateEvaluated => {
            &["quality_gate_schema"]
        }
        HarnessEventKind::ArtifactRegistered => &["artifact_manifest_schema"],
        HarnessEventKind::RunTerminalized => &["terminal_state_contract"],
        _ => &["runtime_contract"],
    };
    ids.iter()
        .map(|id| ContractRef {
            id: ContractId::new(*id),
            version: "current".to_string(),
        })
        .collect()
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
