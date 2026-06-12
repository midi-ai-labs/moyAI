use serde_json::{Value, json};

use crate::error::RuntimeError;

pub trait SchemaDescriptor {
    fn schema_id() -> &'static str;
    fn schema_version() -> &'static str;
    fn json_schema() -> Value;
}

pub fn exported_schemas() -> Vec<(&'static str, Value)> {
    vec![
        ("harness.event.v1.json", event_schema()),
        ("harness.artifact_manifest.v1.json", artifact_schema()),
        ("harness.contract_record.v1.json", contract_schema()),
        ("harness.quality_gate_result.v1.json", gate_schema()),
        ("harness.replay_report.v1.json", replay_report_schema()),
        (
            "harness.repair_operation_template.v1.json",
            repair_operation_template_schema(),
        ),
        (
            "harness.repair_control_snapshot.v1.json",
            repair_control_snapshot_schema(),
        ),
        (
            "harness.contract_reconciliation.v1.json",
            contract_reconciliation_schema(),
        ),
        (
            "harness.verification_failure_cluster.v1.json",
            verification_failure_cluster_schema(),
        ),
        (
            "harness.ahe_lite_change_manifest.v1.json",
            ahe_lite_change_manifest_schema(),
        ),
        (
            "harness.completed_todo_evidence.v1.json",
            completed_todo_evidence_schema(),
        ),
        (
            "harness.tool_no_progress_signature.v1.json",
            tool_no_progress_signature_schema(),
        ),
        ("manual_st.route_manifest.v1.json", route_manifest_schema()),
        ("manual_st.case_progress.v1.json", case_progress_schema()),
        (
            "manual_st.verification_command_log.v1.json",
            verification_command_log_schema(),
        ),
        (
            "manual_st.contract_reconciliation_report.v1.json",
            contract_reconciliation_report_schema(),
        ),
        (
            "manual_st.workspace_diff_manifest.v1.json",
            workspace_diff_manifest_schema(),
        ),
        (
            "manual_st.request_payload_summary.v1.json",
            request_payload_summary_schema(),
        ),
        (
            "manual_st.timeout_classification.v1.json",
            timeout_classification_schema(),
        ),
    ]
}

pub fn write_schema_files(output: &camino::Utf8Path) -> Result<(), RuntimeError> {
    std::fs::create_dir_all(output.as_std_path()).map_err(|error| {
        RuntimeError::Message(format!("failed to create schema output directory: {error}"))
    })?;
    for (name, schema) in exported_schemas() {
        let path = output.join(name);
        let json = serde_json::to_string_pretty(&schema)
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
        std::fs::write(path.as_std_path(), json)
            .map_err(|error| RuntimeError::Message(format!("failed to write schema: {error}")))?;
    }
    Ok(())
}

fn base_schema(id: &str, title: &str, required: &[&str], properties: Value) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": id,
        "title": title,
        "type": "object",
        "required": required,
        "properties": properties,
        "additionalProperties": false
    })
}

fn event_schema() -> Value {
    base_schema(
        "moyai.harness.event.v1",
        "HarnessEvent",
        &[
            "id",
            "run_id",
            "sequence_no",
            "created_at_ms",
            "kind",
            "payload",
            "contract_refs",
            "artifact_refs",
            "parent_event_id",
        ],
        json!({
            "id": {"type": "string", "minLength": 26},
            "run_id": {"type": "string", "minLength": 26},
            "sequence_no": {"type": "integer", "minimum": 0},
            "created_at_ms": {"type": "integer", "minimum": 0},
            "kind": {"type": "string"},
            "payload": {"type": "object"},
            "contract_refs": {"type": "array"},
            "artifact_refs": {"type": "array", "items": {"type": "string"}},
            "parent_event_id": {"type": ["string", "null"]}
        }),
    )
}

