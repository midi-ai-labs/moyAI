use std::collections::BTreeSet;

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Map, Value, json};

use crate::cli::ConfirmationPrompt;
use crate::config::ResolvedConfig;
use crate::error::{AgentError, CliPromptError, ToolError};
use crate::protocol::{
    CandidateRepairEdit, OperationIntent, RejectedToolProposal, ToolProgressEffect,
    VerificationRunResult, VerificationRunStatus,
};
use crate::runtime::{RunEventSink, build_cancel_token};
use crate::session::repository::SessionRepository;
use crate::session::{
    DiffSummaryPart, FailureKind, MessageId, MessagePart, NewPart, PartKind, SessionContext,
    SessionId, ToolCallId, ToolCallPart, ToolCallRecord, ToolCallStatus, ToolResultPart,
    VerificationFailureCluster,
};
use crate::storage::SqliteSessionRepository;
use crate::tool::context::{ToolContext, ToolServices};
use crate::tool::registry::ToolRegistry;
use crate::tool::{ToolName, ToolResult};
use crate::workspace::Workspace;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolRouteRequest<'a> {
    pub requested_tool: String,
    pub effective_tool: String,
    pub record_tool: String,
    pub original_arguments_json: String,
    pub effective_arguments_json: String,
    pub allowed_tool_names: &'a BTreeSet<String>,
    pub tool_exists: bool,
    pub tool_allowed: bool,
    pub redirected_from_arguments_json: Option<String>,
    pub redirect_reason: Option<&'a str>,
    pub tool_choice: Option<&'a str>,
    pub control_projection: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ToolRouteDecision {
    pub requested_tool: String,
    pub effective_tool: String,
    pub record_tool: String,
    pub original_arguments_json: String,
    pub effective_arguments_json: String,
    pub tool_exists: bool,
    pub tool_allowed: bool,
    metadata: Value,
}

pub(crate) struct ToolExecutionRequest<'a> {
    pub session: &'a SessionContext,
    pub workspace: &'a Workspace,
    pub config: &'a ResolvedConfig,
    pub tool_call_id: ToolCallId,
    pub prompt: &'a mut dyn ConfirmationPrompt,
    pub services: &'a ToolServices,
}

pub(crate) struct ToolOrchestrator;

impl ToolOrchestrator {
    pub(crate) fn route(request: ToolRouteRequest<'_>) -> ToolRouteDecision {
        let allowed_tools = request
            .allowed_tool_names
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let original_arguments = arguments_value(&request.original_arguments_json);
        let effective_arguments = arguments_value(&request.effective_arguments_json);
        let adjusted_arguments = (request.original_arguments_json
            != request.effective_arguments_json)
            .then_some(effective_arguments.clone());
        let repaired_tool = (request.requested_tool != request.effective_tool)
            .then(|| request.effective_tool.clone());
        let route_snapshot = json!({
            "requested_tool": request.requested_tool.clone(),
            "effective_tool": request.effective_tool.clone(),
            "record_tool": request.record_tool.clone(),
            "resolved_tool": request.record_tool.clone(),
            "repaired_tool": repaired_tool.clone(),
            "tool_exists": request.tool_exists,
            "tool_allowed": request.tool_allowed,
            "allowed_tools": allowed_tools,
            "original_arguments": original_arguments,
            "adjusted_arguments": adjusted_arguments,
            "original_arguments_json": request.original_arguments_json.clone(),
            "effective_arguments_json": request.effective_arguments_json.clone(),
            "redirected_from_arguments": request.redirected_from_arguments_json.clone(),
            "tool_redirect_reason": request.redirect_reason,
            "tool_choice": request.tool_choice,
            "control_projection": request.control_projection.clone(),
            "permission_decision": "not_required",
            "sandbox_decision": {
                "profile": "workspace_write",
                "network_allowed": false,
                "escalated": false
            },
            "retry_policy": {
                "owner": "tool_orchestrator",
                "decision": "not_scheduled"
            },
            "terminal_guard_policy": {
                "owner": "tool_orchestrator",
                "no_progress_guard": true,
                "result_hash_required": true
            },
        });
        let metadata = json!({
            "tool_route": route_snapshot,
            "requested_tool": request.requested_tool.clone(),
            "effective_tool": request.effective_tool.clone(),
            "record_tool": request.record_tool.clone(),
            "resolved_tool": request.record_tool.clone(),
            "repaired_tool": repaired_tool,
            "tool_exists": request.tool_exists,
            "tool_allowed": request.tool_allowed,
            "allowed_tools": request.allowed_tool_names.iter().cloned().collect::<Vec<_>>(),
            "original_arguments": arguments_value(&request.original_arguments_json),
            "adjusted_arguments": adjusted_arguments,
            "original_arguments_json": request.original_arguments_json.clone(),
            "effective_arguments_json": request.effective_arguments_json.clone(),
            "redirected_from_arguments": request.redirected_from_arguments_json,
            "tool_redirect_reason": request.redirect_reason,
            "tool_choice": request.tool_choice,
            "control_projection": request.control_projection,
            "permission_decision": "not_required",
            "sandbox_decision": {
                "profile": "workspace_write",
                "network_allowed": false,
                "escalated": false
            },
            "retry_policy": {
                "owner": "tool_orchestrator",
                "decision": "not_scheduled"
            },
            "terminal_guard_policy": {
                "owner": "tool_orchestrator",
                "no_progress_guard": true,
                "result_hash_required": true
            },
        });

        ToolRouteDecision {
            requested_tool: request.requested_tool,
            effective_tool: request.effective_tool,
            record_tool: request.record_tool,
            original_arguments_json: request.original_arguments_json,
            effective_arguments_json: request.effective_arguments_json,
            tool_exists: request.tool_exists,
            tool_allowed: request.tool_allowed,
            metadata,
        }
    }

