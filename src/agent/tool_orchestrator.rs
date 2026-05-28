use std::collections::{BTreeMap, BTreeSet};

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Map, Value, json};

use crate::agent::state::ActiveWorkContract;
use crate::agent::verification::{
    canonical_verification_command_identity_key, verification_command_identity_key,
    verification_command_satisfaction_keys,
};
use crate::cli::ConfirmationPrompt;
use crate::config::ResolvedConfig;
use crate::error::{AgentError, CliPromptError, ToolError};
use crate::protocol::{
    CandidateRepairEdit, HistoryItem, OperationIntent, RejectedToolProposal, RequiredAction,
    ToolChoice, ToolProgressEffect, VerificationRunResult, VerificationRunStatus,
};
use crate::runtime::RunEventSink;
use crate::session::{
    DiffSummaryPart, FailureKind, MessageId, MessagePart, NewPart, PartKind, SessionContext,
    SessionId, SessionStateSnapshot, TaskRoute, ToolCallId, ToolCallRecord,
    VerificationFailureCluster,
};
use crate::storage::SqliteSessionRepository;
use crate::tool::context::{ToolContext, ToolServices};
use crate::tool::registry::ToolRegistry;
use crate::tool::{ToolName, ToolResult};
use crate::workspace::Workspace;
use tokio_util::sync::CancellationToken;

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
    pub sandbox_decision: Value,
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
    pub tool_name: ToolName,
    pub cancel: CancellationToken,
    pub prompt: &'a mut dyn ConfirmationPrompt,
    pub services: &'a ToolServices,
}

pub(crate) struct PreExecutionCorrectiveInput<'a> {
    pub effective_tool_name: &'a str,
    pub parsed_arguments: &'a Value,
    pub active_work: Option<&'a ActiveWorkContract>,
    pub state: &'a SessionStateSnapshot,
    pub workspace_root: &'a Utf8Path,
    pub workspace_cwd: Option<&'a Utf8Path>,
    pub allowed_tools: &'a BTreeSet<String>,
    pub history_items: &'a [HistoryItem],
    pub shell_family: crate::config::ShellFamily,
}

pub(crate) struct PreExecutionCorrectiveDecision {
    pub kind: PreExecutionCorrectiveKind,
    pub result: ToolResult,
}

pub(crate) struct PreExecutionCorrectiveNoProgressInput<'a> {
    pub kind: PreExecutionCorrectiveKind,
    pub result: &'a ToolResult,
    pub effective_tool_name: &'a str,
    pub parsed_arguments: &'a Value,
    pub active_work: Option<&'a ActiveWorkContract>,
    pub state: &'a SessionStateSnapshot,
    pub workspace_root: &'a Utf8Path,
    pub allowed_tools: &'a BTreeSet<String>,
    pub tool_choice: &'a ToolChoice,
    pub open_executable_work: bool,
    pub operation_non_content_no_progress_counts: &'a mut BTreeMap<String, usize>,
    pub repair_target_authority_violation_counts: &'a mut BTreeMap<String, usize>,
    pub wrong_authoring_target_counts: &'a mut BTreeMap<String, usize>,
    pub docs_spec_semantic_reconciliation_counts: &'a mut BTreeMap<String, usize>,
    pub public_command_contract_counts: &'a mut BTreeMap<String, usize>,
    pub wrong_verification_command_counts: &'a mut BTreeMap<String, usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreExecutionCorrectiveKind {
    ArtifactContentShapeViolation,
    RepairTargetAuthorityViolation,
    RepairActiveShellProbeTarget,
    WrongAuthoringTarget,
    DocsSpecSemanticReconciliation,
    PublicCommandContract,
    WrongVerificationShellCommand,
}

pub(crate) struct ToolLifecycleRuntime;

impl ToolLifecycleRuntime {
    pub(crate) fn route_adjudicated_call(request: ToolRouteRequest<'_>) -> ToolRouteDecision {
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
        let permission_decision = if request.tool_exists && request.tool_allowed {
            "pending"
        } else {
            "not_required"
        };
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
            "permission_decision": permission_decision,
            "sandbox_decision": request.sandbox_decision.clone(),
            "retry_policy": {
                "owner": "tool_lifecycle_runtime",
                "decision": "not_scheduled"
            },
            "terminal_guard_policy": {
                "owner": "tool_lifecycle_runtime",
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
            "permission_decision": permission_decision,
            "sandbox_decision": request.sandbox_decision,
            "retry_policy": {
                "owner": "tool_lifecycle_runtime",
                "decision": "not_scheduled"
            },
            "terminal_guard_policy": {
                "owner": "tool_lifecycle_runtime",
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
        protocol_turn_id: crate::protocol::TurnId,
        route: &ToolRouteDecision,
        sink: &mut dyn RunEventSink,
    ) -> Result<ToolCallRecord, AgentError> {
        let (record, event) = session_repo
            .record_pending_tool_call_with_protocol_bundle(
                session_id,
                assistant_message_id,
                &route.record_tool,
                &route.effective_arguments_json,
                Some(&route.requested_tool),
                route.pending_metadata(),
                protocol_turn_id,
                sink.reserve_protocol_sequence_no(),
            )
            .await?;
        sink.emit_pre_recorded(event)?;
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
            tool_name: request.tool_name,
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
                    cancel: request.cancel,
                    prompt: &mut prompt,
                    services: request.services,
                },
            )
            .await
    }