fn artifact_schema() -> Value {
    base_schema(
        "moyai.harness.artifact_manifest.v1",
        "ArtifactManifest",
        &[
            "id",
            "run_id",
            "kind",
            "relative_path",
            "sha256",
            "size_bytes",
            "tags",
            "created_by_event",
            "contract_refs",
        ],
        json!({
            "id": {"type": "string", "minLength": 26},
            "run_id": {"type": "string", "minLength": 26},
            "kind": {"type": "string"},
            "relative_path": {"type": "string", "minLength": 1},
            "sha256": {"type": "string", "pattern": "^[a-f0-9]{64}$"},
            "size_bytes": {"type": "integer", "minimum": 0},
            "tags": {"type": "array", "items": {"type": "string"}},
            "created_by_event": {"type": ["string", "null"]},
            "contract_refs": {"type": "array"}
        }),
    )
}

fn contract_schema() -> Value {
    base_schema(
        "moyai.harness.contract_record.v1",
        "ContractRecord",
        &[
            "id",
            "kind",
            "version",
            "source_path",
            "content_sha256",
            "schema_ref",
            "model_visible_summary",
        ],
        json!({
            "id": {"type": "string", "minLength": 1},
            "kind": {"type": "string"},
            "version": {"type": "string", "minLength": 1},
            "source_path": {"type": "string", "minLength": 1},
            "content_sha256": {"type": "string", "minLength": 1},
            "schema_ref": {"type": ["string", "null"]},
            "model_visible_summary": {"type": ["string", "null"]}
        }),
    )
}

fn gate_schema() -> Value {
    base_schema(
        "moyai.harness.quality_gate_result.v1",
        "QualityGateResult",
        &[
            "gate_id",
            "gate_kind",
            "status",
            "severity",
            "owner",
            "summary",
            "evidence_refs",
            "event_refs",
            "contract_refs",
            "next_actions",
        ],
        json!({
            "gate_id": {"type": "string", "minLength": 26},
            "gate_kind": {"type": "string"},
            "status": {"type": "string"},
            "severity": {"type": "string"},
            "owner": {"type": ["string", "null"]},
            "summary": {"type": "string", "minLength": 1},
            "evidence_refs": {"type": "array", "items": {"type": "string"}},
            "event_refs": {"type": "array", "items": {"type": "string"}},
            "contract_refs": {"type": "array"},
            "next_actions": {"type": "array"}
        }),
    )
}

fn replay_report_schema() -> Value {
    base_schema(
        "moyai.harness.replay_report.v1",
        "ReplayReport",
        &[
            "schema_version",
            "run_id",
            "status",
            "primary_owner",
            "summary",
            "gate_results",
            "restart_point",
            "next_actions",
        ],
        json!({
            "schema_version": {"type": "string", "const": "replay.report.v1"},
            "run_id": {"type": "string", "minLength": 26},
            "status": {"type": "string"},
            "primary_owner": {"type": ["string", "null"]},
            "summary": {"type": "string", "minLength": 1},
            "gate_results": {"type": "array"},
            "restart_point": {"type": ["string", "null"]},
            "next_actions": {"type": "array", "items": {"type": "string"}}
        }),
    )
}

fn repair_operation_template_schema() -> Value {
    base_schema(
        "moyai.harness.repair_operation_template.v1",
        "RepairOperationTemplate",
        &[
            "operation_id",
            "operation_kind",
            "source_test_ownership",
            "required_edit_surface",
            "forbidden_stale_tools",
            "evidence_markers",
            "sibling_obligations",
        ],
        json!({
            "operation_id": {"type": "string", "minLength": 1},
            "operation_kind": {"type": "string", "minLength": 1},
            "exact_target": {"type": ["string", "null"]},
            "source_test_ownership": {"type": "string", "minLength": 1},
            "required_edit_surface": {"type": "array", "items": {"type": "string"}},
            "forbidden_stale_tools": {"type": "array", "items": {"type": "string"}},
            "verification_rerun_condition": {"type": ["string", "null"]},
            "evidence_markers": {"type": "array", "items": {"type": "string"}},
            "sibling_obligations": {"type": "array", "items": {"type": "string"}},
            "repair_intent": {
                "type": ["object", "null"],
                "additionalProperties": false,
                "properties": {
                    "repair_owner": {"type": "string"},
                    "rollback_depth": {"type": "string"},
                    "recovery_action": {"type": "string"},
                    "required_edit_intent": {"type": "string"},
                    "required_evidence": {"type": "array", "items": {"type": "string"}},
                    "progress_evidence": {"type": "array", "items": {"type": "string"}},
                    "forbidden_directions": {"type": "array", "items": {"type": "string"}}
                }
            }
        }),
    )
}