    pub(crate) async fn record_pending_call(
        session_repo: &SqliteSessionRepository,
        session_id: SessionId,
        assistant_message_id: MessageId,
        route: &ToolRouteDecision,
        sink: &mut dyn RunEventSink,
    ) -> Result<ToolCallRecord, AgentError> {
        let record = session_repo
            .insert_tool_call(
                session_id,
                assistant_message_id,
                &route.record_tool,
                &route.effective_arguments_json,
                Some(&route.requested_tool),
                route.pending_metadata(),
            )
            .await?;
        sink.emit(crate::session::RunEvent::ToolCallPending {
            tool_call_id: record.id,
            tool: record.tool_name,
            title: route.requested_tool.clone(),
            metadata: route.pending_metadata(),
        })?;
        session_repo
            .append_part(
                assistant_message_id,
                NewPart {
                    kind: PartKind::ToolCall,
                    payload: MessagePart::ToolCall(ToolCallPart {
                        tool_call_id: record.id,
                        tool_name: record.tool_name,
                        arguments_json: route.effective_arguments_json.clone(),
                        model_arguments_json: Some(route.original_arguments_json.clone()),
                        effective_arguments_json: Some(route.effective_arguments_json.clone()),
                    }),
                },
            )
            .await?;
        Ok(record)
    }

    pub(crate) async fn mark_running(
        session_repo: &SqliteSessionRepository,
        tool_call_id: ToolCallId,
    ) -> Result<(), AgentError> {
        session_repo.mark_tool_call_running(tool_call_id).await?;
        Ok(())
    }

    pub(crate) async fn execute_registered_call(
        registry: &ToolRegistry,
        effective_tool_name: &str,
        parsed_arguments: Value,
        request: ToolExecutionRequest<'_>,
        sink: &mut dyn RunEventSink,
    ) -> Result<ToolResult, ToolError> {
        let mut prompt = LifecycleConfirmationPrompt {
            inner: request.prompt,
            tool_call_id: request.tool_call_id,
            sink,
        };
        registry
            .execute(
                effective_tool_name,
                parsed_arguments,
                ToolContext {
                    session: request.session,
                    workspace: request.workspace,
                    config: request.config,
                    tool_call_id: request.tool_call_id,
                    cancel: build_cancel_token(),
                    prompt: &mut prompt,
                    services: request.services,
                },
            )
            .await
    }

    pub(crate) async fn complete_corrective_call(
        session_repo: &SqliteSessionRepository,
        assistant_message_id: MessageId,
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        result: &ToolResult,
        route: &ToolRouteDecision,
        sink: &mut dyn RunEventSink,
    ) -> Result<(), AgentError> {
        Self::complete_text_call(
            session_repo,
            assistant_message_id,
            tool_call_id,
            tool_name,
            &result.title,
            &result.output_text,
            result.metadata.clone(),
            None,
            route,
            sink,
        )
        .await
        .map(|_| ())
    }

    pub(crate) async fn complete_text_call(
        session_repo: &SqliteSessionRepository,
        assistant_message_id: MessageId,
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        title: &str,
        summary: &str,
        result_metadata: Value,
        truncated_output_path: Option<&Utf8Path>,
        route: &ToolRouteDecision,
        sink: &mut dyn RunEventSink,
    ) -> Result<Value, AgentError> {
        let metadata = with_verification_run_result(
            tool_name,
            summary,
            route.completion_metadata(result_metadata),
            truncated_output_path,
        );
        session_repo
            .complete_tool_call(
                tool_call_id,
                title,
                metadata.clone(),
                summary,
                truncated_output_path,
            )
            .await?;
        append_tool_result_part(
            session_repo,
            assistant_message_id,
            tool_call_id,
            title,
            summary,
            &metadata,
        )
        .await?;
        if let Some(proposal) = rejected_tool_proposal_from_metadata(&metadata) {
            sink.emit(crate::session::RunEvent::ToolProposalRejected {
                tool_call_id,
                proposal,
            })?;
        }
        if let Some(candidate) = candidate_repair_edit_from_metadata(&metadata) {
            sink.emit(crate::session::RunEvent::CandidateRepairEditRecorded {
                tool_call_id,
                candidate,
            })?;
        }
        sink.emit(crate::session::RunEvent::ToolCallCompleted {
            tool_call_id,
            tool: tool_name,
            title: title.to_string(),
            summary: summary.to_string(),
            metadata: metadata.clone(),
        })?;
        Ok(metadata)
    }

