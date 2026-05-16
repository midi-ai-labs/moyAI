use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::agent::prompt_assets::{
    SystemPromptInput, active_follow_up_request_reminder, ask_route_reminder,
    code_block_stall_reminder, compaction_continuation_reminder, compaction_replay_reminder,
    completion_ready_reminder, debug_route_reminder, docs_route_reminder, edit_recovery_reminder,
    failure_reminder, follow_up_boundary_reminder, follow_up_documentation_scope_reminder,
    follow_up_implementation_scope_reminder, follow_up_implementation_stall_reminder,
    follow_up_spec_alignment_reminder, inactive_target_edit_recovery_reminder,
    interrupted_resume_reminder, patch_recovery_reminder, pseudo_tool_call_stall_reminder,
    readonly_stall_reminder, render_system_prompt, review_route_reminder,
    staged_task_closeout_recovery_reminder, staged_task_closeout_reminder,
    staged_task_closeout_repair_reminder, staged_task_documentation_audit_escalation_reminder,
    staged_task_documentation_audit_feedback_excerpt,
    staged_task_documentation_audit_repair_reminder,
    staged_task_documentation_authoring_focus_reminder,
    staged_task_documentation_authoring_reminder, staged_task_documentation_grounding_reminder,
    staged_task_execution_reminder, structured_document_summary_reminder, summary_route_reminder,
    superseded_tool_denial_reminder, verification_failure_repair_edit_focused_reminder,
    verification_failure_repair_reminder, verification_pending_reminder,
    verification_recovery_reminder, verification_rerun_preferred_reminder,
};
use crate::agent::repair_lane::project_repair_lane;
use crate::agent::state::{
    ActiveWorkContract, active_work_contract_for_history_items, docs_route_pending_repair_targets,
    latest_verification_failure_context, project_model_turn_state, render_active_work_contract,
    render_model_turn_state, structured_document_summary_snapshot,
};
use crate::agent::verification::{
    explicit_verification_commands_from_text, latest_failed_verification_preceding_repair_targets,
    latest_verification_repair_cycle, looks_like_verification_command,
    looks_like_verification_failure, verification_evidence_after_latest_user_with_freshness,
    verification_freshness_targets_after_latest_user, verification_requirements,
};
use crate::config::{AgentConfig, PromptProfile, ResolvedConfig, ShellFamily};
use crate::edit::PatchParser;
use crate::error::AgentError;
use crate::llm::{ModelContentPart, ModelMessage, ModelProfile, ModelToolCall, ToolSchema};
use crate::protocol::{ContentPart, HistoryItem, HistoryItemPayload};
use crate::session::{
    FailureKind, MessageMetadata, MessagePart, MessageRole, ProcessPhase,
    RequestReplayPolicyDiagnostic, SessionRecord, SessionStateSnapshot, TaskRoute, TodoItem,
    ToolCallStatus, ToolResultPart, Transcript, VerificationFailureCluster,
    transcript_from_history_items,
};
use crate::tool::{ToolName, registry::ToolRegistry};
use crate::workspace::instruction_file_names;

#[derive(Debug, Clone)]
pub struct AgentRunRequest {
    pub session: crate::session::SessionContext,
    pub user_message_id: crate::session::MessageId,
    pub runtime_input: RuntimeInputView,
    pub state: SessionStateSnapshot,
    pub config: ResolvedConfig,
    pub model: ModelProfile,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone)]
pub struct RuntimeInputView {
    session: SessionRecord,
    pub history_items: Vec<HistoryItem>,
    legacy_import_transcript: Option<Transcript>,
}

impl RuntimeInputView {
    pub fn from_history_items(session: &SessionRecord, history_items: Vec<HistoryItem>) -> Self {
        Self {
            session: session.clone(),
            history_items,
            legacy_import_transcript: None,
        }
    }

    pub fn from_compatibility_transcript(transcript: Transcript) -> Self {
        Self {
            session: transcript.session.clone(),
            history_items: Vec::new(),
            legacy_import_transcript: Some(transcript),
        }
    }

    pub fn materialized_transcript_projection(&self) -> Transcript {
        if self.history_items.is_empty() {
            return self
                .legacy_import_transcript
                .clone()
                .unwrap_or_else(|| transcript_from_history_items(&self.session, &[]));
        }
        transcript_from_history_items(&self.session, &self.history_items)
    }

    pub fn into_compatibility_transcript(self) -> Transcript {
        if self.history_items.is_empty() {
            return self
                .legacy_import_transcript
                .unwrap_or_else(|| transcript_from_history_items(&self.session, &[]));
        }
        transcript_from_history_items(&self.session, &self.history_items)
    }

    pub fn has_user_turn(&self) -> bool {
        self.history_items
            .iter()
            .any(|item| matches!(item.payload, HistoryItemPayload::UserTurn { .. }))
    }
}