fn verification_failure_cluster_schema() -> Value {
    base_schema(
        "moyai.harness.verification_failure_cluster.v1",
        "VerificationFailureCluster",
        &["cluster_id", "failing_labels", "sibling_obligations"],
        json!({
            "cluster_id": {"type": "string", "minLength": 1},
            "failing_labels": {"type": "array", "items": {"type": "string"}},
            "primary_failure": {"type": ["string", "null"]},
            "sibling_obligations": {"type": "array", "items": {"type": "string"}},
            "source_refs": {"type": "array", "items": {"type": "string"}},
            "test_refs": {"type": "array", "items": {"type": "string"}}
        }),
    )
}

fn repair_control_snapshot_schema() -> Value {
    base_schema(
        "moyai.harness.repair_control_snapshot.v1",
        "RepairControlSnapshot",
        &[
            "admitted",
            "admission_reason",
            "repair_subtype",
            "repair_owner",
            "selected_recovery_action",
            "rollback_depth",
            "allowed_surface_snapshot",
            "hard_invariants",
            "recovery_choices",
            "forbidden_actions",
            "progress_evidence",
        ],
        json!({
            "admitted": {"type": "boolean"},
            "admission_reason": {"type": "string", "minLength": 1},
            "repair_subtype": {"type": "string", "minLength": 1},
            "repair_owner": {"type": "string", "minLength": 1},
            "selected_recovery_action": {"type": "string", "minLength": 1},
            "rollback_depth": {"type": "string", "minLength": 1},
            "operation_id": {"type": ["string", "null"]},
            "required_target": {"type": ["string", "null"]},
            "allowed_surface_snapshot": {"type": "array", "items": {"type": "string"}},
            "hard_invariants": {"type": "array", "items": {"type": "string"}},
            "recovery_choices": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["recovery_action", "rollback_depth"],
                    "additionalProperties": false,
                    "properties": {
                        "recovery_action": {"type": "string"},
                        "rollback_depth": {"type": "string"},
                        "allowed_tools": {"type": "array", "items": {"type": "string"}},
                        "required_evidence": {"type": "array", "items": {"type": "string"}},
                        "forbidden_directions": {"type": "array", "items": {"type": "string"}},
                        "progress_evidence": {"type": "array", "items": {"type": "string"}}
                    }
                }
            },
            "forbidden_actions": {"type": "array", "items": {"type": "string"}},
            "progress_evidence": {"type": "array", "items": {"type": "string"}},
            "verification_rerun_condition": {"type": ["string", "null"]},
            "verification_cluster_id": {"type": ["string", "null"]}
        }),
    )
}

fn contract_reconciliation_schema() -> Value {
    base_schema(
        "moyai.harness.contract_reconciliation.v1",
        "ContractReconciliation",
        &[
            "owner",
            "strict_contract_active",
            "requirement_ids",
            "source_repair_allowed",
            "test_repair_allowed",
            "reason",
            "evidence",
        ],
        json!({
            "owner": {
                "type": "string",
                "enum": [
                    "SourceViolatesContract",
                    "SourceTestContractMismatch",
                    "TestViolatesContract",
                    "GeneratedTestOutOfScope",
                    "ContractInsufficient",
                    "HarnessInvariantViolation",
                    "GeneratedTestInsufficient",
                    "ProviderCapabilityMismatch",
                    "ToolOrEnvironmentFailure",
                    "OracleConflict"
                ]
            },
            "strict_contract_active": {"type": "boolean"},
            "requirement_ids": {"type": "array", "items": {"type": "string"}},
            "required_target": {"type": ["string", "null"]},
            "source_repair_allowed": {"type": "boolean"},
            "test_repair_allowed": {"type": "boolean"},
            "reason": {"type": "string", "minLength": 1},
            "evidence": {"type": "array", "items": {"type": "string"}}
        }),
    )
}