    pub(crate) async fn complete_executed_call(
        session_repo: &SqliteSessionRepository,
        assistant_message_id: MessageId,
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        result: &ToolResult,
        route: &ToolRouteDecision,
        workspace_root: &Utf8Path,
        active_targets: &[Utf8PathBuf],
        sink: &mut dyn RunEventSink,
    ) -> Result<Value, AgentError> {
        let result_metadata =
            classify_executed_result_for_operation_intent(tool_name, result, route);
        let metadata = with_active_targets_for_operation_feedback(
            with_verification_run_result(
                tool_name,
                &result.output_text,
                route.completion_metadata(result_metadata),
                result.truncated_output_path.as_deref(),
            ),
            active_targets,
        );
        let provider_output_text =
            render_provider_visible_operation_progress_feedback(&result.output_text, &metadata);
        session_repo
            .complete_tool_call(
                tool_call_id,
                &result.title,
                metadata.clone(),
                &provider_output_text,
                result.truncated_output_path.as_deref(),
            )
            .await?;
        append_tool_result_part(
            session_repo,
            assistant_message_id,
            tool_call_id,
            &result.title,
            &provider_output_text,
            &metadata,
        )
        .await?;
        if !result.recorded_changes.is_empty() {
            let summary = result
                .change_summaries
                .iter()
                .map(|change| change.summary_line(Some(workspace_root)))
                .collect::<Vec<_>>()
                .join("\n");
            session_repo
                .append_part(
                    assistant_message_id,
                    NewPart {
                        kind: PartKind::DiffSummary,
                        payload: MessagePart::DiffSummary(DiffSummaryPart {
                            change_ids: result.recorded_changes.clone(),
                            changes: result
                                .change_summaries
                                .iter()
                                .map(|change| crate::protocol::FileChangeEvidence {
                                    change_id: change.change_id,
                                    kind: change.kind,
                                    path_before: change.path_before.clone(),
                                    path_after: change.path_after.clone(),
                                    summary: change.summary_line(Some(workspace_root)),
                                })
                                .collect(),
                            summary,
                        }),
                    },
                )
                .await?;
            sink.emit(crate::session::RunEvent::FileChangesRecorded {
                tool_call_id,
                changes: result.change_summaries.clone(),
            })?;
        }
        sink.emit(crate::session::RunEvent::ToolCallCompleted {
            tool_call_id,
            tool: tool_name,
            title: result.title.clone(),
            summary: provider_output_text,
            metadata: metadata.clone(),
        })?;
        Ok(metadata)
    }

    pub(crate) async fn fail_executed_call(
        session_repo: &SqliteSessionRepository,
        assistant_message_id: MessageId,
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        error_text: &str,
        route: &ToolRouteDecision,
        sink: &mut dyn RunEventSink,
    ) -> Result<(), AgentError> {
        let metadata = route.completion_metadata(tool_failure_metadata(error_text, route));
        session_repo
            .fail_tool_call(tool_call_id, error_text)
            .await?;
        append_tool_result_part(
            session_repo,
            assistant_message_id,
            tool_call_id,
            "Tool failed",
            error_text,
            &metadata,
        )
        .await?;
        sink.emit(crate::session::RunEvent::ToolCallFailed {
            tool_call_id,
            tool: tool_name,
            error: error_text.to_string(),
            metadata,
        })?;
        Ok(())
    }
}

fn tool_failure_metadata(error_text: &str, route: &ToolRouteDecision) -> Value {
    let allowed_surface = route
        .metadata
        .get("tool_route")
        .and_then(|tool_route| tool_route.get("allowed_tools"))
        .cloned()
        .or_else(|| route.metadata.get("allowed_tools").cloned())
        .unwrap_or_else(|| json!([]));
    let failed_tool_call = json!({
        "tool": route.effective_tool,
        "arguments": arguments_value(&route.effective_arguments_json),
        "arguments_hash": crate::harness::artifact::hash_bytes(
            normalized_arguments_for_hash(&route.effective_arguments_json).as_bytes(),
        ),
    });
    let result_hash = crate::harness::artifact::hash_bytes(
        format!(
            "tool_failure|{}|{}|{}",
            route.effective_tool,
            normalized_arguments_for_hash(&route.effective_arguments_json),
            tool_error_class(error_text)
        )
        .as_bytes(),
    );
    json!({
        "tool_error": error_text,
        "success": false,
        "progress_effect": "blocked",
        "failed_tool_call": failed_tool_call.clone(),
        "result_hash": result_hash.clone(),
        "tool_feedback_envelope": {
            "kind": "executed_tool_failure",
            "success": false,
            "progress_effect": "blocked",
            "failed_tool_call": failed_tool_call,
            "allowed_surface_snapshot": allowed_surface,
            "result_hash": result_hash,
            "side_effects_applied": false,
            "error_class": tool_error_class(error_text)
        }
    })
}

