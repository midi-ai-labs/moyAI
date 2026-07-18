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
        ("manual_st.route_manifest.v1.json", route_manifest_schema()),
        ("manual_st.case_progress.v1.json", case_progress_schema()),
        (
            "manual_st.verification_command_log.v1.json",
            verification_command_log_schema(),
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
            "tool_or_environment_stall",
            "outer_timeout",
            "classified_terminal_before_timeout",
        ],
        json!({
            "provider_stream_stall": {"type": "boolean"},
            "verification_non_convergence": {"type": "boolean"},
            "tool_or_environment_stall": {"type": "boolean"},
            "outer_timeout": {"type": "boolean"},
            "classified_terminal_before_timeout": {"type": "boolean"},
            "primary_timeout_owner": {"type": ["string", "null"]},
            "evidence_refs": {"type": "array", "items": {"type": "string"}}
        }),
    )
}