fn route_manifest_schema() -> Value {
    base_schema(
        "moyai.manual_st.route_manifest.v1",
        "ManualStRouteManifest",
        &[
            "route_id",
            "case_ids",
            "route_type",
            "build_identifier",
            "model_id",
            "provider_base_url",
            "provider_metadata_summary",
            "scenario_contract_hash",
            "fixture_version",
            "workspace_path",
            "session_id",
            "start_time",
            "end_time",
            "route_level_verdict",
        ],
        json!({
            "route_id": {"type": "string", "minLength": 1},
            "case_ids": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "route_type": {
                "type": "string",
                "enum": [
                    "required_core",
                    "required_vision",
                    "targeted_support",
                    "extended",
                    "probe"
                ]
            },
            "build_identifier": {"type": "string", "minLength": 1},
            "model_id": {"type": "string", "minLength": 1},
            "provider_base_url": {"type": "string", "minLength": 1},
            "provider_metadata_summary": {"type": "object"},
            "provider_metadata_hash": {"type": ["string", "null"]},
            "scenario_contract_hash": {"type": ["string", "null"]},
            "fixture_version": {"type": "string", "minLength": 1},
            "workspace_path": {"type": "string", "minLength": 1},
            "session_id": {"type": ["string", "null"]},
            "start_time": {"type": "string", "minLength": 1},
            "end_time": {"type": "string", "minLength": 1},
            "route_level_verdict": {"type": "string", "enum": ["pass", "fail", "blocked", "running", "not_run"]},
            "active_case_id": {"type": ["string", "null"]},
            "progress_status": {"type": ["string", "null"]},
            "last_progress_at": {"type": ["string", "null"]},
            "evidence_artifacts": {"type": "array", "items": {"type": "string"}}
        }),
    )
}

fn case_progress_schema() -> Value {
    base_schema(
        "moyai.manual_st.case_progress.v1",
        "ManualStCaseProgress",
        &[
            "route_id",
            "route_type",
            "route_level_verdict",
            "active_case_id",
            "stage_index",
            "stage_label",
            "session_id",
            "progress_status",
            "last_progress_at",
            "workspace_path",
            "case_artifact_root",
            "harness_event_root",
            "evidence_artifact_schema_version",
        ],
        json!({
            "route_id": {"type": "string", "minLength": 1},
            "route_type": {
                "type": "string",
                "enum": [
                    "required_core",
                    "required_vision",
                    "targeted_support",
                    "extended",
                    "probe"
                ]
            },
            "route_level_verdict": {"type": "string", "enum": ["pass", "fail", "blocked", "running", "not_run"]},
            "active_case_id": {"type": ["string", "null"]},
            "stage_index": {"type": ["integer", "null"], "minimum": 1},
            "stage_label": {"type": ["string", "null"]},
            "session_id": {"type": ["string", "null"]},
            "progress_status": {
                "type": "string",
                "enum": [
                    "route_artifact_written",
                    "route_running",
                    "case_running",
                    "case_started",
                    "model_request_inflight",
                    "runtime_completed",
                    "runtime_non_completed",
                    "runtime_error",
                    "turn_timeout",
                    "route_verification_evaluating",
                    "closeout_continuation_pending",
                    "stage_clean_closeout",
                    "case_completed",
                    "case_terminalized",
                    "route_terminalized"
                ]
            },
            "last_progress_at": {"type": "string", "minLength": 1},
            "workspace_path": {"type": ["string", "null"]},
            "case_artifact_root": {"type": ["string", "null"]},
            "harness_event_root": {"type": ["string", "null"]},
            "evidence_artifact_schema_version": {"type": "string", "const": "manual_st.case_progress.v1"},
            "note": {"type": ["string", "null"]}
        }),
    )
}

fn verification_command_log_schema() -> Value {
    base_schema(
        "moyai.manual_st.verification_command_log.v1",
        "ManualStVerificationCommandLog",
        &["commands"],
        json!({
            "commands": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": [
                        "command",
                        "working_directory",
                        "start_time",
                        "end_time",
                        "exit_code",
                        "stdout_summary",
                        "stderr_summary",
                        "normalized_failure_class",
                        "required",
                        "case_id"
                    ],
                    "additionalProperties": false,
                    "properties": {
                        "command": {"type": "string", "minLength": 1},
                        "working_directory": {"type": "string", "minLength": 1},
                        "start_time": {"type": "string", "minLength": 1},
                        "end_time": {"type": "string", "minLength": 1},
                        "exit_code": {"type": ["integer", "null"]},
                        "stdout_summary": {"type": "string"},
                        "stderr_summary": {"type": "string"},
                        "normalized_failure_class": {"type": ["string", "null"]},
                        "required": {"type": "boolean"},
                        "case_id": {"type": "string", "minLength": 1},
                        "requirement_id": {"type": ["string", "null"]}
                    }
                }
            }
        }),
    )
}