fn classify_executed_result_for_operation_intent(
    tool_name: ToolName,
    result: &ToolResult,
    route: &ToolRouteDecision,
) -> Value {
    let metadata = result.metadata.clone();
    if !route_has_operation_intent(route, OperationIntent::ContentChangingAuthoringRequired) {
        return metadata;
    }

    let progress_class = operation_progress_class(tool_name, result);
    let progress_effect = operation_progress_effect(progress_class);
    let operation_intent = OperationIntent::ContentChangingAuthoringRequired.as_str();
    let result_hash = crate::harness::artifact::hash_bytes(
        format!(
            "operation_progress|{}|{}|{}|{}",
            operation_intent,
            tool_name,
            progress_class,
            normalized_arguments_for_hash(&route.effective_arguments_json)
        )
        .as_bytes(),
    );

    let mut object = match metadata {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            if !other.is_null() {
                map.insert("tool_result_metadata".to_string(), other);
            }
            map
        }
    };
    object.insert(
        "operation_intent".to_string(),
        Value::String(operation_intent.to_string()),
    );
    object.insert(
        "operation_progress_class".to_string(),
        Value::String(progress_class.to_string()),
    );
    object.insert(
        "progress_effect".to_string(),
        Value::String(progress_effect.to_string()),
    );
    object.insert(
        "result_hash".to_string(),
        Value::String(result_hash.clone()),
    );

    let mut feedback = object
        .remove("tool_feedback_envelope")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    feedback.insert(
        "kind".to_string(),
        Value::String("operation_progress_classification".to_string()),
    );
    feedback.insert(
        "operation_intent".to_string(),
        Value::String(operation_intent.to_string()),
    );
    feedback.insert(
        "operation_progress_class".to_string(),
        Value::String(progress_class.to_string()),
    );
    feedback.insert(
        "progress_effect".to_string(),
        Value::String(progress_effect.to_string()),
    );
    feedback.insert("result_hash".to_string(), Value::String(result_hash));
    feedback.insert(
        "side_effects_applied".to_string(),
        Value::Bool(!result.recorded_changes.is_empty() || !result.change_summaries.is_empty()),
    );
    feedback.insert(
        "content_changing_progress_required".to_string(),
        Value::Bool(true),
    );
    object.insert(
        "tool_feedback_envelope".to_string(),
        Value::Object(feedback),
    );

    Value::Object(object)
}

fn with_active_targets_for_operation_feedback(
    metadata: Value,
    active_targets: &[Utf8PathBuf],
) -> Value {
    if active_targets.is_empty() {
        return metadata;
    }
    let operation_intent = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_intent"))
        .or_else(|| metadata.get("operation_intent"))
        .and_then(Value::as_str);
    let progress_effect = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("progress_effect"))
        .or_else(|| metadata.get("progress_effect"))
        .and_then(Value::as_str);
    if operation_intent != Some(OperationIntent::ContentChangingAuthoringRequired.as_str())
        || progress_effect != Some("no_progress")
    {
        return metadata;
    }

    let active_target_values = active_targets
        .iter()
        .map(|target| Value::String(target.as_str().to_string()))
        .collect::<Vec<_>>();
    let active_targets_value = Value::Array(active_target_values);
    let mut object = match metadata {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            if !other.is_null() {
                map.insert("tool_result_metadata".to_string(), other);
            }
            map
        }
    };
    let mut feedback = object
        .remove("tool_feedback_envelope")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    feedback.insert("active_targets".to_string(), active_targets_value.clone());
    feedback.insert(
        "active_target_count".to_string(),
        json!(active_targets.len()),
    );
    object.insert("active_targets".to_string(), active_targets_value);
    object.insert(
        "tool_feedback_envelope".to_string(),
        Value::Object(feedback),
    );

    Value::Object(object)
}

fn route_has_operation_intent(route: &ToolRouteDecision, intent: OperationIntent) -> bool {
    route_operation_intents(route)
        .iter()
        .any(|value| value == intent.as_str())
}

fn route_operation_intents(route: &ToolRouteDecision) -> Vec<String> {
    operation_intents_from_value(route.metadata.get("control_projection"))
        .or_else(|| {
            route.metadata.get("tool_route").and_then(|tool_route| {
                operation_intents_from_value(tool_route.get("control_projection"))
            })
        })
        .unwrap_or_default()
}

fn operation_intents_from_value(value: Option<&Value>) -> Option<Vec<String>> {
    value?.get("operation_intents")?.as_array().map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>()
    })
}

fn operation_progress_class(tool_name: ToolName, result: &ToolResult) -> &'static str {
    if !result.recorded_changes.is_empty() || !result.change_summaries.is_empty() {
        return "content_changing_progress";
    }
    if result
        .metadata
        .get("progress_projection")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return "progress_projection";
    }
    match tool_name {
        ToolName::List
        | ToolName::Glob
        | ToolName::Grep
        | ToolName::Read
        | ToolName::InspectDirectory
        | ToolName::Skill
        | ToolName::DoclingConvert
        | ToolName::McpCall
        | ToolName::TodoWrite => "supporting_context",
        ToolName::Write | ToolName::ApplyPatch => "no_progress",
        ToolName::Shell => "supporting_context",
        ToolName::Invalid => "blocked_failure",
    }
}

fn operation_progress_effect(progress_class: &str) -> &'static str {
    match progress_class {
        "content_changing_progress" => "made_progress",
        "blocked_failure" => "blocked",
        _ => "no_progress",
    }
}

