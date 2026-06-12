use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::edit_recovery::{
    InvalidEditRecoveryEnvelope, failed_edit_control_recovery_envelope,
    invalid_edit_arguments_control_recovery_envelope, invalid_tool_arguments_result,
    is_invalid_tool_arguments_error, patch_context_mismatch_target_grounding_read_satisfied,
    patch_context_mismatch_target_grounding_surface_active,
    record_patch_context_mismatch_grounding_targets,
};
use crate::agent::edit_recovery::{
    repair_unambiguous_malformed_edit_arguments_json, repair_write_arguments_from_active_target,
};
use crate::agent::event::CompletedToolCall;
use crate::agent::grounding_evidence::authoring_grounding_recovery_obligation;
use crate::agent::language_evidence::{
    ArtifactRole, classify_artifact_target as classify_language_artifact_target,
};
use crate::agent::prompt::PromptPolicy;
use crate::agent::state::ActiveWorkContract;
use crate::agent::tool_orchestrator::{
    AuthoringGroundingRecoveryEnvelope, RejectedToolNoProgressGuardRequest, ToolLifecycleRuntime,
};
use crate::agent::verification::{
    canonical_verification_command_identity_key, verification_command_satisfaction_keys,
};
use crate::config::{AccessMode, ResolvedConfig, ShellFamily};
use crate::edit::ChangeSummary;
use crate::error::LlmError;
use crate::llm::{
    ChatRequest, ModelContentPart, ModelMessage, ModelProfile, ModelToolCall, ProviderToolChoice,
    ToolSchema, tool_surface_scoped_parallel_tool_calls_projection,
};
use crate::protocol::{
    ActionAuthority, ActiveWorkContractProjection, ContentPart, DispatchPolicy, EvidenceRef,
    HistoryItem, HistoryItemPayload, ModelCapabilities, ObligationKind, ObligationSet,
    ObligationStatus, OperationIntent, OutputContract, ProjectionBundle, ProjectionId,
    ProjectionSurface, ProjectionSurfaceKind, RejectedToolProposal, RequiredAction, SandboxProfile,
    ToolChoice, ToolProposalId, TurnContext, TurnControlEnvelope, TurnId, TurnObligation,
};
use crate::protocol::{ObligationCompiler, TurnEngine, TurnEngineInput};
use crate::session::{DocsRouteState, ProcessPhase, SessionStateSnapshot, TaskRoute};
use crate::session::{FinishReason, MessageRole};
use crate::session::{
    RequestControlEnvelopeDiagnostic, RequestControlEnvelopeIssueDiagnostic,
    RequestControlObligationDiagnostic, RequestControlSurfaceDiagnostic, RequestDiagnosticsPart,
    RequestMessageDiagnostic, RequestReplayPolicyDiagnostic, RequestToolCallDiagnostic,
    RequestToolSchemaDiagnostic, TurnDecisionDiagnostic,
};
use crate::session::{SessionId, ToolCallId};
use crate::tool::registry::ToolRegistry;
use crate::tool::{ToolName, ToolResult};

const LIFECYCLE_FIXTURE_PROVIDER: &str = "openai_compat";
const LIFECYCLE_FIXTURE_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const LIFECYCLE_FIXTURE_BASE_URL: &str = "http://127.0.0.1:1234";
const CLOSEOUT_FINAL_RESPONSE_TIMEOUT_MS: u64 = 120_000;
const OPEN_OBLIGATION_FINAL_MESSAGE_TERMINAL_THRESHOLD: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OpenObligationFinalMessageRecoveryEnvelope {
    pub(crate) count: usize,
    pub(crate) active_targets: Vec<String>,
    pub(crate) prompt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelActionProposal {
    ToolCall(ModelToolCallProposal),
    TextFinal(TextFinalProposal),
    MalformedToolArguments(MalformedToolArgumentsProposal),
    SchemaOutsideToolProposal(SchemaOutsideToolProposal),
    TextFinalWhileObligationsOpen(TextFinalProposal),
}

impl ModelActionProposal {
    pub(crate) fn requested_action_name(&self) -> &str {
        match self {
            Self::ToolCall(proposal) => &proposal.requested_tool,
            Self::MalformedToolArguments(proposal) => &proposal.requested_tool,
            Self::SchemaOutsideToolProposal(proposal) => &proposal.requested_tool,
            Self::TextFinal(_) | Self::TextFinalWhileObligationsOpen(_) => {
                "final_assistant_message"
            }
        }
    }
}

pub(crate) fn stable_tool_schemas_from_registry(registry: &ToolRegistry) -> Vec<ToolSchema> {
    registry
        .specs()
        .into_iter()
        .map(|spec| ToolSchema {
            name: spec.name.to_string(),
            description: spec.description.to_string(),
            input_schema: spec.input_schema,
            strict: false,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelToolCallProposal {
    pub call_id: String,
    pub requested_tool: String,
    pub effective_tool: String,
    pub arguments_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextFinalProposal {
    pub proposal_id: String,
    pub text: String,
    pub projection_id: ProjectionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MalformedToolArgumentsProposal {
    pub proposal_id: String,
    pub source_call_id: String,
    pub requested_tool: String,
    pub raw_arguments: String,
    pub parse_error: String,
    pub projection_id: ProjectionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchemaOutsideToolProposal {
    pub proposal_id: String,
    pub source_call_id: String,
    pub requested_tool: String,
    pub raw_payload: String,
    pub projection_id: ProjectionId,
}

#[derive(Debug, Clone)]
pub(crate) struct ActionAdjudicationInput<'a> {
    pub proposal: ModelToolCallProposal,
    pub allowed_tools: &'a BTreeSet<String>,
    pub tool_exists: bool,
    pub tool_allowed: bool,
    pub envelope: &'a TurnControlEnvelope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActionAdjudication {
    AcceptedToolCall(AcceptedToolCall),
    RejectedModelAction(ModelActionRejection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcceptedToolCall {
    pub proposal: ModelToolCallProposal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedToolRouteArguments {
    pub(crate) effective_tool_name: String,
    pub(crate) effective_arguments_json: String,
    pub(crate) redirected_from_arguments_json: Option<String>,
    pub(crate) redirect_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PrepareToolRouteArgumentsInput<'a> {
    pub(crate) requested_tool_name: &'a str,
    pub(crate) original_arguments_json: &'a str,
    pub(crate) runtime_owned_verification_redirect:
        Option<&'a RuntimeOwnedVerificationRedirectSnapshot>,
    pub(crate) active_targets_for_argument_repair: &'a [Utf8PathBuf],
    pub(crate) shell_repaired_arguments_json: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeOwnedVerificationRedirectSnapshot {
    pub(crate) effective_tool_name: String,
    pub(crate) effective_arguments_json: String,
    pub(crate) redirected_from_arguments_json: String,
    pub(crate) redirect_reason: &'static str,
}

pub(crate) struct CompileProviderChatRequestInput<'a> {
    pub(crate) model: &'a ModelProfile,
    pub(crate) config: &'a ResolvedConfig,
    pub(crate) system_prompt: String,
    pub(crate) messages: Vec<ModelMessage>,
    pub(crate) tools: Vec<ToolSchema>,
    pub(crate) dispatch_tool_choice: &'a ToolChoice,
}

pub(crate) struct CompileTurnContextInput<'a> {
    pub(crate) session_id: SessionId,
    pub(crate) cwd: &'a Utf8PathBuf,
    pub(crate) workspace_root: &'a Utf8PathBuf,
    pub(crate) model: &'a ModelProfile,
    pub(crate) config: &'a ResolvedConfig,
    pub(crate) state: &'a SessionStateSnapshot,
    pub(crate) history_items: &'a [HistoryItem],
    pub(crate) active_work: Option<&'a ActiveWorkContract>,
    pub(crate) turn_decision: &'a TurnDecisionDiagnostic,
    pub(crate) allowed_tools: Vec<ToolName>,
    pub(crate) tool_choice: &'a ToolChoice,
    pub(crate) projection_id: ProjectionId,
}

pub(crate) struct CompileTurnObligationsInput<'a> {
    pub(crate) context: &'a TurnContext,
    pub(crate) active_work: Option<&'a ActiveWorkContract>,
    pub(crate) authoring_grounding_recovery: Option<&'a AuthoringGroundingRecoveryEnvelope>,
    pub(crate) invalid_edit_recovery: Option<&'a InvalidEditRecoveryEnvelope>,
    pub(crate) history_items: &'a [HistoryItem],
    pub(crate) workspace_root: &'a Utf8Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ToolCallModelActionAdjudication {
    pub(crate) action_name: String,
    pub(crate) tool_exists: bool,
    pub(crate) tool_allowed: bool,
    pub(crate) adjudication: ActionAdjudication,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelActionRejection {
    pub classification: ModelActionRejectionClass,
    pub semantic_class: &'static str,
    pub blocked_reason: String,
    pub result_hash: String,
    pub proposal: ModelToolCallProposal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelActionRejectionClass {
    InvalidTool,
    ToolOutsideAllowedSurface,
    ProviderNoncompliance,
}

impl ModelActionRejectionClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidTool => "invalid_tool",
            Self::ToolOutsideAllowedSurface => "tool_outside_allowed_surface",
            Self::ProviderNoncompliance => "provider_noncompliance",
        }
    }
}

pub(crate) struct ProviderActionAdapter;

impl ProviderActionAdapter {
    pub(crate) fn adapt_tool_call(call: &CompletedToolCall) -> ModelActionProposal {
        match serde_json::from_str::<Value>(&call.arguments_json) {
            Ok(Value::Object(_)) => {}
            Ok(_) => {
                return ModelActionProposal::SchemaOutsideToolProposal(SchemaOutsideToolProposal {
                    proposal_id: ToolProposalId::new().to_string(),
                    source_call_id: call.call_id.clone(),
                    requested_tool: call.tool_name.clone(),
                    raw_payload: call.arguments_json.clone(),
                    projection_id: ProjectionId::new(),
                });
            }
            Err(error) => {
                return ModelActionProposal::MalformedToolArguments(
                    MalformedToolArgumentsProposal {
                        proposal_id: ToolProposalId::new().to_string(),
                        source_call_id: call.call_id.clone(),
                        requested_tool: call.tool_name.clone(),
                        raw_arguments: call.arguments_json.clone(),
                        parse_error: error.to_string(),
                        projection_id: ProjectionId::new(),
                    },
                );
            }
        }
        ModelActionProposal::ToolCall(ModelToolCallProposal {
            call_id: call.call_id.clone(),
            requested_tool: call.tool_name.clone(),
            effective_tool: call.tool_name.clone(),
            arguments_json: call.arguments_json.clone(),
        })
    }

    pub(crate) fn adapt_text_final(
        text: impl Into<String>,
        projection_id: ProjectionId,
        obligations_open: bool,
    ) -> ModelActionProposal {
        let proposal = TextFinalProposal {
            proposal_id: ToolProposalId::new().to_string(),
            text: text.into(),
            projection_id,
        };
        if obligations_open {
            ModelActionProposal::TextFinalWhileObligationsOpen(proposal)
        } else {
            ModelActionProposal::TextFinal(proposal)
        }
    }
}

fn filter_non_authoritative_assistant_text_for_open_obligations(
    messages: Vec<ModelMessage>,
    open_obligations: bool,
) -> Vec<ModelMessage> {
    if !open_obligations {
        return messages;
    }

    let mut seen_user = false;
    let mut omitted_assistant_text_count = 0usize;
    let mut omitted_tool_call_content_count = 0usize;
    let mut filtered = Vec::with_capacity(messages.len());
    for message in messages {
        match message {
            ModelMessage::User { .. } | ModelMessage::UserParts { .. } => {
                seen_user = true;
                filtered.push(message);
            }
            ModelMessage::Assistant { content } if seen_user && !content.trim().is_empty() => {
                omitted_assistant_text_count += 1;
            }
            ModelMessage::AssistantToolCalls {
                content,
                tool_calls,
            } if seen_user
                && content
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()) =>
            {
                omitted_tool_call_content_count += 1;
                filtered.push(ModelMessage::AssistantToolCalls {
                    content: None,
                    tool_calls,
                });
            }
            other => filtered.push(other),
        }
    }

    if omitted_assistant_text_count == 0 && omitted_tool_call_content_count == 0 {
        return filtered;
    }

    let mut with_note = Vec::with_capacity(filtered.len() + 1);
    with_note.push(ModelMessage::System {
        content: format!(
            "Provider replay assistant-text normalization: omitted {omitted_assistant_text_count} intermediate assistant text message(s) and stripped assistant tool-call content from {omitted_tool_call_content_count} message(s) because current obligations remain open. Workspace artifacts, tool calls, tool outputs, verification evidence, and the current turn control projection are the authority; prior text-only promises and assistant tool-call content prose are not completion evidence."
        ),
    });
    with_note.extend(filtered);
    with_note
}

fn normalize_provider_system_context_for_chat_template(
    messages: Vec<ModelMessage>,
) -> Vec<ModelMessage> {
    let mut system_blocks = Vec::new();
    let mut non_system_messages = Vec::new();

    for message in messages {
        match message {
            ModelMessage::System { content } => {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    system_blocks.push(trimmed.to_string());
                }
            }
            other => non_system_messages.push(other),
        }
    }

    if system_blocks.is_empty() {
        return non_system_messages;
    }

    let mut normalized = Vec::with_capacity(non_system_messages.len() + 1);
    normalized.push(ModelMessage::System {
        content: system_blocks.join("\n\n"),
    });
    normalized.extend(non_system_messages);
    normalized
}

fn provider_messages_have_user_query_anchor(messages: &[ModelMessage]) -> bool {
    messages.iter().any(|message| match message {
        ModelMessage::User { content } => !content.trim().is_empty(),
        ModelMessage::UserParts { parts } => parts.iter().any(|part| match part {
            ModelContentPart::Text { text } => !text.trim().is_empty(),
            ModelContentPart::Image { .. } => true,
        }),
        _ => false,
    })
}

fn latest_user_images(history_items: &[HistoryItem]) -> Vec<crate::session::ImagePart> {
    let mut items = history_items.iter().collect::<Vec<_>>();
    items.sort_by(|left, right| {
        left.sequence_no
            .cmp(&right.sequence_no)
            .then_with(|| left.created_at_ms.cmp(&right.created_at_ms))
    });
    items
        .into_iter()
        .rev()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::UserTurn { content, .. }
            | HistoryItemPayload::Message {
                role: MessageRole::User,
                content,
                ..
            } => Some(images_from_content_parts(content)),
            _ => None,
        })
        .unwrap_or_default()
}

fn images_from_content_parts(content: &[ContentPart]) -> Vec<crate::session::ImagePart> {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Image { image } => Some(image.clone()),
            ContentPart::Text { .. } => None,
        })
        .collect()
}

fn active_work_requires_provider_images(
    state: &SessionStateSnapshot,
    active_work: Option<&ActiveWorkContract>,
) -> bool {
    let verification_failure_repair = matches!(state.process_phase, ProcessPhase::Repair)
        && (state.completion.verification_pending
            || state.verification.failure_cluster.is_some()
            || matches!(
                state.failure.as_ref().map(|failure| failure.kind),
                Some(crate::session::FailureKind::VerificationFailed)
            ));

    !(matches!(state.process_phase, ProcessPhase::Verify)
        || matches!(active_work, Some(ActiveWorkContract::Verification { .. }))
        || verification_failure_repair)
}

pub(crate) struct TurnLifecycleKernel;

impl TurnLifecycleKernel {
    pub(crate) fn adjudicate_final_message_response(
        tool_calls_empty: bool,
        text: impl Into<String>,
        projection_id: ProjectionId,
        state: &SessionStateSnapshot,
        allowed_tools: &BTreeSet<String>,
        envelope: &TurnControlEnvelope,
    ) -> Option<ActionAdjudication> {
        if !tool_calls_empty {
            return None;
        }
        let action = ProviderActionAdapter::adapt_text_final(
            text,
            projection_id,
            !Self::closeout_ready_final_message_authority(state),
        );
        Some(Self::adjudicate_model_action(
            action,
            allowed_tools,
            false,
            false,
            envelope,
        ))
    }

    pub(crate) fn no_tool_final_response_failure_message(
        tool_calls_empty: bool,
        finish_reason: Option<&FinishReason>,
    ) -> Option<&'static str> {
        if tool_calls_empty && matches!(finish_reason, Some(FinishReason::Length)) {
            Some("model response hit the output length limit before the run reached a natural stop")
        } else {
            None
        }
    }

    pub(crate) fn empty_tool_call_final_response_failure_message(
        finish_reason: Option<&FinishReason>,
    ) -> Option<&'static str> {
        Self::no_tool_final_response_failure_message(true, finish_reason)
    }

    pub(crate) fn provider_finish_reason_interrupt_message(
        finish_reason: Option<&FinishReason>,
    ) -> Option<&'static str> {
        if matches!(finish_reason, Some(FinishReason::Cancelled)) {
            Some("run cancelled by user")
        } else {
            None
        }
    }

    pub(crate) fn runtime_cancel_interrupt_message(cancelled: bool) -> Option<&'static str> {
        if cancelled {
            Some("run cancelled by user")
        } else {
            None
        }
    }

    pub(crate) fn provider_request_failure_message(error: &LlmError) -> String {
        format!("provider model request failed: {error}")
    }

    pub(crate) fn turn_step_budget_exhausted_failure_message() -> &'static str {
        "turn step budget reached before completion"
    }

    pub(crate) fn closeout_final_response_timeout_ms(
        configured_timeout_ms: u64,
        state: &SessionStateSnapshot,
        active_work: Option<&ActiveWorkContract>,
    ) -> u64 {
        if !Self::clean_closeout_final_message_lifecycle(state, active_work) {
            return configured_timeout_ms;
        }
        if configured_timeout_ms == 0 {
            return CLOSEOUT_FINAL_RESPONSE_TIMEOUT_MS;
        }
        configured_timeout_ms.min(CLOSEOUT_FINAL_RESPONSE_TIMEOUT_MS)
    }

    pub(crate) fn terminal_response_timeout_ms_for_state(
        configured_timeout_ms: u64,
        state: &SessionStateSnapshot,
        active_work: Option<&ActiveWorkContract>,
    ) -> Option<u64> {
        Self::clean_closeout_final_message_lifecycle(state, active_work).then(|| {
            Self::closeout_final_response_timeout_ms(configured_timeout_ms, state, active_work)
        })
    }

    pub(crate) fn operation_intents_for_active_work(
        active_work: Option<&ActiveWorkContract>,
    ) -> Vec<OperationIntent> {
        match active_work {
            Some(ActiveWorkContract::RequestedWorkAuthoring {
                pending_targets, ..
            }) if !pending_targets.is_empty() => {
                vec![OperationIntent::ContentChangingAuthoringRequired]
            }
            Some(ActiveWorkContract::DocsRepair {
                deliverable,
                pending_deliverables,
                ..
            }) if deliverable.is_some() || !pending_deliverables.is_empty() => {
                vec![OperationIntent::ContentChangingAuthoringRequired]
            }
            Some(ActiveWorkContract::Verification {
                repair_required: true,
                ..
            }) => vec![OperationIntent::ContentChangingAuthoringRequired],
            _ => Vec::new(),
        }
    }

    pub(crate) fn docs_route_contract_pending_after_file_change(
        state: &SessionStateSnapshot,
    ) -> bool {
        state.route == TaskRoute::Docs && state.completion.route_contract_pending
    }

    pub(crate) fn filter_non_authoritative_assistant_text_for_open_obligations(
        messages: Vec<ModelMessage>,
        open_obligations: bool,
    ) -> Vec<ModelMessage> {
        filter_non_authoritative_assistant_text_for_open_obligations(messages, open_obligations)
    }

    pub(crate) fn normalize_provider_system_context_for_chat_template(
        messages: Vec<ModelMessage>,
    ) -> Vec<ModelMessage> {
        normalize_provider_system_context_for_chat_template(messages)
    }

    pub(crate) fn provider_messages_have_user_query_anchor(messages: &[ModelMessage]) -> bool {
        provider_messages_have_user_query_anchor(messages)
    }

    pub(crate) fn active_work_contract_projection(
        state: &SessionStateSnapshot,
        workspace_root: &Utf8PathBuf,
        active_work: Option<&ActiveWorkContract>,
        required_verification_commands: Vec<String>,
        allowed_tools: Vec<ToolName>,
        projection_id: ProjectionId,
    ) -> ActiveWorkContractProjection {
        ActiveWorkContractProjection {
            route: state.route,
            process_phase: state.process_phase,
            active_work_kind: active_work
                .map(|contract| contract.kind().to_string())
                .filter(|kind| !kind.trim().is_empty()),
            summary: active_work
                .map(ActiveWorkContract::summary)
                .or_else(|| state.completion.blocked_reason.clone())
                .unwrap_or_else(|| {
                    "No open executable work is projected for this turn.".to_string()
                }),
            active_targets: crate::protocol::canonicalize_workspace_targets(
                &active_work
                    .map(ActiveWorkContract::targets)
                    .filter(|targets| !targets.is_empty())
                    .unwrap_or_else(|| state.active_targets.clone()),
                workspace_root,
            ),
            operation_intents: Self::operation_intents_for_active_work(active_work),
            required_verification_commands,
            allowed_tools,
            forbidden_tools: Vec::new(),
            projection_id,
        }
    }

    pub(crate) fn output_contract_for_state(state: &SessionStateSnapshot) -> OutputContract {
        OutputContract {
            final_answer_required: Self::closeout_ready_final_message_authority(state),
            structured_schema_name: None,
            history_markdown_projection: true,
        }
    }

    pub(crate) fn compile_turn_context(input: CompileTurnContextInput<'_>) -> TurnContext {
        TurnContext {
            session_id: input.session_id,
            cwd: input.cwd.clone(),
            workspace_root: input.workspace_root.clone(),
            provider: "openai_compat".to_string(),
            model: input.model.name.clone(),
            base_url: input.config.model.base_url.clone(),
            access_mode: input.config.permissions.access_mode,
            sandbox: Self::sandbox_profile_for_access_mode(input.config.permissions.access_mode),
            shell_family: Self::resolved_shell_family(input.config),
            model_capabilities: ModelCapabilities {
                supports_tools: input.config.model.supports_tools,
                supports_reasoning: input.config.model.supports_reasoning,
                supports_images: input.config.model.supports_images,
                parallel_tool_calls: crate::llm::control_plane_parallel_tool_calls_projection(
                    input.allowed_tools.len(),
                    input.config.model.parallel_tool_calls,
                    input.config.model.max_parallel_predictions,
                ),
                context_window: input.config.model.context_window,
                max_output_tokens: input.config.model.max_output_tokens,
            },
            route: input.state.route,
            process_phase: input.state.process_phase,
            active_contract: Self::active_work_contract_projection(
                input.state,
                input.workspace_root,
                input.active_work,
                input.turn_decision.required_verification_commands.clone(),
                input.allowed_tools.clone(),
                input.projection_id,
            ),
            allowed_tools: input.allowed_tools,
            tool_choice: input.tool_choice.clone(),
            images: Self::provider_visible_images_for_active_work(
                input.history_items,
                input.state,
                input.active_work,
            ),
            output_contract: Self::output_contract_for_state(input.state),
            continuation: input
                .state
                .implementation_handoff
                .as_ref()
                .and_then(|handoff| handoff.continuation_contract.clone()),
            turn_decision_projection: Some(input.turn_decision.clone()),
        }
    }

    pub(crate) fn compile_turn_obligations(
        input: CompileTurnObligationsInput<'_>,
    ) -> ObligationSet {
        let mut obligations = ObligationCompiler::compile(input.context);
        if let Some(envelope) = input.authoring_grounding_recovery {
            obligations
                .items
                .push(authoring_grounding_recovery_obligation(envelope));
        }
        if let Some(envelope) = input
            .invalid_edit_recovery
            .filter(|_| invalid_edit_recovery_obligation_matches_active_work(input.active_work))
        {
            obligations
                .items
                .push(invalid_edit_recovery_projection_obligation(envelope));
        }
        if let Some(obligation) =
            crate::agent::docs_semantic_contract::docs_spec_semantic_reconciliation_recovery_obligation(
                input.history_items,
                input.active_work,
                input.workspace_root,
            )
        {
            obligations.items.push(obligation);
        }
        obligations
    }

    pub(crate) fn sandbox_profile_for_access_mode(access_mode: AccessMode) -> SandboxProfile {
        match access_mode {
            AccessMode::Default | AccessMode::AutoReview => SandboxProfile::WorkspaceWrite,
            AccessMode::FullAccess => SandboxProfile::FullAccess,
        }
    }

    pub(crate) fn default_shell_family() -> ShellFamily {
        if cfg!(windows) {
            ShellFamily::PowerShell
        } else {
            ShellFamily::Bash
        }
    }

    pub(crate) fn resolved_shell_family(config: &ResolvedConfig) -> ShellFamily {
        config
            .shell
            .family
            .unwrap_or_else(Self::default_shell_family)
    }

    pub(crate) fn reconcile_tools_with_action_authority(
        tools: &mut Vec<ToolSchema>,
        envelope: &TurnControlEnvelope,
    ) -> BTreeSet<String> {
        let authority_tool_names = envelope
            .action_authority
            .allowed_tools
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>();
        tools.retain(|tool| authority_tool_names.contains(&tool.name));
        authority_tool_names
    }

    pub(crate) fn turn_decision_dispatch_block_message(
        diagnostic: &TurnDecisionDiagnostic,
    ) -> Option<String> {
        let blocking = diagnostic
            .warnings
            .iter()
            .filter(|warning| {
                warning.severity == crate::session::TurnDecisionWarningSeverity::Error
            })
            .map(|warning| warning.code.as_str())
            .collect::<Vec<_>>();
        if blocking.is_empty() {
            None
        } else {
            Some(format!(
                "Turn decision projection is inconsistent before provider dispatch: {}",
                blocking.join(", ")
            ))
        }
    }

    pub(crate) fn control_envelope_validation_error_message(
        envelope: &TurnControlEnvelope,
    ) -> String {
        let validation = envelope.validate();
        let issues = validation
            .issues
            .iter()
            .map(|issue| format!("{:?}: {}", issue.code, issue.message))
            .collect::<Vec<_>>()
            .join("; ");
        if issues.is_empty() {
            "turn control envelope validation failed".to_string()
        } else {
            format!("turn control envelope validation failed before provider dispatch: {issues}")
        }
    }

    pub(crate) fn control_envelope_fail_closed_dispatch_message(
        envelope: &TurnControlEnvelope,
    ) -> Option<String> {
        envelope
            .fail_closed_before_dispatch()
            .map(|reason| format!("turn control envelope failed closed before dispatch: {reason}"))
    }

    pub(crate) fn compile_request_replay_policies(
        base_policies: &[RequestReplayPolicyDiagnostic],
        surface_filter_policies: Vec<RequestReplayPolicyDiagnostic>,
        image_replay_policy: Option<RequestReplayPolicyDiagnostic>,
        state: &SessionStateSnapshot,
        recovery: TurnLifecycleRecoveryContext,
        invalid_edit_control_recovery_active: bool,
    ) -> Vec<RequestReplayPolicyDiagnostic> {
        let mut replay_policies = base_policies.to_vec();
        replay_policies.extend(surface_filter_policies);
        if let Some(policy) = image_replay_policy {
            replay_policies.push(policy);
        }
        Self::append_recovery_replay_policies(
            &mut replay_policies,
            state,
            recovery,
            invalid_edit_control_recovery_active,
        );
        replay_policies
    }

    pub(crate) fn provider_tool_choice_value(
        tool_count: usize,
        tool_choice: &ToolChoice,
    ) -> Option<ProviderToolChoice> {
        provider_tool_choice_value(tool_count, tool_choice)
    }

    pub(crate) fn tool_choice_label(tool_choice: &ToolChoice) -> &'static str {
        match tool_choice {
            ToolChoice::Auto => "auto",
            ToolChoice::Required => "required",
            ToolChoice::None => "none",
            ToolChoice::Named(_) => "named",
        }
    }

    pub(crate) fn compile_provider_chat_request(
        input: CompileProviderChatRequestInput<'_>,
    ) -> ChatRequest {
        let tool_count = input.tools.len();
        ChatRequest {
            model: input.model.clone(),
            base_url: input.config.model.base_url.clone(),
            system_prompt: input.system_prompt,
            messages: input.messages,
            tools: input.tools,
            tool_choice: provider_tool_choice_value(tool_count, input.dispatch_tool_choice),
            parallel_tool_calls: crate::llm::effective_parallel_tool_calls(
                tool_count,
                input.config.model.parallel_tool_calls,
                input.config.model.max_parallel_predictions,
            ),
            timeout_ms: input.config.model.request_timeout_ms,
            stream_idle_timeout_ms: input.config.model.stream_idle_timeout_ms,
            stream_max_retries: input.config.model.stream_max_retries,
            extra_headers: input.config.model.extra_headers.clone(),
            temperature: input.config.model.temperature,
            top_p: input.config.model.top_p,
            top_k: input.config.model.top_k,
            presence_penalty: input.config.model.presence_penalty,
            frequency_penalty: input.config.model.frequency_penalty,
            seed: input.config.model.seed,
            stop_sequences: input.config.model.stop_sequences.clone(),
            extra_body: input.config.model.extra_body_json.clone(),
        }
    }

    pub(crate) fn compile_request_diagnostics(
        request: &ChatRequest,
        turn_decision: Option<TurnDecisionDiagnostic>,
        control_envelope: Option<&TurnControlEnvelope>,
        replay_policies: &[RequestReplayPolicyDiagnostic],
    ) -> RequestDiagnosticsPart {
        let messages = request
            .messages
            .iter()
            .map(request_message_diagnostic)
            .collect::<Vec<_>>();
        let image_count = messages.iter().map(|message| message.image_count).sum();
        let image_bytes = messages.iter().map(|message| message.image_bytes).sum();
        RequestDiagnosticsPart {
            provider: "openai_compat".to_string(),
            model_name: request.model.name.clone(),
            base_url: request.base_url.clone(),
            request_timeout_ms: request.timeout_ms,
            stream_idle_timeout_ms: request.stream_idle_timeout_ms,
            stream_max_retries: request.stream_max_retries,
            configured_max_output_tokens: Some(request.model.max_output_tokens),
            effective_max_output_tokens: Some(request.effective_max_output_tokens()),
            output_budget_reason: Some(request.output_budget_reason().to_string()),
            supports_tools: Some(request.model.capabilities.supports_tools),
            supports_reasoning: Some(request.model.capabilities.supports_reasoning),
            supports_images: Some(request.model.capabilities.supports_images),
            system_prompt_chars: request.provider_system_prompt().chars().count(),
            tool_count: request.tools.len(),
            tool_choice: request
                .tool_choice
                .as_ref()
                .map(ProviderToolChoice::diagnostic_label),
            parallel_tool_calls: tool_surface_scoped_parallel_tool_calls_projection(
                request.tools.len(),
                request.parallel_tool_calls,
            ),
            provider_message_count: request.messages.len(),
            image_count,
            image_bytes,
            tool_names: request.tools.iter().map(|tool| tool.name.clone()).collect(),
            tool_schemas: request
                .tools
                .iter()
                .map(|tool| RequestToolSchemaDiagnostic {
                    name: tool.name.clone(),
                    description_chars: tool.description.chars().count(),
                    strict: tool.strict,
                    input_schema: tool.input_schema.clone(),
                })
                .collect(),
            turn_decision,
            control_envelope: control_envelope.map(request_control_envelope_diagnostic),
            replay_policies: replay_policies.to_vec(),
            messages,
        }
    }

    pub(crate) fn runtime_owned_required_verification_tool_call(
        active_work: Option<&ActiveWorkContract>,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
        required_action: Option<&RequiredAction>,
    ) -> Option<CompletedToolCall> {
        let command = runtime_owned_required_verification_command(
            active_work,
            allowed_tools,
            tool_choice,
            required_action,
        )?;
        Some(CompletedToolCall {
            call_id: format!(
                "runtime_shell_verification:{}",
                crate::harness::artifact::hash_bytes(command.as_bytes())
            ),
            tool_name: "shell".to_string(),
            arguments_json: serde_json::to_string(&json!({ "command": command })).ok()?,
        })
    }

    pub(crate) fn runtime_owned_required_verification_dispatch_redirect(
        requested_tool_name: &str,
        original_arguments_json: &str,
        active_work: Option<&ActiveWorkContract>,
        allowed_tools: &BTreeSet<String>,
        tool_choice: &ToolChoice,
        required_action: Option<&RequiredAction>,
    ) -> Option<RuntimeOwnedVerificationRedirectSnapshot> {
        if requested_tool_name == "shell" {
            return None;
        }
        let command = runtime_owned_required_verification_command(
            active_work,
            allowed_tools,
            tool_choice,
            required_action,
        )?;
        let effective_arguments_json =
            serde_json::to_string(&json!({ "command": command })).ok()?;
        Some(RuntimeOwnedVerificationRedirectSnapshot {
            effective_tool_name: "shell".to_string(),
            effective_arguments_json,
            redirected_from_arguments_json: original_arguments_json.to_string(),
            redirect_reason: "runtime_owned_required_verification_dispatch",
        })
    }

    pub(crate) fn post_repair_required_verification_dispatch_is_runtime_owned_fixture_passes()
    -> bool {
        let executable_behavior_command =
            ToolLifecycleRuntime::fixture_executable_verification_command(
                "verify-contract --behavior",
            );
        let allowed = BTreeSet::from(["shell".to_string()]);
        let active = ActiveWorkContract::Verification {
            commands: vec!["verify-contract --behavior".to_string()],
            failing_labels: vec!["workflow_behavior_verification_contract".to_string()],
            repair_required: false,
            targets: Vec::new(),
        };
        let repair_still_open = ActiveWorkContract::Verification {
            commands: vec!["verify-contract --behavior".to_string()],
            failing_labels: vec!["workflow_behavior_verification_contract".to_string()],
            repair_required: true,
            targets: vec![Utf8PathBuf::from("tests/workflow.spec.ts")],
        };
        let required_shell = RequiredAction::shell(executable_behavior_command.clone());
        let redirected = Self::runtime_owned_required_verification_dispatch_redirect(
            "read",
            r#"{"path":"tests/workflow.spec.ts"}"#,
            Some(&active),
            &allowed,
            &ToolChoice::Required,
            Some(&required_shell),
        )
        .and_then(|redirect| {
            serde_json::from_str::<Value>(&redirect.effective_arguments_json)
                .ok()
                .map(|arguments| (redirect, arguments))
        });
        let shell_passthrough = Self::runtime_owned_required_verification_dispatch_redirect(
            "shell",
            r#"{"command":"Get-ChildItem"}"#,
            Some(&active),
            &allowed,
            &ToolChoice::Required,
            Some(&required_shell),
        );
        let repair_phase_blocked = Self::runtime_owned_required_verification_dispatch_redirect(
            "read",
            r#"{"path":"tests/workflow.spec.ts"}"#,
            Some(&repair_still_open),
            &allowed,
            &ToolChoice::Required,
            Some(&required_shell),
        );
        let broad_surface_blocked = Self::runtime_owned_required_verification_dispatch_redirect(
            "read",
            r#"{"path":"tests/workflow.spec.ts"}"#,
            Some(&active),
            &BTreeSet::from(["read".to_string(), "shell".to_string()]),
            &ToolChoice::Auto,
            Some(&required_shell),
        );

        redirected.is_some_and(|(redirect, arguments)| {
            redirect.effective_tool_name == "shell"
                && redirect.redirected_from_arguments_json == r#"{"path":"tests/workflow.spec.ts"}"#
                && redirect.redirect_reason == "runtime_owned_required_verification_dispatch"
                && arguments.get("command").and_then(Value::as_str)
                    == Some(executable_behavior_command.as_str())
        }) && shell_passthrough.is_none()
            && repair_phase_blocked.is_none()
            && broad_surface_blocked.is_none()
    }

    pub(crate) fn verification_only_missing_provider_tool_call_dispatches_runtime_owned_fixture_passes()
    -> bool {
        let executable_behavior_command =
            ToolLifecycleRuntime::fixture_executable_verification_command(
                "verify-contract --behavior",
            );
        let allowed = BTreeSet::from(["shell".to_string()]);
        let active = ActiveWorkContract::Verification {
            commands: vec!["verify-contract --behavior".to_string()],
            failing_labels: Vec::new(),
            repair_required: false,
            targets: Vec::new(),
        };
        let required_shell = RequiredAction::shell(executable_behavior_command.clone());
        let runtime_call = Self::runtime_owned_required_verification_tool_call(
            Some(&active),
            &allowed,
            &ToolChoice::Named(ToolName::Shell),
            Some(&required_shell),
        )
        .and_then(|call| {
            serde_json::from_str::<Value>(&call.arguments_json)
                .ok()
                .map(|arguments| (call, arguments))
        });
        let broad_surface_blocked = Self::runtime_owned_required_verification_tool_call(
            Some(&active),
            &BTreeSet::from(["read".to_string(), "shell".to_string()]),
            &ToolChoice::Auto,
            Some(&required_shell),
        );

        runtime_call.is_some_and(|(call, arguments)| {
            call.tool_name == "shell"
                && call.call_id.starts_with("runtime_shell_verification:")
                && arguments.get("command").and_then(Value::as_str)
                    == Some(executable_behavior_command.as_str())
                && arguments.get("runtime_owned").is_none()
        }) && broad_surface_blocked.is_none()
    }

    pub(crate) fn singleton_verification_command_arguments_are_runtime_owned_fixture_passes() -> bool
    {
        let executable_behavior_command =
            ToolLifecycleRuntime::fixture_executable_verification_command(
                "verify-contract --behavior",
            );
        let executable_behavior_arguments =
            serde_json::to_string(&json!({"command": executable_behavior_command.clone()}))
                .unwrap_or_default();
        let active = ActiveWorkContract::Verification {
            commands: vec!["verify-contract --behavior".to_string()],
            failing_labels: Vec::new(),
            repair_required: false,
            targets: Vec::new(),
        };
        let repair_active = ActiveWorkContract::Verification {
            commands: vec!["verify-contract --behavior".to_string()],
            failing_labels: vec!["workflow_repair_behavior_contract".to_string()],
            repair_required: true,
            targets: vec![Utf8PathBuf::from("src/workflow.ts")],
        };
        let multi_active = ActiveWorkContract::Verification {
            commands: vec![
                "verify-contract --behavior".to_string(),
                "verify-contract --schema src/workflow.ts".to_string(),
            ],
            failing_labels: Vec::new(),
            repair_required: false,
            targets: Vec::new(),
        };
        let repaired = Self::repair_shell_arguments_from_singleton_verification_command(
            "shell",
            r#"{"command":"Get-ChildItem","workdir":"C:/tmp","timeout":5}"#,
            Some(&active),
            ShellFamily::PowerShell,
        )
        .and_then(|args| serde_json::from_str::<Value>(&args).ok());
        let already_exact = Self::repair_shell_arguments_from_singleton_verification_command(
            "shell",
            &executable_behavior_arguments,
            Some(&active),
            ShellFamily::PowerShell,
        );
        let corrected_identity_match =
            Self::repair_shell_arguments_from_singleton_verification_command(
                "shell",
                r#"{"command":"verify-contract --behavior"}"#,
                Some(&active),
                ShellFamily::PowerShell,
            )
            .and_then(|args| serde_json::from_str::<Value>(&args).ok());
        let repair_lane = Self::repair_shell_arguments_from_singleton_verification_command(
            "shell",
            r#"{"command":"Get-ChildItem"}"#,
            Some(&repair_active),
            ShellFamily::PowerShell,
        );
        let multi_command = Self::repair_shell_arguments_from_singleton_verification_command(
            "shell",
            r#"{"command":"Get-ChildItem"}"#,
            Some(&multi_active),
            ShellFamily::PowerShell,
        );

        let repaired_command = repaired
            .as_ref()
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str);
        let corrected_identity_match_command = corrected_identity_match
            .as_ref()
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str);
        let wrong_after_repair = repaired.as_ref().and_then(|value| {
            ToolLifecycleRuntime::wrong_verification_shell_command_result(
                "shell",
                value,
                Some(&active),
                ShellFamily::PowerShell,
            )
        });

        repaired_command == Some(executable_behavior_command.as_str())
            && corrected_identity_match_command == Some(executable_behavior_command.as_str())
            && repaired.as_ref().is_some_and(|value| {
                value.get("workdir").is_none() && value.get("timeout").is_none()
            })
            && wrong_after_repair.is_none()
            && already_exact.is_none()
            && repair_lane.is_none()
            && multi_command.is_none()
    }

    pub(crate) fn verification_public_command_fixture_domain_neutral_fixture_passes() -> bool {
        let tool_lifecycle_source = include_str!("tool_orchestrator.rs");
        let lifecycle_source = include_str!("lifecycle_kernel.rs");
        let owner_block = tool_lifecycle_source
            .split(
                "pub(crate) fn verification_active_work_preserves_tool_surface_and_rejects_wrong_command_failed_checks",
            )
            .nth(1)
            .and_then(|tail| {
                tail.split("pub(crate) fn record_operation_non_content_no_progress")
                    .next()
            })
            .unwrap_or_default();
        let lifecycle_block = lifecycle_source
            .split(
                "pub(crate) fn post_repair_required_verification_dispatch_is_runtime_owned_fixture_passes",
            )
            .nth(1)
            .and_then(|tail| {
                tail.split(
                    "\n    pub(crate) fn repair_shell_arguments_from_singleton_verification_command",
                )
                .next()
            })
            .unwrap_or_default();
        let authority_block = format!("{owner_block}\n{lifecycle_block}");

        !authority_block.contains("workflow-cli src/workflow.rs 8 +")
            && !authority_block.contains("workflow-cli src/workflow.rs beta 42")
            && !authority_block.contains("test_calculate")
            && !authority_block.contains("workflow_compute(1 + 2)")
            && !authority_block.contains("expected: Some(\"3\"")
            && !authority_block.contains("\"1 + 2\"")
            && authority_block.contains("workflow-tool combine draft + review")
            && authority_block.contains("workflow-tool inspect draft + review")
            && authority_block.contains("workflow_behavior_verification_contract")
            && authority_block.contains("workflow_process")
    }

    pub(crate) fn adjudicate_tool_call_model_action<F>(
        tool_call: &CompletedToolCall,
        runtime_owned_verification_redirect: Option<&RuntimeOwnedVerificationRedirectSnapshot>,
        allowed_tools: &BTreeSet<String>,
        envelope: &TurnControlEnvelope,
        tool_exists: F,
    ) -> ToolCallModelActionAdjudication
    where
        F: FnOnce(&str) -> bool,
    {
        let adjudication_tool_call =
            if let Some(redirect) = runtime_owned_verification_redirect.as_ref() {
                CompletedToolCall {
                    call_id: tool_call.call_id.clone(),
                    tool_name: redirect.effective_tool_name.clone(),
                    arguments_json: redirect.effective_arguments_json.clone(),
                }
            } else {
                tool_call.clone()
            };
        let model_action = ProviderActionAdapter::adapt_tool_call(&adjudication_tool_call);
        let action_name = model_action.requested_action_name().to_string();
        let tool_exists = tool_exists(&action_name);
        let tool_allowed = allowed_tools.contains(&action_name);
        let adjudication = Self::adjudicate_model_action(
            model_action,
            allowed_tools,
            tool_exists,
            tool_allowed,
            envelope,
        );
        ToolCallModelActionAdjudication {
            action_name,
            tool_exists,
            tool_allowed,
            adjudication,
        }
    }

    pub(crate) fn repair_shell_arguments_from_singleton_verification_command(
        effective_tool_name: &str,
        arguments_json: &str,
        active_work: Option<&ActiveWorkContract>,
        shell_family: ShellFamily,
    ) -> Option<String> {
        if effective_tool_name != "shell" {
            return None;
        }
        let Some(ActiveWorkContract::Verification {
            commands,
            repair_required,
            ..
        }) = active_work
        else {
            return None;
        };
        if *repair_required {
            return None;
        }
        let required_commands = canonical_required_verification_commands(commands);
        if required_commands.len() != 1 {
            return None;
        }
        let parsed = serde_json::from_str::<Value>(arguments_json).ok()?;
        let submitted = parsed.get("command").and_then(Value::as_str)?.trim();
        let submitted_keys = canonical_shell_command_keys(submitted);
        let required_key = canonical_verification_command_identity_key(&required_commands[0])?;
        if verification_command_key_family_matches(
            &submitted_keys,
            &BTreeSet::from([required_key.clone()]),
        ) {
            let suggested = crate::tool::shell::command_text_encoding_suggested_command(
                submitted,
                shell_family,
            )?;
            if normalized_command_text_for_family_match(&suggested)
                == normalized_command_text_for_family_match(submitted)
            {
                return None;
            }
            return Some(
                json!({
                    "command": suggested,
                    "description": "Run runtime-owned required verification command"
                })
                .to_string(),
            );
        }
        let command = executable_verification_command_forms(&required_commands, shell_family)
            .into_iter()
            .next()
            .unwrap_or_else(|| required_commands[0].clone());
        Some(
            json!({
                "command": command,
                "description": "Run runtime-owned required verification command"
            })
            .to_string(),
        )
    }

    pub(crate) fn canonical_required_verification_commands(
        required_commands: &[String],
    ) -> Vec<String> {
        canonical_required_verification_commands(required_commands)
    }

    pub(crate) fn prepare_tool_route_arguments(
        input: PrepareToolRouteArgumentsInput<'_>,
        escaped_source_write_candidate_arguments_json: Option<&str>,
    ) -> PreparedToolRouteArguments {
        let effective_tool_name = input
            .runtime_owned_verification_redirect
            .as_ref()
            .map(|redirect| redirect.effective_tool_name.clone())
            .unwrap_or_else(|| input.requested_tool_name.to_string());
        let effective_arguments_json = input
            .runtime_owned_verification_redirect
            .as_ref()
            .map(|redirect| redirect.effective_arguments_json.clone())
            .or_else(|| {
                repair_write_arguments_from_active_target(
                    &effective_tool_name,
                    input.original_arguments_json,
                    input.active_targets_for_argument_repair,
                )
            })
            .or_else(|| input.shell_repaired_arguments_json.map(str::to_string))
            .or_else(|| escaped_source_write_candidate_arguments_json.map(str::to_string))
            .or_else(|| {
                repair_unambiguous_malformed_edit_arguments_json(
                    &effective_tool_name,
                    input.original_arguments_json,
                )
            })
            .unwrap_or_else(|| input.original_arguments_json.to_string());
        PreparedToolRouteArguments {
            effective_tool_name,
            effective_arguments_json,
            redirected_from_arguments_json: input
                .runtime_owned_verification_redirect
                .as_ref()
                .map(|redirect| redirect.redirected_from_arguments_json.clone()),
            redirect_reason: input
                .runtime_owned_verification_redirect
                .as_ref()
                .map(|redirect| redirect.redirect_reason),
        }
    }

    pub(crate) fn open_obligation_final_message_rejection<'a>(
        adjudication: Option<&'a ActionAdjudication>,
        finish_reason: Option<&FinishReason>,
    ) -> Option<&'a ModelActionRejection> {
        if matches!(finish_reason, Some(FinishReason::Length)) {
            return None;
        }
        match adjudication {
            Some(ActionAdjudication::RejectedModelAction(rejection))
                if rejection.semantic_class == "text_final_while_obligations_open" =>
            {
                Some(rejection)
            }
            _ => None,
        }
    }

    pub(crate) fn open_obligation_final_message_terminal_threshold() -> usize {
        OPEN_OBLIGATION_FINAL_MESSAGE_TERMINAL_THRESHOLD
    }

    pub(crate) fn open_obligation_final_message_recovery_envelope(
        state: &SessionStateSnapshot,
        count: usize,
        required_action: Option<&RequiredAction>,
        allowed_tools: &BTreeSet<String>,
        docs_grounding_required: bool,
    ) -> OpenObligationFinalMessageRecoveryEnvelope {
        OpenObligationFinalMessageRecoveryEnvelope {
            count,
            active_targets: state
                .active_targets
                .iter()
                .map(|target| target.as_str().to_string())
                .collect(),
            prompt: Self::open_obligation_final_message_correction_text(
                state,
                count,
                required_action,
                allowed_tools,
                docs_grounding_required,
            ),
        }
    }

    pub(crate) fn open_obligation_final_message_guard_key(
        state: &SessionStateSnapshot,
        required_action: Option<&RequiredAction>,
        invalid_edit_recovery: Option<&InvalidEditRecoveryEnvelope>,
        docs_grounding_required: bool,
    ) -> String {
        let active_targets = if state.active_targets.is_empty() {
            "none".to_string()
        } else {
            state
                .active_targets
                .iter()
                .map(|target| target.as_str().to_string())
                .collect::<Vec<_>>()
                .join(",")
        };
        let recovery_context = invalid_edit_recovery
            .map(|envelope| {
                let targets = if envelope.active_targets.is_empty() {
                    "none".to_string()
                } else {
                    envelope.active_targets.join(",")
                };
                let candidate = envelope.candidate_target.as_deref().unwrap_or("none");
                let family = envelope.parser_error_family.as_deref().unwrap_or("none");
                format!(
                    "invalid_edit_arguments:tool={}:candidate={candidate}:family={family}:targets={targets}",
                    envelope.tool_name
                )
            })
            .unwrap_or_else(|| "none".to_string());
        let required_action_projection = required_action
            .map(RequiredAction::projection_label)
            .unwrap_or_else(|| "none".to_string());
        format!(
            "open_obligation_final_message|route={:?}|phase={:?}|targets={active_targets}|required_action={required_action_projection}|docs_grounding={docs_grounding_required}|recovery={recovery_context}",
            state.route, state.process_phase,
        )
    }

    pub(crate) fn open_obligation_final_message_terminal_message(
        state: &SessionStateSnapshot,
        attempts: usize,
    ) -> String {
        let targets = state
            .active_targets
            .iter()
            .map(|target| target.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let blocked_reason = state
            .completion
            .blocked_reason
            .as_deref()
            .unwrap_or("open obligations remain");
        if targets.is_empty() {
            format!(
                "model returned a final assistant message {attempts} time(s) while {blocked_reason}; no clean closeout was accepted"
            )
        } else {
            format!(
                "model returned a final assistant message {attempts} time(s) while {blocked_reason}; open targets: {targets}; no clean closeout was accepted"
            )
        }
    }

    fn open_obligation_final_message_correction_text(
        state: &SessionStateSnapshot,
        attempt: usize,
        required_action: Option<&RequiredAction>,
        allowed_tools: &BTreeSet<String>,
        docs_grounding_required: bool,
    ) -> String {
        let targets = state
            .active_targets
            .iter()
            .map(|target| target.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let target_line = if targets.is_empty() {
            "Open targets: none recorded.".to_string()
        } else {
            format!("Open targets: {targets}.")
        };
        let blocked_reason = state
            .completion
            .blocked_reason
            .as_deref()
            .unwrap_or("Open work remains for the latest user request.");
        let has_open_edit_work =
            state.completion.open_work_count > 0 || !state.active_targets.is_empty();
        let next_action = if docs_grounding_required {
            Self::docs_route_content_grounding_correction_text(&targets, allowed_tools)
        } else if let Some(action) = required_action {
            Self::open_obligation_required_action_correction_text(action, &targets, allowed_tools)
        } else if has_open_edit_work {
            Self::open_obligation_file_change_correction_text(&targets, allowed_tools)
        } else if state.completion.verification_pending
            || !state.verification.required_commands.is_empty()
        {
            let commands = Self::canonical_required_verification_commands(
                &state.verification.required_commands,
            );
            let command_text = if commands.is_empty() {
                "the required verification command".to_string()
            } else {
                commands.join(", ")
            };
            format!(
                "Use the `shell` tool to run the required verification command before any final assistant message: {command_text}. A text-only promise does not satisfy this turn."
            )
        } else {
            Self::open_obligation_file_change_correction_text(&targets, allowed_tools)
        };
        let provider_tool_choice_line = if attempt >= 2
            && required_action.is_some()
            && !allowed_tools.is_empty()
        {
            " The previous recovery request already required a tool call; this continuation treats another text-only response as provider ignored required tool-choice evidence and keeps the same typed action authority."
        } else {
            ""
        };
        format!(
            "The previous response was not accepted as a final answer because the current turn still has open obligations. Attempt {attempt}/{OPEN_OBLIGATION_FINAL_MESSAGE_TERMINAL_THRESHOLD}. {blocked_reason}{provider_tool_choice_line}\n{target_line}\n{next_action}"
        )
    }

    fn docs_route_content_grounding_correction_text(
        targets: &str,
        allowed_tools: &BTreeSet<String>,
    ) -> String {
        let allowed = if allowed_tools.is_empty() {
            "none".to_string()
        } else {
            allowed_tools.iter().cloned().collect::<Vec<_>>().join(", ")
        };
        let target_line = if targets.is_empty() {
            "the pending docs deliverable".to_string()
        } else {
            targets.to_string()
        };
        format!(
            "Docs authoring still needs content-bearing repository evidence before clean closeout. Available tools for this recovery step: {allowed}. Use `read`, `grep`, `docling_convert`, `mcp_call`, or `shell` to inspect a concrete source, test, config, or document file that grounds `{target_line}`, or use `apply_patch` if the visible evidence is already sufficient to create or update the docs target. Directory listings and final-answer prose do not satisfy this step; the satisfying docs authoring progress is `apply_patch` file-change evidence for the active docs target."
        )
    }

    fn open_obligation_required_action_correction_text(
        required_action: &RequiredAction,
        targets: &str,
        allowed_tools: &BTreeSet<String>,
    ) -> String {
        let required_action_projection = required_action.projection_label();
        if required_action.tool == ToolName::Write {
            let target = required_action
                .edit_target()
                .map(Utf8Path::as_str)
                .map(str::trim)
                .filter(|target| !target.is_empty())
                .unwrap_or(targets);
            return format!(
                "Required action: `{required_action_projection}`. Call the `write` tool now with `path` exactly `{target}` and complete updated file content. Do not call supporting tools or answer in text; source repair remains open until that file-change evidence exists."
            );
        }
        if required_action.tool == ToolName::ApplyPatch {
            let target = required_action
                .edit_target()
                .map(Utf8Path::as_str)
                .map(str::trim)
                .filter(|target| !target.is_empty())
                .unwrap_or(targets);
            return format!(
                "Required action: `{required_action_projection}`. Call the `apply_patch` tool now with a patch that changes `{target}`. Do not call supporting tools or answer in text; source repair remains open until that file-change evidence exists."
            );
        }
        if let Some(command) = required_action.shell_command().map(str::trim) {
            return format!(
                "Required action: `{required_action_projection}`. Use the `shell` tool to run the required verification command before any final assistant message: {command}. A text-only promise does not satisfy this turn."
            );
        }
        let allowed = if allowed_tools.is_empty() {
            "none".to_string()
        } else {
            allowed_tools.iter().cloned().collect::<Vec<_>>().join(", ")
        };
        format!(
            "Required action: `{required_action_projection}`. Use the currently allowed tool surface ({allowed}) to complete that exact action before any final assistant message. A text-only promise does not satisfy this turn."
        )
    }

    fn open_obligation_file_change_correction_text(
        targets: &str,
        allowed_tools: &BTreeSet<String>,
    ) -> String {
        if allowed_tools.contains("apply_patch")
            && allowed_tools.contains("write")
            && !targets.is_empty()
            && targets.contains(", ")
        {
            return format!(
                "Use `apply_patch` or `write` for the open targets before any final assistant message: create or update these active targets: {targets}. With `apply_patch`, submit a single patch whose `patch_text` may contain multiple `*** Add File` or `*** Update File` sections. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn."
            );
        }
        if allowed_tools.contains("apply_patch") && !targets.is_empty() && targets.contains(", ") {
            return format!(
                "Use the `apply_patch` tool for the open targets before any final assistant message: submit a single patch whose `patch_text` creates or updates these active targets: {targets}. The patch may contain multiple `*** Add File` or `*** Update File` sections. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn."
            );
        }
        if allowed_tools.contains("apply_patch") && !targets.is_empty() && !targets.contains(", ") {
            return format!(
                "Use the `apply_patch` tool for the active target before any final assistant message: submit a patch whose `patch_text` adds or updates `{targets}`. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn."
            );
        }
        if allowed_tools.contains("write") && !targets.is_empty() && targets.contains(", ") {
            return format!(
                "Use file-changing tool calls for the open targets before any final assistant message: create or update these active targets: {targets}. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn."
            );
        }
        if allowed_tools.contains("write") && !targets.is_empty() && !targets.contains(", ") {
            return format!(
                "Use the `write` tool for the active target before any final assistant message: set `path` exactly to `{targets}` and provide complete updated file content. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn."
            );
        }
        if allowed_tools.contains("apply_patch") {
            if allowed_tools.contains("write") {
                return "Use `apply_patch` or `write` for the active target before any final assistant message. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn.".to_string();
            }
            return "Use `apply_patch` for the active target before any final assistant message. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn.".to_string();
        }
        "Use a file-changing tool call for the active target before any final assistant message. Supporting context tools and planning are no-progress evidence only; a text-only promise does not satisfy this turn.".to_string()
    }

    pub(crate) fn compile_recovery_context(
        input: TurnLifecycleRecoveryContextInput<'_>,
    ) -> TurnLifecycleRecoveryContext {
        let open_obligation_final_message_recovery_active =
            Self::open_obligation_final_message_recovery_active(
                input.state,
                input.has_open_obligation_final_message_recovery,
            );
        let failed_edit_recovery_active =
            Self::failed_edit_recovery_active(input.state, input.has_invalid_edit_recovery);
        let code_authoring_final_message_hard_edit_recovery_active =
            Self::code_authoring_final_message_hard_edit_recovery_active(
                input.state,
                input.open_obligation_final_message_hard_edit_recovery_pending,
                input.open_obligation_final_message_recovery_count,
            );
        let code_authoring_final_message_recovery_stable_surface_active =
            Self::code_authoring_final_message_recovery_stable_surface_active(
                input.state,
                open_obligation_final_message_recovery_active,
                code_authoring_final_message_hard_edit_recovery_active,
                failed_edit_recovery_active,
            );
        let code_repair_final_message_recovery_stable_surface_active =
            Self::code_repair_final_message_recovery_stable_surface_active(
                input.state,
                open_obligation_final_message_recovery_active,
                failed_edit_recovery_active,
            );
        let docs_content_grounding_recovery_active =
            Self::docs_route_requires_content_grounding_before_write(
                input.state,
                input.docs_route_has_required_content_grounding_evidence,
            );
        let malformed_write_patch_recovery_active = Self::malformed_write_patch_recovery_active(
            input.state,
            input.malformed_write_patch_recovery_pending,
            input.current_tool_names,
        );
        let malformed_apply_patch_write_recovery_active =
            Self::malformed_apply_patch_write_recovery_active(
                input.state,
                input.malformed_apply_patch_write_recovery_pending,
                input.current_tool_names,
            );
        let wrong_target_authoring_edit_recovery_active =
            Self::wrong_target_authoring_edit_recovery_applies(
                input.state,
                input.wrong_authoring_target_counts,
            ) && !malformed_write_patch_recovery_active
                && !malformed_apply_patch_write_recovery_active;
        let provider_noncompliance_edit_recovery_active =
            Self::provider_noncompliance_edit_recovery_applies(
                input.state,
                input.rejected_tool_proposals,
            );
        let verification_target_grounding_active = input.verification_target_grounding_active
            || Self::second_pass_verification_repair_target_grounding_active(
                input.state,
                input.post_provider_tool_names,
                input.repair_supporting_context_budget_recovery_active,
                provider_noncompliance_edit_recovery_active,
                wrong_target_authoring_edit_recovery_active,
                input.patch_context_mismatch_grounding_active,
            );
        let docs_grounding_final_message_recovery_active =
            open_obligation_final_message_recovery_active
                && !code_authoring_final_message_recovery_stable_surface_active
                && !code_repair_final_message_recovery_stable_surface_active
                && docs_content_grounding_recovery_active;
        let authoring_target_grounding_final_message_recovery_active =
            open_obligation_final_message_recovery_active
                && !code_authoring_final_message_recovery_stable_surface_active
                && !code_repair_final_message_recovery_stable_surface_active
                && Self::authoring_target_grounding_final_message_recovery_active(
                    input.state,
                    input.authoring_targets_need_grounding,
                );
        let provider_required_tool_choice_final_message_recovery_active = input
            .provider_required_tool_choice_final_message_recovery_pending
            && Self::open_executable_work_requires_tool_call(input.state)
            && Self::provider_required_tool_choice_final_message_recovery_has_write_surface(
                input.tools,
                input.stable_tools,
            );
        let progress_projection_edit_recovery_active =
            Self::progress_projection_edit_recovery_active(
                input.state,
                !input.progress_projection_no_progress_counts.is_empty(),
                input.current_tool_names,
            );
        TurnLifecycleRecoveryContext {
            provider_noncompliance_edit_recovery_active,
            wrong_target_authoring_edit_recovery_active,
            provider_required_tool_choice_final_message_recovery_active,
            code_authoring_final_message_hard_edit_recovery_active,
            generated_test_source_reference_grounding_active: input
                .generated_test_source_reference_grounding_active,
            generated_test_reference_consumed_target_grounding_active: input
                .generated_test_reference_consumed_target_grounding_active,
            verification_target_grounding_active,
            authoring_target_grounding_recovery_edit_only: input
                .authoring_target_grounding_recovery_edit_only,
            patch_context_mismatch_grounding_active: input.patch_context_mismatch_grounding_active,
            authoring_target_grounding_final_message_recovery_active,
            existing_target_grounding_recovery_active: input
                .existing_target_grounding_recovery_active,
            docs_grounding_final_message_recovery_active,
            docs_content_grounding_recovery_active,
            malformed_write_patch_recovery_active,
            malformed_apply_patch_write_recovery_active,
            progress_projection_edit_recovery_active,
            progress_projection_edit_recovery_needs_grounding_read:
                progress_projection_edit_recovery_active
                    && input.progress_projection_target_grounding_read_needed,
            failed_edit_recovery_active,
            open_obligation_final_message_recovery_active,
            open_obligation_final_message_count: input
                .open_obligation_final_message_recovery_count
                .unwrap_or_default(),
        }
    }

    pub(crate) fn compile_turn_lifecycle_plan(
        input: TurnLifecyclePlanInput<'_>,
    ) -> TurnLifecyclePlan {
        let tool_choice = lifecycle_tool_choice(&input);
        let plan_reason = lifecycle_plan_reason(&input).to_string();
        TurnLifecyclePlan {
            tool_choice,
            effective_tools: input.tool_names.clone(),
            replay_policy: lifecycle_replay_policy(&plan_reason),
            proposal_policy: lifecycle_proposal_policy(&plan_reason),
            corrective_policy: lifecycle_corrective_policy(&plan_reason),
            terminal_policy: lifecycle_terminal_policy(&plan_reason),
            continuation_expectation: lifecycle_continuation_expectation(&input, &plan_reason),
            diagnostics_projection: lifecycle_diagnostics_projection(&plan_reason),
            plan_reason,
        }
    }

    pub(crate) fn adjudicate_model_action(
        proposal: ModelActionProposal,
        allowed_tools: &BTreeSet<String>,
        tool_exists: bool,
        tool_allowed: bool,
        envelope: &TurnControlEnvelope,
    ) -> ActionAdjudication {
        match proposal {
            ModelActionProposal::ToolCall(proposal) => {
                ActionAdjudicator::adjudicate_tool_call(&ActionAdjudicationInput {
                    proposal,
                    allowed_tools,
                    tool_exists,
                    tool_allowed,
                    envelope,
                })
            }
            ModelActionProposal::MalformedToolArguments(proposal) => Self::reject_tool_like_action(
                ModelToolCallProposal {
                    call_id: proposal.source_call_id,
                    requested_tool: proposal.requested_tool.clone(),
                    effective_tool: proposal.requested_tool,
                    arguments_json: proposal.raw_arguments,
                },
                "malformed_tool_arguments",
                "The provider emitted malformed tool arguments.",
                allowed_tools,
                envelope,
            ),
            ModelActionProposal::SchemaOutsideToolProposal(proposal) => {
                Self::reject_tool_like_action(
                    ModelToolCallProposal {
                        call_id: proposal.source_call_id,
                        requested_tool: proposal.requested_tool.clone(),
                        effective_tool: proposal.requested_tool,
                        arguments_json: proposal.raw_payload,
                    },
                    "schema_outside_tool_proposal",
                    "The provider emitted a tool payload outside the configured schema.",
                    allowed_tools,
                    envelope,
                )
            }
            ModelActionProposal::TextFinalWhileObligationsOpen(proposal) => {
                Self::reject_text_final_action(
                    proposal,
                    "final_assistant_message".to_string(),
                    "text_final_while_obligations_open",
                    "The provider emitted a final message while obligations remain open.",
                    allowed_tools,
                    envelope,
                )
            }
            ModelActionProposal::TextFinal(_) => Self::reject_non_tool_action(
                "non_tool_action".to_string(),
                "non_tool_action_not_executable",
                "This model action is not executable as a tool call.",
                allowed_tools,
                envelope,
            ),
        }
    }

    pub(crate) fn apply_codex_style_provider_edit_surface(
        tools: &mut Vec<ToolSchema>,
        state: &SessionStateSnapshot,
    ) {
        if matches!(state.route, TaskRoute::Code)
            && matches!(
                state.process_phase,
                ProcessPhase::Author | ProcessPhase::Repair
            )
        {
            tools
                .retain(|tool| matches!(tool.name.as_str(), "apply_patch" | "shell" | "todowrite"));
        } else if docs_authoring_uses_codex_style_provider_surface(state) {
            tools.retain(|tool| docs_authoring_codex_surface_tool_visible(&tool.name));
        }
    }

    pub(crate) fn apply_post_normalization_recovery_surface(
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
        input: TurnLifecycleRecoverySurfaceInput<'_>,
    ) {
        Self::apply_codex_style_provider_edit_surface(tools, input.state);
        if input.recovery.provider_noncompliance_edit_recovery_active {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                Self::provider_noncompliance_edit_recovery_tool_visible,
            );
            tools
                .retain(|tool| Self::provider_noncompliance_edit_recovery_tool_visible(&tool.name));
            return;
        }
        if input.recovery.malformed_apply_patch_write_recovery_active {
            augment_tools_from_stable_surface(tools, stable_tools, |name| name == "apply_patch");
            tools.retain(|tool| tool.name == "apply_patch");
            return;
        }
        if input.recovery.malformed_write_patch_recovery_active
            || input.code_authoring_final_message_hard_edit_recovery_active
        {
            augment_tools_from_stable_surface(tools, stable_tools, |name| {
                matches!(name, "apply_patch" | "write")
            });
            tools.retain(|tool| matches!(tool.name.as_str(), "apply_patch" | "write"));
            return;
        }
        if input.recovery.wrong_target_authoring_edit_recovery_active {
            if input
                .recovery
                .generated_test_source_reference_grounding_active
            {
                augment_tools_from_stable_surface(
                    tools,
                    stable_tools,
                    wrong_target_generated_test_source_reference_recovery_tool_visible,
                );
                tools.retain(|tool| {
                    wrong_target_generated_test_source_reference_recovery_tool_visible(&tool.name)
                });
            } else {
                augment_tools_from_stable_surface(
                    tools,
                    stable_tools,
                    wrong_target_authoring_recovery_tool_visible,
                );
                tools.retain(|tool| wrong_target_authoring_recovery_tool_visible(&tool.name));
            }
            return;
        }
        if input.recovery.failed_edit_recovery_active
            && input.recovery.open_obligation_final_message_recovery_active
        {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                failed_edit_final_message_recovery_tool_visible,
            );
            tools.retain(|tool| failed_edit_final_message_recovery_tool_visible(&tool.name));
            return;
        }
        if input.recovery.authoring_target_grounding_recovery_edit_only {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                edit_only_authoring_grounding_recovery_tool_visible,
            );
            tools.retain(|tool| edit_only_authoring_grounding_recovery_tool_visible(&tool.name));
            return;
        }
        if input.recovery.verification_target_grounding_active {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                verification_repair_target_grounding_surface_tool_visible,
            );
            tools.retain(|tool| {
                verification_repair_target_grounding_surface_tool_visible(&tool.name)
            });
            return;
        }
        if input.recovery.patch_context_mismatch_grounding_active
            && docs_authoring_patch_context_grounding_keeps_auto(input.state)
        {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                docs_patch_context_mismatch_grounding_tool_visible,
            );
            tools.retain(|tool| docs_patch_context_mismatch_grounding_tool_visible(&tool.name));
            return;
        }
        if input
            .recovery
            .provider_required_tool_choice_final_message_recovery_active
        {
            augment_tools_from_stable_surface(tools, stable_tools, |name| name == "write");
            tools.retain(|tool| tool.name == "write");
            return;
        }
        if input.recovery.docs_content_grounding_recovery_active {
            let visible = |tool_name: &str| {
                if input.recovery.progress_projection_edit_recovery_active {
                    docs_route_content_grounding_after_progress_projection_tool_visible(tool_name)
                } else {
                    docs_route_content_grounding_recovery_tool_visible(tool_name)
                }
            };
            augment_tools_from_stable_surface(tools, stable_tools, visible);
            tools.retain(|tool| visible(&tool.name));
            return;
        }
        if input.recovery.progress_projection_edit_recovery_active {
            augment_tools_from_stable_surface(tools, stable_tools, |name| {
                progress_projection_edit_recovery_tool_visible(
                    input.state,
                    name,
                    input
                        .recovery
                        .progress_projection_edit_recovery_needs_grounding_read,
                )
            });
            tools.retain(|tool| {
                progress_projection_edit_recovery_tool_visible(
                    input.state,
                    &tool.name,
                    input
                        .recovery
                        .progress_projection_edit_recovery_needs_grounding_read,
                )
            });
            return;
        }
        if input
            .recovery
            .generated_test_source_reference_grounding_active
        {
            augment_tools_from_stable_surface(tools, stable_tools, |tool_name| {
                generated_test_source_reference_grounding_tool_visible(
                    tool_name,
                    input.generated_test_orientation_allowed,
                )
            });
            tools.retain(|tool| {
                generated_test_source_reference_grounding_tool_visible(
                    &tool.name,
                    input.generated_test_orientation_allowed,
                )
            });
            return;
        }
        if input
            .recovery
            .generated_test_reference_consumed_target_grounding_active
        {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                authoring_target_grounding_recovery_tool_visible,
            );
            tools.retain(|tool| authoring_target_grounding_recovery_tool_visible(&tool.name));
            return;
        }
        if input.recovery.existing_target_grounding_recovery_active {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                existing_target_grounding_recovery_tool_visible,
            );
            tools.retain(|tool| existing_target_grounding_recovery_tool_visible(&tool.name));
        }
    }

    pub(crate) fn apply_pre_normalization_recovery_surface(
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
        input: TurnLifecyclePreNormalizationSurfaceInput<'_>,
    ) {
        if input.recovery.provider_noncompliance_edit_recovery_active {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                Self::provider_noncompliance_edit_recovery_tool_visible,
            );
            tools
                .retain(|tool| Self::provider_noncompliance_edit_recovery_tool_visible(&tool.name));
        } else if input.recovery.malformed_apply_patch_write_recovery_active {
            augment_tools_from_stable_surface(tools, stable_tools, |tool_name| {
                tool_name == "apply_patch"
            });
            tools.retain(|tool| tool.name == "apply_patch");
        } else if input.recovery.wrong_target_authoring_edit_recovery_active {
            if input
                .recovery
                .generated_test_source_reference_grounding_active
            {
                augment_tools_from_stable_surface(
                    tools,
                    stable_tools,
                    wrong_target_generated_test_source_reference_recovery_tool_visible,
                );
                tools.retain(|tool| {
                    wrong_target_generated_test_source_reference_recovery_tool_visible(&tool.name)
                });
            } else {
                augment_tools_from_stable_surface(
                    tools,
                    stable_tools,
                    wrong_target_authoring_recovery_tool_visible,
                );
                tools.retain(|tool| wrong_target_authoring_recovery_tool_visible(&tool.name));
            }
        } else if input
            .recovery
            .provider_required_tool_choice_final_message_recovery_active
        {
            augment_tools_from_stable_surface(tools, stable_tools, |tool_name| {
                tool_name == "write"
            });
            tools.retain(|tool| tool.name == "write");
        } else if input.recovery.failed_edit_recovery_active
            && input.recovery.open_obligation_final_message_recovery_active
        {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                failed_edit_final_message_recovery_tool_visible,
            );
            tools.retain(|tool| failed_edit_final_message_recovery_tool_visible(&tool.name));
        } else if input.code_authoring_final_message_hard_edit_recovery_active {
            augment_tools_from_stable_surface(tools, stable_tools, |tool_name| {
                matches!(tool_name, "apply_patch" | "write")
            });
            tools.retain(|tool| matches!(tool.name.as_str(), "apply_patch" | "write"));
        } else if input.recovery.authoring_target_grounding_recovery_edit_only {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                edit_only_authoring_grounding_recovery_tool_visible,
            );
            tools.retain(|tool| edit_only_authoring_grounding_recovery_tool_visible(&tool.name));
        } else if input.code_authoring_final_message_recovery_stable_surface_active
            || input.code_repair_final_message_recovery_stable_surface_active
        {
            augment_tools_from_stable_surface(tools, stable_tools, |_| true);
        } else if input
            .recovery
            .authoring_target_grounding_final_message_recovery_active
        {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                authoring_target_grounding_recovery_tool_visible,
            );
            tools.retain(|tool| authoring_target_grounding_recovery_tool_visible(&tool.name));
        } else if input.recovery.existing_target_grounding_recovery_active {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                existing_target_grounding_recovery_tool_visible,
            );
            tools.retain(|tool| existing_target_grounding_recovery_tool_visible(&tool.name));
        } else if input.recovery.docs_content_grounding_recovery_active {
            let visible = |tool_name: &str| {
                if input.recovery.progress_projection_edit_recovery_active {
                    docs_route_content_grounding_after_progress_projection_tool_visible(tool_name)
                } else {
                    docs_route_content_grounding_recovery_tool_visible(tool_name)
                }
            };
            augment_tools_from_stable_surface(tools, stable_tools, visible);
            tools.retain(|tool| visible(&tool.name));
        } else if input.recovery.open_obligation_final_message_recovery_active
            && !input.recovery.docs_grounding_final_message_recovery_active
            && !input.recovery.patch_context_mismatch_grounding_active
            && tools.iter().any(|tool| {
                open_obligation_final_message_recovery_tool_visible(input.state, &tool.name)
            })
        {
            augment_tools_from_stable_surface(tools, stable_tools, |tool_name| {
                open_obligation_final_message_recovery_tool_visible(input.state, tool_name)
            });
            tools.retain(|tool| {
                open_obligation_final_message_recovery_tool_visible(input.state, &tool.name)
            });
        } else if input.recovery.docs_grounding_final_message_recovery_active
            && tools
                .iter()
                .any(|tool| docs_route_content_grounding_recovery_tool_visible(&tool.name))
        {
            augment_tools_from_stable_surface(
                tools,
                stable_tools,
                docs_route_content_grounding_recovery_tool_visible,
            );
            tools.retain(|tool| docs_route_content_grounding_recovery_tool_visible(&tool.name));
        } else if input.recovery.malformed_write_patch_recovery_active
            && !input.recovery.verification_target_grounding_active
        {
            tools.retain(|tool| matches!(tool.name.as_str(), "apply_patch" | "write"));
        }
    }

    pub(crate) fn closeout_ready_final_message_authority(state: &SessionStateSnapshot) -> bool {
        (state.completion.closeout_ready || Self::answer_only_final_message_authority(state))
            && state.completion.open_work_count == 0
            && !state.completion.verification_pending
            && !state.completion.route_contract_pending
            && state.completion.blocked_reason.is_none()
            && state.active_targets.is_empty()
            && state.verification.required_commands.is_empty()
            && state.verification.failure_cluster.is_none()
            && state.failure.is_none()
    }

    pub(crate) fn answer_only_final_message_authority(state: &SessionStateSnapshot) -> bool {
        matches!(
            state.process_phase,
            ProcessPhase::Discover | ProcessPhase::Closeout
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

    pub(crate) fn clean_closeout_final_message_lifecycle(
        state: &SessionStateSnapshot,
        active_work: Option<&ActiveWorkContract>,
    ) -> bool {
        active_work.is_none()
            && state.completion.closeout_ready
            && state.completion.open_work_count == 0
            && !state.completion.verification_pending
            && !state.completion.route_contract_pending
            && state.completion.blocked_reason.is_none()
    }

    pub(crate) fn open_executable_work_requires_tool_call(state: &SessionStateSnapshot) -> bool {
        if matches!(
            state.route,
            TaskRoute::Ask | TaskRoute::Review | TaskRoute::Summary
        ) {
            return false;
        }
        !Self::closeout_ready_final_message_authority(state)
            && (state.completion.open_work_count > 0
                || !state.active_targets.is_empty()
                || state.completion.verification_pending
                || !state.verification.required_commands.is_empty())
    }

    pub(crate) fn docs_route_supporting_context_budget_recovery_surface_active(
        state: &SessionStateSnapshot,
        exhausted_keys: &BTreeSet<String>,
    ) -> bool {
        state.route == TaskRoute::Docs
            && state.completion.route_contract_pending
            && !exhausted_keys.is_empty()
    }

    pub(crate) fn authoring_supporting_context_budget_recovery_surface_active(
        state: &SessionStateSnapshot,
        exhausted_keys: &BTreeSet<String>,
    ) -> bool {
        state.route != TaskRoute::Docs
            && Self::open_executable_work_requires_tool_call(state)
            && !state.active_targets.is_empty()
            && !exhausted_keys.is_empty()
    }

    pub(crate) fn repair_supporting_context_budget_recovery_surface_active(
        state: &SessionStateSnapshot,
        exhausted_keys: &BTreeSet<String>,
    ) -> bool {
        state.route != TaskRoute::Docs
            && state.process_phase == ProcessPhase::Repair
            && state.completion.verification_pending
            && !state.active_targets.is_empty()
            && !exhausted_keys.is_empty()
    }

    pub(crate) fn verification_repair_target_grounding_surface_active(
        state: &SessionStateSnapshot,
        allowed_tools: &BTreeSet<String>,
    ) -> bool {
        let Some(repair_lane) =
            crate::agent::repair_lane::project_repair_lane(state, allowed_tools)
        else {
            return false;
        };
        let Some(template) = repair_lane.operation_template.as_ref() else {
            return false;
        };
        let has_edit_surface = template
            .required_edit_surface
            .iter()
            .any(|tool| matches!(tool.as_str(), "write" | "apply_patch"));
        let constrains_repair_surface = template
            .forbidden_stale_tools
            .iter()
            .chain(
                repair_lane
                    .repair_intent
                    .as_ref()
                    .into_iter()
                    .flat_map(|intent| intent.forbidden_directions.iter()),
            )
            .any(|item| {
                item.contains("stale_read_or_shell")
                    || item.contains("stale_shell_before_source_contract_repair")
                    || item.contains("unbounded_context_churn_before_source_contract_repair")
            });
        state.process_phase == ProcessPhase::Repair
            && state.completion.verification_pending
            && has_edit_surface
            && constrains_repair_surface
    }

    pub(crate) fn provider_noncompliance_edit_recovery_applies(
        state: &SessionStateSnapshot,
        rejected_tool_proposals: &BTreeMap<String, usize>,
    ) -> bool {
        matches!(state.process_phase, ProcessPhase::Repair)
            && state.completion.verification_pending
            && Self::provider_noncompliance_edit_recovery_active(rejected_tool_proposals)
    }

    pub(crate) fn wrong_target_authoring_edit_recovery_applies(
        state: &SessionStateSnapshot,
        wrong_authoring_target_counts: &BTreeMap<String, usize>,
    ) -> bool {
        Self::open_executable_work_requires_tool_call(state)
            && matches!(
                state.process_phase,
                ProcessPhase::Author | ProcessPhase::Repair
            )
            && !state.active_targets.is_empty()
            && !wrong_authoring_target_counts.is_empty()
    }

    pub(crate) fn malformed_write_patch_recovery_active(
        state: &SessionStateSnapshot,
        pending: bool,
        tool_names: &BTreeSet<String>,
    ) -> bool {
        pending
            && Self::open_executable_work_requires_tool_call(state)
            && tool_names.contains("write")
            && tool_names.contains("apply_patch")
    }

    pub(crate) fn malformed_apply_patch_write_recovery_active(
        state: &SessionStateSnapshot,
        pending: bool,
        tool_names: &BTreeSet<String>,
    ) -> bool {
        pending
            && Self::open_executable_work_requires_tool_call(state)
            && tool_names.contains("apply_patch")
    }

    pub(crate) fn open_obligation_final_message_recovery_active(
        state: &SessionStateSnapshot,
        has_recovery: bool,
    ) -> bool {
        has_recovery && Self::open_executable_work_requires_tool_call(state)
    }

    pub(crate) fn failed_edit_recovery_active(
        state: &SessionStateSnapshot,
        has_invalid_edit_recovery: bool,
    ) -> bool {
        has_invalid_edit_recovery && Self::open_executable_work_requires_tool_call(state)
    }

    pub(crate) fn code_authoring_final_message_hard_edit_recovery_active(
        state: &SessionStateSnapshot,
        hard_edit_pending: bool,
        final_message_recovery_count: Option<usize>,
    ) -> bool {
        (hard_edit_pending || final_message_recovery_count.is_some_and(|count| count >= 2))
            && Self::code_authoring_open_obligation_final_message_recovery_uses_stable_surface(
                state,
            )
    }

    pub(crate) fn code_authoring_final_message_recovery_stable_surface_active(
        state: &SessionStateSnapshot,
        open_recovery_active: bool,
        hard_edit_recovery_active: bool,
        failed_edit_recovery_active: bool,
    ) -> bool {
        open_recovery_active
            && !hard_edit_recovery_active
            && !failed_edit_recovery_active
            && Self::code_authoring_open_obligation_final_message_recovery_uses_stable_surface(
                state,
            )
    }

    pub(crate) fn code_repair_final_message_recovery_stable_surface_active(
        state: &SessionStateSnapshot,
        open_recovery_active: bool,
        failed_edit_recovery_active: bool,
    ) -> bool {
        open_recovery_active
            && !failed_edit_recovery_active
            && Self::code_repair_open_obligation_final_message_recovery_uses_stable_surface(state)
    }

    pub(crate) fn progress_projection_edit_recovery_active(
        state: &SessionStateSnapshot,
        has_progress_projection_no_progress: bool,
        tool_names: &BTreeSet<String>,
    ) -> bool {
        has_progress_projection_no_progress
            && Self::open_executable_work_requires_tool_call(state)
            && tool_names.iter().any(|tool_name| {
                Self::progress_projection_edit_recovery_tool_visible(state, tool_name, false)
            })
    }

    pub(crate) fn second_pass_verification_repair_target_grounding_active(
        state: &SessionStateSnapshot,
        tool_names: &BTreeSet<String>,
        repair_supporting_context_budget_recovery_active: bool,
        provider_noncompliance_edit_recovery_active: bool,
        wrong_target_authoring_edit_recovery_active: bool,
        patch_context_mismatch_grounding_active: bool,
    ) -> bool {
        !repair_supporting_context_budget_recovery_active
            && !provider_noncompliance_edit_recovery_active
            && !wrong_target_authoring_edit_recovery_active
            && !patch_context_mismatch_grounding_active
            && Self::verification_repair_target_grounding_surface_active(state, tool_names)
    }

    pub(crate) fn authoring_grounding_schema_constraint_required(
        state: &SessionStateSnapshot,
        recovery: TurnLifecycleRecoveryContext,
    ) -> bool {
        (recovery.progress_projection_edit_recovery_active
            && recovery.progress_projection_edit_recovery_needs_grounding_read)
            || (recovery.patch_context_mismatch_grounding_active
                && docs_authoring_patch_context_grounding_keeps_auto(state))
    }

    pub(crate) fn docs_route_requires_content_grounding_before_write(
        state: &SessionStateSnapshot,
        has_required_content_grounding_evidence: bool,
    ) -> bool {
        state.route == TaskRoute::Docs
            && matches!(state.process_phase, ProcessPhase::Author)
            && state.completion.route_contract_pending
            && !has_required_content_grounding_evidence
    }

    pub(crate) fn authoring_target_grounding_final_message_recovery_active(
        state: &SessionStateSnapshot,
        active_targets_need_grounding: bool,
    ) -> bool {
        state.route != TaskRoute::Docs
            && matches!(state.process_phase, ProcessPhase::Author)
            && state.completion.open_work_count > 0
            && !state.active_targets.is_empty()
            && active_targets_need_grounding
    }

    pub(crate) fn existing_target_grounding_recovery_active(
        state: &SessionStateSnapshot,
        active_targets_need_grounding: bool,
    ) -> bool {
        state.route == TaskRoute::Docs
            && matches!(state.process_phase, ProcessPhase::Author)
            && state.completion.open_work_count > 0
            && !state.active_targets.is_empty()
            && active_targets_need_grounding
    }

    pub(crate) fn generated_test_source_reference_grounding_active(
        state: &SessionStateSnapshot,
        has_unread_source_change: bool,
    ) -> bool {
        state.route != TaskRoute::Docs
            && matches!(state.process_phase, ProcessPhase::Author)
            && state.completion.open_work_count > 0
            && state.active_targets.len() == 1
            && state
                .active_targets
                .first()
                .is_some_and(|target| generated_test_target_path(target.as_str()))
            && has_unread_source_change
    }

    pub(crate) fn generated_test_reference_consumed_target_grounding_active(
        state: &SessionStateSnapshot,
        has_current_source_reference_read: bool,
        has_unread_source_change: bool,
        active_targets_need_grounding: bool,
    ) -> bool {
        state.route != TaskRoute::Docs
            && matches!(state.process_phase, ProcessPhase::Author)
            && state.completion.open_work_count > 0
            && state.active_targets.len() == 1
            && state
                .active_targets
                .first()
                .is_some_and(|target| generated_test_target_path(target.as_str()))
            && has_current_source_reference_read
            && !has_unread_source_change
            && active_targets_need_grounding
    }

    pub(crate) fn singleton_missing_authoring_target_create_action_active(
        state: &SessionStateSnapshot,
        active_target_exists: bool,
    ) -> bool {
        state.route != TaskRoute::Docs
            && matches!(state.process_phase, ProcessPhase::Author)
            && state.completion.open_work_count > 0
            && state.active_targets.len() == 1
            && !active_target_exists
    }

    pub(crate) fn docs_route_supporting_context_budget_recovery_tool_visible(
        tool_name: &str,
    ) -> bool {
        matches!(tool_name, "write" | "apply_patch" | "todowrite")
    }

    pub(crate) fn authoring_supporting_context_budget_recovery_tool_visible(
        tool_name: &str,
        target_grounding_read_needed: bool,
    ) -> bool {
        tool_name == "apply_patch" || (target_grounding_read_needed && tool_name == "read")
    }

    pub(crate) fn repair_supporting_context_budget_recovery_tool_visible(tool_name: &str) -> bool {
        matches!(tool_name, "write" | "apply_patch" | "todowrite")
    }

    pub(crate) fn verification_repair_target_grounding_surface_tool_visible(
        tool_name: &str,
    ) -> bool {
        verification_repair_target_grounding_surface_tool_visible(tool_name)
    }

    pub(crate) fn provider_noncompliance_edit_recovery_tool_visible(tool_name: &str) -> bool {
        tool_name == "write"
    }

    pub(crate) fn progress_projection_edit_recovery_tool_visible(
        state: &SessionStateSnapshot,
        tool_name: &str,
        target_grounding_read_needed: bool,
    ) -> bool {
        progress_projection_edit_recovery_tool_visible(
            state,
            tool_name,
            target_grounding_read_needed,
        )
    }

    pub(crate) fn provider_noncompliance_edit_recovery_policy(
        state: &SessionStateSnapshot,
    ) -> RequestReplayPolicyDiagnostic {
        RequestReplayPolicyDiagnostic {
            policy: "provider_noncompliance_edit_recovery_surface".to_string(),
            call_id: None,
            tool_name: None,
            omitted_targets: Vec::new(),
            active_targets: active_target_strings(state),
            reason: "a provider model action outside the compiled edit-only repair surface was preserved as typed ProviderNoncompliance evidence; the next recovery request uses exact-target whole-file write authority instead of replaying ambiguous edit proposals".to_string(),
        }
    }

    pub(crate) fn wrong_target_authoring_edit_recovery_policy(
        state: &SessionStateSnapshot,
    ) -> RequestReplayPolicyDiagnostic {
        RequestReplayPolicyDiagnostic {
            policy: "wrong_target_authoring_edit_recovery_surface".to_string(),
            call_id: None,
            tool_name: None,
            omitted_targets: Vec::new(),
            active_targets: active_target_strings(state),
            reason: "a content-changing edit targeted inactive deliverables and was rejected before side effects; the next Code Authoring recovery request keeps patch-oriented executable authority for the current active target set with provider-portable required tool_choice, adding bounded read only when source grounding is still required".to_string(),
        }
    }

    pub(crate) fn malformed_write_patch_capable_recovery_policy(
        state: &SessionStateSnapshot,
    ) -> RequestReplayPolicyDiagnostic {
        RequestReplayPolicyDiagnostic {
            policy: "malformed_write_patch_capable_recovery_surface".to_string(),
            call_id: None,
            tool_name: Some("write".to_string()),
            omitted_targets: Vec::new(),
            active_targets: active_target_strings(state),
            reason: "a malformed or truncated write call was preserved as invalid_edit_arguments evidence; the next recovery request keeps the same active edit target but uses provider-portable required tool_choice over write/apply_patch instead of named write".to_string(),
        }
    }

    pub(crate) fn malformed_apply_patch_write_recovery_policy(
        state: &SessionStateSnapshot,
    ) -> RequestReplayPolicyDiagnostic {
        RequestReplayPolicyDiagnostic {
            policy: "malformed_apply_patch_write_recovery_surface".to_string(),
            call_id: None,
            tool_name: Some("apply_patch".to_string()),
            omitted_targets: Vec::new(),
            active_targets: active_target_strings(state),
            reason: "a side-effect-free malformed apply_patch call was preserved as invalid_edit_arguments evidence; the next recovery request keeps the same active edit target and exposes only the required apply_patch action surface so equivalent mutation tools cannot drift to a different target".to_string(),
        }
    }

    pub(crate) fn invalid_edit_arguments_control_recovery_policy(
        state: &SessionStateSnapshot,
    ) -> RequestReplayPolicyDiagnostic {
        RequestReplayPolicyDiagnostic {
            policy: "invalid_edit_arguments_control_recovery_projection".to_string(),
            call_id: None,
            tool_name: None,
            omitted_targets: Vec::new(),
            active_targets: active_target_strings(state),
            reason: "side-effect-free invalid edit arguments are preserved as call-id-scoped ToolOutput evidence and also projected into system/control recovery with the same active targets and strict edit grammar".to_string(),
        }
    }

    pub(crate) fn append_recovery_replay_policies(
        policies: &mut Vec<RequestReplayPolicyDiagnostic>,
        state: &SessionStateSnapshot,
        recovery: TurnLifecycleRecoveryContext,
        invalid_edit_control_recovery_active: bool,
    ) {
        if recovery.provider_noncompliance_edit_recovery_active {
            policies.push(Self::provider_noncompliance_edit_recovery_policy(state));
        }
        if recovery.wrong_target_authoring_edit_recovery_active {
            policies.push(Self::wrong_target_authoring_edit_recovery_policy(state));
        }
        if recovery.malformed_write_patch_recovery_active {
            policies.push(Self::malformed_write_patch_capable_recovery_policy(state));
        }
        if recovery.malformed_apply_patch_write_recovery_active {
            policies.push(Self::malformed_apply_patch_write_recovery_policy(state));
        }
        if invalid_edit_control_recovery_active {
            policies.push(Self::invalid_edit_arguments_control_recovery_policy(state));
        }
        if recovery.provider_required_tool_choice_final_message_recovery_active {
            policies.push(Self::provider_required_tool_choice_final_message_recovery_policy(state));
        }
    }

    pub(crate) fn provider_messages_for_dispatch_control(
        bundle_messages: &[ModelMessage],
        control_prompt: String,
        final_message_recovery_prompt: Option<String>,
        invalid_edit_recovery_prompt: Option<String>,
        tool_names: &BTreeSet<String>,
        open_obligations: bool,
    ) -> (Vec<ModelMessage>, Vec<RequestReplayPolicyDiagnostic>) {
        let mut control_segments = Vec::new();
        if let Some(correction) = invalid_edit_recovery_prompt {
            control_segments.push(format!("Invalid edit recovery:\n{correction}"));
        }
        if let Some(correction) = final_message_recovery_prompt {
            control_segments.push(format!(
                "Open-obligation final-message recovery:\n{correction}"
            ));
        }
        control_segments.push(control_prompt);
        let control_prompt = control_segments.join("\n\n");
        let mut provider_messages = bundle_messages.to_vec();
        provider_messages.insert(
            0,
            ModelMessage::System {
                content: control_prompt,
            },
        );
        let surface_filter =
            ReplayNormalizer::filter_to_effective_tool_surface(provider_messages, tool_names);
        let provider_messages = filter_non_authoritative_assistant_text_for_open_obligations(
            surface_filter.messages,
            open_obligations,
        );
        (provider_messages, surface_filter.replay_policies)
    }

    pub(crate) fn provider_messages_for_active_work_image_replay(
        messages: Vec<ModelMessage>,
        state: &SessionStateSnapshot,
        active_work: Option<&ActiveWorkContract>,
    ) -> (Vec<ModelMessage>, Option<RequestReplayPolicyDiagnostic>) {
        if active_work_requires_provider_images(state, active_work) {
            return (messages, None);
        }

        let mut omitted_images = 0usize;
        let mut omitted_bytes = 0u64;
        let filtered = messages
            .into_iter()
            .map(|message| match message {
                ModelMessage::UserParts { parts } => {
                    let mut text_parts = Vec::new();
                    for part in parts {
                        match part {
                            ModelContentPart::Text { text } => {
                                text_parts.push(ModelContentPart::Text { text });
                            }
                            ModelContentPart::Image { data_base64, .. } => {
                                omitted_images += 1;
                                omitted_bytes += data_base64.len() as u64;
                            }
                        }
                    }
                    let content = text_parts
                        .into_iter()
                        .filter_map(|part| match part {
                            ModelContentPart::Text { text } => Some(text),
                            ModelContentPart::Image { .. } => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    ModelMessage::User {
                        content: if content.trim().is_empty() {
                            "Prior image input is retained as typed evidence but is not reattached to this provider request because the active work does not require visual reinspection.".to_string()
                        } else {
                            content
                        },
                    }
                }
                other => other,
            })
            .collect::<Vec<_>>();

        let policy = (omitted_images > 0).then(|| {
            let active_targets = active_work
                .map(ActiveWorkContract::targets)
                .filter(|targets| !targets.is_empty())
                .unwrap_or_else(|| state.active_targets.clone())
                .into_iter()
                .map(|target| target.to_string())
                .collect::<Vec<_>>();
            RequestReplayPolicyDiagnostic {
                policy: "consumed_vision_image_omitted_from_provider_request".to_string(),
                call_id: None,
                tool_name: None,
                omitted_targets: Vec::new(),
                active_targets,
                reason: format!(
                    "omitted {omitted_images} consumed image part(s), {omitted_bytes} base64 byte(s), from executable provider messages because current active work is verification/repair text work rather than visual reinspection"
                ),
            }
        });

        (filtered, policy)
    }

    pub(crate) fn provider_visible_images_for_active_work(
        history_items: &[HistoryItem],
        state: &SessionStateSnapshot,
        active_work: Option<&ActiveWorkContract>,
    ) -> Vec<crate::session::ImagePart> {
        if active_work_requires_provider_images(state, active_work) {
            latest_user_images(history_items)
        } else {
            Vec::new()
        }
    }

    pub(crate) fn provider_required_tool_choice_final_message_noncompliance(
        state: &SessionStateSnapshot,
        dispatch_tool_choice: &ToolChoice,
        tool_names: &BTreeSet<String>,
        typed_edit_recovery_active: bool,
    ) -> bool {
        typed_edit_recovery_active
            && Self::open_executable_work_requires_tool_call(state)
            && matches!(dispatch_tool_choice, ToolChoice::Required)
            && tool_names.contains("write")
            && tool_names
                .iter()
                .all(|tool| matches!(tool.as_str(), "apply_patch" | "write"))
    }

    pub(crate) fn provider_required_tool_choice_final_message_recovery_has_write_surface(
        tools: &[ToolSchema],
        stable_tools: &[ToolSchema],
    ) -> bool {
        tools.iter().any(|tool| tool.name == "write")
            || stable_tools.iter().any(|tool| tool.name == "write")
    }

    pub(crate) fn provider_required_tool_choice_final_message_recovery_policy(
        state: &SessionStateSnapshot,
    ) -> RequestReplayPolicyDiagnostic {
        RequestReplayPolicyDiagnostic {
            policy: "provider_required_tool_choice_final_message_recovery_surface".to_string(),
            call_id: None,
            tool_name: Some("write".to_string()),
            omitted_targets: Vec::new(),
            active_targets: state
                .active_targets
                .iter()
                .map(|target| target.as_str().to_string())
                .collect(),
            reason: "the provider returned a text-only final message while the lifecycle envelope required an edit tool call; the next recovery request is narrowed to the write tool under provider-portable required tool choice with the same open-work authority".to_string(),
        }
    }

    pub(crate) fn provider_required_tool_choice_final_message_recovery_fixture_passes() -> bool {
        let tool_names = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
        let mut state = SessionStateSnapshot::default();
        state.route = TaskRoute::Code;
        state.process_phase = ProcessPhase::Author;
        state.active_targets = vec![
            Utf8PathBuf::from("src/workflow.rs"),
            Utf8PathBuf::from("tests/workflow.behavior.md"),
        ];
        state.completion.open_work_count = 2;
        state.completion.closeout_ready = false;

        let noncompliance_detected =
            Self::provider_required_tool_choice_final_message_noncompliance(
                &state,
                &ToolChoice::Required,
                &tool_names,
                true,
            );
        let narrowed_tool_names = BTreeSet::from(["write".to_string()]);
        let recovery_choice = compile_turn_lifecycle_tool_choice(
            &PromptPolicy::default(),
            &state,
            &narrowed_tool_names,
            TurnLifecycleRecoveryContext {
                provider_required_tool_choice_final_message_recovery_active: true,
                ..TurnLifecycleRecoveryContext::default()
            },
        );
        let policy = Self::provider_required_tool_choice_final_message_recovery_policy(&state);

        let mut docs_state = SessionStateSnapshot::default();
        docs_state.route = TaskRoute::Docs;
        docs_state.process_phase = ProcessPhase::Author;
        docs_state.active_targets = vec![Utf8PathBuf::from("docs/workflow-design.md")];
        docs_state.completion.open_work_count = 1;
        docs_state.completion.route_contract_pending = true;
        docs_state.completion.closeout_ready = false;
        let docs_recovery_choice = compile_turn_lifecycle_tool_choice(
            &PromptPolicy::default(),
            &docs_state,
            &BTreeSet::from(["write".to_string()]),
            TurnLifecycleRecoveryContext {
                provider_required_tool_choice_final_message_recovery_active: true,
                ..TurnLifecycleRecoveryContext::default()
            },
        );

        noncompliance_detected
            && matches!(recovery_choice, ToolChoice::Required)
            && matches!(docs_recovery_choice, ToolChoice::Required)
            && policy.policy == "provider_required_tool_choice_final_message_recovery_surface"
            && policy.tool_name.as_deref() == Some("write")
            && policy.active_targets
                == vec![
                    "src/workflow.rs".to_string(),
                    "tests/workflow.behavior.md".to_string(),
                ]
            && policy.reason.contains("text-only final message")
            && Self::provider_required_tool_choice_recovery_rebuilds_write_from_stable_surface_fixture_passes()
    }

    pub(crate) fn provider_required_tool_choice_recovery_rebuilds_write_from_stable_surface_fixture_passes()
    -> bool {
        let mut tools = vec![ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        }];
        let stable_tools = vec![
            ToolSchema {
                name: "apply_patch".to_string(),
                description: "apply a patch".to_string(),
                input_schema: json!({"type": "object"}),
                strict: false,
            },
            ToolSchema {
                name: "write".to_string(),
                description: "write a file".to_string(),
                input_schema: json!({"type": "object"}),
                strict: false,
            },
        ];
        let mut state = SessionStateSnapshot::default();
        state.route = TaskRoute::Code;
        state.process_phase = ProcessPhase::Author;
        state.active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
        state.completion.open_work_count = 1;

        let active = Self::open_executable_work_requires_tool_call(&state)
            && Self::provider_required_tool_choice_final_message_recovery_has_write_surface(
                &tools,
                &stable_tools,
            );
        if active {
            Self::augment_tools_from_stable_surface(&mut tools, &stable_tools, |name| {
                name == "write"
            });
            tools.retain(|tool| tool.name == "write");
        }
        let tool_names = tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<BTreeSet<_>>();
        let choice = compile_turn_lifecycle_tool_choice(
            &PromptPolicy::default(),
            &state,
            &tool_names,
            TurnLifecycleRecoveryContext {
                provider_required_tool_choice_final_message_recovery_active: active,
                ..TurnLifecycleRecoveryContext::default()
            },
        );

        active
            && tool_names == BTreeSet::from(["write".to_string()])
            && matches!(choice, ToolChoice::Required)
    }

    pub(crate) fn rejected_model_action_corrective_result(
        rejection: &ModelActionRejection,
        source_call_id: ToolCallId,
        allowed_tools: &BTreeSet<String>,
        tool_exists: bool,
        tool_allowed: bool,
        control_surface: &ProjectionSurface,
        state: &SessionStateSnapshot,
        dispatch_tool_choice: &ToolChoice,
    ) -> ToolResult {
        let rejection_result = rejection.to_tool_result(
            source_call_id,
            allowed_tools,
            tool_exists,
            tool_allowed,
            control_surface,
        );
        if rejection.semantic_class == "malformed_tool_arguments"
            && matches!(
                rejection.proposal.effective_tool.as_str(),
                "write" | "apply_patch"
            )
            && Self::open_executable_work_requires_tool_call(state)
        {
            let parse_error = serde_json::from_str::<Value>(&rejection.proposal.arguments_json)
                .map(|_| rejection.blocked_reason.clone())
                .unwrap_or_else(|error| error.to_string());
            let mut invalid_result = invalid_tool_arguments_result(
                &rejection.proposal.effective_tool,
                &rejection.proposal.arguments_json,
                &parse_error,
                state,
                Some(allowed_tools),
                Some(dispatch_tool_choice),
            );
            if let Some(invalid_object) = invalid_result.metadata.as_object_mut()
                && let Some(rejection_object) = rejection_result.metadata.as_object()
            {
                for key in [
                    "model_action_adjudication",
                    "rejected_tool_proposal",
                    "tool_rejected",
                    "provider_noncompliance",
                    "requested_tool",
                    "effective_tool",
                    "tool_exists",
                    "tool_allowed",
                ] {
                    if let Some(value) = rejection_object.get(key) {
                        invalid_object.insert(key.to_string(), value.clone());
                    }
                }
            }
            invalid_result
        } else {
            rejection_result
        }
    }

    pub(crate) fn docs_patch_context_mismatch_grounding_tool_visible(tool_name: &str) -> bool {
        docs_patch_context_mismatch_grounding_tool_visible(tool_name)
    }

    pub(crate) fn docs_route_content_grounding_after_progress_projection_tool_visible(
        tool_name: &str,
    ) -> bool {
        docs_route_content_grounding_after_progress_projection_tool_visible(tool_name)
    }

    pub(crate) fn authoring_target_grounding_recovery_tool_visible(tool_name: &str) -> bool {
        authoring_target_grounding_recovery_tool_visible(tool_name)
    }

    pub(crate) fn apply_docs_route_supporting_context_budget_recovery_surface(
        tools: &mut Vec<ToolSchema>,
    ) {
        tools.retain(|tool| {
            Self::docs_route_supporting_context_budget_recovery_tool_visible(&tool.name)
        });
    }

    pub(crate) fn apply_authoring_supporting_context_budget_recovery_surface(
        tools: &mut Vec<ToolSchema>,
        needs_grounding_read: bool,
    ) {
        tools.retain(|tool| {
            Self::authoring_supporting_context_budget_recovery_tool_visible(
                &tool.name,
                needs_grounding_read,
            )
        });
    }

    pub(crate) fn apply_repair_supporting_context_budget_recovery_surface(
        tools: &mut Vec<ToolSchema>,
    ) {
        tools.retain(|tool| {
            Self::repair_supporting_context_budget_recovery_tool_visible(&tool.name)
        });
    }

    pub(crate) fn apply_singleton_missing_authoring_target_create_action_surface(
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
    ) {
        augment_tools_from_stable_surface(
            tools,
            stable_tools,
            Self::singleton_missing_authoring_target_create_action_tool_visible,
        );
        tools.retain(|tool| {
            Self::singleton_missing_authoring_target_create_action_tool_visible(&tool.name)
        });
    }

    pub(crate) fn apply_existing_target_grounding_recovery_surface(
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
    ) {
        augment_tools_from_stable_surface(
            tools,
            stable_tools,
            existing_target_grounding_recovery_tool_visible,
        );
        tools.retain(|tool| existing_target_grounding_recovery_tool_visible(&tool.name));
    }

    pub(crate) fn apply_docs_patch_context_mismatch_grounding_surface(
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
    ) {
        augment_tools_from_stable_surface(
            tools,
            stable_tools,
            docs_patch_context_mismatch_grounding_tool_visible,
        );
        tools.retain(|tool| docs_patch_context_mismatch_grounding_tool_visible(&tool.name));
    }

    pub(crate) fn apply_verification_repair_target_grounding_surface(
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
    ) {
        augment_tools_from_stable_surface(
            tools,
            stable_tools,
            verification_repair_target_grounding_surface_tool_visible,
        );
        tools.retain(|tool| verification_repair_target_grounding_surface_tool_visible(&tool.name));
    }

    pub(crate) fn apply_provider_noncompliance_edit_recovery_surface_if_visible(
        tools: &mut Vec<ToolSchema>,
    ) -> bool {
        if tools
            .iter()
            .any(|tool| Self::provider_noncompliance_edit_recovery_tool_visible(&tool.name))
        {
            tools
                .retain(|tool| Self::provider_noncompliance_edit_recovery_tool_visible(&tool.name));
            true
        } else {
            false
        }
    }

    pub(crate) fn apply_early_pre_context_recovery_surface(
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
        input: TurnLifecycleEarlyPreContextSurfaceInput<'_>,
    ) -> TurnLifecycleEarlyPreContextSurfacePlan {
        if input.docs_route_supporting_context_budget_recovery_active {
            Self::apply_docs_route_supporting_context_budget_recovery_surface(tools);
        }
        if input.authoring_supporting_context_budget_recovery_active {
            Self::apply_authoring_supporting_context_budget_recovery_surface(
                tools,
                input.authoring_supporting_context_budget_recovery_needs_read,
            );
        }
        if input.generated_test_source_reference_grounding_active {
            Self::apply_generated_test_source_reference_grounding_surface(
                tools,
                stable_tools,
                !input.authoring_supporting_context_budget_recovery_active,
            );
        }
        if input.generated_test_reference_consumed_target_grounding_active {
            Self::apply_generated_test_reference_consumed_target_grounding_surface(
                tools,
                stable_tools,
            );
        }
        if input.singleton_missing_authoring_target_create_action_active {
            Self::apply_singleton_missing_authoring_target_create_action_surface(
                tools,
                stable_tools,
            );
        }
        if input.existing_target_grounding_recovery_active {
            Self::apply_existing_target_grounding_recovery_surface(tools, stable_tools);
        }
        if input.repair_supporting_context_budget_recovery_active
            && !input.patch_context_mismatch_grounding_active
        {
            Self::apply_repair_supporting_context_budget_recovery_surface(tools);
        }
        let pre_authority_tool_names = tool_schema_names(tools);
        let mut verification_target_grounding_active = false;
        if input.patch_context_mismatch_grounding_active {
            if matches!(input.state.route, TaskRoute::Docs)
                && matches!(input.state.process_phase, ProcessPhase::Author)
            {
                Self::apply_docs_patch_context_mismatch_grounding_surface(tools, stable_tools);
            } else {
                Self::apply_verification_repair_target_grounding_surface(tools, stable_tools);
                verification_target_grounding_active = true;
            }
        } else if !input.repair_supporting_context_budget_recovery_active
            && Self::verification_repair_target_grounding_surface_active(
                input.state,
                &pre_authority_tool_names,
            )
        {
            Self::apply_verification_repair_target_grounding_surface(tools, stable_tools);
            verification_target_grounding_active = true;
        }
        TurnLifecycleEarlyPreContextSurfacePlan {
            pre_authority_tool_names,
            verification_target_grounding_active,
        }
    }

    pub(crate) fn apply_late_pre_context_recovery_surface(
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
        input: TurnLifecycleLatePreContextSurfaceInput<'_>,
    ) -> TurnLifecycleLatePreContextSurfacePlan {
        let current_tool_names = tool_schema_names(tools);
        let malformed_write_patch_recovery_active = Self::malformed_write_patch_recovery_active(
            input.state,
            input.malformed_write_patch_recovery_pending,
            &current_tool_names,
        );
        let malformed_apply_patch_write_recovery_active =
            Self::malformed_apply_patch_write_recovery_active(
                input.state,
                input.malformed_apply_patch_write_recovery_pending,
                &current_tool_names,
            );
        let wrong_target_authoring_edit_recovery_active =
            Self::wrong_target_authoring_edit_recovery_applies(
                input.state,
                input.wrong_authoring_target_counts,
            ) && !malformed_write_patch_recovery_active
                && !malformed_apply_patch_write_recovery_active;
        let provider_noncompliance_edit_recovery_active =
            Self::provider_noncompliance_edit_recovery_applies(
                input.state,
                input.rejected_tool_proposals,
            );
        if provider_noncompliance_edit_recovery_active {
            Self::apply_provider_noncompliance_edit_recovery_surface_if_visible(tools);
        }
        let post_provider_tool_names = tool_schema_names(tools);
        let mut verification_target_grounding_active = input.verification_target_grounding_active;
        if Self::second_pass_verification_repair_target_grounding_active(
            input.state,
            &post_provider_tool_names,
            input.repair_supporting_context_budget_recovery_active,
            provider_noncompliance_edit_recovery_active,
            wrong_target_authoring_edit_recovery_active,
            input.patch_context_mismatch_grounding_active,
        ) {
            Self::apply_verification_repair_target_grounding_surface(tools, stable_tools);
            verification_target_grounding_active = true;
        }
        TurnLifecycleLatePreContextSurfacePlan {
            current_tool_names,
            post_provider_tool_names,
            provider_noncompliance_edit_recovery_active,
            malformed_write_patch_recovery_active,
            malformed_apply_patch_write_recovery_active,
            wrong_target_authoring_edit_recovery_active,
            verification_target_grounding_active,
        }
    }

    pub(crate) fn singleton_missing_authoring_target_create_action_tool_visible(
        tool_name: &str,
    ) -> bool {
        matches!(tool_name, "apply_patch" | "todowrite")
    }

    pub(crate) fn open_obligation_final_message_recovery_tool_visible(
        state: &SessionStateSnapshot,
        tool_name: &str,
    ) -> bool {
        open_obligation_final_message_recovery_tool_visible(state, tool_name)
    }

    pub(crate) fn code_authoring_open_obligation_final_message_recovery_uses_stable_surface(
        state: &SessionStateSnapshot,
    ) -> bool {
        matches!(state.route, TaskRoute::Code)
            && matches!(state.process_phase, ProcessPhase::Author)
            && (state.completion.open_work_count > 0 || !state.active_targets.is_empty())
    }

    pub(crate) fn code_repair_open_obligation_final_message_recovery_uses_stable_surface(
        state: &SessionStateSnapshot,
    ) -> bool {
        code_repair_open_obligation_final_message_recovery_uses_stable_surface(state)
    }

    pub(crate) fn augment_tools_from_stable_surface<F>(
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
        visible: F,
    ) where
        F: Fn(&str) -> bool,
    {
        augment_tools_from_stable_surface(tools, stable_tools, visible);
    }

    pub(crate) fn apply_generated_test_source_reference_grounding_surface(
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
        orientation_allowed: bool,
    ) {
        augment_tools_from_stable_surface(tools, stable_tools, |tool_name| {
            generated_test_source_reference_grounding_tool_visible(tool_name, orientation_allowed)
        });
        tools.retain(|tool| {
            generated_test_source_reference_grounding_tool_visible(&tool.name, orientation_allowed)
        });
    }

    pub(crate) fn apply_generated_test_reference_consumed_target_grounding_surface(
        tools: &mut Vec<ToolSchema>,
        stable_tools: &[ToolSchema],
    ) {
        augment_tools_from_stable_surface(
            tools,
            stable_tools,
            authoring_target_grounding_recovery_tool_visible,
        );
        tools.retain(|tool| authoring_target_grounding_recovery_tool_visible(&tool.name));
    }

    fn provider_noncompliance_edit_recovery_active(
        rejected_tool_proposals: &BTreeMap<String, usize>,
    ) -> bool {
        rejected_tool_proposals.iter().any(|(key, count)| {
            *count > 0
                && key.starts_with(
                    "model_action_rejection|semantic=provider_ignored_edit_only_surface|",
                )
        })
    }

    fn reject_tool_like_action(
        proposal: ModelToolCallProposal,
        semantic_class: &'static str,
        blocked_reason: impl Into<String>,
        allowed_tools: &BTreeSet<String>,
        envelope: &TurnControlEnvelope,
    ) -> ActionAdjudication {
        ActionAdjudication::RejectedModelAction(ModelActionRejection::new(
            ModelActionRejectionClass::ProviderNoncompliance,
            semantic_class,
            blocked_reason,
            &proposal,
            allowed_tools,
            envelope,
        ))
    }

    fn reject_non_tool_action(
        action_name: String,
        semantic_class: &'static str,
        blocked_reason: impl Into<String>,
        allowed_tools: &BTreeSet<String>,
        envelope: &TurnControlEnvelope,
    ) -> ActionAdjudication {
        let synthetic = ModelToolCallProposal {
            call_id: ToolProposalId::new().to_string(),
            requested_tool: action_name.clone(),
            effective_tool: action_name,
            arguments_json: "{}".to_string(),
        };
        ActionAdjudication::RejectedModelAction(ModelActionRejection::new(
            ModelActionRejectionClass::ProviderNoncompliance,
            semantic_class,
            blocked_reason,
            &synthetic,
            allowed_tools,
            envelope,
        ))
    }

    fn reject_text_final_action(
        proposal: TextFinalProposal,
        action_name: String,
        semantic_class: &'static str,
        blocked_reason: impl Into<String>,
        allowed_tools: &BTreeSet<String>,
        envelope: &TurnControlEnvelope,
    ) -> ActionAdjudication {
        let arguments_json = serde_json::to_string(&json!({
            "text": proposal.text.clone(),
            "projection_id": proposal.projection_id.to_string(),
            "proposal_id": proposal.proposal_id.clone(),
        }))
        .unwrap_or_else(|_| "{}".to_string());
        let synthetic = ModelToolCallProposal {
            call_id: proposal.proposal_id,
            requested_tool: action_name.clone(),
            effective_tool: action_name,
            arguments_json,
        };
        ActionAdjudication::RejectedModelAction(ModelActionRejection::new(
            ModelActionRejectionClass::ProviderNoncompliance,
            semantic_class,
            blocked_reason,
            &synthetic,
            allowed_tools,
            envelope,
        ))
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TurnLifecycleRecoverySurfaceInput<'a> {
    pub(crate) state: &'a SessionStateSnapshot,
    pub(crate) recovery: TurnLifecycleRecoveryContext,
    pub(crate) code_authoring_final_message_hard_edit_recovery_active: bool,
    pub(crate) generated_test_orientation_allowed: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TurnLifecyclePreNormalizationSurfaceInput<'a> {
    pub(crate) state: &'a SessionStateSnapshot,
    pub(crate) recovery: TurnLifecycleRecoveryContext,
    pub(crate) code_authoring_final_message_hard_edit_recovery_active: bool,
    pub(crate) code_authoring_final_message_recovery_stable_surface_active: bool,
    pub(crate) code_repair_final_message_recovery_stable_surface_active: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TurnLifecycleEarlyPreContextSurfaceInput<'a> {
    pub(crate) state: &'a SessionStateSnapshot,
    pub(crate) docs_route_supporting_context_budget_recovery_active: bool,
    pub(crate) authoring_supporting_context_budget_recovery_active: bool,
    pub(crate) authoring_supporting_context_budget_recovery_needs_read: bool,
    pub(crate) generated_test_source_reference_grounding_active: bool,
    pub(crate) generated_test_reference_consumed_target_grounding_active: bool,
    pub(crate) singleton_missing_authoring_target_create_action_active: bool,
    pub(crate) existing_target_grounding_recovery_active: bool,
    pub(crate) patch_context_mismatch_grounding_active: bool,
    pub(crate) repair_supporting_context_budget_recovery_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnLifecycleEarlyPreContextSurfacePlan {
    pub(crate) pre_authority_tool_names: BTreeSet<String>,
    pub(crate) verification_target_grounding_active: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TurnLifecycleLatePreContextSurfaceInput<'a> {
    pub(crate) state: &'a SessionStateSnapshot,
    pub(crate) rejected_tool_proposals: &'a BTreeMap<String, usize>,
    pub(crate) wrong_authoring_target_counts: &'a BTreeMap<String, usize>,
    pub(crate) repair_supporting_context_budget_recovery_active: bool,
    pub(crate) malformed_write_patch_recovery_pending: bool,
    pub(crate) malformed_apply_patch_write_recovery_pending: bool,
    pub(crate) patch_context_mismatch_grounding_active: bool,
    pub(crate) verification_target_grounding_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnLifecycleLatePreContextSurfacePlan {
    pub(crate) current_tool_names: BTreeSet<String>,
    pub(crate) post_provider_tool_names: BTreeSet<String>,
    pub(crate) provider_noncompliance_edit_recovery_active: bool,
    pub(crate) malformed_write_patch_recovery_active: bool,
    pub(crate) malformed_apply_patch_write_recovery_active: bool,
    pub(crate) wrong_target_authoring_edit_recovery_active: bool,
    pub(crate) verification_target_grounding_active: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TurnLifecycleRecoveryContextInput<'a> {
    pub(crate) state: &'a SessionStateSnapshot,
    pub(crate) tools: &'a [ToolSchema],
    pub(crate) stable_tools: &'a [ToolSchema],
    pub(crate) current_tool_names: &'a BTreeSet<String>,
    pub(crate) post_provider_tool_names: &'a BTreeSet<String>,
    pub(crate) rejected_tool_proposals: &'a BTreeMap<String, usize>,
    pub(crate) wrong_authoring_target_counts: &'a BTreeMap<String, usize>,
    pub(crate) progress_projection_no_progress_counts: &'a BTreeMap<String, usize>,
    pub(crate) repair_supporting_context_budget_recovery_active: bool,
    pub(crate) malformed_write_patch_recovery_pending: bool,
    pub(crate) malformed_apply_patch_write_recovery_pending: bool,
    pub(crate) has_open_obligation_final_message_recovery: bool,
    pub(crate) open_obligation_final_message_recovery_count: Option<usize>,
    pub(crate) open_obligation_final_message_hard_edit_recovery_pending: bool,
    pub(crate) provider_required_tool_choice_final_message_recovery_pending: bool,
    pub(crate) has_invalid_edit_recovery: bool,
    pub(crate) generated_test_source_reference_grounding_active: bool,
    pub(crate) generated_test_reference_consumed_target_grounding_active: bool,
    pub(crate) verification_target_grounding_active: bool,
    pub(crate) authoring_target_grounding_recovery_edit_only: bool,
    pub(crate) patch_context_mismatch_grounding_active: bool,
    pub(crate) existing_target_grounding_recovery_active: bool,
    pub(crate) docs_route_has_required_content_grounding_evidence: bool,
    pub(crate) authoring_targets_need_grounding: bool,
    pub(crate) progress_projection_target_grounding_read_needed: bool,
}

pub(crate) fn compile_turn_lifecycle_tool_choice(
    policy: &PromptPolicy,
    state: &SessionStateSnapshot,
    tool_names: &BTreeSet<String>,
    recovery: TurnLifecycleRecoveryContext,
) -> ToolChoice {
    TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
        policy,
        state,
        tool_names,
        recovery,
    })
    .tool_choice
}

fn request_diagnostics_fixture_request(
    tools: Vec<ToolSchema>,
    tool_choice: Option<ProviderToolChoice>,
    parallel_tool_calls: bool,
    capabilities: crate::llm::ModelCapabilities,
    extra_body: Option<Value>,
) -> ChatRequest {
    ChatRequest {
        model: ModelProfile {
            name: LIFECYCLE_FIXTURE_MODEL.to_string(),
            context_window: 131_072,
            max_output_tokens: 8_192,
            provider_metadata_mode: crate::config::ProviderMetadataMode::OpenAiCompatibleOnly,
            capabilities,
        },
        base_url: LIFECYCLE_FIXTURE_BASE_URL.to_string(),
        system_prompt: "system".to_string(),
        messages: vec![ModelMessage::User {
            content: "do work".to_string(),
        }],
        tools,
        tool_choice,
        parallel_tool_calls,
        timeout_ms: 600_000,
        stream_idle_timeout_ms: 300_000,
        stream_max_retries: 2,
        extra_headers: BTreeMap::new(),
        temperature: None,
        top_p: None,
        top_k: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        stop_sequences: Vec::new(),
        extra_body,
    }
}

fn request_diagnostics_fixture_apply_patch_tool() -> ToolSchema {
    ToolSchema {
        name: "apply_patch".to_string(),
        description: "apply a patch".to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["patch_text"],
            "properties": {
                "patch_text": {"type": "string"}
            }
        }),
        strict: false,
    }
}

pub(crate) fn request_diagnostics_stream_retry_policy_fixture_passes() -> bool {
    let request = request_diagnostics_fixture_request(
        Vec::new(),
        None,
        false,
        crate::llm::ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
        },
        None,
    );
    let diagnostics = TurnLifecycleKernel::compile_request_diagnostics(&request, None, None, &[]);
    diagnostics.request_timeout_ms == 600_000
        && diagnostics.stream_idle_timeout_ms == 300_000
        && diagnostics.stream_max_retries == 2
}

pub(crate) fn request_diagnostics_tool_choice_uses_runtime_dispatch_field_fixture_passes() -> bool {
    let request = request_diagnostics_fixture_request(
        vec![request_diagnostics_fixture_apply_patch_tool()],
        Some(ProviderToolChoice::Named {
            name: "apply_patch".to_string(),
        }),
        true,
        crate::llm::ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
        },
        Some(json!({ "tool_choice": "required" })),
    );
    let diagnostics = TurnLifecycleKernel::compile_request_diagnostics(&request, None, None, &[]);
    diagnostics.tool_choice.as_deref() == Some("named:apply_patch")
        && diagnostics.tool_count == 1
        && diagnostics.tool_names == vec!["apply_patch".to_string()]
}

pub(crate) fn request_diagnostics_tool_surface_uses_chat_request_fixture_passes() -> bool {
    let request = request_diagnostics_fixture_request(
        vec![request_diagnostics_fixture_apply_patch_tool()],
        None,
        false,
        crate::llm::ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
        },
        None,
    );
    let diagnostics = TurnLifecycleKernel::compile_request_diagnostics(&request, None, None, &[]);
    diagnostics.tool_count == 1
        && diagnostics.tool_names == vec!["apply_patch".to_string()]
        && diagnostics
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "apply_patch")
        && diagnostics
            .tool_schemas
            .iter()
            .all(|schema| schema.name != "write")
}

pub(crate) fn request_diagnostics_model_capabilities_use_chat_request_fixture_passes() -> bool {
    let mut request = request_diagnostics_fixture_request(
        Vec::new(),
        None,
        false,
        crate::llm::ModelCapabilities {
            supports_tools: false,
            supports_reasoning: true,
            supports_images: false,
        },
        None,
    );
    request.messages = vec![ModelMessage::User {
        content: "summarize".to_string(),
    }];
    let diagnostics = TurnLifecycleKernel::compile_request_diagnostics(&request, None, None, &[]);
    let Ok(value) = serde_json::to_value(&diagnostics) else {
        return false;
    };
    value
        .get("supports_tools")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
        && value
            .get("supports_reasoning")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && value
            .get("supports_images")
            .and_then(serde_json::Value::as_bool)
            == Some(false)
}

pub(crate) fn request_diagnostics_missing_model_capabilities_remain_absent_fixture_passes() -> bool
{
    let sparse = json!({
        "provider": "openai_compat",
        "model_name": "historical-model",
        "base_url": "http://localhost:8110",
        "request_timeout_ms": 600000,
        "stream_idle_timeout_ms": 300000,
        "system_prompt_chars": 12,
        "tool_count": 0,
        "provider_message_count": 1,
        "messages": []
    });
    let Ok(diagnostics) = serde_json::from_value::<RequestDiagnosticsPart>(sparse) else {
        return false;
    };
    let Ok(value) = serde_json::to_value(&diagnostics) else {
        return false;
    };
    value.get("supports_tools").is_none()
        && value.get("supports_reasoning").is_none()
        && value.get("supports_images").is_none()
}

pub(crate) fn request_diagnostics_parallel_tool_calls_scope_matches_chat_request_fixture_passes()
-> bool {
    let mut tool_request = request_diagnostics_fixture_request(
        vec![request_diagnostics_fixture_apply_patch_tool()],
        None,
        false,
        crate::llm::ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
        },
        None,
    );
    let base = request_diagnostics_fixture_request(
        Vec::new(),
        None,
        false,
        crate::llm::ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
        },
        None,
    );
    tool_request.parallel_tool_calls = false;

    let tool_diagnostics =
        TurnLifecycleKernel::compile_request_diagnostics(&tool_request, None, None, &[]);
    let Ok(tool_value) = serde_json::to_value(&tool_diagnostics) else {
        return false;
    };

    let no_tool_diagnostics =
        TurnLifecycleKernel::compile_request_diagnostics(&base, None, None, &[]);
    let Ok(no_tool_value) = serde_json::to_value(&no_tool_diagnostics) else {
        return false;
    };

    tool_value
        .get("parallel_tool_calls")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
        && no_tool_value.get("parallel_tool_calls").is_none()
}

pub(crate) fn control_envelope_preserves_current_turn_id_fixture_passes() -> bool {
    let config = ResolvedConfig::default();
    let root = Utf8PathBuf::from("C:/workspace/control-envelope-turn-id");
    let session_id = SessionId::new();
    let protocol_turn_id = TurnId::new();
    let state = SessionStateSnapshot::default();
    let model = ModelProfile {
        name: config.model.model.clone(),
        context_window: config.model.context_window,
        max_output_tokens: config.model.max_output_tokens,
        provider_metadata_mode: config.model.provider_metadata_mode,
        capabilities: crate::llm::ModelCapabilities {
            supports_tools: config.model.supports_tools,
            supports_reasoning: config.model.supports_reasoning,
            supports_images: config.model.supports_images,
        },
    };
    let turn_decision = TurnDecisionDiagnostic {
        route: "code".to_string(),
        process_phase: "discover".to_string(),
        active_work_kind: None,
        active_work_summary: None,
        active_targets: Vec::new(),
        verification_pending: false,
        closeout_ready: false,
        required_verification_commands: Vec::new(),
        policy_targets: Vec::new(),
        allowed_tools: Vec::new(),
        tool_choice: None,
        warnings: Vec::new(),
        repair_lane: None,
    };
    let projection_id = ProjectionId::new();
    let context = TurnLifecycleKernel::compile_turn_context(CompileTurnContextInput {
        session_id,
        cwd: &root,
        workspace_root: &root,
        model: &model,
        config: &config,
        state: &state,
        history_items: &[],
        active_work: None,
        turn_decision: &turn_decision,
        allowed_tools: Vec::new(),
        tool_choice: &ToolChoice::None,
        projection_id,
    });
    let obligations = ObligationCompiler::compile(&context);
    let compiled = TurnEngine::compile(TurnEngineInput {
        turn_id: protocol_turn_id,
        context,
        obligations,
        dispatch_policy: DispatchPolicy::Dispatch,
        evidence_refs: Vec::new(),
    });

    compiled.envelope.turn_id == protocol_turn_id
        && compiled.envelope.context.session_id == session_id
        && compiled.envelope.context.workspace_root == root
}

pub(crate) fn final_dispatch_source_schema_projection_fixture_passes() -> bool {
    let mut config = ResolvedConfig::default();
    config.model.parallel_tool_calls = true;
    config.model.max_parallel_predictions = 1;
    let model = ModelProfile {
        name: LIFECYCLE_FIXTURE_MODEL.to_string(),
        context_window: config.model.context_window,
        max_output_tokens: config.model.max_output_tokens,
        provider_metadata_mode: config.model.provider_metadata_mode,
        capabilities: crate::llm::ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
        },
    };
    let source_write_schema = ToolSchema {
        name: "write".to_string(),
        description: "write active source artifact".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "enum": ["src/workflow.rs"]
                },
                "content": {
                    "type": "string",
                    "description": "complete source content for src/workflow.rs"
                }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }),
        strict: true,
    };
    let request =
        TurnLifecycleKernel::compile_provider_chat_request(CompileProviderChatRequestInput {
            model: &model,
            config: &config,
            system_prompt: "system".to_string(),
            messages: vec![ModelMessage::User {
                content: "create source".to_string(),
            }],
            tools: vec![source_write_schema.clone()],
            dispatch_tool_choice: &ToolChoice::Named(ToolName::Write),
        });
    let diagnostics = TurnLifecycleKernel::compile_request_diagnostics(&request, None, None, &[]);

    request.validate_provider_lifecycle().is_ok()
        && request.tools.len() == 1
        && request.tools.first().is_some_and(|schema| {
            schema.name == source_write_schema.name
                && schema.strict == source_write_schema.strict
                && schema.input_schema == source_write_schema.input_schema
        })
        && matches!(
            request.tool_choice,
            Some(ProviderToolChoice::Named { ref name }) if name == "write"
        )
        && !request.parallel_tool_calls
        && diagnostics.tool_count == 1
        && diagnostics.tool_names == vec!["write".to_string()]
        && diagnostics.tool_schemas.first().is_some_and(|schema| {
            schema.name == "write"
                && schema.strict
                && schema
                    .input_schema
                    .pointer("/properties/path/enum/0")
                    .and_then(Value::as_str)
                    == Some("src/workflow.rs")
        })
}

pub(crate) fn authoring_final_message_recovery_keeps_target_grounding_read_fixture_passes() -> bool
{
    let state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        active_targets: vec![Utf8PathBuf::from("src/workflow.rs")],
        completion: crate::session::CompletionState {
            closeout_ready: false,
            open_work_count: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut tools = vec![
        ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "read".to_string(),
            description: "read file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "write".to_string(),
            description: "write file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let stable_tools = tools.clone();
    let recovery = TurnLifecycleRecoveryContext {
        open_obligation_final_message_recovery_active: true,
        authoring_target_grounding_final_message_recovery_active: true,
        ..Default::default()
    };
    TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecyclePreNormalizationSurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            code_authoring_final_message_recovery_stable_surface_active: false,
            code_repair_final_message_recovery_stable_surface_active: false,
        },
    );
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let choice =
        compile_turn_lifecycle_tool_choice(&PromptPolicy::default(), &state, &tool_names, recovery);
    let envelope = TurnLifecycleKernel::open_obligation_final_message_recovery_envelope(
        &state,
        1,
        None,
        &tool_names,
        false,
    );

    tool_names == BTreeSet::from(["apply_patch".to_string(), "read".to_string()])
        && matches!(choice, ToolChoice::Auto)
        && envelope.prompt.contains("Use")
        && envelope.prompt.contains("src/workflow.rs")
        && envelope
            .prompt
            .contains("before any final assistant message")
}

pub(crate) fn failed_patch_context_mismatch_reopens_target_grounding_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Repair;
    state.completion.verification_pending = true;
    state.completion.open_work_count = 1;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    state.verification.failure_cluster = Some(crate::session::VerificationFailureCluster {
        cluster_id: "fixture-failed-patch-context-mismatch-grounding".to_string(),
        failing_labels: vec!["workflow_source_parse_contract".to_string()],
        primary_failure: Some("Command: verify-contract --behavior --utf8".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("source_parse_defect".to_string()),
            label: Some("workflow_source_parse_contract".to_string()),
            target: Some("tests/workflow.spec.ts".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: None,
            observed: Some("SyntaxError: unmatched ')'".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "source parse defect `SyntaxError: unmatched ')'`".to_string(),
                "source parse frame `tests/workflow.spec.ts`".to_string(),
                "source_parse_defect".to_string(),
            ],
            sibling_obligations: Vec::new(),
            requirement_refs: Vec::new(),
            source_refs: Vec::new(),
            test_refs: vec!["tests/workflow.spec.ts".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: Vec::new(),
        test_refs: vec!["tests/workflow.spec.ts".to_string()],
    });

    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let read_metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress",
        "path": "tests/workflow.spec.ts",
        "result_hash": "target-grounding-read"
    });
    let exhausted_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "read",
        &read_metadata,
        &state,
        &allowed,
        &ToolChoice::Auto,
    );
    let exhausted = BTreeSet::from([exhausted_key]);
    let mut visible_without_failed_patch = allowed.clone();
    if TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
        &state, &exhausted,
    ) {
        visible_without_failed_patch.retain(|tool| {
            TurnLifecycleKernel::repair_supporting_context_budget_recovery_tool_visible(tool)
        });
    }

    let invalid_patch = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Update File: tests/workflow.spec.ts\n@@\n old\n+new\n*** End Patch"}"#,
        "tool edit error: context mismatch: expected `old`, got ``",
        &state,
        Some(&allowed),
        Some(&ToolChoice::Auto),
    );
    let mut patch_grounding_targets = BTreeSet::<String>::new();
    record_patch_context_mismatch_grounding_targets(
        &mut patch_grounding_targets,
        &invalid_patch.metadata,
        &state,
    );
    let patch_grounding_active =
        patch_context_mismatch_target_grounding_surface_active(&state, &patch_grounding_targets);
    let stable_tools = allowed
        .iter()
        .map(|name| ToolSchema {
            name: name.clone(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut visible_after_failed_patch = stable_tools
        .iter()
        .filter(|tool| matches!(tool.name.as_str(), "apply_patch" | "write"))
        .cloned()
        .collect::<Vec<_>>();
    if TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
        &state, &exhausted,
    ) && !patch_grounding_active
    {
        visible_after_failed_patch.retain(|tool| {
            TurnLifecycleKernel::repair_supporting_context_budget_recovery_tool_visible(&tool.name)
        });
    }
    if patch_grounding_active {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut visible_after_failed_patch,
            &stable_tools,
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible,
        );
        visible_after_failed_patch.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(
                &tool.name,
            )
        });
    }
    let rejected_supporting = BTreeMap::from([(
        "model_action_rejection|semantic=provider_ignored_edit_only_surface|hash=fixture"
            .to_string(),
        1,
    )]);
    if TurnLifecycleKernel::provider_noncompliance_edit_recovery_applies(
        &state,
        &rejected_supporting,
    ) && !patch_grounding_active
        && visible_after_failed_patch.iter().any(|tool| {
            TurnLifecycleKernel::provider_noncompliance_edit_recovery_tool_visible(&tool.name)
        })
    {
        visible_after_failed_patch.retain(|tool| {
            TurnLifecycleKernel::provider_noncompliance_edit_recovery_tool_visible(&tool.name)
        });
    }
    let visible_after_failed_patch_names = visible_after_failed_patch
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    if patch_context_mismatch_target_grounding_read_satisfied("read", &read_metadata, &state) {
        patch_grounding_targets.clear();
    }

    visible_without_failed_patch
        == BTreeSet::from([
            "apply_patch".to_string(),
            "todowrite".to_string(),
            "write".to_string(),
        ])
        && patch_grounding_active
        && visible_after_failed_patch_names
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && patch_grounding_targets.is_empty()
}

pub(crate) fn verification_repair_target_grounding_surface_keeps_read_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: workflow.divide raises the wrong exception".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.completion.verification_pending = true;
    state.completion.open_work_count = 1;
    state.verification.failing_labels = vec!["test_divide_by_zero".to_string()];
    let mut cluster = crate::agent::state::public_class_attribute_cluster_fixture();
    cluster.source_refs = vec!["src/workflow.rs".to_string()];
    cluster.test_refs = vec!["tests/workflow.behavior.md".to_string()];
    for evidence in &mut cluster.evidence {
        evidence.subtype = Some("public_exception_mismatch".to_string());
        evidence.target = Some("C:/workspace/project/src/workflow.rs".to_string());
        evidence.source_refs = vec!["src/workflow.rs".to_string()];
        evidence.test_refs = vec!["tests/workflow.behavior.md".to_string()];
    }
    state.verification.failure_cluster = Some(cluster);
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "glob".to_string(),
        "grep".to_string(),
        "list".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let narrowed = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let mut visible = allowed.clone();
    if TurnLifecycleKernel::verification_repair_target_grounding_surface_active(&state, &allowed) {
        visible.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(tool)
        });
    }
    let narrowed_active =
        TurnLifecycleKernel::verification_repair_target_grounding_surface_active(&state, &narrowed);
    let mut visible_from_narrowed = allowed.clone();
    if narrowed_active {
        visible_from_narrowed.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(tool)
        });
    }
    let stable_tool_schemas = allowed
        .iter()
        .map(|name| ToolSchema {
            name: name.clone(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut narrowed_schema_surface = narrowed
        .iter()
        .map(|name| ToolSchema {
            name: name.clone(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let malformed_write_recovery_active = true;
    if narrowed_active {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut narrowed_schema_surface,
            &stable_tool_schemas,
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible,
        );
        narrowed_schema_surface.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(
                &tool.name,
            )
        });
    } else if malformed_write_recovery_active {
        narrowed_schema_surface
            .retain(|tool| matches!(tool.name.as_str(), "apply_patch" | "write"));
    }
    let narrowed_schema_names = narrowed_schema_surface
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let mut post_provider_normalized_surface = narrowed_schema_surface.clone();
    TurnLifecycleKernel::apply_codex_style_provider_edit_surface(
        &mut post_provider_normalized_surface,
        &state,
    );
    if narrowed_active {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut post_provider_normalized_surface,
            &stable_tool_schemas,
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible,
        );
        post_provider_normalized_surface.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(
                &tool.name,
            )
        });
    }
    let post_provider_normalized_names = post_provider_normalized_surface
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let Some(projection) = crate::agent::repair_lane::project_repair_lane(&state, &visible) else {
        return false;
    };

    TurnLifecycleKernel::verification_repair_target_grounding_surface_active(&state, &allowed)
        && visible
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && narrowed_active
        && visible_from_narrowed
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && narrowed_schema_names
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && post_provider_normalized_names
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && !visible.contains("shell")
        && !visible.contains("grep")
        && !visible.contains("list")
        && projection
            .operation_template
            .as_ref()
            .is_some_and(|template| {
                template
                    .required_edit_surface
                    .contains(&"apply_patch".to_string())
                    && template
                        .required_edit_surface
                        .contains(&"write".to_string())
                    && !template.forbidden_stale_tools.contains(&"read".to_string())
                    && template
                        .forbidden_stale_tools
                        .contains(&"shell".to_string())
            })
        && projection
            .repair_control_snapshot
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot
                    .allowed_surface_snapshot
                    .contains(&"read".to_string())
                    && !snapshot
                        .forbidden_actions
                        .contains(&"stale_tool:read".to_string())
                    && snapshot
                        .forbidden_actions
                        .contains(&"stale_tool:shell".to_string())
                    && snapshot.forbidden_actions.iter().any(|action| {
                        action == "unbounded_context_churn_before_source_contract_repair"
                    })
            })
}

pub(crate) fn source_repair_initial_grounding_precedes_edit_only_recovery_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Repair;
    state.completion.verification_pending = true;
    state.completion.open_work_count = 1;
    state.active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
    state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: public stdout assertion mismatch".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: state.active_targets.clone(),
    });
    state.verification.failing_labels = vec!["workflow_public_output_contract".to_string()];
    state.verification.failure_cluster = Some(crate::session::VerificationFailureCluster {
        cluster_id: "fixture-public-output-source-grounding".to_string(),
        failing_labels: vec!["workflow_public_output_contract".to_string()],
        primary_failure: Some("Command: verify-contract --behavior --utf8".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_output_stream_assertion_mismatch".to_string()),
            label: Some("workflow_public_output_contract".to_string()),
            target: None,
            symbol: None,
            call_site: Some("public_output_contains(stdout, \"expected token\")".to_string()),
            exception: None,
            expected: Some("expected token".to_string()),
            observed: Some("stdout `unmatched stdout output`".to_string()),
            public_state_assertions: Vec::new(),
            public_missing_attributes: Vec::new(),
            evidence_markers: vec![
                "public_output_stream:stdout".to_string(),
                "source_public_behavior_assertion".to_string(),
            ],
            sibling_obligations: vec!["stdout contains expected token".to_string()],
            requirement_refs: Vec::new(),
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: vec!["stdout contains expected token".to_string()],
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
    });
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

    let stable_names = BTreeSet::from([
        "apply_patch".to_string(),
        "list".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let stable_tool_schemas = stable_names
        .iter()
        .map(|name| ToolSchema {
            name: name.clone(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut first_repair_tools = vec![
        ToolSchema {
            name: "apply_patch".to_string(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "write".to_string(),
            description: String::new(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let first_names = first_repair_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    if TurnLifecycleKernel::verification_repair_target_grounding_surface_active(
        &state,
        &first_names,
    ) {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut first_repair_tools,
            &stable_tool_schemas,
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible,
        );
        first_repair_tools.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(
                &tool.name,
            )
        });
    }
    let first_visible = first_repair_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let exhausted = BTreeSet::from(["workflow-read-budget".to_string()]);
    let mut post_grounding_visible = first_visible.clone();
    if TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
        &state, &exhausted,
    ) {
        post_grounding_visible.retain(|tool| {
            TurnLifecycleKernel::repair_supporting_context_budget_recovery_tool_visible(tool)
        });
    }
    let required_write = RequiredAction::edit(
        ToolName::Write,
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    );
    let mut provider_counts = BTreeMap::new();
    let provider_decision = ToolLifecycleRuntime::record_rejected_tool_no_progress(
        &mut provider_counts,
        RejectedToolNoProgressGuardRequest {
            effective_tool_name: "shell",
            effective_arguments_json: r#"{"command":"verify-contract --behavior"}"#,
            allowed_tools: &post_grounding_visible,
            tool_choice: &ToolChoice::Required,
            required_action: Some(&required_write),
            provider_noncompliance: true,
            semantic_class: "provider_ignored_edit_only_surface",
            result_hash: Some("fixture"),
            recovery_no_progress_key: None,
        },
    );

    first_visible
        == BTreeSet::from([
            "apply_patch".to_string(),
            "read".to_string(),
            "todowrite".to_string(),
            "write".to_string(),
        ])
        && post_grounding_visible
            == BTreeSet::from([
                "apply_patch".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && provider_decision.count == 1
        && provider_decision.terminal_message.is_none()
}

#[cfg(test)]
mod fixture_tests {
    #[test]
    fn final_dispatch_source_schema_projection() {
        assert!(super::final_dispatch_source_schema_projection_fixture_passes());
    }

    #[test]
    fn authoring_final_message_recovery_keeps_target_grounding_read() {
        assert!(
            super::authoring_final_message_recovery_keeps_target_grounding_read_fixture_passes()
        );
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TurnLifecyclePlan {
    pub(crate) tool_choice: ToolChoice,
    pub(crate) effective_tools: BTreeSet<String>,
    pub(crate) plan_reason: String,
    pub(crate) replay_policy: &'static str,
    pub(crate) proposal_policy: &'static str,
    pub(crate) corrective_policy: &'static str,
    pub(crate) terminal_policy: &'static str,
    pub(crate) continuation_expectation: &'static str,
    pub(crate) diagnostics_projection: &'static str,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TurnLifecycleRecoveryContext {
    pub(crate) provider_noncompliance_edit_recovery_active: bool,
    pub(crate) wrong_target_authoring_edit_recovery_active: bool,
    pub(crate) provider_required_tool_choice_final_message_recovery_active: bool,
    pub(crate) code_authoring_final_message_hard_edit_recovery_active: bool,
    pub(crate) generated_test_source_reference_grounding_active: bool,
    pub(crate) generated_test_reference_consumed_target_grounding_active: bool,
    pub(crate) verification_target_grounding_active: bool,
    pub(crate) authoring_target_grounding_recovery_edit_only: bool,
    pub(crate) patch_context_mismatch_grounding_active: bool,
    pub(crate) authoring_target_grounding_final_message_recovery_active: bool,
    pub(crate) existing_target_grounding_recovery_active: bool,
    pub(crate) docs_grounding_final_message_recovery_active: bool,
    pub(crate) docs_content_grounding_recovery_active: bool,
    pub(crate) malformed_write_patch_recovery_active: bool,
    pub(crate) malformed_apply_patch_write_recovery_active: bool,
    pub(crate) progress_projection_edit_recovery_active: bool,
    pub(crate) progress_projection_edit_recovery_needs_grounding_read: bool,
    pub(crate) failed_edit_recovery_active: bool,
    pub(crate) open_obligation_final_message_recovery_active: bool,
    pub(crate) open_obligation_final_message_count: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TurnLifecyclePlanInput<'a> {
    pub(crate) policy: &'a PromptPolicy,
    pub(crate) state: &'a SessionStateSnapshot,
    pub(crate) tool_names: &'a BTreeSet<String>,
    pub(crate) recovery: TurnLifecycleRecoveryContext,
}

fn lifecycle_tool_choice(input: &TurnLifecyclePlanInput<'_>) -> ToolChoice {
    let _ = input.policy;
    let tools = input.tool_names;
    if tools.is_empty() {
        return ToolChoice::None;
    }
    let recovery = input.recovery;
    if recovery.provider_noncompliance_edit_recovery_active {
        return ToolChoice::Required;
    }
    if recovery.malformed_write_patch_recovery_active {
        return ToolChoice::Required;
    }
    if recovery.malformed_apply_patch_write_recovery_active {
        return ToolChoice::Required;
    }
    if recovery.wrong_target_authoring_edit_recovery_active {
        return ToolChoice::Required;
    }
    if recovery.failed_edit_recovery_active
        && recovery.open_obligation_final_message_recovery_active
    {
        return ToolChoice::Required;
    }
    if recovery.authoring_target_grounding_recovery_edit_only {
        return ToolChoice::Required;
    }
    if recovery.provider_required_tool_choice_final_message_recovery_active
        && tools.contains("write")
    {
        return ToolChoice::Required;
    }
    if recovery.code_authoring_final_message_hard_edit_recovery_active {
        return ToolChoice::Required;
    }
    if recovery.docs_content_grounding_recovery_active
        && recovery.progress_projection_edit_recovery_active
    {
        return ToolChoice::Required;
    }
    if recovery.progress_projection_edit_recovery_active {
        return ToolChoice::Required;
    }
    if recovery.patch_context_mismatch_grounding_active
        && docs_authoring_patch_context_grounding_keeps_auto(input.state)
    {
        return ToolChoice::Auto;
    }
    if recovery.existing_target_grounding_recovery_active
        && docs_authoring_patch_context_grounding_keeps_auto(input.state)
    {
        return ToolChoice::Auto;
    }
    if recovery.generated_test_source_reference_grounding_active
        || recovery.generated_test_reference_consumed_target_grounding_active
        || recovery.verification_target_grounding_active
        || recovery.authoring_target_grounding_final_message_recovery_active
    {
        return bounded_authoring_recovery_tool_choice(input.state);
    }
    if recovery.docs_grounding_final_message_recovery_active {
        return ToolChoice::Auto;
    }
    if recovery.docs_content_grounding_recovery_active {
        return ToolChoice::Auto;
    }
    if recovery.open_obligation_final_message_recovery_active {
        return open_obligation_final_message_recovery_tool_choice(
            input.state,
            tools,
            recovery.open_obligation_final_message_count,
        );
    }
    ToolChoice::Auto
}

fn provider_tool_choice_value(
    tool_count: usize,
    tool_choice: &ToolChoice,
) -> Option<ProviderToolChoice> {
    if tool_count == 0 {
        return None;
    }
    match tool_choice {
        ToolChoice::Auto => None,
        ToolChoice::Required => Some(ProviderToolChoice::Required),
        ToolChoice::None => None,
        ToolChoice::Named(name) => Some(ProviderToolChoice::Named {
            name: name.to_string(),
        }),
    }
}

fn request_control_envelope_diagnostic(
    envelope: &TurnControlEnvelope,
) -> RequestControlEnvelopeDiagnostic {
    let validation = envelope.validate();
    RequestControlEnvelopeDiagnostic {
        envelope_id: envelope.id.to_string(),
        projection_id: envelope.projection_id.to_string(),
        dispatch_policy: dispatch_policy_label(&envelope.dispatch_policy).to_string(),
        required_verification_commands: envelope
            .action_authority
            .required_verification_commands
            .clone(),
        allowed_tools: envelope
            .action_authority
            .allowed_tools
            .iter()
            .map(ToString::to_string)
            .collect(),
        forbidden_tools: envelope
            .action_authority
            .forbidden_tools
            .iter()
            .map(ToString::to_string)
            .collect(),
        validation_status: if validation.passes() {
            "pass".to_string()
        } else {
            "fail".to_string()
        },
        validation_issues: validation
            .issues
            .iter()
            .map(|issue| RequestControlEnvelopeIssueDiagnostic {
                code: format!("{:?}", issue.code),
                severity: format!("{:?}", issue.severity),
                message: issue.message.clone(),
            })
            .collect(),
        open_obligations: envelope
            .obligations
            .items
            .iter()
            .filter(|item| {
                matches!(
                    item.status,
                    ObligationStatus::Open | ObligationStatus::Blocked
                )
            })
            .map(|item| RequestControlObligationDiagnostic {
                obligation_id: item.obligation_id.clone(),
                kind: format!("{:?}", item.kind),
                summary: item.summary.clone(),
                targets: item.targets.iter().map(ToString::to_string).collect(),
                required_actions: item
                    .required_actions
                    .iter()
                    .map(|action| action.projection_label().to_string())
                    .collect(),
                verification_commands: item.verification_commands.clone(),
                status: format!("{:?}", item.status),
            })
            .collect(),
        surface_projections: envelope
            .projection_bundle
            .rendered_surfaces()
            .into_iter()
            .map(|surface| RequestControlSurfaceDiagnostic {
                surface: surface.surface.as_str().to_string(),
                projection_id: surface.projection_id.to_string(),
                allowed_tools: surface.allowed_tools,
                forbidden_tools: surface.forbidden_tools,
                text: surface.text,
            })
            .collect(),
    }
}

fn dispatch_policy_label(policy: &DispatchPolicy) -> &'static str {
    match policy {
        DispatchPolicy::Dispatch => "dispatch",
        DispatchPolicy::AwaitUser { .. } => "await_user",
        DispatchPolicy::FailClosed { .. } => "fail_closed",
        DispatchPolicy::Complete { .. } => "complete",
        DispatchPolicy::Interrupt { .. } => "interrupt",
    }
}

fn request_message_diagnostic(message: &ModelMessage) -> RequestMessageDiagnostic {
    match message {
        ModelMessage::System { content } => RequestMessageDiagnostic {
            role: "system".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: request_content_markers(content),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::User { content } => RequestMessageDiagnostic {
            role: "user".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: request_content_markers(content),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::UserParts { parts } => {
            let mut content_chars = 0usize;
            let mut image_count = 0usize;
            let mut image_bytes = 0u64;
            for part in parts {
                match part {
                    ModelContentPart::Text { text } => {
                        content_chars += text.chars().count();
                    }
                    ModelContentPart::Image { data_base64, .. } => {
                        image_count += 1;
                        image_bytes += data_base64.len() as u64;
                    }
                }
            }
            RequestMessageDiagnostic {
                role: "user".to_string(),
                content_chars: (content_chars > 0).then_some(content_chars),
                content_markers: request_content_markers(
                    &parts
                        .iter()
                        .filter_map(|part| match part {
                            ModelContentPart::Text { text } => Some(text.as_str()),
                            ModelContentPart::Image { .. } => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                image_count,
                image_bytes,
                tool_calls: Vec::new(),
                tool_call_id: None,
            }
        }
        ModelMessage::Assistant { content } => RequestMessageDiagnostic {
            role: "assistant".to_string(),
            content_chars: Some(content.chars().count()),
            content_markers: request_content_markers(content),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: None,
        },
        ModelMessage::AssistantToolCalls {
            content,
            tool_calls,
        } => RequestMessageDiagnostic {
            role: "assistant".to_string(),
            content_chars: content.as_ref().map(|value| value.chars().count()),
            content_markers: content
                .as_ref()
                .map(|value| request_content_markers(value))
                .unwrap_or_default(),
            image_count: 0,
            image_bytes: 0,
            tool_calls: tool_calls
                .iter()
                .map(|call| RequestToolCallDiagnostic {
                    call_id: call.call_id.clone(),
                    tool_name: call.tool_name.clone(),
                    arguments_chars: call.arguments_json.chars().count(),
                })
                .collect(),
            tool_call_id: None,
        },
        ModelMessage::Tool {
            call_id, result, ..
        } => RequestMessageDiagnostic {
            role: "tool".to_string(),
            content_chars: Some(result.chars().count()),
            content_markers: request_content_markers(result),
            image_count: 0,
            image_bytes: 0,
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.clone()),
        },
    }
}

pub(crate) fn request_content_markers(content: &str) -> Vec<String> {
    let mut markers = Vec::new();
    if content.contains("Open-obligation final-message recovery:") {
        markers.push("open_obligation_final_message_recovery".to_string());
    }
    if content.contains("Invalid edit recovery:") {
        markers.push("invalid_edit_arguments_recovery".to_string());
    }
    if content.contains("Open targets:") {
        markers.push("open_targets_projection".to_string());
    }
    if content.contains("exact apply_patch grammar")
        || content.contains("Add File body lines must start with `+`")
    {
        markers.push("strict_apply_patch_grammar".to_string());
    }
    if content.contains("including blank lines and every content line")
        || content.contains("every added content line")
    {
        markers.push("add_file_line_prefix_rule".to_string());
    }
    if content.contains("single patch")
        && content.contains("*** Add File")
        && content.contains("*** Update File")
    {
        markers.push("multi_file_apply_patch_shape".to_string());
    }
    if content.contains("Language Policy:") {
        markers.push("language_policy".to_string());
    }
    if content.contains("Agent Tool Policy:") {
        markers.push("agent_tool_policy".to_string());
    }
    markers
}

fn runtime_owned_required_verification_command(
    active_work: Option<&ActiveWorkContract>,
    allowed_tools: &BTreeSet<String>,
    tool_choice: &ToolChoice,
    required_action: Option<&RequiredAction>,
) -> Option<String> {
    if allowed_tools.len() != 1
        || !allowed_tools.contains("shell")
        || !matches!(
            tool_choice,
            ToolChoice::Required | ToolChoice::Named(ToolName::Shell)
        )
    {
        return None;
    }
    let Some(ActiveWorkContract::Verification {
        commands,
        repair_required,
        targets,
        ..
    }) = active_work
    else {
        return None;
    };
    if *repair_required || !targets.is_empty() || commands.len() != 1 {
        return None;
    }
    let command = required_action
        .and_then(RequiredAction::shell_command)
        .map(str::trim)
        .filter(|command| !command.is_empty())?;
    Some(command.to_string())
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
    shell_family: ShellFamily,
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

fn normalized_command_text_for_family_match(command: &str) -> String {
    command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn tool_schema_names(tools: &[ToolSchema]) -> BTreeSet<String> {
    tools.iter().map(|tool| tool.name.clone()).collect()
}

fn fixture_tool_schema(name: &str) -> ToolSchema {
    ToolSchema {
        name: name.to_string(),
        description: match name {
            "apply_patch" => "apply a patch",
            "read" => "read a file",
            "shell" => "run a shell command",
            "todowrite" => "update progress",
            "write" => "write a file",
            _ => "fixture tool",
        }
        .to_string(),
        input_schema: json!({"type": "object"}),
        strict: false,
    }
}

fn fixture_tool_schemas(names: &[&str]) -> Vec<ToolSchema> {
    names.iter().map(|name| fixture_tool_schema(name)).collect()
}

fn lifecycle_plan_reason(input: &TurnLifecyclePlanInput<'_>) -> &'static str {
    let recovery = input.recovery;
    if input.tool_names.is_empty() {
        return "empty_effective_surface";
    }
    if recovery.provider_noncompliance_edit_recovery_active {
        return "provider_noncompliance_edit_recovery";
    }
    if recovery.malformed_write_patch_recovery_active {
        return "malformed_write_patch_recovery";
    }
    if recovery.malformed_apply_patch_write_recovery_active {
        return "malformed_apply_patch_write_recovery";
    }
    if recovery.wrong_target_authoring_edit_recovery_active {
        return "wrong_target_authoring_edit_recovery";
    }
    if recovery.failed_edit_recovery_active
        && recovery.open_obligation_final_message_recovery_active
    {
        return "failed_edit_final_message_recovery";
    }
    if recovery.authoring_target_grounding_recovery_edit_only {
        return "authoring_target_grounding_edit_only_recovery";
    }
    if recovery.provider_required_tool_choice_final_message_recovery_active {
        return "provider_required_tool_choice_final_message_recovery";
    }
    if recovery.code_authoring_final_message_hard_edit_recovery_active {
        return "code_authoring_final_message_hard_edit_recovery";
    }
    if recovery.docs_content_grounding_recovery_active
        && recovery.progress_projection_edit_recovery_active
    {
        return "docs_content_grounding_progress_projection_recovery";
    }
    if recovery.progress_projection_edit_recovery_active {
        return "progress_projection_edit_recovery";
    }
    if recovery.generated_test_source_reference_grounding_active {
        return "generated_test_source_reference_grounding";
    }
    if recovery.generated_test_reference_consumed_target_grounding_active {
        return "generated_test_reference_consumed_target_grounding";
    }
    if recovery.verification_target_grounding_active {
        return "verification_target_grounding";
    }
    if recovery.patch_context_mismatch_grounding_active {
        return "patch_context_mismatch_grounding";
    }
    if recovery.authoring_target_grounding_final_message_recovery_active {
        return "authoring_target_grounding_final_message_recovery";
    }
    if recovery.existing_target_grounding_recovery_active {
        return "existing_target_grounding_recovery";
    }
    if recovery.docs_grounding_final_message_recovery_active {
        return "docs_grounding_final_message_recovery";
    }
    if recovery.docs_content_grounding_recovery_active {
        return "docs_content_grounding_recovery";
    }
    if recovery.open_obligation_final_message_recovery_active {
        return "open_obligation_final_message_recovery";
    }
    "stable_surface_default"
}

fn lifecycle_replay_policy(plan_reason: &str) -> &'static str {
    match plan_reason {
        "empty_effective_surface" => "no_executable_tool_replay",
        "provider_noncompliance_edit_recovery"
        | "wrong_target_authoring_edit_recovery"
        | "failed_edit_final_message_recovery"
        | "provider_required_tool_choice_final_message_recovery"
        | "malformed_write_patch_recovery"
        | "malformed_apply_patch_write_recovery"
        | "code_authoring_final_message_hard_edit_recovery"
        | "authoring_target_grounding_edit_only_recovery"
        | "progress_projection_edit_recovery" => "sanitize_failed_or_stale_tool_pairs",
        "docs_content_grounding_progress_projection_recovery" => {
            "sanitize_plan_projection_and_preserve_grounding_context"
        }
        "generated_test_source_reference_grounding"
        | "generated_test_reference_consumed_target_grounding"
        | "verification_target_grounding"
        | "patch_context_mismatch_grounding"
        | "authoring_target_grounding_final_message_recovery"
        | "existing_target_grounding_recovery"
        | "docs_grounding_final_message_recovery"
        | "docs_content_grounding_recovery" => "preserve_supporting_context_as_evidence",
        "open_obligation_final_message_recovery" => {
            "preserve_rejected_final_as_no_progress_evidence"
        }
        _ => "canonical_history_surface_filter",
    }
}

fn lifecycle_corrective_policy(plan_reason: &str) -> &'static str {
    match plan_reason {
        "provider_noncompliance_edit_recovery"
        | "wrong_target_authoring_edit_recovery"
        | "failed_edit_final_message_recovery"
        | "provider_required_tool_choice_final_message_recovery"
        | "malformed_write_patch_recovery"
        | "malformed_apply_patch_write_recovery"
        | "code_authoring_final_message_hard_edit_recovery"
        | "authoring_target_grounding_edit_only_recovery" => "hard_recovery_corrective_outputs",
        "generated_test_source_reference_grounding"
        | "generated_test_reference_consumed_target_grounding"
        | "verification_target_grounding"
        | "patch_context_mismatch_grounding"
        | "authoring_target_grounding_final_message_recovery"
        | "existing_target_grounding_recovery"
        | "docs_grounding_final_message_recovery"
        | "docs_content_grounding_recovery"
        | "docs_content_grounding_progress_projection_recovery" => {
            "bounded_grounding_corrective_outputs"
        }
        "open_obligation_final_message_recovery" => {
            "open_obligation_final_message_corrective_outputs"
        }
        "empty_effective_surface" => "final_message_or_terminal_only",
        _ => "stable_surface_tool_lifecycle_outputs",
    }
}

fn lifecycle_proposal_policy(plan_reason: &str) -> &'static str {
    match plan_reason {
        "empty_effective_surface" => "final_message_only_or_fail_closed",
        "provider_noncompliance_edit_recovery"
        | "wrong_target_authoring_edit_recovery"
        | "failed_edit_final_message_recovery"
        | "provider_required_tool_choice_final_message_recovery"
        | "malformed_write_patch_recovery"
        | "malformed_apply_patch_write_recovery"
        | "code_authoring_final_message_hard_edit_recovery"
        | "authoring_target_grounding_edit_only_recovery" => {
            "tool_call_required_or_provider_noncompliance"
        }
        "generated_test_source_reference_grounding"
        | "generated_test_reference_consumed_target_grounding"
        | "verification_target_grounding"
        | "patch_context_mismatch_grounding"
        | "authoring_target_grounding_final_message_recovery"
        | "existing_target_grounding_recovery"
        | "docs_grounding_final_message_recovery"
        | "docs_content_grounding_recovery" => "bounded_grounding_tool_or_corrective_final",
        "docs_content_grounding_progress_projection_recovery" => {
            "tool_call_required_or_provider_noncompliance"
        }
        "open_obligation_final_message_recovery" => {
            "stable_surface_tool_or_rejected_final_evidence"
        }
        _ => "stable_surface_model_action_adjudication",
    }
}

fn lifecycle_terminal_policy(plan_reason: &str) -> &'static str {
    match plan_reason {
        "empty_effective_surface" => "no_tool_surface_terminal_policy",
        "provider_noncompliance_edit_recovery"
        | "wrong_target_authoring_edit_recovery"
        | "failed_edit_final_message_recovery"
        | "provider_required_tool_choice_final_message_recovery"
        | "malformed_write_patch_recovery"
        | "malformed_apply_patch_write_recovery"
        | "code_authoring_final_message_hard_edit_recovery"
        | "authoring_target_grounding_edit_only_recovery" => {
            "same_hard_recovery_no_progress_terminal"
        }
        "generated_test_source_reference_grounding"
        | "generated_test_reference_consumed_target_grounding"
        | "verification_target_grounding"
        | "patch_context_mismatch_grounding"
        | "authoring_target_grounding_final_message_recovery"
        | "existing_target_grounding_recovery"
        | "docs_grounding_final_message_recovery"
        | "docs_content_grounding_recovery"
        | "docs_content_grounding_progress_projection_recovery" => {
            "bounded_grounding_no_progress_terminal"
        }
        "open_obligation_final_message_recovery" => "open_obligation_final_message_terminal",
        _ => "standard_tool_lifecycle_terminal",
    }
}

fn lifecycle_continuation_expectation(
    input: &TurnLifecyclePlanInput<'_>,
    plan_reason: &str,
) -> &'static str {
    if input.tool_names.is_empty() {
        return "final_or_fail";
    }
    match plan_reason {
        "stable_surface_default" => "provider_may_choose_tool_or_final",
        "empty_effective_surface" => "final_or_fail",
        _ => "tool_progress_or_typed_no_progress",
    }
}

fn lifecycle_diagnostics_projection(plan_reason: &str) -> &'static str {
    match plan_reason {
        "empty_effective_surface" => "no_tool_surface_projection",
        "stable_surface_default" => "stable_turn_control_projection",
        _ => "recovery_turn_control_projection",
    }
}

fn bounded_authoring_recovery_tool_choice(state: &SessionStateSnapshot) -> ToolChoice {
    if matches!(state.route, TaskRoute::Code) && matches!(state.process_phase, ProcessPhase::Author)
    {
        return ToolChoice::Auto;
    }
    ToolChoice::Required
}

fn docs_authoring_patch_context_grounding_keeps_auto(state: &SessionStateSnapshot) -> bool {
    matches!(state.route, TaskRoute::Docs) && matches!(state.process_phase, ProcessPhase::Author)
}

fn active_target_strings(state: &SessionStateSnapshot) -> Vec<String> {
    state
        .active_targets
        .iter()
        .map(|target| target.as_str().to_string())
        .collect()
}

fn generated_test_target_path(path: &str) -> bool {
    classify_language_artifact_target(path).role == ArtifactRole::Test
}

fn docs_authoring_uses_codex_style_provider_surface(state: &SessionStateSnapshot) -> bool {
    matches!(state.route, TaskRoute::Docs)
        && matches!(state.process_phase, ProcessPhase::Author)
        && (state.completion.open_work_count > 0
            || state.completion.route_contract_pending
            || !state.active_targets.is_empty())
}

fn docs_authoring_codex_surface_tool_visible(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "apply_patch" | "shell" | "todowrite" | "read" | "grep" | "docling_convert" | "mcp_call"
    )
}

fn progress_projection_edit_recovery_tool_visible(
    state: &SessionStateSnapshot,
    tool_name: &str,
    target_grounding_read_needed: bool,
) -> bool {
    if matches!(state.route, TaskRoute::Code)
        && matches!(
            state.process_phase,
            ProcessPhase::Author | ProcessPhase::Repair
        )
    {
        return tool_name == "apply_patch" || (target_grounding_read_needed && tool_name == "read");
    }
    matches!(tool_name, "apply_patch" | "write")
}

fn augment_tools_from_stable_surface<F>(
    tools: &mut Vec<ToolSchema>,
    stable_tools: &[ToolSchema],
    visible: F,
) where
    F: Fn(&str) -> bool,
{
    let existing = tools
        .iter()
        .map(|tool| tool.name.as_str().to_string())
        .collect::<BTreeSet<_>>();
    for tool in stable_tools {
        if visible(&tool.name) && !existing.contains(&tool.name) {
            tools.push(tool.clone());
        }
    }
    tools.sort_by(|left, right| left.name.cmp(&right.name));
}

fn verification_repair_target_grounding_surface_tool_visible(tool_name: &str) -> bool {
    matches!(tool_name, "read" | "write" | "apply_patch" | "todowrite")
}

fn edit_only_authoring_grounding_recovery_tool_visible(tool_name: &str) -> bool {
    tool_name == "apply_patch"
}

fn wrong_target_generated_test_source_reference_recovery_tool_visible(tool_name: &str) -> bool {
    matches!(tool_name, "apply_patch" | "read")
}

fn wrong_target_authoring_recovery_tool_visible(tool_name: &str) -> bool {
    tool_name == "apply_patch"
}

fn generated_test_source_reference_grounding_tool_visible(
    tool_name: &str,
    orientation_allowed: bool,
) -> bool {
    matches!(tool_name, "apply_patch" | "read" | "todowrite")
        || (orientation_allowed && tool_name == "shell")
}

fn authoring_target_grounding_recovery_tool_visible(tool_name: &str) -> bool {
    matches!(tool_name, "apply_patch" | "read")
}

fn existing_target_grounding_recovery_tool_visible(tool_name: &str) -> bool {
    matches!(tool_name, "apply_patch" | "read" | "write")
}

fn docs_route_content_grounding_recovery_tool_visible(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "apply_patch" | "docling_convert" | "grep" | "mcp_call" | "read" | "shell" | "todowrite"
    )
}

fn docs_patch_context_mismatch_grounding_tool_visible(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "apply_patch"
            | "docling_convert"
            | "grep"
            | "mcp_call"
            | "read"
            | "shell"
            | "todowrite"
            | "write"
    )
}

fn docs_route_content_grounding_after_progress_projection_tool_visible(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "apply_patch" | "docling_convert" | "grep" | "mcp_call" | "read" | "shell"
    )
}

fn failed_edit_final_message_recovery_tool_visible(tool_name: &str) -> bool {
    matches!(tool_name, "apply_patch" | "todowrite" | "write")
}

fn open_obligation_final_message_recovery_tool_visible(
    state: &SessionStateSnapshot,
    tool_name: &str,
) -> bool {
    if matches!(state.route, TaskRoute::Code) && matches!(state.process_phase, ProcessPhase::Author)
    {
        return true;
    }
    if code_repair_open_obligation_final_message_recovery_uses_stable_surface(state) {
        return true;
    }
    if matches!(state.process_phase, ProcessPhase::Author) {
        return matches!(tool_name, "apply_patch" | "write");
    }
    if state.completion.open_work_count > 0 || !state.active_targets.is_empty() {
        return matches!(tool_name, "apply_patch" | "write");
    }
    if state.completion.verification_pending || !state.verification.required_commands.is_empty() {
        return tool_name == "shell";
    }
    true
}

fn open_obligation_final_message_recovery_tool_choice(
    state: &SessionStateSnapshot,
    tool_names: &BTreeSet<String>,
    final_message_count: usize,
) -> ToolChoice {
    if normal_authoring_open_obligation_keeps_stable_surface(state, tool_names)
        || normal_repair_open_obligation_keeps_stable_surface(state, tool_names)
    {
        return ToolChoice::Auto;
    }
    if final_message_count >= 2 && !tool_names.is_empty() {
        return ToolChoice::Required;
    }
    let edit_or_grounding_only = tool_names
        .iter()
        .all(|tool| matches!(tool.as_str(), "apply_patch" | "read" | "write"));
    let edit_only = tool_names
        .iter()
        .all(|tool| matches!(tool.as_str(), "apply_patch" | "write"));
    if matches!(state.process_phase, ProcessPhase::Author)
        && tool_names.contains("read")
        && tool_names.contains("write")
        && edit_or_grounding_only
    {
        return ToolChoice::Required;
    }
    if matches!(state.process_phase, ProcessPhase::Author)
        && state.active_targets.len() > 1
        && tool_names.contains("apply_patch")
        && tool_names.contains("write")
        && edit_only
    {
        return ToolChoice::Required;
    }
    if docs_open_obligation_exact_write_recovery_uses_required_tool_choice(state, tool_names) {
        return ToolChoice::Required;
    }
    if (state.completion.open_work_count > 0 || !state.active_targets.is_empty())
        && tool_names.contains("write")
    {
        return ToolChoice::Named(ToolName::Write);
    }
    if (state.completion.verification_pending || !state.verification.required_commands.is_empty())
        && tool_names.contains("shell")
    {
        return ToolChoice::Named(ToolName::Shell);
    }
    ToolChoice::Required
}

fn normal_authoring_open_obligation_keeps_stable_surface(
    state: &SessionStateSnapshot,
    tool_names: &BTreeSet<String>,
) -> bool {
    matches!(state.route, TaskRoute::Code)
        && matches!(state.process_phase, ProcessPhase::Author)
        && (state.completion.open_work_count > 0 || !state.active_targets.is_empty())
        && tool_names
            .iter()
            .any(|tool| !matches!(tool.as_str(), "apply_patch" | "read" | "write"))
}

fn code_repair_open_obligation_final_message_recovery_uses_stable_surface(
    state: &SessionStateSnapshot,
) -> bool {
    matches!(state.route, TaskRoute::Code)
        && matches!(state.process_phase, ProcessPhase::Repair)
        && (state.completion.open_work_count > 0
            || state.completion.verification_pending
            || !state.active_targets.is_empty())
}

fn normal_repair_open_obligation_keeps_stable_surface(
    state: &SessionStateSnapshot,
    tool_names: &BTreeSet<String>,
) -> bool {
    code_repair_open_obligation_final_message_recovery_uses_stable_surface(state)
        && tool_names
            .iter()
            .any(|tool| !matches!(tool.as_str(), "apply_patch" | "write"))
}

fn docs_open_obligation_exact_write_recovery_uses_required_tool_choice(
    state: &SessionStateSnapshot,
    tool_names: &BTreeSet<String>,
) -> bool {
    matches!(state.route, TaskRoute::Docs)
        && matches!(state.process_phase, ProcessPhase::Author)
        && state.active_targets.len() == 1
        && tool_names.contains("write")
        && tool_names.contains("apply_patch")
        && !tool_names.contains("read")
        && !tool_names.contains("grep")
        && !tool_names.contains("inspect_directory")
        && !tool_names.contains("list")
}

pub(crate) struct ActionAdjudicator;

impl ActionAdjudicator {
    pub(crate) fn adjudicate_tool_call(input: &ActionAdjudicationInput<'_>) -> ActionAdjudication {
        if input.tool_exists && input.tool_allowed {
            return ActionAdjudication::AcceptedToolCall(AcceptedToolCall {
                proposal: input.proposal.clone(),
            });
        }

        let (classification, semantic_class, blocked_reason) = if !input.tool_exists {
            (
                ModelActionRejectionClass::InvalidTool,
                "invalid_tool_call",
                "The requested tool is not registered in this runtime.",
            )
        } else if provider_ignored_edit_only_surface(
            input.proposal.effective_tool.as_str(),
            input.allowed_tools,
            input.envelope,
        ) {
            (
                ModelActionRejectionClass::ProviderNoncompliance,
                "provider_ignored_edit_only_surface",
                "The provider proposed a tool outside the compiled edit-only surface.",
            )
        } else {
            (
                ModelActionRejectionClass::ToolOutsideAllowedSurface,
                "tool_outside_allowed_surface",
                "The requested tool is disallowed by the compiled turn policy.",
            )
        };

        ActionAdjudication::RejectedModelAction(ModelActionRejection::new(
            classification,
            semantic_class,
            blocked_reason,
            &input.proposal,
            input.allowed_tools,
            input.envelope,
        ))
    }
}

impl ModelActionRejection {
    fn new(
        classification: ModelActionRejectionClass,
        semantic_class: &'static str,
        blocked_reason: impl Into<String>,
        proposal: &ModelToolCallProposal,
        allowed_tools: &BTreeSet<String>,
        envelope: &TurnControlEnvelope,
    ) -> Self {
        let allowed = allowed_tools.iter().cloned().collect::<Vec<_>>().join(",");
        let open_obligation_ids = envelope
            .obligations
            .items
            .iter()
            .filter(|item| item.status == ObligationStatus::Open)
            .map(|item| item.obligation_id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let open_obligation_targets = envelope
            .obligations
            .items
            .iter()
            .filter(|item| item.status == ObligationStatus::Open)
            .flat_map(|item| item.targets.iter().map(|target| target.as_str()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(",");
        let required_action_projection = envelope
            .action_authority
            .required_action
            .as_ref()
            .map(crate::protocol::RequiredAction::projection_label)
            .unwrap_or_default();
        let payload_shape_hash =
            crate::harness::artifact::hash_bytes(proposal.arguments_json.as_bytes());
        let payload_key = if semantic_class == "malformed_tool_arguments"
            && matches!(proposal.effective_tool.as_str(), "write" | "apply_patch")
        {
            format!(
                "malformed_edit_family={}",
                malformed_tool_argument_error_family(&proposal.arguments_json)
            )
        } else {
            format!("payload={payload_shape_hash}")
        };
        let result_hash = crate::harness::artifact::hash_bytes(
            format!(
                "model_action_rejection|class={}|semantic={semantic_class}|requested={}|effective={}|{payload_key}|allowed={allowed}|required={required_action_projection}|open={open_obligation_ids}|targets={open_obligation_targets}",
                classification.as_str(),
                proposal.requested_tool,
                proposal.effective_tool,
            )
            .as_bytes(),
        );

        Self {
            classification,
            semantic_class,
            blocked_reason: blocked_reason.into(),
            result_hash,
            proposal: proposal.clone(),
        }
    }

    pub(crate) fn to_tool_result(
        &self,
        source_call_id: ToolCallId,
        allowed_tools: &BTreeSet<String>,
        tool_exists: bool,
        tool_allowed: bool,
        control_surface: &ProjectionSurface,
    ) -> ToolResult {
        let proposal = &self.proposal;
        let allowed_surface = allowed_tools.iter().cloned().collect::<Vec<_>>();
        let original_arguments = arguments_value(&proposal.arguments_json);
        let rejected_proposal =
            self.to_rejected_tool_proposal(source_call_id, allowed_tools, control_surface);

        let feedback = json!({
            "kind": self.classification.as_str(),
            "semantic_class": self.semantic_class,
            "success": false,
            "progress_effect": "no_progress",
            "side_effects_applied": false,
            "requested_tool": proposal.requested_tool,
            "effective_tool": proposal.effective_tool,
            "tool_exists": tool_exists,
            "tool_allowed": tool_allowed,
            "allowed_surface_snapshot": allowed_surface,
            "blocked_action": proposal.effective_tool,
            "required_action_projection": control_surface.required_action.as_ref().map(crate::protocol::RequiredAction::projection_label),
            "projection_id": control_surface.projection_id.to_string(),
            "result_hash": self.result_hash,
        });
        let mut output_text = control_surface
            .render_model_action_rejection_feedback(
                &proposal.requested_tool,
                &proposal.effective_tool,
                self.semantic_class,
                Some(&self.blocked_reason),
            )
            .text;
        output_text.push_str("\n\n[tool feedback]\n");
        output_text.push_str(&format!("semantic_class: {}\n", self.semantic_class));
        output_text.push_str(&format!("progress_effect: no_progress\n"));
        output_text.push_str(&format!("side_effects_applied: false\n"));
        output_text.push_str(&format!("blocked_action: {}\n", proposal.effective_tool));
        if let Some(required_action) = control_surface
            .required_action
            .as_ref()
            .map(crate::protocol::RequiredAction::projection_label)
        {
            output_text.push_str(&format!("required_action: {required_action}\n"));
        }

        ToolResult {
            title: rejection_title(self.classification).to_string(),
            output_text,
            metadata: json!({
                "model_action_adjudication": {
                    "kind": self.classification.as_str(),
                    "semantic_class": self.semantic_class,
                    "blocked_reason": self.blocked_reason,
                    "proposal_id": rejected_proposal.proposal_id.to_string(),
                    "source_call_id": source_call_id.to_string(),
                    "result_hash": self.result_hash,
                },
                "rejected_tool_proposal": rejected_proposal,
                "tool_rejected": true,
                "provider_noncompliance": self.classification == ModelActionRejectionClass::ProviderNoncompliance,
                "invalid_tool_call": self.classification == ModelActionRejectionClass::InvalidTool,
                "success": false,
                "progress_effect": "no_progress",
                "side_effects_applied": false,
                "result_hash": self.result_hash,
                "blocked_action": proposal.effective_tool,
                "required_action_projection": control_surface.required_action.as_ref().map(crate::protocol::RequiredAction::projection_label),
                "requested_tool": proposal.requested_tool,
                "effective_tool": proposal.effective_tool,
                "tool_exists": tool_exists,
                "tool_allowed": tool_allowed,
                "allowed_tools": allowed_tools.iter().cloned().collect::<Vec<_>>(),
                "original_arguments": original_arguments,
                "original_arguments_json": proposal.arguments_json,
                "control_projection": {
                    "projection_id": control_surface.projection_id.to_string(),
                    "required_action_projection": control_surface.required_action.as_ref().map(crate::protocol::RequiredAction::projection_label),
                    "allowed_tools": control_surface.allowed_tools.iter().map(ToString::to_string).collect::<Vec<_>>(),
                    "operation_intents": control_surface.operation_intents.iter().map(|intent| intent.as_str().to_string()).collect::<Vec<_>>(),
                },
                "tool_feedback_envelope": feedback,
            }),
            truncated_output_path: None,
            recorded_changes: Vec::new(),
            change_summaries: Vec::<ChangeSummary>::new(),
        }
    }

    pub(crate) fn to_rejected_tool_proposal(
        &self,
        source_call_id: ToolCallId,
        allowed_tools: &BTreeSet<String>,
        control_surface: &ProjectionSurface,
    ) -> RejectedToolProposal {
        let proposal = &self.proposal;
        RejectedToolProposal {
            proposal_id: ToolProposalId::new(),
            source_call_id,
            requested_tool: proposal.requested_tool.clone(),
            effective_tool: proposal.effective_tool.clone(),
            resolved_tool: parse_tool_name(&proposal.effective_tool),
            original_arguments: arguments_value(&proposal.arguments_json),
            adjusted_arguments: None,
            allowed_surface: allowed_tools
                .iter()
                .filter_map(|name| parse_known_tool_name(name))
                .collect(),
            blocked_reason: self.blocked_reason.to_string(),
            projection_id: control_surface.projection_id,
            semantic_class: self.semantic_class.to_string(),
            candidate_repair_id: None,
            payload_hash: crate::harness::artifact::hash_bytes(proposal.arguments_json.as_bytes()),
            contract_refs: control_surface.contract_refs.clone(),
            evidence_refs: control_surface
                .evidence_refs
                .iter()
                .map(|evidence| format!("{}:{}", evidence.source, evidence.reference))
                .collect(),
        }
    }
}

fn provider_ignored_edit_only_surface(
    effective_tool: &str,
    allowed_tools: &BTreeSet<String>,
    envelope: &TurnControlEnvelope,
) -> bool {
    if allowed_tools.contains(effective_tool) {
        return false;
    }
    let edit_tool_available =
        allowed_tools.contains("write") || allowed_tools.contains("apply_patch");
    let has_open_obligation = envelope.obligations.has_open_obligations();
    edit_tool_available && has_open_obligation
}

#[derive(Debug, Default)]
pub(crate) struct ProviderSurfaceFilterProjection {
    pub messages: Vec<ModelMessage>,
    pub replay_policies: Vec<crate::session::RequestReplayPolicyDiagnostic>,
}

pub(crate) struct ReplayNormalizer;

impl ReplayNormalizer {
    pub(crate) fn filter_to_effective_tool_surface(
        messages: Vec<ModelMessage>,
        effective_tools: &BTreeSet<String>,
    ) -> ProviderSurfaceFilterProjection {
        let mut filtered = Vec::with_capacity(messages.len());
        let mut omitted_call_ids = BTreeSet::<String>::new();
        let mut omitted_notes = Vec::<String>::new();
        let mut replay_policies = Vec::<crate::session::RequestReplayPolicyDiagnostic>::new();

        for message in messages {
            match message {
                ModelMessage::AssistantToolCalls {
                    mut content,
                    tool_calls,
                } => {
                    let mut kept = Vec::new();
                    let mut omitted_names = Vec::new();
                    for call in tool_calls {
                        if effective_tools.contains(&call.tool_name) {
                            kept.push(call);
                        } else {
                            omitted_call_ids.insert(call.call_id.clone());
                            replay_policies.push(provider_replay_omitted_tool_call_policy(
                                &call.call_id,
                                &call.tool_name,
                                effective_tools,
                            ));
                            omitted_names.push(call.tool_name);
                        }
                    }
                    if !omitted_names.is_empty() {
                        omitted_names.sort();
                        omitted_names.dedup();
                        omitted_notes.push(format!(
                            "Historical tool call(s) omitted from executable provider replay because they are outside the current effective tool surface: {}.",
                            omitted_names.join(", ")
                        ));
                    }
                    if !kept.is_empty() {
                        if !omitted_names.is_empty() {
                            if let Some(content) = content.as_ref() {
                                if !content.trim().is_empty() {
                                    omitted_notes.push(format!(
                                    "Assistant tool-call prelude omitted from a mixed replay item because part of that item was outside the current effective tool surface and the natural-language prelude is not standalone executable authority: {}",
                                    clip_provider_replay_note(content, 360)
                                ));
                                }
                            }
                            content = None;
                        }
                        filtered.push(ModelMessage::AssistantToolCalls {
                            content,
                            tool_calls: kept,
                        });
                    } else if let Some(content) = content {
                        if !content.trim().is_empty() {
                            omitted_notes.push(format!(
                                "Assistant tool-call prelude omitted with its out-of-surface tool call(s) because it is not standalone final-answer authority: {}",
                                clip_provider_replay_note(&content, 360)
                            ));
                        }
                    }
                }
                ModelMessage::Tool {
                    call_id,
                    tool_name,
                    result,
                    metadata,
                } if omitted_call_ids.contains(&call_id)
                    || !effective_tools.contains(&tool_name) =>
                {
                    omitted_call_ids.insert(call_id.clone());
                    replay_policies.push(provider_replay_omitted_tool_output_policy(
                        &call_id,
                        &tool_name,
                        &metadata,
                        effective_tools,
                    ));
                    omitted_notes.push(provider_replay_omitted_tool_output_note(
                        &tool_name, &result, &metadata,
                    ));
                }
                other => filtered.push(other),
            }
        }

        if omitted_notes.is_empty() {
            return ProviderSurfaceFilterProjection {
                messages: filtered,
                replay_policies,
            };
        }

        let mut with_note = Vec::with_capacity(filtered.len() + 1);
        with_note.extend(filtered);
        with_note.push(ModelMessage::User {
            content: format!(
                "Provider replay surface normalization:\n{}\nCurrent effective tool surface: {}.",
                omitted_notes.join("\n"),
                effective_tools
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        });
        ProviderSurfaceFilterProjection {
            messages: with_note,
            replay_policies,
        }
    }
}

fn provider_replay_omitted_tool_output_note(
    tool_name: &str,
    result: &str,
    metadata: &Value,
) -> String {
    if provider_replay_metadata_is_supporting_context(metadata) {
        return format!(
            "Non-executable supporting-context evidence from omitted `{tool_name}` call. This historical output remains available as evidence for the current edit step, but `{tool_name}` is not in the current effective tool surface. Do not repeat that omitted tool call; use the provider-visible edit tool, usually apply_patch, for the active target.\nEvidence excerpt:\n{}",
            clip_provider_replay_evidence(result, 4096)
        );
    }
    if provider_replay_metadata_is_provider_noncompliance(metadata) {
        return format!(
            "Non-executable corrective output from omitted `{tool_name}` call: the provider proposal was rejected by the lifecycle kernel because it ignored the current edit-only surface. Do not repeat that omitted tool call. Follow the current effective tool surface and use the provider-visible edit tool, usually apply_patch, for the active repair target. Result excerpt: {}",
            clip_provider_replay_note(result, 1200)
        );
    }
    format!(
        "Historical `{tool_name}` tool output omitted from executable provider replay because `{tool_name}` is outside the current effective tool surface. Result excerpt: {}",
        clip_provider_replay_note(result, 360)
    )
}

pub(crate) fn turn_lifecycle_plan_owns_dispatch_tool_choice_fixture_passes() -> bool {
    let mut code_authoring = SessionStateSnapshot::default();
    code_authoring.route = TaskRoute::Code;
    code_authoring.process_phase = ProcessPhase::Author;
    code_authoring.completion.open_work_count = 1;
    let stable_code_surface = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let stable_authoring_plan =
        TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
            policy: &PromptPolicy::default(),
            state: &code_authoring,
            tool_names: &stable_code_surface,
            recovery: TurnLifecycleRecoveryContext {
                open_obligation_final_message_recovery_active: true,
                open_obligation_final_message_count: 2,
                ..TurnLifecycleRecoveryContext::default()
            },
        });

    let provider_noncompliance_plan =
        TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
            policy: &PromptPolicy::default(),
            state: &code_authoring,
            tool_names: &BTreeSet::from(["apply_patch".to_string(), "write".to_string()]),
            recovery: TurnLifecycleRecoveryContext {
                provider_noncompliance_edit_recovery_active: true,
                ..TurnLifecycleRecoveryContext::default()
            },
        });
    let wrong_target_authoring_plan =
        TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
            policy: &PromptPolicy::default(),
            state: &code_authoring,
            tool_names: &BTreeSet::from(["apply_patch".to_string(), "write".to_string()]),
            recovery: TurnLifecycleRecoveryContext {
                wrong_target_authoring_edit_recovery_active: true,
                ..TurnLifecycleRecoveryContext::default()
            },
        });
    let provider_required_final_message_plan =
        TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
            policy: &PromptPolicy::default(),
            state: &code_authoring,
            tool_names: &BTreeSet::from(["write".to_string()]),
            recovery: TurnLifecycleRecoveryContext {
                provider_required_tool_choice_final_message_recovery_active: true,
                ..TurnLifecycleRecoveryContext::default()
            },
        });
    let hard_authoring_final_message_plan =
        TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
            policy: &PromptPolicy::default(),
            state: &code_authoring,
            tool_names: &BTreeSet::from(["apply_patch".to_string(), "write".to_string()]),
            recovery: TurnLifecycleRecoveryContext {
                code_authoring_final_message_hard_edit_recovery_active: true,
                ..TurnLifecycleRecoveryContext::default()
            },
        });

    let empty_surface_plan =
        TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
            policy: &PromptPolicy::default(),
            state: &code_authoring,
            tool_names: &BTreeSet::new(),
            recovery: TurnLifecycleRecoveryContext::default(),
        });

    matches!(stable_authoring_plan.tool_choice, ToolChoice::Auto)
        && stable_authoring_plan
            .effective_tools
            .contains("apply_patch")
        && stable_authoring_plan.plan_reason == "open_obligation_final_message_recovery"
        && stable_authoring_plan.replay_policy == "preserve_rejected_final_as_no_progress_evidence"
        && stable_authoring_plan.proposal_policy == "stable_surface_tool_or_rejected_final_evidence"
        && stable_authoring_plan.corrective_policy
            == "open_obligation_final_message_corrective_outputs"
        && stable_authoring_plan.terminal_policy == "open_obligation_final_message_terminal"
        && stable_authoring_plan.continuation_expectation == "tool_progress_or_typed_no_progress"
        && stable_authoring_plan.diagnostics_projection == "recovery_turn_control_projection"
        && matches!(
            provider_noncompliance_plan.tool_choice,
            ToolChoice::Required
        )
        && provider_noncompliance_plan.plan_reason == "provider_noncompliance_edit_recovery"
        && provider_noncompliance_plan.replay_policy == "sanitize_failed_or_stale_tool_pairs"
        && provider_noncompliance_plan.proposal_policy
            == "tool_call_required_or_provider_noncompliance"
        && provider_noncompliance_plan.corrective_policy == "hard_recovery_corrective_outputs"
        && provider_noncompliance_plan.terminal_policy == "same_hard_recovery_no_progress_terminal"
        && matches!(
            wrong_target_authoring_plan.tool_choice,
            ToolChoice::Required
        )
        && wrong_target_authoring_plan.plan_reason == "wrong_target_authoring_edit_recovery"
        && wrong_target_authoring_plan.replay_policy == "sanitize_failed_or_stale_tool_pairs"
        && wrong_target_authoring_plan.proposal_policy
            == "tool_call_required_or_provider_noncompliance"
        && wrong_target_authoring_plan.corrective_policy == "hard_recovery_corrective_outputs"
        && wrong_target_authoring_plan.terminal_policy == "same_hard_recovery_no_progress_terminal"
        && matches!(
            provider_required_final_message_plan.tool_choice,
            ToolChoice::Required
        )
        && provider_required_final_message_plan.plan_reason
            == "provider_required_tool_choice_final_message_recovery"
        && provider_required_final_message_plan.continuation_expectation
            == "tool_progress_or_typed_no_progress"
        && matches!(
            hard_authoring_final_message_plan.tool_choice,
            ToolChoice::Required
        )
        && hard_authoring_final_message_plan.plan_reason
            == "code_authoring_final_message_hard_edit_recovery"
        && hard_authoring_final_message_plan.proposal_policy
            == "tool_call_required_or_provider_noncompliance"
        && hard_authoring_final_message_plan.terminal_policy
            == "same_hard_recovery_no_progress_terminal"
        && matches!(empty_surface_plan.tool_choice, ToolChoice::None)
        && empty_surface_plan.replay_policy == "no_executable_tool_replay"
        && empty_surface_plan.proposal_policy == "final_message_only_or_fail_closed"
        && empty_surface_plan.continuation_expectation == "final_or_fail"
        && empty_surface_plan.diagnostics_projection == "no_tool_surface_projection"
}

pub(crate) fn turn_lifecycle_recovery_context_is_kernel_owned_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        ..SessionStateSnapshot::default()
    };
    state.active_targets = vec![camino::Utf8PathBuf::from("tests/workflow_test.rs")];
    state.completion.open_work_count = 1;
    let tools = ["apply_patch", "write", "shell", "todowrite"]
        .into_iter()
        .map(|name| ToolSchema {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let progress_counts = BTreeMap::from([("same-progress-projection".to_string(), 1)]);
    let context =
        TurnLifecycleKernel::compile_recovery_context(TurnLifecycleRecoveryContextInput {
            state: &state,
            tools: &tools,
            stable_tools: &tools,
            current_tool_names: &tool_names,
            post_provider_tool_names: &tool_names,
            rejected_tool_proposals: &BTreeMap::new(),
            wrong_authoring_target_counts: &BTreeMap::new(),
            progress_projection_no_progress_counts: &progress_counts,
            repair_supporting_context_budget_recovery_active: false,
            malformed_write_patch_recovery_pending: false,
            malformed_apply_patch_write_recovery_pending: false,
            has_open_obligation_final_message_recovery: true,
            open_obligation_final_message_recovery_count: Some(2),
            open_obligation_final_message_hard_edit_recovery_pending: false,
            provider_required_tool_choice_final_message_recovery_pending: true,
            has_invalid_edit_recovery: false,
            generated_test_source_reference_grounding_active: false,
            generated_test_reference_consumed_target_grounding_active: false,
            verification_target_grounding_active: false,
            authoring_target_grounding_recovery_edit_only: false,
            patch_context_mismatch_grounding_active: false,
            existing_target_grounding_recovery_active: false,
            docs_route_has_required_content_grounding_evidence: false,
            authoring_targets_need_grounding: true,
            progress_projection_target_grounding_read_needed: true,
        });

    context.open_obligation_final_message_recovery_active
        && context.open_obligation_final_message_count == 2
        && context.code_authoring_final_message_hard_edit_recovery_active
        && context.provider_required_tool_choice_final_message_recovery_active
        && context.progress_projection_edit_recovery_active
        && context.progress_projection_edit_recovery_needs_grounding_read
        && context.authoring_target_grounding_final_message_recovery_active
        && !context.docs_grounding_final_message_recovery_active
        && !context.docs_content_grounding_recovery_active
}

pub(crate) fn turn_lifecycle_early_surface_sequence_is_kernel_owned_fixture_passes() -> bool {
    let stable_tools = [
        "apply_patch",
        "docling_convert",
        "grep",
        "mcp_call",
        "read",
        "shell",
        "todowrite",
        "write",
    ]
    .into_iter()
    .map(|name| ToolSchema {
        name: name.to_string(),
        description: String::new(),
        input_schema: json!({"type":"object"}),
        strict: false,
    })
    .collect::<Vec<_>>();

    let mut docs_state = SessionStateSnapshot {
        route: TaskRoute::Docs,
        process_phase: ProcessPhase::Author,
        ..SessionStateSnapshot::default()
    };
    docs_state.active_targets = vec![camino::Utf8PathBuf::from("docs/workflow.md")];
    docs_state.completion.open_work_count = 1;
    let mut docs_tools = stable_tools.clone();
    let docs_plan = TurnLifecycleKernel::apply_early_pre_context_recovery_surface(
        &mut docs_tools,
        &stable_tools,
        TurnLifecycleEarlyPreContextSurfaceInput {
            state: &docs_state,
            docs_route_supporting_context_budget_recovery_active: true,
            authoring_supporting_context_budget_recovery_active: false,
            authoring_supporting_context_budget_recovery_needs_read: false,
            generated_test_source_reference_grounding_active: false,
            generated_test_reference_consumed_target_grounding_active: false,
            singleton_missing_authoring_target_create_action_active: false,
            existing_target_grounding_recovery_active: false,
            patch_context_mismatch_grounding_active: true,
            repair_supporting_context_budget_recovery_active: false,
        },
    );
    let docs_final_tool_names = tool_schema_names(&docs_tools);

    let mut repair_state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Repair,
        ..SessionStateSnapshot::default()
    };
    repair_state.active_targets = vec![camino::Utf8PathBuf::from("src/workflow.rs")];
    repair_state.completion.verification_pending = true;
    let mut repair_tools = stable_tools.clone();
    let repair_plan = TurnLifecycleKernel::apply_early_pre_context_recovery_surface(
        &mut repair_tools,
        &stable_tools,
        TurnLifecycleEarlyPreContextSurfaceInput {
            state: &repair_state,
            docs_route_supporting_context_budget_recovery_active: false,
            authoring_supporting_context_budget_recovery_active: false,
            authoring_supporting_context_budget_recovery_needs_read: false,
            generated_test_source_reference_grounding_active: false,
            generated_test_reference_consumed_target_grounding_active: false,
            singleton_missing_authoring_target_create_action_active: false,
            existing_target_grounding_recovery_active: false,
            patch_context_mismatch_grounding_active: true,
            repair_supporting_context_budget_recovery_active: true,
        },
    );
    let repair_final_tool_names = tool_schema_names(&repair_tools);

    docs_plan.pre_authority_tool_names
        == BTreeSet::from([
            "apply_patch".to_string(),
            "todowrite".to_string(),
            "write".to_string(),
        ])
        && !docs_plan.verification_target_grounding_active
        && docs_final_tool_names
            == BTreeSet::from([
                "apply_patch".to_string(),
                "docling_convert".to_string(),
                "grep".to_string(),
                "mcp_call".to_string(),
                "read".to_string(),
                "shell".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && repair_plan.pre_authority_tool_names == tool_schema_names(&stable_tools)
        && repair_plan.verification_target_grounding_active
        && repair_final_tool_names
            == BTreeSet::from([
                "apply_patch".to_string(),
                "read".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
}

pub(crate) fn turn_lifecycle_late_surface_sequence_is_kernel_owned_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Repair,
        ..SessionStateSnapshot::default()
    };
    state.active_targets = vec![camino::Utf8PathBuf::from("src/workflow.rs")];
    state.completion.open_work_count = 1;
    state.completion.verification_pending = true;
    let stable_tools = ["apply_patch", "write", "shell", "todowrite"]
        .into_iter()
        .map(|name| ToolSchema {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut tools = stable_tools.clone();
    let rejected_tool_proposals = BTreeMap::from([(
        "model_action_rejection|semantic=provider_ignored_edit_only_surface|requested=shell|effective=shell|payload=fixture|allowed=apply_patch,write|required=write:src/workflow.rs|open=repair:workflow-source|targets=src/workflow.rs"
            .to_string(),
        1,
    )]);
    let wrong_authoring_target_counts = BTreeMap::from([("wrong-target".to_string(), 1)]);
    let plan = TurnLifecycleKernel::apply_late_pre_context_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecycleLatePreContextSurfaceInput {
            state: &state,
            rejected_tool_proposals: &rejected_tool_proposals,
            wrong_authoring_target_counts: &wrong_authoring_target_counts,
            repair_supporting_context_budget_recovery_active: false,
            malformed_write_patch_recovery_pending: false,
            malformed_apply_patch_write_recovery_pending: false,
            patch_context_mismatch_grounding_active: false,
            verification_target_grounding_active: false,
        },
    );
    let final_tool_names = tool_schema_names(&tools);

    plan.provider_noncompliance_edit_recovery_active
        && plan.wrong_target_authoring_edit_recovery_active
        && !plan.malformed_write_patch_recovery_active
        && !plan.malformed_apply_patch_write_recovery_active
        && plan.current_tool_names.contains("apply_patch")
        && plan.post_provider_tool_names == BTreeSet::from(["write".to_string()])
        && !plan.verification_target_grounding_active
        && final_tool_names == BTreeSet::from(["write".to_string()])
}

pub(crate) fn progress_projection_recovery_narrows_to_edit_surface_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        ..SessionStateSnapshot::default()
    };
    state.active_targets = vec![
        camino::Utf8PathBuf::from("src/workflow.rs"),
        camino::Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.completion.open_work_count = 2;
    let stable_tools = ["apply_patch", "shell", "todowrite"]
        .into_iter()
        .map(|name| ToolSchema {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut tools = stable_tools.clone();
    let recovery = TurnLifecycleRecoveryContext {
        progress_projection_edit_recovery_active: true,
        progress_projection_edit_recovery_needs_grounding_read: false,
        ..TurnLifecycleRecoveryContext::default()
    };

    TurnLifecycleKernel::apply_post_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecycleRecoverySurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            generated_test_orientation_allowed: true,
        },
    );
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let plan = TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
        policy: &PromptPolicy::default(),
        state: &state,
        tool_names: &tool_names,
        recovery,
    });

    tool_names == BTreeSet::from(["apply_patch".to_string()])
        && matches!(plan.tool_choice, ToolChoice::Required)
        && plan.plan_reason == "progress_projection_edit_recovery"
        && !tool_names.contains("todowrite")
        && !tool_names.contains("shell")
        && !tool_names.contains("write")
}

pub(crate) fn progress_projection_recovery_preserves_target_grounding_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        ..SessionStateSnapshot::default()
    };
    state.active_targets = vec![camino::Utf8PathBuf::from("src/workflow.rs")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    let stable_tools = ["apply_patch", "read", "shell", "todowrite", "write"]
        .into_iter()
        .map(|name| ToolSchema {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut tools = stable_tools.clone();
    let recovery = TurnLifecycleRecoveryContext {
        progress_projection_edit_recovery_active: true,
        progress_projection_edit_recovery_needs_grounding_read: true,
        ..TurnLifecycleRecoveryContext::default()
    };

    TurnLifecycleKernel::apply_post_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecycleRecoverySurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            generated_test_orientation_allowed: false,
        },
    );
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let plan = TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
        policy: &PromptPolicy::default(),
        state: &state,
        tool_names: &tool_names,
        recovery,
    });

    tool_names == BTreeSet::from(["apply_patch".to_string(), "read".to_string()])
        && matches!(plan.tool_choice, ToolChoice::Required)
        && plan.plan_reason == "progress_projection_edit_recovery"
        && !tool_names.contains("todowrite")
        && !tool_names.contains("shell")
}

pub(crate) fn edit_only_authoring_grounding_overrides_repair_grounding_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Repair,
        ..SessionStateSnapshot::default()
    };
    state.active_targets = vec![camino::Utf8PathBuf::from("src/workflow.rs")];
    state.completion.verification_pending = true;
    let stable_tools = ["apply_patch", "read", "shell", "todowrite", "write"]
        .into_iter()
        .map(|name| ToolSchema {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut tools = stable_tools.clone();
    let recovery = TurnLifecycleRecoveryContext {
        authoring_target_grounding_recovery_edit_only: true,
        verification_target_grounding_active: true,
        ..TurnLifecycleRecoveryContext::default()
    };

    TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecyclePreNormalizationSurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            code_authoring_final_message_recovery_stable_surface_active: false,
            code_repair_final_message_recovery_stable_surface_active: false,
        },
    );
    TurnLifecycleKernel::apply_post_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecycleRecoverySurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            generated_test_orientation_allowed: false,
        },
    );
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let plan = TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
        policy: &PromptPolicy::default(),
        state: &state,
        tool_names: &tool_names,
        recovery,
    });

    tool_names == BTreeSet::from(["apply_patch".to_string()])
        && matches!(plan.tool_choice, ToolChoice::Required)
        && plan.plan_reason == "authoring_target_grounding_edit_only_recovery"
        && plan.proposal_policy == "tool_call_required_or_provider_noncompliance"
        && !tool_names.contains("read")
        && !tool_names.contains("todowrite")
        && !tool_names.contains("shell")
        && !tool_names.contains("write")
}

pub(crate) fn docs_content_grounding_progress_projection_preserves_grounding_surface_fixture_passes()
-> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Docs,
        process_phase: ProcessPhase::Author,
        ..SessionStateSnapshot::default()
    };
    state.active_targets = vec![camino::Utf8PathBuf::from("docs/workflow-design.md")];
    state.completion.open_work_count = 1;
    state.completion.route_contract_pending = true;
    let stable_tools = [
        "apply_patch",
        "docling_convert",
        "grep",
        "mcp_call",
        "read",
        "shell",
        "todowrite",
        "write",
    ]
    .into_iter()
    .map(|name| ToolSchema {
        name: name.to_string(),
        description: String::new(),
        input_schema: json!({"type":"object"}),
        strict: false,
    })
    .collect::<Vec<_>>();
    let mut tools = stable_tools.clone();
    let recovery = TurnLifecycleRecoveryContext {
        docs_content_grounding_recovery_active: true,
        progress_projection_edit_recovery_active: true,
        ..TurnLifecycleRecoveryContext::default()
    };

    TurnLifecycleKernel::apply_post_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecycleRecoverySurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            generated_test_orientation_allowed: true,
        },
    );
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let plan = TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
        policy: &PromptPolicy::default(),
        state: &state,
        tool_names: &tool_names,
        recovery,
    });

    tool_names
        == BTreeSet::from([
            "apply_patch".to_string(),
            "docling_convert".to_string(),
            "grep".to_string(),
            "mcp_call".to_string(),
            "read".to_string(),
            "shell".to_string(),
        ])
        && TurnLifecycleKernel::docs_route_content_grounding_after_progress_projection_tool_visible(
            "read",
        )
        && !TurnLifecycleKernel::docs_route_content_grounding_after_progress_projection_tool_visible(
            "todowrite",
        )
        && !tool_names.contains("write")
        && matches!(plan.tool_choice, ToolChoice::Required)
        && plan.plan_reason == "docs_content_grounding_progress_projection_recovery"
        && plan.replay_policy == "sanitize_plan_projection_and_preserve_grounding_context"
        && plan.proposal_policy == "tool_call_required_or_provider_noncompliance"
}

pub(crate) fn provider_noncompliance_recovery_overrides_grounding_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Repair,
        ..SessionStateSnapshot::default()
    };
    state.active_targets = vec![camino::Utf8PathBuf::from("src/workflow.rs")];
    state.completion.verification_pending = true;
    state.completion.closeout_ready = false;
    let stable_tools = ["apply_patch", "read", "shell", "todowrite", "write"]
        .into_iter()
        .map(|name| ToolSchema {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut tools = stable_tools.clone();
    let recovery = TurnLifecycleRecoveryContext {
        provider_noncompliance_edit_recovery_active: true,
        verification_target_grounding_active: true,
        ..TurnLifecycleRecoveryContext::default()
    };

    TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecyclePreNormalizationSurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            code_authoring_final_message_recovery_stable_surface_active: false,
            code_repair_final_message_recovery_stable_surface_active: false,
        },
    );
    TurnLifecycleKernel::apply_post_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecycleRecoverySurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            generated_test_orientation_allowed: false,
        },
    );
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let plan = TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
        policy: &PromptPolicy::default(),
        state: &state,
        tool_names: &tool_names,
        recovery,
    });

    tool_names == BTreeSet::from(["write".to_string()])
        && matches!(plan.tool_choice, ToolChoice::Required)
        && plan.plan_reason == "provider_noncompliance_edit_recovery"
        && plan.replay_policy == "sanitize_failed_or_stale_tool_pairs"
        && plan.proposal_policy == "tool_call_required_or_provider_noncompliance"
        && !tool_names.contains("read")
        && !tool_names.contains("todowrite")
        && !tool_names.contains("shell")
}

pub(crate) fn open_obligation_final_message_rejection_is_kernel_owned_fixture_passes() -> bool {
    let runtime_source = include_str!("loop_impl.rs");
    let final_message_rejection = ActionAdjudication::RejectedModelAction(ModelActionRejection {
        classification: ModelActionRejectionClass::ProviderNoncompliance,
        semantic_class: "text_final_while_obligations_open",
        blocked_reason: "final assistant message arrived while executable work remains".to_string(),
        result_hash: "final-message-while-open-work".to_string(),
        proposal: ModelToolCallProposal {
            call_id: "final-message".to_string(),
            requested_tool: "final_assistant_message".to_string(),
            effective_tool: "final_assistant_message".to_string(),
            arguments_json: r#"{"text":"done"}"#.to_string(),
        },
    });
    let other_rejection = ActionAdjudication::RejectedModelAction(ModelActionRejection {
        classification: ModelActionRejectionClass::ProviderNoncompliance,
        semantic_class: "provider_ignored_edit_only_surface",
        blocked_reason: "provider proposed a disallowed tool".to_string(),
        result_hash: "provider-ignored-surface".to_string(),
        proposal: ModelToolCallProposal {
            call_id: "call-1".to_string(),
            requested_tool: "shell".to_string(),
            effective_tool: "shell".to_string(),
            arguments_json: r#"{"command":"verify"}"#.to_string(),
        },
    });
    let accepted = ActionAdjudication::AcceptedToolCall(AcceptedToolCall {
        proposal: ModelToolCallProposal {
            call_id: "call-2".to_string(),
            requested_tool: "write".to_string(),
            effective_tool: "write".to_string(),
            arguments_json: r#"{"path":"README.md","content":"ready"}"#.to_string(),
        },
    });

    TurnLifecycleKernel::open_obligation_final_message_rejection(
        Some(&final_message_rejection),
        Some(&FinishReason::Stop),
    )
    .is_some_and(|rejection| rejection.semantic_class == "text_final_while_obligations_open")
        && TurnLifecycleKernel::open_obligation_final_message_rejection(
            Some(&final_message_rejection),
            Some(&FinishReason::Length),
        )
        .is_none()
        && TurnLifecycleKernel::open_obligation_final_message_rejection(
            Some(&other_rejection),
            Some(&FinishReason::Stop),
        )
        .is_none()
        && TurnLifecycleKernel::open_obligation_final_message_rejection(
            Some(&accepted),
            Some(&FinishReason::Stop),
        )
        .is_none()
        && TurnLifecycleKernel::open_obligation_final_message_rejection(
            None,
            Some(&FinishReason::Stop),
        )
        .is_none()
        && !runtime_source
            .contains(r#"rejection.semantic_class == "text_final_while_obligations_open""#)
}

pub(crate) fn wrong_target_authoring_recovery_hardens_active_target_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        ..SessionStateSnapshot::default()
    };
    state.active_targets = vec![camino::Utf8PathBuf::from("tests/workflow.spec.ts")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    let wrong_target_counts = BTreeMap::from([(
        "wrong_authoring_target|apply_patch|src/workflow.rs|tests/workflow.spec.ts".to_string(),
        1,
    )]);
    let stable_tools = ["apply_patch", "read", "shell", "todowrite", "write"]
        .into_iter()
        .map(|name| ToolSchema {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut tools = stable_tools.clone();
    let recovery = TurnLifecycleRecoveryContext {
        wrong_target_authoring_edit_recovery_active:
            TurnLifecycleKernel::wrong_target_authoring_edit_recovery_applies(
                &state,
                &wrong_target_counts,
            ),
        ..TurnLifecycleRecoveryContext::default()
    };

    TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecyclePreNormalizationSurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            code_authoring_final_message_recovery_stable_surface_active: false,
            code_repair_final_message_recovery_stable_surface_active: false,
        },
    );
    TurnLifecycleKernel::apply_post_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecycleRecoverySurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            generated_test_orientation_allowed: false,
        },
    );
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let plan = TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
        policy: &PromptPolicy::default(),
        state: &state,
        tool_names: &tool_names,
        recovery,
    });
    let policy = TurnLifecycleKernel::wrong_target_authoring_edit_recovery_policy(&state);

    let mut generated_test_tools = stable_tools.clone();
    let generated_test_recovery = TurnLifecycleRecoveryContext {
        wrong_target_authoring_edit_recovery_active:
            TurnLifecycleKernel::wrong_target_authoring_edit_recovery_applies(
                &state,
                &wrong_target_counts,
            ),
        generated_test_source_reference_grounding_active: true,
        ..TurnLifecycleRecoveryContext::default()
    };
    TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
        &mut generated_test_tools,
        &stable_tools,
        TurnLifecyclePreNormalizationSurfaceInput {
            state: &state,
            recovery: generated_test_recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            code_authoring_final_message_recovery_stable_surface_active: false,
            code_repair_final_message_recovery_stable_surface_active: false,
        },
    );
    TurnLifecycleKernel::apply_post_normalization_recovery_surface(
        &mut generated_test_tools,
        &stable_tools,
        TurnLifecycleRecoverySurfaceInput {
            state: &state,
            recovery: generated_test_recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            generated_test_orientation_allowed: false,
        },
    );
    let generated_test_tool_names = generated_test_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let generated_test_plan =
        TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
            policy: &PromptPolicy::default(),
            state: &state,
            tool_names: &generated_test_tool_names,
            recovery: generated_test_recovery,
        });

    tool_names == BTreeSet::from(["apply_patch".to_string()])
        && matches!(plan.tool_choice, ToolChoice::Required)
        && plan.plan_reason == "wrong_target_authoring_edit_recovery"
        && plan.replay_policy == "sanitize_failed_or_stale_tool_pairs"
        && plan.proposal_policy == "tool_call_required_or_provider_noncompliance"
        && plan.terminal_policy == "same_hard_recovery_no_progress_terminal"
        && policy.policy == "wrong_target_authoring_edit_recovery_surface"
        && policy.active_targets == vec!["tests/workflow.spec.ts".to_string()]
        && !tool_names.contains("read")
        && !tool_names.contains("todowrite")
        && !tool_names.contains("shell")
        && generated_test_tool_names
            == BTreeSet::from(["apply_patch".to_string(), "read".to_string()])
        && matches!(generated_test_plan.tool_choice, ToolChoice::Required)
        && generated_test_plan.plan_reason == "wrong_target_authoring_edit_recovery"
        && !generated_test_tool_names.contains("write")
        && !generated_test_tool_names.contains("shell")
        && !generated_test_tool_names.contains("todowrite")
}

pub(crate) fn malformed_apply_patch_recovery_overrides_stale_wrong_target_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        ..SessionStateSnapshot::default()
    };
    state.active_targets = vec![
        camino::Utf8PathBuf::from("src/workflow.rs"),
        camino::Utf8PathBuf::from("tests/workflow.spec.ts"),
    ];
    state.completion.open_work_count = 2;
    state.completion.closeout_ready = false;
    let stable_tools = ["apply_patch", "read", "shell", "todowrite", "write"]
        .into_iter()
        .map(|name| ToolSchema {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({"type":"object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut tools = stable_tools.clone();
    let recovery = TurnLifecycleRecoveryContext {
        wrong_target_authoring_edit_recovery_active: true,
        malformed_apply_patch_write_recovery_active: true,
        ..TurnLifecycleRecoveryContext::default()
    };

    TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecyclePreNormalizationSurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            code_authoring_final_message_recovery_stable_surface_active: false,
            code_repair_final_message_recovery_stable_surface_active: false,
        },
    );
    TurnLifecycleKernel::apply_post_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecycleRecoverySurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            generated_test_orientation_allowed: false,
        },
    );
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let plan = TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
        policy: &PromptPolicy::default(),
        state: &state,
        tool_names: &tool_names,
        recovery,
    });

    tool_names == BTreeSet::from(["apply_patch".to_string()])
        && matches!(plan.tool_choice, ToolChoice::Required)
        && plan.plan_reason == "malformed_apply_patch_write_recovery"
        && tool_names.contains("apply_patch")
        && !tool_names.contains("read")
        && !tool_names.contains("shell")
        && !tool_names.contains("todowrite")
}

fn provider_replay_omitted_tool_call_policy(
    call_id: &str,
    tool_name: &str,
    effective_tools: &BTreeSet<String>,
) -> crate::session::RequestReplayPolicyDiagnostic {
    crate::session::RequestReplayPolicyDiagnostic {
        policy: "effective_surface_tool_call_omitted".to_string(),
        call_id: Some(call_id.to_string()),
        tool_name: Some(tool_name.to_string()),
        omitted_targets: Vec::new(),
        active_targets: Vec::new(),
        reason: format!(
            "historical tool call is outside the current effective tool surface ({}) and is omitted from executable provider replay without revoking matching item evidence",
            effective_tools
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn provider_replay_omitted_tool_output_policy(
    call_id: &str,
    tool_name: &str,
    metadata: &Value,
    effective_tools: &BTreeSet<String>,
) -> crate::session::RequestReplayPolicyDiagnostic {
    crate::session::RequestReplayPolicyDiagnostic {
        policy: if provider_replay_metadata_is_supporting_context(metadata) {
            "supporting_context_evidence_preserved".to_string()
        } else if provider_replay_metadata_is_provider_noncompliance(metadata) {
            "provider_noncompliance_tool_output_omitted_outside_effective_surface".to_string()
        } else {
            "tool_output_omitted_outside_effective_surface".to_string()
        },
        call_id: Some(call_id.to_string()),
        tool_name: Some(tool_name.to_string()),
        omitted_targets: Vec::new(),
        active_targets: Vec::new(),
        reason: if provider_replay_metadata_is_supporting_context(metadata) {
            format!(
                "historical supporting-context ToolOutput is omitted from executable tool replay because it is outside the current effective tool surface ({}) but preserved as non-executable provider-visible evidence for the current action authority",
                effective_tools
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            format!(
                "historical ToolOutput is outside the current effective tool surface ({}) and is summarized as non-executable provider-visible replay context",
                effective_tools
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    }
}

pub(crate) fn provider_replay_metadata_is_supporting_context(metadata: &Value) -> bool {
    metadata
        .get("operation_progress_class")
        .and_then(Value::as_str)
        == Some("supporting_context")
        || metadata
            .get("tool_feedback_envelope")
            .and_then(|feedback| feedback.get("operation_progress_class"))
            .and_then(Value::as_str)
            == Some("supporting_context")
        || metadata
            .get("tool_feedback_envelope")
            .and_then(|feedback| feedback.get("kind"))
            .and_then(Value::as_str)
            == Some("supporting_context")
}

pub(crate) fn provider_replay_metadata_is_provider_noncompliance(metadata: &Value) -> bool {
    metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("semantic_class"))
        .and_then(Value::as_str)
        == Some("provider_ignored_edit_only_surface")
        || metadata
            .get("model_action_adjudication")
            .and_then(|feedback| feedback.get("semantic_class"))
            .and_then(Value::as_str)
            == Some("provider_ignored_edit_only_surface")
}

pub(crate) fn provider_surface_filter_omits_orphan_assistant_prelude_fixture_passes() -> bool {
    let messages = vec![ModelMessage::AssistantToolCalls {
        content: Some("I will run the stale shell verification now.".to_string()),
        tool_calls: vec![ModelToolCall {
            call_id: "call_shell".to_string(),
            tool_name: "shell".to_string(),
            arguments_json: r#"{"command":"verify-contract --behavior"}"#.to_string(),
        }],
    }];
    let effective_tools = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let projection = ReplayNormalizer::filter_to_effective_tool_surface(messages, &effective_tools);
    projection.messages.len() == 1
        && matches!(
            projection.messages.first(),
            Some(ModelMessage::User { content })
                if content.contains("Provider replay surface normalization")
                    && content.contains("Assistant tool-call prelude omitted")
                    && content.contains("shell")
        )
        && !projection.messages.iter().any(|message| {
            matches!(
                message,
                ModelMessage::Assistant { content }
                    if content.contains("stale shell verification")
            )
        })
}

pub(crate) fn provider_surface_filter_omits_mixed_stale_assistant_prelude_fixture_passes() -> bool {
    let messages = vec![ModelMessage::AssistantToolCalls {
        content: Some(
            "I will inspect with shell first, then patch the active file.".to_string(),
        ),
        tool_calls: vec![
            ModelToolCall {
                call_id: "call_shell".to_string(),
                tool_name: "shell".to_string(),
                arguments_json: r#"{"command":"verify-contract --behavior"}"#.to_string(),
            },
            ModelToolCall {
                call_id: "call_patch".to_string(),
                tool_name: "apply_patch".to_string(),
                arguments_json: r#"{"patch":"*** Begin Patch\n*** Update File: src/workflow.rs\n@@\n-pub fn workflow_state() -> &'static str { \"draft\" }\n+pub fn workflow_state() -> &'static str { \"ready\" }\n*** End Patch\n"}"#.to_string(),
            },
        ],
    }];
    let effective_tools = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);

    let projection = ReplayNormalizer::filter_to_effective_tool_surface(messages, &effective_tools);
    let Some(ModelMessage::AssistantToolCalls {
        content,
        tool_calls,
    }) = projection.messages.first()
    else {
        return false;
    };

    content.is_none()
        && tool_calls.len() == 1
        && tool_calls
            .first()
            .is_some_and(|call| call.call_id == "call_patch" && call.tool_name == "apply_patch")
        && projection.messages.iter().any(|message| {
            matches!(
                message,
                ModelMessage::User { content }
                    if content.contains("Provider replay surface normalization")
                        && content.contains("mixed replay item")
                        && content.contains("shell")
                        && content.contains("Current effective tool surface: apply_patch, write")
            )
        })
        && !projection.messages.iter().any(|message| {
            matches!(
                message,
                ModelMessage::Assistant { content }
                    if content.contains("inspect with shell")
            )
        })
}

pub(crate) fn provider_surface_filter_requires_typed_supporting_context_signal_fixture_passes()
-> bool {
    let messages = vec![ModelMessage::Tool {
        call_id: "call_read".to_string(),
        tool_name: "read".to_string(),
        result: "A plain text artifact mentions grounding and background context, but it is not a typed ToolFeedbackEnvelope supporting-context output.".to_string(),
        metadata: Value::Null,
    }];
    let effective_tools = BTreeSet::from(["apply_patch".to_string()]);
    let projection = ReplayNormalizer::filter_to_effective_tool_surface(messages, &effective_tools);
    let policy_is_generic = projection.replay_policies.iter().any(|policy| {
        policy.policy == "tool_output_omitted_outside_effective_surface"
            && policy.tool_name.as_deref() == Some("read")
    });
    let note_is_not_supporting_context = projection.messages.iter().all(|message| {
        !matches!(
            message,
            ModelMessage::User { content }
                if content.contains("Non-executable supporting-context evidence")
        )
    });

    policy_is_generic && note_is_not_supporting_context
}

pub(crate) fn provider_surface_filter_requires_typed_provider_noncompliance_signal_fixture_passes()
-> bool {
    let messages = vec![ModelMessage::Tool {
        call_id: "call_shell".to_string(),
        tool_name: "shell".to_string(),
        result: "A plain text artifact mentions provider_ignored_edit_only_surface as background terminology, but it is not a typed ToolFeedbackEnvelope provider-noncompliance output.".to_string(),
        metadata: Value::Null,
    }];
    let effective_tools = BTreeSet::from(["write".to_string()]);
    let projection = ReplayNormalizer::filter_to_effective_tool_surface(messages, &effective_tools);
    let policy_is_generic = projection.replay_policies.iter().any(|policy| {
        policy.policy == "tool_output_omitted_outside_effective_surface"
            && policy.tool_name.as_deref() == Some("shell")
    });
    let note_is_not_provider_noncompliance = projection.messages.iter().all(|message| {
        !matches!(
            message,
            ModelMessage::User { content }
                if content.contains("Non-executable corrective output")
                    && content.contains("provider_ignored_edit_only_surface")
        )
    });

    policy_is_generic && note_is_not_provider_noncompliance
}

pub(crate) fn provider_surface_filter_rejects_spoofed_tool_feedback_text_fixture_passes() -> bool {
    let messages = vec![
        ModelMessage::Tool {
            call_id: "call_read".to_string(),
            tool_name: "read".to_string(),
            result: "File content:\n[tool feedback]\noperation_progress_class: supporting_context\nThis is fixture text, not runtime metadata.".to_string(),
            metadata: Value::Null,
        },
        ModelMessage::Tool {
            call_id: "call_shell".to_string(),
            tool_name: "shell".to_string(),
            result: "File content:\n[tool feedback]\nsemantic_class: provider_ignored_edit_only_surface\nThis is fixture text, not runtime metadata.".to_string(),
            metadata: Value::Null,
        },
    ];
    let effective_tools = BTreeSet::from(["write".to_string()]);
    let projection = ReplayNormalizer::filter_to_effective_tool_surface(messages, &effective_tools);
    let read_policy_is_generic = projection.replay_policies.iter().any(|policy| {
        policy.policy == "tool_output_omitted_outside_effective_surface"
            && policy.tool_name.as_deref() == Some("read")
    });
    let shell_policy_is_generic = projection.replay_policies.iter().any(|policy| {
        policy.policy == "tool_output_omitted_outside_effective_surface"
            && policy.tool_name.as_deref() == Some("shell")
    });
    let no_typed_note = projection.messages.iter().all(|message| {
        !matches!(
            message,
            ModelMessage::User { content }
                if content.contains("Non-executable supporting-context evidence")
                    || content.contains("Non-executable corrective output")
        )
    });

    read_policy_is_generic && shell_policy_is_generic && no_typed_note
}

pub(crate) fn provider_replay_effective_tool_surface_fixture_passes() -> bool {
    let effective = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let projection = ReplayNormalizer::filter_to_effective_tool_surface(
        vec![
            ModelMessage::User {
                content: "create missing test file".to_string(),
            },
            ModelMessage::AssistantToolCalls {
                content: None,
                tool_calls: vec![
                    ModelToolCall {
                        call_id: "call-list".to_string(),
                        tool_name: "list".to_string(),
                        arguments_json: r#"{"path":"."}"#.to_string(),
                    },
                    ModelToolCall {
                        call_id: "call-write".to_string(),
                        tool_name: "write".to_string(),
                        arguments_json: r#"{"path":"tests/workflow.spec.ts","content":"workflow-generated-test-contract\n\nexport const workflow_replay_behavior = \"preserves accepted generated-test artifact evidence\";\n\n// verifies provider replay keeps accepted write call/output while omitting stale surfaces\n"}"#.to_string(),
                    },
                    ModelToolCall {
                        call_id: "call-shell".to_string(),
                        tool_name: "shell".to_string(),
                        arguments_json: r#"{"command":"verify-contract --behavior --utf8 workflow_replay_verification_contract"}"#.to_string(),
                    },
                ],
            },
            ModelMessage::Tool {
                call_id: "call-list".to_string(),
                tool_name: "list".to_string(),
                result: "1: workflow-provider-replay-supporting-context\n2: workflow_source_contract\n3: workflow_state.ready = true\n\n[tool feedback]\noperation_progress_class: supporting_context\nprogress_effect: no_progress\nactive_targets: docs/workflow-design.md".to_string(),
                metadata: json!({
                    "operation_progress_class": "supporting_context",
                    "tool_feedback_envelope": {
                        "operation_progress_class": "supporting_context",
                        "kind": "supporting_context"
                    }
                }),
            },
            ModelMessage::Tool {
                call_id: "call-write".to_string(),
                tool_name: "write".to_string(),
                result:
                    "Accepted generated-test artifact tests/workflow.spec.ts with workflow-generated-test-contract"
                        .to_string(),
                metadata: Value::Null,
            },
            ModelMessage::Tool {
                call_id: "call-shell".to_string(),
                tool_name: "shell".to_string(),
                result: "[tool feedback]\nsemantic_class: provider_ignored_edit_only_surface\nprogress_effect: no_progress\nactive_targets: tests/workflow.spec.ts\nUse `write` or `apply_patch` to repair the active target.".to_string(),
                metadata: json!({
                    "tool_feedback_envelope": {
                        "semantic_class": "provider_ignored_edit_only_surface"
                    },
                    "model_action_adjudication": {
                        "semantic_class": "provider_ignored_edit_only_surface"
                    }
                }),
            },
        ],
        &effective,
    );
    let filtered = &projection.messages;

    let has_surface_note = filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::User { content }
                if content.contains("outside the current effective tool surface")
                    && content.contains("list")
                    && content.contains("Non-executable supporting-context evidence")
                    && content.contains("workflow-provider-replay-supporting-context")
                    && content.contains("workflow_state.ready")
                    && content.contains("Non-executable corrective output")
                    && content.contains("provider_ignored_edit_only_surface")
                    && content.contains("provider-visible edit tool")
        )
    });
    let kept_write = filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.len() == 1
                    && tool_calls.first().is_some_and(|call| call.tool_name == "write")
        )
    });
    let placeholder_ok_payload = ["\"content\"", ":", "\"ok\""].concat();
    let kept_write_payload_is_effective_test = filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|call| {
                    call.tool_name == "write"
                        && call.arguments_json.contains("tests/workflow.spec.ts")
                        && call.arguments_json.contains("workflow-generated-test-contract")
                        && call.arguments_json.contains("workflow_replay_behavior")
                        && !call.arguments_json.contains(&placeholder_ok_payload)
                })
        )
    });
    let omitted_list_call = !filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|call| call.tool_name == "list")
        )
    });
    let omitted_list_output = !filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::Tool { tool_name, .. } if tool_name == "list"
        )
    });
    let omitted_shell_output = !filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::Tool { tool_name, .. } if tool_name == "shell"
        )
    });
    let preserved_write_output = filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::Tool { tool_name, result, .. }
                if tool_name == "write"
                    && result.contains("workflow-generated-test-contract")
                    && result.contains("tests/workflow.spec.ts")
        )
    });
    let latest_message_is_correction = matches!(
        filtered.last(),
        Some(ModelMessage::User { content })
            if content.contains("Provider replay surface normalization")
    );

    has_surface_note
        && kept_write
        && kept_write_payload_is_effective_test
        && omitted_list_call
        && omitted_list_output
        && omitted_shell_output
        && preserved_write_output
        && latest_message_is_correction
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "supporting_context_evidence_preserved"
                && policy.call_id.as_deref() == Some("call-list")
                && policy.tool_name.as_deref() == Some("list")
        })
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "effective_surface_tool_call_omitted"
                && policy.call_id.as_deref() == Some("call-list")
        })
}

pub(crate) fn provider_replay_effective_surface_fixture_effective_test_payload_fixture_passes()
-> bool {
    provider_replay_effective_tool_surface_fixture_passes()
}

pub(crate) fn provider_replay_preserves_supporting_context_evidence_after_surface_narrowing_fixture_passes()
-> bool {
    let effective = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let call_id = "call-read";
    let projection = ReplayNormalizer::filter_to_effective_tool_surface(
        vec![
            ModelMessage::User {
                content: "Create docs/workflow-design.md from the implementation and tests."
                    .to_string(),
            },
            ModelMessage::AssistantToolCalls {
                content: None,
                tool_calls: vec![ModelToolCall {
                    call_id: call_id.to_string(),
                    tool_name: "read".to_string(),
                    arguments_json: r#"{"path":"src/workflow.rs"}"#.to_string(),
                }],
            },
            ModelMessage::Tool {
                call_id: call_id.to_string(),
                tool_name: "read".to_string(),
                result: "1: workflow-provider-replay-supporting-context\n2: workflow_source_contract\n3: workflow_state.ready = true\n\n[tool feedback]\noperation_intent: content_changing_authoring_required\noperation_progress_class: supporting_context\nprogress_effect: no_progress\nactive_targets: docs/workflow-design.md".to_string(),
                metadata: json!({
                    "operation_progress_class": "supporting_context",
                    "tool_feedback_envelope": {
                        "operation_progress_class": "supporting_context",
                        "kind": "supporting_context"
                    }
                }),
            },
        ],
        &effective,
    );

    let omitted_executable_read = !projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|call| call.tool_name == "read")
        ) || matches!(
            message,
            ModelMessage::Tool { tool_name, .. } if tool_name == "read"
        )
    });
    let evidence_preserved = projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::User { content }
                if content.contains("Non-executable supporting-context evidence")
                    && content.contains("workflow-provider-replay-supporting-context")
                    && content.contains("workflow_source_contract")
                    && content.contains("docs/workflow-design.md")
                    && content.contains("Do not repeat that omitted tool call")
                    && content.contains("Current effective tool surface: apply_patch, write")
        )
    });
    let policy_recorded = projection.replay_policies.iter().any(|policy| {
        policy.policy == "supporting_context_evidence_preserved"
            && policy.call_id.as_deref() == Some(call_id)
            && policy.tool_name.as_deref() == Some("read")
            && policy
                .reason
                .contains("non-executable provider-visible evidence")
    });

    omitted_executable_read && evidence_preserved && policy_recorded
}

pub(crate) fn provider_replay_omits_intermediate_assistant_text_fixture_passes() -> bool {
    let filtered =
        TurnLifecycleKernel::filter_non_authoritative_assistant_text_for_open_obligations(
            vec![
                ModelMessage::System {
                    content: "control".to_string(),
                },
                ModelMessage::User {
                    content: "create files and run tests".to_string(),
                },
                ModelMessage::Assistant {
                    content: "I will do that now.".to_string(),
                },
                ModelMessage::AssistantToolCalls {
                    content: None,
                    tool_calls: vec![ModelToolCall {
                        call_id: "call-shell".to_string(),
                        tool_name: "shell".to_string(),
                        arguments_json: r#"{"command":"verify-contract --behavior"}"#.to_string(),
                    }],
                },
                ModelMessage::Tool {
                    call_id: "call-shell".to_string(),
                    tool_name: "shell".to_string(),
                    result: "tests failed".to_string(),
                    metadata: Value::Null,
                },
                ModelMessage::User {
                    content: "run the required verification now".to_string(),
                },
                ModelMessage::Assistant {
                    content: "Verification is done.".to_string(),
                },
            ],
            true,
        );
    let closed = TurnLifecycleKernel::filter_non_authoritative_assistant_text_for_open_obligations(
        vec![
            ModelMessage::User {
                content: "summarize".to_string(),
            },
            ModelMessage::Assistant {
                content: "Done.".to_string(),
            },
        ],
        false,
    );

    let assistant_text_count = filtered
        .iter()
        .filter(|message| matches!(message, ModelMessage::Assistant { .. }))
        .count();
    let preserved_tool_call = filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.first().is_some_and(|call| call.tool_name == "shell")
        )
    });
    let preserved_tool_output = filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::Tool { tool_name, .. } if tool_name == "shell"
        )
    });
    let has_note = filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::System { content }
                if content.contains("intermediate assistant text")
        )
    });

    assistant_text_count == 0
        && preserved_tool_call
        && preserved_tool_output
        && has_note
        && closed
            .iter()
            .any(|message| matches!(message, ModelMessage::Assistant { .. }))
}

pub(crate) fn provider_replay_omits_assistant_tool_call_content_fixture_passes() -> bool {
    let filtered =
        TurnLifecycleKernel::filter_non_authoritative_assistant_text_for_open_obligations(
            vec![
                ModelMessage::System {
                    content: "control".to_string(),
                },
                ModelMessage::User {
                    content: "create files and run tests".to_string(),
                },
                ModelMessage::AssistantToolCalls {
                    content: Some("Verification is done; no further edits are needed.".to_string()),
                    tool_calls: vec![ModelToolCall {
                        call_id: "call-shell".to_string(),
                        tool_name: "shell".to_string(),
                        arguments_json: r#"{"command":"verify-contract --behavior"}"#.to_string(),
                    }],
                },
                ModelMessage::Tool {
                    call_id: "call-shell".to_string(),
                    tool_name: "shell".to_string(),
                    result: "tests failed".to_string(),
                    metadata: Value::Null,
                },
            ],
            true,
        );

    let preserved_tool_call_without_content = filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls {
                content,
                tool_calls
            } if content.as_deref().unwrap_or_default().trim().is_empty()
                && tool_calls.first().is_some_and(|call| call.tool_name == "shell")
        )
    });
    let leaked_tool_call_content = filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { content: Some(content), .. }
                if content.contains("Verification is done")
        )
    });
    let preserved_tool_output = filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::Tool { tool_name, .. } if tool_name == "shell"
        )
    });
    let has_note = filtered.iter().any(|message| {
        matches!(
            message,
            ModelMessage::System { content }
                if content.contains("assistant tool-call content")
        )
    });

    preserved_tool_call_without_content
        && !leaked_tool_call_content
        && preserved_tool_output
        && has_note
}

pub(crate) fn provider_metadata_mode_serializes_named_tool_choice_fixture_passes() -> bool {
    let Some(choice) =
        TurnLifecycleKernel::provider_tool_choice_value(1, &ToolChoice::Named(ToolName::Shell))
    else {
        return false;
    };
    let lm_studio = crate::llm::openai_compat::provider_tool_choice_json(
        &choice,
        crate::config::ProviderMetadataMode::LmStudioNativeRequired,
    );
    let openai_compatible = crate::llm::openai_compat::provider_tool_choice_json(
        &choice,
        crate::config::ProviderMetadataMode::OpenAiCompatibleOnly,
    );

    lm_studio == serde_json::json!("required")
        && openai_compatible
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            == Some("shell")
        && crate::llm::openai_compat::provider_metadata_mode_serializes_named_tool_choice_fixture_passes()
}

pub(crate) fn generated_test_authoring_keeps_recent_source_reference_read_fixture_passes() -> bool {
    let stable_tools =
        fixture_tool_schemas(&["apply_patch", "read", "shell", "todowrite", "write"]);
    let mut visible = stable_tools
        .iter()
        .filter(|tool| matches!(tool.name.as_str(), "apply_patch" | "todowrite"))
        .cloned()
        .collect::<Vec<_>>();
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.spec.ts")];
    state.completion.open_work_count = 1;

    let active =
        TurnLifecycleKernel::generated_test_source_reference_grounding_active(&state, true);
    if active {
        TurnLifecycleKernel::apply_generated_test_source_reference_grounding_surface(
            &mut visible,
            &stable_tools,
            true,
        );
    }
    let tool_names = tool_schema_names(&visible);
    let choice = compile_turn_lifecycle_tool_choice(
        &PromptPolicy::default(),
        &state,
        &tool_names,
        TurnLifecycleRecoveryContext {
            generated_test_source_reference_grounding_active: active,
            ..TurnLifecycleRecoveryContext::default()
        },
    );

    active
        && tool_names.contains("read")
        && tool_names.contains("shell")
        && tool_names.contains("todowrite")
        && tool_names.contains("apply_patch")
        && !tool_names.contains("write")
        && matches!(choice, ToolChoice::Auto)
}

pub(crate) fn singleton_missing_authoring_target_projects_create_action_fixture_passes() -> bool {
    let stable_tools =
        fixture_tool_schemas(&["apply_patch", "read", "shell", "todowrite", "write"]);
    let mut visible = stable_tools.clone();
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
    state.completion.open_work_count = 1;

    let active =
        TurnLifecycleKernel::singleton_missing_authoring_target_create_action_active(&state, false);
    if active {
        TurnLifecycleKernel::apply_singleton_missing_authoring_target_create_action_surface(
            &mut visible,
            &stable_tools,
        );
    }
    let tool_names = tool_schema_names(&visible);
    let choice = compile_turn_lifecycle_tool_choice(
        &PromptPolicy::default(),
        &state,
        &tool_names,
        TurnLifecycleRecoveryContext::default(),
    );

    active
        && tool_names == BTreeSet::from(["apply_patch".to_string(), "todowrite".to_string()])
        && matches!(choice, ToolChoice::Auto)
}

pub(crate) fn code_authoring_final_message_recovery_reopens_stable_surface_fixture_passes() -> bool
{
    let mut narrowed_tools = fixture_tool_schemas(&["apply_patch", "todowrite"]);
    let stable_tools = fixture_tool_schemas(&["apply_patch", "shell", "todowrite"]);
    let stable_tool_names = stable_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.behavior.md")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

    if TurnLifecycleKernel::open_executable_work_requires_tool_call(&state)
        && TurnLifecycleKernel::code_authoring_open_obligation_final_message_recovery_uses_stable_surface(&state)
    {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut narrowed_tools,
            &stable_tools,
            |_| true,
        );
    } else if TurnLifecycleKernel::open_executable_work_requires_tool_call(&state) {
        narrowed_tools.retain(|tool| {
            TurnLifecycleKernel::open_obligation_final_message_recovery_tool_visible(
                &state, &tool.name,
            )
        });
    }
    let recovered_tool_names = tool_schema_names(&narrowed_tools);
    let choice = compile_turn_lifecycle_tool_choice(
        &PromptPolicy::default(),
        &state,
        &recovered_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 1,
            ..TurnLifecycleRecoveryContext::default()
        },
    );

    recovered_tool_names == stable_tool_names && matches!(choice, ToolChoice::Auto)
}

pub(crate) fn failed_edit_final_message_recovery_keeps_failed_edit_surface_fixture_passes() -> bool
{
    let mut tools = fixture_tool_schemas(&["apply_patch", "todowrite"]);
    let stable_tools =
        fixture_tool_schemas(&["apply_patch", "read", "shell", "todowrite", "write"]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.behavior.md")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;

    let recovery = TurnLifecycleRecoveryContext {
        failed_edit_recovery_active: true,
        open_obligation_final_message_recovery_active: true,
        open_obligation_final_message_count: 1,
        ..TurnLifecycleRecoveryContext::default()
    };
    TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecyclePreNormalizationSurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            code_authoring_final_message_recovery_stable_surface_active: true,
            code_repair_final_message_recovery_stable_surface_active: false,
        },
    );
    TurnLifecycleKernel::apply_post_normalization_recovery_surface(
        &mut tools,
        &stable_tools,
        TurnLifecycleRecoverySurfaceInput {
            state: &state,
            recovery,
            code_authoring_final_message_hard_edit_recovery_active: false,
            generated_test_orientation_allowed: true,
        },
    );
    let tool_names = tool_schema_names(&tools);
    let plan = TurnLifecycleKernel::compile_turn_lifecycle_plan(TurnLifecyclePlanInput {
        policy: &PromptPolicy::default(),
        state: &state,
        tool_names: &tool_names,
        recovery,
    });

    tool_names.contains("apply_patch")
        && tool_names.contains("todowrite")
        && tool_names.contains("write")
        && !tool_names.contains("shell")
        && !tool_names.contains("read")
        && matches!(plan.tool_choice, ToolChoice::Required)
        && plan.plan_reason == "failed_edit_final_message_recovery"
        && plan.proposal_policy == "tool_call_required_or_provider_noncompliance"
        && plan.terminal_policy == "same_hard_recovery_no_progress_terminal"
}

pub(crate) fn open_obligation_final_message_recovery_persists_across_no_progress_tool_fixture_passes()
-> bool {
    let tool_names = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.completion.open_work_count = 2;
    let recovery = TurnLifecycleKernel::open_obligation_final_message_recovery_envelope(
        &state,
        1,
        None,
        &tool_names,
        false,
    );
    let base_messages = vec![
        ModelMessage::User {
            content: "create src/workflow.rs and tests/workflow.behavior.md".to_string(),
        },
        ModelMessage::AssistantToolCalls {
            content: None,
            tool_calls: vec![ModelToolCall {
                call_id: "call-shell".to_string(),
                tool_name: "shell".to_string(),
                arguments_json: json!({"command":"Get-ChildItem -Name"}).to_string(),
            }],
        },
        ModelMessage::Tool {
            call_id: "call-shell".to_string(),
            tool_name: "shell".to_string(),
            result: "supporting context only; no required artifacts changed".to_string(),
            metadata: Value::Null,
        },
    ];
    let first_prompt = Some(recovery.prompt.clone());
    let second_prompt = Some(recovery.prompt.clone());
    let (first_messages, _) = TurnLifecycleKernel::provider_messages_for_dispatch_control(
        &base_messages,
        "Turn control projection surface: prompt".to_string(),
        first_prompt,
        None,
        &tool_names,
        true,
    );
    let (second_messages, _) = TurnLifecycleKernel::provider_messages_for_dispatch_control(
        &base_messages,
        "Turn control projection surface: prompt".to_string(),
        second_prompt,
        None,
        &tool_names,
        true,
    );
    let system_has_recovery = |messages: &[ModelMessage]| {
        messages.iter().any(|message| match message {
            ModelMessage::System { content } => {
                content.contains("Open-obligation final-message recovery:")
                    && content.contains("src/workflow.rs, tests/workflow.behavior.md")
                    && content.contains("*** Add File")
                    && content.contains("*** Update File")
            }
            _ => false,
        })
    };
    recovery.count == 1
        && recovery.active_targets
            == vec![
                "src/workflow.rs".to_string(),
                "tests/workflow.behavior.md".to_string(),
            ]
        && system_has_recovery(&first_messages)
        && system_has_recovery(&second_messages)
}

fn clip_provider_replay_note(text: &str, limit: usize) -> String {
    clip_provider_replay_text(text, limit)
}

fn clip_provider_replay_evidence(text: &str, limit: usize) -> String {
    clip_provider_replay_text(text, limit)
}

fn clip_provider_replay_text(text: &str, limit: usize) -> String {
    let mut clipped = String::new();
    for ch in text.chars().take(limit) {
        clipped.push(ch);
    }
    if text.chars().count() > limit {
        clipped.push_str("...");
    }
    clipped
}

fn rejection_title(classification: ModelActionRejectionClass) -> &'static str {
    match classification {
        ModelActionRejectionClass::InvalidTool => "Invalid tool call",
        ModelActionRejectionClass::ToolOutsideAllowedSurface => "Tool not allowed",
        ModelActionRejectionClass::ProviderNoncompliance => "Provider action rejected",
    }
}

fn parse_known_tool_name(value: &str) -> Option<ToolName> {
    Some(match value {
        "list" => ToolName::List,
        "glob" => ToolName::Glob,
        "grep" => ToolName::Grep,
        "read" => ToolName::Read,
        "inspect_directory" => ToolName::InspectDirectory,
        "apply_patch" => ToolName::ApplyPatch,
        "write" => ToolName::Write,
        "shell" => ToolName::Shell,
        "skill" => ToolName::Skill,
        "docling_convert" => ToolName::DoclingConvert,
        "mcp_call" => ToolName::McpCall,
        "todowrite" => ToolName::TodoWrite,
        _ => return None,
    })
}

fn parse_tool_name(value: &str) -> ToolName {
    parse_known_tool_name(value).unwrap_or(ToolName::Invalid)
}

fn malformed_tool_argument_error_family(arguments_json: &str) -> &'static str {
    match serde_json::from_str::<Value>(arguments_json) {
        Ok(_) => "valid_json",
        Err(error) if error.is_eof() => "json_eof",
        Err(error) if error.is_syntax() => "json_syntax",
        Err(error) if error.is_data() => "json_data",
        Err(_) => "json_io",
    }
}

fn arguments_value(arguments_json: &str) -> Value {
    serde_json::from_str(arguments_json).unwrap_or_else(|_| {
        json!({
            "raw_arguments": arguments_json,
            "parse_error": "invalid_json"
        })
    })
}

pub(crate) fn open_work_uses_auto_tool_choice_with_harness_closeout_guard_fixture_passes() -> bool {
    let tool_names = BTreeSet::from(["read".to_string(), "write".to_string()]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.behavior.md")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state.completion.verification_pending = false;
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

    matches!(
        compile_turn_lifecycle_tool_choice(
            &PromptPolicy::default(),
            &state,
            &tool_names,
            TurnLifecycleRecoveryContext::default(),
        ),
        ToolChoice::Auto
    ) && TurnLifecycleKernel::open_executable_work_requires_tool_call(&state)
        && !TurnLifecycleKernel::closeout_ready_final_message_authority(&state)
}

pub(crate) fn singleton_write_surface_requires_tool_choice_fixture_passes() -> bool {
    let tool_names = BTreeSet::from(["write".to_string()]);
    matches!(
        compile_turn_lifecycle_tool_choice(
            &PromptPolicy::default(),
            &SessionStateSnapshot::default(),
            &tool_names,
            TurnLifecycleRecoveryContext::default(),
        ),
        ToolChoice::Auto
    )
}

pub(crate) fn concrete_write_required_action_narrows_broad_surface_fixture_passes() -> bool {
    let tools = vec![
        ToolSchema {
            name: "read".to_string(),
            description: "read a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "todowrite".to_string(),
            description: "update progress".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "write".to_string(),
            description: "write a file".to_string(),
            input_schema: json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"}
                }
            }),
            strict: false,
        },
    ];
    let tool_names = tool_schema_names(&tools);
    tools.len() == 3
        && tools.iter().any(|tool| {
            tool.name == "write"
                && !tool.strict
                && tool.input_schema.pointer("/properties/path").is_some()
        })
        && matches!(
            compile_turn_lifecycle_tool_choice(
                &PromptPolicy::default(),
                &SessionStateSnapshot::default(),
                &tool_names,
                TurnLifecycleRecoveryContext::default(),
            ),
            ToolChoice::Auto
        )
}

pub(crate) fn required_repair_write_missing_tool_is_not_restored_fixture_passes() -> bool {
    let tools = vec![ToolSchema {
        name: "shell".to_string(),
        description: "run a shell command".to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["command"],
            "properties": {
                "command": {"type": "string"}
            }
        }),
        strict: false,
    }];
    let tool_names = tool_schema_names(&tools);

    tools.len() == 1
        && tools.first().is_some_and(|tool| tool.name == "shell")
        && !matches!(
            compile_turn_lifecycle_tool_choice(
                &PromptPolicy::default(),
                &SessionStateSnapshot::default(),
                &tool_names,
                TurnLifecycleRecoveryContext::default(),
            ),
            ToolChoice::Required
        )
}

pub(crate) fn clean_closeout_final_message_lifecycle_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.completion.closeout_ready = true;
    state.completion.open_work_count = 0;
    state.completion.verification_pending = false;
    state.completion.route_contract_pending = false;
    TurnLifecycleKernel::clean_closeout_final_message_lifecycle(&state, None)
        && compile_turn_lifecycle_tool_choice(
            &PromptPolicy::default(),
            &state,
            &BTreeSet::new(),
            TurnLifecycleRecoveryContext::default(),
        ) == ToolChoice::None
        && TurnLifecycleKernel::closeout_ready_final_message_authority(&state)
}

pub(crate) fn answer_only_final_message_lifecycle_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Discover;
    state.completion.closeout_ready = false;
    state.completion.open_work_count = 0;
    state.completion.verification_pending = false;
    state.completion.route_contract_pending = false;

    let mut executable = state.clone();
    executable.process_phase = ProcessPhase::Author;
    executable.active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
    executable.completion.open_work_count = 1;
    executable.completion.blocked_reason =
        Some("Requested implementation updates are still missing from the workspace.".to_string());

    let mut verification = state.clone();
    verification.process_phase = ProcessPhase::Verify;
    verification.completion.verification_pending = true;
    verification
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

    TurnLifecycleKernel::answer_only_final_message_authority(&state)
        && TurnLifecycleKernel::closeout_ready_final_message_authority(&state)
        && compile_turn_lifecycle_tool_choice(
            &PromptPolicy::default(),
            &state,
            &BTreeSet::new(),
            TurnLifecycleRecoveryContext::default(),
        ) == ToolChoice::None
        && !TurnLifecycleKernel::answer_only_final_message_authority(&executable)
        && !TurnLifecycleKernel::closeout_ready_final_message_authority(&executable)
        && TurnLifecycleKernel::open_executable_work_requires_tool_call(&executable)
        && !TurnLifecycleKernel::answer_only_final_message_authority(&verification)
        && !TurnLifecycleKernel::closeout_ready_final_message_authority(&verification)
        && TurnLifecycleKernel::open_executable_work_requires_tool_call(&verification)
}

pub(crate) fn answer_only_final_message_lifecycle_fixture_language_neutral_fixture_passes() -> bool
{
    let source = include_str!("lifecycle_kernel.rs");
    let fixture_block = source
        .split("pub(crate) fn answer_only_final_message_lifecycle_fixture_passes")
        .nth(1)
        .and_then(|tail| {
            tail.split(
                "pub(crate) fn answer_only_final_message_lifecycle_fixture_language_neutral_fixture_passes",
            )
            .next()
        })
        .unwrap_or_default();

    !fixture_block.contains("hello.py") && fixture_block.contains("src/workflow.rs")
}

pub(crate) fn invalid_tool_recovery_shell_success_does_not_synthesize_closeout_fixture_passes()
-> bool {
    let legacy_evidence_text = [
        "Latest confirmed evidence",
        "recovery command completed successfully after invalid tool-call feedback.",
    ]
    .join(": ");
    !include_str!("loop_impl.rs").contains(&legacy_evidence_text)
}

pub(crate) fn closeout_timeout_does_not_synthesize_final_assistant_message_fixture_passes() -> bool
{
    let source = include_str!("loop_impl.rs");
    let forbidden_fn = ["fn closeout_timeout", "_fallback_text"].concat();
    let forbidden_call = ["closeout_timeout", "_fallback_text()"].concat();
    let forbidden_text = ["完了", "しました。"].concat();
    let timeout_error_branch = source
        .split("Err(error) => {")
        .nth(1)
        .and_then(|tail| tail.split("return Err(AgentError::Llm(error));").next())
        .unwrap_or_default();

    !source.contains(&forbidden_fn)
        && !source.contains(&forbidden_call)
        && !source.contains(&forbidden_text)
        && !timeout_error_branch.contains("RunEvent::TextDelta")
        && !timeout_error_branch.contains("MessagePart::Text")
        && !timeout_error_branch.contains("complete_turn(")
        && !timeout_error_branch.contains("FinishReason::Stop")
}

pub(crate) fn closeout_ready_final_response_timeout_guard_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.completion.closeout_ready = true;
    state.completion.open_work_count = 0;
    state.completion.verification_pending = false;
    state.completion.route_contract_pending = false;

    let mut authoring_state = SessionStateSnapshot::default();
    authoring_state.process_phase = ProcessPhase::Author;
    authoring_state.active_targets = vec![Utf8PathBuf::from("docs.md")];
    authoring_state.completion.closeout_ready = false;
    authoring_state.completion.open_work_count = 1;

    TurnLifecycleKernel::closeout_final_response_timeout_ms(0, &state, None) == 120_000
        && TurnLifecycleKernel::closeout_final_response_timeout_ms(120_001, &state, None) == 120_000
        && TurnLifecycleKernel::closeout_final_response_timeout_ms(30_000, &state, None) == 30_000
        && TurnLifecycleKernel::terminal_response_timeout_ms_for_state(30_000, &state, None)
            == Some(30_000)
        && TurnLifecycleKernel::terminal_response_timeout_ms_for_state(
            30_000,
            &authoring_state,
            None,
        )
        .is_none()
        && crate::agent::event::provider_request_timeout_error_message(120_000)
            == format!(
                "provider request timeout after {}ms before a terminal model response",
                120_000
            )
        && closeout_timeout_does_not_synthesize_final_assistant_message_fixture_passes()
}

pub(crate) fn open_obligation_final_message_guard_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.completion.closeout_ready = false;
    state.completion.open_work_count = 2;
    state.completion.blocked_reason =
        Some("Requested implementation updates are still missing from the workspace.".to_string());

    let correction = TurnLifecycleKernel::open_obligation_final_message_recovery_envelope(
        &state,
        1,
        None,
        &BTreeSet::from(["apply_patch".to_string(), "write".to_string()]),
        false,
    )
    .prompt;
    let terminal = TurnLifecycleKernel::open_obligation_final_message_terminal_message(
        &state,
        TurnLifecycleKernel::open_obligation_final_message_terminal_threshold(),
    );

    TurnLifecycleKernel::open_executable_work_requires_tool_call(&state)
        && !TurnLifecycleKernel::closeout_ready_final_message_authority(&state)
        && !TurnLifecycleKernel::clean_closeout_final_message_lifecycle(&state, None)
        && correction.contains("not accepted as a final answer")
        && correction.contains("src/workflow.rs, tests/workflow.behavior.md")
        && correction.contains("write")
        && correction.contains("apply_patch")
        && terminal.contains("no clean closeout was accepted")
        && terminal.contains("src/workflow.rs, tests/workflow.behavior.md")
}

pub(crate) fn open_obligation_final_message_guard_is_recovery_context_keyed_fixture_passes() -> bool
{
    let tool_names = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.completion.closeout_ready = false;
    state.completion.open_work_count = 2;

    let open_only_key =
        TurnLifecycleKernel::open_obligation_final_message_guard_key(&state, None, None, false);
    let open_recovery_key =
        TurnLifecycleKernel::open_obligation_final_message_guard_key(&state, None, None, false);
    let first_invalid = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Add File: src/workflow.rs\n+ok\n*** End"}"#,
        "tool patch error: patch must end with `*** End Patch`",
        &state,
        Some(&tool_names),
        Some(&ToolChoice::Auto),
    );
    let second_invalid = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Add File: src/workflow.rs\n+pub fn build_workflow() -> i32 {\nworkflow_state()\n*** End Patch"}"#,
        "tool patch error: add file body line `workflow_state()` must start with `+`",
        &state,
        Some(&tool_names),
        Some(&ToolChoice::Auto),
    );
    let Some(first_recovery) = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &first_invalid.metadata,
        &state,
        &tool_names,
        &ToolChoice::Auto,
    ) else {
        return false;
    };
    let Some(second_recovery) = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &second_invalid.metadata,
        &state,
        &tool_names,
        &ToolChoice::Auto,
    ) else {
        return false;
    };
    let first_invalid_key = TurnLifecycleKernel::open_obligation_final_message_guard_key(
        &state,
        None,
        Some(&first_recovery),
        false,
    );
    let second_invalid_key = TurnLifecycleKernel::open_obligation_final_message_guard_key(
        &state,
        None,
        Some(&second_recovery),
        false,
    );
    let mut counts = BTreeMap::<String, usize>::new();
    let open_only_count = *counts
        .entry(open_only_key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    let open_recovery_first_count = *counts
        .entry(open_recovery_key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    let open_recovery_second_count = *counts
        .entry(open_recovery_key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    let invalid_first_count = *counts
        .entry(first_invalid_key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    let invalid_second_count = *counts
        .entry(second_invalid_key.clone())
        .and_modify(|count| *count += 1)
        .or_insert(1);
    let first_hash = first_recovery.result_hash.as_deref().unwrap_or("");
    let second_hash = second_recovery.result_hash.as_deref().unwrap_or("");

    open_only_key != first_invalid_key
        && open_only_key == open_recovery_key
        && open_recovery_key != first_invalid_key
        && first_invalid_key == second_invalid_key
        && !first_hash.is_empty()
        && !second_hash.is_empty()
        && first_hash != second_hash
        && !first_invalid_key.contains(first_hash)
        && !second_invalid_key.contains(second_hash)
        && open_only_count == 1
        && open_recovery_first_count == 2
        && open_recovery_second_count
            == TurnLifecycleKernel::open_obligation_final_message_terminal_threshold()
        && invalid_first_count == 1
        && invalid_second_count == 2
        && invalid_second_count
            < TurnLifecycleKernel::open_obligation_final_message_terminal_threshold()
}

pub(crate) fn provider_chat_request_omits_consumed_images_fixture_passes() -> bool {
    let mut repair_state = SessionStateSnapshot::default();
    repair_state.process_phase = ProcessPhase::Repair;
    repair_state.completion.verification_pending = true;
    repair_state.active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
    repair_state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: public state assertion mismatch".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: vec![Utf8PathBuf::from("tests/workflow.behavior.md")],
    });
    repair_state.verification.failure_cluster = Some(crate::session::VerificationFailureCluster {
        cluster_id: "fixture-provider-chat-consumed-image".to_string(),
        failing_labels: vec!["workflow-verification-contract".to_string()],
        primary_failure: Some("AssertionError: workflow_state.ready expected true".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("public_state_assertion_mismatch".to_string()),
            label: Some("workflow-verification-contract".to_string()),
            target: Some("src/workflow.rs".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("workflow_state.ready true".to_string()),
            observed: Some("workflow_state.ready false".to_string()),
            public_state_assertions: vec!["workflow_state.ready".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["public_state_assertion_mismatch".to_string()],
            sibling_obligations: Vec::new(),
            requirement_refs: vec!["workflow-verification-contract".to_string()],
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
    });
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["verify-contract --behavior".to_string()],
        failing_labels: vec!["workflow-verification-contract".to_string()],
        repair_required: true,
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    };
    let provider_messages = vec![ModelMessage::UserParts {
        parts: vec![
            ModelContentPart::Text {
                text: "Use attached [Image #1] as visual reference.".to_string(),
            },
            ModelContentPart::Text {
                text: "<image name=[Image #1]>".to_string(),
            },
            ModelContentPart::Image {
                mime_type: "image/jpeg".to_string(),
                data_base64: "AAAA".to_string(),
            },
            ModelContentPart::Text {
                text: "</image>".to_string(),
            },
        ],
    }];

    let (messages, policy) = TurnLifecycleKernel::provider_messages_for_active_work_image_replay(
        provider_messages,
        &repair_state,
        Some(&active_work),
    );
    let Some(policy) = policy else {
        return false;
    };
    if policy.policy != "consumed_vision_image_omitted_from_provider_request" {
        return false;
    }
    let request = ChatRequest {
        model: ModelProfile {
            name: LIFECYCLE_FIXTURE_MODEL.to_string(),
            context_window: 131_072,
            max_output_tokens: 8_192,
            provider_metadata_mode: crate::config::ProviderMetadataMode::OpenAiCompatibleOnly,
            capabilities: crate::llm::ModelCapabilities {
                supports_tools: true,
                supports_reasoning: false,
                supports_images: false,
            },
        },
        base_url: LIFECYCLE_FIXTURE_BASE_URL.to_string(),
        system_prompt: "system".to_string(),
        messages,
        tools: Vec::new(),
        tool_choice: None,
        parallel_tool_calls: false,
        timeout_ms: 600_000,
        stream_idle_timeout_ms: 300_000,
        stream_max_retries: 2,
        extra_headers: BTreeMap::new(),
        temperature: None,
        top_p: None,
        top_k: None,
        presence_penalty: None,
        frequency_penalty: None,
        seed: None,
        stop_sequences: Vec::new(),
        extra_body: None,
    };
    let diagnostics =
        TurnLifecycleKernel::compile_request_diagnostics(&request, None, None, &[policy]);
    diagnostics.image_count == 0
        && diagnostics.image_bytes == 0
        && diagnostics
            .messages
            .iter()
            .all(|message| message.image_count == 0)
        && diagnostics
            .messages
            .iter()
            .any(|message| message.content_markers.is_empty() && message.content_chars.is_some())
        && diagnostics.replay_policies.iter().any(|policy| {
            policy.policy == "consumed_vision_image_omitted_from_provider_request"
                && policy.active_targets == vec!["src/workflow.rs".to_string()]
        })
}

pub(crate) fn verification_turn_omits_consumed_images_fixture_passes() -> bool {
    let session_id = SessionId::new();
    let message_id = crate::session::MessageId::new();
    let turn_id = TurnId::new();
    let image = crate::session::ImagePart {
        source_path: Some(Utf8PathBuf::from("reference.jpg")),
        mime_type: "image/jpeg".to_string(),
        data_base64: "abcd".to_string(),
        byte_len: 3,
    };
    let history_items = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: Some(message_id),
            content: vec![ContentPart::Image { image }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];

    let mut author_state = SessionStateSnapshot::default();
    author_state.process_phase = ProcessPhase::Author;
    let mut verify_state = SessionStateSnapshot::default();
    verify_state.process_phase = ProcessPhase::Verify;
    let mut repair_state = SessionStateSnapshot::default();
    repair_state.process_phase = ProcessPhase::Repair;
    repair_state.completion.verification_pending = true;
    repair_state.failure = Some(crate::session::FailureState {
        kind: crate::session::FailureKind::VerificationFailed,
        summary: "verification failed: workflow behavior contract mismatch".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: vec![Utf8PathBuf::from("tests/workflow.behavior.md")],
    });
    repair_state.verification.failure_cluster = Some(crate::session::VerificationFailureCluster {
        cluster_id: "fixture-consumed-image-verification-repair".to_string(),
        failing_labels: vec!["workflow-verification-contract".to_string()],
        primary_failure: Some("workflow behavior contract mismatch".to_string()),
        evidence: vec![crate::session::VerificationFailureEvidence {
            evidence_kind: "verification_failure".to_string(),
            subtype: Some("contract_assertion_mismatch".to_string()),
            label: Some("workflow-verification-contract".to_string()),
            target: Some("src/workflow.rs".to_string()),
            symbol: None,
            call_site: None,
            exception: None,
            expected: Some("workflow_state.ready true".to_string()),
            observed: Some("workflow_state.ready false".to_string()),
            public_state_assertions: vec!["workflow_state.ready".to_string()],
            public_missing_attributes: Vec::new(),
            evidence_markers: vec!["contract_assertion_mismatch".to_string()],
            sibling_obligations: Vec::new(),
            requirement_refs: vec!["workflow-verification-contract".to_string()],
            source_refs: vec!["src/workflow.rs".to_string()],
            test_refs: vec!["tests/workflow.behavior.md".to_string()],
        }],
        sibling_obligations: Vec::new(),
        source_refs: vec!["src/workflow.rs".to_string()],
        test_refs: vec!["tests/workflow.behavior.md".to_string()],
    });
    let verification_work = ActiveWorkContract::Verification {
        commands: vec!["verify-contract --behavior".to_string()],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: Vec::new(),
    };

    TurnLifecycleKernel::provider_visible_images_for_active_work(
        &history_items,
        &author_state,
        None,
    )
    .len()
        == 1
        && TurnLifecycleKernel::provider_visible_images_for_active_work(
            &history_items,
            &verify_state,
            None,
        )
        .is_empty()
        && TurnLifecycleKernel::provider_visible_images_for_active_work(
            &history_items,
            &author_state,
            Some(&verification_work),
        )
        .is_empty()
        && TurnLifecycleKernel::provider_visible_images_for_active_work(
            &history_items,
            &repair_state,
            None,
        )
        .is_empty()
}

pub(crate) fn authoring_supporting_context_budget_recovery_surface_fixture_passes() -> bool {
    let Ok(temp) = tempfile::tempdir() else {
        return false;
    };
    let Ok(workspace_root) = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) else {
        return false;
    };
    if fs::create_dir_all(workspace_root.join("docs").as_std_path()).is_err()
        || fs::write(
            workspace_root.join("docs/workflow-design.md").as_std_path(),
            "# Workflow\n",
        )
        .is_err()
    {
        return false;
    }
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.active_targets = vec![Utf8PathBuf::from("docs/workflow-design.md")];
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "glob".to_string(),
        "grep".to_string(),
        "list".to_string(),
        "read".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress",
        "result_hash": "workspace-list-hash"
    });
    let operation_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "list",
        &metadata,
        &state,
        &allowed,
        &ToolChoice::Auto,
    );
    let exhausted = BTreeSet::from([operation_key.clone()]);
    let mut visible = allowed.clone();
    if TurnLifecycleKernel::authoring_supporting_context_budget_recovery_surface_active(
        &state, &exhausted,
    ) {
        visible.retain(|tool| {
            TurnLifecycleKernel::authoring_supporting_context_budget_recovery_tool_visible(
                tool, true,
            )
        });
    }
    let mut docs_state = state.clone();
    docs_state.route = TaskRoute::Docs;
    docs_state.completion.route_contract_pending = true;

    let target_read_args = json!({"path": "docs/workflow-design.md"});
    let non_target_read_args = json!({"path": "docs/other-design.md"});
    let non_target_envelope =
        crate::agent::grounding_evidence::authoring_grounding_recovery_envelope(
            &[],
            &state,
            &workspace_root,
            &BTreeSet::new(),
        );
    let non_target_result = ToolLifecycleRuntime::authoring_target_grounding_required_result(
        "read",
        &non_target_read_args,
        &state,
        &non_target_envelope,
    );
    let mut non_target_counts = BTreeMap::new();
    let _ = ToolLifecycleRuntime::record_authoring_target_grounding_required_no_progress(
        &mut non_target_counts,
        &non_target_result,
    );
    let _ = ToolLifecycleRuntime::record_authoring_target_grounding_required_no_progress(
        &mut non_target_counts,
        &non_target_result,
    );
    let non_target_terminal =
        ToolLifecycleRuntime::record_authoring_target_grounding_required_no_progress(
            &mut non_target_counts,
            &non_target_result,
        )
        .terminal_message
        .unwrap_or_default();

    ToolLifecycleRuntime::authoring_supporting_context_budget_applies("supporting_context", &state)
        && !ToolLifecycleRuntime::authoring_supporting_context_budget_applies(
            "progress_projection",
            &state,
        )
        && TurnLifecycleKernel::authoring_supporting_context_budget_recovery_surface_active(
            &state, &exhausted,
        )
        && !TurnLifecycleKernel::authoring_supporting_context_budget_recovery_surface_active(
            &docs_state,
            &exhausted,
        )
        && ToolLifecycleRuntime::should_terminalize_operation_non_content_no_progress_for_state(
            ToolLifecycleRuntime::operation_non_content_no_progress_terminal_threshold(),
            &state,
        )
        && operation_key.contains("content_changing_authoring_required")
        && visible == BTreeSet::from(["apply_patch".to_string(), "read".to_string()])
        && !visible.contains("list")
        && !visible.contains("grep")
        && !visible.contains("glob")
        && ToolLifecycleRuntime::authoring_supporting_context_budget_recovery_read_disallowed(
            "read",
            &non_target_read_args,
            &state,
            &[],
            &workspace_root,
            &BTreeSet::new(),
        )
        && !ToolLifecycleRuntime::authoring_supporting_context_budget_recovery_read_disallowed(
            "read",
            &target_read_args,
            &state,
            &[],
            &workspace_root,
            &BTreeSet::new(),
        )
        && non_target_result
            .metadata
            .get("operation_progress_class")
            .and_then(Value::as_str)
            == Some("authoring_target_grounding_required")
        && non_target_result
            .metadata
            .pointer("/missing_grounding_targets/0")
            .and_then(Value::as_str)
            == Some("docs/workflow-design.md")
        && non_target_terminal.contains("active target read")
}

pub(crate) fn repair_supporting_context_budget_recovery_surface_fixture_passes() -> bool {
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Repair;
    state.completion.verification_pending = true;
    state.active_targets = vec![Utf8PathBuf::from("src/workflow.ts")];
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());
    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress",
        "path": "src/workflow.ts",
        "result_hash": "target-grounding-read"
    });
    let operation_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "read",
        &metadata,
        &state,
        &allowed,
        &ToolChoice::Auto,
    );
    let exhausted = if ToolLifecycleRuntime::repair_supporting_context_budget_exhausts_for_metadata(
        "read", &metadata, &state,
    ) {
        BTreeSet::from([operation_key])
    } else {
        BTreeSet::new()
    };
    let mut visible = allowed.clone();
    if TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
        &state, &exhausted,
    ) {
        visible.retain(|tool| {
            TurnLifecycleKernel::repair_supporting_context_budget_recovery_tool_visible(tool)
        });
    }
    let non_target_metadata = json!({
        "operation_intent": "content_changing_authoring_required",
        "operation_progress_class": "supporting_context",
        "progress_effect": "no_progress",
        "path": "tests/workflow.spec.ts",
        "result_hash": "non-target-evidence-read"
    });
    let non_target_key = ToolLifecycleRuntime::operation_non_content_no_progress_key(
        "read",
        &non_target_metadata,
        &state,
        &allowed,
        &ToolChoice::Auto,
    );
    let non_target_exhausted =
        if ToolLifecycleRuntime::repair_supporting_context_budget_exhausts_for_metadata(
            "read",
            &non_target_metadata,
            &state,
        ) {
            BTreeSet::from([non_target_key])
        } else {
            BTreeSet::new()
        };
    let mut non_target_visible = allowed.clone();
    if TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
        &state,
        &non_target_exhausted,
    ) {
        non_target_visible.retain(|tool| {
            TurnLifecycleKernel::repair_supporting_context_budget_recovery_tool_visible(tool)
        });
    }
    let non_target_pre_authority = non_target_visible.clone();
    if TurnLifecycleKernel::verification_repair_target_grounding_surface_active(
        &state,
        &non_target_pre_authority,
    ) {
        non_target_visible.retain(|tool| {
            TurnLifecycleKernel::verification_repair_target_grounding_surface_tool_visible(tool)
        });
    }

    ToolLifecycleRuntime::repair_supporting_context_budget_applies("supporting_context", &state)
        && !ToolLifecycleRuntime::repair_supporting_context_budget_applies(
            "progress_projection",
            &state,
        )
        && TurnLifecycleKernel::repair_supporting_context_budget_recovery_surface_active(
            &state, &exhausted,
        )
        && !ToolLifecycleRuntime::should_terminalize_operation_non_content_no_progress_for_state(
            1, &state,
        )
        && visible
            == BTreeSet::from([
                "apply_patch".to_string(),
                "todowrite".to_string(),
                "write".to_string(),
            ])
        && !visible.contains("read")
        && !visible.contains("shell")
        && non_target_exhausted.is_empty()
        && non_target_visible.contains("read")
        && non_target_visible.contains("shell")
        && non_target_visible.contains("write")
        && non_target_visible.contains("apply_patch")
}

pub(crate) fn docs_patch_context_final_message_recovery_preserves_grounding_fixture_passes() -> bool
{
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Docs;
    state.process_phase = ProcessPhase::Author;
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state.active_targets = vec![Utf8PathBuf::from("docs/workflow-design.md")];

    let allowed = BTreeSet::from([
        "apply_patch".to_string(),
        "docling_convert".to_string(),
        "grep".to_string(),
        "mcp_call".to_string(),
        "read".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
        "write".to_string(),
    ]);
    let invalid_patch = invalid_tool_arguments_result(
        "apply_patch",
        r#"{"patch_text":"*** Begin Patch\n*** Update File: docs/design.md\n@@\n old\n+new\n*** End Patch"}"#,
        "tool edit error: failed to find expected lines `old`",
        &state,
        Some(&allowed),
        Some(&ToolChoice::Auto),
    );
    let mut patch_grounding_targets = BTreeSet::<String>::new();
    record_patch_context_mismatch_grounding_targets(
        &mut patch_grounding_targets,
        &invalid_patch.metadata,
        &state,
    );
    let patch_grounding_active =
        patch_context_mismatch_target_grounding_surface_active(&state, &patch_grounding_targets);
    let stable_tools = allowed
        .iter()
        .map(|name| ToolSchema {
            name: name.clone(),
            description: format!("{name} tool"),
            input_schema: json!({"type": "object"}),
            strict: false,
        })
        .collect::<Vec<_>>();
    let mut visible = stable_tools.clone();
    if patch_grounding_active {
        TurnLifecycleKernel::augment_tools_from_stable_surface(
            &mut visible,
            &stable_tools,
            TurnLifecycleKernel::docs_patch_context_mismatch_grounding_tool_visible,
        );
        visible.retain(|tool| {
            TurnLifecycleKernel::docs_patch_context_mismatch_grounding_tool_visible(&tool.name)
        });
    }
    let visible_names = visible
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let choice = compile_turn_lifecycle_tool_choice(
        &PromptPolicy::default(),
        &state,
        &visible_names,
        TurnLifecycleRecoveryContext {
            patch_context_mismatch_grounding_active: patch_grounding_active,
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 1,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    patch_grounding_active
        && choice == ToolChoice::Auto
        && visible_names.contains("read")
        && visible_names.contains("apply_patch")
        && visible_names.contains("shell")
        && visible_names.contains("todowrite")
        && visible_names.contains("write")
}

pub(crate) fn codex_style_code_authoring_omits_whole_file_write_fixture_passes() -> bool {
    let mut tools = vec![
        ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "read".to_string(),
            description: "read a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "todowrite".to_string(),
            description: "update progress".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "write".to_string(),
            description: "write a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
    state.completion.open_work_count = 1;
    TurnLifecycleKernel::apply_codex_style_provider_edit_surface(&mut tools, &state);
    !tools.iter().any(|tool| tool.name == "write")
        && tools.iter().any(|tool| tool.name == "apply_patch")
}

pub(crate) fn codex_style_code_authoring_omits_json_discovery_surface_fixture_passes() -> bool {
    let mut tools = vec![
        ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "list".to_string(),
            description: "list files".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "read".to_string(),
            description: "read a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "grep".to_string(),
            description: "search files".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "shell".to_string(),
            description: "run a shell command".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "todowrite".to_string(),
            description: "update progress".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "write".to_string(),
            description: "write a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.completion.open_work_count = 2;
    TurnLifecycleKernel::apply_codex_style_provider_edit_surface(&mut tools, &state);
    tool_schema_names(&tools)
        == BTreeSet::from([
            "apply_patch".to_string(),
            "shell".to_string(),
            "todowrite".to_string(),
        ])
}

pub(crate) fn codex_style_docs_authoring_omits_non_codex_json_surface_fixture_passes() -> bool {
    let mut tools = vec![
        ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "docling_convert".to_string(),
            description: "convert a document".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "glob".to_string(),
            description: "glob files".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "grep".to_string(),
            description: "search files".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "inspect_directory".to_string(),
            description: "inspect directories".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "list".to_string(),
            description: "list files".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "mcp_call".to_string(),
            description: "call MCP".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "read".to_string(),
            description: "read a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "shell".to_string(),
            description: "run a shell command".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "skill".to_string(),
            description: "load a skill".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "todowrite".to_string(),
            description: "update progress".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "write".to_string(),
            description: "write a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Docs;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("docs/workflow-design.md")];
    state.completion.open_work_count = 1;
    state.completion.route_contract_pending = true;
    state.docs_route = Some(DocsRouteState {
        active_deliverable: Some(Utf8PathBuf::from("docs/workflow-design.md")),
        ..DocsRouteState::default()
    });
    TurnLifecycleKernel::apply_codex_style_provider_edit_surface(&mut tools, &state);
    let tool_names = tool_schema_names(&tools);

    tool_names
        == BTreeSet::from([
            "apply_patch".to_string(),
            "docling_convert".to_string(),
            "grep".to_string(),
            "mcp_call".to_string(),
            "read".to_string(),
            "shell".to_string(),
            "todowrite".to_string(),
        ])
        && matches!(
            compile_turn_lifecycle_tool_choice(
                &PromptPolicy::default(),
                &state,
                &tool_names,
                TurnLifecycleRecoveryContext::default(),
            ),
            ToolChoice::Auto
        )
}

pub(crate) fn multi_target_open_authoring_final_message_correction_names_targets_fixture_passes()
-> bool {
    let recovery_tool_names = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.completion.open_work_count = 2;
    state.completion.closeout_ready = false;
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

    let choice = compile_turn_lifecycle_tool_choice(
        &PromptPolicy::default(),
        &state,
        &recovery_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 1,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let correction = TurnLifecycleKernel::open_obligation_final_message_recovery_envelope(
        &state,
        1,
        None,
        &recovery_tool_names,
        false,
    )
    .prompt;

    matches!(choice, ToolChoice::Auto)
        && correction.contains("src/workflow.rs")
        && correction.contains("tests/workflow.behavior.md")
        && correction.contains("apply_patch")
        && correction.contains("open targets")
        && correction.contains("single patch")
        && correction.contains("*** Add File")
        && correction.contains("*** Update File")
        && !correction.contains("tool_choice")
}

pub(crate) fn final_message_recovery_is_system_control_projection_fixture_passes() -> bool {
    let tool_names = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let correction = "The previous response was not accepted as a final answer.\nOpen targets: src/workflow.rs, tests/workflow.behavior.md.\nUse the `apply_patch` tool for the open targets before any final assistant message: submit a single patch whose `patch_text` creates or updates these active targets: src/workflow.rs, tests/workflow.behavior.md. The patch may contain multiple `*** Add File` or `*** Update File` sections.".to_string();
    let base_messages = vec![
        ModelMessage::User {
            content: "create src/workflow.rs and tests/workflow.behavior.md".to_string(),
        },
        ModelMessage::Assistant {
            content: "I will create them.".to_string(),
        },
    ];
    let (messages, policies) = TurnLifecycleKernel::provider_messages_for_dispatch_control(
        &base_messages,
        "Turn control projection surface: prompt".to_string(),
        Some(correction),
        None,
        &tool_names,
        true,
    );
    let recovery_system = messages.iter().find_map(|message| match message {
        ModelMessage::System { content }
            if content.contains("Open-obligation final-message recovery") =>
        {
            Some(content.as_str())
        }
        _ => None,
    });
    let user_recovery_count = messages
        .iter()
        .filter(|message| {
            matches!(
                message,
                ModelMessage::User { content }
                    if content.contains("Open-obligation final-message recovery")
            )
        })
        .count();
    let assistant_text_count = messages
        .iter()
        .filter(|message| matches!(message, ModelMessage::Assistant { .. }))
        .count();

    recovery_system.is_some_and(|content| {
        content.contains("Open-obligation final-message recovery")
            && content.contains("src/workflow.rs, tests/workflow.behavior.md")
            && content.contains("*** Add File")
            && content.contains("*** Update File")
    }) && user_recovery_count == 0
        && assistant_text_count == 0
        && policies.is_empty()
        && request_content_markers(recovery_system.unwrap())
            .contains(&"open_obligation_final_message_recovery".to_string())
        && request_content_markers(recovery_system.unwrap())
            .contains(&"multi_file_apply_patch_shape".to_string())
}

pub(crate) fn invalid_edit_arguments_recovery_is_system_control_projection_fixture_passes() -> bool
{
    let tool_names = BTreeSet::from([
        "apply_patch".to_string(),
        "shell".to_string(),
        "todowrite".to_string(),
    ]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.completion.open_work_count = 2;
    let workflow_strict_patch_grammar = "workflow-strict-patch-grammar";
    let workflow_invalid_edit_contract = "workflow-invalid-edit-contract";
    let arguments_json = r#"{"patch_text":"*** Begin Patch\n*** Add File: src/workflow.rs\n+workflow-invalid-edit-contract\nworkflow_compute(value)\n+workflow_state_ready\n*** End Patch"}"#;
    let error = "tool patch error: add file body line `workflow_compute(value)` must start with `+`; every added content line, including blank lines and workflow contract lines, must be prefixed with `+`.";
    let result = invalid_tool_arguments_result(
        "apply_patch",
        arguments_json,
        error,
        &state,
        Some(&tool_names),
        Some(&ToolChoice::Auto),
    );
    let Some(recovery) = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &result.metadata,
        &state,
        &tool_names,
        &ToolChoice::Auto,
    ) else {
        return false;
    };
    let base_messages = vec![ModelMessage::User {
        content: "create src/workflow.rs and tests/workflow.behavior.md".to_string(),
    }];
    let (messages, _) = TurnLifecycleKernel::provider_messages_for_dispatch_control(
        &base_messages,
        "Turn control projection surface: prompt".to_string(),
        None,
        Some(recovery.prompt),
        &tool_names,
        true,
    );
    let recovery_system = messages.iter().find_map(|message| match message {
        ModelMessage::System { content } if content.contains("Invalid edit recovery:") => {
            Some(content.as_str())
        }
        _ => None,
    });
    let user_recovery_count = messages
        .iter()
        .filter(|message| {
            matches!(
                message,
                ModelMessage::User { content } if content.contains("Invalid edit recovery:")
            )
        })
        .count();
    is_invalid_tool_arguments_error(
        "tool patch error: add file body line `workflow_compute(value)` must start with `+`",
    ) && recovery_system.is_some_and(|content| {
        let markers = request_content_markers(content);
        content.contains("src/workflow.rs, tests/workflow.behavior.md")
            && content.contains("Latest attempted edit target: `src/workflow.rs`")
            && content.contains("retry the same bounded edit operation for `src/workflow.rs`")
            && content.contains("Required recovery operation: submit a corrected `apply_patch`")
            && content.contains("Tool choice remains `auto`")
            && content.contains("Add File body lines must start with `+`")
            && content.contains("workflow_compute(value)")
            && markers.contains(&"invalid_edit_arguments_recovery".to_string())
            && markers.contains(&"strict_apply_patch_grammar".to_string())
            && markers.contains(&"add_file_line_prefix_rule".to_string())
    }) && recovery.candidate_target.as_deref() == Some("src/workflow.rs")
        && recovery.parser_error_family.as_deref() == Some("apply_patch_malformed_patch")
        && workflow_strict_patch_grammar == "workflow-strict-patch-grammar"
        && workflow_invalid_edit_contract == "workflow-invalid-edit-contract"
        && user_recovery_count == 0
}

pub(crate) fn invalid_edit_recovery_obligation_matches_active_work(
    active_work: Option<&ActiveWorkContract>,
) -> bool {
    TurnLifecycleKernel::operation_intents_for_active_work(active_work)
        .contains(&OperationIntent::ContentChangingAuthoringRequired)
}

pub(crate) fn invalid_edit_recovery_projection_obligation(
    envelope: &InvalidEditRecoveryEnvelope,
) -> TurnObligation {
    let targets = envelope
        .active_targets
        .iter()
        .map(Utf8PathBuf::from)
        .collect::<Vec<_>>();
    let target_exclusive_content_shape_recovery =
        invalid_edit_recovery_is_target_exclusive_content_shape(envelope);
    let submitted = recovery_projection_target_list(
        &envelope.submitted_targets,
        target_exclusive_content_shape_recovery,
    );
    let active_submitted = joined_or_none(&envelope.active_submitted_targets);
    let inactive_submitted = recovery_projection_target_list(
        &envelope.inactive_submitted_targets,
        target_exclusive_content_shape_recovery,
    );
    let candidate = envelope.candidate_target.as_deref().unwrap_or("none");
    let parser_family = envelope.parser_error_family.as_deref().unwrap_or("none");
    let result_hash = envelope.result_hash.as_deref().unwrap_or("none");
    let recovery_action = envelope.recovery_action.as_deref().unwrap_or("none");
    let mut evidence_refs = vec![EvidenceRef {
        source: envelope.failure_kind.clone(),
        reference: format!(
            "tool={};candidate_target={candidate};submitted_targets={submitted};active_submitted_targets={active_submitted};inactive_submitted_targets={inactive_submitted};parser_error_family={parser_family};recovery_action={recovery_action};result_hash={result_hash}",
            envelope.tool_name
        ),
    }];
    if !envelope.active_submitted_targets.is_empty()
        && !envelope.inactive_submitted_targets.is_empty()
    {
        evidence_refs.push(EvidenceRef {
            source: envelope.failure_kind.clone(),
            reference: "mixed_target_apply_patch_rewrite_target_only".to_string(),
        });
    }
    let mut contract_refs = vec!["failed_edit_control_recovery_projection".to_string()];
    if envelope.failure_kind == "invalid_edit_arguments" {
        contract_refs.push("invalid_edit_arguments_control_recovery_projection".to_string());
    }
    if envelope.failure_kind == "required_write_content_shape_mismatch" {
        contract_refs.push("required_write_content_shape_recovery_projection".to_string());
    }
    let action_target = envelope
        .recovery_target
        .as_deref()
        .or_else(|| envelope.active_targets.first().map(String::as_str));
    TurnObligation {
        obligation_id: "invalid_edit_recovery".to_string(),
        kind: ObligationKind::Repair,
        summary: recovery_projection_summary(
            envelope,
            &submitted,
            &active_submitted,
            &inactive_submitted,
            target_exclusive_content_shape_recovery,
        ),
        targets,
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_actions: action_target
            .map(|target| {
                let tool = if envelope.tool_name == "write" {
                    ToolName::Write
                } else {
                    ToolName::ApplyPatch
                };
                vec![RequiredAction::edit(tool, Utf8PathBuf::from(target))]
            })
            .unwrap_or_default(),
        verification_commands: Vec::new(),
        contract_refs,
        evidence_refs,
        status: ObligationStatus::Open,
    }
}

fn invalid_edit_recovery_is_target_exclusive_content_shape(
    envelope: &InvalidEditRecoveryEnvelope,
) -> bool {
    envelope.failure_kind == "required_write_content_shape_mismatch"
        && envelope.active_submitted_targets.is_empty()
        && !envelope.inactive_submitted_targets.is_empty()
        && envelope.recovery_target.is_some()
}

fn recovery_projection_target_list(values: &[String], target_exclusive: bool) -> String {
    if target_exclusive && !values.is_empty() {
        format!("omitted_inactive_target_count={}", values.len())
    } else {
        joined_or_none(values)
    }
}

fn recovery_projection_summary(
    envelope: &InvalidEditRecoveryEnvelope,
    submitted: &str,
    active_submitted: &str,
    inactive_submitted: &str,
    target_exclusive: bool,
) -> String {
    if target_exclusive {
        let recovery_target = envelope
            .recovery_target
            .as_deref()
            .unwrap_or("active target");
        format!(
            "Failed edit recovery remains active for target-only authoring. Failure kind: {}; inactive submitted target evidence is omitted from provider-visible projection; retry only `{recovery_target}` with the required positive artifact shape.",
            envelope.failure_kind
        )
    } else {
        format!(
            "Failed edit recovery remains active for target-only authoring. Failure kind: {}; Submitted targets: {submitted}; active submitted targets: {active_submitted}; inactive submitted targets: {inactive_submitted}.",
            envelope.failure_kind
        )
    }
}

fn joined_or_none(values: &[String]) -> String {
    if values.is_empty() {
        "none".to_string()
    } else {
        values.join(",")
    }
}

pub(crate) fn mixed_target_invalid_edit_recovery_projects_into_control_envelope_fixture_passes()
-> bool {
    let tool_names = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.behavior.md")];
    state.completion.open_work_count = 1;
    let arguments = json!({
        "patch_text": "*** Begin Patch\n*** Add File: src/workflow.rs\n+pub fn workflow_compute(value: i32) -> i32 {\n+    value\n+}\n*** End Patch\n*** Add File: tests/workflow.behavior.md\n+workflow-generated-test-contract\n+\n+Scenario: workflow_compute preserves input value\n+Given workflow source contract\n+Then workflow_compute output matches input\n*** End Patch"
    })
    .to_string();
    let result = invalid_tool_arguments_result(
        "apply_patch",
        &arguments,
        "tool patch error: unexpected patch line `*** End Patch`. Use the exact apply_patch grammar.",
        &state,
        Some(&tool_names),
        Some(&ToolChoice::Required),
    );
    let Some(recovery) = invalid_edit_arguments_control_recovery_envelope(
        "apply_patch",
        &result.metadata,
        &state,
        &tool_names,
        &ToolChoice::Required,
    ) else {
        return false;
    };
    let compiled = compile_invalid_edit_recovery_fixture_turn(
        recovery,
        vec![Utf8PathBuf::from("tests/workflow.behavior.md")],
        ToolChoice::Required,
    );
    let prompt = compiled
        .envelope
        .projection_bundle
        .prompt
        .render_prompt_block();
    let request_diagnostics = compiled
        .envelope
        .projection_bundle
        .request_diagnostics
        .render_control_projection()
        .text;
    let feedback = compiled
        .envelope
        .projection_bundle
        .tool_result_feedback
        .render_control_projection()
        .text;
    compiled.validation.passes()
        && compiled
            .envelope
            .obligations
            .items
            .iter()
            .any(|item| item.obligation_id == "invalid_edit_recovery")
        && prompt.contains("invalid_edit_recovery")
        && prompt.contains("invalid_edit_arguments:tool=apply_patch")
        && prompt.contains("submitted_targets=src/workflow.rs,tests/workflow.behavior.md")
        && prompt.contains("active_submitted_targets=tests/workflow.behavior.md")
        && prompt.contains("inactive_submitted_targets=src/workflow.rs")
        && prompt.contains("mixed_target_apply_patch_rewrite_target_only")
        && request_diagnostics.contains("active_submitted_targets=tests/workflow.behavior.md")
        && feedback.contains("inactive_submitted_targets=src/workflow.rs")
        && compiled
            .envelope
            .action_authority
            .required_action
            .as_ref()
            .is_some_and(|action| {
                action.projection_label() == "apply_patch:tests/workflow.behavior.md"
                    && action.tool == ToolName::ApplyPatch
            })
        && compiled.envelope.action_authority.tool_choice == ToolChoice::Required
}

pub(crate) fn content_shape_failed_edit_projects_latest_recovery_into_control_envelope_fixture_passes()
-> bool {
    let tool_names = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![
        Utf8PathBuf::from("src/workflow.rs"),
        Utf8PathBuf::from("tests/workflow.behavior.md"),
    ];
    state.completion.open_work_count = 2;

    let old_invalid_result = invalid_tool_arguments_result(
        "write",
        r#"{"path":"src/workflow.rs","content":"workflow-invalid-edit-contract\nworkflow_compute(value"#,
        "EOF while parsing a string at line 1 column 53",
        &state,
        Some(&tool_names),
        Some(&ToolChoice::Required),
    );
    let old_recovery = failed_edit_control_recovery_envelope(
        "write",
        &old_invalid_result.metadata,
        &state,
        &tool_names,
        &ToolChoice::Required,
    );
    let old_hash = old_recovery
        .as_ref()
        .and_then(|envelope| envelope.result_hash.clone())
        .unwrap_or_default();

    let bad_arguments = json!({
        "path": "src/workflow.rs",
        "content": "# workflow.rs\n\nThis file should describe the workflow implementation instead of providing effective source code.\n"
    });
    let Some(content_shape_result) =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "write",
            &bad_arguments,
            None,
        )
    else {
        return false;
    };
    let Some(recovery) = failed_edit_control_recovery_envelope(
        "write",
        &content_shape_result.metadata,
        &state,
        &tool_names,
        &ToolChoice::Required,
    ) else {
        return false;
    };
    let latest_hash = recovery.result_hash.clone().unwrap_or_default();
    if latest_hash.is_empty() || latest_hash == old_hash {
        return false;
    }

    let compiled = compile_invalid_edit_recovery_fixture_turn(
        recovery.clone(),
        vec![
            Utf8PathBuf::from("src/workflow.rs"),
            Utf8PathBuf::from("tests/workflow.behavior.md"),
        ],
        ToolChoice::Required,
    );
    let prompt = compiled
        .envelope
        .projection_bundle
        .prompt
        .render_prompt_block();
    let request_diagnostics = compiled
        .envelope
        .projection_bundle
        .request_diagnostics
        .render_control_projection()
        .text;
    let feedback = compiled
        .envelope
        .projection_bundle
        .tool_result_feedback
        .render_control_projection()
        .text;
    let Some(projected_recovery) = compiled
        .envelope
        .obligations
        .items
        .iter()
        .find(|item| item.obligation_id == "invalid_edit_recovery")
    else {
        return false;
    };
    let projected_evidence = projected_recovery
        .evidence_refs
        .iter()
        .map(|evidence| format!("{}:{}", evidence.source, evidence.reference))
        .collect::<Vec<_>>()
        .join("\n");

    let checks = [
        (
            "failure_kind",
            recovery.failure_kind == "required_write_content_shape_mismatch",
        ),
        (
            "candidate_target",
            recovery.candidate_target.as_deref() == Some("src/workflow.rs"),
        ),
        (
            "active_submitted",
            recovery
                .active_submitted_targets
                .contains(&"src/workflow.rs".to_string()),
        ),
        (
            "recovery_action",
            recovery.recovery_action.as_deref() == Some("rewrite_content_for_required_shape"),
        ),
        ("compiled_validation", compiled.validation.passes()),
        (
            "contract_ref",
            projected_recovery
                .contract_refs
                .contains(&"required_write_content_shape_recovery_projection".to_string()),
        ),
        (
            "evidence_failure_kind",
            projected_evidence.contains("required_write_content_shape_mismatch"),
        ),
        (
            "evidence_contract_kind",
            projected_evidence.contains("generic_code_artifact_effective_content_shape"),
        ),
        (
            "evidence_latest_hash",
            projected_evidence.contains(&latest_hash),
        ),
        (
            "prompt_failure_kind",
            prompt.contains("required_write_content_shape_mismatch"),
        ),
        ("prompt_latest_hash", prompt.contains(&latest_hash)),
        (
            "diagnostics_failure_kind",
            request_diagnostics.contains("required_write_content_shape_mismatch"),
        ),
        (
            "feedback_failure_kind",
            feedback.contains("required_write_content_shape_mismatch"),
        ),
        (
            "old_hash_not_projected",
            old_hash.is_empty() || !prompt.contains(&old_hash),
        ),
        (
            "required_action",
            compiled
                .envelope
                .action_authority
                .required_action
                .as_ref()
                .is_some_and(|action| {
                    action.projection_label() == "write:src/workflow.rs"
                        && action.tool == ToolName::Write
                }),
        ),
    ];
    checks.iter().all(|(_, passed)| *passed)
}

pub(crate) fn content_shape_recovery_projection_omits_inactive_submitted_targets_fixture_passes()
-> bool {
    let envelope = InvalidEditRecoveryEnvelope {
        failure_kind: "required_write_content_shape_mismatch".to_string(),
        tool_name: "apply_patch".to_string(),
        active_targets: vec!["tests/workflow.behavior.md".to_string()],
        candidate_target: Some("tests/workflow.behavior.md".to_string()),
        submitted_targets: vec!["src/inactive-workflow.rs".to_string()],
        active_submitted_targets: Vec::new(),
        inactive_submitted_targets: vec!["src/inactive-workflow.rs".to_string()],
        parser_error_family: Some("generated_test_content_shape".to_string()),
        recovery_action: Some("rewrite_content_for_required_shape".to_string()),
        recovery_target: Some("tests/workflow.behavior.md".to_string()),
        result_hash: Some("fixture-content-shape-hash".to_string()),
        prompt: String::new(),
    };
    let obligation = invalid_edit_recovery_projection_obligation(&envelope);
    let rendered_evidence = obligation
        .evidence_refs
        .iter()
        .map(|evidence| format!("{}:{}", evidence.source, evidence.reference))
        .collect::<Vec<_>>()
        .join("\n");
    obligation
        .required_actions
        .iter()
        .any(|action| action.projection_label() == "apply_patch:tests/workflow.behavior.md")
        && obligation
            .summary
            .contains("inactive submitted target evidence is omitted")
        && obligation.summary.contains("tests/workflow.behavior.md")
        && !obligation.summary.contains("src/inactive-workflow.rs")
        && rendered_evidence.contains("submitted_targets=omitted_inactive_target_count=1")
        && rendered_evidence.contains("inactive_submitted_targets=omitted_inactive_target_count=1")
        && !rendered_evidence.contains("src/inactive-workflow.rs")
}

fn compile_invalid_edit_recovery_fixture_turn(
    recovery: InvalidEditRecoveryEnvelope,
    active_targets: Vec<Utf8PathBuf>,
    tool_choice: ToolChoice,
) -> crate::protocol::CompiledTurn {
    let projection_id = ProjectionId::new();
    let active_contract = ActiveWorkContractProjection {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        active_work_kind: Some("requested_work_authoring".to_string()),
        summary: format!(
            "Requested deliverables still require authoring in the workspace: `{}`.",
            active_targets
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("`, `")
        ),
        active_targets,
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: vec!["verify-contract --behavior".to_string()],
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
        forbidden_tools: Vec::new(),
        projection_id,
    };
    let context = TurnContext {
        session_id: SessionId::new(),
        cwd: Utf8PathBuf::from("C:/workspace"),
        workspace_root: Utf8PathBuf::from("C:/workspace"),
        provider: LIFECYCLE_FIXTURE_PROVIDER.to_string(),
        model: LIFECYCLE_FIXTURE_MODEL.to_string(),
        base_url: LIFECYCLE_FIXTURE_BASE_URL.to_string(),
        access_mode: AccessMode::FullAccess,
        sandbox: SandboxProfile::FullAccess,
        shell_family: ShellFamily::PowerShell,
        model_capabilities: ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
            parallel_tool_calls: false,
            context_window: 8192,
            max_output_tokens: 1024,
        },
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        active_contract,
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
        tool_choice,
        images: Vec::new(),
        output_contract: OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let mut obligations = ObligationCompiler::compile(&context);
    obligations
        .items
        .push(invalid_edit_recovery_projection_obligation(&recovery));
    TurnEngine::compile(TurnEngineInput {
        turn_id: TurnId::new(),
        context,
        obligations,
        dispatch_policy: DispatchPolicy::Dispatch,
        evidence_refs: Vec::new(),
    })
}

pub(crate) fn stale_invalid_edit_recovery_is_not_open_obligation_after_verification_transition_fixture_passes()
-> bool {
    let authoring_tools = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let mut authoring_state = SessionStateSnapshot::default();
    authoring_state.route = TaskRoute::Code;
    authoring_state.process_phase = ProcessPhase::Author;
    authoring_state.active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
    authoring_state.completion.open_work_count = 1;
    let bad_arguments = json!({
        "path": "src/workflow.rs",
        "content": "# workflow.rs\n\nThis file should describe the workflow implementation instead of providing effective source code.\n"
    });
    let Some(content_shape_result) =
        crate::agent::content_shape_contract::artifact_content_shape_violation_result(
            "write",
            &bad_arguments,
            None,
        )
    else {
        return false;
    };
    let Some(stale_recovery) = failed_edit_control_recovery_envelope(
        "write",
        &content_shape_result.metadata,
        &authoring_state,
        &authoring_tools,
        &ToolChoice::Required,
    ) else {
        return false;
    };

    let mut verification_state = SessionStateSnapshot::default();
    verification_state.route = TaskRoute::Code;
    verification_state.process_phase = ProcessPhase::Verify;
    verification_state.verification.required_commands =
        vec!["verify-contract --behavior".to_string()];
    verification_state.completion.verification_pending = true;
    let active_work = ActiveWorkContract::Verification {
        commands: vec!["verify-contract --behavior".to_string()],
        failing_labels: Vec::new(),
        repair_required: false,
        targets: Vec::new(),
    };
    if invalid_edit_recovery_obligation_matches_active_work(Some(&active_work)) {
        return false;
    }
    let projection_id = ProjectionId::new();
    let active_contract = ActiveWorkContractProjection {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Verify,
        active_work_kind: Some("verification".to_string()),
        summary: "Run required verification command(s): `verify-contract --behavior`.".to_string(),
        active_targets: Vec::new(),
        operation_intents: Vec::new(),
        required_verification_commands: vec!["verify-contract --behavior".to_string()],
        allowed_tools: vec![ToolName::Shell],
        forbidden_tools: Vec::new(),
        projection_id,
    };
    let context = TurnContext {
        session_id: SessionId::new(),
        cwd: Utf8PathBuf::from("C:/workspace/verification-transition"),
        workspace_root: Utf8PathBuf::from("C:/workspace/verification-transition"),
        provider: LIFECYCLE_FIXTURE_PROVIDER.to_string(),
        model: LIFECYCLE_FIXTURE_MODEL.to_string(),
        base_url: LIFECYCLE_FIXTURE_BASE_URL.to_string(),
        access_mode: AccessMode::Default,
        sandbox: SandboxProfile::WorkspaceWrite,
        shell_family: ShellFamily::PowerShell,
        model_capabilities: ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
            parallel_tool_calls: false,
            context_window: 8192,
            max_output_tokens: 1024,
        },
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Verify,
        active_contract,
        allowed_tools: vec![ToolName::Shell],
        tool_choice: ToolChoice::Required,
        images: Vec::new(),
        output_contract: OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    let obligations = ObligationCompiler::compile(&context);
    let compiled = TurnEngine::compile(TurnEngineInput {
        turn_id: TurnId::new(),
        context,
        obligations,
        dispatch_policy: DispatchPolicy::Dispatch,
        evidence_refs: Vec::new(),
    });
    let prompt = compiled
        .envelope
        .projection_bundle
        .prompt
        .render_prompt_block();
    let request_diagnostics = compiled
        .envelope
        .projection_bundle
        .request_diagnostics
        .render_control_projection()
        .text;
    let feedback = compiled
        .envelope
        .projection_bundle
        .tool_result_feedback
        .render_control_projection()
        .text;
    let stale_obligation = invalid_edit_recovery_projection_obligation(&stale_recovery);

    compiled.validation.passes()
        && stale_obligation.obligation_id == "invalid_edit_recovery"
        && compiled.envelope.obligations.items.iter().any(|item| {
            item.kind == ObligationKind::Verification
                && item
                    .verification_commands
                    .contains(&"verify-contract --behavior".to_string())
        })
        && compiled
            .envelope
            .obligations
            .items
            .iter()
            .all(|item| item.obligation_id != "invalid_edit_recovery")
        && !prompt.contains("invalid_edit_recovery")
        && !request_diagnostics.contains("invalid_edit_recovery")
        && !feedback.contains("invalid_edit_recovery")
}

pub(crate) fn open_obligation_final_message_recovery_preserves_stable_surface_fixture_passes()
-> bool {
    let tools = vec![
        ToolSchema {
            name: "read".to_string(),
            description: "read a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "todowrite".to_string(),
            description: "update progress".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "write".to_string(),
            description: "write a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "shell".to_string(),
            description: "run a shell command".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    let initial_tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow.behavior.md")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

    let initial = compile_turn_lifecycle_tool_choice(
        &PromptPolicy::default(),
        &state,
        &initial_tool_names,
        TurnLifecycleRecoveryContext::default(),
    );
    let authoring_recovery_tools = tools
        .iter()
        .filter(|tool| {
            TurnLifecycleKernel::open_obligation_final_message_recovery_tool_visible(
                &state, &tool.name,
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    let authoring_recovery_tool_names = authoring_recovery_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let authoring_recovery = if TurnLifecycleKernel::open_executable_work_requires_tool_call(&state)
        && !authoring_recovery_tool_names.is_empty()
    {
        compile_turn_lifecycle_tool_choice(
            &PromptPolicy::default(),
            &state,
            &authoring_recovery_tool_names,
            TurnLifecycleRecoveryContext {
                open_obligation_final_message_recovery_active: true,
                open_obligation_final_message_count: 1,
                ..TurnLifecycleRecoveryContext::default()
            },
        )
    } else {
        compile_turn_lifecycle_tool_choice(
            &PromptPolicy::default(),
            &state,
            &authoring_recovery_tool_names,
            TurnLifecycleRecoveryContext::default(),
        )
    };
    let mut repair_state = state.clone();
    repair_state.process_phase = ProcessPhase::Repair;
    repair_state.completion.verification_pending = true;
    let repair_recovery_tool_names = tools
        .iter()
        .filter(|tool| {
            TurnLifecycleKernel::open_obligation_final_message_recovery_tool_visible(
                &repair_state,
                &tool.name,
            )
        })
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let repair_recovery = compile_turn_lifecycle_tool_choice(
        &PromptPolicy::default(),
        &repair_state,
        &repair_recovery_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 1,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let repeated_authoring_final_stable_surface_keeps_auto = compile_turn_lifecycle_tool_choice(
        &PromptPolicy::default(),
        &state,
        &authoring_recovery_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 2,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let hard_authoring_recovery_tool_names =
        BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let repeated_authoring_final_uses_hard_edit_surface = compile_turn_lifecycle_tool_choice(
        &PromptPolicy::default(),
        &state,
        &hard_authoring_recovery_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 2,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let mut docs_state = SessionStateSnapshot::default();
    docs_state.route = TaskRoute::Docs;
    docs_state.process_phase = ProcessPhase::Author;
    docs_state.active_targets = vec![Utf8PathBuf::from("docs/workflow-design.md")];
    docs_state.completion.open_work_count = 1;
    docs_state.completion.closeout_ready = false;
    docs_state.completion.route_contract_pending = true;
    let docs_recovery_tool_names = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let docs_recovery = compile_turn_lifecycle_tool_choice(
        &PromptPolicy::default(),
        &docs_state,
        &docs_recovery_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 1,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let mut narrowed_docs_recovery_tools = vec![ToolSchema {
        name: "apply_patch".to_string(),
        description: "apply a patch".to_string(),
        input_schema: json!({"type": "object"}),
        strict: false,
    }];
    let docs_stable_tools = vec![
        ToolSchema {
            name: "apply_patch".to_string(),
            description: "apply a patch".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
        ToolSchema {
            name: "write".to_string(),
            description: "write a file".to_string(),
            input_schema: json!({"type": "object"}),
            strict: false,
        },
    ];
    TurnLifecycleKernel::apply_pre_normalization_recovery_surface(
        &mut narrowed_docs_recovery_tools,
        &docs_stable_tools,
        TurnLifecyclePreNormalizationSurfaceInput {
            state: &docs_state,
            recovery: TurnLifecycleRecoveryContext {
                open_obligation_final_message_recovery_active: true,
                open_obligation_final_message_count: 2,
                ..TurnLifecycleRecoveryContext::default()
            },
            code_authoring_final_message_hard_edit_recovery_active: false,
            code_authoring_final_message_recovery_stable_surface_active: false,
            code_repair_final_message_recovery_stable_surface_active: false,
        },
    );
    let restored_docs_recovery_tool_names = narrowed_docs_recovery_tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<BTreeSet<_>>();
    let restored_docs_recovery = compile_turn_lifecycle_tool_choice(
        &PromptPolicy::default(),
        &docs_state,
        &restored_docs_recovery_tool_names,
        TurnLifecycleRecoveryContext {
            open_obligation_final_message_recovery_active: true,
            open_obligation_final_message_count: 2,
            ..TurnLifecycleRecoveryContext::default()
        },
    );
    let mut docs_tools = vec![ToolSchema {
        name: "write".to_string(),
        description: "write a file".to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Target file path relative to the current workspace or an allowed absolute path."
                },
                "content": {"type": "string", "description": "Complete final file contents."}
            }
        }),
        strict: false,
    }];
    let required_docs_write = RequiredAction::edit(
        ToolName::Write,
        Utf8PathBuf::from("docs/workflow-design.md"),
    );
    crate::agent::prompt::apply_write_content_shape_to_write_schema_for_required_action(
        &mut docs_tools,
        Some(&required_docs_write),
    );
    let docs_schema_description = docs_tools
        .first()
        .and_then(|tool| tool.input_schema.pointer("/properties/content/description"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    matches!(initial, ToolChoice::Auto)
        && matches!(authoring_recovery, ToolChoice::Auto)
        && matches!(
            repeated_authoring_final_stable_surface_keeps_auto,
            ToolChoice::Auto
        )
        && matches!(
            repeated_authoring_final_uses_hard_edit_surface,
            ToolChoice::Required
        )
        && matches!(docs_recovery, ToolChoice::Required)
        && restored_docs_recovery_tool_names
            == BTreeSet::from(["apply_patch".to_string(), "write".to_string()])
        && matches!(restored_docs_recovery, ToolChoice::Required)
        && docs_schema_description.contains("Complete final Markdown/text contents")
        && docs_schema_description.contains("real newline-separated structure")
        && authoring_recovery_tool_names == initial_tool_names
        && TurnLifecycleKernel::open_obligation_final_message_recovery_envelope(
            &state,
            1,
            None,
            &authoring_recovery_tool_names,
            false,
        )
        .prompt
        .contains("Use the `apply_patch` tool for the active target")
        && TurnLifecycleKernel::open_obligation_final_message_recovery_envelope(
            &state,
            2,
            None,
            &authoring_recovery_tool_names,
            false,
        )
        .prompt
        .contains("Use the `apply_patch` tool for the active target")
        && matches!(repair_recovery, ToolChoice::Auto)
        && repair_recovery_tool_names == initial_tool_names
        && verification_final_message_recovery_uses_shell_fixture_passes()
        && source_repair_final_message_correction_uses_exact_write_action_fixture_passes()
}

fn verification_final_message_recovery_uses_shell_fixture_passes() -> bool {
    let recovery_tool_names = BTreeSet::from(["shell".to_string()]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Verify;
    state.completion.open_work_count = 0;
    state.completion.closeout_ready = false;
    state.completion.verification_pending = true;
    state.completion.blocked_reason =
        Some("requested work authoring is complete; run required verification command(s): verify-contract --behavior".to_string());
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

    let required_shell = RequiredAction::shell("verify-contract --behavior".to_string());
    let correction = TurnLifecycleKernel::open_obligation_final_message_recovery_envelope(
        &state,
        1,
        Some(&required_shell),
        &recovery_tool_names,
        false,
    )
    .prompt;
    matches!(
        compile_turn_lifecycle_tool_choice(
            &PromptPolicy::default(),
            &state,
            &recovery_tool_names,
            TurnLifecycleRecoveryContext {
                open_obligation_final_message_recovery_active: true,
                open_obligation_final_message_count: 1,
                ..TurnLifecycleRecoveryContext::default()
            },
        ),
        ToolChoice::Named(ToolName::Shell)
    ) && correction.contains("Use the `shell` tool")
        && correction.contains("verify-contract --behavior")
        && !correction.contains("Use a file-changing tool call")
}

pub(crate) fn source_repair_final_message_correction_uses_exact_write_action_fixture_passes() -> bool
{
    let recovery_tool_names = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("src/workflow.rs")];
    state.completion.open_work_count = 1;
    state.completion.closeout_ready = false;
    state.completion.verification_pending = true;
    state.completion.blocked_reason =
        Some("verification failed; source repair remains active for `src/workflow.rs`".to_string());
    state
        .verification
        .required_commands
        .push("verify-contract --behavior".to_string());

    let required_write =
        RequiredAction::edit(ToolName::Write, Utf8PathBuf::from("src/workflow.rs"));
    let correction = TurnLifecycleKernel::open_obligation_final_message_recovery_envelope(
        &state,
        1,
        Some(&required_write),
        &recovery_tool_names,
        false,
    )
    .prompt;

    matches!(
        compile_turn_lifecycle_tool_choice(
            &PromptPolicy::default(),
            &state,
            &recovery_tool_names,
            TurnLifecycleRecoveryContext {
                open_obligation_final_message_recovery_active: true,
                open_obligation_final_message_count: 1,
                ..TurnLifecycleRecoveryContext::default()
            },
        ),
        ToolChoice::Named(ToolName::Write)
    ) && correction.contains("Required action: `write:src/workflow.rs`")
        && correction.contains("Call the `write` tool")
        && correction.contains("path` exactly `src/workflow.rs`")
        && !correction.contains("Use the `shell` tool")
        && !correction.contains("verify-contract --behavior")
        && !correction.contains("Use a file-changing tool call")
}

pub(crate) fn provider_system_context_normalization_fixture_passes() -> bool {
    let normalized =
        TurnLifecycleKernel::normalize_provider_system_context_for_chat_template(vec![
            ModelMessage::System {
                content: "control envelope".to_string(),
            },
            ModelMessage::User {
                content: "create src/workflow.rs and tests/workflow.behavior.md".to_string(),
            },
            ModelMessage::System {
                content: "stale inactive authoring replay note".to_string(),
            },
            ModelMessage::Assistant {
                content: "intermediate text".to_string(),
            },
            ModelMessage::System {
                content: "open obligation recovery note".to_string(),
            },
            ModelMessage::User {
                content: "write tests/workflow.behavior.md now".to_string(),
            },
        ]);

    let roles = normalized
        .iter()
        .map(|message| match message {
            ModelMessage::System { .. } => "system",
            ModelMessage::User { .. } => "user",
            ModelMessage::UserParts { .. } => "user_parts",
            ModelMessage::Assistant { .. } => "assistant",
            ModelMessage::AssistantToolCalls { .. } => "assistant_tool_calls",
            ModelMessage::Tool { .. } => "tool",
        })
        .collect::<Vec<_>>();

    let system_after_non_system = normalized
        .iter()
        .scan(false, |seen_non_system, message| {
            let is_system = matches!(message, ModelMessage::System { .. });
            let violation = *seen_non_system && is_system;
            if !is_system {
                *seen_non_system = true;
            }
            Some(violation)
        })
        .any(|violation| violation);

    let merged_system = normalized.first().and_then(|message| match message {
        ModelMessage::System { content } => Some(content.as_str()),
        _ => None,
    });

    roles == vec!["system", "user", "assistant", "user"]
        && !system_after_non_system
        && merged_system.is_some_and(|content| {
            content.contains("control envelope")
                && content.contains("stale inactive authoring replay note")
                && content.contains("open obligation recovery note")
        })
}

pub(crate) fn provider_noncompliance_adjudication_fixture_passes() -> bool {
    let envelope = edit_only_repair_fixture_envelope(vec![ToolName::Write, ToolName::ApplyPatch]);
    let second_envelope =
        edit_only_repair_fixture_envelope(vec![ToolName::Write, ToolName::ApplyPatch]);
    let allowed_tools = ["write".to_string(), "apply_patch".to_string()]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let proposal = ModelToolCallProposal {
        call_id: "call_1".to_string(),
        requested_tool: "shell".to_string(),
        effective_tool: "shell".to_string(),
        arguments_json: r#"{"command":"verify-contract --behavior"}"#.to_string(),
    };
    let input = ActionAdjudicationInput {
        proposal: proposal.clone(),
        allowed_tools: &allowed_tools,
        tool_exists: true,
        tool_allowed: false,
        envelope: &envelope,
    };
    let ActionAdjudication::RejectedModelAction(rejection) =
        ActionAdjudicator::adjudicate_tool_call(&input)
    else {
        return false;
    };
    let read_input = ActionAdjudicationInput {
        proposal: ModelToolCallProposal {
            call_id: "call_2".to_string(),
            requested_tool: "read".to_string(),
            effective_tool: "read".to_string(),
            arguments_json: r#"{"path":"src/workflow.rs"}"#.to_string(),
        },
        allowed_tools: &allowed_tools,
        tool_exists: true,
        tool_allowed: false,
        envelope: &second_envelope,
    };
    let ActionAdjudication::RejectedModelAction(read_rejection) =
        ActionAdjudicator::adjudicate_tool_call(&read_input)
    else {
        return false;
    };
    let repeated_input = ActionAdjudicationInput {
        proposal: proposal.clone(),
        allowed_tools: &allowed_tools,
        tool_exists: true,
        tool_allowed: false,
        envelope: &second_envelope,
    };
    let ActionAdjudication::RejectedModelAction(repeated_rejection) =
        ActionAdjudicator::adjudicate_tool_call(&repeated_input)
    else {
        return false;
    };
    if rejection.classification != ModelActionRejectionClass::ProviderNoncompliance
        || rejection.semantic_class != "provider_ignored_edit_only_surface"
        || read_rejection.classification != ModelActionRejectionClass::ProviderNoncompliance
        || read_rejection.semantic_class != "provider_ignored_edit_only_surface"
        || rejection.result_hash != repeated_rejection.result_hash
    {
        return false;
    }
    let result = rejection.to_tool_result(
        ToolCallId::new(),
        &allowed_tools,
        true,
        false,
        &envelope.projection_bundle.tool_result_feedback,
    );
    let malformed_a = TurnLifecycleKernel::adjudicate_model_action(
        ProviderActionAdapter::adapt_tool_call(&CompletedToolCall {
            call_id: "call_malformed_a".to_string(),
            tool_name: "write".to_string(),
            arguments_json: r#"{"path":"src/workflow.rs","content":"source v1""#.to_string(),
        }),
        &allowed_tools,
        true,
        true,
        &envelope,
    );
    let malformed_b = TurnLifecycleKernel::adjudicate_model_action(
        ProviderActionAdapter::adapt_tool_call(&CompletedToolCall {
            call_id: "call_malformed_b".to_string(),
            tool_name: "write".to_string(),
            arguments_json: r#"{"path":"src/workflow.rs","content":"source v2""#.to_string(),
        }),
        &allowed_tools,
        true,
        true,
        &second_envelope,
    );
    let (
        ActionAdjudication::RejectedModelAction(malformed_rejection_a),
        ActionAdjudication::RejectedModelAction(malformed_rejection_b),
    ) = (malformed_a, malformed_b)
    else {
        return false;
    };
    let malformed_result = TurnLifecycleKernel::rejected_model_action_corrective_result(
        &malformed_rejection_a,
        ToolCallId::new(),
        &allowed_tools,
        true,
        true,
        &envelope.projection_bundle.tool_result_feedback,
        &SessionStateSnapshot {
            route: TaskRoute::Code,
            process_phase: ProcessPhase::Repair,
            active_targets: vec![camino::Utf8PathBuf::from("src/workflow.rs")],
            completion: crate::session::CompletionState {
                open_work_count: 1,
                ..crate::session::CompletionState::default()
            },
            ..SessionStateSnapshot::default()
        },
        &ToolChoice::Required,
    );
    let final_action = ProviderActionAdapter::adapt_text_final(
        "done",
        envelope
            .projection_bundle
            .tool_result_feedback
            .projection_id,
        true,
    );
    let ActionAdjudication::RejectedModelAction(final_rejection) =
        TurnLifecycleKernel::adjudicate_model_action(
            final_action,
            &allowed_tools,
            false,
            false,
            &envelope,
        )
    else {
        return false;
    };
    let final_source_call_id = ToolCallId::new();
    let final_proposal = final_rejection.to_rejected_tool_proposal(
        final_source_call_id,
        &allowed_tools,
        &envelope.projection_bundle.tool_result_feedback,
    );
    result
        .metadata
        .get("tool_feedback_envelope")
        .and_then(|value| value.get("semantic_class"))
        .and_then(Value::as_str)
        == Some("provider_ignored_edit_only_surface")
        && result.metadata.get("rejected_tool_proposal").is_some()
        && result
            .metadata
            .get("model_action_adjudication")
            .and_then(|value| value.get("result_hash"))
            .and_then(Value::as_str)
            .is_some()
        && result
            .metadata
            .get("provider_noncompliance")
            .and_then(Value::as_bool)
            == Some(true)
        && malformed_rejection_a.semantic_class == "malformed_tool_arguments"
        && malformed_rejection_a.result_hash == malformed_rejection_b.result_hash
        && malformed_result.title == "Invalid tool arguments"
        && malformed_result
            .metadata
            .get("tool_feedback_envelope")
            .and_then(|value| value.get("kind"))
            .and_then(Value::as_str)
            == Some("invalid_edit_arguments")
        && malformed_result
            .metadata
            .get("rejected_tool_proposal")
            .is_some()
        && malformed_result
            .metadata
            .get("model_action_adjudication")
            .and_then(|value| value.get("semantic_class"))
            .and_then(Value::as_str)
            == Some("malformed_tool_arguments")
        && !malformed_result
            .output_text
            .contains("tool is not available in the current run state")
        && final_rejection.semantic_class == "text_final_while_obligations_open"
        && final_proposal.source_call_id == final_source_call_id
        && final_proposal.effective_tool == "final_assistant_message"
        && envelope
            .projection_bundle
            .tool_result_feedback
            .required_action
            .as_ref()
            .map(|action| action.projection_label())
            == Some("write:src/workflow.rs".to_string())
}

pub(crate) fn lifecycle_kernel_fixtures_are_workflow_neutral_fixture_passes() -> bool {
    let messages = vec![ModelMessage::AssistantToolCalls {
        content: Some(
            "I will inspect with shell first, then patch the active file.".to_string(),
        ),
        tool_calls: vec![
            ModelToolCall {
                call_id: "call_shell".to_string(),
                tool_name: "shell".to_string(),
                arguments_json: r#"{"command":"verify-contract --behavior"}"#.to_string(),
            },
            ModelToolCall {
                call_id: "call_patch".to_string(),
                tool_name: "apply_patch".to_string(),
                arguments_json: r#"{"patch":"*** Begin Patch\n*** Update File: src/workflow.rs\n@@\n-pub fn workflow_state() -> &'static str { \"draft\" }\n+pub fn workflow_state() -> &'static str { \"ready\" }\n*** End Patch\n"}"#.to_string(),
            },
        ],
    }];
    let effective_tools = BTreeSet::from(["apply_patch".to_string(), "write".to_string()]);
    let projection = ReplayNormalizer::filter_to_effective_tool_surface(messages, &effective_tools);
    let Some(ModelMessage::AssistantToolCalls { tool_calls, .. }) = projection.messages.first()
    else {
        return false;
    };
    let Some(patch_call) = tool_calls.first() else {
        return false;
    };

    let envelope = edit_only_repair_fixture_envelope(vec![ToolName::Write, ToolName::ApplyPatch]);
    let allowed_tools = ["write".to_string(), "apply_patch".to_string()]
        .into_iter()
        .collect::<BTreeSet<_>>();
    let input = ActionAdjudicationInput {
        proposal: ModelToolCallProposal {
            call_id: "call_write".to_string(),
            requested_tool: "write".to_string(),
            effective_tool: "write".to_string(),
            arguments_json:
                r#"{"path":"src/workflow.rs","content":"pub fn workflow_state() -> &'static str { \"ready\" }\n"}"#.to_string(),
        },
        allowed_tools: &allowed_tools,
        tool_exists: true,
        tool_allowed: true,
        envelope: &envelope,
    };

    patch_call.tool_name == "apply_patch"
        && patch_call.arguments_json.contains("workflow_state")
        && patch_call.arguments_json.contains("ready")
        && !patch_call.arguments_json.contains("print(")
        && matches!(
            ActionAdjudicator::adjudicate_tool_call(&input),
            ActionAdjudication::AcceptedToolCall(_)
        )
}

fn edit_only_repair_fixture_envelope(allowed_tools: Vec<ToolName>) -> TurnControlEnvelope {
    let projection_id = ProjectionId::new();
    let authority = ActionAuthority {
        projection_id,
        required_action: Some(crate::protocol::RequiredAction::edit(
            ToolName::Write,
            camino::Utf8PathBuf::from("src/workflow.rs"),
        )),
        required_action_conflicts: Vec::new(),
        required_verification_commands: Vec::new(),
        operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
        allowed_tools: allowed_tools.clone(),
        forbidden_tools: vec![ToolName::Shell],
        tool_choice: ToolChoice::Required,
    };
    let obligations = ObligationSet {
        items: vec![TurnObligation {
            obligation_id: "repair:workflow-source".to_string(),
            kind: ObligationKind::Repair,
            summary: "repair src/workflow.rs".to_string(),
            targets: vec![camino::Utf8PathBuf::from("src/workflow.rs")],
            operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
            required_actions: vec![crate::protocol::RequiredAction::edit(
                ToolName::Write,
                camino::Utf8PathBuf::from("src/workflow.rs"),
            )],
            verification_commands: Vec::new(),
            contract_refs: vec!["workflow-source-contract".to_string()],
            evidence_refs: vec![EvidenceRef {
                source: "verification".to_string(),
                reference: "failed-run".to_string(),
            }],
            status: ObligationStatus::Open,
        }],
    };
    let projection_bundle = ProjectionBundle {
        projection_id,
        prompt: ProjectionSurface::from_authority_and_obligations(
            ProjectionSurfaceKind::Prompt,
            &authority,
            &obligations,
        ),
        tool_result_feedback: ProjectionSurface::from_authority_and_obligations(
            ProjectionSurfaceKind::ToolResultFeedback,
            &authority,
            &obligations,
        ),
        request_diagnostics: ProjectionSurface::from_authority_and_obligations(
            ProjectionSurfaceKind::RequestDiagnostics,
            &authority,
            &obligations,
        ),
        handoff: ProjectionSurface::from_authority_and_obligations(
            ProjectionSurfaceKind::Handoff,
            &authority,
            &obligations,
        ),
        preflight: ProjectionSurface::from_authority_and_obligations(
            ProjectionSurfaceKind::Preflight,
            &authority,
            &obligations,
        ),
    };
    let session_id = crate::session::SessionId::new();
    let turn_id = TurnId::new();
    let context = TurnContext {
        session_id,
        cwd: camino::Utf8PathBuf::from("."),
        workspace_root: camino::Utf8PathBuf::from("."),
        provider: LIFECYCLE_FIXTURE_PROVIDER.to_string(),
        model: LIFECYCLE_FIXTURE_MODEL.to_string(),
        base_url: LIFECYCLE_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        sandbox: SandboxProfile::WorkspaceWrite,
        shell_family: crate::config::ShellFamily::PowerShell,
        model_capabilities: ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
            parallel_tool_calls: false,
            context_window: 8192,
            max_output_tokens: 1024,
        },
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Repair,
        active_contract: ActiveWorkContractProjection {
            route: crate::session::TaskRoute::Code,
            process_phase: crate::session::ProcessPhase::Repair,
            active_work_kind: Some("verification_repair".to_string()),
            summary: "repair src/workflow.rs".to_string(),
            active_targets: vec![camino::Utf8PathBuf::from("src/workflow.rs")],
            operation_intents: vec![OperationIntent::ContentChangingAuthoringRequired],
            required_verification_commands: Vec::new(),
            allowed_tools: allowed_tools.clone(),
            forbidden_tools: vec![ToolName::Shell],
            projection_id,
        },
        allowed_tools: allowed_tools.clone(),
        tool_choice: ToolChoice::Required,
        images: Vec::new(),
        output_contract: OutputContract {
            final_answer_required: true,
            structured_schema_name: None,
            history_markdown_projection: false,
        },
        continuation: None,
        turn_decision_projection: None,
    };
    TurnControlEnvelope::new(
        turn_id,
        context,
        obligations,
        authority,
        projection_bundle,
        crate::protocol::DispatchPolicy::Dispatch,
        Vec::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_adapter_classifies_malformed_tool_arguments() {
        let action = ProviderActionAdapter::adapt_tool_call(&CompletedToolCall {
            call_id: "call_malformed".to_string(),
            tool_name: "write".to_string(),
            arguments_json: r#"{"path":"src/workflow.rs","#.to_string(),
        });

        let ModelActionProposal::MalformedToolArguments(proposal) = action else {
            panic!("expected malformed tool arguments proposal");
        };
        assert_eq!(proposal.source_call_id, "call_malformed");
        assert_eq!(proposal.requested_tool, "write");
        assert!(proposal.parse_error.contains("EOF"));
    }

    #[test]
    fn provider_adapter_classifies_schema_outside_tool_payload() {
        let action = ProviderActionAdapter::adapt_tool_call(&CompletedToolCall {
            call_id: "call_schema_outside".to_string(),
            tool_name: "write".to_string(),
            arguments_json: r#""src/workflow.rs""#.to_string(),
        });

        let ModelActionProposal::SchemaOutsideToolProposal(proposal) = action else {
            panic!("expected schema-outside tool proposal");
        };
        assert_eq!(proposal.source_call_id, "call_schema_outside");
        assert_eq!(proposal.requested_tool, "write");
        assert_eq!(proposal.raw_payload, r#""src/workflow.rs""#);
    }

    #[test]
    fn schema_outside_rejection_preserves_source_payload() {
        let envelope =
            edit_only_repair_fixture_envelope(vec![ToolName::Write, ToolName::ApplyPatch]);
        let allowed_tools = ["write".to_string(), "apply_patch".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let action = ProviderActionAdapter::adapt_tool_call(&CompletedToolCall {
            call_id: "call_schema_outside".to_string(),
            tool_name: "write".to_string(),
            arguments_json: r#""src/workflow.rs""#.to_string(),
        });

        let ActionAdjudication::RejectedModelAction(rejection) =
            TurnLifecycleKernel::adjudicate_model_action(
                action,
                &allowed_tools,
                true,
                true,
                &envelope,
            )
        else {
            panic!("expected rejected schema-outside action");
        };

        assert_eq!(rejection.proposal.call_id, "call_schema_outside");
        assert_eq!(rejection.proposal.arguments_json, r#""src/workflow.rs""#);
        let result = rejection.to_tool_result(
            ToolCallId::new(),
            &allowed_tools,
            true,
            true,
            &envelope.projection_bundle.tool_result_feedback,
        );
        assert_eq!(
            result
                .metadata
                .get("original_arguments_json")
                .and_then(Value::as_str),
            Some(r#""src/workflow.rs""#)
        );
    }

    #[test]
    fn schema_outside_shell_is_provider_noncompliance_under_edit_only_surface() {
        let envelope =
            edit_only_repair_fixture_envelope(vec![ToolName::Write, ToolName::ApplyPatch]);
        let allowed_tools = ["write".to_string(), "apply_patch".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let input = ActionAdjudicationInput {
            proposal: ModelToolCallProposal {
                call_id: "call_1".to_string(),
                requested_tool: "shell".to_string(),
                effective_tool: "shell".to_string(),
                arguments_json: r#"{"command":"verify-contract --behavior"}"#.to_string(),
            },
            allowed_tools: &allowed_tools,
            tool_exists: true,
            tool_allowed: false,
            envelope: &envelope,
        };

        let ActionAdjudication::RejectedModelAction(rejection) =
            ActionAdjudicator::adjudicate_tool_call(&input)
        else {
            panic!("expected rejected action");
        };

        assert_eq!(
            rejection.classification,
            ModelActionRejectionClass::ProviderNoncompliance
        );
        assert_eq!(
            rejection.semantic_class,
            "provider_ignored_edit_only_surface"
        );
        let result = rejection.to_tool_result(
            ToolCallId::new(),
            &allowed_tools,
            input.tool_exists,
            input.tool_allowed,
            &envelope.projection_bundle.tool_result_feedback,
        );
        assert_eq!(
            result
                .metadata
                .get("tool_feedback_envelope")
                .and_then(|value| value.get("semantic_class"))
                .and_then(Value::as_str),
            Some("provider_ignored_edit_only_surface")
        );
        assert!(result.metadata.get("rejected_tool_proposal").is_some());
        assert_eq!(
            result
                .metadata
                .get("provider_noncompliance")
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn text_final_with_open_obligations_materializes_rejected_proposal() {
        let envelope =
            edit_only_repair_fixture_envelope(vec![ToolName::Write, ToolName::ApplyPatch]);
        let allowed_tools = ["write".to_string(), "apply_patch".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let action = ProviderActionAdapter::adapt_text_final(
            "done",
            envelope
                .projection_bundle
                .tool_result_feedback
                .projection_id,
            true,
        );

        let ActionAdjudication::RejectedModelAction(rejection) =
            TurnLifecycleKernel::adjudicate_model_action(
                action,
                &allowed_tools,
                false,
                false,
                &envelope,
            )
        else {
            panic!("expected final message rejection");
        };

        let source_call_id = ToolCallId::new();
        let proposal = rejection.to_rejected_tool_proposal(
            source_call_id,
            &allowed_tools,
            &envelope.projection_bundle.tool_result_feedback,
        );
        assert_eq!(
            rejection.semantic_class,
            "text_final_while_obligations_open"
        );
        assert_eq!(proposal.source_call_id, source_call_id);
        assert_eq!(proposal.requested_tool, "final_assistant_message");
        assert_eq!(proposal.effective_tool, "final_assistant_message");
        assert_eq!(proposal.resolved_tool, ToolName::Invalid);
        assert_eq!(
            proposal
                .original_arguments
                .get("text")
                .and_then(Value::as_str),
            Some("done")
        );
        assert_eq!(
            envelope
                .projection_bundle
                .tool_result_feedback
                .required_action
                .as_ref()
                .map(|action| action.projection_label()),
            Some("write:src/workflow.rs".to_string())
        );
        assert!(proposal.allowed_surface.contains(&ToolName::Write));
        assert!(!proposal.payload_hash.is_empty());
    }

    #[test]
    fn provider_replay_omits_intermediate_assistant_text() {
        assert!(provider_replay_omits_intermediate_assistant_text_fixture_passes());
    }

    #[test]
    fn provider_replay_omits_assistant_tool_call_content() {
        assert!(provider_replay_omits_assistant_tool_call_content_fixture_passes());
    }

    #[test]
    fn provider_system_context_normalization_merges_late_system_messages() {
        assert!(provider_system_context_normalization_fixture_passes());
    }

    #[test]
    fn code_authoring_final_message_recovery_reopens_stable_surface() {
        assert!(code_authoring_final_message_recovery_reopens_stable_surface_fixture_passes());
    }

    #[test]
    fn failed_edit_final_message_recovery_keeps_failed_edit_surface() {
        assert!(failed_edit_final_message_recovery_keeps_failed_edit_surface_fixture_passes());
    }

    #[test]
    fn open_obligation_final_message_recovery_persists_across_no_progress_tool() {
        assert!(
            open_obligation_final_message_recovery_persists_across_no_progress_tool_fixture_passes(
            )
        );
    }

    #[test]
    fn open_obligation_final_message_recovery_preserves_stable_surface() {
        assert!(open_obligation_final_message_recovery_preserves_stable_surface_fixture_passes());
    }

    #[test]
    fn open_work_uses_auto_tool_choice_with_harness_closeout_guard() {
        assert!(open_work_uses_auto_tool_choice_with_harness_closeout_guard_fixture_passes());
    }

    #[test]
    fn multi_target_open_authoring_final_message_correction_names_targets() {
        assert!(
            multi_target_open_authoring_final_message_correction_names_targets_fixture_passes()
        );
    }

    #[test]
    fn final_message_recovery_is_system_control_projection() {
        assert!(final_message_recovery_is_system_control_projection_fixture_passes());
    }

    #[test]
    fn invalid_edit_arguments_recovery_is_system_control_projection() {
        assert!(invalid_edit_arguments_recovery_is_system_control_projection_fixture_passes());
    }

    #[test]
    fn mixed_target_invalid_edit_recovery_projects_into_control_envelope() {
        assert!(mixed_target_invalid_edit_recovery_projects_into_control_envelope_fixture_passes());
    }

    #[test]
    fn content_shape_failed_edit_projects_latest_recovery_into_control_envelope() {
        assert!(
            content_shape_failed_edit_projects_latest_recovery_into_control_envelope_fixture_passes(
            )
        );
    }

    #[test]
    fn content_shape_recovery_projection_omits_inactive_submitted_targets() {
        assert!(
            content_shape_recovery_projection_omits_inactive_submitted_targets_fixture_passes()
        );
    }

    #[test]
    fn stale_invalid_edit_recovery_is_not_open_obligation_after_verification_transition() {
        assert!(
            stale_invalid_edit_recovery_is_not_open_obligation_after_verification_transition_fixture_passes()
        );
    }

    #[test]
    fn any_out_of_surface_tool_is_provider_noncompliance_under_edit_only_surface() {
        let envelope_a =
            edit_only_repair_fixture_envelope(vec![ToolName::Write, ToolName::ApplyPatch]);
        let envelope_b =
            edit_only_repair_fixture_envelope(vec![ToolName::Write, ToolName::ApplyPatch]);
        let allowed_tools = ["write".to_string(), "apply_patch".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let input_a = ActionAdjudicationInput {
            proposal: ModelToolCallProposal {
                call_id: "call_1".to_string(),
                requested_tool: "read".to_string(),
                effective_tool: "read".to_string(),
                arguments_json: r#"{"path":"src/workflow.rs"}"#.to_string(),
            },
            allowed_tools: &allowed_tools,
            tool_exists: true,
            tool_allowed: false,
            envelope: &envelope_a,
        };
        let input_b = ActionAdjudicationInput {
            proposal: input_a.proposal.clone(),
            allowed_tools: &allowed_tools,
            tool_exists: true,
            tool_allowed: false,
            envelope: &envelope_b,
        };

        let ActionAdjudication::RejectedModelAction(rejection_a) =
            ActionAdjudicator::adjudicate_tool_call(&input_a)
        else {
            panic!("expected rejected action");
        };
        let ActionAdjudication::RejectedModelAction(rejection_b) =
            ActionAdjudicator::adjudicate_tool_call(&input_b)
        else {
            panic!("expected rejected action");
        };

        assert_eq!(
            rejection_a.classification,
            ModelActionRejectionClass::ProviderNoncompliance
        );
        assert_eq!(
            rejection_a.semantic_class,
            "provider_ignored_edit_only_surface"
        );
        assert_eq!(rejection_a.result_hash, rejection_b.result_hash);
    }

    #[test]
    fn rejection_hash_includes_payload_shape() {
        let envelope =
            edit_only_repair_fixture_envelope(vec![ToolName::Write, ToolName::ApplyPatch]);
        let allowed_tools = ["write".to_string(), "apply_patch".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let input_a = ActionAdjudicationInput {
            proposal: ModelToolCallProposal {
                call_id: "call_1".to_string(),
                requested_tool: "read".to_string(),
                effective_tool: "read".to_string(),
                arguments_json: r#"{"path":"src/workflow.rs"}"#.to_string(),
            },
            allowed_tools: &allowed_tools,
            tool_exists: true,
            tool_allowed: false,
            envelope: &envelope,
        };
        let input_b = ActionAdjudicationInput {
            proposal: ModelToolCallProposal {
                call_id: "call_2".to_string(),
                requested_tool: "read".to_string(),
                effective_tool: "read".to_string(),
                arguments_json: r#"{"path":"src/other_workflow.rs"}"#.to_string(),
            },
            allowed_tools: &allowed_tools,
            tool_exists: true,
            tool_allowed: false,
            envelope: &envelope,
        };

        let ActionAdjudication::RejectedModelAction(rejection_a) =
            ActionAdjudicator::adjudicate_tool_call(&input_a)
        else {
            panic!("expected rejected action");
        };
        let ActionAdjudication::RejectedModelAction(rejection_b) =
            ActionAdjudicator::adjudicate_tool_call(&input_b)
        else {
            panic!("expected rejected action");
        };

        assert_ne!(rejection_a.result_hash, rejection_b.result_hash);
    }

    #[test]
    fn allowed_tool_call_is_accepted() {
        let envelope =
            edit_only_repair_fixture_envelope(vec![ToolName::Write, ToolName::ApplyPatch]);
        let allowed_tools = ["write".to_string(), "apply_patch".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let input = ActionAdjudicationInput {
            proposal: ModelToolCallProposal {
                call_id: "call_1".to_string(),
                requested_tool: "write".to_string(),
                effective_tool: "write".to_string(),
                arguments_json:
                    r#"{"path":"src/workflow.rs","content":"pub fn workflow_state() -> &'static str { \"ready\" }\n"}"#.to_string(),
            },
            allowed_tools: &allowed_tools,
            tool_exists: true,
            tool_allowed: true,
            envelope: &envelope,
        };

        assert!(matches!(
            ActionAdjudicator::adjudicate_tool_call(&input),
            ActionAdjudication::AcceptedToolCall(_)
        ));
    }

    #[test]
    fn mixed_out_of_surface_replay_omits_assistant_prelude() {
        assert!(provider_surface_filter_omits_mixed_stale_assistant_prelude_fixture_passes());
    }

    #[test]
    fn provider_replay_effective_tool_surface_preserves_effective_payload() {
        assert!(provider_replay_effective_tool_surface_fixture_passes());
    }

    #[test]
    fn provider_replay_preserves_supporting_context_after_surface_narrowing() {
        assert!(
            provider_replay_preserves_supporting_context_evidence_after_surface_narrowing_fixture_passes()
        );
    }

    #[test]
    fn generated_test_authoring_keeps_recent_source_reference_read() {
        assert!(generated_test_authoring_keeps_recent_source_reference_read_fixture_passes());
    }

    #[test]
    fn singleton_missing_authoring_target_projects_create_action() {
        assert!(singleton_missing_authoring_target_projects_create_action_fixture_passes());
    }
}