    pub(crate) async fn complete_corrective_call(
        session_repo: &SqliteSessionRepository,
        assistant_message_id: MessageId,
        session_id: SessionId,
        protocol_turn_id: crate::protocol::TurnId,
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        result: &ToolResult,
        route: &ToolRouteDecision,
        sink: &mut dyn RunEventSink,
    ) -> Result<(), AgentError> {
        Self::complete_text_call(
            session_repo,
            assistant_message_id,
            session_id,
            protocol_turn_id,
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
        session_id: SessionId,
        protocol_turn_id: crate::protocol::TurnId,
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
        let event = session_repo
            .complete_tool_call_with_protocol_bundle(
                session_id,
                assistant_message_id,
                tool_call_id,
                tool_name,
                title,
                metadata.clone(),
                summary,
                truncated_output_path,
                protocol_turn_id,
                sink.reserve_protocol_sequence_no(),
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
        sink.emit_pre_recorded(event)?;
        Ok(metadata)
    }

    pub(crate) async fn complete_executed_call(
        session_repo: &SqliteSessionRepository,
        assistant_message_id: MessageId,
        session_id: SessionId,
        protocol_turn_id: crate::protocol::TurnId,
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
        let event = session_repo
            .complete_tool_call_with_protocol_bundle(
                session_id,
                assistant_message_id,
                tool_call_id,
                tool_name,
                &result.title,
                metadata.clone(),
                &provider_output_text,
                result.truncated_output_path.as_deref(),
                protocol_turn_id,
                sink.reserve_protocol_sequence_no(),
            )
            .await?;
        sink.emit_pre_recorded(event)?;
        if !result.recorded_changes.is_empty() {
            let content_satisfying_changes =
                content_satisfying_change_summaries_for_protocol(result, &metadata);
            if content_satisfying_changes.is_empty() {
                return Ok(metadata);
            }
            let summary = content_satisfying_changes
                .iter()
                .map(|change| change.summary_line(Some(workspace_root)))
                .collect::<Vec<_>>()
                .join("\n");
            let content_satisfying_change_ids = content_satisfying_changes
                .iter()
                .map(|change| change.change_id)
                .collect::<Vec<_>>();
            let event = crate::session::RunEvent::FileChangesRecorded {
                tool_call_id,
                changes: content_satisfying_changes.clone(),
            };
            session_repo
                .append_part_with_protocol_bundle(
                    session_id,
                    assistant_message_id,
                    NewPart {
                        kind: PartKind::DiffSummary,
                        payload: MessagePart::DiffSummary(DiffSummaryPart {
                            change_ids: content_satisfying_change_ids,
                            changes: content_satisfying_changes
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
                    &event,
                    protocol_turn_id,
                    sink.reserve_protocol_sequence_no(),
                )
                .await?;
            sink.emit_pre_recorded(event)?;
        }
        Ok(metadata)
    }

    pub(crate) async fn fail_executed_call(
        session_repo: &SqliteSessionRepository,
        assistant_message_id: MessageId,
        session_id: SessionId,
        protocol_turn_id: crate::protocol::TurnId,
        tool_call_id: ToolCallId,
        tool_name: ToolName,
        error_text: &str,
        route: &ToolRouteDecision,
        sink: &mut dyn RunEventSink,
    ) -> Result<(), AgentError> {
        let metadata = route.completion_metadata(tool_failure_metadata(error_text, route));
        let event = session_repo
            .fail_tool_call_with_protocol_bundle(
                session_id,
                assistant_message_id,
                tool_call_id,
                tool_name,
                error_text,
                metadata,
                protocol_turn_id,
                sink.reserve_protocol_sequence_no(),
            )
            .await?;
        sink.emit_pre_recorded(event)?;
        Ok(())
    }

    pub(crate) fn record_rejected_tool_no_progress(
        counts: &mut BTreeMap<String, usize>,
        request: RejectedToolNoProgressGuardRequest<'_>,
    ) -> ToolTerminalGuardDecision {
        let key = Self::rejected_tool_no_progress_guard_key(&request);
        let count = counts
            .entry(key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let terminal = if request.provider_noncompliance {
            *count >= PROVIDER_NONCOMPLIANCE_TERMINAL_THRESHOLD
        } else {
            should_terminalize_rejected_tool_no_progress(*count)
        };
        let terminal_message = terminal.then(|| {
            if request.provider_noncompliance {
                format!(
                    "Provider repeated a rejected model action with no progress {count} time(s). Runtime stopped on the lifecycle adjudication cluster `{}` before applying side effects outside the compiled TurnControlEnvelope lifecycle.",
                    request.semantic_class,
                    count = *count,
                )
            } else {
                rejected_tool_no_progress_terminal_message(
                    request.effective_tool_name,
                    *count,
                    request.allowed_tools,
                    request.required_action,
                )
            }
        });
        ToolTerminalGuardDecision {
            count: *count,
            terminal_message,
        }
    }

    pub(crate) fn classify_pre_execution_corrective_result(
        input: PreExecutionCorrectiveInput<'_>,
    ) -> Option<PreExecutionCorrectiveDecision> {
        if let Some(result) = Self::artifact_content_shape_violation_result(
            input.effective_tool_name,
            input.parsed_arguments,
            Some(input.workspace_root),
        ) {
            return Some(PreExecutionCorrectiveDecision {
                kind: PreExecutionCorrectiveKind::ArtifactContentShapeViolation,
                result,
            });
        }
        if let Some(result) = Self::repair_target_authority_violation_result(
            input.effective_tool_name,
            input.parsed_arguments,
            input.active_work,
            input.state,
            input.workspace_root,
            input.allowed_tools,
        ) {
            return Some(PreExecutionCorrectiveDecision {
                kind: PreExecutionCorrectiveKind::RepairTargetAuthorityViolation,
                result,
            });
        }
        if let Some(result) = Self::repair_active_shell_probe_target_result(
            input.effective_tool_name,
            input.parsed_arguments,
            input.active_work,
            input.state,
            input.workspace_root,
            input.allowed_tools,
        ) {
            return Some(PreExecutionCorrectiveDecision {
                kind: PreExecutionCorrectiveKind::RepairActiveShellProbeTarget,
                result,
            });
        }
        if let Some(result) = Self::wrong_authoring_target_result(
            input.effective_tool_name,
            input.parsed_arguments,
            input.active_work,
            input.workspace_root,
        ) {
            return Some(PreExecutionCorrectiveDecision {
                kind: PreExecutionCorrectiveKind::WrongAuthoringTarget,
                result,
            });
        }
        if let Some(result) = Self::docs_spec_semantic_reconciliation_result(
            input.effective_tool_name,
            input.parsed_arguments,
            input.state,
            input.active_work,
            input.workspace_root,
            input.history_items,
        ) {
            return Some(PreExecutionCorrectiveDecision {
                kind: PreExecutionCorrectiveKind::DocsSpecSemanticReconciliation,
                result,
            });
        }
        if let Some(result) = Self::public_command_contract_result(
            input.effective_tool_name,
            input.parsed_arguments,
            input.history_items,
            input.workspace_cwd,
        ) {
            return Some(PreExecutionCorrectiveDecision {
                kind: PreExecutionCorrectiveKind::PublicCommandContract,
                result,
            });
        }
        if !Self::repair_active_shell_probe_matches_exact_target(
            input.effective_tool_name,
            input.parsed_arguments,
            input.active_work,
            input.state,
            input.workspace_root,
            input.allowed_tools,
        ) && let Some(result) = Self::wrong_verification_shell_command_result(
            input.effective_tool_name,
            input.parsed_arguments,
            input.active_work,
            input.shell_family,
        ) {
            return Some(PreExecutionCorrectiveDecision {
                kind: PreExecutionCorrectiveKind::WrongVerificationShellCommand,
                result,
            });
        }
        None
    }

    pub(crate) fn record_pre_execution_corrective_no_progress(
        input: PreExecutionCorrectiveNoProgressInput<'_>,
    ) -> ToolTerminalGuardDecision {
        match input.kind {
            PreExecutionCorrectiveKind::ArtifactContentShapeViolation => {
                Self::record_corrective_content_shape_no_progress(
                    input.operation_non_content_no_progress_counts,
                    input.effective_tool_name,
                    &input.result.metadata,
                    input.state,
                    input.allowed_tools,
                    input.tool_choice,
                    input.open_executable_work,
                )
                .unwrap_or(ToolTerminalGuardDecision {
                    count: 0,
                    terminal_message: None,
                })
            }
            PreExecutionCorrectiveKind::RepairTargetAuthorityViolation
            | PreExecutionCorrectiveKind::RepairActiveShellProbeTarget => {
                Self::record_repair_target_authority_violation_no_progress(
                    input.repair_target_authority_violation_counts,
                    input.allowed_tools,
                    input.tool_choice,
                    input.result,
                )
            }
            PreExecutionCorrectiveKind::WrongAuthoringTarget => {
                Self::record_wrong_authoring_target_no_progress(
                    input.wrong_authoring_target_counts,
                    input.effective_tool_name,
                    input.parsed_arguments,
                    input.active_work,
                    input.workspace_root,
                    input.allowed_tools,
                    input.tool_choice,
                    input.result,
                )
            }
            PreExecutionCorrectiveKind::DocsSpecSemanticReconciliation => {
                Self::record_docs_spec_semantic_reconciliation_no_progress(
                    input.docs_spec_semantic_reconciliation_counts,
                    input.result,
                )
            }
            PreExecutionCorrectiveKind::PublicCommandContract => {
                Self::record_public_command_contract_no_progress(
                    input.public_command_contract_counts,
                    input.result,
                )
            }
            PreExecutionCorrectiveKind::WrongVerificationShellCommand => {
                Self::record_wrong_verification_command_no_progress(
                    input.wrong_verification_command_counts,
                    input.parsed_arguments,
                    input.active_work,
                    input.allowed_tools,
                    input.tool_choice,
                    input.result,
                )
            }
        }
    }

    pub(crate) fn rejected_tool_no_progress_guard_key(
        request: &RejectedToolNoProgressGuardRequest<'_>,
    ) -> String {
        if request.provider_noncompliance
            && let Some(recovery_key) = request.recovery_no_progress_key
        {
            return format!("model_action_rejection_recovery|{recovery_key}");
        }
        if request.provider_noncompliance || request.result_hash.is_some() {
            let required_action_projection = request
                .required_action
                .map(RequiredAction::projection_label)
                .unwrap_or("none");
            return format!(
                "model_action_rejection|semantic={}|tool={}|allowed={}|choice={}|required_action={required_action_projection}",
                request.semantic_class,
                request.effective_tool_name,
                request
                    .allowed_tools
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(","),
                tool_choice_label(request.tool_choice),
            );
        }
        rejected_tool_no_progress_key(
            request.effective_tool_name,
            request.effective_arguments_json,
            request.allowed_tools,
            request.tool_choice,
            request.required_action,
        )
    }

    pub(crate) fn record_executed_tool_failure_no_progress(
        counts: &mut BTreeMap<String, usize>,
        effective_tool_name: &str,
        effective_arguments_json: &str,
        allowed_tools: &BTreeSet<String>,
        error_text: &str,
    ) -> ToolTerminalGuardDecision {
        let key = executed_tool_failure_no_progress_key(
            effective_tool_name,
            effective_arguments_json,
            allowed_tools,
            error_text,
        );
        let count = counts
            .entry(key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let terminal_message = (*count >= EXECUTED_TOOL_FAILURE_TERMINAL_THRESHOLD).then(|| {
            executed_tool_failure_terminal_message(effective_tool_name, *count, error_text)
        });
        ToolTerminalGuardDecision {
            count: *count,
            terminal_message,
        }
    }

    pub(crate) fn wrong_verification_shell_command_result(
        effective_tool_name: &str,
        parsed_arguments: &Value,
        active_work: Option<&ActiveWorkContract>,
        shell_family: crate::config::ShellFamily,
    ) -> Option<ToolResult> {
        if effective_tool_name != "shell" {
            return None;
        }
        let required_commands = verification_commands_for_active_work(active_work)?;
        let required_commands = canonical_required_verification_commands(required_commands);
        let submitted = parsed_arguments.get("command")?.as_str()?.trim();
        let submitted_keys = canonical_shell_command_keys(submitted);
        let required_keys = required_commands
            .iter()
            .filter_map(|required| canonical_verification_command_identity_key(required))
            .collect::<BTreeSet<_>>();
        if verification_command_key_family_matches(&submitted_keys, &required_keys) {
            return None;
        }
        let executable_commands =
            executable_verification_command_forms(&required_commands, shell_family);
        let executable_guidance = if executable_commands.is_empty() {
            String::new()
        } else {
            format!(
                " Acceptable executable form(s) for this shell/encoding contract: {}.",
                executable_commands.join(", ")
            )
        };
        Some(ToolResult {
            title: "Run required verification command".to_string(),
            output_text: format!(
                "Verification is still pending. The submitted shell command `{submitted}` is a public/diagnostic probe and does not match any remaining required verification command identity. Run one of these required command identity/identities now: {}.{} Do not run public command probes until this exact verification rerun has passed after the latest content-changing file update.",
                required_commands.join(", "),
                executable_guidance
            ),
            metadata: json!({
                "corrective_result": true,
                "operation_progress_class": "wrong_verification_command",
                "progress_effect": "no_progress",
                "submitted_command": submitted,
                "required_verification_commands": required_commands,
                "executable_verification_commands": executable_commands,
                "terminal_guard_policy": {
                    "owner": "tool_lifecycle_runtime",
                    "no_progress_guard": true,
                    "side_effects_applied": false,
                    "terminal_after_repeated_corrections": WRONG_VERIFICATION_COMMAND_TERMINAL_THRESHOLD
                }
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }

    pub(crate) fn repair_active_shell_probe_target_result(
        effective_tool_name: &str,
        parsed_arguments: &Value,
        active_work: Option<&ActiveWorkContract>,
        state: &SessionStateSnapshot,
        workspace_root: &Utf8Path,
        allowed_tools: &BTreeSet<String>,
    ) -> Option<ToolResult> {
        if effective_tool_name != "shell" {
            return None;
        }
        if !matches!(
            active_work,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                ..
            })
        ) || state.process_phase != crate::session::ProcessPhase::Repair
        {
            return None;
        }
        let submitted = parsed_arguments.get("command")?.as_str()?.trim();
        let submitted_targets = shell_file_probe_targets(submitted);
        if submitted_targets.is_empty() {
            return None;
        }
        let repair_lane = crate::agent::repair_lane::project_repair_lane(state, allowed_tools)?;
        let template = repair_lane.operation_template.as_ref()?;
        let exact_target = template
            .exact_target
            .as_deref()
            .or(repair_lane.required_target.as_deref())?
            .trim();
        if exact_target.is_empty() {
            return None;
        }
        let exact_keys = normalized_target_keys(exact_target, workspace_root)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let submitted_keys = submitted_targets
            .iter()
            .flat_map(|target| normalized_target_keys(target, workspace_root))
            .collect::<BTreeSet<_>>();
        if !exact_keys.is_empty() && !submitted_keys.is_disjoint(&exact_keys) {
            return None;
        }

        let submitted_target_strings = submitted_targets.clone();
        let submitted_line = submitted_target_strings
            .iter()
            .map(|target| format!("`{target}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let repair_owner = repair_lane
            .repair_control_snapshot
            .as_ref()
            .map(|snapshot| snapshot.repair_owner.clone())
            .or_else(|| {
                repair_lane
                    .repair_intent
                    .as_ref()
                    .map(|intent| intent.repair_owner.clone())
            })
            .unwrap_or_else(|| "unknown".to_string());
        let source_test_ownership = template.source_test_ownership.clone();
        let operation_kind = template.operation_kind.clone();
        let operation_id = template.operation_id.clone();
        let result_hash = crate::harness::artifact::hash_bytes(
            format!(
                "repair_shell_probe_target_mismatch|submitted={}|exact={}|owner={repair_owner}|ownership={source_test_ownership}|operation_kind={operation_kind}|operation_id={operation_id}",
                submitted_keys
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(","),
                exact_keys
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(","),
            )
            .as_bytes(),
        );

        Some(ToolResult {
            title: "Repair target shell probe mismatch".to_string(),
            output_text: format!(
                "The current verification repair has exact target `{exact_target}`, but the submitted shell file-inspection command targets {submitted_line}. Runtime rejected this shell probe before execution because repair supporting context must be scoped to the exact repair target until a content-changing edit is made."
            ),
            metadata: json!({
                "corrective_result": true,
                "success": false,
                "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                "operation_progress_class": "wrong_repair_target",
                "progress_effect": "no_progress",
                "result_hash": result_hash,
                "submitted_command": submitted,
                "submitted_targets": submitted_target_strings,
                "active_repair_targets": [exact_target],
                "repair_target_authority": {
                    "kind": "repair_shell_probe_target_mismatch",
                    "exact_target": exact_target,
                    "repair_owner": repair_owner,
                    "source_test_ownership": source_test_ownership,
                    "operation_kind": operation_kind,
                    "operation_id": operation_id,
                    "required_edit_surface": template.required_edit_surface.clone(),
                },
                "tool_feedback_envelope": {
                    "kind": "repair_shell_probe_target_mismatch",
                    "success": false,
                    "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                    "operation_progress_class": "wrong_repair_target",
                    "progress_effect": "no_progress",
                    "submitted_targets": submitted_targets,
                    "active_targets": [exact_target],
                    "required_target": exact_target,
                    "side_effects_applied": false,
                    "repair_owner": repair_owner,
                    "source_test_ownership": source_test_ownership,
                    "result_hash": result_hash
                },
                "terminal_guard_policy": {
                    "owner": "tool_lifecycle_runtime",
                    "no_progress_guard": true,
                    "side_effects_applied": false,
                    "terminal_after_repeated_corrections": WRONG_REPAIR_TARGET_TERMINAL_THRESHOLD
                }
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }

    pub(crate) fn repair_active_shell_probe_matches_exact_target(
        effective_tool_name: &str,
        parsed_arguments: &Value,
        active_work: Option<&ActiveWorkContract>,
        state: &SessionStateSnapshot,
        workspace_root: &Utf8Path,
        allowed_tools: &BTreeSet<String>,
    ) -> bool {
        if effective_tool_name != "shell"
            || state.process_phase != crate::session::ProcessPhase::Repair
            || !matches!(
                active_work,
                Some(ActiveWorkContract::Verification {
                    repair_required: true,
                    ..
                })
            )
        {
            return false;
        }
        let Some(submitted) = parsed_arguments.get("command").and_then(Value::as_str) else {
            return false;
        };
        let submitted_targets = shell_file_probe_targets(submitted);
        if submitted_targets.is_empty() {
            return false;
        }
        let Some(repair_lane) =
            crate::agent::repair_lane::project_repair_lane(state, allowed_tools)
        else {
            return false;
        };
        let Some(template) = repair_lane.operation_template.as_ref() else {
            return false;
        };
        let Some(exact_target) = template
            .exact_target
            .as_deref()
            .or(repair_lane.required_target.as_deref())
            .map(str::trim)
            .filter(|target| !target.is_empty())
        else {
            return false;
        };
        let exact_keys = normalized_target_keys(exact_target, workspace_root)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let submitted_keys = submitted_targets
            .iter()
            .flat_map(|target| normalized_target_keys(target, workspace_root))
            .collect::<BTreeSet<_>>();
        !exact_keys.is_empty() && !submitted_keys.is_disjoint(&exact_keys)
    }

    pub(crate) fn record_wrong_verification_command_no_progress(
        counts: &mut BTreeMap<String, usize>,
        parsed_arguments: &Value,
        active_work: Option<&ActiveWorkContract>,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
        result: &ToolResult,
    ) -> ToolTerminalGuardDecision {
        let key = wrong_verification_command_key(
            parsed_arguments,
            active_work,
            allowed_tools,
            tool_choice,
        );
        let count = counts
            .entry(key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let terminal_message = (*count >= WRONG_VERIFICATION_COMMAND_TERMINAL_THRESHOLD)
            .then(|| wrong_verification_command_terminal_message(result, *count));
        ToolTerminalGuardDecision {
            count: *count,
            terminal_message,
        }
    }

    pub(crate) fn wrong_authoring_target_result(
        effective_tool_name: &str,
        parsed_arguments: &Value,
        active_work: Option<&ActiveWorkContract>,
        workspace_root: &Utf8Path,
    ) -> Option<ToolResult> {
        if !operation_content_changing_tool_name(effective_tool_name) {
            return None;
        }
        let active_targets = active_requested_work_targets(active_work)?;
        let submitted_targets = submitted_authoring_targets(effective_tool_name, parsed_arguments);
        if submitted_targets.is_empty() {
            return None;
        }
        let active_keys = active_targets
            .iter()
            .flat_map(|target| normalized_target_keys(target.as_str(), workspace_root))
            .collect::<BTreeSet<_>>();
        let submitted_keys = submitted_targets
            .iter()
            .flat_map(|target| normalized_target_keys(target, workspace_root))
            .collect::<BTreeSet<_>>();
        if !active_keys.is_empty() && !submitted_keys.is_disjoint(&active_keys) {
            return None;
        }

        let active_target_strings = active_targets
            .iter()
            .map(|target| target.as_str().to_string())
            .collect::<Vec<_>>();
        let target_line = active_target_strings
            .iter()
            .map(|target| format!("`{target}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let submitted_line = submitted_targets
            .iter()
            .map(|target| format!("`{target}`"))
            .collect::<Vec<_>>()
            .join(", ");
        Some(ToolResult {
            title: "Wrong authoring target".to_string(),
            output_text: format!(
                "The submitted content-changing `{effective_tool_name}` call targets {submitted_line}, but the current active requested deliverables are {target_line}. Runtime rejected this call before applying filesystem side effects because it would not satisfy the open requested-work authoring lifecycle."
            ),
            metadata: json!({
                "corrective_result": true,
                "success": false,
                "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                "operation_progress_class": "wrong_authoring_target",
                "progress_effect": "no_progress",
                "submitted_targets": submitted_targets,
                "active_authoring_targets": active_target_strings,
                "tool_feedback_envelope": {
                    "kind": "wrong_authoring_target",
                    "success": false,
                    "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                    "operation_progress_class": "wrong_authoring_target",
                    "progress_effect": "no_progress",
                    "submitted_targets": submitted_targets,
                    "active_targets": active_target_strings,
                    "side_effects_applied": false
                },
                "terminal_guard_policy": {
                    "owner": "tool_lifecycle_runtime",
                    "no_progress_guard": true,
                    "side_effects_applied": false,
                    "terminal_after_repeated_corrections": WRONG_AUTHORING_TARGET_TERMINAL_THRESHOLD
                }
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }

    pub(crate) fn repair_target_authority_violation_result(
        effective_tool_name: &str,
        parsed_arguments: &Value,
        active_work: Option<&ActiveWorkContract>,
        state: &SessionStateSnapshot,
        workspace_root: &Utf8Path,
        allowed_tools: &BTreeSet<String>,
    ) -> Option<ToolResult> {
        if !operation_content_changing_tool_name(effective_tool_name) {
            return None;
        }
        if !matches!(
            active_work,
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                ..
            })
        ) {
            return None;
        }
        if state.process_phase != crate::session::ProcessPhase::Repair
            || !state.completion.verification_pending
        {
            return None;
        }
        let submitted_targets = submitted_authoring_targets(effective_tool_name, parsed_arguments);
        if submitted_targets.is_empty() {
            return None;
        }
        let repair_lane = crate::agent::repair_lane::project_repair_lane(state, allowed_tools)?;
        let template = repair_lane.operation_template.as_ref()?;
        let exact_target = template
            .exact_target
            .as_deref()
            .or(repair_lane.required_target.as_deref())?
            .trim();
        if exact_target.is_empty() {
            return None;
        }
        let exact_keys = normalized_target_keys(exact_target, workspace_root)
            .into_iter()
            .collect::<BTreeSet<_>>();
        let submitted_keys = submitted_targets
            .iter()
            .flat_map(|target| normalized_target_keys(target, workspace_root))
            .collect::<BTreeSet<_>>();
        if !exact_keys.is_empty() && !submitted_keys.is_disjoint(&exact_keys) {
            return None;
        }

        let submitted_target_strings = submitted_targets.clone();
        let forbidden_actions = repair_lane
            .repair_control_snapshot
            .as_ref()
            .map(|snapshot| snapshot.forbidden_actions.clone())
            .unwrap_or_else(|| {
                repair_lane
                    .repair_intent
                    .as_ref()
                    .map(|intent| intent.forbidden_directions.clone())
                    .unwrap_or_default()
            });
        let hard_invariants = repair_lane
            .repair_control_snapshot
            .as_ref()
            .map(|snapshot| snapshot.hard_invariants.clone())
            .unwrap_or_default();
        let repair_owner = repair_lane
            .repair_control_snapshot
            .as_ref()
            .map(|snapshot| snapshot.repair_owner.clone())
            .or_else(|| {
                repair_lane
                    .repair_intent
                    .as_ref()
                    .map(|intent| intent.repair_owner.clone())
            })
            .unwrap_or_else(|| "unknown".to_string());
        let source_test_ownership = template.source_test_ownership.clone();
        let operation_kind = template.operation_kind.clone();
        let operation_id = template.operation_id.clone();
        let required_edit_surface = template.required_edit_surface.clone();
        let submitted_line = submitted_target_strings
            .iter()
            .map(|target| format!("`{target}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let generated_test_rewrite_blocked = repair_owner == "source"
            && submitted_target_strings
                .iter()
                .any(|target| repair_admission_target_is_test_like(target));
        let result_hash = crate::harness::artifact::hash_bytes(
            format!(
                "repair_target_authority_violation|tool={effective_tool_name}|submitted={}|exact={}|owner={repair_owner}|ownership={source_test_ownership}|operation_kind={operation_kind}|operation_id={operation_id}",
                submitted_keys
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(","),
                exact_keys
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(","),
            )
            .as_bytes(),
        );

        Some(ToolResult {
            title: "Required repair target mismatch".to_string(),
            output_text: format!(
                "The current verification repair has exact target `{exact_target}`, but the submitted content-changing `{effective_tool_name}` call targets {submitted_line}. Runtime rejected this call before applying filesystem side effects because only a content-changing edit to the exact repair target can satisfy this repair lane."
            ),
            metadata: json!({
                "corrective_result": true,
                "success": false,
                "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                "operation_progress_class": "wrong_repair_target",
                "progress_effect": "no_progress",
                "result_hash": result_hash,
                "submitted_targets": submitted_target_strings,
                "active_repair_targets": [exact_target],
                "repair_target_authority": {
                    "kind": "repair_operation_template_exact_target",
                    "exact_target": exact_target,
                    "repair_owner": repair_owner,
                    "source_test_ownership": source_test_ownership,
                    "operation_kind": operation_kind,
                    "operation_id": operation_id,
                    "required_edit_surface": required_edit_surface,
                    "forbidden_actions": forbidden_actions,
                    "hard_invariants": hard_invariants,
                    "generated_test_rewrite_blocked": generated_test_rewrite_blocked
                },
                "tool_feedback_envelope": {
                    "kind": "repair_target_authority_violation",
                    "success": false,
                    "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                    "operation_progress_class": "wrong_repair_target",
                    "progress_effect": "no_progress",
                    "submitted_targets": submitted_targets,
                    "active_targets": [exact_target],
                    "required_target": exact_target,
                    "side_effects_applied": false,
                    "repair_owner": repair_owner,
                    "source_test_ownership": source_test_ownership,
                    "result_hash": result_hash
                },
                "terminal_guard_policy": {
                    "owner": "tool_lifecycle_runtime",
                    "no_progress_guard": true,
                    "side_effects_applied": false,
                    "terminal_after_repeated_corrections": WRONG_REPAIR_TARGET_TERMINAL_THRESHOLD
                }
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        })
    }

    pub(crate) fn record_repair_target_authority_violation_no_progress(
        counts: &mut BTreeMap<String, usize>,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
        result: &ToolResult,
    ) -> ToolTerminalGuardDecision {
        let key = repair_target_authority_violation_key(result, allowed_tools, tool_choice);
        let count = counts
            .entry(key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let terminal_message = (*count >= WRONG_REPAIR_TARGET_TERMINAL_THRESHOLD)
            .then(|| repair_target_authority_violation_terminal_message(result, *count));
        ToolTerminalGuardDecision {
            count: *count,
            terminal_message,
        }
    }

    pub(crate) fn record_wrong_authoring_target_no_progress(
        counts: &mut BTreeMap<String, usize>,
        effective_tool_name: &str,
        parsed_arguments: &Value,
        active_work: Option<&ActiveWorkContract>,
        workspace_root: &Utf8Path,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
        result: &ToolResult,
    ) -> ToolTerminalGuardDecision {
        let key = wrong_authoring_target_key(
            effective_tool_name,
            parsed_arguments,
            active_work,
            workspace_root,
            allowed_tools,
            tool_choice,
        );
        let count = counts
            .entry(key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let terminal_message = (*count >= WRONG_AUTHORING_TARGET_TERMINAL_THRESHOLD)
            .then(|| wrong_authoring_target_terminal_message(result, *count));
        ToolTerminalGuardDecision {
            count: *count,
            terminal_message,
        }
    }

    pub(crate) fn docs_spec_semantic_reconciliation_result(
        effective_tool_name: &str,
        parsed_arguments: &Value,
        state: &SessionStateSnapshot,
        active_work: Option<&ActiveWorkContract>,
        workspace_root: &Utf8Path,
        history_items: &[HistoryItem],
    ) -> Option<ToolResult> {
        let latest_user_text =
            crate::agent::docs_semantic_contract::latest_user_authority_text(history_items);
        crate::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_result(
            effective_tool_name,
            parsed_arguments,
            state,
            active_work,
            workspace_root,
            latest_user_text.as_deref(),
        )
    }

    pub(crate) fn record_docs_spec_semantic_reconciliation_no_progress(
        counts: &mut BTreeMap<String, usize>,
        result: &ToolResult,
    ) -> ToolTerminalGuardDecision {
        let key =
            crate::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_key(result);
        let count = counts
            .entry(key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let terminal_message =
            (*count
                >= crate::agent::docs_semantic_contract::DOCS_SPEC_SEMANTIC_RECONCILIATION_TERMINAL_THRESHOLD)
                .then(|| {
                crate::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_terminal_message(
                    result,
                    *count,
                )
            });
        ToolTerminalGuardDecision {
            count: *count,
            terminal_message,
        }
    }

    pub(crate) fn public_command_contract_result(
        effective_tool_name: &str,
        parsed_arguments: &Value,
        history_items: &[HistoryItem],
        workspace_root: Option<&Utf8Path>,
    ) -> Option<ToolResult> {
        let latest_user_text =
            crate::agent::docs_semantic_contract::latest_user_authority_text(history_items);
        crate::agent::public_command_contract::public_command_contract_result(
            effective_tool_name,
            parsed_arguments,
            latest_user_text.as_deref(),
            workspace_root,
        )
    }

    pub(crate) fn record_public_command_contract_no_progress(
        counts: &mut BTreeMap<String, usize>,
        result: &ToolResult,
    ) -> ToolTerminalGuardDecision {
        let key = crate::agent::public_command_contract::public_command_contract_key(result);
        let count = counts
            .entry(key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let terminal_message = (*count >= PUBLIC_COMMAND_CONTRACT_TERMINAL_THRESHOLD).then(|| {
            crate::agent::public_command_contract::public_command_contract_terminal_message(
                result, *count,
            )
        });
        ToolTerminalGuardDecision {
            count: *count,
            terminal_message,
        }
    }

    pub(crate) fn artifact_content_shape_violation_result(
        effective_tool_name: &str,
        parsed_arguments: &Value,
        workspace_root: Option<&Utf8Path>,
    ) -> Option<ToolResult> {
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            effective_tool_name,
            parsed_arguments,
            workspace_root,
        )
    }

    pub(crate) fn authoring_target_grounding_required_result(
        tool_name: &str,
        arguments: &Value,
        state: &SessionStateSnapshot,
        envelope: &AuthoringGroundingRecoveryEnvelope,
    ) -> ToolResult {
        let targets = state
            .active_targets
            .iter()
            .map(|target| target.as_str().to_string())
            .collect::<Vec<_>>();
        let submitted_path = arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("<missing path>");
        let submitted_normalized = submitted_path.replace('\\', "/");
        let submitted_consumed = envelope.consumed_targets.iter().any(|target| {
            target == &submitted_normalized || submitted_normalized.ends_with(&format!("/{target}"))
        });
        let reason = if submitted_consumed {
            format!(
                "`{submitted_path}` is already grounded for this authoring recovery. Remaining read target(s): {}.",
                envelope.missing_text()
            )
        } else {
            format!(
                "`{submitted_path}` is not an admissible remaining grounding target. Remaining read target(s): {}.",
                envelope.missing_text()
            )
        };
        let result_hash = format!(
            "authoring_target_grounding_required:{}:{}:missing={}:consumed={}:active={}",
            tool_name,
            submitted_normalized,
            envelope.missing_grounding_targets.join("|"),
            envelope.consumed_targets.join("|"),
            targets.join("|")
        );
        ToolResult {
            title: "Authoring target grounding required".to_string(),
            output_text: format!(
                "Authoring supporting-context budget is exhausted. Runtime rejected `{tool_name}` before filesystem or workspace side effects. {reason} Consumed active target(s): {}. Active target set: {}. Use `read` only for remaining ungrounded active target(s), or use `write` / `apply_patch` to create content-changing progress.",
                envelope.consumed_text(),
                envelope.active_text()
            ),
            metadata: json!({
                "success": false,
                "authoring_target_grounding_required": true,
                "requested_tool": tool_name,
                "requested_arguments": arguments,
                "submitted_path": submitted_path,
                "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                "operation_progress_class": "authoring_target_grounding_required",
                "progress_effect": "no_progress",
                "active_targets": targets,
                "consumed_targets": envelope.consumed_targets.clone(),
                "missing_grounding_targets": envelope.missing_grounding_targets.clone(),
                "result_hash": result_hash,
                "tool_feedback_envelope": {
                    "kind": "authoring_target_grounding_required",
                    "success": false,
                    "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                    "operation_progress_class": "authoring_target_grounding_required",
                    "progress_effect": "no_progress",
                    "side_effects_applied": false,
                    "active_targets": targets,
                    "consumed_targets": envelope.consumed_targets.clone(),
                    "missing_grounding_targets": envelope.missing_grounding_targets.clone(),
                    "submitted_path": submitted_path,
                    "result_hash": result_hash
                },
                "terminal_guard_policy": {
                    "owner": "tool_lifecycle_runtime",
                    "no_progress_guard": true,
                    "side_effects_applied": false,
                    "terminal_after_repeated_corrections": AUTHORING_TARGET_GROUNDING_CORRECTION_TERMINAL_THRESHOLD
                }
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        }
    }

    pub(crate) fn generated_test_target_grounding_required_result(
        tool_name: &str,
        arguments: &Value,
        state: &SessionStateSnapshot,
    ) -> ToolResult {
        let targets = state
            .active_targets
            .iter()
            .map(|target| target.as_str().to_string())
            .collect::<Vec<_>>();
        let target_text = if targets.is_empty() {
            "one active generated-test target".to_string()
        } else {
            targets.join(", ")
        };
        let submitted_path = arguments
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("<missing path>");
        let result_hash = format!(
            "generated_test_target_grounding_required:{}:{}:{}",
            tool_name,
            submitted_path.replace('\\', "/"),
            targets.join("|")
        );
        ToolResult {
            title: "Generated-test active target grounding required".to_string(),
            output_text: format!(
                "The production source reference input is already current for this generated-test authoring turn. Runtime rejected `{tool_name}` before filesystem or workspace side effects because this lane only permits `read` for the current active generated-test target(s): {target_text}. Use `read` only for an active test target path if its current content is needed, then use `write` or `apply_patch` to update that test target."
            ),
            metadata: json!({
                "success": false,
                "generated_test_source_reference_consumed": true,
                "authoring_target_grounding_required": true,
                "requested_tool": tool_name,
                "requested_arguments": arguments,
                "submitted_path": submitted_path,
                "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                "operation_progress_class": "generated_test_target_grounding_required",
                "progress_effect": "no_progress",
                "active_targets": targets,
                "result_hash": result_hash,
                "tool_feedback_envelope": {
                    "kind": "generated_test_target_grounding_required",
                    "success": false,
                    "generated_test_source_reference_consumed": true,
                    "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                    "operation_progress_class": "generated_test_target_grounding_required",
                    "progress_effect": "no_progress",
                    "side_effects_applied": false,
                    "active_targets": targets,
                    "submitted_path": submitted_path,
                    "result_hash": result_hash
                },
                "terminal_guard_policy": {
                    "owner": "tool_lifecycle_runtime",
                    "no_progress_guard": true,
                    "side_effects_applied": false,
                    "terminal_after_repeated_corrections": AUTHORING_TARGET_GROUNDING_CORRECTION_TERMINAL_THRESHOLD
                }
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        }
    }

    pub(crate) fn record_authoring_target_grounding_required_no_progress(
        counts: &mut BTreeMap<String, usize>,
        result: &ToolResult,
    ) -> ToolTerminalGuardDecision {
        let key = authoring_target_grounding_required_key(result);
        let count = counts
            .entry(key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let terminal_message = (*count >= AUTHORING_TARGET_GROUNDING_CORRECTION_TERMINAL_THRESHOLD)
            .then(|| authoring_target_grounding_required_terminal_message(*count, result));
        ToolTerminalGuardDecision {
            count: *count,
            terminal_message,
        }
    }

    pub(crate) fn record_generated_test_target_grounding_required_no_progress(
        counts: &mut BTreeMap<String, usize>,
        result: &ToolResult,
        state: &SessionStateSnapshot,
    ) -> ToolTerminalGuardDecision {
        let key = generated_test_target_grounding_required_key(result);
        let count = counts
            .entry(key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let terminal_message = (*count >= AUTHORING_TARGET_GROUNDING_CORRECTION_TERMINAL_THRESHOLD)
            .then(|| generated_test_target_grounding_required_terminal_message(*count, state));
        ToolTerminalGuardDecision {
            count: *count,
            terminal_message,
        }
    }

    pub(crate) fn docs_route_supporting_context_budget_key(
        state: &SessionStateSnapshot,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
    ) -> String {
        let metadata = json!({
            "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
            "operation_progress_class": "supporting_context",
            "progress_effect": "no_progress"
        });
        operation_non_content_no_progress_key(
            "docs_route_supporting_context",
            &metadata,
            state,
            allowed_tools,
            tool_choice,
        )
    }

    pub(crate) fn docs_supporting_context_budget_exhausted_result(
        tool_name: &str,
        arguments: &Value,
        state: &SessionStateSnapshot,
    ) -> ToolResult {
        let targets = docs_supporting_context_budget_targets(state);
        let target_text = if targets.is_empty() {
            "one pending docs deliverable".to_string()
        } else {
            targets.join(", ")
        };
        let result_hash = format!(
            "docs_supporting_context_budget_exhausted:{}:{}",
            tool_name,
            targets.join("|")
        );
        ToolResult {
            title: "Docs supporting context budget exhausted".to_string(),
            output_text: format!(
                "Docs route supporting-context budget is exhausted for this authoring step. Runtime rejected `{tool_name}` before filesystem or workspace side effects. Do not continue broad read/list/search discovery now. Use `write` or `apply_patch` to create or update one pending docs deliverable: {target_text}. Use `不明` for still-unconfirmed details instead of opening more source context."
            ),
            metadata: json!({
                "success": false,
                "docs_supporting_context_budget_exhausted": true,
                "requested_tool": tool_name,
                "requested_arguments": arguments,
                "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                "operation_progress_class": "docs_supporting_context_budget_exhausted",
                "progress_effect": "no_progress",
                "active_targets": targets,
                "result_hash": result_hash,
                "tool_feedback_envelope": {
                    "kind": "docs_supporting_context_budget_exhausted",
                    "success": false,
                    "operation_intent": OperationIntent::ContentChangingAuthoringRequired.as_str(),
                    "operation_progress_class": "docs_supporting_context_budget_exhausted",
                    "progress_effect": "no_progress",
                    "side_effects_applied": false,
                    "active_targets": targets,
                    "result_hash": result_hash
                },
                "terminal_guard_policy": {
                    "owner": "tool_lifecycle_runtime",
                    "no_progress_guard": true,
                    "side_effects_applied": false,
                    "terminal_after_repeated_corrections": DOCS_ROUTE_BUDGET_EXHAUSTED_CORRECTION_TERMINAL_THRESHOLD
                }
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::new(),
        }
    }

    pub(crate) fn record_docs_supporting_context_budget_exhausted_no_progress(
        counts: &mut BTreeMap<String, usize>,
        budget_key: String,
        state: &SessionStateSnapshot,
    ) -> ToolTerminalGuardDecision {
        let count = counts
            .entry(budget_key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let terminal_message = (*count
            >= DOCS_ROUTE_BUDGET_EXHAUSTED_CORRECTION_TERMINAL_THRESHOLD)
            .then(|| docs_supporting_context_budget_exhausted_terminal_message(*count, state));
        ToolTerminalGuardDecision {
            count: *count,
            terminal_message,
        }
    }

    pub(crate) fn record_operation_non_content_no_progress(
        counts: &mut BTreeMap<String, usize>,
        effective_tool_name: &str,
        metadata: &Value,
        state: &SessionStateSnapshot,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
        open_authoring_required: bool,
    ) -> Option<OperationNoProgressGuardDecision> {
        if !operation_non_content_no_progress_under_open_authoring(
            metadata,
            open_authoring_required,
        ) {
            return None;
        }
        let key = operation_non_content_no_progress_key(
            effective_tool_name,
            metadata,
            state,
            allowed_tools,
            tool_choice,
        );
        let count = counts
            .entry(key.clone())
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let progress_class = operation_progress_class_from_metadata(metadata)
            .unwrap_or("")
            .to_string();
        let repair_supporting_context_exhausted =
            repair_supporting_context_budget_applies(
                &progress_class,
                state,
                open_authoring_required,
            ) && repair_supporting_context_budget_exhausts_for_metadata(
                effective_tool_name,
                metadata,
                state,
            ) && *count >= REPAIR_SUPPORTING_CONTEXT_BUDGET_THRESHOLD;
        let terminal =
            should_terminalize_operation_non_content_no_progress_for_state(*count, state);
        let budget_exhaustion = if terminal
            && docs_route_semantic_operation_no_progress(state, &progress_class)
            && progress_class == "supporting_context"
        {
            Some(OperationNoProgressBudgetExhaustion::DocsSupportingContext)
        } else if terminal
            && authoring_supporting_context_budget_applies(
                &progress_class,
                state,
                open_authoring_required,
            )
        {
            Some(OperationNoProgressBudgetExhaustion::AuthoringSupportingContext)
        } else if (terminal || repair_supporting_context_exhausted)
            && repair_supporting_context_budget_applies(
                &progress_class,
                state,
                open_authoring_required,
            )
        {
            Some(OperationNoProgressBudgetExhaustion::RepairSupportingContext)
        } else {
            None
        };
        let terminal_message = (terminal && budget_exhaustion.is_none()).then(|| {
            operation_non_content_no_progress_terminal_message(
                effective_tool_name,
                *count,
                metadata,
                state,
            )
        });
        Some(OperationNoProgressGuardDecision {
            key,
            count: *count,
            budget_exhaustion,
            terminal_message,
        })
    }

    pub(crate) fn record_corrective_content_shape_no_progress(
        counts: &mut BTreeMap<String, usize>,
        effective_tool_name: &str,
        metadata: &Value,
        state: &SessionStateSnapshot,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
        open_authoring_required: bool,
    ) -> Option<ToolTerminalGuardDecision> {
        if operation_progress_class_from_metadata(metadata) != Some("no_progress") {
            return None;
        }
        Self::record_operation_non_content_no_progress(
            counts,
            effective_tool_name,
            metadata,
            state,
            allowed_tools,
            tool_choice,
            open_authoring_required,
        )
        .and_then(|decision| {
            decision
                .terminal_message
                .map(|terminal_message| ToolTerminalGuardDecision {
                    count: decision.count,
                    terminal_message: Some(terminal_message),
                })
        })
    }

    pub(crate) fn record_verification_supporting_context_no_progress(
        counts: &mut BTreeMap<String, usize>,
        effective_tool_name: &str,
        arguments_json: &str,
        result: &ToolResult,
        state: &SessionStateSnapshot,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
    ) -> Option<ToolTerminalGuardDecision> {
        if !verification_supporting_context_no_progress_under_active_verification(
            effective_tool_name,
            result,
            state,
        ) {
            return None;
        }
        let key = verification_supporting_context_no_progress_key(
            effective_tool_name,
            arguments_json,
            state,
            allowed_tools,
            tool_choice,
        );
        let count = counts
            .entry(key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let terminal_message =
            should_terminalize_verification_supporting_context_no_progress(*count).then(|| {
                verification_supporting_context_no_progress_terminal_message(
                    effective_tool_name,
                    *count,
                    state,
                )
            });
        Some(ToolTerminalGuardDecision {
            count: *count,
            terminal_message,
        })
    }

    pub(crate) fn record_same_verification_failure_no_progress(
        counts: &mut BTreeMap<String, usize>,
        metadata: &Value,
    ) -> Option<ToolTerminalGuardDecision> {
        let key = same_verification_failure_no_progress_key(metadata)?;
        let count = counts
            .entry(key)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        let terminal_message = should_terminalize_same_verification_failure(*count)
            .then(|| same_verification_failure_terminal_message(*count));
        Some(ToolTerminalGuardDecision {
            count: *count,
            terminal_message,
        })
    }

    pub(crate) fn operation_non_content_no_progress_key(
        effective_tool_name: &str,
        metadata: &Value,
        state: &SessionStateSnapshot,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
    ) -> String {
        operation_non_content_no_progress_key(
            effective_tool_name,
            metadata,
            state,
            allowed_tools,
            tool_choice,
        )
    }

    pub(crate) fn operation_non_content_no_progress_under_open_authoring(
        metadata: &Value,
        state: &SessionStateSnapshot,
    ) -> bool {
        operation_non_content_no_progress_under_open_authoring(
            metadata,
            open_executable_work_requires_tool_call(state),
        )
    }

    pub(crate) fn operation_progress_class_from_metadata(metadata: &Value) -> Option<&str> {
        operation_progress_class_from_metadata(metadata)
    }

    pub(crate) fn should_terminalize_operation_non_content_no_progress(
        progress_count: usize,
    ) -> bool {
        progress_count >= OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD
    }

    pub(crate) fn should_terminalize_operation_non_content_no_progress_for_state(
        progress_count: usize,
        state: &SessionStateSnapshot,
    ) -> bool {
        should_terminalize_operation_non_content_no_progress_for_state(progress_count, state)
    }

    pub(crate) fn authoring_supporting_context_budget_applies(
        progress_class: &str,
        state: &SessionStateSnapshot,
    ) -> bool {
        authoring_supporting_context_budget_applies(
            progress_class,
            state,
            open_executable_work_requires_tool_call(state),
        )
    }

    pub(crate) fn repair_supporting_context_budget_applies(
        progress_class: &str,
        state: &SessionStateSnapshot,
    ) -> bool {
        repair_supporting_context_budget_applies(
            progress_class,
            state,
            open_executable_work_requires_tool_call(state),
        )
    }

    pub(crate) fn repair_supporting_context_budget_exhausts_for_metadata(
        effective_tool_name: &str,
        metadata: &Value,
        state: &SessionStateSnapshot,
    ) -> bool {
        repair_supporting_context_budget_exhausts_for_metadata(effective_tool_name, metadata, state)
    }

    pub(crate) fn verification_supporting_context_no_progress_under_active_verification(
        tool_name: &str,
        arguments_json: &str,
        result: &ToolResult,
        state: &SessionStateSnapshot,
    ) -> bool {
        let _ = arguments_json;
        verification_supporting_context_no_progress_under_active_verification(
            tool_name, result, state,
        )
    }

    pub(crate) fn verification_supporting_context_no_progress_key(
        effective_tool_name: &str,
        arguments_json: &str,
        state: &SessionStateSnapshot,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
    ) -> String {
        verification_supporting_context_no_progress_key(
            effective_tool_name,
            arguments_json,
            state,
            allowed_tools,
            tool_choice,
        )
    }

    pub(crate) fn should_terminalize_verification_supporting_context_no_progress(
        progress_count: usize,
    ) -> bool {
        should_terminalize_verification_supporting_context_no_progress(progress_count)
    }

    pub(crate) fn same_verification_failure_no_progress_key(metadata: &Value) -> Option<String> {
        same_verification_failure_no_progress_key(metadata)
    }

    pub(crate) fn should_terminalize_same_verification_failure(failure_count: usize) -> bool {
        should_terminalize_same_verification_failure(failure_count)
    }

    pub(crate) fn same_verification_failure_terminal_message(failure_count: usize) -> String {
        same_verification_failure_terminal_message(failure_count)
    }

    pub(crate) fn verification_run_passed(metadata: &Value) -> bool {
        verification_run_passed(metadata)
    }
}

pub(crate) struct RejectedToolNoProgressGuardRequest<'a> {
    pub effective_tool_name: &'a str,
    pub effective_arguments_json: &'a str,
    pub allowed_tools: &'a BTreeSet<String>,
    pub tool_choice: &'a ToolChoice,
    pub required_action: Option<&'a RequiredAction>,
    pub provider_noncompliance: bool,
    pub semantic_class: &'a str,
    pub result_hash: Option<&'a str>,
    pub recovery_no_progress_key: Option<&'a str>,
}

pub(crate) struct ToolTerminalGuardDecision {
    pub count: usize,
    pub terminal_message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OperationNoProgressBudgetExhaustion {
    DocsSupportingContext,
    AuthoringSupportingContext,
    RepairSupportingContext,
}

pub(crate) struct OperationNoProgressGuardDecision {
    pub key: String,
    pub count: usize,
    pub budget_exhaustion: Option<OperationNoProgressBudgetExhaustion>,
    pub terminal_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthoringGroundingRecoveryEnvelope {
    pub(crate) active_targets: Vec<String>,
    pub(crate) consumed_targets: Vec<String>,
    pub(crate) missing_grounding_targets: Vec<String>,
}

impl AuthoringGroundingRecoveryEnvelope {
    pub(crate) fn missing_text(&self) -> String {
        if self.missing_grounding_targets.is_empty() {
            "none; recovery is edit-only".to_string()
        } else {
            self.missing_grounding_targets.join(", ")
        }
    }

    pub(crate) fn consumed_text(&self) -> String {
        if self.consumed_targets.is_empty() {
            "none".to_string()
        } else {
            self.consumed_targets.join(", ")
        }
    }

    pub(crate) fn active_text(&self) -> String {
        if self.active_targets.is_empty() {
            "none".to_string()
        } else {
            self.active_targets.join(", ")
        }
    }

    pub(crate) fn evidence_ref(&self) -> String {
        format!(
            "active={};consumed={};missing={}",
            self.active_text(),
            self.consumed_text(),
            self.missing_text()
        )
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
        if file_change_content_evidence_is_non_satisfying(&result.metadata) {
            return "empty_artifact_no_progress";
        }
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
        "empty_artifact_no_progress" => {
            "This tool changed filesystem state, but the changed artifact has no content-bearing after-state and does not satisfy requested authoring work."
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

fn file_change_content_evidence_is_non_satisfying(metadata: &Value) -> bool {
    metadata
        .get("file_change_content_evidence")
        .and_then(|value| value.get("content_bearing"))
        .and_then(Value::as_bool)
        == Some(false)
}

fn content_satisfying_change_summaries_for_protocol(
    result: &ToolResult,
    metadata: &Value,
) -> Vec<crate::edit::ChangeSummary> {
    let Some(content_evidence) = metadata.get("file_change_content_evidence") else {
        return result.change_summaries.clone();
    };
    if content_evidence
        .get("content_bearing")
        .and_then(Value::as_bool)
        == Some(false)
    {
        return Vec::new();
    }
    let Some(content_bearing_ids) = content_evidence
        .get("content_bearing_change_ids")
        .and_then(Value::as_array)
    else {
        return result.change_summaries.clone();
    };
    let content_bearing_ids = content_bearing_ids
        .iter()
        .filter_map(Value::as_str)
        .collect::<BTreeSet<_>>();
    if content_bearing_ids.is_empty() {
        return Vec::new();
    }
    result
        .change_summaries
        .iter()
        .filter(|change| content_bearing_ids.contains(change.change_id.to_string().as_str()))
        .cloned()
        .collect()
}

fn normalized_arguments_for_hash(arguments_json: &str) -> String {
    serde_json::from_str::<Value>(arguments_json)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| arguments_json.trim().to_string())
}

const PROVIDER_NONCOMPLIANCE_TERMINAL_THRESHOLD: usize = 3;
const EXECUTED_TOOL_FAILURE_TERMINAL_THRESHOLD: usize = 3;
const WRONG_VERIFICATION_COMMAND_TERMINAL_THRESHOLD: usize = 3;
const WRONG_AUTHORING_TARGET_TERMINAL_THRESHOLD: usize = 3;
const WRONG_REPAIR_TARGET_TERMINAL_THRESHOLD: usize = 3;
const PUBLIC_COMMAND_CONTRACT_TERMINAL_THRESHOLD: usize = 3;
const AUTHORING_TARGET_GROUNDING_CORRECTION_TERMINAL_THRESHOLD: usize = 3;
const DOCS_ROUTE_BUDGET_EXHAUSTED_CORRECTION_TERMINAL_THRESHOLD: usize = 3;
const OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD: usize = 3;
const DOCS_ROUTE_OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD: usize = 8;
const REPAIR_SUPPORTING_CONTEXT_BUDGET_THRESHOLD: usize = 1;
const VERIFICATION_SUPPORTING_CONTEXT_NO_PROGRESS_TERMINAL_THRESHOLD: usize = 3;
const SAME_VERIFICATION_FAILURE_TERMINAL_THRESHOLD: usize = 3;

pub(crate) fn rejected_tool_no_progress_key(
    effective_tool_name: &str,
    effective_arguments_json: &str,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
    required_action: Option<&RequiredAction>,
) -> String {
    let _ = effective_arguments_json;
    let required_action_projection = required_action
        .map(RequiredAction::projection_label)
        .unwrap_or("none");
    format!(
        "rejected_tool|tool={}|allowed={}|choice={}|required_action={required_action_projection}",
        effective_tool_name,
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_choice_label(tool_choice),
    )
}

fn should_terminalize_rejected_tool_no_progress(rejection_count: usize) -> bool {
    rejection_count >= 3
}

pub(crate) fn rejected_tool_no_progress_terminal_message(
    effective_tool_name: &str,
    rejection_count: usize,
    allowed_tools: &BTreeSet<String>,
    required_action: Option<&RequiredAction>,
) -> String {
    let allowed = allowed_tools.iter().cloned().collect::<Vec<_>>().join(", ");
    let required = required_action
        .map(RequiredAction::projection_label)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!(" Required action: {value}."))
        .unwrap_or_default();
    format!(
        "Tool `{}` was disallowed {} time(s) without state progress. Runtime stopped this run instead of continuing unavailable-tool feedback until the turn step budget. Allowed tools for this turn: {}.{}",
        effective_tool_name, rejection_count, allowed, required
    )
}

fn fixture_required_edit_action(tool: ToolName, target: &str) -> RequiredAction {
    let prefix = match tool {
        ToolName::ApplyPatch => "apply_patch",
        ToolName::Write => "write",
        _ => "edit",
    };
    RequiredAction {
        kind: crate::protocol::RequiredActionKind::EditTarget,
        tool,
        target: Some(Utf8PathBuf::from(target)),
        command: None,
        projection_text: format!("{prefix}:{target}"),
    }
}

pub(crate) fn executed_tool_failure_no_progress_key(
    effective_tool_name: &str,
    effective_arguments_json: &str,
    allowed_tools: &BTreeSet<String>,
    error_text: &str,
) -> String {
    format!(
        "{}|{}|{}|{}",
        effective_tool_name,
        normalized_arguments_for_hash(effective_arguments_json),
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_error_class(error_text)
    )
}

pub(crate) fn executed_tool_failure_terminal_message(
    tool_name: &str,
    failure_count: usize,
    error_text: &str,
) -> String {
    format!(
        "Tool `{tool_name}` failed with the same no-progress execution error {failure_count} time(s). Runtime stopped before repeating the same failed call-id-scoped tool output until the turn step budget. Error class: {}. Latest error: {error_text}",
        tool_error_class(error_text)
    )
}

fn tool_choice_label(tool_choice: &ToolChoice) -> String {
    match tool_choice {
        ToolChoice::Auto => "auto".to_string(),
        ToolChoice::Required => "required".to_string(),
        ToolChoice::None => "none".to_string(),
        ToolChoice::Named(tool) => format!("named:{tool}"),
    }
}

fn verification_commands_for_active_work(
    active_work: Option<&ActiveWorkContract>,
) -> Option<&[String]> {
    match active_work {
        Some(ActiveWorkContract::Verification { commands, .. }) if !commands.is_empty() => {
            Some(commands.as_slice())
        }
        _ => None,
    }
}

fn canonical_shell_command_keys(command: &str) -> BTreeSet<String> {
    let mut keys = verification_command_satisfaction_keys(command);
    if let Some(key) = canonical_verification_command_identity_key(command) {
        keys.insert(key);
    }
    keys
}

fn verification_command_key_family_matches(
    submitted_keys: &BTreeSet<String>,
    required_keys: &BTreeSet<String>,
) -> bool {
    if submitted_keys.is_empty() || required_keys.is_empty() {
        return false;
    }
    submitted_keys.iter().any(|submitted| {
        required_keys.iter().any(|required| {
            submitted == required
                || submitted.starts_with(&format!("{required} "))
                || required.starts_with(&format!("{submitted} "))
        })
    })
}

fn canonical_required_verification_commands(required_commands: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut commands = Vec::new();
    for command in required_commands {
        let key = canonical_verification_command_identity_key(command).unwrap_or_else(|| {
            command
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_ascii_lowercase()
        });
        if seen.insert(key) {
            commands.push(command.clone());
        }
    }
    commands
}

fn executable_verification_command_forms(
    required_commands: &[String],
    shell_family: crate::config::ShellFamily,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut commands = Vec::new();
    for command in required_commands {
        let executable = if let Some(suggested) =
            crate::tool::shell::command_text_encoding_suggested_command(command, shell_family)
        {
            suggested
        } else {
            command.clone()
        };
        if seen.insert(executable.clone()) {
            commands.push(executable);
        }
    }
    commands
}

fn wrong_verification_command_key(
    parsed_arguments: &Value,
    active_work: Option<&ActiveWorkContract>,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> String {
    let command = parsed_arguments
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let required = verification_commands_for_active_work(active_work)
        .map(|commands| canonical_required_verification_commands(commands).join("|"))
        .unwrap_or_default();
    format!(
        "command={command}|required={required}|allowed={}|choice={}",
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_choice_label(tool_choice)
    )
}

fn wrong_verification_command_terminal_message(result: &ToolResult, count: usize) -> String {
    format!(
        "Submitted shell command did not match the remaining required verification command {count} time(s): {}",
        result.output_text
    )
}

fn operation_content_changing_tool_name(tool_name: &str) -> bool {
    matches!(tool_name, "write" | "apply_patch")
}

fn active_requested_work_targets(
    active_work: Option<&ActiveWorkContract>,
) -> Option<&[Utf8PathBuf]> {
    match active_work {
        Some(ActiveWorkContract::RequestedWorkAuthoring {
            pending_targets, ..
        }) if !pending_targets.is_empty() => Some(pending_targets.as_slice()),
        Some(ActiveWorkContract::DocsRepair {
            deliverable: Some(deliverable),
            ..
        }) => Some(std::slice::from_ref(deliverable)),
        Some(ActiveWorkContract::DocsRepair {
            pending_deliverables,
            ..
        }) if !pending_deliverables.is_empty() => None,
        _ => None,
    }
}

fn submitted_authoring_targets(tool_name: &str, parsed_arguments: &Value) -> Vec<String> {
    let mut targets = BTreeSet::new();
    match tool_name {
        "write" => {
            if let Some(path) = parsed_arguments.get("path").and_then(Value::as_str) {
                targets.insert(path.trim().to_string());
            }
        }
        "apply_patch" => {
            if let Some(patch_text) = parsed_arguments.get("patch_text").and_then(Value::as_str) {
                targets.extend(apply_patch_declared_targets(patch_text));
            }
        }
        _ => {}
    }
    targets
        .into_iter()
        .filter(|target| !target.trim().is_empty())
        .collect()
}

fn shell_file_probe_targets(command: &str) -> Vec<String> {
    let mut targets = BTreeSet::new();
    for segment in command.split(';') {
        let tokens = segment
            .split_whitespace()
            .map(|token| token.trim_matches(|c| c == '\'' || c == '"' || c == '`'))
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        let Some((command_index, command_name)) =
            tokens.iter().enumerate().find_map(|(index, token)| {
                let normalized = token
                    .trim_start_matches('&')
                    .trim_matches(|c| c == '\'' || c == '"')
                    .to_ascii_lowercase();
                matches!(normalized.as_str(), "get-content" | "cat" | "type" | "gc")
                    .then_some((index, normalized))
            })
        else {
            continue;
        };
        let mut index = command_index + 1;
        while index < tokens.len() {
            let token = tokens[index].trim_matches(|c| c == '\'' || c == '"' || c == '`');
            if token.is_empty() {
                index += 1;
                continue;
            }
            let lower = token.to_ascii_lowercase();
            if lower == "-path" || lower == "-literalpath" {
                if let Some(value) = tokens.get(index + 1) {
                    let value = value.trim_matches(|c| c == '\'' || c == '"' || c == '`');
                    if !value.is_empty() {
                        targets.insert(value.to_string());
                    }
                }
                index += 2;
                continue;
            }
            if lower == "-encoding"
                || lower == "-totalcount"
                || lower == "-tail"
                || lower == "-head"
                || lower == "-readcount"
            {
                index += 2;
                continue;
            }
            if token.starts_with('-') {
                index += 1;
                continue;
            }
            if command_name == "type" && token.eq_ignore_ascii_case("nul") {
                index += 1;
                continue;
            }
            targets.insert(token.to_string());
            index += 1;
        }
    }
    targets.into_iter().collect()
}

fn apply_patch_declared_targets(patch_text: &str) -> BTreeSet<String> {
    let mut targets = BTreeSet::new();
    for line in patch_text.lines() {
        for marker in [
            "*** Add File: ",
            "*** Update File: ",
            "*** Delete File: ",
            "*** Move to: ",
        ] {
            if let Some(target) = line.strip_prefix(marker) {
                let target = target.trim();
                if !target.is_empty() {
                    targets.insert(target.to_string());
                }
            }
        }
    }
    targets
}

fn wrong_authoring_target_key(
    effective_tool_name: &str,
    parsed_arguments: &Value,
    active_work: Option<&ActiveWorkContract>,
    workspace_root: &Utf8Path,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> String {
    let submitted = submitted_authoring_targets(effective_tool_name, parsed_arguments)
        .into_iter()
        .flat_map(|target| normalized_target_keys(&target, workspace_root))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    let active = active_requested_work_targets(active_work)
        .map(|targets| {
            targets
                .iter()
                .flat_map(|target| normalized_target_keys(target.as_str(), workspace_root))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    format!(
        "tool={effective_tool_name}|submitted={submitted}|active={active}|allowed={}|choice={}",
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_choice_label(tool_choice),
    )
}

fn wrong_authoring_target_terminal_message(result: &ToolResult, count: usize) -> String {
    format!(
        "Submitted content-changing calls missed the active requested-work deliverable set {count} time(s): {}",
        result.output_text
    )
}

fn repair_target_authority_violation_key(
    result: &ToolResult,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> String {
    let result_hash = tool_result_result_hash(&result.metadata)
        .unwrap_or_else(|| crate::harness::artifact::hash_bytes(result.output_text.as_bytes()));
    let submitted = metadata_string_array(&result.metadata, "submitted_targets").join(",");
    let active = metadata_string_array(&result.metadata, "active_repair_targets").join(",");
    format!(
        "repair_target_authority_violation|hash={result_hash}|submitted={submitted}|active={active}|allowed={}|choice={}",
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_choice_label(tool_choice),
    )
}

fn repair_target_authority_violation_terminal_message(result: &ToolResult, count: usize) -> String {
    format!(
        "Submitted content-changing calls missed the exact repair target {count} time(s): {}",
        result.output_text
    )
}

fn generated_test_target_grounding_required_key(result: &ToolResult) -> String {
    let targets = metadata_string_array(&result.metadata, "active_targets").join("|");
    let submitted_path = result
        .metadata
        .get("submitted_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    format!(
        "generated_test_target_grounding_required|submitted={}|active={targets}",
        submitted_path.replace('\\', "/")
    )
}

fn generated_test_target_grounding_required_terminal_message(
    correction_count: usize,
    state: &SessionStateSnapshot,
) -> String {
    let targets = state
        .active_targets
        .iter()
        .map(|target| target.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Generated-test source reference input was already grounded and the model repeated non-active source read proposals {correction_count} time(s) instead of reading or editing the active generated-test target. Runtime stopped before growing provider history with more no-progress corrections. Active generated-test target paths: {targets}."
    )
}

fn authoring_target_grounding_required_key(result: &ToolResult) -> String {
    let targets = metadata_string_array(&result.metadata, "active_targets").join("|");
    let missing_targets =
        metadata_string_array(&result.metadata, "missing_grounding_targets").join("|");
    let consumed_targets = metadata_string_array(&result.metadata, "consumed_targets").join("|");
    let submitted_path = result
        .metadata
        .get("submitted_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    format!(
        "authoring_target_grounding_required|submitted={}|missing={missing_targets}|consumed={consumed_targets}|active={targets}",
        submitted_path.replace('\\', "/")
    )
}

fn authoring_target_grounding_required_terminal_message(
    correction_count: usize,
    result: &ToolResult,
) -> String {
    let active_targets = metadata_string_array(&result.metadata, "active_targets");
    let missing_targets = metadata_string_array(&result.metadata, "missing_grounding_targets");
    let consumed_targets = metadata_string_array(&result.metadata, "consumed_targets");
    let active = active_targets.join(", ");
    let missing = missing_targets.join(", ");
    let consumed = consumed_targets.join(", ");
    let submitted = result
        .metadata
        .get("submitted_path")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    let submitted_normalized = normalize_target_key(submitted);
    let submitted_consumed = consumed_targets.iter().any(|target| {
        submitted_normalized == *target || submitted_normalized.ends_with(&format!("/{target}"))
    });
    let proposal_kind = if submitted_consumed {
        "consumed active target read proposals"
    } else {
        "non-remaining active target read proposals"
    };
    format!(
        "Authoring supporting-context budget was exhausted and the model repeated {proposal_kind} {correction_count} time(s) for `{submitted}` instead of reading the remaining target or producing file-change evidence. Runtime stopped before growing provider history with more no-progress corrections. Consumed target paths: {consumed}. Remaining read target paths: {missing}. Active target set: {active}."
    )
}

fn metadata_string_array(metadata: &Value, key: &str) -> Vec<String> {
    metadata
        .get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn docs_supporting_context_budget_targets(state: &SessionStateSnapshot) -> Vec<String> {
    state
        .active_targets
        .iter()
        .map(|target| target.as_str().to_string())
        .collect()
}

fn docs_supporting_context_budget_exhausted_terminal_message(
    correction_count: usize,
    state: &SessionStateSnapshot,
) -> String {
    let targets = docs_supporting_context_budget_targets(state).join(", ");
    format!(
        "Docs route supporting-context budget was exhausted and the model repeated budget-exhausted discovery {correction_count} time(s) instead of producing file-change evidence. Runtime stopped before growing provider history with more no-progress tool calls. Open docs targets: {targets}."
    )
}

fn operation_non_content_no_progress_key(
    effective_tool_name: &str,
    metadata: &Value,
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> String {
    let progress_class = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_progress_class"))
        .or_else(|| metadata.get("operation_progress_class"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let result_hash = if docs_route_semantic_operation_no_progress(state, progress_class) {
        None
    } else {
        tool_result_result_hash(metadata)
    };
    let tool_name = if docs_route_semantic_operation_no_progress(state, progress_class) {
        "docs_route_supporting_context"
    } else {
        effective_tool_name
    };
    format!(
        "operation_intent=content_changing_authoring_required|progress_class={progress_class}|{}",
        progress_projection_no_progress_key(
            tool_name,
            state,
            allowed_tools,
            tool_choice,
            result_hash.as_deref(),
        )
    )
}

fn operation_progress_class_from_metadata(metadata: &Value) -> Option<&str> {
    metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_progress_class"))
        .or_else(|| metadata.get("operation_progress_class"))
        .and_then(Value::as_str)
}

fn operation_non_content_no_progress_under_open_authoring(
    metadata: &Value,
    open_authoring_required: bool,
) -> bool {
    if !open_authoring_required {
        return false;
    }
    let operation_intent = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_intent"))
        .or_else(|| metadata.get("operation_intent"))
        .and_then(Value::as_str);
    let progress_class = operation_progress_class_from_metadata(metadata);
    operation_intent == Some(OperationIntent::ContentChangingAuthoringRequired.as_str())
        && matches!(
            progress_class,
            Some("supporting_context" | "no_progress" | "docs_spec_semantic_reconciliation_failed")
        )
}

fn open_executable_work_requires_tool_call(state: &SessionStateSnapshot) -> bool {
    if matches!(
        state.route,
        TaskRoute::Ask | TaskRoute::Review | TaskRoute::Summary
    ) {
        return false;
    }
    !closeout_ready_final_message_authority(state)
        && (state.completion.open_work_count > 0
            || !state.active_targets.is_empty()
            || state.completion.verification_pending
            || !state.verification.required_commands.is_empty())
}

fn closeout_ready_final_message_authority(state: &SessionStateSnapshot) -> bool {
    (state.completion.closeout_ready || answer_only_final_message_authority(state))
        && state.completion.open_work_count == 0
        && !state.completion.verification_pending
        && !state.completion.route_contract_pending
        && state.completion.blocked_reason.is_none()
        && state.active_targets.is_empty()
        && state.verification.required_commands.is_empty()
        && state.verification.failure_cluster.is_none()
        && state.failure.is_none()
}

fn answer_only_final_message_authority(state: &SessionStateSnapshot) -> bool {
    matches!(
        state.process_phase,
        crate::session::ProcessPhase::Discover | crate::session::ProcessPhase::Closeout
    ) && state.completion.open_work_count == 0
        && !state.completion.verification_pending
        && !state.completion.route_contract_pending
        && state.completion.blocked_reason.is_none()
        && state.active_targets.is_empty()
        && state.verification.required_commands.is_empty()
        && state.verification.failing_labels.is_empty()
        && state.verification.failure_cluster.is_none()
        && state.failure.is_none()
}

fn docs_route_semantic_operation_no_progress(
    state: &SessionStateSnapshot,
    progress_class: &str,
) -> bool {
    state.route == TaskRoute::Docs
        && state.completion.route_contract_pending
        && matches!(
            progress_class,
            "supporting_context"
                | "progress_projection"
                | "docs_spec_semantic_reconciliation_failed"
        )
}

fn should_terminalize_operation_non_content_no_progress_for_state(
    progress_count: usize,
    state: &SessionStateSnapshot,
) -> bool {
    let threshold = if state.route == TaskRoute::Docs && state.completion.route_contract_pending {
        DOCS_ROUTE_OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD
    } else {
        OPERATION_NON_CONTENT_NO_PROGRESS_TERMINAL_THRESHOLD
    };
    progress_count >= threshold
}

fn authoring_supporting_context_budget_applies(
    progress_class: &str,
    state: &SessionStateSnapshot,
    open_authoring_required: bool,
) -> bool {
    state.route != TaskRoute::Docs
        && open_authoring_required
        && !state.active_targets.is_empty()
        && progress_class == "supporting_context"
}

fn repair_supporting_context_budget_applies(
    progress_class: &str,
    state: &SessionStateSnapshot,
    open_authoring_required: bool,
) -> bool {
    state.route != TaskRoute::Docs
        && open_authoring_required
        && state.process_phase == crate::session::ProcessPhase::Repair
        && state.completion.verification_pending
        && !state.active_targets.is_empty()
        && progress_class == "supporting_context"
}

fn repair_supporting_context_budget_exhausts_for_metadata(
    effective_tool_name: &str,
    metadata: &Value,
    state: &SessionStateSnapshot,
) -> bool {
    effective_tool_name == "read" && metadata_path_matches_active_target(metadata, state)
}

fn metadata_path_matches_active_target(metadata: &Value, state: &SessionStateSnapshot) -> bool {
    let Some(path) = metadata.get("path").and_then(Value::as_str) else {
        return false;
    };
    let normalized_path = normalize_path_for_target_match(path);
    state.active_targets.iter().any(|target| {
        let normalized_target = normalize_path_for_target_match(target.as_str());
        normalized_path == normalized_target
            || normalized_path.ends_with(&format!("/{normalized_target}"))
    })
}

fn normalize_path_for_target_match(path: &str) -> String {
    path.replace('\\', "/").trim_start_matches("./").to_string()
}

fn operation_non_content_no_progress_terminal_message(
    tool_name: &str,
    progress_count: usize,
    metadata: &Value,
    state: &SessionStateSnapshot,
) -> String {
    let progress_class = operation_progress_class_from_metadata(metadata).unwrap_or("non_content");
    let targets = state
        .active_targets
        .iter()
        .map(|target| target.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if state.route == TaskRoute::Docs && state.completion.route_contract_pending {
        format!(
            "Tool `{tool_name}` returned `{progress_class}` output {progress_count} time(s) while docs authoring is required. Runtime stopped before allowing more no-progress docs-route turns to grow provider history. Use write/apply_patch for one pending docs deliverable, remove contradictory docs/spec claims, and use `不明` for still-unconfirmed details. Open targets: {targets}."
        )
    } else {
        format!(
            "Tool `{tool_name}` returned `{progress_class}` output {progress_count} time(s) while content-changing authoring is required. Runtime stopped before treating non-content tool calls as artifact progress. Use apply_patch or equivalent file-change evidence for open targets: {targets}."
        )
    }
}

fn verification_supporting_context_no_progress_under_active_verification(
    tool_name: &str,
    result: &ToolResult,
    state: &SessionStateSnapshot,
) -> bool {
    state.completion.verification_pending
        && !state.verification.required_commands.is_empty()
        && verification_supporting_context_tool_name(tool_name)
        && result.recorded_changes.is_empty()
        && result.change_summaries.is_empty()
}

fn verification_supporting_context_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read"
            | "list"
            | "glob"
            | "grep"
            | "inspect_directory"
            | "skill"
            | "docling_convert"
            | "mcp_call"
            | "todowrite"
    )
}

fn verification_supporting_context_no_progress_key(
    effective_tool_name: &str,
    arguments_json: &str,
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
) -> String {
    let normalized_args = serde_json::from_str::<Value>(arguments_json)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| arguments_json.trim().to_string());
    format!(
        "verification_supporting_context|tool={effective_tool_name}|args={normalized_args}|{}",
        progress_projection_no_progress_key(
            "verification_supporting_context",
            state,
            allowed_tools,
            tool_choice,
            None,
        )
    )
}

fn should_terminalize_verification_supporting_context_no_progress(progress_count: usize) -> bool {
    progress_count >= VERIFICATION_SUPPORTING_CONTEXT_NO_PROGRESS_TERMINAL_THRESHOLD
}

fn verification_supporting_context_no_progress_terminal_message(
    tool_name: &str,
    progress_count: usize,
    state: &SessionStateSnapshot,
) -> String {
    let commands = state.verification.required_commands.join(", ");
    format!(
        "Tool `{tool_name}` returned supporting context {progress_count} time(s) while verification commands remain pending. Runtime stopped before repeating context-only calls until the turn step budget. Run one of the remaining verification commands instead: {commands}."
    )
}

fn same_verification_failure_no_progress_key(metadata: &Value) -> Option<String> {
    let run = verification_run_from_metadata(metadata)?;
    if !matches!(
        run.status,
        VerificationRunStatus::Failed | VerificationRunStatus::TimedOut
    ) {
        return None;
    }
    let command_key =
        verification_command_identity_key(&run.command).unwrap_or_else(|| run.command.clone());
    let failure_signature = semantic_verification_failure_signature(&run)
        .or_else(|| tool_result_result_hash(metadata))
        .unwrap_or_else(|| crate::harness::artifact::hash_bytes(run.output_summary.as_bytes()));
    Some(format!(
        "same_verification_failure|command={command_key}|status={:?}|exit={:?}|timeout={}|failure_signature={failure_signature}",
        run.status, run.exit_code, run.timed_out
    ))
}

fn semantic_verification_failure_signature(run: &VerificationRunResult) -> Option<String> {
    let cluster = run.failure_cluster.as_ref()?;
    let mut parts = Vec::new();
    parts.push(format!(
        "labels={}",
        sorted_join(cluster.failing_labels.iter().map(String::as_str))
    ));
    parts.push(format!(
        "source_refs={}",
        sorted_join(cluster.source_refs.iter().map(String::as_str))
    ));
    parts.push(format!(
        "test_refs={}",
        sorted_join(cluster.test_refs.iter().map(String::as_str))
    ));
    parts.push(format!(
        "sibling_obligations={}",
        sorted_join(cluster.sibling_obligations.iter().map(String::as_str))
    ));
    let mut evidence_parts = cluster
        .evidence
        .iter()
        .map(|evidence| {
            let fields = [
                format!("kind={}", evidence.evidence_kind),
                format!("subtype={}", evidence.subtype.as_deref().unwrap_or("")),
                format!("label={}", evidence.label.as_deref().unwrap_or("")),
                format!("target={}", evidence.target.as_deref().unwrap_or("")),
                format!("symbol={}", evidence.symbol.as_deref().unwrap_or("")),
                format!("call_site={}", evidence.call_site.as_deref().unwrap_or("")),
                format!("exception={}", evidence.exception.as_deref().unwrap_or("")),
                format!("expected={}", evidence.expected.as_deref().unwrap_or("")),
                format!(
                    "public_state_assertions={}",
                    sorted_join(evidence.public_state_assertions.iter().map(String::as_str))
                ),
                format!(
                    "public_missing_attributes={}",
                    sorted_join(
                        evidence
                            .public_missing_attributes
                            .iter()
                            .map(String::as_str)
                    )
                ),
                format!(
                    "markers={}",
                    sorted_join(evidence.evidence_markers.iter().map(String::as_str))
                ),
                format!(
                    "sibling_obligations={}",
                    sorted_join(evidence.sibling_obligations.iter().map(String::as_str))
                ),
                format!(
                    "requirement_refs={}",
                    sorted_join(evidence.requirement_refs.iter().map(String::as_str))
                ),
                format!(
                    "source_refs={}",
                    sorted_join(evidence.source_refs.iter().map(String::as_str))
                ),
                format!(
                    "test_refs={}",
                    sorted_join(evidence.test_refs.iter().map(String::as_str))
                ),
            ];
            fields.join(";")
        })
        .collect::<Vec<_>>();
    evidence_parts.sort();
    parts.push(format!("evidence={}", evidence_parts.join("||")));
    let signature_text = parts.join("|");
    if signature_text.trim_matches('|').is_empty() {
        None
    } else {
        Some(format!(
            "semantic:{}",
            crate::harness::artifact::hash_bytes(signature_text.as_bytes())
        ))
    }
}

fn sorted_join<'a>(values: impl Iterator<Item = &'a str>) -> String {
    let mut items = values
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    items.sort();
    items.dedup();
    items.join(",")
}

fn verification_run_passed(metadata: &Value) -> bool {
    verification_run_from_metadata(metadata)
        .is_some_and(|run| matches!(run.status, VerificationRunStatus::Passed))
}

fn verification_run_from_metadata(metadata: &Value) -> Option<VerificationRunResult> {
    metadata
        .get("verification_run_result")
        .and_then(|value| serde_json::from_value::<VerificationRunResult>(value.clone()).ok())
}

fn should_terminalize_same_verification_failure(failure_count: usize) -> bool {
    failure_count >= SAME_VERIFICATION_FAILURE_TERMINAL_THRESHOLD
}

fn same_verification_failure_terminal_message(failure_count: usize) -> String {
    format!(
        "The same verification failure evidence repeated {failure_count} time(s). Runtime stopped before continuing an unbounded repair/rerun loop. Inspect the latest stdout/stderr and make a materially different repair before rerunning verification."
    )
}

fn progress_projection_no_progress_key(
    effective_tool_name: &str,
    state: &SessionStateSnapshot,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
    result_hash: Option<&str>,
) -> String {
    let active_targets = state
        .active_targets
        .iter()
        .map(|target| target.as_str().to_ascii_lowercase())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    let required_commands = state
        .verification
        .required_commands
        .iter()
        .map(|command| command.trim().to_ascii_lowercase())
        .filter(|command| !command.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{}|result_hash={}|route={:?}|phase={:?}|open={}|verification_pending={}|route_contract_pending={}|targets={}|commands={}|allowed={}|choice={}",
        effective_tool_name,
        result_hash.unwrap_or(""),
        state.route,
        state.process_phase,
        state.completion.open_work_count,
        state.completion.verification_pending,
        state.completion.route_contract_pending,
        active_targets,
        required_commands,
        allowed_tools.iter().cloned().collect::<Vec<_>>().join(","),
        tool_choice_label(tool_choice),
    )
}

fn tool_result_result_hash(metadata: &Value) -> Option<String> {
    metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("result_hash"))
        .or_else(|| metadata.get("result_hash"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn normalized_target_keys(target: &str, workspace_root: &Utf8Path) -> Vec<String> {
    let normalized = normalize_target_key(target);
    if let Some(relative) =
        crate::workspace::project::workspace_relative_key_for_match(target, workspace_root.as_str())
    {
        vec![normalized, relative]
    } else {
        vec![normalized]
    }
}

fn normalize_target_key(target: &str) -> String {
    crate::workspace::project::path_key_for_workspace_match(target)
}

fn repair_admission_target_is_test_like(target: &str) -> bool {
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    let file_name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    file_name.starts_with("test_")
        || file_name.ends_with("_test.py")
        || file_name.ends_with(".test.ts")
        || file_name.ends_with(".spec.ts")
        || file_name.ends_with(".test.js")
        || file_name.ends_with(".spec.js")
        || normalized.contains("/tests/")
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
    let route = ToolLifecycleRuntime::route_adjudicated_call(ToolRouteRequest {
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
        sandbox_decision: json!({
            "profile": "workspace_write",
            "network_allowed": false,
            "escalated": false
        }),
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

pub(crate) fn empty_file_change_is_not_authoring_progress_fixture_passes() -> bool {
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "write".to_string(),
    ]);
    let route = ToolLifecycleRuntime::route_adjudicated_call(ToolRouteRequest {
        requested_tool: "shell".to_string(),
        effective_tool: "shell".to_string(),
        record_tool: "shell".to_string(),
        original_arguments_json:
            r#"{"command":"New-Item -Path test_component.py -ItemType File -Force"}"#.to_string(),
        effective_arguments_json:
            r#"{"command":"New-Item -Path test_component.py -ItemType File -Force"}"#.to_string(),
        allowed_tool_names: &allowed,
        tool_exists: true,
        tool_allowed: true,
        redirected_from_arguments_json: None,
        redirect_reason: None,
        tool_choice: Some("auto"),
        control_projection: Some(json!({
            "operation_intents": ["content_changing_authoring_required"],
            "allowed_tools": ["apply_patch", "shell", "write"]
        })),
        sandbox_decision: json!({
            "profile": "workspace_write",
            "network_allowed": false,
            "escalated": false
        }),
    });
    let change_id = crate::session::ChangeId::new();
    let result = ToolResult {
        title: "Create empty file".to_string(),
        output_text: "Length 0 test_component.py".to_string(),
        metadata: json!({
            "success": true,
            "changed_files": [change_id],
            "file_change_content_evidence": {
                "kind": "file_change_content_evidence",
                "content_bearing": false,
                "all_changes_content_bearing": false,
                "content_bearing_change_ids": [],
                "non_satisfying_change_ids": [change_id.to_string()],
                "content_bearing_paths": [],
                "non_satisfying_paths": ["test_component.py"]
            }
        }),
        truncated_output_path: None,
        recorded_changes: vec![change_id],
        change_summaries: vec![crate::edit::ChangeSummary {
            change_id,
            kind: crate::session::ChangeKind::Add,
            path_before: None,
            path_after: Some(Utf8PathBuf::from("test_component.py")),
        }],
    };
    let active_targets = vec![Utf8PathBuf::from("test_component.py")];
    let metadata = with_active_targets_for_operation_feedback(
        classify_executed_result_for_operation_intent(ToolName::Shell, &result, &route),
        &active_targets,
    );
    let provider_output =
        render_provider_visible_operation_progress_feedback(&result.output_text, &metadata);

    metadata
        .get("operation_progress_class")
        .and_then(Value::as_str)
        == Some("empty_artifact_no_progress")
        && metadata.get("progress_effect").and_then(Value::as_str) == Some("no_progress")
        && metadata
            .pointer("/tool_feedback_envelope/side_effects_applied")
            .and_then(Value::as_bool)
            == Some(true)
        && content_satisfying_change_summaries_for_protocol(&result, &metadata).is_empty()
        && provider_output.contains("empty_artifact_no_progress")
        && provider_output.contains("no content-bearing after-state")
        && provider_output.contains("active_targets: test_component.py")
        && matches!(
            tool_progress_effect_from_metadata(&metadata),
            ToolProgressEffect::NoProgress
        )
}

pub(crate) fn executed_tool_failure_metadata_fixture_passes() -> bool {
    let allowed = BTreeSet::from(["read".to_string()]);
    let route = ToolLifecycleRuntime::route_adjudicated_call(ToolRouteRequest {
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
        sandbox_decision: json!({
            "profile": "workspace_write",
            "network_allowed": false,
            "escalated": false
        }),
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
            .and_then(|value| value.get("error_class"))
            .and_then(Value::as_str)
            == Some("io_not_found")
}

pub(crate) fn rejected_tool_semantic_terminal_guard_fixture_passes() -> bool {
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "shell".to_string(),
    ]);
    let first_key = rejected_tool_no_progress_key(
        "write",
        r#"{"path":"arcade_game.py","content":"source v1"}"#,
        &allowed,
        &ToolChoice::Auto,
        None,
    );
    let second_key = rejected_tool_no_progress_key(
        "write",
        r#"{"path":"arcade_game.py","content":"source v2 with a different payload"}"#,
        &allowed,
        &ToolChoice::Auto,
        None,
    );
    let different_tool_key = rejected_tool_no_progress_key(
        "todowrite",
        r#"{"todos":[{"content":"plan"}]}"#,
        &allowed,
        &ToolChoice::Auto,
        None,
    );
    let required_write = fixture_required_edit_action(ToolName::Write, "test_widget.py");
    let exact_action_key = rejected_tool_no_progress_key(
        "inspect_directory",
        r#"{"path":"."}"#,
        &BTreeSet::from(["apply_patch".to_string(), "write".to_string()]),
        &ToolChoice::Named(ToolName::Write),
        Some(&required_write),
    );
    let exact_action_message = rejected_tool_no_progress_terminal_message(
        "inspect_directory",
        3,
        &BTreeSet::from(["apply_patch".to_string(), "write".to_string()]),
        Some(&required_write),
    );
    let mut counts = BTreeMap::new();
    let first_decision = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "write",
            effective_arguments_json: r#"{"path":"arcade_game.py","content":"source v1"}"#,
            allowed_tools: &allowed,
            tool_choice: &ToolChoice::Auto,
            required_action: None,
            provider_noncompliance: false,
            semantic_class: "tool_outside_allowed_surface",
            result_hash: None,
            recovery_no_progress_key: None,
        },
    );
    let second_decision = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "write",
            effective_arguments_json: r#"{"path":"arcade_game.py","content":"source v2"}"#,
            allowed_tools: &allowed,
            tool_choice: &ToolChoice::Auto,
            required_action: None,
            provider_noncompliance: false,
            semantic_class: "tool_outside_allowed_surface",
            result_hash: None,
            recovery_no_progress_key: None,
        },
    );
    let third_decision = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "write",
            effective_arguments_json: r#"{"path":"arcade_game.py","content":"source v3"}"#,
            allowed_tools: &allowed,
            tool_choice: &ToolChoice::Auto,
            required_action: None,
            provider_noncompliance: false,
            semantic_class: "tool_outside_allowed_surface",
            result_hash: None,
            recovery_no_progress_key: None,
        },
    );
    let mut provider_counts = BTreeMap::new();
    let provider_decision_a = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut provider_counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "shell",
            effective_arguments_json: r#"{"command":"python -m unittest"}"#,
            allowed_tools: &BTreeSet::from(["apply_patch".to_string(), "write".to_string()]),
            tool_choice: &ToolChoice::Required,
            required_action: Some(&required_write),
            provider_noncompliance: true,
            semantic_class: "provider_ignored_edit_only_surface",
            result_hash: Some("stable-adjudication-hash"),
            recovery_no_progress_key: None,
        },
    );
    let provider_decision_b = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut provider_counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "shell",
            effective_arguments_json: r#"{"command":"python -m unittest"}"#,
            allowed_tools: &BTreeSet::from(["apply_patch".to_string(), "write".to_string()]),
            tool_choice: &ToolChoice::Required,
            required_action: Some(&required_write),
            provider_noncompliance: true,
            semantic_class: "provider_ignored_edit_only_surface",
            result_hash: Some("stable-adjudication-hash"),
            recovery_no_progress_key: None,
        },
    );
    let provider_decision_c = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut provider_counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "shell",
            effective_arguments_json: r#"{"command":"python -m unittest"}"#,
            allowed_tools: &BTreeSet::from(["apply_patch".to_string(), "write".to_string()]),
            tool_choice: &ToolChoice::Required,
            required_action: Some(&required_write),
            provider_noncompliance: true,
            semantic_class: "provider_ignored_edit_only_surface",
            result_hash: Some("stable-adjudication-hash"),
            recovery_no_progress_key: None,
        },
    );
    let malformed_key_a = ToolLifecycleRuntime::rejected_tool_no_progress_guard_key(
        &RejectedToolNoProgressGuardRequest {
            effective_tool_name: "todowrite",
            effective_arguments_json: r#"{"todos":[{"content":"write source","status":"in_progress"}])}"#,
            allowed_tools: &BTreeSet::from([
                "apply_patch".to_string(),
                "shell".to_string(),
                "todowrite".to_string(),
            ]),
            tool_choice: &ToolChoice::Auto,
            required_action: None,
            provider_noncompliance: true,
            semantic_class: "malformed_tool_arguments",
            result_hash: Some("malformed-progress-result-hash-a"),
            recovery_no_progress_key: None,
        },
    );
    let malformed_key_b = ToolLifecycleRuntime::rejected_tool_no_progress_guard_key(
        &RejectedToolNoProgressGuardRequest {
            effective_tool_name: "todowrite",
            effective_arguments_json: r#"{"todos":[{"content":"write tests","status":"pending"}]]}"#,
            allowed_tools: &BTreeSet::from([
                "apply_patch".to_string(),
                "shell".to_string(),
                "todowrite".to_string(),
            ]),
            tool_choice: &ToolChoice::Auto,
            required_action: None,
            provider_noncompliance: true,
            semantic_class: "malformed_tool_arguments",
            result_hash: Some("malformed-progress-result-hash-b"),
            recovery_no_progress_key: None,
        },
    );
    let mut malformed_counts = BTreeMap::new();
    let malformed_decision_a = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut malformed_counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "todowrite",
            effective_arguments_json: r#"{"todos":[{"content":"write source","status":"in_progress"}])}"#,
            allowed_tools: &BTreeSet::from([
                "apply_patch".to_string(),
                "shell".to_string(),
                "todowrite".to_string(),
            ]),
            tool_choice: &ToolChoice::Auto,
            required_action: None,
            provider_noncompliance: true,
            semantic_class: "malformed_tool_arguments",
            result_hash: Some("malformed-progress-result-hash-a"),
            recovery_no_progress_key: None,
        },
    );
    let malformed_decision_b = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut malformed_counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "todowrite",
            effective_arguments_json: r#"{"todos":[{"content":"write tests","status":"pending"}]]}"#,
            allowed_tools: &BTreeSet::from([
                "apply_patch".to_string(),
                "shell".to_string(),
                "todowrite".to_string(),
            ]),
            tool_choice: &ToolChoice::Auto,
            required_action: None,
            provider_noncompliance: true,
            semantic_class: "malformed_tool_arguments",
            result_hash: Some("malformed-progress-result-hash-b"),
            recovery_no_progress_key: None,
        },
    );
    let malformed_decision_c = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut malformed_counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "todowrite",
            effective_arguments_json: r#"{"todos":[{"content":"apply patch","status":"pending"}],}"#,
            allowed_tools: &BTreeSet::from([
                "apply_patch".to_string(),
                "shell".to_string(),
                "todowrite".to_string(),
            ]),
            tool_choice: &ToolChoice::Auto,
            required_action: None,
            provider_noncompliance: true,
            semantic_class: "malformed_tool_arguments",
            result_hash: Some("malformed-progress-result-hash-c"),
            recovery_no_progress_key: None,
        },
    );
    let mut recovery_counts = BTreeMap::new();
    let recovery_key = "invalid_edit_recovery|tool=apply_patch|parser_family=apply_patch_malformed_patch|candidate_target=calculator.py|targets=calculator.py,test_calculator.py|submitted=calculator.py,test_calculator.py|active_submitted=calculator.py,test_calculator.py|inactive_submitted=";
    let recovery_decision_a = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut recovery_counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "final_assistant_message",
            effective_arguments_json: "{}",
            allowed_tools: &BTreeSet::from(["apply_patch".to_string(), "write".to_string()]),
            tool_choice: &ToolChoice::Required,
            required_action: None,
            provider_noncompliance: true,
            semantic_class: "text_final_while_obligations_open",
            result_hash: Some("final-a"),
            recovery_no_progress_key: Some(recovery_key),
        },
    );
    let recovery_decision_b = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut recovery_counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "shell",
            effective_arguments_json: r#"{"command":"Get-ChildItem"}"#,
            allowed_tools: &BTreeSet::from(["write".to_string()]),
            tool_choice: &ToolChoice::Required,
            required_action: None,
            provider_noncompliance: true,
            semantic_class: "provider_ignored_edit_only_surface",
            result_hash: Some("shell-b"),
            recovery_no_progress_key: Some(recovery_key),
        },
    );
    let recovery_decision_c = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut recovery_counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "final_assistant_message",
            effective_arguments_json: "{}",
            allowed_tools: &BTreeSet::from(["write".to_string()]),
            tool_choice: &ToolChoice::Required,
            required_action: None,
            provider_noncompliance: true,
            semantic_class: "text_final_while_obligations_open",
            result_hash: Some("final-c"),
            recovery_no_progress_key: Some(recovery_key),
        },
    );

    first_key == second_key
        && first_key != different_tool_key
        && first_key.contains("rejected_tool|tool=write")
        && exact_action_key.contains("required_action=write:test_widget.py")
        && first_decision.count == 1
        && first_decision.terminal_message.is_none()
        && second_decision.count == 2
        && second_decision.terminal_message.is_none()
        && third_decision.count == 3
        && third_decision
            .terminal_message
            .as_deref()
            .is_some_and(|message| message.contains("Allowed tools for this turn"))
        && exact_action_message.contains("Required action: write:test_widget.py")
        && provider_decision_a.terminal_message.is_none()
        && provider_decision_b.terminal_message.is_none()
        && provider_decision_c
            .terminal_message
            .as_deref()
            .is_some_and(|message| message.contains("provider_ignored_edit_only_surface"))
        && malformed_key_a == malformed_key_b
        && malformed_key_a.contains("model_action_rejection|semantic=malformed_tool_arguments")
        && malformed_key_a.contains("tool=todowrite")
        && !malformed_key_a.contains("malformed-progress-result-hash")
        && malformed_decision_a.terminal_message.is_none()
        && malformed_decision_b.terminal_message.is_none()
        && malformed_decision_c
            .terminal_message
            .as_deref()
            .is_some_and(|message| message.contains("malformed_tool_arguments"))
        && recovery_decision_a.count == 1
        && recovery_decision_b.count == 2
        && recovery_decision_c.count == 3
        && recovery_decision_c
            .terminal_message
            .as_deref()
            .is_some_and(|message| message.contains("text_final_while_obligations_open"))
}

pub(crate) fn docs_spec_semantic_reconciliation_no_progress_terminal_guard_fixture_passes() -> bool
{
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Docs;
    state.process_phase = crate::session::ProcessPhase::Author;
    state.completion.route_contract_pending = true;
    state.completion.open_work_count = 1;
    state.active_targets = vec![Utf8PathBuf::from("docs/calculator-design.md")];
    let active_work = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: state.active_targets.clone(),
        verification_commands: Vec::new(),
    };
    let authority = "Docs only. Unknown two-token `python calculator.py log 10` must be a usage error with exit code 1; do not document it as an undefined function exit code 2.";
    let first_result =
        crate::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_result(
            "write",
            &json!({
                "path": "docs/calculator-design.md",
                "content": "Unknown two-token `python calculator.py log 10` is an undefined function and exits with code 2."
            }),
            &state,
            Some(&active_work),
            Utf8Path::new("C:/workspace"),
            Some(authority),
        );
    let second_result =
        crate::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_result(
            "write",
            &json!({
                "path": "docs/calculator-design.md",
                "content": "The CLI may treat `python calculator.py log 10` as undefined function exit code 2."
            }),
            &state,
            Some(&active_work),
            Utf8Path::new("C:/workspace"),
            Some(authority),
        );
    let (Some(first_result), Some(second_result)) = (first_result, second_result) else {
        return false;
    };
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "write".to_string(),
    ]);
    let first_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "write",
        &first_result.metadata,
        &state,
        &allowed,
        &ToolChoice::Required,
    );
    let mut altered_metadata = second_result.metadata.clone();
    if let Some(object) = altered_metadata.as_object_mut() {
        object.insert(
            "result_hash".to_string(),
            Value::String("payload-dependent-hash-that-must-not-leak".to_string()),
        );
    }
    if let Some(envelope) = altered_metadata
        .get_mut("tool_feedback_envelope")
        .and_then(Value::as_object_mut)
    {
        envelope.insert(
            "result_hash".to_string(),
            Value::String("payload-dependent-hash-that-must-not-leak".to_string()),
        );
    }
    let second_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "write",
        &altered_metadata,
        &state,
        &allowed,
        &ToolChoice::Required,
    );
    let mut counts = BTreeMap::new();
    let first_decision = ToolLifecycleRuntime::record_docs_spec_semantic_reconciliation_no_progress(
        &mut counts,
        &first_result,
    );
    let second_decision =
        ToolLifecycleRuntime::record_docs_spec_semantic_reconciliation_no_progress(
            &mut counts,
            &second_result,
        );

    ToolLifecycleRuntime::operation_non_content_no_progress_under_open_authoring(
        &first_result.metadata,
        &state,
    ) && first_key == second_key
        && !first_key.contains("payload-dependent-hash")
        && first_decision.terminal_message.is_none()
        && second_decision
            .terminal_message
            .as_deref()
            .is_some_and(|message| {
                message.contains("Docs/spec semantic reconciliation rejected")
                    && message.contains("docs/calculator-design.md")
            })
        && first_result
            .metadata
            .pointer("/terminal_guard_policy/terminal_after_repeated_corrections")
            .and_then(Value::as_u64)
            == Some(
                crate::agent::docs_semantic_contract::DOCS_SPEC_SEMANTIC_RECONCILIATION_TERMINAL_THRESHOLD
                    as u64,
            )
}

pub(crate) fn pre_execution_corrective_order_authority_fixture_passes() -> bool {
    let workspace_root = Utf8Path::new("C:/workspace/pre-execution");
    let workspace_cwd = Utf8Path::new("C:/workspace/pre-execution");
    let allowed_tools = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let history_items = Vec::new();

    let mut repair_state = SessionStateSnapshot::default();
    repair_state.process_phase = crate::session::ProcessPhase::Repair;
    repair_state.active_targets = vec![Utf8PathBuf::from("test_widget.py")];
    repair_state.completion.verification_pending = true;
    repair_state.verification.required_commands = vec!["python -m unittest".to_string()];
    repair_state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: generated-test public output overreach".to_string(),
        tool_name: Some(crate::tool::ToolName::Shell),
        targets: repair_state.active_targets.clone(),
    });
    repair_state.verification.failure_cluster = Some(VerificationFailureCluster {
        cluster_id: "fixture-pre-execution-corrective-order".to_string(),
        failing_labels: vec!["test_widget_cli".to_string()],
        primary_failure: Some("stdout assertion overreach".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_output_stream_assertion_mismatch".to_string()),
            label: Some("test_widget_cli".to_string()),
            target: Some("test_widget.py".to_string()),
            symbol: None,
            call_site: Some("self.assertIn(\"decorative\", proc.stdout)".to_string()),
            exception: None,
            expected: Some("decorative".to_string()),
            observed: Some("stdout `7`".to_string()),
            public_state_assertions: vec!["proc.returncode".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "generated_test_contract_overreach".to_string(),
                "public_output_stream_assertion_mismatch".to_string(),
            ],
            sibling_obligations: vec!["proc.returncode".to_string()],
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["test_widget.py".to_string()],
        }],
        sibling_obligations: vec!["proc.returncode".to_string()],
        source_refs: Vec::new(),
        test_refs: vec!["test_widget.py".to_string()],
    });
    let repair_active = ActiveWorkContract::Verification {
        commands: vec!["python -m unittest".to_string()],
        failing_labels: vec!["test_widget_cli".to_string()],
        repair_required: true,
        targets: vec![Utf8PathBuf::from("test_widget.py")],
    };

    let exact_repair_probe = ToolLifecycleRuntime::classify_pre_execution_corrective_result(
        PreExecutionCorrectiveInput {
            effective_tool_name: "shell",
            parsed_arguments: &json!({"command": "Get-Content -Encoding UTF8 test_widget.py"}),
            active_work: Some(&repair_active),
            state: &repair_state,
            workspace_root,
            workspace_cwd: Some(workspace_cwd),
            allowed_tools: &allowed_tools,
            history_items: &history_items,
            shell_family: crate::config::ShellFamily::PowerShell,
        },
    );
    let wrong_repair_probe = ToolLifecycleRuntime::classify_pre_execution_corrective_result(
        PreExecutionCorrectiveInput {
            effective_tool_name: "shell",
            parsed_arguments: &json!({"command": "Get-Content -Encoding UTF8 widget.py"}),
            active_work: Some(&repair_active),
            state: &repair_state,
            workspace_root,
            workspace_cwd: Some(workspace_cwd),
            allowed_tools: &allowed_tools,
            history_items: &history_items,
            shell_family: crate::config::ShellFamily::PowerShell,
        },
    );

    let mut verify_state = SessionStateSnapshot::default();
    verify_state.verification.required_commands = vec!["python -m unittest".to_string()];
    let verify_active = ActiveWorkContract::Verification {
        commands: vec!["python -m unittest".to_string()],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: Vec::new(),
    };
    let wrong_verification = ToolLifecycleRuntime::classify_pre_execution_corrective_result(
        PreExecutionCorrectiveInput {
            effective_tool_name: "shell",
            parsed_arguments: &json!({"command": "python -m py_compile widget.py"}),
            active_work: Some(&verify_active),
            state: &verify_state,
            workspace_root,
            workspace_cwd: Some(workspace_cwd),
            allowed_tools: &allowed_tools,
            history_items: &history_items,
            shell_family: crate::config::ShellFamily::PowerShell,
        },
    );

    let authoring_active = ActiveWorkContract::RequestedWorkAuthoring {
        pending_targets: vec![Utf8PathBuf::from("test_widget.py")],
        verification_commands: vec!["python -m unittest".to_string()],
    };
    let wrong_authoring = ToolLifecycleRuntime::classify_pre_execution_corrective_result(
        PreExecutionCorrectiveInput {
            effective_tool_name: "write",
            parsed_arguments: &json!({"path": "widget.py", "content": "def render():\n    return 'ok'\n"}),
            active_work: Some(&authoring_active),
            state: &SessionStateSnapshot::default(),
            workspace_root,
            workspace_cwd: Some(workspace_cwd),
            allowed_tools: &allowed_tools,
            history_items: &history_items,
            shell_family: crate::config::ShellFamily::PowerShell,
        },
    );

    exact_repair_probe.is_none()
        && wrong_repair_probe.as_ref().is_some_and(|decision| {
            decision.kind == PreExecutionCorrectiveKind::RepairActiveShellProbeTarget
                && decision
                    .result
                    .metadata
                    .pointer("/tool_feedback_envelope/kind")
                    .and_then(Value::as_str)
                    == Some("repair_shell_probe_target_mismatch")
        })
        && wrong_verification.as_ref().is_some_and(|decision| {
            decision.kind == PreExecutionCorrectiveKind::WrongVerificationShellCommand
                && decision
                    .result
                    .metadata
                    .get("operation_progress_class")
                    .and_then(Value::as_str)
                    == Some("wrong_verification_command")
        })
        && wrong_authoring.as_ref().is_some_and(|decision| {
            decision.kind == PreExecutionCorrectiveKind::WrongAuthoringTarget
                && decision
                    .result
                    .metadata
                    .pointer("/tool_feedback_envelope/kind")
                    .and_then(Value::as_str)
                    == Some("wrong_authoring_target")
        })
}

struct LifecycleConfirmationPrompt<'a> {
    inner: &'a mut dyn ConfirmationPrompt,
    tool_call_id: ToolCallId,
    tool_name: ToolName,
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
                tool: self.tool_name,
                summary: request.summary.clone(),
            })
            .map_err(|error| CliPromptError::Message(error.to_string()))?;
        let approved = self.inner.confirm(request)?;
        self.sink
            .emit(crate::session::RunEvent::PermissionResolved {
                tool_call_id: self.tool_call_id,
                tool: self.tool_name,
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
    if metadata
        .get("corrective_result")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
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
        satisfies_command_identities: verification_command_satisfaction_keys(&command)
            .into_iter()
            .collect(),
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
    metadata.get("exit_code").and_then(Value::as_i64).is_some()
        || metadata.get("timeout").and_then(Value::as_bool).is_some()
        || metadata.get("tool_result_metadata").is_some_and(|value| {
            value.get("exit_code").and_then(Value::as_i64).is_some()
                || value.get("timeout").and_then(Value::as_bool).is_some()
        })
}

pub(crate) fn synthetic_corrective_shell_feedback_is_not_verification_run_fixture_passes() -> bool {
    let synthetic = with_verification_run_result(
        ToolName::Shell,
        "The requested shell command is not the current executable action. Preserve the existing verification failure and follow the typed Required action.",
        serde_json::json!({
            "progress_effect": "no_progress",
            "corrective_result": true,
            "exit_code": null,
            "timeout": false,
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
    let executed_json_only = with_verification_run_result(
        ToolName::Shell,
        "FAILED (failures=1)",
        serde_json::json!({
            "exit_code": 1,
            "timeout": false,
            "tool_route": {
                "effective_arguments_json": "{\"command\":\"python -X utf8 -m unittest\"}"
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
        && executed_json_only
            .get("verification_run_result")
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
            == Some("python -X utf8 -m unittest")
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
            "target": "component.py"
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
    if let Some(command) = metadata
        .get("tool_route")
        .and_then(|route| route.get("effective_arguments"))
        .and_then(|args| args.get("command"))
        .and_then(Value::as_str)
    {
        return Some(command.to_string());
    }
    if let Some(command) = metadata
        .get("tool_route")
        .and_then(|route| route.get("effective_arguments_json"))
        .and_then(Value::as_str)
        .and_then(command_from_arguments_json)
    {
        return Some(command);
    }
    if let Some(command) = metadata
        .get("tool_route")
        .and_then(|route| route.get("original_arguments"))
        .and_then(|args| args.get("command"))
        .and_then(Value::as_str)
    {
        return Some(command.to_string());
    }
    metadata
        .get("tool_route")
        .and_then(|route| route.get("original_arguments_json"))
        .and_then(Value::as_str)
        .and_then(command_from_arguments_json)
}

fn command_from_arguments_json(arguments_json: &str) -> Option<String> {
    serde_json::from_str::<Value>(arguments_json)
        .ok()
        .and_then(|value| {
            value
                .get("command")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
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