fn render_provider_visible_operation_progress_feedback(
    output_text: &str,
    metadata: &Value,
) -> String {
    let operation_intent = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_intent"))
        .or_else(|| metadata.get("operation_intent"))
        .and_then(Value::as_str);
    let progress_class = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_progress_class"))
        .or_else(|| metadata.get("operation_progress_class"))
        .and_then(Value::as_str);
    let progress_effect = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("progress_effect"))
        .or_else(|| metadata.get("progress_effect"))
        .and_then(Value::as_str);
    if operation_intent != Some(OperationIntent::ContentChangingAuthoringRequired.as_str())
        || progress_effect != Some("no_progress")
    {
        return output_text.to_string();
    }
    let Some(progress_class) = progress_class else {
        return output_text.to_string();
    };
    if output_text.contains("[tool feedback]") {
        return output_text.to_string();
    }
    let active_targets = operation_feedback_active_targets(metadata);
    let active_target_line = if active_targets.is_empty() {
        String::new()
    } else {
        format!("\nactive_targets: {}", active_targets.join(", "))
    };
    let continuation = if active_targets.is_empty() {
        "Open executable authoring remains. Continue with a file-changing tool output that creates or updates the requested artifacts before verification or final answer.".to_string()
    } else {
        format!(
            "Open executable authoring remains for active target(s): {}. Continue with a file-changing tool output that creates or updates those active targets before verification or final answer.",
            active_targets.join(", ")
        )
    };
    let note = match progress_class {
        "progress_projection" => {
            "This plan update is recorded, but it did not create or modify any required workspace artifact."
        }
        "supporting_context" => {
            "This context output is recorded, but it did not create or modify any required workspace artifact."
        }
        "no_progress" => {
            "This tool output is recorded, but it did not create or modify any required workspace artifact."
        }
        _ => return output_text.to_string(),
    };
    format!(
        "{output_text}\n\n[tool feedback]\noperation_intent: content_changing_authoring_required\noperation_progress_class: {progress_class}\nprogress_effect: no_progress{active_target_line}\n{note}\n{continuation}"
    )
}

fn operation_feedback_active_targets(metadata: &Value) -> Vec<String> {
    metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("active_targets"))
        .or_else(|| metadata.get("active_targets"))
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn normalized_arguments_for_hash(arguments_json: &str) -> String {
    serde_json::from_str::<Value>(arguments_json)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| arguments_json.trim().to_string())
}

fn tool_error_class(error_text: &str) -> String {
    let lower = error_text.to_ascii_lowercase();
    if lower.contains("os error") || lower.contains("not found") || lower.contains("見つかりません")
    {
        "io_not_found".to_string()
    } else if lower.contains("permission") || lower.contains("denied") {
        "permission_denied".to_string()
    } else if lower.contains("timeout") {
        "timeout".to_string()
    } else {
        lower
            .split_whitespace()
            .take(8)
            .collect::<Vec<_>>()
            .join("_")
    }
}