fn contract_reconciliation_report_schema() -> Value {
    base_schema(
        "moyai.manual_st.contract_reconciliation_report.v1",
        "ManualStContractReconciliationReport",
        &[
            "case_id",
            "failure_owner",
            "source_repair_allowed",
            "generated_test_repair_allowed",
            "contract_update_required",
            "harness_invariant_violation",
            "final_reconciliation_verdict",
            "contract_refs",
            "generated_test_requirement_coverage",
        ],
        json!({
            "case_id": {"type": "string", "minLength": 1},
            "failure_owner": {
                "type": "string",
                "enum": [
                    "SourceViolatesContract",
                    "SourceTestContractMismatch",
                    "TestViolatesContract",
                    "GeneratedTestOutOfScope",
                    "ContractInsufficient",
                    "HarnessInvariantViolation",
                    "GeneratedTestInsufficient",
                    "ProviderCapabilityMismatch",
                    "ToolOrEnvironmentFailure",
                    "OracleConflict"
                ]
            },
            "requirement_id": {"type": ["string", "null"]},
            "assertion_subject": {"type": ["string", "null"]},
            "expected": {"type": ["string", "null"]},
            "observed": {"type": ["string", "null"]},
            "source_repair_allowed": {"type": "boolean"},
            "generated_test_repair_allowed": {"type": "boolean"},
            "contract_update_required": {"type": "boolean"},
            "harness_invariant_violation": {"type": "boolean"},
            "final_reconciliation_verdict": {
                "type": "string",
                "enum": [
                    "source_repair",
                    "source_test_contract_reconciliation",
                    "generated_test_repair",
                    "contract_update",
                    "harness_fix",
                    "provider_or_environment_fix",
                    "oracle_conflict",
                    "report_only",
                    "blocked"
                ]
            },
            "contract_refs": {"type": "array", "items": {"type": "string"}},
            "generated_test_requirement_coverage": {
                "type": "object",
                "required": [
                    "missing_requirement_ids",
                    "out_of_scope_assertion_ids",
                    "contract_conflict_assertion_ids",
                    "unlisted_public_terms",
                    "unlisted_constructor_keywords",
                    "self_consistency_blocker_ids"
                ],
                "properties": {
                    "missing_requirement_ids": {"type": "array", "items": {"type": "string"}},
                    "out_of_scope_assertion_ids": {"type": "array", "items": {"type": "string"}},
                    "contract_conflict_assertion_ids": {"type": "array", "items": {"type": "string"}},
                    "unlisted_public_terms": {"type": "array", "items": {"type": "string"}},
                    "unlisted_constructor_keywords": {"type": "array", "items": {"type": "string"}},
                    "self_consistency_blocker_ids": {"type": "array", "items": {"type": "string"}}
                },
                "additionalProperties": false
            },
            "evidence_refs": {"type": "array", "items": {"type": "string"}}
        }),
    )
}

fn workspace_diff_manifest_schema() -> Value {
    base_schema(
        "moyai.manual_st.workspace_diff_manifest.v1",
        "ManualStWorkspaceDiffManifest",
        &[
            "expected_artifacts",
            "actual_added_files",
            "actual_modified_files",
            "actual_deleted_files",
            "unexpected_outside_workspace_access_or_change",
            "fixture_input_mutation",
            "verdict",
        ],
        json!({
            "expected_artifacts": {"type": "array", "items": {"type": "string"}},
            "actual_added_files": {"type": "array", "items": {"type": "string"}},
            "actual_modified_files": {"type": "array", "items": {"type": "string"}},
            "actual_deleted_files": {"type": "array", "items": {"type": "string"}},
            "unexpected_outside_workspace_access_or_change": {"type": "boolean"},
            "fixture_input_mutation": {"type": "boolean"},
            "verdict": {"type": "string", "enum": ["clean", "dirty", "blocked"]},
            "diagnostics": {"type": "array", "items": {"type": "string"}}
        }),
    )
}