#[derive(Debug, Clone)]
pub struct PromptBundle {
    pub system_prompt: String,
    pub instruction_sources: Vec<Utf8PathBuf>,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolSchema>,
    pub policy: PromptPolicy,
    pub replay_policies: Vec<RequestReplayPolicyDiagnostic>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PromptPolicy {
    pub follow_up_focus: FollowUpFocus,
    pub follow_up_implementation: bool,
    pub documentation_scope_explicit: bool,
    pub completion_closeout_ready: bool,
    pub staged_task_execution_active: bool,
    pub staged_task_artifacts: Vec<String>,
    pub staged_task_output_targets: Vec<String>,
    pub execution_focus_targets: Vec<String>,
    pub requested_artifact_targets: Vec<String>,
    pub required_verification_commands: Vec<String>,
    pub documentation_scope_targets: Vec<String>,
    pub enforce_edit_after_readonly_stall: bool,
    pub readonly_stall_targets: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct RequestedWorkContract {
    pub deliverable_targets: Vec<String>,
    pub reference_inputs: Vec<String>,
    pub example_targets: Vec<String>,
    pub naming_patterns: Vec<String>,
    pub verification_commands: Vec<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum FollowUpFocus {
    #[default]
    Unknown,
    Documentation,
    Implementation,
    Mixed,
}

#[derive(Debug, Default, Clone)]
pub struct PromptBuilder;

// These prompt and history guardrails are intentionally fixed. opencode and Roo Code
// both hardcode prompt/output budgets; moyai keeps tighter caps so local LLMs
// see a shallow, deterministic prompt surface instead of a sprawling context plan.
const MAX_TOTAL_INSTRUCTION_CHARS: usize = 6_000;
const MAX_PRIMARY_INSTRUCTION_CHARS: usize = 1_800;
const MAX_SECONDARY_INSTRUCTION_CHARS: usize = 1_000;
const MAX_TERTIARY_INSTRUCTION_CHARS: usize = 600;
const INSTRUCTION_RENDER_STOP_THRESHOLD_CHARS: usize = 120;
const INSTRUCTION_SUMMARY_MAX_LINES: usize = 8;
const INSTRUCTION_TRUNCATION_RESERVE_CHARS: usize = 16;
const RECENT_TOOL_CALL_WINDOW: usize = 6;
const IMPLEMENTATION_READS_ASSUME_IMPLEMENTATION_FOCUS: usize = 2;
const MAX_VERIFICATION_FAILURE_LABELS: usize = 3;
const TODO_FOCUS_BLOCKED_PREVIEW_LIMIT: usize = 2;
const TODO_FOCUS_NEXT_PREVIEW_LIMIT: usize = 2;
const TODO_FOCUS_TARGET_PREVIEW_LIMIT: usize = 2;
const STAGED_TASK_EVIDENCE_LINE_LIMIT: usize = 8;
const STAGED_TASK_LIST_PREVIEW_LIMIT: usize = 8;
const STAGED_TASK_READ_PREVIEW_LIMIT: usize = 5;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct PromptSignals {
    interrupted_resume: bool,
    compaction_replay: bool,
    follow_up_boundary: bool,
    follow_up_focus: FollowUpFocus,
    follow_up_implementation: bool,
    documentation_scope_explicit: bool,
    active_follow_up_request: Option<String>,
    requested_artifact_targets: Vec<String>,
    requested_verification_commands: Vec<String>,
    structured_document_summary_mode: bool,
    structured_document_summary_conversion_only_mode: bool,
    structured_document_summary_write_due: bool,
    staged_task_execution_active: bool,
    staged_task_artifacts: Vec<String>,
    staged_task_output_targets: Vec<String>,
    staged_task_verification_commands: Vec<String>,
    execution_focus_targets: Vec<String>,
    staged_task_documentation_authoring_mode: bool,
    staged_task_documentation_authoring_focus_mode: bool,
    staged_task_documentation_authoring_no_replan_mode: bool,
    staged_task_documentation_evidence_snapshot: Option<String>,
    staged_task_documentation_audit_feedback: Option<String>,
    staged_task_documentation_audit_repair_mode: bool,
    staged_task_documentation_audit_escalation_mode: bool,
    last_failure: Option<String>,
    documentation_scope_targets: Vec<String>,
    completion_closeout_ready: bool,
    readonly_stall: bool,
    readonly_stall_targets: Vec<String>,
    code_block_stall: bool,
    pseudo_tool_call_stall: bool,
    no_tool_authoring_error_stall: bool,
    inactive_target_edit_recovery_mode: bool,
    inactive_target_edit_recovery_targets: Vec<String>,
    inactive_target_edit_recovery_read_target: Option<String>,
    edit_recovery_mode: bool,
    patch_recovery_mode: bool,
    patch_recovery_targets: Vec<String>,
    verification_failure_repair_mode: bool,
    verification_repair_rerun_due: bool,
    verification_failure_repair_edit_focused_mode: bool,
    verification_repair_read_budget_exhausted: bool,
    verification_repair_next_read_target: Option<String>,
    verification_repair_focus_target: Option<String>,
    verification_repair_focus_target_read_after_failure: bool,
    verification_failure_labels: Vec<String>,
    staged_task_closeout_mode: bool,
    staged_task_closeout_read_complete: bool,
    staged_task_closeout_recovery_mode: bool,
    staged_task_closeout_repair_mode: bool,
    staged_task_closeout_repair_targets: Vec<String>,
    verification_recovery_mode: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ProviderReplayContext {
    active_authoring_targets: Vec<String>,
}

#[derive(Debug, Default, Clone)]
struct ProviderReplayProjection {
    messages: Vec<ModelMessage>,
    replay_policies: Vec<RequestReplayPolicyDiagnostic>,
}

#[derive(Debug, Default, Clone)]
struct PromptMessageProjection {
    messages: Vec<ModelMessage>,
    replay_policies: Vec<RequestReplayPolicyDiagnostic>,
}

impl ProviderReplayContext {
    fn from_state(state: &SessionStateSnapshot) -> Self {
        let authoring_active = matches!(
            state.process_phase,
            ProcessPhase::Author | ProcessPhase::Repair
        ) && !state.completion.closeout_ready
            && !state.completion.verification_pending
            && !state.active_targets.is_empty();
        if !authoring_active {
            return Self::default();
        }
        Self {
            active_authoring_targets: state
                .active_targets
                .iter()
                .map(|target| target.as_str().to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct StagedTaskDocumentationAuditPromptState {
    target: String,
    feedback: String,
    actionable_feedback: bool,
    failure_count: usize,
}

impl StagedTaskDocumentationAuditPromptState {
    fn escalated(&self, agent_config: &AgentConfig) -> bool {
        self.failure_count >= agent_config.staged_task_audit_repair_rewrite_escalation_threshold
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestedLineIntent {
    Deliverable,
    Reference,
    Convention,
    Verification,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstructionRenderMode {
    Full,
    Summary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstructionSourceEntry {
    path: Utf8PathBuf,
    priority: usize,
    mode: InstructionRenderMode,
    discovery_index: usize,
}

impl PromptBuilder {
    pub fn build(
        &self,
        request: &AgentRunRequest,
        registry: &ToolRegistry,
        todos: &[TodoItem],
    ) -> Result<PromptBundle, AgentError> {
        let instruction_sources = collect_instruction_sources(
            &request.session.workspace.cwd,
            &request.session.workspace.root,
            request.state.route,
            &request.config.instructions.additional_files,
        )?;
        let instruction_text = render_instruction_text(
            &request.session.workspace.cwd,
            &request.session.workspace.root,
            request.state.route,
            &instruction_sources,
            &request.config.instructions.additional_files,
        );

        let prompt_profile = request
            .config
            .model
            .prompt_profile
            .resolved_for_model(&request.model.name);
        let transcript = request.runtime_input.materialized_transcript_projection();
        let active_work = active_work_contract_for_history_items(
            &request.session.session,
            &request.runtime_input.history_items,
            &request.state,
            todos,
        );
        let mut signals = detect_prompt_signals_with_config(
            &transcript,
            todos,
            &request.config.agent,
            Some(&request.state),
        );
        apply_state_driven_signal_overrides(
            &mut signals,
            &transcript,
            &request.state,
            active_work.as_ref(),
        );
        signals.completion_closeout_ready = request.state.completion.closeout_ready;
        if signals.completion_closeout_ready {
            signals.edit_recovery_mode = false;
            signals.patch_recovery_mode = false;
            signals.staged_task_documentation_audit_repair_mode = false;
            signals.staged_task_documentation_audit_escalation_mode = false;
            signals.verification_failure_repair_mode = false;
            signals.verification_recovery_mode = false;
        }
        if request.state.completion.route_contract_pending
            || request
                .state
                .completion
                .blocked_reason
                .as_deref()
                .is_some_and(blocked_reason_mentions_missing_deliverables)
        {
            signals.staged_task_closeout_mode = false;
            signals.staged_task_closeout_read_complete = false;
            signals.staged_task_closeout_recovery_mode = false;
        }
        if docs_route_should_suppress_authoring_focus_mode(&request.state) {
            signals.staged_task_documentation_authoring_focus_mode = false;
        }
        signals.inactive_target_edit_recovery_read_target =
            inactive_target_recovery_required_read_target(
                &transcript,
                &request.session.workspace.root,
                &signals.inactive_target_edit_recovery_targets,
            );
        let (tools, tool_names, available_skills_text) = prepare_prompt_tools(
            registry,
            &signals,
            &request.state,
            &request.session.workspace.root,
        );
        let system_prompt = build_system_prompt(
            request,
            &instruction_text,
            &available_skills_text,
            &tool_names,
            prompt_profile,
        )?;
        let message_projection = build_messages_with_state(
            &transcript,
            &request.session.session,
            &request.runtime_input.history_items,
            &request.state,
            todos,
            request.config.session.transcript_limit_messages,
            &tool_names,
            &signals,
            active_work.as_ref(),
        );
        let mut messages = message_projection.messages;
        if let Some(content) =
            render_model_turn_state(&project_model_turn_state(&request.state, todos))
        {
            messages.insert(0, ModelMessage::System { content });
        }

        Ok(PromptBundle {
            system_prompt,
            instruction_sources,
            messages,
            tools,
            replay_policies: message_projection.replay_policies,
            policy: PromptPolicy {
                follow_up_focus: signals.follow_up_focus,
                follow_up_implementation: signals.follow_up_implementation,
                documentation_scope_explicit: signals.documentation_scope_explicit,
                completion_closeout_ready: signals.completion_closeout_ready,
                staged_task_execution_active: signals.staged_task_execution_active,
                staged_task_artifacts: signals.staged_task_artifacts.clone(),
                staged_task_output_targets: signals.staged_task_output_targets.clone(),
                execution_focus_targets: signals.execution_focus_targets.clone(),
                requested_artifact_targets: signals.requested_artifact_targets.clone(),
                required_verification_commands: request
                    .state
                    .verification
                    .required_commands
                    .clone(),
                documentation_scope_targets: signals.documentation_scope_targets.clone(),
                enforce_edit_after_readonly_stall: signals.follow_up_implementation
                    && signals.readonly_stall,
                readonly_stall_targets: signals.readonly_stall_targets.clone(),
            },
        })
    }
}

fn prepare_prompt_tools(
    registry: &ToolRegistry,
    signals: &PromptSignals,
    state: &SessionStateSnapshot,
    workspace_root: &Utf8Path,
) -> (Vec<ToolSchema>, Vec<String>, String) {
    let mut tools: Vec<ToolSchema> = registry
        .specs()
        .into_iter()
        .map(|spec| ToolSchema {
            name: spec.name.to_string(),
            description: spec.description.to_string(),
            input_schema: spec.input_schema,
            strict: false,
        })
        .collect();
    let skill_tool = tools.iter().find(|tool| tool.name == "skill").cloned();
    apply_candidate_tool_availability_for_prompt_state(&mut tools, signals, state);
    restore_skill_tool(&mut tools, skill_tool.as_ref(), state);
    let tool_names = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<Vec<_>>();
    let available_skills_text = if tool_names.iter().any(|name| name == "skill") {
        crate::skill::render_available_skills(workspace_root)
    } else {
        String::new()
    };
    (tools, tool_names, available_skills_text)
}

fn repair_lane_projection_for_prompt(
    state: &SessionStateSnapshot,
) -> Option<crate::agent::repair_lane::RepairLaneProjection> {
    let allowed_tools = ["write", "apply_patch", "todowrite"]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    project_repair_lane(state, &allowed_tools, None)
}

fn verification_repair_target_was_read(
    cycle: &crate::agent::verification::VerificationRepairCycle,
    target: &str,
) -> bool {
    cycle.post_failure_read_targets.iter().any(|read_target| {
        prompt_target_matches_required_output(read_target.as_str(), &[target.to_string()])
            || prompt_target_matches_required_output(target, &[read_target.as_str().to_string()])
    })
}

fn verification_repair_target_is_contract_ref(target: &str, contract_refs: &[Utf8PathBuf]) -> bool {
    contract_refs.iter().any(|contract_ref| {
        prompt_target_matches_required_output(contract_ref.as_str(), &[target.to_string()])
            || prompt_target_matches_required_output(target, &[contract_ref.as_str().to_string()])
    }) || target
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .map(|file_name| {
            matches!(
                file_name.to_ascii_lowercase().as_str(),
                "scenario_contract.md" | "scenario_contract.json"
            )
        })
        .unwrap_or(false)
}

pub(crate) fn verification_repair_next_read_target(
    verification_failure_repair_mode: bool,
    verification_repair_rerun_due: bool,
    verification_repair_focus_target: Option<&str>,
    cycle: Option<&crate::agent::verification::VerificationRepairCycle>,
    active_targets: &[Utf8PathBuf],
    contract_refs: &[Utf8PathBuf],
    read_budget: usize,
    allow_focus_read_after_budget_exhausted: bool,
) -> Option<String> {
    if !verification_failure_repair_mode || verification_repair_rerun_due || read_budget == 0 {
        return None;
    }
    let focus_target = verification_repair_focus_target?;
    let Some(cycle) = cycle else {
        return Some(focus_target.to_string());
    };
    if cycle.repair_recorded {
        return None;
    }
    if cycle.post_failure_read_attempt_count >= read_budget {
        if allow_focus_read_after_budget_exhausted
            && (!verification_repair_target_was_read(cycle, focus_target)
                || verification_repair_target_chunk_window_open(cycle, focus_target, read_budget))
        {
            return Some(focus_target.to_string());
        }
        return None;
    }

    if !verification_repair_target_was_read(cycle, focus_target) {
        return Some(focus_target.to_string());
    }

    active_targets
        .iter()
        .map(|target| target.as_str().to_string())
        .filter(|target| !verification_repair_target_is_contract_ref(target, contract_refs))
        .find(|target| !verification_repair_target_was_read(cycle, target))
}

fn verification_repair_target_chunk_window_open(
    cycle: &crate::agent::verification::VerificationRepairCycle,
    target: &str,
    read_budget: usize,
) -> bool {
    if read_budget == 0 {
        return false;
    }
    let spans = cycle
        .post_failure_read_spans
        .iter()
        .filter(|span| {
            prompt_target_matches_required_output(span.target.as_str(), &[target.to_string()])
        })
        .collect::<Vec<_>>();
    !spans.is_empty()
        && spans.len() < read_budget.saturating_add(1)
        && spans.iter().any(|span| span.offset.is_some())
}

fn apply_state_driven_signal_overrides(
    signals: &mut PromptSignals,
    transcript: &Transcript,
    state: &SessionStateSnapshot,
    active_work: Option<&ActiveWorkContract>,
) {
    if state.completion.closeout_ready {
        signals.completion_closeout_ready = true;
        signals.verification_recovery_mode = false;
        signals.verification_failure_repair_mode = false;
        signals.verification_failure_repair_edit_focused_mode = false;
        signals.verification_repair_rerun_due = false;
        signals.edit_recovery_mode = false;
        signals.patch_recovery_mode = false;
        signals.inactive_target_edit_recovery_mode = false;
        signals.inactive_target_edit_recovery_targets.clear();
        signals.inactive_target_edit_recovery_read_target = None;
        signals.staged_task_closeout_mode = false;
        signals.staged_task_closeout_recovery_mode = false;
        signals.staged_task_closeout_repair_mode = false;
        signals.staged_task_closeout_repair_targets.clear();
        return;
    }

    match active_work {
        Some(ActiveWorkContract::RequestedWorkAuthoring { .. }) => {
            signals.verification_recovery_mode = false;
            signals.staged_task_closeout_mode = false;
            signals.staged_task_closeout_recovery_mode = false;
        }
        Some(ActiveWorkContract::DocsRepair {
            deliverable,
            pending_deliverables,
            ..
        }) => {
            let pending_targets = pending_deliverables
                .iter()
                .map(|item| item.target.as_str().to_string())
                .collect::<Vec<_>>();
            if !pending_targets.is_empty() {
                signals.execution_focus_targets = pending_targets.clone();
                signals.documentation_scope_targets = pending_targets;
                signals.staged_task_documentation_authoring_mode = true;
                if !docs_route_should_suppress_authoring_focus_mode(state) {
                    signals.staged_task_documentation_authoring_focus_mode = true;
                    signals.staged_task_documentation_authoring_no_replan_mode = true;
                }
            } else if let Some(deliverable) = deliverable {
                let target = deliverable.as_str().to_string();
                signals.execution_focus_targets = vec![target.clone()];
                signals.documentation_scope_targets = vec![target];
                signals.staged_task_documentation_authoring_mode = true;
                if !docs_route_should_suppress_authoring_focus_mode(state) {
                    signals.staged_task_documentation_authoring_focus_mode = true;
                    signals.staged_task_documentation_authoring_no_replan_mode = true;
                }
            }
            signals.verification_recovery_mode = false;
            signals.verification_failure_repair_mode = false;
            signals.edit_recovery_mode = false;
            signals.patch_recovery_mode = false;
            signals.staged_task_closeout_mode = false;
            signals.staged_task_closeout_recovery_mode = false;
        }
        _ => {}
    }

    let verification_failure_active = matches!(
        state.failure.as_ref().map(|failure| failure.kind),
        Some(FailureKind::VerificationFailed)
    );
    let verification_rerun_due_from_state = !state.completion.closeout_ready
        && (verification_failure_active || state.completion.open_work_count == 0)
        && latest_verification_repair_cycle(transcript)
            .as_ref()
            .is_some_and(|cycle| cycle.repair_recorded)
        && (verification_failure_active
            || state.completion.verification_pending
            || state.verification.pending_todo_id.is_some());
    if verification_rerun_due_from_state {
        signals.verification_repair_rerun_due = true;
        signals.verification_recovery_mode = true;
        signals.verification_failure_repair_mode = false;
        signals.verification_failure_repair_edit_focused_mode = false;
        signals.verification_repair_next_read_target = None;
        signals.verification_repair_focus_target = None;
        signals.verification_repair_focus_target_read_after_failure = false;
        signals.edit_recovery_mode = false;
        signals.patch_recovery_mode = false;
        signals.inactive_target_edit_recovery_mode = false;
        signals.inactive_target_edit_recovery_targets.clear();
        signals.inactive_target_edit_recovery_read_target = None;
        signals.staged_task_execution_active = false;
        signals.staged_task_documentation_authoring_mode = false;
        signals.staged_task_documentation_authoring_focus_mode = false;
        signals.staged_task_documentation_authoring_no_replan_mode = false;
        signals.staged_task_documentation_audit_repair_mode = false;
        signals.staged_task_documentation_audit_escalation_mode = false;
        signals.staged_task_closeout_mode = false;
        signals.staged_task_closeout_recovery_mode = false;
        signals.staged_task_closeout_repair_mode = false;
        signals.staged_task_closeout_repair_targets.clear();
    }

    if signals.verification_repair_read_budget_exhausted && !state.completion.closeout_ready {
        signals.verification_failure_repair_mode = true;
        signals.verification_failure_repair_edit_focused_mode = true;
        signals.verification_repair_next_read_target = None;
        signals.verification_recovery_mode = false;
        signals.edit_recovery_mode = false;
        signals.patch_recovery_mode = false;
        signals.inactive_target_edit_recovery_mode = false;
        signals.inactive_target_edit_recovery_targets.clear();
        signals.inactive_target_edit_recovery_read_target = None;
        signals.staged_task_execution_active = false;
        signals.staged_task_documentation_authoring_mode = false;
        signals.staged_task_documentation_authoring_focus_mode = false;
        signals.staged_task_documentation_authoring_no_replan_mode = false;
        signals.staged_task_documentation_audit_repair_mode = false;
        signals.staged_task_documentation_audit_escalation_mode = false;
        signals.staged_task_closeout_mode = false;
        signals.staged_task_closeout_recovery_mode = false;
        signals.staged_task_closeout_repair_mode = false;
        signals.staged_task_closeout_repair_targets.clear();
    }

    let state_patch_recovery_targets = patch_recovery_targets_from_state(state);
    if !state_patch_recovery_targets.is_empty()
        && !state.completion.closeout_ready
        && !state.completion.route_contract_pending
        && !signals.verification_repair_rerun_due
        && !signals.staged_task_closeout_mode
    {
        signals.patch_recovery_targets = state_patch_recovery_targets;
        signals.patch_recovery_mode = true;
        signals.edit_recovery_mode = false;
        signals.verification_recovery_mode = false;
        signals.verification_failure_repair_mode = false;
        signals.verification_failure_repair_edit_focused_mode = false;
        signals.verification_repair_rerun_due = false;
    }

    if state.failure.is_none()
        && state.completion.verification_pending
        && state.completion.open_work_count == 0
        && !state.completion.closeout_ready
        && !state.completion.route_contract_pending
        && !signals.staged_task_closeout_mode
        && !signals.patch_recovery_mode
    {
        signals.verification_failure_repair_mode = false;
        signals.verification_failure_repair_edit_focused_mode = false;
        signals.verification_recovery_mode = true;
        signals.edit_recovery_mode = false;
    }

    if matches!(
        state.failure.as_ref().map(|failure| failure.kind),
        Some(FailureKind::VerificationFailed)
    ) && state.completion.verification_pending
        && state.completion.open_work_count == 0
        && !state.completion.closeout_ready
        && !state.completion.route_contract_pending
        && !signals.staged_task_closeout_mode
        && !signals.patch_recovery_mode
    {
        signals.edit_recovery_mode = false;
        if !signals.verification_failure_repair_mode {
            signals.verification_recovery_mode = true;
        }
    }
}

fn patch_recovery_targets_from_state(state: &SessionStateSnapshot) -> Vec<String> {
    if !matches!(
        state.failure.as_ref().map(|failure| failure.kind),
        Some(FailureKind::PatchMismatch)
    ) {
        return Vec::new();
    }

    if typed_verification_cluster_indicates_generated_test_expectation_drift(
        state.verification.failure_cluster.as_ref(),
    )
    .unwrap_or(false)
    {
        let test_targets = state
            .failure
            .as_ref()
            .into_iter()
            .flat_map(|failure| failure.targets.iter())
            .chain(state.active_targets.iter())
            .filter(|target| target_is_test_like(target.as_str()))
            .map(|target| target.as_str().to_string())
            .collect::<Vec<_>>();
        if !test_targets.is_empty() {
            return dedupe_targets(test_targets);
        }
    }

    let failure_targets = state
        .failure
        .as_ref()
        .map(|failure| failure.targets.as_slice())
        .unwrap_or_default();
    let targets = if failure_targets.is_empty() {
        state
            .active_targets
            .iter()
            .map(|target| target.as_str().to_string())
            .collect::<Vec<_>>()
    } else {
        failure_targets
            .iter()
            .map(|target| target.as_str().to_string())
            .collect::<Vec<_>>()
    };

    dedupe_targets(targets)
}

fn typed_verification_cluster_indicates_generated_test_expectation_drift(
    cluster: Option<&VerificationFailureCluster>,
) -> Option<bool> {
    let cluster = cluster?;
    let marker_or_subtype_matches = cluster.evidence.iter().any(|evidence| {
        evidence.subtype.as_deref().is_some_and(|subtype| {
            matches!(
                subtype,
                "generated_test_logging_contract_overreach"
                    | "source_test_import_export_reconciliation"
            )
        }) || evidence.evidence_markers.iter().any(|marker| {
            let marker = marker.to_ascii_lowercase();
            marker.contains("generated-test data model contradicts")
                || marker.contains("generated test setup contradicts")
                || marker.contains("generated-test setup contradicts")
                || marker.contains("generated-test contract")
                || marker.contains("generated-test conflict evidence")
                || marker.contains("generated-test logging side-effect assertion")
        })
    });
    if marker_or_subtype_matches {
        return Some(true);
    }

    let source_backed_public_failure = cluster.evidence.iter().any(|evidence| {
        evidence.evidence_markers.iter().any(|marker| {
            let marker = marker.to_ascii_lowercase();
            marker.contains("source_public_behavior_assertion")
                || marker.contains("public state assertion")
                || marker.contains("public callable signature")
        }) || !evidence.source_refs.is_empty()
            || !evidence.public_state_assertions.is_empty()
            || !evidence.public_missing_attributes.is_empty()
    });
    if source_backed_public_failure {
        return Some(false);
    }

    None
}

fn build_system_prompt(
    request: &AgentRunRequest,
    instruction_text: &str,
    available_skills_text: &str,
    tool_names: &[String],
    prompt_profile: PromptProfile,
) -> Result<String, AgentError> {
    let shell_family = request.config.shell.family.unwrap_or(if cfg!(windows) {
        ShellFamily::PowerShell
    } else {
        ShellFamily::Bash
    });
    let cwd_is_empty = directory_is_empty(&request.session.workspace.cwd)?;
    Ok(render_system_prompt(SystemPromptInput {
        prompt_profile,
        shell_family,
        workspace_root: request.session.workspace.root.as_str(),
        cwd: request.session.workspace.cwd.as_str(),
        model_name: &request.model.name,
        tool_names,
        instruction_text,
        available_skills_text,
        cwd_is_empty,
    }))
}

fn directory_is_empty(path: &Utf8Path) -> Result<bool, AgentError> {
    let mut entries = fs::read_dir(path).map_err(|error| {
        AgentError::Message(format!("failed to read directory `{path}`: {error}"))
    })?;
    Ok(entries
        .next()
        .transpose()
        .map_err(|error| {
            AgentError::Message(format!("failed to inspect directory `{path}`: {error}"))
        })?
        .is_none())
}

fn blocked_reason_mentions_missing_deliverables(reason: &str) -> bool {
    reason
        .to_ascii_lowercase()
        .contains("requested deliverables are still missing from the workspace")
}

fn build_messages_with_state(
    transcript: &Transcript,
    session: &SessionRecord,
    history_items: &[HistoryItem],
    state: &SessionStateSnapshot,
    todos: &[TodoItem],
    limit: usize,
    tool_names: &[String],
    signals: &PromptSignals,
    active_work: Option<&ActiveWorkContract>,
) -> PromptMessageProjection {
    let mut result = Vec::new();
    let start_index = prompt_window_start_index(transcript);
    let (superseded_denied_tools, _) =
        superseded_tool_denial_replay_state(&transcript.messages[start_index..], tool_names);
    let required_write_target = exact_active_authoring_write_required(state);
    let staged_task_docs_only =
        staged_task_documentation_outputs_only(&signals.staged_task_output_targets);
    let staged_task_documentation_focus_targets =
        staged_task_documentation_focus_targets(&signals.execution_focus_targets);
    if !todos.is_empty() {
        result.push(ModelMessage::System {
            content: format!("Current todo focus:\n{}", format_todo_focus(todos)),
        });
    }
    if let Some(target) = required_write_target.as_deref() {
        result.push(ModelMessage::System {
            content: exact_write_target_contract(target),
        });
    }
    if let Some(contract) = active_work {
        result.push(ModelMessage::System {
            content: render_active_work_contract(contract),
        });
    }
    if let Some(rejection_summary) =
        latest_inactive_target_edit_rejection(&transcript.messages[start_index..])
    {
        let active = current_active_todo_item(todos);
        let state_active_targets = state
            .active_targets
            .iter()
            .map(|target| target.as_str().to_string())
            .collect::<Vec<_>>();
        let active_targets = if state_active_targets.is_empty() {
            active
                .map(|todo| {
                    todo.targets
                        .iter()
                        .map(|target| target.as_str().to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        } else {
            state_active_targets
        };
        result.push(ModelMessage::System {
            content: inactive_target_edit_recovery_reminder(
                active.map(|todo| todo.content.as_str()),
                &active_targets,
                &rejection_summary,
                signals.inactive_target_edit_recovery_read_target.as_deref(),
            ),
        });
    }
    match state.route {
        TaskRoute::Summary if !signals.completion_closeout_ready => {
            result.push(ModelMessage::System {
                content: summary_route_reminder(state.completion.blocked_reason.as_deref()),
            })
        }
        TaskRoute::Summary => {}
        TaskRoute::Docs => result.push(ModelMessage::System {
            content: docs_route_reminder(
                &route_scope_targets(state, signals),
                state
                    .docs_route
                    .as_ref()
                    .and_then(|docs| docs.survey_packet_summary.as_deref()),
                state.completion.route_contract_summary.as_deref(),
                docs_route_contract_repair_hint(state).as_deref(),
            ),
        }),
        TaskRoute::Review => result.push(ModelMessage::System {
            content: review_route_reminder(
                &route_scope_targets(state, signals),
                state
                    .review_scope
                    .as_ref()
                    .map(|scope| scope.summary.as_str()),
            ),
        }),
        TaskRoute::Debug => result.push(ModelMessage::System {
            content: debug_route_reminder().to_string(),
        }),
        TaskRoute::Ask => result.push(ModelMessage::System {
            content: ask_route_reminder().to_string(),
        }),
        TaskRoute::Code => {}
    }
    if signals.structured_document_summary_mode {
        result.push(ModelMessage::System {
            content: structured_document_summary_reminder(&route_scope_targets(state, signals)),
        });
        if let Some(content) = render_structured_document_summary_progress(transcript, start_index)
        {
            result.push(ModelMessage::System { content });
        }
    }
    if signals.compaction_replay {
        result.push(ModelMessage::System {
            content: compaction_replay_reminder().to_string(),
        });
        if let Some(content) = render_compaction_continuation_message(state, todos, signals) {
            result.push(ModelMessage::System { content });
        }
    }
    if signals.follow_up_boundary {
        result.push(ModelMessage::System {
            content: follow_up_boundary_reminder().to_string(),
        });
    }
    if let Some(active_request) = &signals.active_follow_up_request {
        result.push(ModelMessage::System {
            content: active_follow_up_request_reminder(active_request),
        });
    }
    if !signals.staged_task_artifacts.is_empty() && !signals.completion_closeout_ready {
        result.push(ModelMessage::System {
            content: staged_task_execution_reminder(
                &signals.staged_task_artifacts,
                &signals.staged_task_output_targets,
                &signals.staged_task_verification_commands,
                current_active_todo_item(todos).map(|todo| todo.content.as_str()),
                &signals.execution_focus_targets,
            ),
        });
        if !signals.staged_task_closeout_mode
            && (staged_task_docs_only
                || (signals.staged_task_documentation_authoring_mode
                    && signals
                        .staged_task_documentation_evidence_snapshot
                        .is_none()))
        {
            result.push(ModelMessage::System {
                content: staged_task_documentation_grounding_reminder(
                    if staged_task_documentation_focus_targets.is_empty() {
                        &signals.staged_task_output_targets
                    } else {
                        &staged_task_documentation_focus_targets
                    },
                ),
            });
        }
        if signals.staged_task_documentation_audit_repair_mode {
            let audit_targets = if signals.staged_task_closeout_mode
                || signals.execution_focus_targets.is_empty()
            {
                signals.staged_task_output_targets.as_slice()
            } else {
                signals.execution_focus_targets.as_slice()
            };
            if let Some(audit_feedback) = &signals.staged_task_documentation_audit_feedback {
                result.push(ModelMessage::System {
                    content: if signals.staged_task_documentation_audit_escalation_mode {
                        staged_task_documentation_audit_escalation_reminder(
                            audit_targets,
                            audit_feedback,
                        )
                    } else {
                        staged_task_documentation_audit_repair_reminder(
                            audit_targets,
                            audit_feedback,
                        )
                    },
                });
            }
        }
        if signals.staged_task_documentation_authoring_mode {
            if let Some(snapshot) = &signals.staged_task_documentation_evidence_snapshot {
                result.push(ModelMessage::System {
                    content: if signals.staged_task_documentation_authoring_focus_mode {
                        staged_task_documentation_authoring_focus_reminder(
                            &signals.execution_focus_targets,
                            &signals.readonly_stall_targets,
                            snapshot,
                            signals.staged_task_documentation_authoring_no_replan_mode,
                        )
                    } else {
                        staged_task_documentation_authoring_reminder(
                            &signals.execution_focus_targets,
                            snapshot,
                        )
                    },
                });
            }
        }
        if signals.staged_task_closeout_mode {
            result.push(ModelMessage::System {
                content: if signals.staged_task_closeout_repair_mode {
                    staged_task_closeout_repair_reminder(
                        &signals.staged_task_closeout_repair_targets,
                        &[],
                        &[],
                    )
                } else if signals.staged_task_closeout_recovery_mode {
                    staged_task_closeout_recovery_reminder(
                        &signals.staged_task_output_targets,
                        &[],
                        &[],
                    )
                } else {
                    staged_task_closeout_reminder(
                        &signals.staged_task_output_targets,
                        signals.staged_task_closeout_read_complete,
                    )
                },
            });
        }
    }
    if signals.follow_up_implementation && !signals.requested_artifact_targets.is_empty() {
        result.push(ModelMessage::System {
            content: follow_up_implementation_scope_reminder(&signals.requested_artifact_targets),
        });
    }
    let spec_targets = implementation_spec_targets(signals);
    if signals.follow_up_implementation && !spec_targets.is_empty() {
        result.push(ModelMessage::System {
            content: follow_up_spec_alignment_reminder(&spec_targets),
        });
    }
    if signals.documentation_scope_explicit && !signals.documentation_scope_targets.is_empty() {
        result.push(ModelMessage::System {
            content: follow_up_documentation_scope_reminder(
                &signals.documentation_scope_targets,
                signals
                    .active_follow_up_request
                    .as_deref()
                    .is_some_and(documentation_change_may_lead_implementation),
            ),
        });
    }
    if signals.interrupted_resume {
        result.push(ModelMessage::System {
            content: interrupted_resume_reminder().to_string(),
        });
    }
    if let Some(error_message) = &signals.last_failure {
        result.push(ModelMessage::System {
            content: failure_reminder(tool_names, error_message),
        });
    }
    if !superseded_denied_tools.is_empty() {
        result.push(ModelMessage::System {
            content: superseded_tool_denial_reminder(&superseded_denied_tools, tool_names),
        });
    }
    if signals.readonly_stall && !(staged_task_docs_only && !signals.follow_up_implementation) {
        result.push(ModelMessage::System {
            content: if signals.follow_up_implementation {
                follow_up_implementation_stall_reminder(&signals.readonly_stall_targets)
            } else {
                readonly_stall_reminder().to_string()
            },
        });
    }
    if signals.edit_recovery_mode && !signals.structured_document_summary_mode {
        result.push(ModelMessage::System {
            content: edit_recovery_reminder(None, &signals.readonly_stall_targets),
        });
    }
    if signals.code_block_stall {
        result.push(ModelMessage::System {
            content: code_block_stall_reminder().to_string(),
        });
    }
    if signals.pseudo_tool_call_stall {
        result.push(ModelMessage::System {
            content: pseudo_tool_call_stall_reminder().to_string(),
        });
    }
    if signals.patch_recovery_mode {
        result.push(ModelMessage::System {
            content: patch_recovery_reminder(&signals.patch_recovery_targets),
        });
    }
    if signals.completion_closeout_ready {
        result.push(ModelMessage::System {
            content: completion_ready_reminder().to_string(),
        });
    }
    let verification_focus_active = matches!(
        state.process_phase,
        crate::session::ProcessPhase::Verify | crate::session::ProcessPhase::Repair
    );
    let typed_verification_failure_projection =
        repair_lane_projection_for_prompt(state).and_then(|projection| {
            projection.operation_template.map(|template| {
                format!(
                    "Typed repair operation `{}` is focused on target {:?}.",
                    template.operation_kind, template.exact_target
                )
            })
        });
    if verification_focus_active {
        let todo_content = "Run the required verification command";
        result.push(ModelMessage::System {
            content: verification_pending_reminder(
                todo_content,
                &state.verification.required_commands,
            ),
        });
        {
            let todo = todo_content;
            if signals.verification_failure_repair_mode {
                result.push(ModelMessage::System {
                    content: if signals.verification_failure_repair_edit_focused_mode {
                        verification_failure_repair_edit_focused_reminder(
                            todo,
                            &signals.verification_failure_labels,
                            typed_verification_failure_projection.as_deref(),
                            &state
                                .active_targets
                                .iter()
                                .map(|value| value.as_str().to_string())
                                .collect::<Vec<_>>(),
                            signals.verification_repair_focus_target.as_deref(),
                        )
                    } else if signals.verification_repair_rerun_due {
                        verification_rerun_preferred_reminder(
                            todo,
                            &state.verification.required_commands,
                            &signals.verification_failure_labels,
                            typed_verification_failure_projection.as_deref(),
                            &state
                                .active_targets
                                .iter()
                                .map(|value| value.as_str().to_string())
                                .collect::<Vec<_>>(),
                        )
                    } else {
                        verification_failure_repair_reminder(
                            todo,
                            &signals.verification_failure_labels,
                            typed_verification_failure_projection.as_deref(),
                            &state
                                .active_targets
                                .iter()
                                .map(|value| value.as_str().to_string())
                                .collect::<Vec<_>>(),
                        )
                    },
                });
                if let Some(target) = signals.verification_repair_next_read_target.as_ref() {
                    result.push(ModelMessage::System {
                        content: format!(
                            "Verification repair grounding: before the next rewrite, use `read` exactly once on `{target}` if the file has not already been shown in this failure cycle. Do not reread another target. After this grounding read, make one concrete `write` repair and rerun the exact verification command."
                        ),
                    });
                }
            }
            if signals.verification_recovery_mode && !signals.patch_recovery_mode {
                let verification_failure_targets = state
                    .active_targets
                    .iter()
                    .map(|value| value.as_str().to_string())
                    .collect::<Vec<_>>();
                result.push(ModelMessage::System {
                    content: if signals.verification_repair_rerun_due
                        && matches!(
                            state.failure.as_ref().map(|failure| failure.kind),
                            Some(crate::session::FailureKind::VerificationFailed)
                        ) {
                        verification_rerun_preferred_reminder(
                            todo,
                            &state.verification.required_commands,
                            &signals.verification_failure_labels,
                            typed_verification_failure_projection.as_deref(),
                            &verification_failure_targets,
                        )
                    } else {
                        verification_recovery_reminder(todo, &state.verification.required_commands)
                    },
                });
            }
        }
    }
    let replay_context = ProviderReplayContext::from_state(state);
    let provider_replay = build_provider_replay_projection_from_history_items(
        session,
        history_items,
        limit,
        &replay_context,
    );
    result.extend(provider_replay.messages);
    let _ = (todos, tool_names);
    PromptMessageProjection {
        messages: result,
        replay_policies: provider_replay.replay_policies,
    }
}

pub fn build_provider_replay_messages_from_history_items(
    _session: &SessionRecord,
    history_items: &[HistoryItem],
    limit: usize,
) -> Vec<ModelMessage> {
    build_provider_replay_projection_from_history_items(
        _session,
        history_items,
        limit,
        &ProviderReplayContext::default(),
    )
    .messages
}

fn build_provider_replay_projection_from_history_items(
    _session: &SessionRecord,
    history_items: &[HistoryItem],
    limit: usize,
    replay_context: &ProviderReplayContext,
) -> ProviderReplayProjection {
    let mut result = Vec::new();
    let mut replay_policies = Vec::new();
    let replay_start = latest_compaction_history_index(history_items)
        .map(|index| index + 1)
        .unwrap_or(0);
    if let Some(content) = latest_compaction_provider_context(history_items) {
        result.push(ModelMessage::System { content });
    }
    let selected_indices = provider_replay_repair_leading_orphans(
        history_items,
        provider_replay_selected_indices(history_items, replay_start, limit),
    );
    let tool_call_index = tool_call_history_index_after(history_items, replay_start);
    let tool_output_index = first_tool_output_history_index_after(history_items, replay_start);
    let stale_inactive_authoring_calls = stale_inactive_authoring_tool_call_targets_after(
        history_items,
        replay_start,
        replay_context,
    );
    let failed_inactive_authoring_calls = failed_inactive_authoring_tool_call_targets_after(
        history_items,
        replay_start,
        replay_context,
        &tool_output_index,
    );
    let historical_progress_projection_calls =
        historical_progress_projection_tool_call_targets_after(
            history_items,
            replay_start,
            replay_context,
            &tool_output_index,
        );
    let mut emitted_outputs = BTreeSet::new();
    let mut emitted_tool_calls = BTreeSet::new();

    for index in selected_indices {
        let Some(item) = history_items.get(index) else {
            continue;
        };
        match &item.payload {
            HistoryItemPayload::UserTurn {
                content,
                editor_context,
                ..
            } => {
                if let Some(message) = content_parts_to_user_message(content, editor_context) {
                    result.push(message);
                }
            }
            HistoryItemPayload::Message { role, content, .. } => match role {
                MessageRole::User => {
                    if let Some(message) = content_parts_to_user_message(content, &None) {
                        result.push(message);
                    }
                }
                MessageRole::Assistant => {
                    if let Some(text) = content_parts_text(content) {
                        result.push(ModelMessage::Assistant { content: text });
                    }
                }
            },
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                model_arguments,
                effective_arguments,
                ..
            } => {
                let call_id_text = call_id.to_string();
                let content_shape_mismatch = tool == &ToolName::Write
                    && tool_call_has_content_shape_mismatch_output(
                        history_items,
                        &tool_output_index,
                        &call_id_text,
                    );
                let arguments_json = if content_shape_mismatch {
                    sanitized_content_shape_mismatch_arguments_json()
                } else if let Some(stale_targets) =
                    failed_inactive_authoring_calls.get(&call_id_text)
                {
                    replay_policies.push(failed_inactive_authoring_replay_policy(
                        &call_id_text,
                        tool,
                        stale_targets,
                        &replay_context.active_authoring_targets,
                    ));
                    replay_tool_arguments_json(arguments, model_arguments, effective_arguments)
                } else if let Some(stale_targets) =
                    stale_inactive_authoring_calls.get(&call_id_text)
                {
                    replay_policies.push(stale_inactive_authoring_replay_policy(
                        &call_id_text,
                        tool,
                        stale_targets,
                        &replay_context.active_authoring_targets,
                    ));
                    result.push(ModelMessage::System {
                        content: stale_inactive_authoring_pair_replay_note(
                            stale_targets,
                            &replay_context.active_authoring_targets,
                        ),
                    });
                    continue;
                } else if let Some(progress_targets) =
                    historical_progress_projection_calls.get(&call_id_text)
                {
                    replay_policies.push(progress_projection_replay_policy(
                        &call_id_text,
                        tool,
                        progress_targets,
                        &replay_context.active_authoring_targets,
                    ));
                    result.push(ModelMessage::System {
                        content: progress_projection_pair_replay_note(
                            &replay_context.active_authoring_targets,
                        ),
                    });
                    continue;
                } else {
                    replay_tool_arguments_json(arguments, model_arguments, effective_arguments)
                };
                if !tool_call_arguments_are_replayable(&arguments_json) {
                    continue;
                }
                result.push(ModelMessage::AssistantToolCalls {
                    content: None,
                    tool_calls: vec![ModelToolCall {
                        call_id: call_id_text.clone(),
                        tool_name: tool.to_string(),
                        arguments_json,
                    }],
                });
                emitted_tool_calls.insert(call_id_text.clone());
                if !tool_output_index.contains_key(&call_id_text)
                    && emitted_outputs.insert(call_id_text.clone())
                {
                    result.push(ModelMessage::Tool {
                        call_id: call_id_text,
                        tool_name: tool.to_string(),
                        result: "aborted".to_string(),
                    });
                }
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                output_text,
                ..
            } => {
                let call_id_text = call_id.to_string();
                if !emitted_outputs.insert(call_id_text.clone()) {
                    continue;
                }
                if !emitted_tool_calls.contains(&call_id_text) {
                    continue;
                }
                let Some((tool_name, _)) = tool_call_index.get(&call_id_text) else {
                    continue;
                };
                let result_text = if tool_call_has_content_shape_mismatch_output(
                    history_items,
                    &tool_output_index,
                    &call_id_text,
                ) {
                    output_text.clone()
                } else if failed_inactive_authoring_calls.contains_key(&call_id_text) {
                    output_text.clone()
                } else if stale_inactive_authoring_calls.contains_key(&call_id_text) {
                    continue;
                } else if historical_progress_projection_calls.contains_key(&call_id_text) {
                    continue;
                } else {
                    output_text.clone()
                };
                result.push(ModelMessage::Tool {
                    call_id: call_id_text,
                    tool_name: tool_name.clone(),
                    result: result_text,
                });
            }
            HistoryItemPayload::Reasoning { .. }
            | HistoryItemPayload::RejectedToolProposal { .. }
            | HistoryItemPayload::CandidateRepairEdit { .. }
            | HistoryItemPayload::RequestDiagnostics { .. }
            | HistoryItemPayload::Continuation { .. }
            | HistoryItemPayload::StateProjection { .. }
            | HistoryItemPayload::SessionState { .. }
            | HistoryItemPayload::ApprovalDecision { .. }
            | HistoryItemPayload::RetryDecision { .. }
            | HistoryItemPayload::ControlEnvelope { .. }
            | HistoryItemPayload::Compaction { .. }
            | HistoryItemPayload::FileChange { .. }
            | HistoryItemPayload::Error { .. }
            | HistoryItemPayload::PromptDispatch { .. } => {}
        }
    }

    ProviderReplayProjection {
        messages: result,
        replay_policies,
    }
}

fn provider_replay_selected_indices(
    history_items: &[HistoryItem],
    start: usize,
    limit: usize,
) -> Vec<usize> {
    let mut selected = BTreeSet::new();
    let effective_limit = limit.max(1);
    for index in (start..history_items.len()).rev() {
        if selected.len() >= effective_limit {
            break;
        }
        if provider_replay_item_is_visible(&history_items[index].payload) {
            selected.insert(index);
        }
    }
    if let Some(latest_user) = latest_user_turn_index_after(history_items, start) {
        selected.insert(latest_user);
    } else if start > 0
        && let Some(latest_user) = latest_user_turn_index_after(history_items, 0)
    {
        selected.insert(latest_user);
    }
    provider_replay_add_tool_pairs(history_items, start, &mut selected);
    selected.into_iter().collect()
}

fn provider_replay_repair_leading_orphans(
    history_items: &[HistoryItem],
    selected_indices: Vec<usize>,
) -> Vec<usize> {
    let mut selected_indices = selected_indices;
    if let Some(first_index) = selected_indices.first().copied()
        && !history_item_is_user_query(history_items, first_index)
        && let Some(prior_user) = latest_user_turn_index_before(history_items, first_index)
    {
        selected_indices.push(prior_user);
        selected_indices.sort_unstable();
        selected_indices.dedup();
    }

    let Some(first_user_position) = selected_indices
        .iter()
        .position(|index| history_item_is_user_query(history_items, *index))
    else {
        return selected_indices;
    };
    selected_indices[first_user_position..].to_vec()
}

fn history_item_is_user_query(history_items: &[HistoryItem], index: usize) -> bool {
    history_items
        .get(index)
        .map(|item| {
            matches!(
                item.payload,
                HistoryItemPayload::UserTurn { .. }
                    | HistoryItemPayload::Message {
                        role: MessageRole::User,
                        ..
                    }
            )
        })
        .unwrap_or(false)
}

fn latest_user_turn_index_before(history_items: &[HistoryItem], index: usize) -> Option<usize> {
    history_items[..index]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(offset, item)| {
            matches!(
                item.payload,
                HistoryItemPayload::UserTurn { .. }
                    | HistoryItemPayload::Message {
                        role: MessageRole::User,
                        ..
                    }
            )
            .then_some(offset)
        })
}

fn provider_replay_item_is_visible(payload: &HistoryItemPayload) -> bool {
    matches!(
        payload,
        HistoryItemPayload::UserTurn { .. }
            | HistoryItemPayload::Message { .. }
            | HistoryItemPayload::ToolCall { .. }
            | HistoryItemPayload::ToolOutput { .. }
    )
}

fn provider_replay_add_tool_pairs(
    history_items: &[HistoryItem],
    start: usize,
    selected: &mut BTreeSet<usize>,
) {
    let call_index = tool_call_history_index_after(history_items, start);
    let output_index = first_tool_output_history_index_after(history_items, start);
    let mut changed = true;
    while changed {
        changed = false;
        for index in selected.clone() {
            match &history_items[index].payload {
                HistoryItemPayload::ToolCall { call_id, .. } => {
                    if let Some(output_index) = output_index.get(&call_id.to_string())
                        && selected.insert(*output_index)
                    {
                        changed = true;
                    }
                }
                HistoryItemPayload::ToolOutput { call_id, .. } => {
                    if let Some((_, call_index)) = call_index.get(&call_id.to_string())
                        && selected.insert(*call_index)
                    {
                        changed = true;
                    }
                }
                _ => {}
            }
        }
    }
}

fn tool_call_history_index_after(
    history_items: &[HistoryItem],
    start: usize,
) -> BTreeMap<String, (String, usize)> {
    let mut calls = BTreeMap::new();
    for (index, item) in history_items.iter().enumerate().skip(start) {
        if let HistoryItemPayload::ToolCall { call_id, tool, .. } = &item.payload {
            calls
                .entry(call_id.to_string())
                .or_insert_with(|| (tool.to_string(), index));
        }
    }
    calls
}

fn first_tool_output_history_index_after(
    history_items: &[HistoryItem],
    start: usize,
) -> BTreeMap<String, usize> {
    let mut outputs = BTreeMap::new();
    for (index, item) in history_items.iter().enumerate().skip(start) {
        if let HistoryItemPayload::ToolOutput { call_id, .. } = &item.payload {
            outputs.entry(call_id.to_string()).or_insert(index);
        }
    }
    outputs
}

fn tool_call_has_content_shape_mismatch_output(
    history_items: &[HistoryItem],
    output_index: &BTreeMap<String, usize>,
    call_id: &str,
) -> bool {
    let Some(index) = output_index.get(call_id) else {
        return false;
    };
    matches!(
        history_items.get(*index).map(|item| &item.payload),
        Some(HistoryItemPayload::ToolOutput { title, .. })
            if title == "Required write content shape mismatch"
    )
}

fn tool_call_has_wrong_authoring_target_output(
    history_items: &[HistoryItem],
    output_index: &BTreeMap<String, usize>,
    call_id: &str,
) -> bool {
    let Some(index) = output_index.get(call_id) else {
        return false;
    };
    match history_items.get(*index).map(|item| &item.payload) {
        Some(HistoryItemPayload::ToolOutput {
            title, metadata, ..
        }) => {
            title == "Wrong authoring target"
                || metadata
                    .get("operation_progress_class")
                    .and_then(Value::as_str)
                    == Some("wrong_authoring_target")
                || metadata
                    .get("tool_feedback_envelope")
                    .and_then(|feedback| feedback.get("operation_progress_class"))
                    .and_then(Value::as_str)
                    == Some("wrong_authoring_target")
        }
        _ => false,
    }
}

fn sanitized_content_shape_mismatch_arguments_json() -> String {
    json!({
        "content": "[omitted incompatible write payload; runtime rejected it before side effects. See the following tool result for the required content contract.]"
    })
    .to_string()
}

fn latest_user_turn_index_after(history_items: &[HistoryItem], start: usize) -> Option<usize> {
    history_items[start..]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(offset, item)| {
            matches!(
                item.payload,
                HistoryItemPayload::UserTurn { .. }
                    | HistoryItemPayload::Message {
                        role: MessageRole::User,
                        ..
                    }
            )
            .then_some(start + offset)
        })
}

fn latest_compaction_history_index(history_items: &[HistoryItem]) -> Option<usize> {
    history_items
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, item)| {
            matches!(item.payload, HistoryItemPayload::Compaction { .. }).then_some(index)
        })
}

fn latest_compaction_provider_context(history_items: &[HistoryItem]) -> Option<String> {
    history_items.iter().rev().find_map(|item| {
        if let HistoryItemPayload::Compaction {
            summary,
            continuation,
            ..
        } = &item.payload
        {
            let mut sections = Vec::new();
            if !summary.trim().is_empty() {
                sections.push(format!(
                    "Conversation summary from earlier turns:\n{summary}"
                ));
            }
            if let Some(continuation) = continuation {
                sections.push(format!(
                    "Typed continuation contract:\n{}",
                    serde_json::to_string(continuation)
                        .unwrap_or_else(|_| "unserializable continuation".to_string())
                ));
            }
            (!sections.is_empty()).then(|| sections.join("\n\n"))
        } else {
            None
        }
    })
}

fn replay_tool_arguments_json(
    arguments: &Value,
    model_arguments: &Value,
    effective_arguments: &Value,
) -> String {
    let value = if !effective_arguments.is_null() {
        effective_arguments
    } else if !model_arguments.is_null() {
        model_arguments
    } else {
        arguments
    };
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

fn stale_inactive_authoring_tool_call_targets_after(
    history_items: &[HistoryItem],
    start: usize,
    replay_context: &ProviderReplayContext,
) -> BTreeMap<String, Vec<String>> {
    let active_targets = &replay_context.active_authoring_targets;
    if active_targets.is_empty() {
        return BTreeMap::new();
    }
    let mut stale_calls = BTreeMap::new();
    for item in history_items.iter().skip(start) {
        let HistoryItemPayload::ToolCall {
            call_id,
            tool,
            arguments,
            model_arguments,
            effective_arguments,
            ..
        } = &item.payload
        else {
            continue;
        };
        if !matches!(tool, ToolName::Write | ToolName::ApplyPatch) {
            continue;
        }
        let arguments_json =
            replay_tool_arguments_json(arguments, model_arguments, effective_arguments);
        let submitted_targets = artifact_targets_from_tool_call(&tool.to_string(), &arguments_json);
        if submitted_targets.is_empty()
            || submitted_targets_intersect_active(&submitted_targets, active_targets)
        {
            continue;
        }
        stale_calls.insert(call_id.to_string(), submitted_targets);
    }
    stale_calls
}

fn failed_inactive_authoring_tool_call_targets_after(
    history_items: &[HistoryItem],
    start: usize,
    replay_context: &ProviderReplayContext,
    output_index: &BTreeMap<String, usize>,
) -> BTreeMap<String, Vec<String>> {
    stale_inactive_authoring_tool_call_targets_after(history_items, start, replay_context)
        .into_iter()
        .filter(|(call_id, _)| {
            tool_call_has_wrong_authoring_target_output(history_items, output_index, call_id)
        })
        .collect()
}

fn historical_progress_projection_tool_call_targets_after(
    history_items: &[HistoryItem],
    start: usize,
    replay_context: &ProviderReplayContext,
    output_index: &BTreeMap<String, usize>,
) -> BTreeMap<String, Vec<String>> {
    if replay_context.active_authoring_targets.is_empty() {
        return BTreeMap::new();
    }
    let mut progress_calls = BTreeMap::new();
    for item in history_items.iter().skip(start) {
        let HistoryItemPayload::ToolCall {
            call_id,
            tool,
            arguments,
            model_arguments,
            effective_arguments,
            ..
        } = &item.payload
        else {
            continue;
        };
        if tool != &ToolName::TodoWrite {
            continue;
        }
        let call_id_text = call_id.to_string();
        if progress_projection_output_carries_current_feedback(
            history_items,
            output_index,
            &call_id_text,
            &replay_context.active_authoring_targets,
        ) {
            continue;
        }
        let arguments_json =
            replay_tool_arguments_json(arguments, model_arguments, effective_arguments);
        progress_calls.insert(
            call_id_text,
            progress_projection_targets_from_todo_arguments(&arguments_json),
        );
    }
    progress_calls
}

fn progress_projection_output_carries_current_feedback(
    history_items: &[HistoryItem],
    output_index: &BTreeMap<String, usize>,
    call_id: &str,
    active_targets: &[String],
) -> bool {
    if active_targets.is_empty() {
        return false;
    }
    let Some(index) = output_index.get(call_id) else {
        return false;
    };
    let Some(HistoryItemPayload::ToolOutput {
        output_text,
        metadata,
        ..
    }) = history_items.get(*index).map(|item| &item.payload)
    else {
        return false;
    };
    let output_lower = output_text.to_ascii_lowercase();
    let metadata_progress_class = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("operation_progress_class"))
        .or_else(|| metadata.get("operation_progress_class"))
        .and_then(Value::as_str);
    let metadata_progress_effect = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("progress_effect"))
        .or_else(|| metadata.get("progress_effect"))
        .and_then(Value::as_str);
    let is_current_no_progress_projection = (output_lower.contains("progress_projection")
        || metadata_progress_class == Some("progress_projection"))
        && (output_lower.contains("no_progress")
            || metadata_progress_effect == Some("no_progress"));
    if !is_current_no_progress_projection {
        return false;
    }
    let metadata_targets = metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get("active_targets"))
        .or_else(|| metadata.get("active_targets"))
        .and_then(Value::as_array)
        .map(|targets| {
            targets
                .iter()
                .filter_map(Value::as_str)
                .map(normalize_prompt_target)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    active_targets.iter().all(|target| {
        let normalized = normalize_prompt_target(target);
        output_text.contains(target)
            || metadata_targets
                .iter()
                .any(|candidate| candidate == &normalized)
    })
}

fn progress_projection_targets_from_todo_arguments(arguments_json: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(arguments_json) else {
        return Vec::new();
    };
    let Some(todos) = value.get("todos").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut targets = Vec::new();
    for todo in todos {
        if let Some(todo_targets) = todo.get("targets").and_then(Value::as_array) {
            targets.extend(
                todo_targets
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string),
            );
        }
        if let Some(content) = todo.get("content").and_then(Value::as_str) {
            targets.extend(extract_requested_artifact_targets(content));
        }
        if let Some(success_criteria) = todo.get("success_criteria").and_then(Value::as_array) {
            for criterion in success_criteria.iter().filter_map(Value::as_str) {
                targets.extend(extract_requested_artifact_targets(criterion));
            }
        }
    }
    dedupe_targets(targets)
}

fn submitted_targets_intersect_active(submitted: &[String], active: &[String]) -> bool {
    submitted.iter().any(|submitted_target| {
        prompt_target_matches_required_output(submitted_target, active)
            || active.iter().any(|active_target| {
                prompt_target_matches_required_output(active_target, &[submitted_target.clone()])
            })
    })
}

fn stale_inactive_authoring_pair_replay_note(
    stale_targets: &[String],
    active_targets: &[String],
) -> String {
    let stale = stale_targets
        .iter()
        .map(|target| format!("`{target}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let active = active_targets
        .iter()
        .map(|target| format!("`{target}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Previous authoring tool call/output pair for inactive target(s) {stale} is omitted from executable provider tool-call history because the current active requested-work target set is {active}. Treat this as non-executable historical context; use the current active-work projection and stable tool schema."
    )
}

fn progress_projection_pair_replay_note(active_targets: &[String]) -> String {
    let active = active_targets
        .iter()
        .map(|target| format!("`{target}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Historical progress-projection tool call/output pair is omitted from executable provider tool-call history because content-changing authoring is still active. Treat this as non-executable planning context only; the current active requested-work target set is {active}."
    )
}

fn stale_inactive_authoring_replay_policy(
    call_id: &str,
    tool: &ToolName,
    stale_targets: &[String],
    active_targets: &[String],
) -> RequestReplayPolicyDiagnostic {
    RequestReplayPolicyDiagnostic {
        policy: "stale_inactive_authoring_payload_omitted".to_string(),
        call_id: Some(call_id.to_string()),
        tool_name: Some(tool.to_string()),
        omitted_targets: stale_targets.to_vec(),
        active_targets: active_targets.to_vec(),
        reason: "canonical tool call/output items are preserved, but stale inactive authoring arguments are omitted from provider-visible replay after active requested-work target rotation".to_string(),
    }
}

fn failed_inactive_authoring_replay_policy(
    call_id: &str,
    tool: &ToolName,
    stale_targets: &[String],
    active_targets: &[String],
) -> RequestReplayPolicyDiagnostic {
    RequestReplayPolicyDiagnostic {
        policy: "failed_inactive_authoring_call_output_preserved".to_string(),
        call_id: Some(call_id.to_string()),
        tool_name: Some(tool.to_string()),
        omitted_targets: stale_targets.to_vec(),
        active_targets: active_targets.to_vec(),
        reason: "failed wrong-target authoring remains a call-id-scoped ToolCall/ToolOutput pair in provider replay so the model sees the previous rejected call result; successful stale inactive authoring payloads remain summary-only".to_string(),
    }
}

fn progress_projection_replay_policy(
    call_id: &str,
    tool: &ToolName,
    progress_targets: &[String],
    active_targets: &[String],
) -> RequestReplayPolicyDiagnostic {
    RequestReplayPolicyDiagnostic {
        policy: "progress_projection_payload_omitted".to_string(),
        call_id: Some(call_id.to_string()),
        tool_name: Some(tool.to_string()),
        omitted_targets: progress_targets.to_vec(),
        active_targets: active_targets.to_vec(),
        reason: "canonical progress-projection tool call/output items are preserved, but historical todo/plan JSON is omitted from provider-visible executable replay while active content-changing authoring targets remain".to_string(),
    }
}

fn content_parts_to_user_message(
    content: &[ContentPart],
    editor_context: &Option<crate::session::EditorContext>,
) -> Option<ModelMessage> {
    let mut parts = Vec::new();
    let mut text = content_parts_text(content).unwrap_or_default();
    if let Some(editor_context) = editor_context {
        let suffix = render_editor_context(editor_context);
        if !suffix.is_empty() {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(&suffix);
        }
    }
    if !text.is_empty() {
        parts.push(ModelContentPart::Text { text });
    }
    let mut image_index = 0usize;
    for part in content {
        let ContentPart::Image { image } = part else {
            continue;
        };
        image_index += 1;
        parts.push(ModelContentPart::Text {
            text: format!("<image name=[Image #{image_index}]>"),
        });
        parts.push(ModelContentPart::Image {
            mime_type: image.mime_type.clone(),
            data_base64: image.data_base64.clone(),
        });
        parts.push(ModelContentPart::Text {
            text: "</image>".to_string(),
        });
    }
    if parts.is_empty() {
        None
    } else if parts
        .iter()
        .all(|part| matches!(part, ModelContentPart::Text { .. }))
    {
        let text = parts
            .into_iter()
            .filter_map(|part| match part {
                ModelContentPart::Text { text } => Some(text),
                ModelContentPart::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(ModelMessage::User { content: text })
    } else {
        Some(ModelMessage::UserParts { parts })
    }
}

pub fn vision_input_provider_projection_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "vision fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: camino::Utf8PathBuf::from("."),
        model: "fixture-model".to_string(),
        base_url: "http://fixture".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let source_path = camino::Utf8PathBuf::from("C:/diagnostic/source/js-space_invaders01.jpg");
    let history_items = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id.clone(),
        turn_id: crate::protocol::TurnId::new(),
        sequence_no: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![
                ContentPart::Text {
                    text: "Use attached [Image #1] as visual reference.".to_string(),
                },
                ContentPart::Image {
                    image: crate::session::ImagePart {
                        source_path: Some(source_path),
                        mime_type: "image/jpeg".to_string(),
                        data_base64: "AAAA".to_string(),
                        byte_len: 4,
                    },
                },
            ],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
        created_at_ms: 1,
    }];

    let messages = build_provider_replay_messages_from_history_items(&session, &history_items, 10);
    let Some(ModelMessage::UserParts { parts }) = messages.first() else {
        return false;
    };
    if parts.len() != 4 {
        return false;
    }
    let rendered_text = parts
        .iter()
        .filter_map(|part| match part {
            ModelContentPart::Text { text } => Some(text.as_str()),
            ModelContentPart::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    rendered_text.contains("[Image #1]")
        && rendered_text.contains("<image name=[Image #1]>")
        && rendered_text.contains("</image>")
        && !rendered_text.contains("js-space_invaders01.jpg")
        && !rendered_text.contains("C:/diagnostic/source")
        && matches!(
            parts.get(2),
            Some(ModelContentPart::Image {
                mime_type,
                data_base64
            }) if mime_type == "image/jpeg" && data_base64 == "AAAA"
        )
}

fn content_parts_text(content: &[ContentPart]) -> Option<String> {
    let text = content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.trim()),
            ContentPart::Image { .. } => None,
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn target_is_test_like(target: &str) -> bool {
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

fn target_is_documentation_like(target: &str) -> bool {
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    let file_name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    matches!(
        file_name,
        "readme.md" | "design.md" | "basic_design.md" | "detail_design.md" | "detailed_design.md"
    ) || file_name.ends_with(".md")
        || file_name.ends_with(".markdown")
        || normalized.contains("/docs/")
}

fn target_is_python_source_like(target: &str) -> bool {
    target
        .replace('\\', "/")
        .to_ascii_lowercase()
        .ends_with(".py")
}

fn verification_repair_import_export_focus_target(state: &SessionStateSnapshot) -> Option<String> {
    let projection = repair_lane_projection_for_prompt(state)?;
    let diagnostic = projection.diagnostic();
    if diagnostic.subtype != "import_export_missing_export" {
        return None;
    }

    diagnostic
        .required_target
        .filter(|target| !target_is_test_like(target))
        .or_else(|| {
            state
                .active_targets
                .iter()
                .find(|target| !target_is_test_like(target.as_str()))
                .map(|target| target.as_str().to_string())
        })
}

fn render_structured_document_summary_progress(
    transcript: &Transcript,
    start_index: usize,
) -> Option<String> {
    let latest_user = latest_user_text(transcript, start_index);
    let snapshot = structured_document_summary_snapshot(transcript, latest_user.as_deref())?;
    let mut lines = vec![
        "Structured document progress in this run:".to_string(),
        format!(
            "Processed: {}/{} via `docling_convert`.",
            snapshot.processed_files.len(),
            snapshot.expected_files.len()
        ),
    ];
    if snapshot.batch_size.is_some() && !snapshot.expected_batch_sizes.is_empty() {
        lines.push(format!(
            "Requested batch loop: {:?}. Observed so far: {:?}.",
            snapshot.expected_batch_sizes, snapshot.observed_batch_sizes
        ));
    }
    if let Some(current_batch_expected) = snapshot.current_batch_expected {
        lines.push(format!(
            "Current batch progress before the next summary update: {}/{}.",
            snapshot.current_batch_processed, current_batch_expected
        ));
        if snapshot.current_batch_processed >= current_batch_expected {
            if snapshot.missing_files.is_empty() {
                lines.push(
                    "The final batch is complete. Rewrite the full `docs.md` now, then verify the processed count and update count before finishing."
                        .to_string(),
                );
            } else {
                lines.push(
                    "The current batch is complete. Do not convert another source yet. Rewrite the full `docs.md` now before starting the next batch."
                        .to_string(),
                );
            }
        } else if !snapshot.missing_files.is_empty() && !snapshot.processed_files.is_empty() {
            lines.push(
                "Do not reread or rewrite `docs.md` yet. Continue the current batch with `docling_convert` on the next exact source file."
                    .to_string(),
            );
        }
    }
    if !snapshot.missing_files.is_empty() {
        let preview = snapshot
            .missing_files
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>();
        lines.push(format!("Next exact filenames: {}", preview.join(", ")));
        if snapshot.missing_files.len() > preview.len() {
            lines.push(format!(
                "Still remaining after that: {} more file(s).",
                snapshot.missing_files.len() - preview.len()
            ));
        }
    }
    lines.push(
        "Preserve the exact filenames above. Do not swap hyphens and underscores, and do not restart broad discovery. Convert the next unprocessed file or update the summary document now."
            .to_string(),
    );
    Some(lines.join("\n"))
}

fn render_compaction_continuation_message(
    state: &SessionStateSnapshot,
    todos: &[TodoItem],
    signals: &PromptSignals,
) -> Option<String> {
    if !signals.compaction_replay || signals.completion_closeout_ready {
        return None;
    }

    let _ = todos;
    let active_todo = None;
    let verification_todo = None;
    let targets = state
        .active_targets
        .iter()
        .map(|value| value.as_str().to_string())
        .collect::<Vec<_>>();
    let failure_summary = signals
        .last_failure
        .as_deref()
        .or_else(|| state.failure.as_ref().map(|value| value.summary.as_str()));

    if active_todo.is_none()
        && verification_todo.is_none()
        && targets.is_empty()
        && failure_summary.is_none()
    {
        return None;
    }

    Some(compaction_continuation_reminder(
        active_todo,
        verification_todo,
        failure_summary,
        &targets,
    ))
}

fn render_editor_context(editor_context: &crate::session::EditorContext) -> String {
    let mut lines = vec![
        "[editor context]".to_string(),
        format!("shell_family={:?}", editor_context.shell_family).to_lowercase(),
        format!("current_time_ms={}", editor_context.current_time_ms),
    ];
    if let Some(active_file) = editor_context.active_file.as_ref() {
        lines.push(format!("active_file={active_file}"));
    }
    if !editor_context.visible_files.is_empty() {
        lines.push(format!(
            "visible_files={}",
            editor_context
                .visible_files
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !editor_context.open_tabs.is_empty() {
        lines.push(format!(
            "open_tabs={}",
            editor_context
                .open_tabs
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    lines.join("\n")
}

fn detect_prompt_signals_with_config(
    transcript: &Transcript,
    todos: &[TodoItem],
    agent_config: &AgentConfig,
    state: Option<&SessionStateSnapshot>,
) -> PromptSignals {
    let start_index = prompt_window_start_index(transcript);
    let latest_user_text = latest_user_text(transcript, start_index);
    let requested_contract = latest_user_text
        .as_deref()
        .map(requested_work_contract_from_instruction_text)
        .unwrap_or_default();
    let follow_up_boundary = has_historical_turns_before_latest_user(transcript, start_index);
    let explicit_targets = latest_user_text
        .as_deref()
        .map(explicit_artifact_targets_in_text)
        .unwrap_or_default();
    let protected_targets = latest_user_text
        .as_deref()
        .map(extract_protected_artifact_targets)
        .unwrap_or_default();
    let effective_requested_targets = requested_contract
        .deliverable_targets
        .iter()
        .filter(|target| {
            !protected_targets
                .iter()
                .any(|protected| protected.eq_ignore_ascii_case(target))
        })
        .cloned()
        .collect::<Vec<_>>();
    let effective_explicit_targets = explicit_targets
        .iter()
        .filter(|target| {
            !protected_targets
                .iter()
                .any(|protected| protected.eq_ignore_ascii_case(target))
        })
        .cloned()
        .collect::<Vec<_>>();
    let editor_context_targets = latest_user_editor_context_targets(transcript, start_index);
    let implementation_follows_prior_documentation = latest_user_text
        .as_deref()
        .is_some_and(implementation_follow_up_references_prior_design);
    let implicit_documentation_targets = if effective_requested_targets.is_empty()
        && effective_explicit_targets.is_empty()
        && latest_user_text
            .as_deref()
            .is_some_and(documentation_only_follow_up_requested)
    {
        dedupe_targets(
            editor_context_targets
                .iter()
                .cloned()
                .chain(recent_documentation_targets_before_latest_user(
                    transcript,
                    start_index,
                ))
                .collect(),
        )
    } else {
        Vec::new()
    };
    let implicit_prior_design_targets = if implementation_follows_prior_documentation {
        dedupe_targets(
            editor_context_targets
                .iter()
                .cloned()
                .chain(recent_documentation_targets_before_latest_user(
                    transcript,
                    start_index,
                ))
                .collect(),
        )
    } else {
        Vec::new()
    };
    let focus_seed_targets = dedupe_targets(
        effective_requested_targets
            .iter()
            .cloned()
            .chain(effective_explicit_targets.iter().cloned())
            .chain(implicit_documentation_targets.iter().cloned())
            .collect(),
    );
    let requested_focus = focus_from_targets(&focus_seed_targets);
    let documentation_scope_explicit = documentation_scope_explicit_for_requested_focus(
        requested_focus,
        latest_user_text.as_deref(),
    );
    let observed_activity = observe_follow_up_activity(transcript, start_index);
    let focus = if documentation_scope_explicit {
        FollowUpFocus::Documentation
    } else {
        resolve_follow_up_focus(requested_focus, &observed_activity)
    };
    let staged_task_artifacts = dedupe_targets(
        effective_explicit_targets
            .iter()
            .filter(|target| is_staged_task_artifact_target(target))
            .cloned()
            .chain(staged_task_artifacts_seen(transcript, start_index))
            .collect(),
    );
    let staged_task_output_targets =
        staged_task_output_targets(transcript, start_index, &staged_task_artifacts);
    let staged_task_verification_commands = staged_task_verification_commands(
        latest_user_text.as_deref(),
        &staged_task_artifacts,
        transcript.session.cwd.as_path(),
    );
    let execution_focus_targets = current_active_todo_item(todos)
        .map(|todo| {
            let explicit_targets = if todo.targets.is_empty() {
                extract_requested_artifact_targets(&todo.content)
            } else {
                Vec::new()
            };
            todo.targets
                .iter()
                .map(|target| target.as_str().to_string())
                .chain(explicit_targets)
                .collect::<Vec<_>>()
        })
        .map(dedupe_targets)
        .unwrap_or_default();
    let staged_task_documentation_focus_targets =
        staged_task_documentation_focus_targets(&execution_focus_targets);
    let staged_task_execution_active =
        !staged_task_artifacts.is_empty() && current_active_todo_item(todos).is_some();
    let structured_document_summary_mode = structured_document_summary_active(
        latest_user_text.as_deref(),
        todos,
        &effective_requested_targets,
        &execution_focus_targets,
    );
    let structured_document_summary_state = structured_document_summary_mode
        .then(|| structured_document_summary_snapshot(transcript, latest_user_text.as_deref()))
        .flatten();
    let structured_document_summary_conversion_only_mode = structured_document_summary_state
        .as_ref()
        .and_then(|snapshot| {
            snapshot.current_batch_expected.map(|expected| {
                snapshot.current_batch_processed < expected
                    && !snapshot.missing_files.is_empty()
                    && !snapshot.processed_files.is_empty()
            })
        })
        .unwrap_or(false);
    let structured_document_summary_write_due = structured_document_summary_state
        .as_ref()
        .and_then(|snapshot| {
            snapshot
                .current_batch_expected
                .map(|expected| snapshot.current_batch_processed >= expected)
        })
        .unwrap_or(false);
    let follow_up_implementation =
        follow_up_boundary && matches!(focus, FollowUpFocus::Implementation | FollowUpFocus::Mixed);
    let (readonly_stall, readonly_stall_targets) = recent_tool_call_stalled_with_config(
        transcript,
        start_index,
        follow_up_implementation,
        agent_config,
    );
    let code_block_stall = recent_assistant_code_block_stall(transcript, start_index);
    let pseudo_tool_call_stall = recent_assistant_pseudo_tool_call_stall(transcript, start_index);
    let invalid_tool_stall = recent_invalid_tool_result_stall(transcript, start_index);
    let verification_pending_error_stall = false;
    let no_tool_authoring_error_stall = false;
    let staged_task_recovery_stall =
        recent_nonprogress_recovery_result_stall_with_config(transcript, start_index, agent_config);
    let patch_recovery_targets = recent_patch_repair_targets(transcript, start_index);
    let interrupted_resume = false;
    let last_failure = None;
    let documentation_scope_targets = {
        let targets =
            documentation_scope_targets(&focus_seed_targets, transcript, start_index, focus);
        if targets.is_empty()
            && (documentation_scope_explicit || implementation_follows_prior_documentation)
        {
            let implicit_targets = if documentation_scope_explicit {
                &implicit_documentation_targets
            } else {
                &implicit_prior_design_targets
            };
            implicit_targets
                .iter()
                .filter(|target| {
                    classify_artifact_target(target) == ArtifactTargetKind::Documentation
                })
                .cloned()
                .collect()
        } else {
            targets
        }
    };
    let completion_closeout_ready = false;
    let verification_failure_labels = recent_verification_failures(transcript, start_index);
    let inactive_target_edit_recovery_targets =
        latest_inactive_target_edit_rejection(&transcript.messages[start_index..])
            .and_then(|_| state)
            .map(|state| {
                state
                    .active_targets
                    .iter()
                    .map(|target| target.as_str().to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
    let inactive_target_edit_recovery_mode = !inactive_target_edit_recovery_targets.is_empty();
    let verification_repair_cycle = latest_verification_repair_cycle(transcript);
    let verification_repair_read_budget_exhausted =
        latest_verification_repair_focus_required_result(transcript, start_index);
    let staged_task_output_targets_changed = staged_task_output_targets_changed_after_latest_user(
        transcript,
        start_index,
        &staged_task_output_targets,
    );
    let staged_task_closeout_mode = !completion_closeout_ready
        && staged_task_documentation_closeout_mode(
            todos,
            staged_task_execution_active || !staged_task_artifacts.is_empty(),
            &staged_task_output_targets,
            staged_task_output_targets_changed,
        );
    let staged_task_closeout_read_complete = staged_task_closeout_mode
        && staged_task_output_targets_read_after_latest_user(
            transcript,
            start_index,
            &staged_task_output_targets,
        );
    let staged_task_closeout_recovery_mode = staged_task_closeout_mode
        && !staged_task_closeout_read_complete
        && (no_tool_authoring_error_stall || pseudo_tool_call_stall || invalid_tool_stall);
    let staged_task_closeout_repair_targets = staged_task_closeout_mode
        .then(|| latest_denied_edit_targets_after_latest_user(transcript, start_index))
        .unwrap_or_default();
    let staged_task_closeout_repair_mode =
        staged_task_closeout_mode && !staged_task_closeout_repair_targets.is_empty();
    let staged_task_docs_only = (staged_task_execution_active || !staged_task_artifacts.is_empty())
        && staged_task_documentation_outputs_only(&staged_task_output_targets);
    let staged_task_documentation_authoring_mode = staged_task_execution_active
        && !structured_document_summary_mode
        && !staged_task_closeout_mode
        && !staged_task_documentation_focus_targets.is_empty()
        && staged_task_documentation_authoring_active(
            &staged_task_output_targets,
            &execution_focus_targets,
        );
    let staged_task_documentation_audit_state = (!staged_task_documentation_focus_targets
        .is_empty())
    .then(|| {
        latest_staged_task_documentation_audit_state(
            transcript,
            start_index,
            &staged_task_documentation_focus_targets,
        )
    })
    .flatten();
    let staged_task_documentation_evidence_snapshot = staged_task_documentation_authoring_mode
        .then(|| {
            staged_task_documentation_evidence_snapshot(
                transcript,
                start_index,
                &staged_task_documentation_focus_targets,
            )
        })
        .flatten();
    let staged_task_documentation_audit_feedback = staged_task_documentation_audit_state
        .as_ref()
        .map(|state| state.feedback.clone());
    let staged_task_documentation_audit_repair_mode = (staged_task_documentation_authoring_mode
        || staged_task_closeout_mode)
        && !completion_closeout_ready
        && staged_task_documentation_audit_state.is_some();
    let staged_task_documentation_audit_escalation_mode =
        staged_task_documentation_audit_repair_mode
            && (staged_task_documentation_audit_state
                .as_ref()
                .map(|state| state.escalated(agent_config))
                .unwrap_or(false)
                || readonly_stall
                || no_tool_authoring_error_stall
                || pseudo_tool_call_stall
                || invalid_tool_stall
                || staged_task_recovery_stall);
    let staged_task_documentation_authoring_focus_mode = staged_task_documentation_authoring_mode
        && !staged_task_documentation_audit_repair_mode
        && (readonly_stall
            || no_tool_authoring_error_stall
            || pseudo_tool_call_stall
            || invalid_tool_stall
            || staged_task_recovery_stall)
        && staged_task_documentation_evidence_snapshot.is_some();
    let staged_task_documentation_authoring_no_replan_mode =
        staged_task_documentation_authoring_focus_mode
            && !staged_task_documentation_focus_targets.is_empty()
            && !staged_task_output_targets_changed_after_latest_user(
                transcript,
                start_index,
                &staged_task_documentation_focus_targets,
            );
    let verification_requirements = verification_requirements(latest_user_text.as_deref(), todos);
    let verification_freshness_targets =
        verification_freshness_targets_after_latest_user(transcript, start_index, todos);
    let verification_evidence = verification_evidence_after_latest_user_with_freshness(
        transcript,
        start_index,
        &verification_freshness_targets,
    );
    let verification_requirements_satisfied =
        verification_requirements.is_satisfied_by(verification_evidence);
    let verification_pending_without_open_work =
        verification_requirements.is_required() && !verification_requirements_satisfied;
    let verification_focus_active = verification_pending_without_open_work
        || state.is_some_and(|state| {
            state.completion.verification_pending
                || matches!(
                    state.failure.as_ref().map(|failure| failure.kind),
                    Some(FailureKind::VerificationFailed)
                )
        });
    let stale_authoring_recovery_during_verification =
        verification_focus_active && no_tool_authoring_error_stall;
    let patch_recovery_mode = !completion_closeout_ready
        && !staged_task_closeout_mode
        && !patch_recovery_targets.is_empty();
    let follow_up_implementation_recovery = follow_up_implementation
        && state
            .map(|state| state.completion.open_work_count == 0)
            .unwrap_or(true);
    let edit_recovery_mode = !completion_closeout_ready
        && !staged_task_closeout_mode
        && !patch_recovery_mode
        && !structured_document_summary_mode
        && !verification_focus_active
        && (state
            .map(|state| state.completion.open_work_count > 0)
            .unwrap_or(false)
            || follow_up_implementation_recovery)
        && (readonly_stall || no_tool_authoring_error_stall || pseudo_tool_call_stall)
        && !staged_task_docs_only;
    let verification_repair_target_rotation_target =
        latest_verification_repair_target_rotation_required_target(transcript, start_index);
    let verification_repair_target_rotation_active =
        verification_repair_target_rotation_target.is_some();
    let repair_recorded_with_active_verification = verification_repair_cycle
        .as_ref()
        .is_some_and(|cycle| cycle.repair_recorded)
        && (verification_focus_active
            || (state
                .map(|state| state.completion.open_work_count == 0)
                .unwrap_or(true)
                && (verification_pending_without_open_work
                    || !verification_failure_labels.is_empty())));
    let verification_repair_rerun_due = (verification_focus_active
        || repair_recorded_with_active_verification)
        && !completion_closeout_ready
        && !staged_task_closeout_mode
        && !patch_recovery_mode
        && verification_repair_target_rotation_target.is_none()
        && repair_recorded_with_active_verification;
    let verification_failure_repair_mode = (verification_focus_active
        || verification_repair_target_rotation_active)
        && !completion_closeout_ready
        && !staged_task_closeout_mode
        && !patch_recovery_mode
        && !verification_repair_rerun_due
        && !verification_failure_labels.is_empty();
    let verification_repair_rotated_focus_target = verification_repair_target_rotation_target
        .clone()
        .or_else(|| {
            verification_failure_repair_mode
                .then(|| verification_repair_rotated_focus_target(transcript, start_index, state))
                .flatten()
        });
    let verification_repair_import_focus_target = verification_failure_repair_mode
        .then(|| state.and_then(verification_repair_import_export_focus_target))
        .flatten();
    let verification_repair_feedback_focus_target =
        latest_verification_repair_focus_required_target(transcript, start_index);
    let verification_failure_repair_edit_focused_mode = verification_failure_repair_mode
        && (no_tool_authoring_error_stall
            || verification_repair_rotated_focus_target.is_some()
            || verification_repair_import_focus_target.is_some()
            || verification_repair_feedback_focus_target.is_some()
            || verification_repair_cycle.as_ref().is_some_and(|cycle| {
                !cycle.repair_recorded
                    && cycle.post_failure_read_attempt_count
                        >= agent_config.verification_failure_repair_read_budget
            }));
    let verification_repair_focus_target = verification_failure_repair_edit_focused_mode
        .then(|| {
            verification_repair_rotated_focus_target
                .clone()
                .or_else(|| verification_repair_import_focus_target.clone())
                .or_else(|| verification_repair_feedback_focus_target.clone())
        })
        .flatten();
    let verification_repair_active_targets = latest_verification_failure_context(transcript)
        .map(|failure| failure.targets)
        .unwrap_or_default();
    let verification_repair_contract_refs = requested_contract
        .reference_inputs
        .iter()
        .chain(protected_targets.iter())
        .map(Utf8PathBuf::from)
        .collect::<Vec<_>>();
    let verification_repair_next_read_target = verification_repair_next_read_target(
        verification_failure_repair_mode,
        verification_repair_rerun_due,
        verification_repair_focus_target.as_deref(),
        verification_repair_cycle.as_ref(),
        &verification_repair_active_targets,
        &verification_repair_contract_refs,
        agent_config.verification_failure_repair_read_budget,
        verification_repair_focus_target.is_some(),
    );
    let verification_repair_focus_target_read_after_failure = verification_repair_focus_target
        .as_ref()
        .is_some_and(|target| {
            verification_repair_cycle.as_ref().is_some_and(|cycle| {
                cycle.post_failure_read_targets.iter().any(|read_target| {
                    prompt_target_matches_required_output(
                        read_target.as_str(),
                        std::slice::from_ref(target),
                    )
                })
            })
        });
    let verification_recovery_mode = verification_focus_active
        && !completion_closeout_ready
        && !staged_task_closeout_mode
        && !patch_recovery_mode
        && !verification_failure_repair_mode
        && (verification_repair_rerun_due
            || code_block_stall
            || pseudo_tool_call_stall
            || invalid_tool_stall
            || verification_pending_error_stall
            || stale_authoring_recovery_during_verification);
    PromptSignals {
        interrupted_resume,
        compaction_replay: latest_summary_index(transcript).is_some(),
        follow_up_boundary,
        follow_up_focus: focus,
        follow_up_implementation,
        documentation_scope_explicit,
        active_follow_up_request: follow_up_boundary
            .then_some(())
            .and(latest_user_text.clone()),
        requested_artifact_targets: effective_requested_targets.clone(),
        requested_verification_commands: requested_contract.verification_commands.clone(),
        structured_document_summary_mode,
        structured_document_summary_conversion_only_mode,
        structured_document_summary_write_due,
        staged_task_execution_active,
        staged_task_artifacts,
        staged_task_output_targets,
        staged_task_verification_commands,
        execution_focus_targets,
        staged_task_documentation_authoring_mode,
        staged_task_documentation_authoring_focus_mode,
        staged_task_documentation_authoring_no_replan_mode,
        staged_task_documentation_evidence_snapshot,
        staged_task_documentation_audit_feedback,
        staged_task_documentation_audit_repair_mode,
        staged_task_documentation_audit_escalation_mode,
        last_failure,
        documentation_scope_targets,
        completion_closeout_ready,
        readonly_stall,
        readonly_stall_targets,
        code_block_stall,
        pseudo_tool_call_stall,
        no_tool_authoring_error_stall,
        inactive_target_edit_recovery_mode,
        inactive_target_edit_recovery_targets,
        inactive_target_edit_recovery_read_target: None,
        edit_recovery_mode,
        patch_recovery_mode,
        patch_recovery_targets,
        verification_failure_repair_mode,
        verification_repair_rerun_due,
        verification_failure_repair_edit_focused_mode,
        verification_repair_read_budget_exhausted,
        verification_repair_next_read_target,
        verification_repair_focus_target,
        verification_repair_focus_target_read_after_failure,
        verification_failure_labels,
        staged_task_closeout_mode,
        staged_task_closeout_read_complete,
        staged_task_closeout_recovery_mode,
        staged_task_closeout_repair_mode,
        staged_task_closeout_repair_targets,
        verification_recovery_mode,
    }
}

fn apply_candidate_tool_availability_for_prompt_state(
    tools: &mut Vec<ToolSchema>,
    signals: &PromptSignals,
    state: &SessionStateSnapshot,
) {
    let _ = signals;
    if signals.completion_closeout_ready {
        tools.clear();
        return;
    }
    if matches!(state.route, TaskRoute::Summary) {
        tools.clear();
        return;
    }
    if matches!(state.route, TaskRoute::Review) {
        tools.retain(|tool| {
            matches!(
                tool.name.as_str(),
                "read"
                    | "list"
                    | "glob"
                    | "grep"
                    | "inspect_directory"
                    | "shell"
                    | "docling_convert"
                    | "mcp_call"
            )
        });
        return;
    }
    if matches!(state.route, TaskRoute::Ask) {
        tools.retain(|tool| {
            matches!(
                tool.name.as_str(),
                "read"
                    | "list"
                    | "glob"
                    | "grep"
                    | "inspect_directory"
                    | "docling_convert"
                    | "mcp_call"
            )
        });
    }
}

fn exact_active_authoring_write_required(state: &SessionStateSnapshot) -> Option<String> {
    if state.completion.verification_pending
        || state.completion.closeout_ready
        || !matches!(
            state.process_phase,
            ProcessPhase::Author | ProcessPhase::Repair
        )
        || state.active_targets.len() != 1
    {
        return None;
    }
    let target = state.active_targets[0].as_str();
    if !prompt_target_is_test_like(target) {
        return None;
    }
    Some(target.to_string())
}

fn prompt_target_is_test_like(target: &str) -> bool {
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    name.starts_with("test_") || name.ends_with("_test.py")
}

fn restore_skill_tool(
    tools: &mut Vec<ToolSchema>,
    skill_tool: Option<&ToolSchema>,
    state: &SessionStateSnapshot,
) {
    if matches!(state.route, TaskRoute::Summary) || tools.is_empty() {
        return;
    }
    if matches!(
        state.failure.as_ref().map(|failure| failure.kind),
        Some(crate::session::FailureKind::VerificationFailed)
    ) && matches!(
        state.process_phase,
        ProcessPhase::Verify | ProcessPhase::Repair
    ) {
        return;
    }
    if tools.iter().all(|tool| {
        matches!(
            tool.name.as_str(),
            "read" | "docling_convert" | "todowrite" | "write" | "apply_patch" | "shell"
        )
    }) {
        return;
    }
    let Some(skill_tool) = skill_tool else {
        return;
    };
    if tools.iter().any(|tool| tool.name == "skill") {
        return;
    }
    tools.push(skill_tool.clone());
    tools.sort_by(|left, right| left.name.cmp(&right.name));
}

fn implementation_spec_targets(signals: &PromptSignals) -> Vec<String> {
    if !signals.documentation_scope_targets.is_empty() {
        return signals.documentation_scope_targets.clone();
    }
    dedupe_targets(
        signals
            .requested_artifact_targets
            .iter()
            .filter(|target| classify_artifact_target(target) == ArtifactTargetKind::Documentation)
            .cloned()
            .collect(),
    )
}

fn route_scope_targets(state: &SessionStateSnapshot, signals: &PromptSignals) -> Vec<String> {
    if !signals.execution_focus_targets.is_empty() {
        return signals.execution_focus_targets.clone();
    }
    let docs_targets = docs_route_pending_repair_targets(state.docs_route.as_ref());
    if !docs_targets.is_empty() {
        return docs_targets
            .into_iter()
            .map(|target| target.as_str().to_string())
            .collect();
    }
    if !signals.documentation_scope_targets.is_empty() {
        return signals.documentation_scope_targets.clone();
    }
    if !signals.requested_artifact_targets.is_empty() {
        return signals.requested_artifact_targets.clone();
    }
    state
        .active_targets
        .iter()
        .map(|value| value.as_str().to_string())
        .collect()
}

pub(crate) fn docs_route_contract_repair_hint(state: &SessionStateSnapshot) -> Option<String> {
    let docs = state.docs_route.as_ref()?;

    if let Some(coverage) = docs
        .area_coverage
        .iter()
        .find(|coverage| coverage.status == crate::session::ContractStatus::Pending)
    {
        let example = docs_area_example_path(docs, coverage.area);
        return Some(format!(
            "For the docs survey, inspect one concrete {} path such as `{}` with `inspect_directory`, `list`, or `read` before closing out.",
            docs_area_label(coverage.area),
            example
        ));
    }

    if docs.pending_deliverables.len() > 1 {
        let pending = docs
            .pending_deliverables
            .iter()
            .take(5)
            .map(|item| {
                if item.summary.trim().is_empty() {
                    format!("`{}`", item.target.as_str())
                } else {
                    format!("`{}`: {}", item.target.as_str(), item.summary)
                }
            })
            .collect::<Vec<_>>()
            .join(" / ");
        return Some(format!(
            "Pending docs deliverables are {pending}. Pick one pending deliverable for each `write`, add the listed concrete evidence, and move to another pending deliverable instead of repeating an identical rewrite."
        ));
    }

    if let Some((deliverable, grounding)) = docs.deliverables.iter().find_map(|deliverable| {
        deliverable
            .grounding
            .iter()
            .find(|grounding| grounding.status == crate::session::ContractStatus::Pending)
            .map(|grounding| (deliverable, grounding))
    }) {
        let label = docs_grounding_requirement_label(grounding.requirement);
        let example = grounding
            .representative_path
            .as_ref()
            .map(|path| path.as_str().replace('\\', "/"))
            .unwrap_or_else(|| label.to_string());
        return Some(docs_deliverable_write_contract_hint(
            deliverable,
            &format!(
                "Literally cite the concrete {} path `{}` in the draft before closing out.",
                label, example
            ),
        ));
    }

    if let Some(deliverable) = docs
        .deliverables
        .iter()
        .find(|deliverable| !docs_route_missing_areas(deliverable).is_empty())
    {
        let missing = docs_route_missing_areas(deliverable);
        let hints = missing
            .into_iter()
            .map(|area| {
                let example = docs_area_example_path(docs, area);
                docs_missing_area_repair_hint(deliverable, area, &example)
            })
            .collect::<Vec<_>>();
        if !hints.is_empty() {
            return Some(hints.join(" "));
        }
    }

    if let Some((deliverable, topic)) = docs.deliverables.iter().find_map(|deliverable| {
        docs_route_missing_topics(deliverable)
            .into_iter()
            .next()
            .map(|topic| (deliverable, topic))
    }) {
        return Some(docs_topic_repair_hint(deliverable, &topic));
    }

    docs.factual_checks
        .iter()
        .find(|check| check.status == crate::session::ContractStatus::Pending)
        .map(|check| {
            format!(
                "Inspect or cite the exact pending fact path `{}` before closing out.",
                check.subject.replace('\\', "/")
            )
        })
}

fn docs_route_should_suppress_authoring_focus_mode(state: &SessionStateSnapshot) -> bool {
    if !state.completion.route_contract_pending {
        return false;
    }
    let Some(docs) = state.docs_route.as_ref() else {
        return false;
    };
    if docs.deliverables.is_empty() {
        return true;
    }
    docs.area_coverage
        .iter()
        .any(|coverage| coverage.status == crate::session::ContractStatus::Pending)
        || docs
            .factual_checks
            .iter()
            .any(|check| check.status == crate::session::ContractStatus::Pending)
}

fn docs_grounding_requirement_label(
    requirement: crate::session::DocsGroundingRequirement,
) -> &'static str {
    match requirement {
        crate::session::DocsGroundingRequirement::BackendMetadata => "backend project metadata",
        crate::session::DocsGroundingRequirement::BackendSource => "backend source entry/config",
        crate::session::DocsGroundingRequirement::BackendRoute => "backend route source",
        crate::session::DocsGroundingRequirement::FrontendMetadata => "frontend package metadata",
        crate::session::DocsGroundingRequirement::FrontendSource => {
            "frontend route/component source"
        }
        crate::session::DocsGroundingRequirement::Examples => "examples sample",
        crate::session::DocsGroundingRequirement::Tests => "test file",
        crate::session::DocsGroundingRequirement::Data => "data artifact",
    }
}

fn docs_route_missing_areas(
    deliverable: &crate::session::DocsDeliverableCoverage,
) -> Vec<crate::session::DocsArea> {
    let present = deliverable
        .representative_paths
        .iter()
        .map(|path| path.as_str().replace('\\', "/"))
        .chain(
            deliverable
                .grounding
                .iter()
                .filter(|grounding| grounding.status == crate::session::ContractStatus::Satisfied)
                .filter_map(|grounding| {
                    grounding
                        .representative_path
                        .as_ref()
                        .map(|path| path.as_str().replace('\\', "/"))
                }),
        )
        .collect::<std::collections::BTreeSet<_>>();
    deliverable
        .required_areas
        .iter()
        .copied()
        .filter(|area| {
            !present
                .iter()
                .any(|path| prompt_path_matches_docs_area(path, *area))
        })
        .collect()
}

fn docs_route_missing_topics(deliverable: &crate::session::DocsDeliverableCoverage) -> Vec<String> {
    deliverable
        .required_topics
        .iter()
        .filter(|topic| {
            !deliverable
                .satisfied_topics
                .iter()
                .any(|entry| entry == *topic)
        })
        .cloned()
        .collect()
}

fn docs_area_example_path(
    docs: &crate::session::DocsRouteState,
    area: crate::session::DocsArea,
) -> String {
    docs.area_coverage
        .iter()
        .find(|coverage| coverage.area == area)
        .and_then(|coverage| {
            coverage
                .representative_paths
                .iter()
                .max_by_key(|path| path.as_str().len())
        })
        .map(|path| path.as_str().replace('\\', "/"))
        .unwrap_or_else(|| docs_area_fallback_path(area).to_string())
}

fn docs_missing_area_repair_hint(
    deliverable: &crate::session::DocsDeliverableCoverage,
    area: crate::session::DocsArea,
    example: &str,
) -> String {
    match (deliverable.kind, area) {
        (crate::session::DocsDeliverableKind::DetailDesign, crate::session::DocsArea::Data) => {
            format!(
                "For `{}`, rewrite the data section so it literally cites a concrete runtime data path such as `{}` and explains what is stored there; do not stop at abstract config keys like `run_artifact_root` or `document_storage_path`.",
                deliverable.target.as_str(),
                example
            )
        }
        _ => format!(
            "For `{}`, add one concrete {} path such as `{}`.",
            deliverable.target.as_str(),
            docs_area_label(area),
            example
        ),
    }
}

fn docs_deliverable_write_contract_hint(
    deliverable: &crate::session::DocsDeliverableCoverage,
    guidance: &str,
) -> String {
    format!(
        "For `{}`, do not narrate the next draft. Use `write` now with one JSON object that includes both `path` and `content`, for example `{{\"path\":\"{}\",\"content\":\"...\"}}`. {}",
        deliverable.target.as_str(),
        deliverable.target.as_str(),
        guidance
    )
}

fn docs_topic_repair_hint(
    deliverable: &crate::session::DocsDeliverableCoverage,
    topic: &str,
) -> String {
    match (deliverable.kind, topic) {
        (crate::session::DocsDeliverableKind::Readme, "repo overview") => {
            docs_deliverable_write_contract_hint(
                deliverable,
                "Add a short repository overview near the top using wording such as `概要` or `全体像` so the project purpose is explicit.",
            )
        }
        (crate::session::DocsDeliverableKind::Readme, "main directories") => {
            docs_deliverable_write_contract_hint(
                deliverable,
                "Enumerate the main repository directories with concrete paths such as `backend/`, `frontend/`, `examples/`, or `data/`.",
            )
        }
        (crate::session::DocsDeliverableKind::Readme, "run/test entrypoints") => {
            let manifest_paths = deliverable
                .grounding
                .iter()
                .filter(|grounding| {
                    grounding.status == crate::session::ContractStatus::Satisfied
                        && matches!(
                            grounding.requirement,
                            crate::session::DocsGroundingRequirement::BackendMetadata
                                | crate::session::DocsGroundingRequirement::FrontendMetadata
                        )
                })
                .filter_map(|grounding| grounding.representative_path.as_ref())
                .map(|path| format!("`{}`", path.as_str().replace('\\', "/")))
                .collect::<Vec<_>>();
            if manifest_paths.is_empty() {
                format!(
                    "{}",
                    docs_deliverable_write_contract_hint(
                        deliverable,
                        "Inspect the confirmed manifest or entrypoint paths and replace unresolved run/test sections with concrete commands instead of leaving them as `不明`.",
                    )
                )
            } else {
                docs_deliverable_write_contract_hint(
                    deliverable,
                    &format!(
                        "Inspect {} and replace unresolved run/test sections with concrete commands instead of leaving them as `不明`.",
                        manifest_paths.join(" and ")
                    ),
                )
            }
        }
        _ => docs_deliverable_write_contract_hint(
            deliverable,
            &format!(
                "Cover the missing topic `{}` explicitly before closing out.",
                topic
            ),
        ),
    }
}

fn prompt_path_matches_docs_area(path: &str, area: crate::session::DocsArea) -> bool {
    let normalized = path.replace('\\', "/");
    match area {
        crate::session::DocsArea::Backend => {
            normalized.starts_with("backend/")
                && !normalized.starts_with("backend/tests/")
                && !normalized.starts_with("backend/data/")
        }
        crate::session::DocsArea::Frontend => {
            normalized.starts_with("frontend/")
                && !normalized.starts_with("frontend/tests/")
                && !normalized.starts_with("frontend/data/")
        }
        crate::session::DocsArea::Tests => {
            normalized.starts_with("tests/")
                || normalized.starts_with("backend/tests/")
                || normalized.starts_with("frontend/tests/")
        }
        crate::session::DocsArea::Data => {
            normalized.starts_with("data/")
                || normalized.starts_with("backend/data/")
                || normalized.starts_with("frontend/data/")
        }
        crate::session::DocsArea::Examples => normalized.starts_with("examples/"),
    }
}

fn tool_call_arguments_are_replayable(arguments_json: &str) -> bool {
    matches!(
        serde_json::from_str::<Value>(arguments_json),
        Ok(Value::Object(_))
    )
}

fn docs_area_label(area: crate::session::DocsArea) -> &'static str {
    match area {
        crate::session::DocsArea::Backend => "backend",
        crate::session::DocsArea::Frontend => "frontend",
        crate::session::DocsArea::Tests => "tests",
        crate::session::DocsArea::Data => "data",
        crate::session::DocsArea::Examples => "examples",
    }
}

fn docs_area_fallback_path(area: crate::session::DocsArea) -> &'static str {
    match area {
        crate::session::DocsArea::Backend => "backend/app/core/config.py",
        crate::session::DocsArea::Frontend => "frontend/app/(workspace)/scenarios/page.tsx",
        crate::session::DocsArea::Tests => "backend/tests/integration/test_health_api.py",
        crate::session::DocsArea::Data => {
            "data/runs/2025-04-05T14-30-00Z-ripplefish-simulation-001/log.txt"
        }
        crate::session::DocsArea::Examples => "examples/phase2-sample-bundle.json",
    }
}

fn latest_user_index(transcript: &Transcript, start_index: usize) -> Option<usize> {
    transcript.messages[start_index..]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(offset, message)| {
            matches!(message.record.role, MessageRole::User).then_some(start_index + offset)
        })
}

fn latest_user_text(transcript: &Transcript, start_index: usize) -> Option<String> {
    let latest_user = latest_user_index(transcript, start_index)?;
    let text = transcript.messages[latest_user]
        .parts
        .iter()
        .filter_map(|part| match &part.payload {
            MessagePart::Text(value) => Some(value.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if text.is_empty() { None } else { Some(text) }
}

fn has_historical_turns_before_latest_user(transcript: &Transcript, start_index: usize) -> bool {
    let Some(latest_user) = latest_user_index(transcript, start_index) else {
        return false;
    };
    let history_start = latest_summary_before_user_index(transcript, latest_user)
        .map(|index| index + 1)
        .unwrap_or(0);
    transcript.messages[history_start..latest_user]
        .iter()
        .any(|message| {
            matches!(
                message.record.role,
                MessageRole::User | MessageRole::Assistant
            )
        })
}

fn recent_tool_call_stalled_with_config(
    transcript: &Transcript,
    start_index: usize,
    follow_up_implementation: bool,
    agent_config: &AgentConfig,
) -> (bool, Vec<String>) {
    let recent_calls = transcript.messages[start_index..]
        .iter()
        .rev()
        .flat_map(|message| message.parts.iter().rev())
        .filter_map(|part| match &part.payload {
            MessagePart::ToolCall(value) => Some((
                value.tool_name.to_string(),
                extract_readonly_target(&value.tool_name.to_string(), &value.arguments_json),
            )),
            MessagePart::DiffSummary(_) => Some(("__write__".to_string(), None)),
            _ => None,
        })
        .take(RECENT_TOOL_CALL_WINDOW)
        .collect::<Vec<_>>();

    let threshold = if follow_up_implementation {
        agent_config.readonly_stall_threshold_implementation
    } else {
        agent_config.readonly_stall_threshold_general
    };
    let readonly_only = recent_calls.iter().all(|entry| match entry {
        (name, _) => matches!(name.as_str(), "read" | "list" | "glob" | "grep"),
        #[allow(unreachable_patterns)]
        _ => false,
    });
    let targets = recent_calls
        .iter()
        .filter_map(|(_, target)| target.clone())
        .collect::<Vec<_>>();

    (
        recent_calls.len() >= threshold && readonly_only,
        dedupe_targets(targets),
    )
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct FollowUpActivity {
    documentation_reads: usize,
    implementation_reads: usize,
    documentation_writes: usize,
    implementation_writes: usize,
}

fn observe_follow_up_activity(transcript: &Transcript, start_index: usize) -> FollowUpActivity {
    let Some(latest_user) = latest_user_index(transcript, start_index) else {
        return FollowUpActivity::default();
    };

    let mut activity = FollowUpActivity::default();

    for message in &transcript.messages[latest_user + 1..] {
        for part in &message.parts {
            if let MessagePart::ToolCall(call) = &part.payload {
                let is_write = is_write_tool_name(&call.tool_name.to_string());
                for target in artifact_targets_from_tool_call(
                    &call.tool_name.to_string(),
                    &call.arguments_json,
                ) {
                    match classify_artifact_target(&target) {
                        ArtifactTargetKind::Documentation => {
                            if is_write {
                                activity.documentation_writes += 1;
                            } else {
                                activity.documentation_reads += 1;
                            }
                        }
                        ArtifactTargetKind::Implementation => {
                            if is_write {
                                activity.implementation_writes += 1;
                            } else {
                                activity.implementation_reads += 1;
                            }
                        }
                        ArtifactTargetKind::Unknown => {}
                    }
                }
            }
        }
    }

    activity
}

fn resolve_follow_up_focus(
    requested_focus: FollowUpFocus,
    activity: &FollowUpActivity,
) -> FollowUpFocus {
    if activity.implementation_writes > 0 && activity.documentation_writes > 0 {
        return FollowUpFocus::Mixed;
    }
    if activity.implementation_writes > 0 {
        return FollowUpFocus::Implementation;
    }
    if activity.documentation_writes > 0 {
        return FollowUpFocus::Documentation;
    }
    match requested_focus {
        FollowUpFocus::Documentation => return FollowUpFocus::Documentation,
        FollowUpFocus::Implementation => return FollowUpFocus::Implementation,
        FollowUpFocus::Mixed => return FollowUpFocus::Mixed,
        FollowUpFocus::Unknown => {}
    }
    if activity.implementation_reads >= IMPLEMENTATION_READS_ASSUME_IMPLEMENTATION_FOCUS {
        if activity.documentation_reads > 0 {
            return FollowUpFocus::Mixed;
        }
        return FollowUpFocus::Implementation;
    }
    if activity.documentation_reads > 0 {
        return FollowUpFocus::Documentation;
    }

    FollowUpFocus::Unknown
}

fn latest_user_editor_context_targets(transcript: &Transcript, start_index: usize) -> Vec<String> {
    let Some(latest_user) = latest_user_index(transcript, start_index) else {
        return Vec::new();
    };
    let Some(crate::session::MessageMetadata::User(meta)) = transcript
        .messages
        .get(latest_user)
        .map(|message| &message.record.metadata)
    else {
        return Vec::new();
    };
    let Some(editor_context) = meta.editor_context.as_ref() else {
        return Vec::new();
    };

    dedupe_targets(
        editor_context
            .active_file
            .iter()
            .map(|path| path.as_str().to_string())
            .chain(
                editor_context
                    .visible_files
                    .iter()
                    .map(|path| path.as_str().to_string()),
            )
            .chain(
                editor_context
                    .open_tabs
                    .iter()
                    .map(|path| path.as_str().to_string()),
            )
            .map(|target| normalize_prompt_target(&target))
            .filter(|target| classify_artifact_target(target) != ArtifactTargetKind::Unknown)
            .collect(),
    )
}

fn recent_documentation_targets_before_latest_user(
    transcript: &Transcript,
    start_index: usize,
) -> Vec<String> {
    let Some(latest_user) = latest_user_index(transcript, start_index) else {
        return Vec::new();
    };

    for message in transcript.messages[..latest_user].iter().rev() {
        let targets = dedupe_targets(
            message
                .parts
                .iter()
                .rev()
                .flat_map(|part| artifact_targets_from_part(&part.payload))
                .filter(|target| {
                    classify_artifact_target(target) == ArtifactTargetKind::Documentation
                })
                .collect(),
        );
        if !targets.is_empty() {
            return targets;
        }
    }

    Vec::new()
}

fn documentation_scope_targets(
    requested_targets: &[String],
    transcript: &Transcript,
    start_index: usize,
    focus: FollowUpFocus,
) -> Vec<String> {
    let Some(latest_user) = latest_user_index(transcript, start_index) else {
        return Vec::new();
    };
    let observed_targets = dedupe_targets(
        transcript.messages[latest_user + 1..]
            .iter()
            .flat_map(|message| message.parts.iter())
            .flat_map(|part| artifact_targets_from_part(&part.payload))
            .filter(|target| classify_artifact_target(target) == ArtifactTargetKind::Documentation)
            .collect(),
    );
    let requested_documentation_targets = requested_targets
        .iter()
        .filter(|target| classify_artifact_target(target) == ArtifactTargetKind::Documentation)
        .cloned()
        .collect::<Vec<_>>();
    if matches!(focus, FollowUpFocus::Documentation) {
        return dedupe_targets(
            requested_documentation_targets
                .into_iter()
                .chain(observed_targets)
                .collect(),
        );
    }
    observed_targets
}

pub(crate) fn focus_from_targets(targets: &[String]) -> FollowUpFocus {
    let mut has_documentation = false;
    let mut has_implementation = false;
    for target in targets {
        match classify_artifact_target(target) {
            ArtifactTargetKind::Documentation => has_documentation = true,
            ArtifactTargetKind::Implementation => has_implementation = true,
            ArtifactTargetKind::Unknown => {}
        }
    }
    match (has_documentation, has_implementation) {
        (true, true) => FollowUpFocus::Mixed,
        (true, false) => FollowUpFocus::Documentation,
        (false, true) => FollowUpFocus::Implementation,
        (false, false) => FollowUpFocus::Unknown,
    }
}

pub(crate) fn documentation_scope_explicit_for_requested_focus(
    requested_focus: FollowUpFocus,
    latest_user_text: Option<&str>,
) -> bool {
    let documentation_only_requested =
        latest_user_text.is_some_and(documentation_only_follow_up_requested);
    matches!(requested_focus, FollowUpFocus::Documentation)
        || (documentation_only_requested
            && !matches!(
                requested_focus,
                FollowUpFocus::Implementation | FollowUpFocus::Mixed
            ))
}

pub(crate) fn extract_requested_artifact_targets(text: &str) -> Vec<String> {
    artifact_targets_from_instruction_text(text, false)
}

pub(crate) fn staged_task_artifact_targets_from_text(text: &str) -> Vec<String> {
    dedupe_targets(
        explicit_artifact_targets_in_text(text)
            .into_iter()
            .filter(|target| is_staged_task_artifact_target(target))
            .collect(),
    )
}

pub(crate) fn requested_work_contract_from_instruction_text(text: &str) -> RequestedWorkContract {
    let mut contract = RequestedWorkContract::default();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let (line_without_code, code_spans) = split_backtick_spans(line);
        let line_intent = classify_requested_line_intent(line, &line_without_code, &code_spans);
        contract
            .verification_commands
            .extend(explicit_verification_commands_from_text(line));

        for code_span in code_spans {
            let trimmed = code_span.trim();
            if trimmed.is_empty() {
                continue;
            }
            if instruction_contains_verification_command(trimmed) {
                contract
                    .verification_commands
                    .extend(explicit_verification_commands_from_text(trimmed));
                continue;
            }
            if looks_like_naming_pattern(trimmed) {
                contract.naming_patterns.push(trimmed.to_string());
                continue;
            }
            if line_target_is_reference_input(line, trimmed) {
                contract.reference_inputs.push(trimmed.to_string());
                continue;
            }
            if let Some(target) = normalize_artifact_token(trimmed) {
                record_requested_target(&mut contract, &target, line_intent);
            }
        }

        for token in line_without_code
            .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | '、' | '，'))
            .filter_map(normalize_artifact_token)
        {
            if looks_like_naming_pattern(&token) {
                contract.naming_patterns.push(token);
                continue;
            }
            if line_target_is_reference_input(line, &token) {
                contract.reference_inputs.push(token);
                continue;
            }
            record_requested_target(&mut contract, &token, line_intent);
        }
    }

    RequestedWorkContract {
        deliverable_targets: dedupe_targets(contract.deliverable_targets),
        reference_inputs: dedupe_targets(contract.reference_inputs),
        example_targets: dedupe_targets(contract.example_targets),
        naming_patterns: dedupe_targets(contract.naming_patterns),
        verification_commands: dedupe_targets(contract.verification_commands),
    }
}

fn explicit_artifact_targets_in_text(text: &str) -> Vec<String> {
    artifact_targets_from_instruction_text(text, true)
}

fn artifact_targets_from_instruction_text(text: &str, include_unknown: bool) -> Vec<String> {
    let mut targets = Vec::new();
    for raw_line in text.lines() {
        let (line_without_code, code_spans) = split_backtick_spans(raw_line);
        for code_span in code_spans {
            if let Some(target) = normalize_artifact_token(code_span.trim()) {
                if include_unknown
                    || classify_artifact_target(&target) != ArtifactTargetKind::Unknown
                {
                    targets.push(target);
                }
            }
        }
        targets.extend(
            line_without_code
                .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | '、' | '，'))
                .filter_map(normalize_artifact_token)
                .filter(|target| {
                    include_unknown
                        || classify_artifact_target(target) != ArtifactTargetKind::Unknown
                }),
        );
    }
    dedupe_targets(targets)
}

fn staged_task_verification_commands(
    latest_user_text: Option<&str>,
    staged_task_artifacts: &[String],
    workspace_root: &Utf8Path,
) -> Vec<String> {
    let mut commands = latest_user_text
        .map(explicit_verification_commands_from_text)
        .unwrap_or_default();
    for artifact in staged_task_artifacts {
        let normalized = normalize_prompt_target(artifact);
        if normalized.is_empty() || !is_staged_task_artifact_target(&normalized) {
            continue;
        }
        let full_path = workspace_root.join(&normalized);
        let Ok(content) = fs::read_to_string(full_path.as_std_path()) else {
            continue;
        };
        commands.extend(explicit_verification_commands_from_text(&content));
    }
    dedupe_targets(commands)
}

pub(crate) fn extract_protected_artifact_targets(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut targets = Vec::new();

    for phrase in ["do not change", "don't change", "without changing"] {
        let mut search_from = 0usize;
        while let Some(found) = lower[search_from..].find(phrase) {
            let start = search_from + found + phrase.len();
            let end = text[start..]
                .find('\n')
                .map(|offset| start + offset)
                .unwrap_or(text.len());
            targets.extend(extract_requested_artifact_targets(&text[start..end]));
            search_from = start;
        }
    }

    let mut search_from = 0usize;
    while let Some(found) = lower[search_from..].find("keep ") {
        let start = search_from + found + "keep ".len();
        let Some(unchanged_offset) = lower[start..].find(" unchanged") else {
            break;
        };
        let end = start + unchanged_offset;
        targets.extend(extract_requested_artifact_targets(&text[start..end]));
        search_from = end + " unchanged".len();
    }

    for phrase in ["は変更しない", "はまだ変更しない", "は変更せず"] {
        let mut search_from = 0usize;
        while let Some(found) = text[search_from..].find(phrase) {
            let end = search_from + found;
            let start = text[..end]
                .rfind('\n')
                .map(|offset| offset + 1)
                .unwrap_or(0);
            targets.extend(extract_requested_artifact_targets(&text[start..end]));
            search_from = end + phrase.len();
        }
    }

    for raw_line in text.lines() {
        let normalized_line = raw_line.to_ascii_lowercase().replace('`', "");
        if !line_has_harness_owned_marker(&normalized_line) {
            continue;
        }
        for target in extract_requested_artifact_targets(raw_line) {
            if target_is_contract_reference(&target) {
                targets.push(target);
            }
        }
    }

    dedupe_targets(targets)
}

pub(crate) fn documentation_only_follow_up_requested(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "docs only",
        "documentation only",
        "only update the documentation",
        "only update docs",
        "keep the current implementation unchanged",
        "keep current implementation unchanged",
        "do not change source",
        "do not change code",
        "do not change tests",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || text.contains("文書だけ")
        || text.contains("ドキュメントだけ")
        || text.contains("実装コードと test は変更しない")
        || text.contains("実装コードと test はまだ変更しない")
        || text.contains("コードと test は変更しない")
        || text.contains("コードと test はまだ変更しない")
        || text.contains("変更せず")
}

pub(crate) fn documentation_change_may_lead_implementation(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let fact_only = [
        "confirmed facts only",
        "document the current implementation",
        "describe the current implementation",
        "document the current calculator design",
        "current implementation only",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || text.contains("確認できた事実だけ")
        || text.contains("現在の実装を調査")
        || text.contains("現在の実装を文書化")
        || text.contains("現状実装");
    if fact_only {
        return false;
    }

    let deferred_turn = [
        "this turn",
        "for this turn",
        "not yet",
        "still do not change",
        "leave the implementation unchanged for now",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || text.contains("この turn では")
        || text.contains("このターンでは")
        || text.contains("まだ変更しない")
        || text.contains("まだ変えない");
    let spec_shift = [
        "specification becomes",
        "update the specification",
        "becomes a scientific calculator",
        "support sin",
        "support cos",
        "support sqrt",
        "support pow",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || text.contains("仕様へ更新")
        || text.contains("扱える仕様")
        || text.contains("関数電卓版")
        || text.contains("文書だけを更新");
    let implementation_locked = documentation_only_follow_up_requested(text);

    implementation_locked && deferred_turn && spec_shift
}

fn implementation_follow_up_references_prior_design(text: &str) -> bool {
    if documentation_only_follow_up_requested(text) {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    let mentions_spec_or_design = [
        "design document",
        "design doc",
        "specification",
        "spec",
        "requirements",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || text.contains("設計書")
        || text.contains("仕様")
        || text.contains("要件");
    let asks_for_implementation = [
        "implement",
        "update",
        "change",
        "modify",
        "adjust",
        "according to",
        "based on",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || text.contains("合わせて")
        || text.contains("参考に")
        || text.contains("変更")
        || text.contains("実装")
        || text.contains("作成");

    mentions_spec_or_design && asks_for_implementation
}

fn record_requested_target(
    contract: &mut RequestedWorkContract,
    target: &str,
    line_intent: RequestedLineIntent,
) {
    if target.is_empty() {
        return;
    }
    if is_instruction_input_target(target) || matches!(line_intent, RequestedLineIntent::Reference)
    {
        contract.reference_inputs.push(target.to_string());
        return;
    }

    match line_intent {
        RequestedLineIntent::Deliverable => contract.deliverable_targets.push(target.to_string()),
        RequestedLineIntent::Convention => contract.example_targets.push(target.to_string()),
        RequestedLineIntent::Verification => {}
        RequestedLineIntent::Reference => contract.reference_inputs.push(target.to_string()),
        RequestedLineIntent::Unknown => {
            if classify_artifact_target(target) != ArtifactTargetKind::Unknown {
                contract.deliverable_targets.push(target.to_string());
            }
        }
    }
}

fn line_target_is_reference_input(line: &str, target: &str) -> bool {
    let lower_line = line.to_ascii_lowercase();
    let normalized_line = lower_line.replace('`', "");
    let lower_target = target.to_ascii_lowercase();
    if line_target_is_protected_reference_input(&normalized_line, &lower_target) {
        return true;
    }
    if [
        format!("{lower_target} を参照"),
        format!("{lower_target} を参考"),
        format!("{lower_target} に従って"),
        format!("read {lower_target}"),
        format!("follow {lower_target}"),
        format!("according to {lower_target}"),
        format!("based on {lower_target}"),
        format!("consult {lower_target}"),
    ]
    .into_iter()
    .any(|pattern| normalized_line.contains(&pattern))
    {
        return true;
    }

    [
        format!("{lower_target} の"),
        format!("{lower_target} section"),
        format!("{lower_target} heading"),
        format!("{lower_target} セクション"),
    ]
    .into_iter()
    .any(|pattern| normalized_line.contains(&pattern))
        && [
            "参照",
            "参考",
            "に従って",
            "従って",
            "follow",
            "according to",
            "based on",
            "consult",
            "read ",
        ]
        .into_iter()
        .any(|marker| normalized_line.contains(marker))
        && ["section", "heading", "節", "セクション"]
            .into_iter()
            .any(|marker| normalized_line.contains(marker))
}

fn line_target_is_protected_reference_input(normalized_line: &str, lower_target: &str) -> bool {
    if !normalized_line.contains(lower_target) {
        return false;
    }

    if target_is_contract_reference(lower_target) && line_has_harness_owned_marker(normalized_line)
    {
        return true;
    }

    let stem = lower_target
        .rsplit('/')
        .next()
        .unwrap_or(lower_target)
        .split_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(lower_target);
    let aliases = if stem != lower_target {
        vec![lower_target, stem]
    } else {
        vec![lower_target]
    };

    if ["already exists", "pre-existing", "既にあります", "既に存在"]
        .iter()
        .any(|marker| normalized_line.contains(marker))
        && [
            "変更せず",
            "変更しない",
            "do not change",
            "do not modify",
            "don't change",
            "without changing",
            "keep ",
        ]
        .iter()
        .any(|marker| normalized_line.contains(marker))
    {
        return true;
    }

    for marker in [
        "変更せず",
        "変更しない",
        "do not change",
        "do not modify",
        "don't change",
        "without changing",
        "unchanged",
    ] {
        let Some(marker_start) = normalized_line.find(marker) else {
            continue;
        };
        for alias in &aliases {
            for (target_start, _) in normalized_line.match_indices(*alias) {
                if target_start >= marker_start {
                    continue;
                }
                let target_end = target_start + alias.len();
                let between = normalized_line[target_end..marker_start].trim();
                if !between_contains_deliverable_verb(between) {
                    return true;
                }
            }
        }
    }

    aliases.iter().any(|alias| {
        [
            format!("{alias} を変更せず"),
            format!("{alias} を変更しない"),
            format!("{alias} は変更せず"),
            format!("{alias} は変更しない"),
            format!("do not change {alias}"),
            format!("do not modify {alias}"),
            format!("don't change {alias}"),
            format!("without changing {alias}"),
            format!("keep {alias} unchanged"),
        ]
        .into_iter()
        .any(|pattern| normalized_line.contains(&pattern))
    })
}

fn line_has_harness_owned_marker(normalized_line: &str) -> bool {
    normalized_line.contains("harness-owned")
        || normalized_line.contains("harness owned")
        || normalized_line.contains("harness 管理")
}

fn target_is_contract_reference(target: &str) -> bool {
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    let filename = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    filename.contains("contract")
}

fn between_contains_deliverable_verb(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "作成",
        "生成",
        "更新",
        "実装",
        "create",
        "make",
        "write",
        "implement",
        "update",
        "modify",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

pub(crate) fn is_instruction_input_target(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let filename = lower.rsplit('/').next().unwrap_or(lower.as_str());
    if lower.starts_with(".moyai/commands/") || lower.starts_with(".codex/commands/") {
        return true;
    }
    !lower.starts_with("docs/")
        && !lower.contains("/docs/")
        && matches!(
            filename,
            "task.md" | "task.txt" | "instruction.md" | "instructions.md" | "agents.md"
        )
}

fn classify_requested_line_intent(
    original_line: &str,
    line_without_code: &str,
    code_spans: &[String],
) -> RequestedLineIntent {
    const OUTPUT_MARKERS: &[&str] = &[
        "作成",
        "更新",
        "修正",
        "生成",
        "出力",
        "編集",
        "追記",
        "追加",
        "書き",
        "create",
        "write",
        "update",
        "edit",
        "modify",
        "rewrite",
        "generate",
        "add ",
        "required output",
        "required outputs",
        "deliverable",
        "deliverables",
    ];
    const REFERENCE_MARKERS: &[&str] = &[
        "に従って",
        "従って",
        "参照",
        "参考",
        "source:",
        "履歴",
        "残課題",
        "follow",
        "according to",
        "based on",
        "consult",
        "history",
        "known issue",
        "known issues",
        "read ",
    ];
    const EXAMPLE_MARKERS: &[&str] = &[
        "例:",
        "例：",
        "たとえば",
        "例えば",
        "e.g.",
        "for example",
        "such as",
    ];
    const NAMING_MARKERS: &[&str] = &["命名規則", "規則", "pattern", "naming"];

    let lower = original_line.to_ascii_lowercase();
    let has_output_markers = OUTPUT_MARKERS.iter().any(|marker| lower.contains(marker));
    let has_reference_markers = REFERENCE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker));
    let has_example_markers = EXAMPLE_MARKERS.iter().any(|marker| lower.contains(marker));
    let has_naming_markers = NAMING_MARKERS.iter().any(|marker| lower.contains(marker))
        || code_spans
            .iter()
            .any(|span| looks_like_naming_pattern(span));
    let has_verification_markers = instruction_contains_verification_command(original_line)
        || instruction_contains_verification_command(line_without_code)
        || code_spans
            .iter()
            .any(|span| instruction_contains_verification_command(span))
        || [
            "pytest",
            "cargo test",
            "python -m unittest",
            "uv run",
            "verification",
        ]
        .iter()
        .any(|marker| lower.contains(marker));

    if has_reference_markers && !has_output_markers {
        RequestedLineIntent::Reference
    } else if has_output_markers {
        RequestedLineIntent::Deliverable
    } else if has_verification_markers {
        RequestedLineIntent::Verification
    } else if (has_example_markers || has_naming_markers) && !has_output_markers {
        RequestedLineIntent::Convention
    } else {
        RequestedLineIntent::Unknown
    }
}

fn split_backtick_spans(line: &str) -> (String, Vec<String>) {
    let mut outside = String::with_capacity(line.len());
    let mut spans = Vec::new();
    let mut current = String::new();
    let mut inside = false;

    for ch in line.chars() {
        if ch == '`' {
            if inside {
                spans.push(current.clone());
                current.clear();
            }
            inside = !inside;
            outside.push(' ');
            continue;
        }
        if inside {
            current.push(ch);
            outside.push(' ');
        } else {
            outside.push(ch);
        }
    }

    if inside && !current.is_empty() {
        outside.push_str(&current);
    }

    (outside, spans)
}

fn instruction_contains_verification_command(text: &str) -> bool {
    looks_like_verification_command(Some(text), text)
}

fn looks_like_naming_pattern(candidate: &str) -> bool {
    candidate.contains('*') || candidate.contains('?')
}

fn normalize_artifact_token(token: &str) -> Option<String> {
    let trimmed = token
        .trim_matches(|ch: char| {
            matches!(
                ch,
                '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ','
            )
        })
        .trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';' | '!' | '?'))
        .trim_start_matches(|ch: char| matches!(ch, '*' | '-' | '+'));
    if trimmed.is_empty() {
        return None;
    }

    let candidate = trimmed.strip_prefix("./").unwrap_or(trimmed);
    if !(candidate.contains('/') || candidate.contains('\\') || candidate.contains('.')) {
        return None;
    }
    if candidate.chars().any(char::is_whitespace) {
        return None;
    }
    if !candidate
        .chars()
        .any(artifact_token_has_substantive_character)
    {
        return None;
    }
    if artifact_token_is_inline_numeric_or_version_literal(candidate) {
        return None;
    }
    if artifact_token_is_requirement_category_list(candidate) {
        return None;
    }
    Some(candidate.to_string())
}

fn artifact_token_has_substantive_character(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '*' | '?')
}

fn artifact_token_is_inline_numeric_or_version_literal(candidate: &str) -> bool {
    let normalized = candidate.replace('\\', "/");
    if normalized.contains('/') {
        return false;
    }
    let without_sign = normalized
        .strip_prefix('+')
        .or_else(|| normalized.strip_prefix('-'))
        .unwrap_or(normalized.as_str());
    if dotted_digits_only(without_sign) {
        return true;
    }
    let Some(version_body) = without_sign
        .strip_prefix('v')
        .or_else(|| without_sign.strip_prefix('V'))
    else {
        return false;
    };
    dotted_digits_only(version_body)
}

fn artifact_token_is_requirement_category_list(candidate: &str) -> bool {
    let normalized = candidate.replace('\\', "/");
    if !normalized.contains('/') || normalized.ends_with('/') || normalized.contains('.') {
        return false;
    }
    let segments = normalized
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    segments.len() >= 2
        && segments.iter().all(|segment| {
            segment
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'))
                && segment.chars().any(|ch| ch.is_ascii_uppercase())
        })
}

fn dotted_digits_only(value: &str) -> bool {
    value.contains('.')
        && value.chars().any(|ch| ch.is_ascii_digit())
        && value.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactTargetKind {
    Documentation,
    Implementation,
    Unknown,
}

pub(crate) fn is_staged_task_artifact_target(target: &str) -> bool {
    let normalized = target.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let filename = lower.rsplit('/').next().unwrap_or(lower.as_str());
    !lower.starts_with("docs/")
        && !lower.contains("/docs/")
        && matches!(
            filename,
            "task.md"
                | "task.txt"
                | "instruction.md"
                | "instructions.md"
                | "instruction.txt"
                | "instructions.txt"
        )
}

pub(crate) fn classify_artifact_target(target: &str) -> ArtifactTargetKind {
    let normalized = target.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let filename = lower.rsplit('/').next().unwrap_or(lower.as_str());
    let extension = filename.rsplit('.').next().unwrap_or_default();
    let inside_docs = lower.starts_with("docs/") || lower.contains("/docs/");

    if is_staged_task_artifact_target(target) {
        return ArtifactTargetKind::Unknown;
    }

    if inside_docs
        || matches!(
            filename,
            "readme" | "readme.md" | "changelog" | "changelog.md"
        )
        || matches!(extension, "md" | "rst" | "adoc")
    {
        return ArtifactTargetKind::Documentation;
    }

    if matches!(
        extension,
        "rs" | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "java"
            | "kt"
            | "go"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "cs"
            | "swift"
            | "rb"
            | "php"
            | "scala"
            | "sh"
            | "ps1"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
    ) || (normalized.contains('/') && filename.starts_with("test_"))
        || lower.contains("/src/")
        || lower.contains("/tests/")
    {
        return ArtifactTargetKind::Implementation;
    }

    ArtifactTargetKind::Unknown
}

pub(crate) fn target_looks_like_textual_document_output(target: &str) -> bool {
    let normalized = target.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let filename = lower.rsplit('/').next().unwrap_or(lower.as_str());
    let extension = filename.rsplit('.').next().unwrap_or_default();
    matches!(extension, "md" | "txt" | "rst" | "adoc")
}

pub(crate) fn looks_like_structured_document_work(text: Option<&str>) -> bool {
    let Some(text) = text else {
        return false;
    };
    let lower = text.to_ascii_lowercase();
    lower.contains("docling_convert")
        || lower.contains("structured document")
        || ["pdf", "docx", "xlsx", "pptx", "csv", "html"]
            .iter()
            .any(|needle| lower.contains(needle))
}

pub(crate) fn todo_looks_like_structured_document_work(todo: &TodoItem) -> bool {
    looks_like_structured_document_work(Some(todo.content.as_str()))
}

fn structured_document_summary_active(
    latest_user: Option<&str>,
    todos: &[TodoItem],
    requested_targets: &[String],
    execution_focus_targets: &[String],
) -> bool {
    let has_text_output_target = requested_targets
        .iter()
        .chain(execution_focus_targets.iter())
        .any(|target| target_looks_like_textual_document_output(target));
    has_text_output_target
        && (looks_like_structured_document_work(latest_user)
            || todos.iter().any(todo_looks_like_structured_document_work))
}

fn artifact_targets_from_part(part: &MessagePart) -> Vec<String> {
    match part {
        MessagePart::ToolCall(call) => {
            artifact_targets_from_tool_call(&call.tool_name.to_string(), &call.arguments_json)
        }
        _ => Vec::new(),
    }
}

fn artifact_targets_from_tool_call(tool_name: &str, arguments_json: &str) -> Vec<String> {
    if matches!(tool_name, "read" | "list" | "glob" | "grep") {
        return extract_readonly_target(tool_name, arguments_json)
            .into_iter()
            .collect();
    }

    if tool_name == "apply_patch" {
        let value: Value = match serde_json::from_str(arguments_json) {
            Ok(value) => value,
            Err(_) => return Vec::new(),
        };
        let Some(patch_text) = value.get("patch_text").and_then(Value::as_str) else {
            return Vec::new();
        };
        return extract_patch_targets(patch_text);
    }

    if tool_name == "write" {
        let value: Value = match serde_json::from_str(arguments_json) {
            Ok(value) => value,
            Err(_) => return Vec::new(),
        };
        return value
            .get("path")
            .and_then(Value::as_str)
            .map(|path| vec![path.to_string()])
            .unwrap_or_default();
    }

    Vec::new()
}

fn staged_task_artifacts_seen(transcript: &Transcript, start_index: usize) -> Vec<String> {
    let mut artifacts = Vec::new();
    for message in &transcript.messages[start_index..] {
        for part in &message.parts {
            for target in artifact_targets_from_part(&part.payload) {
                if is_staged_task_artifact_target(&target) {
                    artifacts.push(target);
                }
            }
        }
    }
    dedupe_targets(artifacts)
}

fn staged_task_output_targets(
    transcript: &Transcript,
    start_index: usize,
    staged_task_artifacts: &[String],
) -> Vec<String> {
    if staged_task_artifacts.is_empty() {
        return Vec::new();
    }

    let mut staged_task_read_calls = HashMap::new();
    let mut targets = Vec::new();
    for message in &transcript.messages[start_index..] {
        for part in &message.parts {
            match &part.payload {
                MessagePart::ToolCall(call) if call.tool_name == ToolName::Read => {
                    if let Some(target) =
                        extract_readonly_target(&call.tool_name.to_string(), &call.arguments_json)
                    {
                        if is_staged_task_artifact_target(&target) {
                            staged_task_read_calls.insert(call.tool_call_id, target);
                        }
                    }
                }
                MessagePart::ToolResult(value) => {
                    if staged_task_read_calls.contains_key(&value.tool_call_id)
                        && staged_task_artifact_read_result_contributes_output_targets(
                            &value.title,
                            value.status,
                        )
                    {
                        targets.extend(extract_requested_artifact_targets(&value.summary));
                    }
                }
                _ => {}
            }
        }
    }

    dedupe_targets(
        targets
            .into_iter()
            .filter(|target| {
                !staged_task_artifacts
                    .iter()
                    .any(|artifact| artifact.eq_ignore_ascii_case(target))
            })
            .collect(),
    )
}

fn staged_task_artifact_read_result_contributes_output_targets(
    title: &str,
    status: ToolCallStatus,
) -> bool {
    status == ToolCallStatus::Completed && title.starts_with("Read ")
}

fn is_write_tool_name(tool_name: &str) -> bool {
    matches!(tool_name, "apply_patch" | "write")
}

fn extract_patch_targets(patch_text: &str) -> Vec<String> {
    let parsed_targets = PatchParser::parse(patch_text)
        .ok()
        .map(|operations| {
            operations
                .into_iter()
                .flat_map(|operation| match operation {
                    crate::edit::PatchOperation::Add { path, .. }
                    | crate::edit::PatchOperation::Update { path, .. }
                    | crate::edit::PatchOperation::Delete { path } => {
                        vec![path.as_str().to_string()]
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !parsed_targets.is_empty() {
        return parsed_targets;
    }

    patch_text
        .lines()
        .filter_map(|line| {
            line.strip_prefix("*** Add File: ")
                .or_else(|| line.strip_prefix("*** Update File: "))
                .or_else(|| line.strip_prefix("*** Delete File: "))
                .map(|value| value.trim().to_string())
        })
        .collect()
}

fn extract_readonly_target(tool_name: &str, arguments_json: &str) -> Option<String> {
    if !matches!(
        tool_name,
        "read" | "list" | "glob" | "grep" | "inspect_directory"
    ) {
        return None;
    }

    let value: Value = serde_json::from_str(arguments_json).ok()?;
    value
        .get("path")
        .and_then(Value::as_str)
        .or_else(|| value.get("pattern").and_then(Value::as_str))
        .or_else(|| value.get("query").and_then(Value::as_str))
        .map(|value| value.to_string())
}

fn dedupe_targets(targets: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    targets
        .iter()
        .filter_map(|target| {
            if seen.insert(target.clone()) {
                Some(target.clone())
            } else {
                None
            }
        })
        .collect()
}

fn recent_assistant_code_block_stall(transcript: &Transcript, start_index: usize) -> bool {
    for message in transcript.messages[start_index..].iter().rev() {
        if !matches!(message.record.role, MessageRole::Assistant) {
            continue;
        }
        let has_code_block = message.parts.iter().any(|part| {
            matches!(
                &part.payload,
                MessagePart::Text(value) if value.text.contains("```")
            )
        });
        if !has_code_block {
            continue;
        }
        let has_tool_call = message
            .parts
            .iter()
            .any(|part| matches!(&part.payload, MessagePart::ToolCall(_)));
        return !has_tool_call;
    }
    false
}

fn recent_assistant_pseudo_tool_call_stall(transcript: &Transcript, start_index: usize) -> bool {
    transcript.messages[start_index..]
        .iter()
        .rev()
        .find(|message| matches!(message.record.role, MessageRole::Assistant))
        .map(|message| {
            message.parts.iter().any(|part| match &part.payload {
                MessagePart::Text(value) => contains_pseudo_tool_call_markup(&value.text),
                MessagePart::Reasoning(value) => contains_pseudo_tool_call_markup(&value.text),
                _ => false,
            })
        })
        .unwrap_or(false)
}

fn contains_pseudo_tool_call_markup(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("<tool_call>")
        || lower.contains("</tool_call>")
        || lower.contains("<function=")
        || lower.contains("<parameter=")
}

fn recent_invalid_tool_result_stall(transcript: &Transcript, start_index: usize) -> bool {
    for message in transcript.messages[start_index..].iter().rev() {
        if !matches!(message.record.role, MessageRole::Assistant) {
            continue;
        }
        for part in &message.parts {
            if let MessagePart::ToolResult(value) = &part.payload {
                let title = value.title.to_ascii_lowercase();
                let summary = value.summary.to_ascii_lowercase();
                if title.contains("invalid tool call")
                    || title.contains("invalid tool arguments")
                    || title.contains("tool not allowed in current run state")
                    || summary.contains("unknown tool `")
                    || summary.contains("available tools:")
                    || summary.contains("allowed tools for this turn:")
                    || summary.contains("not available in the current run state")
                    || summary.contains("tool was called with invalid")
                    || summary.contains("satisfies the expected schema")
                {
                    return true;
                }
            }
        }
    }
    false
}

fn recent_nonprogress_recovery_result_stall_with_config(
    transcript: &Transcript,
    start_index: usize,
    agent_config: &AgentConfig,
) -> bool {
    let consecutive_recovery_results = transcript.messages[start_index..]
        .iter()
        .rev()
        .flat_map(|message| message.parts.iter().rev())
        .filter_map(|part| match &part.payload {
            MessagePart::ToolResult(value) if value.status == ToolCallStatus::Completed => {
                Some(value)
            }
            _ => None,
        })
        .take_while(|value| prompt_tool_result_is_nonprogress(value))
        .take(RECENT_TOOL_CALL_WINDOW)
        .count();

    consecutive_recovery_results >= agent_config.staged_task_recovery_stall_threshold
}

fn latest_verification_repair_focus_required_result(
    transcript: &Transcript,
    start_index: usize,
) -> bool {
    for part in transcript.messages[start_index..]
        .iter()
        .rev()
        .flat_map(|message| message.parts.iter().rev())
    {
        let MessagePart::ToolResult(value) = &part.payload else {
            continue;
        };
        if value.status != ToolCallStatus::Completed {
            continue;
        }
        if verification_repair_read_budget_exhausted_result(&value.title, &value.summary) {
            return true;
        }
        if generic_disallowed_verification_repair_read_result(&value.title, &value.summary) {
            continue;
        }
        return false;
    }

    false
}

fn verification_repair_read_budget_exhausted_result(title: &str, summary: &str) -> bool {
    title == "Verification repair focus required"
        || (title == "Tool not allowed in current run state"
            && summary.contains("`read` tool is not available in the current run state")
            && (summary.contains("Do not spend another turn rereading")
                || summary.contains("Do not keep using `read`")
                || summary.contains("reread budget")))
}

fn generic_disallowed_verification_repair_read_result(title: &str, summary: &str) -> bool {
    title == "Tool not allowed in current run state"
        && summary.contains("`read` tool is not available in the current run state")
}

fn latest_verification_repair_focus_required_target(
    transcript: &Transcript,
    start_index: usize,
) -> Option<String> {
    let latest_user = latest_user_index(transcript, start_index)?;
    for message in transcript.messages.iter().skip(latest_user + 1).rev() {
        if !matches!(message.record.role, MessageRole::Assistant) {
            continue;
        }
        for part in message.parts.iter().rev() {
            let MessagePart::ToolResult(value) = &part.payload else {
                continue;
            };
            if value.status != ToolCallStatus::Completed {
                continue;
            }
            if value.title == "Verification repair focus required" {
                return required_write_path_from_repair_focus_summary(&value.summary);
            }
            if verification_output_looks_successful_prompt_result(value)
                || looks_like_verification_failure(None, &value.title, &value.summary)
                || successful_write_result_title(&value.title)
            {
                return None;
            }
        }
    }
    None
}

fn required_write_path_from_repair_focus_summary(summary: &str) -> Option<String> {
    let marker = "Required next `write.path`: exactly `";
    let remainder = summary.split(marker).nth(1)?;
    let end = remainder.find('`')?;
    let target = remainder[..end].trim();
    (!target.is_empty()).then(|| target.replace('\\', "/"))
}

fn latest_staged_task_documentation_audit_state(
    transcript: &Transcript,
    start_index: usize,
    required_targets: &[String],
) -> Option<StagedTaskDocumentationAuditPromptState> {
    let latest_user = latest_user_index(transcript, start_index)?;
    let mut write_targets_by_call = HashMap::new();
    let mut current: Option<StagedTaskDocumentationAuditPromptState> = None;

    for message in &transcript.messages[latest_user + 1..] {
        for part in &message.parts {
            match &part.payload {
                MessagePart::ToolCall(value) if value.tool_name.to_string() == "write" => {
                    let Ok(arguments) = serde_json::from_str::<Value>(&value.arguments_json) else {
                        continue;
                    };
                    if let Some(path) = arguments.get("path").and_then(Value::as_str) {
                        write_targets_by_call
                            .insert(value.tool_call_id.to_string(), path.trim().to_string());
                    }
                }
                MessagePart::ToolResult(value) => {
                    if value.status != ToolCallStatus::Completed {
                        continue;
                    }
                    if is_staged_task_documentation_audit_result_title(&value.title) {
                        let feedback =
                            staged_task_documentation_audit_feedback_excerpt(&value.summary);
                        let target = write_targets_by_call
                            .get(&value.tool_call_id.to_string())
                            .cloned()
                            .unwrap_or_else(|| "the current deliverable".to_string());
                        let failure_count = current
                            .as_ref()
                            .filter(|state| {
                                prompt_target_matches_required_output(
                                    &target,
                                    std::slice::from_ref(&state.target),
                                )
                            })
                            .map(|state| state.failure_count + 1)
                            .unwrap_or(1);
                        current = Some(StagedTaskDocumentationAuditPromptState {
                            target,
                            feedback,
                            actionable_feedback: value
                                .summary
                                .contains("Apply these concrete fixes in the next rewrite:"),
                            failure_count,
                        });
                        continue;
                    }
                    let Some(state) = current.as_mut() else {
                        continue;
                    };
                    let tool_call_id = value.tool_call_id.to_string();
                    if let Some(target) = write_targets_by_call.get(&tool_call_id) {
                        let matches_required = required_targets.is_empty()
                            || required_targets.iter().any(|required| {
                                prompt_target_matches_required_output(
                                    target,
                                    std::slice::from_ref(required),
                                )
                            });
                        if matches_required
                            && prompt_target_matches_required_output(
                                target,
                                std::slice::from_ref(&state.target),
                            )
                            && !prompt_tool_result_is_nonprogress(value)
                        {
                            current = None;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    current
}

fn is_staged_task_documentation_audit_result_title(title: &str) -> bool {
    matches!(
        title,
        "Staged task documentation audit failed"
            | "Staged task documentation close-out audit failed"
    )
}

fn recent_patch_repair_targets(transcript: &Transcript, start_index: usize) -> Vec<String> {
    let tool_calls = tool_name_by_call_id(&transcript.messages[start_index..]);
    for message in transcript.messages[start_index..].iter().rev() {
        if !matches!(message.record.role, MessageRole::Assistant) {
            continue;
        }
        let message_targets = dedupe_targets(
            message
                .parts
                .iter()
                .flat_map(|part| artifact_targets_from_part(&part.payload))
                .collect::<Vec<_>>(),
        );
        for part in message.parts.iter().rev() {
            match &part.payload {
                MessagePart::DiffSummary(_) => return Vec::new(),
                MessagePart::ToolResult(value)
                    if tool_result_clears_patch_recovery(
                        value,
                        tool_calls.get(&value.tool_call_id).copied(),
                    ) =>
                {
                    return Vec::new();
                }
                MessagePart::ToolResult(value) if value.title == "Patch repair escalation" => {
                    return message_targets.clone();
                }
                _ => {}
            }
        }
    }
    Vec::new()
}

fn recent_verification_failures(transcript: &Transcript, start_index: usize) -> Vec<String> {
    let tool_calls = tool_call_details(&transcript.messages[start_index..]);

    for message in transcript.messages[start_index..].iter().rev() {
        for part in message.parts.iter().rev() {
            match &part.payload {
                MessagePart::DiffSummary(_) => continue,
                MessagePart::ToolResult(value) => {
                    if value.status != ToolCallStatus::Completed {
                        continue;
                    }
                    let Some((tool_name, command)) = tool_calls.get(&value.tool_call_id) else {
                        continue;
                    };
                    if verification_failure_is_cleared_by_result(
                        value,
                        *tool_name,
                        command.as_deref(),
                    ) {
                        return Vec::new();
                    }
                    if *tool_name != ToolName::Shell
                        || !looks_like_verification_command(command.as_deref(), &value.title)
                        || !looks_like_verification_failure(
                            command.as_deref(),
                            &value.title,
                            &value.summary,
                        )
                    {
                        continue;
                    }
                    let labels = extract_failure_labels(&value.summary);
                    if !labels.is_empty() {
                        return labels;
                    }
                    return vec![fallback_verification_failure_label(
                        command.as_deref(),
                        &value.title,
                    )];
                }
                _ => {}
            }
        }
    }

    Vec::new()
}

fn verification_repair_rotated_focus_target(
    transcript: &Transcript,
    start_index: usize,
    state: Option<&SessionStateSnapshot>,
) -> Option<String> {
    latest_user_index(transcript, start_index)?;
    let failure = latest_verification_failure_context(transcript)?;
    let preceding_repair_targets = latest_failed_verification_preceding_repair_targets(transcript);
    let public_behavior_contract = state
        .and_then(|state| state.verification.failure_cluster.as_ref())
        .map(verification_cluster_indicates_public_behavior_contract)
        .unwrap_or(false);
    if public_behavior_contract
        && preceding_repair_targets
            .iter()
            .any(|target| !target_is_test_like(target.as_str()))
    {
        return None;
    }
    if failure.targets.len() <= 1 {
        return failure
            .targets
            .first()
            .map(|target| target.as_str().to_string());
    }
    if preceding_repair_targets.is_empty() {
        return None;
    }

    failure
        .targets
        .iter()
        .find(|candidate| {
            !preceding_repair_targets.iter().any(|repaired| {
                prompt_target_matches_required_output(
                    candidate.as_str(),
                    &[repaired.as_str().to_string()],
                )
            })
        })
        .map(|target| target.as_str().to_string())
}

fn verification_cluster_indicates_public_behavior_contract(
    cluster: &crate::session::VerificationFailureCluster,
) -> bool {
    cluster.evidence.iter().any(|evidence| {
        evidence.subtype.as_deref().is_some_and(|subtype| {
            subtype.starts_with("public_") || subtype == "source_test_contract_repair"
        }) || !evidence.public_state_assertions.is_empty()
            || !evidence.public_missing_attributes.is_empty()
            || evidence.evidence_markers.iter().any(|marker| {
                let lower = marker.to_ascii_lowercase();
                lower.contains("public")
                    || lower.contains("contract-owned")
                    || lower.contains("scenario_contract")
                    || lower.contains("beh-")
                    || lower.contains("state-")
            })
    })
}

fn latest_verification_repair_target_rotation_required_target(
    transcript: &Transcript,
    start_index: usize,
) -> Option<String> {
    let latest_user = latest_user_index(transcript, start_index)?;
    for message in transcript.messages.iter().skip(latest_user + 1).rev() {
        if !matches!(message.record.role, MessageRole::Assistant) {
            continue;
        }
        for part in message.parts.iter().rev() {
            let MessagePart::ToolResult(value) = &part.payload else {
                continue;
            };
            if value.status != ToolCallStatus::Completed {
                continue;
            }
            if value.title == "Verification repair target rotation required" {
                return required_target_from_target_rotation_summary(&value.summary);
            }
            if verification_output_looks_successful_prompt_result(value)
                || looks_like_verification_failure(None, &value.title, &value.summary)
                || successful_write_result_title(&value.title)
            {
                return None;
            }
        }
    }
    None
}

fn required_target_from_target_rotation_summary(summary: &str) -> Option<String> {
    let marker = "The next edit must target `";
    let remainder = summary.split(marker).nth(1)?;
    let end = remainder.find('`')?;
    let target = remainder[..end].trim();
    (!target.is_empty()).then(|| target.replace('\\', "/"))
}

fn verification_output_looks_successful_prompt_result(result: &ToolResultPart) -> bool {
    let lower_title = result.title.to_ascii_lowercase();
    let lower_summary = result.summary.to_ascii_lowercase();
    lower_title.contains("success")
        || lower_summary.contains("ok") && lower_summary.contains("passed")
}

fn successful_write_result_title(title: &str) -> bool {
    title.starts_with("Wrote ") || title.starts_with("Applied patch")
}

fn tool_name_by_call_id(
    messages: &[crate::session::TranscriptMessage],
) -> HashMap<crate::session::ToolCallId, ToolName> {
    let mut tool_calls = HashMap::new();
    for message in messages {
        for part in &message.parts {
            if let MessagePart::ToolCall(value) = &part.payload {
                tool_calls.insert(value.tool_call_id, value.tool_name);
            }
        }
    }
    tool_calls
}

fn tool_call_details(
    messages: &[crate::session::TranscriptMessage],
) -> HashMap<crate::session::ToolCallId, (ToolName, Option<String>)> {
    let mut tool_calls = HashMap::new();
    for message in messages {
        for part in &message.parts {
            if let MessagePart::ToolCall(value) = &part.payload {
                let command = if value.tool_name == ToolName::Shell {
                    extract_shell_command(&value.arguments_json)
                } else {
                    None
                };
                tool_calls.insert(value.tool_call_id, (value.tool_name, command));
            }
        }
    }
    tool_calls
}

fn tool_result_clears_patch_recovery(value: &ToolResultPart, tool_name: Option<ToolName>) -> bool {
    if value.status != ToolCallStatus::Completed {
        return false;
    }
    let Some(tool_name) = tool_name else {
        return false;
    };
    if !matches!(
        tool_name,
        ToolName::Read
            | ToolName::InspectDirectory
            | ToolName::DoclingConvert
            | ToolName::Write
            | ToolName::ApplyPatch
            | ToolName::Shell
            | ToolName::McpCall
    ) {
        return false;
    }
    !prompt_tool_result_is_nonprogress(value)
}

fn verification_failure_is_cleared_by_result(
    value: &ToolResultPart,
    tool_name: ToolName,
    command: Option<&str>,
) -> bool {
    if value.status != ToolCallStatus::Completed {
        return false;
    }
    tool_name == ToolName::Shell
        && looks_like_verification_command(command, &value.title)
        && !looks_like_verification_failure(command, &value.title, &value.summary)
        && !prompt_tool_result_is_nonprogress(value)
}

fn prompt_tool_result_is_nonprogress(value: &ToolResultPart) -> bool {
    if value.success == Some(false) {
        return true;
    }
    if matches!(
        value.progress_effect,
        crate::protocol::ToolProgressEffect::NoProgress
            | crate::protocol::ToolProgressEffect::Blocked
            | crate::protocol::ToolProgressEffect::VerificationFailed
    ) {
        return true;
    }
    false
}

fn extract_failure_labels(summary: &str) -> Vec<String> {
    let mut labels = Vec::new();
    for line in summary.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("FAIL: ") {
            labels.push(compact_failure_label(rest));
        } else if let Some(rest) = trimmed.strip_prefix("ERROR: ") {
            labels.push(compact_failure_label(rest));
        } else if trimmed.starts_with("test_")
            && (trimmed.contains("... FAIL") || trimmed.contains("... ERROR"))
        {
            labels.push(
                trimmed
                    .split_whitespace()
                    .next()
                    .unwrap_or(trimmed)
                    .to_string(),
            );
        }
        if labels.len() >= MAX_VERIFICATION_FAILURE_LABELS {
            break;
        }
    }
    dedupe_targets(labels)
}

fn compact_failure_label(raw: &str) -> String {
    raw.split_once(" (")
        .map(|(label, _)| label.trim().to_string())
        .unwrap_or_else(|| raw.trim().to_string())
}

fn fallback_verification_failure_label(command: Option<&str>, title: &str) -> String {
    let command = command.unwrap_or_default().trim();
    if !command.is_empty() {
        format!("verification command failed: {command}")
    } else if !title.trim().is_empty() {
        format!("verification command failed: {}", title.trim())
    } else {
        "latest verification command failed".to_string()
    }
}

fn extract_shell_command(arguments_json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(arguments_json).ok()?;
    value
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn latest_inactive_target_edit_rejection(
    messages: &[crate::session::TranscriptMessage],
) -> Option<String> {
    for part in messages
        .iter()
        .rev()
        .flat_map(|message| message.parts.iter().rev())
    {
        let MessagePart::ToolResult(value) = &part.payload else {
            continue;
        };
        if value.status == ToolCallStatus::Completed
            && value.title == "Inactive target edit blocked"
        {
            return Some(value.summary.clone());
        }
        if prompt_tool_result_is_nonprogress(value) {
            continue;
        }
        return None;
    }
    None
}

fn inactive_target_recovery_required_read_target(
    transcript: &Transcript,
    workspace_root: &Utf8Path,
    active_targets: &[String],
) -> Option<String> {
    if active_targets.len() != 1 {
        return None;
    }
    let target = active_targets[0].clone();
    if !target_is_test_like(&target) {
        return None;
    }
    let target_path = Utf8Path::new(&target);
    let absolute = if target_path.is_absolute() {
        target_path.to_path_buf()
    } else {
        workspace_root.join(target_path)
    };
    if !absolute.is_file() {
        return None;
    }
    let start_index = prompt_window_start_index(transcript);
    let latest_user = latest_user_index(transcript, start_index)?;
    let latest_rejection_index = transcript.messages[latest_user + 1..]
        .iter()
        .enumerate()
        .filter_map(|(offset, message)| {
            message
                .parts
                .iter()
                .any(|part| {
                    matches!(
                        &part.payload,
                        MessagePart::ToolResult(value)
                            if value.status == ToolCallStatus::Completed
                                && value.title == "Inactive target edit blocked"
                    )
                })
                .then_some(latest_user + 1 + offset)
        })
        .last()?;
    let read_after_rejection = transcript.messages[latest_rejection_index + 1..]
        .iter()
        .flat_map(|message| message.parts.iter())
        .any(|part| {
            let MessagePart::ToolCall(value) = &part.payload else {
                return false;
            };
            value.tool_name == ToolName::Read
                && extract_readonly_target(&value.tool_name.to_string(), &value.arguments_json)
                    .is_some_and(|read_target| {
                        prompt_target_matches_required_output(&read_target, &[target.clone()])
                    })
        });
    (!read_after_rejection).then_some(target)
}

fn assistant_message_has_suppressed_tool_call(
    message: &crate::session::TranscriptMessage,
    suppressed_tool_call_ids: &BTreeSet<String>,
) -> bool {
    !suppressed_tool_call_ids.is_empty()
        && message.parts.iter().any(|part| {
            matches!(
                &part.payload,
                MessagePart::ToolCall(value)
                    if suppressed_tool_call_ids.contains(&value.tool_call_id.to_string())
            )
        })
}

fn stale_write_tool_call_replay_targets(
    messages: &[crate::session::TranscriptMessage],
    required_target: &str,
) -> BTreeMap<String, String> {
    let mut targets = BTreeMap::new();
    if required_target.trim().is_empty() {
        return targets;
    }
    for message in messages {
        for part in &message.parts {
            let MessagePart::ToolCall(value) = &part.payload else {
                continue;
            };
            if value.tool_name.to_string() != "write" {
                continue;
            }
            let Some(path) = write_path_from_arguments_json(&value.arguments_json) else {
                continue;
            };
            if !prompt_target_matches_required_output(&path, &[required_target.to_string()]) {
                targets.insert(value.tool_call_id.to_string(), path);
            }
        }
    }
    targets
}

fn stale_write_prelude_message_indices(
    messages: &[crate::session::TranscriptMessage],
    start_index: usize,
    stale_write_tool_call_ids: &BTreeSet<String>,
) -> BTreeSet<usize> {
    stale_tool_call_prelude_message_indices(messages, start_index, stale_write_tool_call_ids)
}

fn stale_tool_call_prelude_message_indices(
    messages: &[crate::session::TranscriptMessage],
    start_index: usize,
    stale_tool_call_ids: &BTreeSet<String>,
) -> BTreeSet<usize> {
    let mut indices = BTreeSet::new();
    if stale_tool_call_ids.is_empty() {
        return indices;
    }
    for (offset, message) in messages.iter().enumerate() {
        if !assistant_message_has_suppressed_tool_call(message, stale_tool_call_ids) {
            continue;
        }
        let mut cursor = offset;
        while cursor > 0 {
            cursor -= 1;
            let candidate = &messages[cursor];
            if !assistant_message_is_stale_tool_call_prelude(candidate) {
                break;
            }
            indices.insert(start_index + cursor);
        }
    }
    indices
}

fn stale_todo_progress_tool_call_replay_ids(
    messages: &[crate::session::TranscriptMessage],
    required_write_target: Option<&str>,
    todos: &[TodoItem],
) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let Some(required_write_target) = required_write_target else {
        return ids;
    };
    if required_write_target.trim().is_empty() || todos.is_empty() {
        return ids;
    }
    if !target_is_test_like(required_write_target) {
        return ids;
    }
    for message in messages {
        for part in &message.parts {
            let MessagePart::ToolCall(value) = &part.payload else {
                continue;
            };
            if value.tool_name != ToolName::TodoWrite {
                continue;
            }
            ids.insert(value.tool_call_id.to_string());
        }
    }
    ids
}

fn assistant_message_is_stale_tool_call_prelude(
    message: &crate::session::TranscriptMessage,
) -> bool {
    if !matches!(message.record.role, MessageRole::Assistant) {
        return false;
    }
    !message.parts.is_empty()
        && message.parts.iter().all(|part| {
            matches!(
                part.payload,
                MessagePart::Text(_) | MessagePart::Reasoning(_)
            )
        })
}

pub(crate) fn stale_write_tool_call_replay_is_summary_only(
    arguments_json: &str,
    required_target: &str,
) -> bool {
    write_path_from_arguments_json(arguments_json).is_some_and(|path| {
        !prompt_target_matches_required_output(&path, &[required_target.to_string()])
    })
}

pub(crate) fn stale_write_tool_call_replay_omits_payload(
    arguments_json: &str,
    required_target: &str,
    stale_payload_probe: &str,
) -> bool {
    let Some(stale_target) = write_path_from_arguments_json(arguments_json) else {
        return false;
    };
    if prompt_target_matches_required_output(&stale_target, &[required_target.to_string()]) {
        return false;
    }
    let note = stale_write_tool_result_replay_note(&stale_target, required_target);
    !note.contains(stale_payload_probe)
        && !note.contains(arguments_json)
        && note.contains(&format!("active write target is `{required_target}`"))
}

pub(crate) fn stale_inactive_authoring_replay_uses_live_builder() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let stale_payload = "def implementation_only():\n    return 'old source'\n";
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "stale inactive authoring replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let history_items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create source.py and test_source.py".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                tool: ToolName::Write,
                arguments: json!({
                    "path": "source.py",
                    "content": stale_payload,
                }),
                model_arguments: Value::Null,
                effective_arguments: json!({
                    "path": "source.py",
                    "content": stale_payload,
                }),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::Write],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Wrote Added source.py".to_string(),
                output_text: format!("Added source.py\n{stale_payload}"),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("fixture-source-write".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        active_authoring_targets: vec!["test_source.py".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let mut saw_omission_note = false;
    let mut saw_stale_tool_call = false;
    let mut saw_stale_tool_output = false;
    for message in &projection.messages {
        match message {
            ModelMessage::System { content } => {
                if content.contains("inactive target")
                    && content.contains("non-executable historical context")
                    && content.contains("test_source.py")
                    && !content.contains("[omitted inactive authoring target]")
                    && !content.contains("[omitted stale inactive authoring payload")
                    && !content.contains(stale_payload)
                {
                    saw_omission_note = true;
                }
            }
            ModelMessage::AssistantToolCalls { tool_calls, .. } => {
                for call in tool_calls {
                    if call.call_id == call_id.to_string() {
                        saw_stale_tool_call = true;
                    }
                }
            }
            ModelMessage::Tool {
                call_id: replayed_call_id,
                ..
            } => {
                if replayed_call_id == &call_id.to_string() {
                    saw_stale_tool_output = true;
                }
            }
            _ => {}
        }
    }
    let serialized = serde_json::to_string(&projection.messages).unwrap_or_default();
    saw_omission_note
        && !saw_stale_tool_call
        && !saw_stale_tool_output
        && !serialized.contains("[omitted inactive authoring target]")
        && !serialized.contains("[omitted stale inactive authoring payload")
        && !serialized.contains(stale_payload)
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "stale_inactive_authoring_payload_omitted"
                && policy.call_id.as_deref() == Some(&call_id.to_string())
                && policy.omitted_targets == vec!["source.py".to_string()]
                && policy.active_targets == vec!["test_source.py".to_string()]
        })
}

pub fn stale_inactive_authoring_replay_omits_fake_executable_arguments() -> bool {
    stale_inactive_authoring_replay_uses_live_builder()
}

pub(crate) fn failed_inactive_authoring_replay_uses_call_scoped_summary() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let stale_payload = "def implementation_only():\n    return 'wrong target rewrite'\n";
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "failed inactive authoring replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let history_items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create source.py, README.md, and test_source.py".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                tool: ToolName::Write,
                arguments: json!({
                    "path": "source.py",
                    "content": stale_payload,
                }),
                model_arguments: Value::Null,
                effective_arguments: json!({
                    "path": "source.py",
                    "content": stale_payload,
                }),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::Write],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Wrong authoring target".to_string(),
                output_text: "The submitted content-changing `write` call targets `source.py`, but the current active requested deliverables are `README.md`, `test_source.py`.".to_string(),
                metadata: json!({
                    "operation_progress_class": "wrong_authoring_target",
                    "progress_effect": "no_progress",
                    "submitted_targets": ["source.py"],
                    "active_authoring_targets": ["README.md", "test_source.py"],
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("fixture-wrong-target".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        active_authoring_targets: vec!["README.md".to_string(), "test_source.py".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let call_id_text = call_id.to_string();
    let mut saw_failed_tool_call = false;
    let mut saw_failed_tool_output = false;
    for message in &projection.messages {
        match message {
            ModelMessage::AssistantToolCalls { tool_calls, .. } => {
                if tool_calls.iter().any(|tool_call| {
                    tool_call.call_id == call_id_text
                        && tool_call.tool_name == "write"
                        && tool_call.arguments_json.contains("\"path\":\"source.py\"")
                        && tool_call.arguments_json.contains("implementation_only")
                }) {
                    saw_failed_tool_call = true;
                }
            }
            ModelMessage::Tool {
                call_id: replayed_call_id,
                result,
                ..
            } => {
                if replayed_call_id == &call_id_text
                    && result.contains("source.py")
                    && result.contains("README.md")
                    && result.contains("test_source.py")
                {
                    saw_failed_tool_output = true;
                }
            }
            _ => {}
        }
    }
    let serialized = serde_json::to_string(&projection.messages).unwrap_or_default();
    saw_failed_tool_call
        && saw_failed_tool_output
        && serialized.contains("implementation_only")
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "failed_inactive_authoring_call_output_preserved"
                && policy.call_id.as_deref() == Some(call_id_text.as_str())
                && policy.omitted_targets == vec!["source.py".to_string()]
                && policy.active_targets
                    == vec!["README.md".to_string(), "test_source.py".to_string()]
        })
}

pub fn provider_replay_preserves_failed_inactive_authoring_feedback() -> bool {
    failed_inactive_authoring_replay_uses_call_scoped_summary()
}

pub(crate) fn stale_progress_projection_replay_uses_live_builder() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "stale progress projection replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let stale_plan_text = "space_invader.py 作成";
    let history_items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create the requested deliverables.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                tool: ToolName::TodoWrite,
                arguments: json!({
                    "todos": [
                        {
                            "id": "step1",
                            "content": stale_plan_text,
                            "status": "in_progress",
                            "priority": "high",
                            "targets": ["space_invader.py"]
                        }
                    ]
                }),
                model_arguments: Value::Null,
                effective_arguments: Value::Null,
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::TodoWrite],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Plan updated".to_string(),
                output_text:
                    "Plan updated [tool feedback] progress_projection no_progress space_invader.py"
                        .to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("fixture-plan".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        active_authoring_targets: vec![
            "README.md".to_string(),
            "test_space_invader.py".to_string(),
        ],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let mut saw_omission_note = false;
    let mut saw_progress_tool_call = false;
    let mut saw_progress_tool_output = false;
    for message in &projection.messages {
        match message {
            ModelMessage::System { content } => {
                if content.contains("Historical progress-projection tool call/output pair")
                    && content.contains("non-executable planning context")
                    && content.contains("README.md")
                    && content.contains("test_space_invader.py")
                    && !content.contains(stale_plan_text)
                {
                    saw_omission_note = true;
                }
            }
            ModelMessage::AssistantToolCalls { tool_calls, .. } => {
                for call in tool_calls {
                    if call.call_id == call_id.to_string() {
                        saw_progress_tool_call = true;
                    }
                }
            }
            ModelMessage::Tool {
                call_id: replayed_call_id,
                ..
            } => {
                if replayed_call_id == &call_id.to_string() {
                    saw_progress_tool_output = true;
                }
            }
            _ => {}
        }
    }
    let serialized = serde_json::to_string(&projection.messages).unwrap_or_default();
    saw_omission_note
        && !saw_progress_tool_call
        && !saw_progress_tool_output
        && !serialized.contains(stale_plan_text)
        && !serialized.contains("Plan updated [tool feedback]")
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "progress_projection_payload_omitted"
                && policy.call_id.as_deref() == Some(&call_id.to_string())
                && policy.tool_name.as_deref() == Some("todowrite")
                && policy.omitted_targets == vec!["space_invader.py".to_string()]
                && policy.active_targets
                    == vec!["README.md".to_string(), "test_space_invader.py".to_string()]
        })
}

pub fn provider_replay_omits_stale_progress_projection_arguments() -> bool {
    stale_progress_projection_replay_uses_live_builder()
        && current_progress_projection_feedback_replay_preserves_call_output()
}

pub(crate) fn current_progress_projection_feedback_replay_preserves_call_output() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let stale_call_id = crate::session::ToolCallId::new();
    let current_call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "current progress projection feedback replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let history_items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create README.md and test_space_invader.py.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id: stale_call_id,
                tool: ToolName::TodoWrite,
                arguments: json!({
                    "todos": [{
                        "id": "step1",
                        "content": "space_invader.py 作成",
                        "status": "in_progress",
                        "targets": ["space_invader.py"]
                    }]
                }),
                model_arguments: Value::Null,
                effective_arguments: Value::Null,
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::TodoWrite],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: stale_call_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Plan updated".to_string(),
                output_text: "Plan updated [tool feedback] progress_projection no_progress space_invader.py"
                    .to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("stale-plan".to_string()),
                verification_run: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::ToolCall {
                call_id: current_call_id,
                tool: ToolName::TodoWrite,
                arguments: json!({
                    "todos": [{
                        "id": "step2",
                        "content": "test_space_invader.py の作成",
                        "status": "in_progress",
                        "targets": ["test_space_invader.py"]
                    }, {
                        "id": "step3",
                        "content": "README.md の作成",
                        "status": "pending",
                        "targets": ["README.md"]
                    }]
                }),
                model_arguments: Value::Null,
                effective_arguments: Value::Null,
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::TodoWrite],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::ToolOutput {
                call_id: current_call_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Plan updated".to_string(),
                output_text:
                    "Plan updated\n\n[tool feedback]\noperation_progress_class: progress_projection\nprogress_effect: no_progress\nactive_targets: README.md, test_space_invader.py\nContinue with a file-changing tool output."
                        .to_string(),
                metadata: json!({
                    "tool_feedback_envelope": {
                        "operation_progress_class": "progress_projection",
                        "progress_effect": "no_progress",
                        "active_targets": ["README.md", "test_space_invader.py"]
                    }
                }),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("current-plan".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        active_authoring_targets: vec![
            "README.md".to_string(),
            "test_space_invader.py".to_string(),
        ],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let stale_call_id = stale_call_id.to_string();
    let current_call_id = current_call_id.to_string();
    let stale_pair_omitted = !projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|call| call.call_id == stale_call_id)
        ) || matches!(message, ModelMessage::Tool { call_id, .. } if call_id == &stale_call_id)
    });
    let current_call_index = projection.messages.iter().position(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|call| call.call_id == current_call_id)
        )
    });
    let current_output_index = projection.messages.iter().position(|message| {
        matches!(
            message,
            ModelMessage::Tool { call_id, result, .. }
                if call_id == &current_call_id
                    && result.contains("progress_projection")
                    && result.contains("README.md")
                    && result.contains("test_space_invader.py")
        )
    });
    stale_pair_omitted
        && matches!((current_call_index, current_output_index), (Some(call), Some(output)) if call < output)
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "progress_projection_payload_omitted"
                && policy.call_id.as_deref() == Some(&stale_call_id)
        })
        && !projection.replay_policies.iter().any(|policy| {
            policy.policy == "progress_projection_payload_omitted"
                && policy.call_id.as_deref() == Some(&current_call_id)
        })
}

pub(crate) fn content_shape_mismatch_replay_preserves_tool_lifecycle_without_payload() -> bool {
    let session_id = crate::session::SessionId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let call_id = crate::session::ToolCallId::new();
    let stale_payload = "def main():\n    input('> ')\n";
    let transcript = Transcript {
        session: SessionRecord {
            id: session_id,
            project_id: crate::session::ProjectId::new(),
            title: "content-shape mismatch replay".to_string(),
            status: crate::session::SessionStatus::Running,
            cwd: Utf8PathBuf::from("C:/workspace"),
            model: "local".to_string(),
            base_url: "http://localhost:1234".to_string(),
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: None,
        },
        messages: vec![
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: user_message_id,
                    session_id,
                    role: MessageRole::User,
                    parent_message_id: None,
                    sequence_no: 1,
                    created_at_ms: 1,
                    metadata: MessageMetadata::User(crate::session::UserMessageMeta {
                        cwd: Utf8PathBuf::from("C:/workspace"),
                        requested_model: None,
                        editor_context: None,
                    }),
                },
                parts: vec![crate::session::PartRecord {
                    id: crate::session::PartId::new(),
                    message_id: user_message_id,
                    sequence_no: 1,
                    kind: crate::session::PartKind::Text,
                    payload: MessagePart::Text(crate::session::TextPart {
                        text: "create calculator.py and test_calculator.py".to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: assistant_message_id,
                    session_id,
                    role: MessageRole::Assistant,
                    parent_message_id: None,
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: "local".to_string(),
                        base_url: "http://localhost:1234".to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                parts: vec![
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 1,
                        kind: crate::session::PartKind::ToolCall,
                        payload: MessagePart::ToolCall(crate::session::ToolCallPart {
                            tool_call_id: call_id,
                            tool_name: ToolName::Write,
                            arguments_json: json!({
                                "path": "calculator.py",
                                "content": stale_payload,
                            })
                            .to_string(),
                            model_arguments_json: None,
                            effective_arguments_json: None,
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 2,
                        kind: crate::session::PartKind::ToolResult,
                        payload: MessagePart::ToolResult(crate::session::ToolResultPart {
                            tool_call_id: call_id,
                            status: ToolCallStatus::Completed,
                            title: "Required write content shape mismatch".to_string(),
                            summary: "The submitted `write` call targeted `calculator.py`, but current active work requires test content in `test_calculator.py` that imports `calculator`.".to_string(),
                            success: Some(false),
                            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                            blocked_action: None,
                            required_next_action: None,
                            result_hash: Some("fixture-required-write-content-shape-mismatch".to_string()),
                        }),
                    },
                ],
            },
        ],
    };
    let state = SessionStateSnapshot {
        process_phase: ProcessPhase::Author,
        active_targets: vec![Utf8PathBuf::from("test_calculator.py")],
        ..SessionStateSnapshot::default()
    };
    let turn_id = crate::protocol::TurnId::new();
    let history_items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: Some(user_message_id),
                content: vec![ContentPart::Text {
                    text: "create calculator.py and test_calculator.py".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                tool: ToolName::Write,
                arguments: json!({
                    "path": "calculator.py",
                    "content": stale_payload,
                }),
                model_arguments: Value::Null,
                effective_arguments: json!({
                    "path": "calculator.py",
                    "content": stale_payload,
                }),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::Write],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Required write content shape mismatch".to_string(),
                output_text: "The submitted `write` call targeted `calculator.py`, but current active work requires test content in `test_calculator.py` that imports `calculator`.".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("fixture-required-write-content-shape-mismatch".to_string()),
                verification_run: None,
            },
        },
    ];
    let messages = build_messages_with_state(
        &transcript,
        &transcript.session,
        &history_items,
        &state,
        &[],
        50,
        &["write".to_string()],
        &PromptSignals::default(),
        None,
    )
    .messages;
    let mut saw_sanitized_tool_call = false;
    let mut saw_tool_output = false;
    for message in messages {
        match message {
            ModelMessage::AssistantToolCalls { tool_calls, .. } => {
                for call in tool_calls {
                    if call.call_id == call_id.to_string()
                        && call.tool_name == "write"
                        && call
                            .arguments_json
                            .contains("omitted incompatible write payload")
                        && !call.arguments_json.contains(stale_payload)
                        && !call.arguments_json.contains("calculator.py")
                    {
                        saw_sanitized_tool_call = true;
                    }
                }
            }
            ModelMessage::Tool {
                call_id: replayed_call_id,
                tool_name,
                result,
            } => {
                if replayed_call_id == call_id.to_string()
                    && tool_name == "write"
                    && result.contains("current active work requires test content")
                    && !result.contains(stale_payload)
                {
                    saw_tool_output = true;
                }
            }
            _ => {}
        }
    }
    saw_sanitized_tool_call && saw_tool_output
}

pub(crate) fn stale_write_prelude_replay_omits_text(
    required_target: &str,
    stale_target: &str,
) -> bool {
    let session_id = crate::session::SessionId::new();
    let message_id = crate::session::MessageId::new();
    let tool_call_message_id = crate::session::MessageId::new();
    let call_id = crate::session::ToolCallId::new();
    let transcript = Transcript {
        session: SessionRecord {
            id: session_id,
            project_id: crate::session::ProjectId::new(),
            title: "stale write prelude".to_string(),
            status: crate::session::SessionStatus::Running,
            cwd: Utf8PathBuf::from("C:/workspace"),
            model: "local".to_string(),
            base_url: "http://localhost:1234".to_string(),
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: None,
        },
        messages: vec![
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: message_id,
                    session_id,
                    role: MessageRole::Assistant,
                    parent_message_id: None,
                    sequence_no: 1,
                    created_at_ms: 1,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: "local".to_string(),
                        base_url: "http://localhost:1234".to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                parts: vec![crate::session::PartRecord {
                    id: crate::session::PartId::new(),
                    message_id,
                    sequence_no: 1,
                    kind: crate::session::PartKind::Text,
                    payload: MessagePart::Text(crate::session::TextPart {
                        text: format!("`{stale_target}` を作成します。"),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: tool_call_message_id,
                    session_id,
                    role: MessageRole::Assistant,
                    parent_message_id: None,
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: "local".to_string(),
                        base_url: "http://localhost:1234".to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                parts: vec![crate::session::PartRecord {
                    id: crate::session::PartId::new(),
                    message_id: tool_call_message_id,
                    sequence_no: 1,
                    kind: crate::session::PartKind::ToolCall,
                    payload: MessagePart::ToolCall(crate::session::ToolCallPart {
                        tool_call_id: call_id,
                        tool_name: ToolName::Write,
                        arguments_json: json!({
                            "path": stale_target,
                            "content": "old payload"
                        })
                        .to_string(),
                        model_arguments_json: None,
                        effective_arguments_json: None,
                    }),
                }],
            },
        ],
    };
    let stale_targets = stale_write_tool_call_replay_targets(&transcript.messages, required_target);
    let stale_ids = stale_targets.keys().cloned().collect::<BTreeSet<_>>();
    let prelude_indices = stale_write_prelude_message_indices(&transcript.messages, 0, &stale_ids);
    prelude_indices.contains(&0) && !prelude_indices.contains(&1)
}

pub(crate) fn stale_todo_progress_replay_omits_prior_plan(
    required_target: &str,
    stale_plan_text: &str,
) -> bool {
    let session_id = crate::session::SessionId::new();
    let message_id = crate::session::MessageId::new();
    let tool_call_message_id = crate::session::MessageId::new();
    let call_id = crate::session::ToolCallId::new();
    let call_id_text = call_id.to_string();
    let transcript = Transcript {
        session: SessionRecord {
            id: session_id,
            project_id: crate::session::ProjectId::new(),
            title: "stale todo progress".to_string(),
            status: crate::session::SessionStatus::Running,
            cwd: Utf8PathBuf::from("C:/workspace"),
            model: "local".to_string(),
            base_url: "http://localhost:1234".to_string(),
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: None,
        },
        messages: vec![
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: message_id,
                    session_id,
                    role: MessageRole::Assistant,
                    parent_message_id: None,
                    sequence_no: 1,
                    created_at_ms: 1,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: "local".to_string(),
                        base_url: "http://localhost:1234".to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                parts: vec![crate::session::PartRecord {
                    id: crate::session::PartId::new(),
                    message_id,
                    sequence_no: 1,
                    kind: crate::session::PartKind::Text,
                    payload: MessagePart::Text(crate::session::TextPart {
                        text: stale_plan_text.to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: tool_call_message_id,
                    session_id,
                    role: MessageRole::Assistant,
                    parent_message_id: None,
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: "local".to_string(),
                        base_url: "http://localhost:1234".to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                parts: vec![crate::session::PartRecord {
                    id: crate::session::PartId::new(),
                    message_id: tool_call_message_id,
                    sequence_no: 1,
                    kind: crate::session::PartKind::ToolCall,
                    payload: MessagePart::ToolCall(crate::session::ToolCallPart {
                        tool_call_id: call_id,
                        tool_name: ToolName::TodoWrite,
                        arguments_json: json!({
                            "todos": [
                                {
                                    "id": "source",
                                    "content": "write calculator.py",
                                    "status": "in_progress",
                                    "priority": "high",
                                    "targets": ["calculator.py"]
                                },
                                {
                                    "id": "test",
                                    "content": "write test_calculator.py",
                                    "status": "pending",
                                    "priority": "high",
                                    "targets": [required_target]
                                }
                            ]
                        })
                        .to_string(),
                        model_arguments_json: None,
                        effective_arguments_json: None,
                    }),
                }],
            },
        ],
    };
    let mut current_todo = TodoItem::simple(
        format!("write {required_target}"),
        crate::session::TodoStatus::InProgress,
        crate::session::TodoPriority::High,
    );
    current_todo
        .targets
        .push(Utf8PathBuf::from(required_target));
    let ids = stale_todo_progress_tool_call_replay_ids(
        &transcript.messages,
        Some(required_target),
        &[current_todo],
    );
    let prelude_indices = stale_tool_call_prelude_message_indices(&transcript.messages, 0, &ids);
    let no_current_focus_ids =
        stale_todo_progress_tool_call_replay_ids(&transcript.messages, Some(required_target), &[]);
    ids.contains(&call_id_text)
        && prelude_indices.contains(&0)
        && !prelude_indices.contains(&1)
        && no_current_focus_ids.is_empty()
}

pub(crate) fn exact_write_target_contract_projects_content_authority(target: &str) -> bool {
    let contract = exact_write_target_contract(target);
    contract.contains("write")
        && contract.contains(&format!("`path` set to `{target}`"))
        && contract
            .contains("The provider-visible tool schema remains the stable `write` interface")
        && !contract.contains("ActionAuthority")
        && contract.contains("Older assistant narration")
        && if target_is_test_like(target) {
            contract.contains("test module")
                && contract.contains("Required positive shape")
                && contract.contains("Forbidden shape")
                && crate::agent::content_shape_contract::python_source_for_test_target(target)
                    .is_none_or(|shape| {
                        contract.contains(&format!(
                            "`{}` is the inferred production source",
                            shape.source_path
                        )) && contract.contains(&format!("import `{}`", shape.module_name))
                            && contract.contains(&format!("do not rewrite `{}`", shape.source_path))
                            && contract
                                .contains(&format!("{}(unittest.TestCase)", shape.class_name))
                    })
        } else {
            contract.contains("active target only")
        }
}

pub(crate) fn exact_authoring_write_required_preserves_source_progress_projection() -> bool {
    let mut source_state = SessionStateSnapshot {
        process_phase: ProcessPhase::Author,
        active_targets: vec![Utf8PathBuf::from("calculator.py")],
        ..SessionStateSnapshot::default()
    };
    source_state.completion.open_work_count = 1;
    let mut test_state = source_state.clone();
    test_state.active_targets = vec![Utf8PathBuf::from("test_calculator.py")];
    exact_active_authoring_write_required(&source_state).is_none()
        && exact_active_authoring_write_required(&test_state).as_deref()
            == Some("test_calculator.py")
}

pub fn provider_replay_preserves_latest_user_across_trailing_compaction() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "trailing compaction replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let call_id = crate::session::ToolCallId::new();
    let original_user_text = "create calculator.py and test_calculator.py";
    let current_hook_text = "Manual verification-repair continuation: repair calculator.py, then rerun python -m unittest.";
    let items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: original_user_text.to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                tool: ToolName::Shell,
                arguments: json!({"command": "python -m unittest"}),
                model_arguments: json!({"command": "python -m unittest"}),
                effective_arguments: json!({"command": "python -m unittest"}),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::Shell],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "verification failed".to_string(),
                output_text: "very long old verification output that compaction should summarize"
                    .to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("old-verification".to_string()),
                verification_run: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: current_hook_text.to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::MidTurn,
                summary: "previous verification repair context was compacted".to_string(),
                replacement_item_ids: vec![crate::protocol::HistoryItemId::new()],
                continuation: Some(crate::session::ContinuationContract {
                    route: "code".to_string(),
                    process_phase: "repair".to_string(),
                    active_work_kind: Some("typed_continuation".to_string()),
                    active_work_summary: Some(
                        "repair calculator.py then rerun python -m unittest".to_string(),
                    ),
                    required_next_action: None,
                    target_files: vec![Utf8PathBuf::from("calculator.py")],
                    verification_commands: vec!["python -m unittest".to_string()],
                    failure_kind: Some("VerificationFailed".to_string()),
                    failure_summary: Some("unit test failed".to_string()),
                    completion_blocker: Some("verification failed".to_string()),
                    invariant_refs: vec!["CompactionContinuity".to_string()],
                }),
            },
        },
    ];

    let replay = build_provider_replay_messages_from_history_items(&session, &items, 1);
    let serialized = serde_json::to_string(&replay).unwrap_or_default();
    replay.iter().any(|message| {
        matches!(
            message,
            ModelMessage::System { content }
                if content.contains("previous verification repair context was compacted")
                    && content.contains("CompactionContinuity")
        )
    }) && replay.iter().any(|message| {
        matches!(
            message,
            ModelMessage::User { content } if content == current_hook_text
        )
    }) && !serialized.contains(original_user_text)
        && !serialized.contains("very long old verification output")
}

pub fn provider_replay_after_compaction_repairs_orphan_assistant_before_user() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "compaction orphan assistant fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: "local".to_string(),
        base_url: "http://localhost:1234".to_string(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let latest_user = "continue after compaction";
    let items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 0,
            created_at_ms: 0,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "user query paired with assistant after compaction".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::MidTurn,
                summary: "older user turn and answer were compacted".to_string(),
                replacement_item_ids: vec![crate::protocol::HistoryItemId::new()],
                continuation: Some(crate::session::ContinuationContract {
                    route: "code".to_string(),
                    process_phase: "discover".to_string(),
                    active_work_kind: Some("typed_continuation".to_string()),
                    active_work_summary: Some("continue the chat".to_string()),
                    required_next_action: None,
                    target_files: Vec::new(),
                    verification_commands: Vec::new(),
                    failure_kind: None,
                    failure_summary: None,
                    completion_blocker: None,
                    invariant_refs: vec!["CompactionContinuity".to_string()],
                }),
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::Message {
                message_id: None,
                role: MessageRole::Assistant,
                content: vec![ContentPart::Text {
                    text: "orphan answer from compacted pair".to_string(),
                }],
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: latest_user.to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
    ];

    let replay = build_provider_replay_messages_from_history_items(&session, &items, 32);
    let Some(first_non_system) = replay
        .iter()
        .find(|message| !matches!(message, ModelMessage::System { .. }))
    else {
        return false;
    };
    let serialized = serde_json::to_string(&replay).unwrap_or_default();
    matches!(first_non_system, ModelMessage::User { content } if content == "user query paired with assistant after compaction")
        && serialized.contains("orphan answer from compacted pair")
        && serialized.contains(latest_user)
}

pub fn provider_replay_preserves_tool_pair_symmetry_with_model_arguments() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let orphan_call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "tool pair symmetry fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: "fixture-model".to_string(),
        base_url: "http://fixture".to_string(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let hook_text = "Manual ST closeout continuation: create test_space_invader.py.";
    let items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "inspect workspace".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                tool: ToolName::Read,
                arguments: Value::Null,
                model_arguments: json!({"path": "space_invader.py"}),
                effective_arguments: Value::Null,
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::Read],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Read space_invader.py".to_string(),
                output_text: "class Player: pass".to_string(),
                metadata: json!({"success": true}),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::Unknown,
                blocked_action: None,
                required_next_action: None,
                result_hash: Some("read-hash".to_string()),
                verification_run: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::ToolOutput {
                call_id: orphan_call_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Orphan output".to_string(),
                output_text: "orphan output must not be provider-visible".to_string(),
                metadata: json!({}),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::Unknown,
                blocked_action: None,
                required_next_action: None,
                result_hash: None,
                verification_run: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: hook_text.to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
    ];

    let replay = build_provider_replay_messages_from_history_items(&session, &items, 32);
    let serialized = serde_json::to_string(&replay).unwrap_or_default();
    let call_id_text = call_id.to_string();
    let call_index = replay.iter().position(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|tool_call| {
                    tool_call.call_id == call_id_text
                        && tool_call.tool_name == "read"
                        && tool_call.arguments_json.contains("space_invader.py")
                })
        )
    });
    let output_index = replay.iter().position(|message| {
        matches!(
            message,
            ModelMessage::Tool { call_id: replayed, result, .. }
                if replayed == &call_id_text && result.contains("class Player")
        )
    });
    let user_index = replay.iter().rposition(
        |message| matches!(message, ModelMessage::User { content } if content == hook_text),
    );

    matches!((call_index, output_index, user_index), (Some(call), Some(output), Some(user)) if call < output && output < user)
        && !serialized.contains("orphan output must not be provider-visible")
}

fn stale_write_tool_result_replay_note(stale_target: &str, required_target: &str) -> String {
    format!(
        "Previous `write` arguments for `{stale_target}` are intentionally omitted from provider-visible history because the current active write target is `{required_target}`. Do not reuse the omitted arguments; use the current tool schema and active-work projection."
    )
}

fn write_path_from_arguments_json(arguments_json: &str) -> Option<String> {
    let value: Value = serde_json::from_str(arguments_json).ok()?;
    value
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
}

fn superseded_tool_denial_replay_state(
    messages: &[crate::session::TranscriptMessage],
    current_tool_names: &[String],
) -> (Vec<String>, BTreeSet<String>) {
    let mut denied_tools: Vec<String> = Vec::new();
    let mut suppressed_ids = BTreeSet::new();
    for message in messages {
        for part in &message.parts {
            let MessagePart::ToolResult(value) = &part.payload else {
                continue;
            };
            if value.title != "Tool not allowed in current run state" {
                continue;
            }
            let Some(denied_tool) = denied_tool_name_from_unavailable_summary(&value.summary)
            else {
                continue;
            };
            if current_tool_names
                .iter()
                .any(|tool| tool.eq_ignore_ascii_case(denied_tool.as_str()))
            {
                suppressed_ids.insert(value.tool_call_id.to_string());
                if !denied_tools
                    .iter()
                    .any(|tool| tool.eq_ignore_ascii_case(denied_tool.as_str()))
                {
                    denied_tools.push(denied_tool);
                }
            }
        }
    }
    (denied_tools, suppressed_ids)
}

fn denied_tool_name_from_unavailable_summary(summary: &str) -> Option<String> {
    let remainder = summary.strip_prefix("The `")?;
    let end = remainder.find("` tool is not available in the current run state")?;
    let tool_name = remainder[..end].trim();
    (!tool_name.is_empty()).then(|| tool_name.to_string())
}

fn latest_denied_edit_targets_after_latest_user(
    transcript: &Transcript,
    start_index: usize,
) -> Vec<String> {
    let Some(latest_user) = latest_user_index(transcript, start_index) else {
        return Vec::new();
    };

    let mut tool_calls = HashMap::new();
    for message in &transcript.messages[latest_user + 1..] {
        for part in &message.parts {
            let MessagePart::ToolCall(value) = &part.payload else {
                continue;
            };
            tool_calls.insert(
                value.tool_call_id.to_string(),
                (value.tool_name.to_string(), value.arguments_json.clone()),
            );
        }
    }

    for message in transcript.messages[latest_user + 1..].iter().rev() {
        for part in message.parts.iter().rev() {
            let MessagePart::ToolResult(value) = &part.payload else {
                continue;
            };
            if value.title != "Tool not allowed in current run state" {
                continue;
            }
            let Some(denied_tool) = denied_tool_name_from_unavailable_summary(&value.summary)
            else {
                continue;
            };
            if denied_tool != "write" && denied_tool != "apply_patch" {
                return Vec::new();
            }
            let Some((tool_name, arguments_json)) = tool_calls.get(&value.tool_call_id.to_string())
            else {
                return Vec::new();
            };
            return prompt_edit_targets_from_arguments_json(tool_name, arguments_json);
        }
    }

    Vec::new()
}

fn prompt_edit_targets_from_arguments_json(tool_name: &str, arguments_json: &str) -> Vec<String> {
    let Ok(arguments): Result<Value, _> = serde_json::from_str(arguments_json) else {
        return Vec::new();
    };
    let targets = match tool_name {
        "write" => arguments
            .get("path")
            .and_then(Value::as_str)
            .map(|path| vec![path.to_string()])
            .unwrap_or_default(),
        "apply_patch" => arguments
            .get("patch_text")
            .and_then(Value::as_str)
            .map(extract_patch_targets)
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut seen = BTreeSet::new();
    targets
        .into_iter()
        .map(|target| normalize_prompt_target(&target))
        .filter(|target| !target.is_empty() && seen.insert(target.clone()))
        .collect()
}

fn changed_artifact_targets_after_latest_user(
    transcript: &Transcript,
    start_index: usize,
) -> Vec<String> {
    let Some(latest_user) = latest_user_index(transcript, start_index) else {
        return Vec::new();
    };

    let mut targets = Vec::new();
    for message in &transcript.messages[latest_user + 1..] {
        for part in &message.parts {
            if let MessagePart::DiffSummary(value) = &part.payload {
                targets.extend(extract_requested_artifact_targets(&value.summary));
            }
        }
    }
    dedupe_targets(targets)
}

fn staged_task_output_targets_read_after_latest_user(
    transcript: &Transcript,
    start_index: usize,
    required_targets: &[String],
) -> bool {
    if required_targets.is_empty() {
        return false;
    }
    let Some(latest_user) = latest_user_index(transcript, start_index) else {
        return false;
    };

    let mut readonly_targets_by_call = HashMap::new();
    for message in &transcript.messages[latest_user + 1..] {
        for part in &message.parts {
            if let MessagePart::ToolCall(value) = &part.payload {
                if let Some(target) =
                    extract_readonly_target(&value.tool_name.to_string(), &value.arguments_json)
                {
                    readonly_targets_by_call.insert(value.tool_call_id.to_string(), target);
                }
            }
        }
    }

    let mut successful_reads = BTreeSet::new();
    for message in &transcript.messages[latest_user + 1..] {
        for part in &message.parts {
            if let MessagePart::ToolResult(value) = &part.payload {
                if value.status != ToolCallStatus::Completed {
                    continue;
                }
                let Some(target) = readonly_targets_by_call.get(&value.tool_call_id.to_string())
                else {
                    continue;
                };
                for required in required_targets {
                    if prompt_target_matches_required_output(target, std::slice::from_ref(required))
                    {
                        successful_reads.insert(normalize_prompt_target(required));
                    }
                }
            }
        }
    }

    required_targets
        .iter()
        .all(|target| successful_reads.contains(&normalize_prompt_target(target)))
}

fn staged_task_output_targets_changed_after_latest_user(
    transcript: &Transcript,
    start_index: usize,
    required_targets: &[String],
) -> bool {
    if required_targets.is_empty() {
        return false;
    }

    let changed_targets = changed_artifact_targets_after_latest_user(transcript, start_index);
    required_targets.iter().all(|required| {
        changed_targets.iter().any(|target| {
            prompt_target_matches_required_output(target, std::slice::from_ref(required))
        })
    })
}

fn staged_task_documentation_closeout_mode(
    todos: &[TodoItem],
    staged_task_active: bool,
    required_targets: &[String],
    output_targets_already_generated: bool,
) -> bool {
    let _ = todos;
    if !staged_task_active || !staged_task_documentation_outputs_only(required_targets) {
        return false;
    }
    output_targets_already_generated
}

fn staged_task_documentation_outputs_only(required_targets: &[String]) -> bool {
    !required_targets.is_empty()
        && required_targets
            .iter()
            .all(|target| classify_artifact_target(target) == ArtifactTargetKind::Documentation)
}

fn staged_task_documentation_focus_targets(execution_focus_targets: &[String]) -> Vec<String> {
    let mut has_implementation = false;
    let mut documentation_targets = Vec::new();

    for target in execution_focus_targets {
        match classify_artifact_target(target) {
            ArtifactTargetKind::Documentation => documentation_targets.push(target.clone()),
            ArtifactTargetKind::Implementation => has_implementation = true,
            ArtifactTargetKind::Unknown => {}
        }
    }

    if has_implementation {
        return Vec::new();
    }

    dedupe_targets(documentation_targets)
}

fn staged_task_documentation_authoring_active(
    required_targets: &[String],
    execution_focus_targets: &[String],
) -> bool {
    let focus_targets = staged_task_documentation_focus_targets(execution_focus_targets);
    if focus_targets.is_empty() {
        return false;
    }

    if required_targets.is_empty() {
        return true;
    }

    focus_targets.iter().any(|target| {
        required_targets.iter().any(|required| {
            prompt_target_matches_required_output(target, std::slice::from_ref(required))
        })
    })
}

fn staged_task_documentation_evidence_snapshot(
    transcript: &Transcript,
    start_index: usize,
    documentation_targets: &[String],
) -> Option<String> {
    let focus_targets = staged_task_documentation_focus_targets(documentation_targets);
    if focus_targets.is_empty() {
        return None;
    }
    let Some(latest_user) = latest_user_index(transcript, start_index) else {
        return None;
    };

    let mut readonly_targets_by_call = HashMap::new();
    let mut readonly_tool_names_by_call = HashMap::new();
    for message in &transcript.messages[latest_user + 1..] {
        for part in &message.parts {
            if let MessagePart::ToolCall(value) = &part.payload {
                if let Some(target) =
                    extract_readonly_target(&value.tool_name.to_string(), &value.arguments_json)
                {
                    readonly_targets_by_call.insert(value.tool_call_id.to_string(), target);
                    readonly_tool_names_by_call
                        .insert(value.tool_call_id.to_string(), value.tool_name.to_string());
                }
            }
        }
    }

    let mut seen = BTreeSet::new();
    let mut lines = Vec::new();
    'outer: for message in &transcript.messages[latest_user + 1..] {
        for part in &message.parts {
            let MessagePart::ToolResult(value) = &part.payload else {
                continue;
            };
            if value.status != ToolCallStatus::Completed
                || value.summary.trim().is_empty()
                || prompt_tool_result_is_nonprogress(value)
            {
                continue;
            }
            let tool_call_id = value.tool_call_id.to_string();
            let Some(target) = readonly_targets_by_call.get(&tool_call_id) else {
                continue;
            };
            let Some(tool_name) = readonly_tool_names_by_call.get(&tool_call_id) else {
                continue;
            };
            if let Some(line) = staged_task_documentation_evidence_line(
                tool_name,
                target,
                &value.summary,
                &focus_targets,
            ) {
                if seen.insert(line.clone()) {
                    lines.push(line);
                    if lines.len() >= STAGED_TASK_EVIDENCE_LINE_LIMIT {
                        break 'outer;
                    }
                }
            }
        }
    }

    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn staged_task_documentation_evidence_line(
    tool_name: &str,
    target: &str,
    summary: &str,
    required_targets: &[String],
) -> Option<String> {
    let normalized = normalize_prompt_target(target);
    if normalized.is_empty()
        || is_noise_only_prompt_documentation_target(&normalized)
        || is_staged_task_artifact_target(&normalized)
        || prompt_target_matches_required_output(&normalized, required_targets)
    {
        return None;
    }

    let detail = match tool_name {
        "list" | "inspect_directory" => summarize_staged_task_list_evidence(summary),
        "read" => summarize_staged_task_read_evidence(&normalized, summary),
        _ => None,
    }
    .unwrap_or_else(|| "inspected successfully".to_string());

    Some(format!("- `{normalized}`: {detail}"))
}

fn summarize_staged_task_list_evidence(summary: &str) -> Option<String> {
    let items = summary
        .lines()
        .map(|line| line.trim())
        .filter(|line| {
            !line.is_empty()
                && !line.contains("Major repository areas present:")
                && !line.starts_with("Use this top-level overview")
                && !line.starts_with("Survey coverage so far:")
                && !line.starts_with("Still missing:")
                && !line.starts_with("Inspect one of these next:")
                && !line.contains("does not exist yet")
                && !line.contains("already been converted into the runtime contract")
        })
        .filter(|line| {
            !line.split('/').any(|segment| {
                matches!(
                    segment,
                    ".venv"
                        | ".pytest_cache"
                        | "__pycache__"
                        | "node_modules"
                        | ".next"
                        | "playwright-report"
                        | "test-results"
                )
            })
        })
        .take(STAGED_TASK_LIST_PREVIEW_LIMIT)
        .map(str::to_string)
        .collect::<Vec<_>>();

    (!items.is_empty()).then(|| format!("contains {}", items.join(", ")))
}

fn summarize_staged_task_read_evidence(target: &str, summary: &str) -> Option<String> {
    let lower = target.to_ascii_lowercase();
    if lower.ends_with("frontend/package.json") {
        let mut details = Vec::new();
        if summary.contains("\"next\"") {
            details.push("Next.js");
        }
        if summary.contains("\"react\"") {
            details.push("React");
        }
        if summary.contains("\"test\"") {
            details.push("frontend test script");
        }
        if summary.contains("\"test:e2e\"") || summary.contains("playwright") {
            details.push("Playwright e2e");
        }
        return (!details.is_empty()).then(|| details.join(", "));
    }
    if lower.ends_with("backend/pyproject.toml") {
        let mut details = Vec::new();
        if summary.contains("requires-python") {
            details.push("Python project metadata");
        }
        if summary.to_ascii_lowercase().contains("fastapi") {
            details.push("FastAPI");
        }
        if summary.to_ascii_lowercase().contains("pydantic") {
            details.push("Pydantic");
        }
        if summary.to_ascii_lowercase().contains("sqlalchemy") {
            details.push("SQLAlchemy");
        }
        if summary.to_ascii_lowercase().contains("pytest") {
            details.push("pytest");
        }
        if summary.to_ascii_lowercase().contains("pymupdf") {
            details.push("PyMuPDF");
        }
        return (!details.is_empty()).then(|| details.join(", "));
    }
    if lower.ends_with("backend/app/core/config.py") {
        let mut details = Vec::new();
        for marker in [
            "database_url",
            "memory_storage_path",
            "run_artifact_root",
            "report_output_dir",
            "document_storage_path",
            "scenario_template_dir",
            "llm_provider",
            "default_locale",
        ] {
            if summary.contains(marker) {
                details.push(marker);
            }
        }
        return (!details.is_empty()).then(|| format!("settings include {}", details.join(", ")));
    }

    let lines = summary
        .lines()
        .filter_map(strip_numbered_summary_line)
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with('#')
                && !line.starts_with('{')
                && !line.starts_with('}')
                && !line.starts_with('[')
                && !line.starts_with(']')
        })
        .filter(|line| {
            line.contains("FastAPI")
                || line.contains("APIRouter")
                || line.contains("router")
                || line.contains("include_router")
                || line.starts_with("class ")
                || line.starts_with("def ")
                || line.starts_with("export ")
                || line.starts_with("\"name\"")
        })
        .take(STAGED_TASK_READ_PREVIEW_LIMIT)
        .map(str::to_string)
        .collect::<Vec<_>>();

    (!lines.is_empty()).then(|| lines.join(" | "))
}

fn strip_numbered_summary_line(line: &str) -> Option<&str> {
    let (prefix, rest) = line.split_once(':')?;
    prefix.trim().parse::<usize>().ok()?;
    Some(rest.trim())
}

fn is_noise_only_prompt_documentation_target(target: &str) -> bool {
    target.split('/').any(|segment| {
        matches!(
            segment,
            ".venv"
                | ".pytest_cache"
                | "__pycache__"
                | "node_modules"
                | ".next"
                | "playwright-report"
                | "test-results"
        )
    })
}

fn latest_summary_index(transcript: &Transcript) -> Option<usize> {
    transcript
        .messages
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| match &message.record.metadata {
            MessageMetadata::Assistant(meta) if meta.summary => Some(index),
            _ => None,
        })
}

fn latest_summary_before_user_index(transcript: &Transcript, latest_user: usize) -> Option<usize> {
    transcript.messages[..latest_user]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| match &message.record.metadata {
            MessageMetadata::Assistant(meta) if meta.summary => Some(index),
            _ => None,
        })
}

fn prompt_window_start_index(transcript: &Transcript) -> usize {
    let latest_summary = latest_summary_index(transcript);
    let latest_user = latest_user_index(transcript, 0);
    match (latest_summary, latest_user) {
        (Some(summary), Some(user)) if summary >= user => user,
        (Some(summary), _) => summary + 1,
        (None, _) => 0,
    }
}

fn exact_write_target_contract(target: &str) -> String {
    let content_contract = if target_is_test_like(target) {
        if let Some(contract) =
            crate::agent::content_shape_contract::python_source_for_test_target(target)
        {
            return contract.prompt_contract();
        }
        "The `content` must be a test module for the active target only: import the production module, define unittest or pytest-style tests, and assert the requested behavior. Do not define production functions, CLI entrypoints, or paste implementation code from a completed source file."
    } else if target_is_documentation_like(target) {
        "The `content` must be Markdown documentation/design text for the active target only. Do not write Python, Rust, JavaScript, imports, functions, CLI loops, or paste implementation code from a completed source file."
    } else if target_is_python_source_like(target) {
        "The `content` must be complete Python source code for the active implementation target only. Do not write tests, Markdown, or a different deliverable."
    } else {
        "The `content` must be complete final contents for the active target only. Do not paste content from a completed or inactive target."
    };
    format!(
        "Active write target contract:\n- Use the `write` tool with `path` set to `{target}` and `content` set to the complete replacement content for that file.\n- The provider-visible tool schema remains the stable `write` interface; target validation belongs to the tool lifecycle for the submitted call.\n- {content_contract}\n- Older assistant narration, previous tool arguments, and prior progress output are not tool-call authority for this turn."
    )
}

fn format_todo_focus(todos: &[TodoItem]) -> String {
    let open_count = todos.iter().filter(|todo| todo.status.is_open()).count();
    let blocked = todos
        .iter()
        .filter(|todo| matches!(todo.status, crate::session::TodoStatus::Blocked))
        .collect::<Vec<_>>();
    let active = current_active_todo_item(todos);
    let mut lines = vec![format!("Open work items: {open_count}")];

    if let Some(todo) = active {
        lines.push(format!("Active: {}", format_todo_focus_line(todo)));
    }
    if !blocked.is_empty() {
        let blocked_line = blocked
            .iter()
            .take(TODO_FOCUS_BLOCKED_PREVIEW_LIMIT)
            .map(|todo| format_todo_focus_line(todo))
            .collect::<Vec<_>>()
            .join(" | ");
        lines.push(format!("Blocked: {blocked_line}"));
        if blocked.len() > TODO_FOCUS_BLOCKED_PREVIEW_LIMIT {
            lines.push(format!(
                "Blocked more: {}",
                blocked.len() - TODO_FOCUS_BLOCKED_PREVIEW_LIMIT
            ));
        }
    }

    let remaining = todos
        .iter()
        .filter(|todo| {
            todo.status.is_open()
                && Some(todo.id) != active.map(|value| value.id)
                && !matches!(todo.status, crate::session::TodoStatus::Blocked)
        })
        .take(TODO_FOCUS_NEXT_PREVIEW_LIMIT)
        .map(format_todo_focus_line)
        .collect::<Vec<_>>();
    if !remaining.is_empty() {
        lines.push(format!("Next: {}", remaining.join(" | ")));
    }

    lines.join("\n")
}

fn format_todo_focus_line(todo: &TodoItem) -> String {
    let mut line = format!(
        "{} [{} / {}]",
        todo.content,
        todo_status_label(todo),
        todo_priority_label(todo)
    );
    if !matches!(todo.kind, crate::session::TodoKind::Work) {
        line.push_str(&format!(" <{}>", todo_kind_label(todo)));
    }
    if !todo.targets.is_empty() {
        let targets = todo
            .targets
            .iter()
            .take(TODO_FOCUS_TARGET_PREVIEW_LIMIT)
            .map(|value| value.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        line.push_str(&format!(" targets: {targets}"));
        if todo.targets.len() > TODO_FOCUS_TARGET_PREVIEW_LIMIT {
            line.push_str(&format!(
                " (+{} more)",
                todo.targets.len() - TODO_FOCUS_TARGET_PREVIEW_LIMIT
            ));
        }
    }
    if matches!(todo.status, crate::session::TodoStatus::Blocked) && !todo.blocked_by.is_empty() {
        line.push_str(&format!(" because {}", todo.blocked_by.join("; ")));
    }
    line
}

fn current_active_todo_item(todos: &[TodoItem]) -> Option<&TodoItem> {
    todos
        .iter()
        .find(|todo| matches!(todo.status, crate::session::TodoStatus::InProgress))
        .or_else(|| {
            todos.iter().find(|todo| {
                matches!(
                    todo.status,
                    crate::session::TodoStatus::Pending | crate::session::TodoStatus::Blocked
                )
            })
        })
}

fn todo_status_label(todo: &TodoItem) -> &'static str {
    match todo.status {
        crate::session::TodoStatus::Pending => "pending",
        crate::session::TodoStatus::InProgress => "in_progress",
        crate::session::TodoStatus::Blocked => "blocked",
        crate::session::TodoStatus::Completed => "completed",
        crate::session::TodoStatus::Cancelled => "cancelled",
    }
}

fn todo_kind_label(todo: &TodoItem) -> &'static str {
    match todo.kind {
        crate::session::TodoKind::Work => "work",
        crate::session::TodoKind::Verification => "verification",
        crate::session::TodoKind::Repair => "repair",
        crate::session::TodoKind::Completion => "completion",
    }
}

fn todo_priority_label(todo: &TodoItem) -> &'static str {
    match todo.priority {
        crate::session::TodoPriority::High => "high",
        crate::session::TodoPriority::Medium => "medium",
        crate::session::TodoPriority::Low => "low",
    }
}

fn normalize_prompt_target(target: &str) -> String {
    target.trim().replace('\\', "/")
}

fn prompt_target_matches_required_output(target: &str, required_targets: &[String]) -> bool {
    let normalized_target = normalize_prompt_target(target).to_ascii_lowercase();
    required_targets.iter().any(|required| {
        let normalized_required = normalize_prompt_target(required).to_ascii_lowercase();
        normalized_target == normalized_required
            || normalized_target.ends_with(&format!("/{normalized_required}"))
    })
}

fn collect_instruction_sources(
    cwd: &Utf8Path,
    root: &Utf8Path,
    route: TaskRoute,
    additional: &[Utf8PathBuf],
) -> Result<Vec<Utf8PathBuf>, AgentError> {
    let mut ancestry = Vec::new();
    let mut current = Some(cwd);
    while let Some(dir) = current {
        ancestry.push(dir.to_path_buf());
        if dir == root {
            break;
        }
        current = dir.parent();
    }
    ancestry.reverse();

    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for dir in ancestry {
        for file_name in instruction_file_names() {
            let candidate = dir.join(file_name);
            if candidate.exists() && seen.insert(candidate.clone()) {
                selected.push(candidate);
            }
        }
    }
    for candidate in discover_rule_files(root, route)? {
        if seen.insert(candidate.clone()) {
            selected.push(candidate);
        }
    }

    for path in additional {
        let candidate = if path.is_absolute() {
            path.clone()
        } else {
            root.join(path)
        };
        if candidate.exists() && seen.insert(candidate.clone()) {
            selected.push(candidate);
        }
    }

    Ok(selected)
}

fn render_instruction_text(
    cwd: &Utf8Path,
    root: &Utf8Path,
    route: TaskRoute,
    sources: &[Utf8PathBuf],
    additional: &[Utf8PathBuf],
) -> String {
    let prioritized = prioritize_instruction_sources(cwd, root, route, sources, additional);
    let mut remaining = MAX_TOTAL_INSTRUCTION_CHARS;
    let mut rendered = Vec::new();

    for entry in prioritized {
        if remaining < INSTRUCTION_RENDER_STOP_THRESHOLD_CHARS {
            break;
        }
        let Ok(text) = fs::read_to_string(&entry.path) else {
            continue;
        };
        let per_file_budget = match entry.priority {
            0 | 1 => MAX_PRIMARY_INSTRUCTION_CHARS,
            2 | 3 => MAX_SECONDARY_INSTRUCTION_CHARS,
            _ => MAX_TERTIARY_INSTRUCTION_CHARS,
        }
        .min(remaining);
        let Some(block) = render_instruction_block(&entry.path, &text, entry.mode, per_file_budget)
        else {
            continue;
        };
        remaining = remaining.saturating_sub(block.len());
        rendered.push(block);
    }

    rendered.join("\n\n")
}

fn prioritize_instruction_sources(
    cwd: &Utf8Path,
    root: &Utf8Path,
    route: TaskRoute,
    sources: &[Utf8PathBuf],
    additional: &[Utf8PathBuf],
) -> Vec<InstructionSourceEntry> {
    let resolved_additional = additional
        .iter()
        .map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                root.join(path)
            }
        })
        .collect::<BTreeSet<_>>();
    let root_primary_instruction = instruction_file_names()
        .iter()
        .filter(|file_name| !file_name.eq_ignore_ascii_case("CLAUDE.md"))
        .map(|file_name| root.join(file_name))
        .find(|candidate| candidate.exists());
    let nearest_primary_instruction = nearest_instruction_file(
        cwd,
        root,
        &instruction_file_names()
            .iter()
            .copied()
            .filter(|file_name| !file_name.eq_ignore_ascii_case("CLAUDE.md"))
            .collect::<Vec<_>>(),
    );
    let nearest_claude = nearest_instruction_file(cwd, root, &["CLAUDE.md"]);
    let route_rule_dir = root.join(format!(".moyai/rules-{}", task_route_dir(route)));
    let shared_rule_dir = root.join(".moyai/rules");

    let mut entries = sources
        .iter()
        .cloned()
        .enumerate()
        .map(|(discovery_index, path)| {
            let filename = path.file_name().unwrap_or_default();
            let priority = if root_primary_instruction.as_ref() == Some(&path) {
                0
            } else if nearest_primary_instruction.as_ref() == Some(&path) {
                1
            } else if resolved_additional.contains(&path) {
                2
            } else if path.starts_with(&route_rule_dir) {
                3
            } else if path.starts_with(&shared_rule_dir) {
                4
            } else if nearest_claude.as_ref() == Some(&path) {
                5
            } else if instruction_file_names()
                .iter()
                .filter(|file_name| !file_name.eq_ignore_ascii_case("CLAUDE.md"))
                .any(|file_name| filename.eq_ignore_ascii_case(file_name))
            {
                6
            } else if filename.eq_ignore_ascii_case("CLAUDE.md") {
                7
            } else {
                8
            };
            let mode = if priority <= 4 {
                InstructionRenderMode::Full
            } else {
                InstructionRenderMode::Summary
            };
            InstructionSourceEntry {
                path,
                priority,
                mode,
                discovery_index,
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (entry.priority, entry.discovery_index));
    entries
}

fn discover_rule_files(root: &Utf8Path, route: TaskRoute) -> Result<Vec<Utf8PathBuf>, AgentError> {
    let mut files = Vec::new();
    for dir in [
        root.join(".moyai/rules"),
        root.join(format!(".moyai/rules-{}", task_route_dir(route))),
    ] {
        if !dir.exists() {
            continue;
        }
        let mut dir_files = fs::read_dir(dir.as_std_path())
            .map_err(|error| {
                AgentError::Message(format!("failed to read rules directory `{dir}`: {error}"))
            })?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .filter_map(|path| Utf8PathBuf::from_path_buf(path).ok())
            .filter(|path| {
                path.extension()
                    .map(|value| value.eq_ignore_ascii_case("md"))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        dir_files.sort();
        files.extend(dir_files);
    }
    Ok(files)
}

fn task_route_dir(route: TaskRoute) -> &'static str {
    match route {
        TaskRoute::Code => "code",
        TaskRoute::Docs => "docs",
        TaskRoute::Review => "review",
        TaskRoute::Debug => "debug",
        TaskRoute::Ask => "ask",
        TaskRoute::Summary => "summary",
    }
}

fn nearest_instruction_file(
    cwd: &Utf8Path,
    root: &Utf8Path,
    file_names: &[&str],
) -> Option<Utf8PathBuf> {
    let mut current = Some(cwd);
    while let Some(dir) = current {
        for file_name in file_names {
            let candidate = dir.join(file_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        if dir == root {
            break;
        }
        current = dir.parent();
    }
    None
}

fn render_instruction_block(
    path: &Utf8Path,
    text: &str,
    mode: InstructionRenderMode,
    budget: usize,
) -> Option<String> {
    let header = match mode {
        InstructionRenderMode::Full => format!("## {path}"),
        InstructionRenderMode::Summary => format!("## {path} (summary)"),
    };
    let body_budget = budget.saturating_sub(header.len() + 1);
    if body_budget < 40 {
        return None;
    }

    let body = match mode {
        InstructionRenderMode::Full => clip_instruction_text(text, body_budget, false),
        InstructionRenderMode::Summary => clip_instruction_text(text, body_budget, true),
    };
    if body.is_empty() {
        return None;
    }
    Some(format!("{header}\n{body}"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn stale_inactive_authoring_replay_live_builder_fixture_passes() {
        assert!(super::stale_inactive_authoring_replay_uses_live_builder());
    }

    #[test]
    fn stale_progress_projection_replay_live_builder_fixture_passes() {
        assert!(super::stale_progress_projection_replay_uses_live_builder());
    }

    #[test]
    fn failed_inactive_authoring_replay_live_builder_fixture_passes() {
        assert!(super::failed_inactive_authoring_replay_uses_call_scoped_summary());
    }
}

fn clip_instruction_text(text: &str, max_chars: usize, summary_mode: bool) -> String {
    let source_lines = if summary_mode {
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .take(INSTRUCTION_SUMMARY_MAX_LINES)
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
    } else {
        text.lines()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
    };

    let mut lines = Vec::new();
    let mut used = 0usize;
    for line in source_lines {
        let extra = if lines.is_empty() {
            line.len()
        } else {
            line.len() + 1
        };
        if used + extra > max_chars.saturating_sub(INSTRUCTION_TRUNCATION_RESERVE_CHARS) {
            break;
        }
        used += extra;
        lines.push(line);
    }

    if lines.is_empty() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        lines.push(
            trimmed
                .chars()
                .take(max_chars.saturating_sub(INSTRUCTION_TRUNCATION_RESERVE_CHARS))
                .collect::<String>(),
        );
    }

    let mut clipped = lines.join("\n");
    if text.len() > clipped.len() {
        if !clipped.ends_with('\n') {
            clipped.push('\n');
        }
        clipped.push_str("[truncated]");
    }
    clipped
}