pub(crate) fn open_authoring_operation_intent_classification_fixture_passes() -> bool {
    let allowed = BTreeSet::from([
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let route = ToolOrchestrator::route(ToolRouteRequest {
        requested_tool: "read".to_string(),
        effective_tool: "read".to_string(),
        record_tool: "read".to_string(),
        original_arguments_json: r#"{"path":"README.md"}"#.to_string(),
        effective_arguments_json: r#"{"path":"README.md"}"#.to_string(),
        allowed_tool_names: &allowed,
        tool_exists: true,
        tool_allowed: true,
        redirected_from_arguments_json: None,
        redirect_reason: None,
        tool_choice: Some("required"),
        control_projection: Some(json!({
            "operation_intents": ["content_changing_authoring_required"],
            "allowed_tools": ["read", "todowrite", "write"]
        })),
    });
    let read_result = ToolResult {
        title: "Read".to_string(),
        output_text: "README.md content".to_string(),
        metadata: json!({ "success": true }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    };
    let todo_result = ToolResult {
        title: "Plan updated".to_string(),
        output_text: "Plan updated".to_string(),
        metadata: json!({ "success": true, "progress_projection": true }),
        truncated_output_path: None,
        recorded_changes: Vec::new(),
        change_summaries: Vec::new(),
    };

    let active_targets = vec![Utf8PathBuf::from("test_source.py")];
    let read_metadata = with_active_targets_for_operation_feedback(
        classify_executed_result_for_operation_intent(ToolName::Read, &read_result, &route),
        &active_targets,
    );
    let todo_metadata = with_active_targets_for_operation_feedback(
        classify_executed_result_for_operation_intent(ToolName::TodoWrite, &todo_result, &route),
        &active_targets,
    );
    let read_output = render_provider_visible_operation_progress_feedback(
        &read_result.output_text,
        &read_metadata,
    );
    let todo_output = render_provider_visible_operation_progress_feedback(
        &todo_result.output_text,
        &todo_metadata,
    );

    read_metadata
        .get("operation_intent")
        .and_then(Value::as_str)
        == Some("content_changing_authoring_required")
        && read_metadata
            .get("operation_progress_class")
            .and_then(Value::as_str)
            == Some("supporting_context")
        && read_metadata.get("progress_effect").and_then(Value::as_str) == Some("no_progress")
        && todo_metadata
            .get("operation_progress_class")
            .and_then(Value::as_str)
            == Some("progress_projection")
        && todo_metadata.get("progress_effect").and_then(Value::as_str) == Some("no_progress")
        && read_output.contains("[tool feedback]")
        && read_output.contains("supporting_context")
        && read_output.contains("active_targets: test_source.py")
        && read_output.contains("file-changing tool output")
        && todo_output.contains("[tool feedback]")
        && todo_output.contains("progress_projection")
        && todo_output.contains("active_targets: test_source.py")
        && todo_output.contains("file-changing tool output")
}

pub(crate) fn executed_tool_failure_metadata_fixture_passes() -> bool {
    let allowed = BTreeSet::from(["read".to_string()]);
    let route = ToolOrchestrator::route(ToolRouteRequest {
        requested_tool: "read".to_string(),
        effective_tool: "read".to_string(),
        record_tool: "read".to_string(),
        original_arguments_json: r#"{"path":"missing.py"}"#.to_string(),
        effective_arguments_json: r#"{"path":"missing.py"}"#.to_string(),
        allowed_tool_names: &allowed,
        tool_exists: true,
        tool_allowed: true,
        redirected_from_arguments_json: None,
        redirect_reason: None,
        tool_choice: Some("required"),
        control_projection: None,
    });
    let metadata = route.completion_metadata(tool_failure_metadata(
        "指定されたパスが見つかりません。 (os error 3)",
        &route,
    ));
    metadata.get("success").and_then(Value::as_bool) == Some(false)
        && metadata
            .get("tool_feedback_envelope")
            .and_then(|value| value.get("result_hash"))
            .and_then(Value::as_str)
            .is_some()
        && metadata
            .get("tool_feedback_envelope")
            .and_then(|value| value.get("required_next_action"))
            .is_none()
        && metadata
            .get("tool_feedback_envelope")
            .and_then(|value| value.get("error_class"))
            .and_then(Value::as_str)
            == Some("io_not_found")
}

struct LifecycleConfirmationPrompt<'a> {
    inner: &'a mut dyn ConfirmationPrompt,
    tool_call_id: ToolCallId,
    sink: &'a mut dyn RunEventSink,
}

impl ConfirmationPrompt for LifecycleConfirmationPrompt<'_> {
    fn confirm(
        &mut self,
        request: &crate::tool::PermissionRequest,
    ) -> Result<bool, CliPromptError> {
        self.sink
            .emit(crate::session::RunEvent::PermissionRequested {
                tool_call_id: self.tool_call_id,
                summary: request.summary.clone(),
            })
            .map_err(|error| CliPromptError::Message(error.to_string()))?;
        let approved = self.inner.confirm(request)?;
        self.sink
            .emit(crate::session::RunEvent::PermissionResolved {
                tool_call_id: self.tool_call_id,
                approved,
            })
            .map_err(|error| CliPromptError::Message(error.to_string()))?;
        Ok(approved)
    }
}

impl ToolRouteDecision {
    pub(crate) fn pending_metadata(&self) -> Value {
        self.metadata.clone()
    }

    pub(crate) fn completion_metadata(&self, result_metadata: Value) -> Value {
        merge_tool_lifecycle_metadata(self.metadata.clone(), result_metadata)
    }
}

pub(crate) fn merge_tool_lifecycle_metadata(
    route_metadata: Value,
    result_metadata: Value,
) -> Value {
    let route_snapshot = route_metadata
        .get("tool_route")
        .cloned()
        .unwrap_or_else(|| route_metadata.clone());
    let mut merged = match route_metadata {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            if !other.is_null() {
                map.insert("tool_route".to_string(), other);
            }
            map
        }
    };

    match result_metadata.clone() {
        Value::Object(result_map) => {
            for (key, value) in result_map {
                merged.insert(key, value);
            }
        }
        other if !other.is_null() => {
            merged.insert("tool_result_metadata".to_string(), other);
        }
        _ => {}
    }

    merged.insert("tool_route".to_string(), route_snapshot);
    if !result_metadata.is_null() {
        merged.insert("tool_result_metadata".to_string(), result_metadata);
    }

    Value::Object(merged)
}

fn arguments_value(arguments_json: &str) -> Value {
    serde_json::from_str(arguments_json)
        .unwrap_or_else(|_| Value::String(arguments_json.to_string()))
}