fn request_payload_summary_schema() -> Value {
    base_schema(
        "moyai.manual_st.request_payload_summary.v1",
        "ManualStRequestPayloadSummary",
        &[
            "model",
            "provider",
            "tool_settings",
            "image_part_present",
            "image_count",
            "request_diagnostics_summary",
            "provider_metadata_summary",
            "vision_capability_evidence",
        ],
        json!({
            "model": {"type": "string", "minLength": 1},
            "provider": {"type": "string", "minLength": 1},
            "tool_settings": {"type": "object"},
            "image_part_present": {"type": "boolean"},
            "image_count": {"type": "integer", "minimum": 0},
            "request_diagnostics_summary": {"type": "object"},
            "context_size": {"type": ["integer", "null"], "minimum": 0},
            "provider_metadata_summary": {"type": "object"},
            "vision_capability_evidence": {"type": ["string", "null"]}
        }),
    )
}

fn timeout_classification_schema() -> Value {
    base_schema(
        "moyai.manual_st.timeout_classification.v1",
        "ManualStTimeoutClassification",
        &[
            "provider_stream_stall",
            "verification_non_convergence",
            "repeated_no_progress_repair",
            "tool_or_environment_stall",
            "outer_timeout",
            "classified_terminal_before_timeout",
        ],
        json!({
            "provider_stream_stall": {"type": "boolean"},
            "verification_non_convergence": {"type": "boolean"},
            "repeated_no_progress_repair": {"type": "boolean"},
            "tool_or_environment_stall": {"type": "boolean"},
            "outer_timeout": {"type": "boolean"},
            "classified_terminal_before_timeout": {"type": "boolean"},
            "primary_timeout_owner": {"type": ["string", "null"]},
            "evidence_refs": {"type": "array", "items": {"type": "string"}}
        }),
    )
}

fn ahe_lite_change_manifest_schema() -> Value {
    base_schema(
        "moyai.harness.ahe_lite_change_manifest.v1",
        "AheLiteChangeManifest",
        &[
            "change_id",
            "affected_component",
            "evidence",
            "root_cause",
            "expected_fix",
            "expected_regression_risk",
            "rollback_condition",
            "next_eval_required",
        ],
        json!({
            "change_id": {"type": "string", "minLength": 1},
            "affected_component": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "evidence": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "root_cause": {"type": "string", "minLength": 1},
            "expected_fix": {"type": "array", "items": {"type": "string"}, "minItems": 1},
            "expected_regression_risk": {"type": "array", "items": {"type": "string"}},
            "rollback_condition": {"type": "string", "minLength": 1},
            "next_eval_required": {"type": "array", "items": {"type": "string"}, "minItems": 1}
        }),
    )
}

fn completed_todo_evidence_schema() -> Value {
    base_schema(
        "moyai.harness.completed_todo_evidence.v1",
        "CompletedTodoEvidenceState",
        &[
            "status",
            "contradicted_todos",
            "missing_evidence_todos",
            "evidence_refs",
        ],
        json!({
            "status": {"type": "string", "minLength": 1},
            "contradicted_todos": {"type": "array", "items": {"type": "string"}},
            "missing_evidence_todos": {"type": "array", "items": {"type": "string"}},
            "evidence_refs": {"type": "array", "items": {"type": "string"}}
        }),
    )
}

fn tool_no_progress_signature_schema() -> Value {
    base_schema(
        "moyai.harness.tool_no_progress_signature.v1",
        "ToolNoProgressSignature",
        &[
            "result_hash",
            "tool",
            "progress_effect",
            "allowed_surface_snapshot",
            "repeat_count",
        ],
        json!({
            "result_hash": {"type": "string", "pattern": "^[a-f0-9]{64}$"},
            "tool": {"type": ["string", "null"]},
            "progress_effect": {"type": "string", "const": "no_progress"},
            "blocked_action": {"type": ["string", "null"]},
            "allowed_surface_snapshot": {"type": "array", "items": {"type": "string"}},
            "repeat_count": {"type": "integer", "minimum": 0}
        }),
    )
}