fn with_verification_run_result(
    tool_name: ToolName,
    summary: &str,
    mut metadata: Value,
    truncated_output_path: Option<&Utf8Path>,
) -> Value {
    if tool_name != ToolName::Shell || metadata.get("verification_run_result").is_some() {
        return metadata;
    }
    let Some(command) = shell_command_from_metadata(&metadata) else {
        return metadata;
    };
    if !looks_like_verification_command(&command) {
        return metadata;
    }
    if !has_executed_shell_result_metadata(&metadata) {
        return metadata;
    }
    let exit_code = metadata
        .get("exit_code")
        .and_then(Value::as_i64)
        .or_else(|| {
            metadata
                .get("tool_result_metadata")
                .and_then(|value| value.get("exit_code"))
                .and_then(Value::as_i64)
        });
    let timed_out = metadata
        .get("timeout")
        .and_then(Value::as_bool)
        .or_else(|| {
            metadata
                .get("tool_result_metadata")
                .and_then(|value| value.get("timeout"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false);
    let status = if timed_out {
        VerificationRunStatus::TimedOut
    } else if exit_code == Some(0) {
        VerificationRunStatus::Passed
    } else {
        VerificationRunStatus::Failed
    };
    let failure_cluster = matches!(
        status,
        VerificationRunStatus::Failed | VerificationRunStatus::TimedOut
    )
    .then(|| verification_cluster_from_output(&command, summary));
    let result = VerificationRunResult {
        command,
        status,
        exit_code,
        timed_out,
        output_summary: summary.to_string(),
        failure_cluster,
        artifact_refs: verification_artifact_refs(&metadata, truncated_output_path),
        requirement_refs: requirement_refs_from_output(summary),
    };
    if let Value::Object(map) = &mut metadata
        && let Ok(value) = serde_json::to_value(result)
    {
        map.insert("verification_run_result".to_string(), value);
    }
    metadata
}

fn has_executed_shell_result_metadata(metadata: &Value) -> bool {
    metadata.get("exit_code").is_some()
        || metadata.get("timeout").is_some()
        || metadata
            .get("tool_result_metadata")
            .is_some_and(|value| value.get("exit_code").is_some() || value.get("timeout").is_some())
}

pub(crate) fn synthetic_corrective_shell_feedback_is_not_verification_run_fixture_passes() -> bool {
    let synthetic = with_verification_run_result(
        ToolName::Shell,
        "The requested shell command is not the current executable action. Preserve the existing verification failure and follow the typed required next action.",
        serde_json::json!({
            "progress_effect": "no_progress",
            "tool_route": {
                "effective_arguments": {
                    "command": "python -m unittest"
                }
            }
        }),
        None,
    );
    let executed = with_verification_run_result(
        ToolName::Shell,
        "FAILED (errors=1)",
        serde_json::json!({
            "exit_code": 1,
            "timeout": false,
            "tool_route": {
                "effective_arguments": {
                    "command": "python -m unittest"
                }
            }
        }),
        None,
    );
    synthetic.get("verification_run_result").is_none()
        && executed
            .get("verification_run_result")
            .and_then(|value| value.get("status"))
            .and_then(Value::as_str)
            == Some("failed")
}

pub(crate) fn no_content_write_metadata_projects_no_progress_fixture_passes() -> bool {
    let metadata = serde_json::json!({
        "no_content_change": true,
        "success": false,
        "progress_effect": "no_progress",
        "tool_feedback_envelope": {
            "success": false,
            "progress_effect": "no_progress",
            "tool": "write",
            "target": "calculator.py"
        }
    });

    tool_success_from_metadata(&metadata) == Some(false)
        && matches!(
            tool_progress_effect_from_metadata(&metadata),
            ToolProgressEffect::NoProgress
        )
}

fn verification_artifact_refs(
    metadata: &Value,
    truncated_output_path: Option<&Utf8Path>,
) -> Vec<String> {
    let mut refs = metadata
        .get("artifact_refs")
        .or_else(|| {
            metadata
                .get("tool_result_metadata")
                .and_then(|value| value.get("artifact_refs"))
        })
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    if let Some(path) = truncated_output_path {
        refs.push(path.to_string());
    }
    refs.sort();
    refs.dedup();
    refs
}

fn shell_command_from_metadata(metadata: &Value) -> Option<String> {
    metadata
        .get("tool_route")
        .and_then(|route| route.get("effective_arguments"))
        .and_then(|args| args.get("command"))
        .and_then(Value::as_str)
        .or_else(|| {
            metadata
                .get("tool_route")
                .and_then(|route| route.get("original_arguments"))
                .and_then(|args| args.get("command"))
                .and_then(Value::as_str)
        })
        .map(str::to_string)
}

fn looks_like_verification_command(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    [
        "cargo test",
        "pytest",
        "unittest",
        "py_compile",
        "npm test",
        "pnpm test",
        "yarn test",
        "go test",
        "mvn test",
        "gradle test",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn verification_cluster_from_output(command: &str, summary: &str) -> VerificationFailureCluster {
    let failing_labels = summary
        .lines()
        .filter_map(failing_label_from_output_line)
        .take(12)
        .collect::<Vec<_>>();
    let evidence = crate::agent::repair_lane::verification_failure_evidence_from_summary(
        FailureKind::VerificationFailed,
        summary,
    );
    let mut sibling_obligations = evidence
        .iter()
        .flat_map(|evidence| evidence.sibling_obligations.iter().cloned())
        .collect::<Vec<_>>();
    sibling_obligations.sort();
    sibling_obligations.dedup();
    let mut source_refs = evidence
        .iter()
        .flat_map(|evidence| evidence.source_refs.iter().cloned())
        .collect::<Vec<_>>();
    source_refs.sort();
    source_refs.dedup();
    let mut test_refs = evidence
        .iter()
        .flat_map(|evidence| evidence.test_refs.iter().cloned())
        .collect::<Vec<_>>();
    test_refs.sort();
    test_refs.dedup();
    VerificationFailureCluster {
        cluster_id: crate::harness::artifact::hash_bytes(
            format!("verification:{command}:{summary}").as_bytes(),
        ),
        failing_labels,
        primary_failure: summary
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(|line| {
                let trimmed = line.trim();
                trimmed.chars().take(240).collect::<String>()
            }),
        evidence,
        sibling_obligations,
        source_refs,
        test_refs,
    }
}

fn failing_label_from_output_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("FAIL: ") {
        return Some(rest.split_whitespace().next().unwrap_or(rest).to_string());
    }
    if let Some(rest) = trimmed.strip_prefix("ERROR: ") {
        return Some(rest.split_whitespace().next().unwrap_or(rest).to_string());
    }
    if trimmed.starts_with("test_")
        && (trimmed.contains(" ... FAIL") || trimmed.contains(" ... ERROR"))
    {
        return trimmed.split_whitespace().next().map(str::to_string);
    }
    None
}

fn requirement_refs_from_output(summary: &str) -> Vec<String> {
    summary
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-'))
        .filter(|token| {
            let upper = token.to_ascii_uppercase();
            matches!(
                upper.split_once('-'),
                Some(("BEH" | "API" | "STATE" | "UI" | "REQ", suffix))
                    if suffix.chars().all(|ch| ch.is_ascii_digit()) && !suffix.is_empty()
            )
        })
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn rejected_tool_proposal_from_metadata(metadata: &Value) -> Option<RejectedToolProposal> {
    metadata
        .get("rejected_tool_proposal")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn candidate_repair_edit_from_metadata(metadata: &Value) -> Option<CandidateRepairEdit> {
    metadata
        .get("candidate_repair_edit")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

async fn append_tool_result_part(
    session_repo: &SqliteSessionRepository,
    assistant_message_id: MessageId,
    tool_call_id: ToolCallId,
    title: &str,
    summary: &str,
    metadata: &Value,
) -> Result<(), AgentError> {
    session_repo
        .append_part(
            assistant_message_id,
            NewPart {
                kind: PartKind::ToolResult,
                payload: MessagePart::ToolResult(ToolResultPart {
                    tool_call_id,
                    status: ToolCallStatus::Completed,
                    title: title.to_string(),
                    summary: summary.to_string(),
                    success: tool_success_from_metadata(metadata),
                    progress_effect: tool_progress_effect_from_metadata(metadata),
                    blocked_action: None,
                    required_next_action: None,
                    result_hash: metadata_string(
                        metadata,
                        &["tool_feedback_envelope", "result_hash"],
                    )
                    .or_else(|| metadata_string(metadata, &["result_hash"])),
                }),
            },
        )
        .await?;
    Ok(())
}

fn tool_success_from_metadata(metadata: &Value) -> Option<bool> {
    if let Some(success) = metadata
        .get("success")
        .or_else(|| {
            metadata
                .get("tool_feedback_envelope")
                .and_then(|feedback| feedback.get("success"))
        })
        .and_then(Value::as_bool)
    {
        return Some(success);
    }
    if let Some(run) = metadata
        .get("verification_run_result")
        .and_then(|value| serde_json::from_value::<VerificationRunResult>(value.clone()).ok())
    {
        return Some(matches!(run.status, VerificationRunStatus::Passed));
    }
    Some(!matches!(
        tool_progress_effect_from_metadata(metadata),
        ToolProgressEffect::NoProgress
            | ToolProgressEffect::Blocked
            | ToolProgressEffect::VerificationFailed
    ))
}

fn tool_progress_effect_from_metadata(metadata: &Value) -> ToolProgressEffect {
    if let Some(run) = metadata
        .get("verification_run_result")
        .and_then(|value| serde_json::from_value::<VerificationRunResult>(value.clone()).ok())
    {
        return match run.status {
            VerificationRunStatus::Passed => ToolProgressEffect::VerificationPassed,
            VerificationRunStatus::Failed | VerificationRunStatus::TimedOut => {
                ToolProgressEffect::VerificationFailed
            }
            VerificationRunStatus::NotVerification => ToolProgressEffect::Unknown,
        };
    }
    metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("progress_effect"))
        .or_else(|| metadata.get("progress_effect"))
        .and_then(Value::as_str)
        .map(|value| match value {
            "made_progress" | "progress" => ToolProgressEffect::MadeProgress,
            "no_progress" => ToolProgressEffect::NoProgress,
            "blocked" => ToolProgressEffect::Blocked,
            "verification_passed" => ToolProgressEffect::VerificationPassed,
            "verification_failed" => ToolProgressEffect::VerificationFailed,
            _ => ToolProgressEffect::Unknown,
        })
        .unwrap_or(ToolProgressEffect::Unknown)
}

fn metadata_string(metadata: &Value, path: &[&str]) -> Option<String> {
    let mut value = metadata;
    for key in path {
        value = value.get(*key)?;
    }
    value.as_str().map(str::to_string)
}
