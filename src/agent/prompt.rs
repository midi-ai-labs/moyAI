use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::agent::language_evidence::{
    ArtifactRole, LanguageFamily, classify_artifact_target as classify_language_artifact_target,
    language_failure_labels_from_summary,
};
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
    project_model_turn_state, render_active_work_contract, render_model_turn_state,
    structured_document_summary_snapshot_from_history_items,
};
use crate::agent::verification::{
    explicit_verification_commands_from_text,
    latest_failed_verification_preceding_repair_targets_from_history_items,
    latest_verification_repair_cycle_from_history_items, looks_like_verification_command,
    verification_evidence_after_latest_user_with_freshness,
    verification_freshness_targets_after_latest_user, verification_requirements,
};
use crate::config::{AgentConfig, PromptProfile, ResolvedConfig, ShellFamily};
use crate::edit::PatchParser;
use crate::error::AgentError;
use crate::llm::{ModelContentPart, ModelMessage, ModelProfile, ModelToolCall, ToolSchema};
use crate::protocol::{
    ContentPart, HistoryItem, HistoryItemId, HistoryItemPayload, RequiredAction,
    ToolLifecycleStatus, canonical_tool_call_arguments,
};
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
    pub protocol_turn_id: crate::protocol::TurnId,
    pub runtime_input: RuntimeInputView,
    pub state: SessionStateSnapshot,
    pub config: ResolvedConfig,
    pub model: ModelProfile,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone)]
pub struct RuntimeInputView {
    pub history_items: Vec<HistoryItem>,
}

impl RuntimeInputView {
    pub fn from_history_items(history_items: Vec<HistoryItem>) -> Self {
        Self { history_items }
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

#[derive(Debug, Clone, Copy)]
struct PromptProjectionInput<'a> {
    workspace_cwd: &'a Utf8Path,
}

impl<'a> PromptProjectionInput<'a> {
    fn from_session(session: &'a SessionRecord) -> Self {
        Self {
            workspace_cwd: session.cwd.as_path(),
        }
    }
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
const PROMPT_FIXTURE_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const PROMPT_FIXTURE_BASE_URL: &str = "http://127.0.0.1:1234";

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
    staged_task_recovery_stall: bool,
    inactive_target_edit_recovery_mode: bool,
    inactive_target_edit_recovery_targets: Vec<String>,
    inactive_target_edit_recovery_read_target: Option<String>,
    edit_recovery_mode: bool,
    patch_recovery_mode: bool,
    patch_recovery_targets: Vec<String>,
    verification_failure_repair_mode: bool,
    verification_repair_rerun_due: bool,
    verification_pending_without_open_work: bool,
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
    workspace_root: Option<Utf8PathBuf>,
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
    fn from_state(state: &SessionStateSnapshot, workspace_root: Option<&Utf8Path>) -> Self {
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
            workspace_root: workspace_root.map(Utf8Path::to_path_buf),
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
        let prompt_input = PromptProjectionInput::from_session(&request.session.session);
        let active_work = active_work_contract_for_history_items(
            &request.session.session,
            &request.runtime_input.history_items,
            &request.state,
            todos,
        );
        let mut signals = detect_prompt_signals_with_config(
            prompt_input,
            &request.runtime_input.history_items,
            todos,
            &request.config.agent,
            Some(&request.state),
        );
        apply_state_driven_signal_overrides(
            &mut signals,
            prompt_input,
            &request.runtime_input.history_items,
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
                &request.runtime_input.history_items,
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
            prompt_input,
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
    crate::agent::lifecycle_kernel::TurnLifecycleKernel::apply_codex_style_provider_edit_surface(
        &mut tools, state,
    );
    apply_active_content_shape_to_write_schema(&mut tools, state);
    apply_active_target_to_apply_patch_schema(&mut tools, state);
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

fn apply_active_target_to_apply_patch_schema(
    tools: &mut [ToolSchema],
    state: &SessionStateSnapshot,
) {
    let Some(target) = active_authoring_schema_target(state) else {
        return;
    };
    let Some(tool) = tools.iter_mut().find(|tool| tool.name == "apply_patch") else {
        return;
    };
    let Some(patch_text) = tool.input_schema.pointer_mut("/properties/patch_text") else {
        return;
    };
    let Some(schema) = patch_text.as_object_mut() else {
        return;
    };
    let base = schema
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let shape =
        crate::agent::content_shape_contract::artifact_content_shape_tool_schema_description(
            &target,
        )
        .unwrap_or_else(|| {
            "Required positive artifact shape: complete target file content with real newlines."
                .to_string()
        });
    schema.insert(
        "description".to_string(),
        Value::String(format!(
            "{base} Current active target-only patch skeleton: `*** Begin Patch\n*** Add File: {target}\n+<complete content for {target}>\n*** End Patch`; if `{target}` already exists, use one `*** Update File: {target}` operation instead. The patch_text must contain exactly one file operation for `{target}`, exactly one final `*** End Patch`, and no inactive target hunks. {shape}"
        )),
    );
}

fn active_authoring_schema_target(state: &SessionStateSnapshot) -> Option<String> {
    if let Some(target) = exact_active_authoring_write_required(state) {
        return Some(target);
    }
    if !matches!(
        state.process_phase,
        ProcessPhase::Author | ProcessPhase::Repair
    ) || state.completion.open_work_count == 0
        || state.active_targets.len() != 1
    {
        return None;
    }
    let target = state.active_targets.first()?.as_str().trim();
    (!target.is_empty()).then(|| target.to_string())
}

fn repair_lane_projection_for_prompt(
    state: &SessionStateSnapshot,
) -> Option<crate::agent::repair_lane::RepairLaneProjection> {
    let allowed_tools = ["write", "apply_patch", "todowrite"]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    project_repair_lane(state, &allowed_tools)
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
    prompt_input: PromptProjectionInput<'_>,
    history_items: &[HistoryItem],
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
        && latest_verification_repair_cycle_from_history_items(
            history_items,
            prompt_history_window_start_index(history_items),
            prompt_input.workspace_cwd,
        )
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
    let reason = reason.to_ascii_lowercase();
    reason.contains("requested deliverables are still missing from the workspace")
        || reason.contains("requested deliverables still require authoring in the workspace")
}

fn build_messages_with_state(
    prompt_input: PromptProjectionInput<'_>,
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
    let canonical_history_items = canonical_history_items_for_projection(history_items);
    let history_items = canonical_history_items.as_ref();
    let history_start_index = prompt_history_window_start_index(history_items);
    let latest_user_text = latest_user_text_from_history_items(history_items, history_start_index);
    let (superseded_denied_tools, _) = superseded_tool_denial_replay_state_from_history(
        history_items,
        history_start_index,
        tool_names,
    );
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
    if let Some(contract) = latest_content_shape_repair_contract_for_prompt(history_items, state) {
        result.push(ModelMessage::System { content: contract });
    }
    if let Some(contract) = active_work {
        result.push(ModelMessage::System {
            content: render_active_work_contract(contract),
        });
    }
    if let Some(rejection_summary) =
        latest_wrong_authoring_target_rejection_for_prompt(history_items, history_start_index)
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
                inactive_target_recovery_authoring_tool(tool_names).as_deref(),
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
        if let Some(content) = render_structured_document_summary_progress(
            prompt_input.workspace_cwd,
            history_items,
            latest_user_text.as_deref(),
        ) {
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
    let replay_context = ProviderReplayContext::from_state(state, Some(prompt_input.workspace_cwd));
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
    let canonical_history_items = canonical_history_items_for_projection(history_items);
    let history_items = canonical_history_items.as_ref();
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
    let consumed_supporting_context_calls =
        consumed_supporting_context_tool_call_targets_after_content_shape_rejection(
            history_items,
            replay_start,
            replay_context,
            &tool_output_index,
        );
    let target_exclusive_contract_violation_calls =
        target_exclusive_apply_patch_contract_violation_calls_after(
            history_items,
            replay_start,
            replay_context,
            &tool_output_index,
        );
    let suppressed_tool_call_ids = replay_prelude_suppressed_tool_call_ids(
        &stale_inactive_authoring_calls,
        &historical_progress_projection_calls,
        &consumed_supporting_context_calls,
        &target_exclusive_contract_violation_calls,
    );
    let suppressed_prelude_indices = stale_history_tool_call_prelude_indices(
        history_items,
        replay_start,
        &suppressed_tool_call_ids,
    );
    let suppress_inactive_filechange_references =
        inactive_content_shape_recovery_requires_target_exclusive_replay_after(
            history_items,
            replay_start,
            replay_context,
            &tool_output_index,
        );
    if !suppress_inactive_filechange_references {
        for (target, note) in
            inactive_filechange_reference_notes_after(history_items, replay_start, replay_context)
        {
            replay_policies.push(inactive_filechange_reference_snapshot_policy(
                &target,
                &replay_context.active_authoring_targets,
            ));
            result.push(ModelMessage::System { content: note });
        }
    }
    let mut emitted_outputs = BTreeSet::new();
    let mut emitted_tool_calls = BTreeSet::new();

    for index in selected_indices {
        if suppressed_prelude_indices.contains(&index) {
            continue;
        }
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
            HistoryItemPayload::SteerTurn {
                content,
                additional_context,
                ..
            } => {
                if !additional_context.is_empty() {
                    result.push(ModelMessage::System {
                        content: steer_additional_context_note(additional_context),
                    });
                }
                if let Some(message) = content_parts_to_user_message(content, &None) {
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
                let content_shape_mismatch = matches!(tool, ToolName::Write | ToolName::ApplyPatch)
                    && tool_call_has_content_shape_mismatch_output(
                        history_items,
                        &tool_output_index,
                        &call_id_text,
                    );
                let malformed_edit_arguments =
                    matches!(tool, ToolName::Write | ToolName::ApplyPatch)
                        && tool_call_has_invalid_edit_arguments_output(
                            history_items,
                            &tool_output_index,
                            &call_id_text,
                        );
                let mixed_target_invalid_edit_arguments =
                    tool_call_has_mixed_target_invalid_edit_arguments_output(
                        history_items,
                        &tool_output_index,
                        &call_id_text,
                    );
                let inactive_content_shape_targets =
                    tool_call_has_inactive_target_content_shape_mismatch_output(
                        history_items,
                        &tool_output_index,
                        &call_id_text,
                        &replay_context.active_authoring_targets,
                    );
                let target_exclusive_contract_violation =
                    tool_call_has_target_exclusive_apply_patch_contract_violation_output(
                        history_items,
                        &tool_output_index,
                        &call_id_text,
                    );
                let arguments_json = if let Some(stale_targets) =
                    inactive_content_shape_targets.as_ref()
                {
                    replay_policies.push(inactive_target_content_shape_replay_policy(
                        &call_id_text,
                        tool,
                        stale_targets,
                        &replay_context.active_authoring_targets,
                    ));
                    result.push(ModelMessage::System {
                        content: inactive_target_content_shape_pair_replay_note(
                            &call_id_text,
                            tool,
                            &replay_context.active_authoring_targets,
                        ),
                    });
                    continue;
                } else if let Some(violation_targets) = target_exclusive_contract_violation.as_ref()
                {
                    replay_policies.push(target_exclusive_contract_violation_replay_policy(
                        &call_id_text,
                        tool,
                        violation_targets,
                        &replay_context.active_authoring_targets,
                    ));
                    result.push(ModelMessage::System {
                        content: target_exclusive_contract_violation_pair_replay_note(
                            &call_id_text,
                            tool,
                            violation_targets,
                            &replay_context.active_authoring_targets,
                        ),
                    });
                    continue;
                } else if content_shape_mismatch {
                    sanitized_content_shape_mismatch_arguments_json(tool)
                } else if let Some(mixed) = mixed_target_invalid_edit_arguments.as_ref() {
                    replay_policies.push(mixed_target_invalid_edit_replay_policy(
                        &call_id_text,
                        tool,
                        mixed,
                        &replay_context.active_authoring_targets,
                    ));
                    result.push(ModelMessage::System {
                        content: mixed_target_invalid_edit_pair_replay_note(
                            &call_id_text,
                            tool,
                            mixed,
                            &replay_context.active_authoring_targets,
                        ),
                    });
                    continue;
                } else if malformed_edit_arguments {
                    replay_policies.push(malformed_edit_arguments_replay_policy(
                        &call_id_text,
                        tool,
                        &replay_context.active_authoring_targets,
                    ));
                    sanitized_malformed_edit_arguments_json(tool)
                } else if let Some(stale_targets) =
                    failed_inactive_authoring_calls.get(&call_id_text)
                {
                    replay_policies.push(failed_inactive_authoring_replay_policy(
                        &call_id_text,
                        tool,
                        stale_targets,
                        &replay_context.active_authoring_targets,
                    ));
                    result.push(ModelMessage::System {
                        content: failed_inactive_authoring_pair_replay_note(
                            &call_id_text,
                            tool,
                            stale_targets,
                            &replay_context.active_authoring_targets,
                            tool_output_text_for_call(
                                history_items,
                                &tool_output_index,
                                &call_id_text,
                            )
                            .as_deref(),
                        ),
                    });
                    continue;
                } else if let Some(stale_targets) =
                    stale_inactive_authoring_calls.get(&call_id_text)
                {
                    let arguments_json =
                        replay_tool_arguments_json(arguments, model_arguments, effective_arguments);
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
                            inactive_authoring_reference_snapshot(
                                history_items,
                                &call_id_text,
                                tool,
                                &arguments_json,
                                stale_targets,
                                replay_context.workspace_root.as_deref(),
                            )
                            .as_deref(),
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
                } else if let Some(context_target) =
                    consumed_supporting_context_calls.get(&call_id_text)
                {
                    replay_policies.push(consumed_supporting_context_replay_policy(
                        &call_id_text,
                        tool,
                        context_target,
                        &replay_context.active_authoring_targets,
                    ));
                    result.push(ModelMessage::System {
                        content: consumed_supporting_context_pair_replay_note(
                            context_target,
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
                        metadata: Value::Null,
                    });
                }
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                output_text,
                metadata,
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
                } else if consumed_supporting_context_calls.contains_key(&call_id_text) {
                    continue;
                } else {
                    output_text.clone()
                };
                result.push(ModelMessage::Tool {
                    call_id: call_id_text,
                    tool_name: tool_name.clone(),
                    result: result_text,
                    metadata: metadata.clone(),
                });
            }
            HistoryItemPayload::RejectedToolProposal { proposal } => {
                if proposal.semantic_class == "text_final_while_obligations_open" {
                    replay_policies.push(rejected_final_assistant_message_replay_policy(
                        proposal,
                        &replay_context.active_authoring_targets,
                    ));
                    result.push(ModelMessage::System {
                        content: rejected_model_action_replay_note(proposal),
                    });
                }
            }
            HistoryItemPayload::Reasoning { .. }
            | HistoryItemPayload::CandidateRepairEdit { .. }
            | HistoryItemPayload::RequestDiagnostics { .. }
            | HistoryItemPayload::Continuation { .. }
            | HistoryItemPayload::StateProjection { .. }
            | HistoryItemPayload::SessionState { .. }
            | HistoryItemPayload::LifecycleGuard { .. }
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

fn canonical_history_items_for_projection(history_items: &[HistoryItem]) -> Cow<'_, [HistoryItem]> {
    if history_items_in_canonical_order(history_items) {
        return Cow::Borrowed(history_items);
    }
    let mut ordered = history_items.to_vec();
    ordered.sort_by_key(history_item_order_key);
    Cow::Owned(ordered)
}

fn history_items_in_canonical_order(history_items: &[HistoryItem]) -> bool {
    history_items
        .windows(2)
        .all(|pair| history_item_order_key(&pair[0]) <= history_item_order_key(&pair[1]))
}

fn history_item_order_key(item: &HistoryItem) -> (i64, i64) {
    (item.sequence_no, item.created_at_ms)
}

pub(crate) fn provider_replay_sequence_order_resists_timestamp_drift_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let assistant_item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 1,
        payload: HistoryItemPayload::Message {
            message_id: None,
            role: MessageRole::Assistant,
            content: vec![ContentPart::Text {
                text: "assistant-after-user".to_string(),
            }],
        },
    };
    let user_item = HistoryItem {
        id: HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 9_999,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "provider replay canonical sequence order".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    };

    let items = vec![assistant_item, user_item];
    let ordered = canonical_history_items_for_projection(&items);
    ordered
        .iter()
        .map(|item| item.sequence_no)
        .collect::<Vec<_>>()
        == vec![1, 2]
}

#[cfg(test)]
pub(crate) fn provider_replay_includes_active_turn_steer_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "active steer replay fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 3,
        completed_at_ms: None,
    };
    let items = vec![
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Start the requested implementation.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::SteerTurn {
                expected_turn_id: turn_id,
                content: vec![ContentPart::Text {
                    text: "Steer: include the running-turn update before continuing.".to_string(),
                }],
                additional_context: BTreeMap::from([(
                    "desktop.composer".to_string(),
                    crate::protocol::AdditionalContextEntry {
                        value: "submitted while the turn was running".to_string(),
                        kind: crate::protocol::AdditionalContextKind::Application,
                    },
                )]),
                client_user_message_id: Some("client-steer-fixture".to_string()),
            },
        },
    ];

    let messages = build_provider_replay_messages_from_history_items(&session, &items, 10);
    let has_initial = messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::User { content } if content.contains("requested implementation")
        )
    });
    let has_context = messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::System { content }
                if content.contains("Active-turn steer additional context")
                    && content.contains("desktop.composer")
        )
    });
    let has_steer = messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::User { content } if content.contains("running-turn update")
        )
    });
    has_initial && has_context && has_steer
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
                    | HistoryItemPayload::SteerTurn { .. }
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
                    | HistoryItemPayload::SteerTurn { .. }
                    | HistoryItemPayload::Message {
                        role: MessageRole::User,
                        ..
                    }
            )
            .then_some(offset)
        })
}

fn provider_replay_item_is_visible(payload: &HistoryItemPayload) -> bool {
    payload.is_provider_replay_candidate()
}

fn rejected_model_action_replay_note(proposal: &crate::protocol::RejectedToolProposal) -> String {
    let allowed = proposal
        .allowed_surface
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Rejected model action evidence: `{}` was rejected as `{}` before turn completion. Reason: {} Allowed tool surface: [{}]. Projection id: {}. Payload hash: {}. This item is no-progress lifecycle evidence; continue by using an allowed tool under the current TurnControlEnvelope, not by repeating a final answer.",
        proposal.effective_tool,
        proposal.semantic_class,
        proposal.blocked_reason,
        allowed,
        proposal.projection_id,
        proposal.payload_hash,
    )
}

fn steer_additional_context_note(
    additional_context: &BTreeMap<String, crate::protocol::AdditionalContextEntry>,
) -> String {
    let mut lines = vec![
        "Active-turn steer additional context follows. Treat application entries as trusted UI/session context and untrusted entries as user-supplied context.".to_string(),
    ];
    for (key, entry) in additional_context {
        lines.push(format!("- {key} ({:?}): {}", entry.kind, entry.value));
    }
    lines.join("\n")
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

fn replay_prelude_suppressed_tool_call_ids(
    stale_inactive_authoring_calls: &BTreeMap<String, Vec<String>>,
    historical_progress_projection_calls: &BTreeMap<String, Vec<String>>,
    consumed_supporting_context_calls: &BTreeMap<String, String>,
    target_exclusive_contract_violation_calls: &BTreeMap<String, Vec<String>>,
) -> BTreeSet<String> {
    stale_inactive_authoring_calls
        .keys()
        .chain(historical_progress_projection_calls.keys())
        .chain(consumed_supporting_context_calls.keys())
        .chain(target_exclusive_contract_violation_calls.keys())
        .cloned()
        .collect()
}

fn stale_history_tool_call_prelude_indices(
    history_items: &[HistoryItem],
    start: usize,
    stale_tool_call_ids: &BTreeSet<String>,
) -> BTreeSet<usize> {
    let mut indices = BTreeSet::new();
    if stale_tool_call_ids.is_empty() {
        return indices;
    }
    for (index, item) in history_items.iter().enumerate().skip(start) {
        if !history_item_has_suppressed_tool_call(item, stale_tool_call_ids) {
            continue;
        }
        let mut cursor = index;
        while cursor > start {
            cursor -= 1;
            let candidate = &history_items[cursor];
            if !history_item_is_stale_tool_call_prelude(candidate) {
                break;
            }
            indices.insert(cursor);
        }
    }
    indices
}

fn history_item_has_suppressed_tool_call(
    item: &HistoryItem,
    stale_tool_call_ids: &BTreeSet<String>,
) -> bool {
    matches!(
        &item.payload,
        HistoryItemPayload::ToolCall { call_id, .. }
            if stale_tool_call_ids.contains(&call_id.to_string())
    )
}

fn history_item_is_stale_tool_call_prelude(item: &HistoryItem) -> bool {
    matches!(
        &item.payload,
        HistoryItemPayload::Message {
            role: MessageRole::Assistant,
            content,
            ..
        } if content_parts_text(content).is_some()
            && content
                .iter()
                .all(|part| matches!(part, ContentPart::Text { .. }))
    )
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
    match history_items.get(*index).map(|item| &item.payload) {
        Some(HistoryItemPayload::ToolOutput { metadata, .. }) => {
            tool_output_payload_is_content_shape_mismatch(metadata)
        }
        _ => false,
    }
}

fn tool_output_payload_is_content_shape_mismatch(metadata: &Value) -> bool {
    metadata_operation_progress_class_is_content_shape_mismatch(metadata)
        || metadata
            .get("tool_feedback_envelope")
            .is_some_and(metadata_operation_progress_class_is_content_shape_mismatch)
        || metadata
            .get("tool_feedback_envelope")
            .and_then(|feedback| feedback.get("kind"))
            .and_then(Value::as_str)
            .is_some_and(|kind| {
                matches!(
                    kind,
                    "required_write_content_shape_mismatch"
                        | "artifact_content_shape_violation"
                        | "artifact_content_shape_no_progress"
                )
            })
}

fn metadata_operation_progress_class_is_content_shape_mismatch(metadata: &Value) -> bool {
    metadata
        .get("operation_progress_class")
        .and_then(Value::as_str)
        .is_some_and(|class| {
            matches!(
                class,
                "required_write_content_shape_mismatch"
                    | "artifact_content_shape_violation"
                    | "artifact_content_shape_no_progress"
            )
        })
}

fn tool_call_has_target_exclusive_apply_patch_contract_violation_output(
    history_items: &[HistoryItem],
    output_index: &BTreeMap<String, usize>,
    call_id: &str,
) -> Option<Vec<String>> {
    let index = output_index.get(call_id)?;
    let item = history_items.get(*index)?;
    let HistoryItemPayload::ToolOutput { metadata, .. } = &item.payload else {
        return None;
    };
    if !tool_output_payload_is_target_exclusive_apply_patch_contract_violation(metadata) {
        return None;
    }
    let mut targets = metadata_replay_string_array(metadata, "inactive_submitted_targets");
    if targets.is_empty() {
        targets = metadata_replay_string_array(metadata, "submitted_targets");
    }
    Some(targets)
}

fn tool_output_payload_is_target_exclusive_apply_patch_contract_violation(
    metadata: &Value,
) -> bool {
    metadata_operation_progress_class_is_target_exclusive_apply_patch_contract_violation(metadata)
        || metadata.get("tool_feedback_envelope").is_some_and(
            metadata_operation_progress_class_is_target_exclusive_apply_patch_contract_violation,
        )
        || metadata
            .get("tool_feedback_envelope")
            .and_then(|feedback| feedback.get("kind"))
            .and_then(Value::as_str)
            == Some("target_exclusive_apply_patch_contract_violation")
}

fn metadata_operation_progress_class_is_target_exclusive_apply_patch_contract_violation(
    metadata: &Value,
) -> bool {
    metadata
        .get("operation_progress_class")
        .and_then(Value::as_str)
        == Some("target_exclusive_apply_patch_contract_violation")
}

fn tool_call_has_invalid_edit_arguments_output(
    history_items: &[HistoryItem],
    output_index: &BTreeMap<String, usize>,
    call_id: &str,
) -> bool {
    let Some(index) = output_index.get(call_id) else {
        return false;
    };
    match history_items.get(*index).map(|item| &item.payload) {
        Some(HistoryItemPayload::ToolOutput { metadata, .. }) => {
            metadata
                .get("operation_progress_class")
                .and_then(Value::as_str)
                == Some("invalid_edit_arguments")
                || metadata
                    .get("tool_feedback_envelope")
                    .and_then(|feedback| feedback.get("operation_progress_class"))
                    .and_then(Value::as_str)
                    == Some("invalid_edit_arguments")
                || metadata
                    .get("tool_feedback_envelope")
                    .and_then(|feedback| feedback.get("kind"))
                    .and_then(Value::as_str)
                    == Some("invalid_edit_arguments")
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MixedTargetInvalidEditReplay {
    active_submitted_targets: Vec<String>,
    inactive_submitted_targets: Vec<String>,
}

fn tool_call_has_mixed_target_invalid_edit_arguments_output(
    history_items: &[HistoryItem],
    output_index: &BTreeMap<String, usize>,
    call_id: &str,
) -> Option<MixedTargetInvalidEditReplay> {
    let index = output_index.get(call_id)?;
    let item = history_items.get(*index)?;
    let HistoryItemPayload::ToolOutput { metadata, .. } = &item.payload else {
        return None;
    };
    if !tool_call_has_invalid_edit_arguments_output(history_items, output_index, call_id) {
        return None;
    }
    let active_submitted_targets =
        metadata_replay_string_array(metadata, "active_submitted_targets");
    let inactive_submitted_targets =
        metadata_replay_string_array(metadata, "inactive_submitted_targets");
    (!active_submitted_targets.is_empty() && !inactive_submitted_targets.is_empty()).then_some(
        MixedTargetInvalidEditReplay {
            active_submitted_targets,
            inactive_submitted_targets,
        },
    )
}

fn metadata_replay_string_array(metadata: &Value, key: &str) -> Vec<String> {
    metadata
        .get("tool_feedback_envelope")
        .and_then(|feedback| feedback.get(key))
        .or_else(|| metadata.get(key))
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

fn tool_call_has_inactive_target_content_shape_mismatch_output(
    history_items: &[HistoryItem],
    output_index: &BTreeMap<String, usize>,
    call_id: &str,
    active_targets: &[String],
) -> Option<Vec<String>> {
    if active_targets.is_empty() {
        return None;
    }
    let index = output_index.get(call_id)?;
    let item = history_items.get(*index)?;
    let HistoryItemPayload::ToolOutput { metadata, .. } = &item.payload else {
        return None;
    };
    if !tool_output_payload_is_content_shape_mismatch(metadata) {
        return None;
    }
    let submitted_targets = metadata_replay_string_array(metadata, "submitted_targets");
    if submitted_targets.is_empty()
        || submitted_targets_intersect_active(&submitted_targets, active_targets)
    {
        return None;
    }
    Some(submitted_targets)
}

fn inactive_content_shape_recovery_requires_target_exclusive_replay_after(
    history_items: &[HistoryItem],
    start: usize,
    replay_context: &ProviderReplayContext,
    output_index: &BTreeMap<String, usize>,
) -> bool {
    if replay_context.active_authoring_targets.is_empty() {
        return false;
    }
    history_items.iter().skip(start).any(|item| {
        let HistoryItemPayload::ToolCall { call_id, .. } = &item.payload else {
            return false;
        };
        tool_call_has_inactive_target_content_shape_mismatch_output(
            history_items,
            output_index,
            &call_id.to_string(),
            &replay_context.active_authoring_targets,
        )
        .is_some()
    })
}

fn target_exclusive_apply_patch_contract_violation_calls_after(
    history_items: &[HistoryItem],
    start: usize,
    replay_context: &ProviderReplayContext,
    output_index: &BTreeMap<String, usize>,
) -> BTreeMap<String, Vec<String>> {
    if replay_context.active_authoring_targets.is_empty() {
        return BTreeMap::new();
    }
    let mut calls = BTreeMap::new();
    for item in history_items.iter().skip(start) {
        let HistoryItemPayload::ToolCall { call_id, tool, .. } = &item.payload else {
            continue;
        };
        if tool != &ToolName::ApplyPatch {
            continue;
        }
        let call_id_text = call_id.to_string();
        if let Some(targets) = tool_call_has_target_exclusive_apply_patch_contract_violation_output(
            history_items,
            output_index,
            &call_id_text,
        ) {
            calls.insert(call_id_text, targets);
        }
    }
    calls
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
        Some(HistoryItemPayload::ToolOutput { metadata, .. }) => {
            metadata
                .get("operation_progress_class")
                .and_then(Value::as_str)
                == Some("wrong_authoring_target")
                || metadata
                    .get("tool_feedback_envelope")
                    .and_then(|feedback| feedback.get("operation_progress_class"))
                    .and_then(Value::as_str)
                    == Some("wrong_authoring_target")
                || metadata
                    .get("tool_feedback_envelope")
                    .and_then(|feedback| feedback.get("kind"))
                    .and_then(Value::as_str)
                    == Some("wrong_authoring_target")
        }
        _ => false,
    }
}

fn sanitized_content_shape_mismatch_arguments_json(tool: &ToolName) -> String {
    match tool {
        ToolName::ApplyPatch => json!({
            "patch_text": "[omitted incompatible patch payload; runtime rejected it before side effects. See the following tool result for the required content contract and active target.]"
        })
        .to_string(),
        _ => json!({
            "content": "[omitted incompatible write payload; runtime rejected it before side effects. See the following tool result for the required content contract.]"
        })
        .to_string(),
    }
}

fn sanitized_malformed_edit_arguments_json(tool: &ToolName) -> String {
    match tool {
        ToolName::ApplyPatch => json!({
            "patch_text": "[omitted malformed edit payload; runtime rejected it before side effects. See the following tool result for parser error, active target, and required edit action.]"
        })
        .to_string(),
        _ => json!({
            "content": "[omitted malformed edit payload; runtime rejected it before side effects. See the following tool result for parser error, active target, and required edit action.]"
        })
        .to_string(),
    }
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
            if let Some(continuation) = continuation {
                sections.push(format!(
                    "Typed continuation contract:\n{}",
                    serde_json::to_string(continuation)
                        .unwrap_or_else(|_| "unserializable continuation".to_string())
                ));
            }
            if !summary.trim().is_empty() {
                sections.push(format!(
                    "Conversation summary from earlier turns:\n{summary}"
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
    let value = crate::protocol::canonical_tool_call_arguments(
        arguments,
        model_arguments,
        effective_arguments,
    );
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

fn consumed_supporting_context_tool_call_targets_after_content_shape_rejection(
    history_items: &[HistoryItem],
    start: usize,
    replay_context: &ProviderReplayContext,
    output_index: &BTreeMap<String, usize>,
) -> BTreeMap<String, String> {
    if replay_context.active_authoring_targets.is_empty() {
        return BTreeMap::new();
    }
    let Some(latest_rejection_index) =
        latest_content_shape_rejection_for_active_target(history_items, start, replay_context)
    else {
        return BTreeMap::new();
    };

    let mut consumed_calls = BTreeMap::new();
    for item in history_items
        .iter()
        .take(latest_rejection_index)
        .skip(start)
    {
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
        if !matches!(
            tool,
            ToolName::Read
                | ToolName::List
                | ToolName::Glob
                | ToolName::Grep
                | ToolName::InspectDirectory
        ) {
            continue;
        }
        let call_id_text = call_id.to_string();
        let Some(output_position) = output_index.get(&call_id_text).copied() else {
            continue;
        };
        if output_position >= latest_rejection_index {
            continue;
        }
        if !tool_output_is_successful_supporting_context(history_items, output_position) {
            continue;
        }
        let arguments_json =
            replay_tool_arguments_json(arguments, model_arguments, effective_arguments);
        let Some(target) = extract_readonly_target(&tool.to_string(), &arguments_json) else {
            continue;
        };
        let normalized = normalize_prompt_target(&target);
        if normalized.is_empty()
            || prompt_target_matches_required_output(
                &normalized,
                &replay_context.active_authoring_targets,
            )
        {
            continue;
        }
        consumed_calls.insert(call_id_text, normalized);
    }
    consumed_calls
}

fn latest_content_shape_rejection_for_active_target(
    history_items: &[HistoryItem],
    start: usize,
    replay_context: &ProviderReplayContext,
) -> Option<usize> {
    let mut tool_targets = BTreeMap::new();
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
        tool_targets.insert(
            call_id.to_string(),
            artifact_targets_from_tool_call(&tool.to_string(), &arguments_json),
        );
    }

    history_items
        .iter()
        .enumerate()
        .skip(start)
        .rev()
        .find_map(|(index, item)| {
            let HistoryItemPayload::ToolOutput {
                call_id, metadata, ..
            } = &item.payload
            else {
                return None;
            };
            if !tool_output_payload_is_content_shape_mismatch(metadata) {
                return None;
            }
            let targets = tool_targets.get(&call_id.to_string())?;
            targets
                .iter()
                .any(|target| {
                    prompt_target_matches_required_output(
                        target,
                        &replay_context.active_authoring_targets,
                    )
                })
                .then_some(index)
        })
}

fn tool_output_is_successful_supporting_context(
    history_items: &[HistoryItem],
    output_position: usize,
) -> bool {
    matches!(
        history_items.get(output_position).map(|item| &item.payload),
        Some(HistoryItemPayload::ToolOutput {
            status: crate::protocol::ToolLifecycleStatus::Completed,
            success,
            metadata,
            ..
        }) if success != &Some(false)
            && crate::agent::lifecycle_kernel::provider_replay_metadata_is_supporting_context(
                metadata
            )
    )
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
    let Some(HistoryItemPayload::ToolOutput { metadata, .. }) =
        history_items.get(*index).map(|item| &item.payload)
    else {
        return false;
    };
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
    let is_current_no_progress_projection = metadata_progress_class == Some("progress_projection")
        && metadata_progress_effect == Some("no_progress");
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
        metadata_targets
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
    reference_snapshot: Option<&str>,
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
    let mut note = format!(
        "Previous authoring tool call/output pair for inactive target(s) {stale} is omitted from executable provider tool-call history because the current active requested-work target set is {active}. Treat this as non-executable historical context; use the current active-work projection and stable tool schema."
    );
    if let Some(snapshot) = reference_snapshot.filter(|value| !value.trim().is_empty()) {
        note.push_str("\nReference-only accepted artifact snapshot for omitted inactive target. Do not rewrite this inactive target to satisfy progress; use it only as context for the current active target.\n");
        note.push_str(snapshot);
    }
    note
}

fn failed_inactive_authoring_pair_replay_note(
    call_id: &str,
    tool: &ToolName,
    stale_targets: &[String],
    active_targets: &[String],
    tool_output_text: Option<&str>,
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
    let mut note = format!(
        "Prior failed wrong-target authoring tool call/output `{call_id}` for `{tool}` targeting inactive target(s) {stale} is omitted from executable provider tool-call history because the current active requested-work target set is {active}. Treat this as non-executable historical feedback; do not replay, repair, or continue the omitted call. Use the current TurnControlEnvelope, active-work projection, and stable tool schema for the active target."
    );
    if let Some(output) = tool_output_text.filter(|value| !value.trim().is_empty()) {
        note.push_str("\nRejected call feedback summary:\n");
        note.push_str(output.trim());
    }
    note
}

fn tool_output_text_for_call(
    history_items: &[HistoryItem],
    output_index: &BTreeMap<String, usize>,
    call_id: &str,
) -> Option<String> {
    let index = *output_index.get(call_id)?;
    let item = history_items.get(index)?;
    match &item.payload {
        HistoryItemPayload::ToolOutput { output_text, .. } => Some(output_text.clone()),
        _ => None,
    }
}

fn inactive_filechange_reference_notes_after(
    history_items: &[HistoryItem],
    start: usize,
    replay_context: &ProviderReplayContext,
) -> Vec<(String, String)> {
    let active_targets = &replay_context.active_authoring_targets;
    if active_targets.is_empty() {
        return Vec::new();
    }
    let active = active_targets
        .iter()
        .map(|target| format!("`{target}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut notes = BTreeMap::<String, String>::new();
    for item in history_items.iter().skip(start) {
        let HistoryItemPayload::FileChange { changes, .. } = &item.payload else {
            continue;
        };
        for change in changes {
            let Some(path) = change
                .path_after
                .as_ref()
                .or(change.path_before.as_ref())
                .map(|path| normalize_prompt_target(path.as_str()))
            else {
                continue;
            };
            if path.is_empty()
                || active_targets.iter().any(|active_target| {
                    prompt_targets_have_exact_normalized_identity(
                        &path,
                        active_target,
                        replay_context.workspace_root.as_deref(),
                    )
                })
            {
                continue;
            }
            let display_path = provider_visible_reference_path(&path);
            let note = format!(
                "Reference-only accepted artifact snapshot for inactive target.\nartifact_path: `{}`\nThis artifact already exists from accepted FileChange evidence and must not be rewritten to satisfy progress. Current active requested-work target set is {active}. Do not rewrite this inactive target; use it only as context for the current active target.\nsummary: {}",
                display_path, change.summary
            );
            notes.insert(display_path, note);
        }
    }
    notes.into_iter().collect()
}

fn provider_visible_reference_path(path: &str) -> String {
    let normalized = normalize_prompt_target(path);
    if normalized.is_empty() {
        return normalized;
    }
    if let Some((_, after_workspace)) = normalized.rsplit_once("/workspace/") {
        let trimmed = after_workspace.trim_start_matches('/');
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let is_absolute = normalized.starts_with('/')
        || normalized
            .as_bytes()
            .get(1)
            .is_some_and(|value| *value == b':');
    if is_absolute {
        return normalized
            .rsplit('/')
            .next()
            .filter(|value| !value.is_empty())
            .unwrap_or(normalized.as_str())
            .to_string();
    }
    normalized
}

fn inactive_authoring_reference_snapshot(
    history_items: &[HistoryItem],
    call_id: &str,
    tool: &ToolName,
    arguments_json: &str,
    stale_targets: &[String],
    workspace_root: Option<&Utf8Path>,
) -> Option<String> {
    if tool == &ToolName::Write {
        return inactive_write_reference_snapshot(arguments_json, stale_targets, workspace_root);
    }
    inactive_filechange_reference_snapshot(history_items, call_id, stale_targets, workspace_root)
}

fn inactive_write_reference_snapshot(
    arguments_json: &str,
    stale_targets: &[String],
    workspace_root: Option<&Utf8Path>,
) -> Option<String> {
    let value = serde_json::from_str::<Value>(arguments_json).ok()?;
    let path = value.get("path").and_then(Value::as_str)?.trim();
    let content = value.get("content").and_then(Value::as_str)?;
    if path.is_empty()
        || content.trim().is_empty()
        || !stale_targets.iter().any(|target| {
            prompt_targets_have_exact_normalized_identity(path, target, workspace_root)
        })
    {
        return None;
    }
    let clipped = clip_reference_snapshot(content, 2400);
    Some(format!("artifact_path: `{path}`\n```text\n{clipped}\n```"))
}

fn inactive_filechange_reference_snapshot(
    history_items: &[HistoryItem],
    call_id: &str,
    stale_targets: &[String],
    workspace_root: Option<&Utf8Path>,
) -> Option<String> {
    history_items.iter().find_map(|item| {
        let HistoryItemPayload::FileChange {
            call_id: filechange_call_id,
            changes,
            ..
        } = &item.payload
        else {
            return None;
        };
        if filechange_call_id.to_string() != call_id {
            return None;
        }
        changes.iter().find_map(|change| {
            let target = change
                .path_after
                .as_ref()
                .or(change.path_before.as_ref())
                .and_then(|path| {
                    stale_target_for_path(path.as_str(), stale_targets, workspace_root)
                })?;
            let display_target = provider_visible_reference_path(&target);
            Some(format!(
                "artifact_path: `{display_target}`\nsummary: {}",
                change.summary
            ))
        })
    })
}

fn stale_target_for_path(
    path: &str,
    stale_targets: &[String],
    workspace_root: Option<&Utf8Path>,
) -> Option<String> {
    let normalized = normalize_prompt_target(path);
    stale_targets
        .iter()
        .find(|target| {
            prompt_targets_have_exact_normalized_identity(&normalized, target, workspace_root)
        })
        .cloned()
}

fn prompt_targets_have_exact_normalized_identity(
    left: &str,
    right: &str,
    workspace_root: Option<&Utf8Path>,
) -> bool {
    let left_keys = prompt_exact_normalized_target_identity_keys(left, workspace_root);
    let right_keys = prompt_exact_normalized_target_identity_keys(right, workspace_root);
    !left_keys.is_empty()
        && left_keys
            .iter()
            .any(|left_key| right_keys.iter().any(|right_key| right_key == left_key))
}

fn prompt_exact_normalized_target_identity_keys(
    target: &str,
    workspace_root: Option<&Utf8Path>,
) -> Vec<String> {
    let normalized = normalize_prompt_target(target);
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut keys = vec![normalized];
    if let Some(workspace_root) = workspace_root
        && let Some(relative) = crate::workspace::project::workspace_relative_key_for_match(
            target,
            workspace_root.as_str(),
        )
    {
        keys.push(relative);
    }
    keys.sort();
    keys.dedup();
    keys
}

pub(crate) fn provider_replay_inactive_filechange_exact_target_identity_fixture_passes() -> bool {
    let workspace_root = Utf8PathBuf::from("C:/workspace/project");
    let active_target = "src/workflow.rs";
    let same_workspace_absolute = "C:/workspace/project/src/workflow.rs";
    let sibling_workspace_absolute = "C:/workspace/other/src/workflow.rs";

    let same_workspace_matches = prompt_targets_have_exact_normalized_identity(
        same_workspace_absolute,
        active_target,
        Some(workspace_root.as_path()),
    );
    let sibling_workspace_rejected = !prompt_targets_have_exact_normalized_identity(
        sibling_workspace_absolute,
        active_target,
        Some(workspace_root.as_path()),
    );
    let stale_targets = vec![active_target.to_string()];
    let same_workspace_stale_target = stale_target_for_path(
        same_workspace_absolute,
        &stale_targets,
        Some(workspace_root.as_path()),
    );
    let sibling_workspace_stale_target = stale_target_for_path(
        sibling_workspace_absolute,
        &stale_targets,
        Some(workspace_root.as_path()),
    );

    same_workspace_matches
        && sibling_workspace_rejected
        && same_workspace_stale_target.as_deref() == Some(active_target)
        && sibling_workspace_stale_target.is_none()
}

fn clip_reference_snapshot(content: &str, limit: usize) -> String {
    let normalized = content.replace("\r\n", "\n");
    if normalized.chars().count() <= limit {
        return normalized;
    }
    let mut clipped = normalized.chars().take(limit).collect::<String>();
    clipped.push_str("\n[reference snapshot truncated]");
    clipped
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

fn consumed_supporting_context_pair_replay_note(
    context_target: &str,
    active_targets: &[String],
) -> String {
    let active = active_targets
        .iter()
        .map(|target| format!("`{target}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Prior supporting-context tool call/output for `{context_target}` is omitted from executable provider tool-call history because the current exact repair target is {active}. Treat that content as already-consumed evidence; do not call read/list/search again for it. Use the provider-visible edit tool, usually apply_patch, for the active target."
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

fn inactive_filechange_reference_snapshot_policy(
    target: &str,
    active_targets: &[String],
) -> RequestReplayPolicyDiagnostic {
    RequestReplayPolicyDiagnostic {
        policy: "inactive_filechange_reference_snapshot_projected".to_string(),
        call_id: None,
        tool_name: None,
        omitted_targets: vec![target.to_string()],
        active_targets: active_targets.to_vec(),
        reason: "accepted inactive FileChange evidence is projected as non-executable provider-visible context even when its original content-changing tool call is not replayable under the current effective surface".to_string(),
    }
}

fn failed_inactive_authoring_replay_policy(
    call_id: &str,
    tool: &ToolName,
    stale_targets: &[String],
    active_targets: &[String],
) -> RequestReplayPolicyDiagnostic {
    RequestReplayPolicyDiagnostic {
        policy: "failed_inactive_authoring_executable_pair_omitted".to_string(),
        call_id: Some(call_id.to_string()),
        tool_name: Some(tool.to_string()),
        omitted_targets: stale_targets.to_vec(),
        active_targets: active_targets.to_vec(),
        reason: "failed wrong-target authoring remains in canonical event history, but its executable ToolCall/ToolOutput pair is omitted from provider replay after active target rotation and replaced with call-id-scoped non-executable feedback".to_string(),
    }
}

fn inactive_target_content_shape_replay_policy(
    call_id: &str,
    tool: &ToolName,
    stale_targets: &[String],
    active_targets: &[String],
) -> RequestReplayPolicyDiagnostic {
    RequestReplayPolicyDiagnostic {
        policy: "inactive_target_content_shape_executable_pair_omitted".to_string(),
        call_id: Some(call_id.to_string()),
        tool_name: Some(tool.to_string()),
        omitted_targets: stale_targets.to_vec(),
        active_targets: active_targets.to_vec(),
        reason: "content-shape mismatch for an inactive submitted target is preserved as typed non-executable evidence, while the executable ToolCall/ToolOutput pair is omitted so recovery stays target-exclusive for the active artifact".to_string(),
    }
}

fn mixed_target_invalid_edit_replay_policy(
    call_id: &str,
    tool: &ToolName,
    mixed: &MixedTargetInvalidEditReplay,
    active_targets: &[String],
) -> RequestReplayPolicyDiagnostic {
    RequestReplayPolicyDiagnostic {
        policy: "mixed_target_invalid_edit_executable_pair_omitted".to_string(),
        call_id: Some(call_id.to_string()),
        tool_name: Some(tool.to_string()),
        omitted_targets: mixed.inactive_submitted_targets.clone(),
        active_targets: active_targets.to_vec(),
        reason: "mixed-target invalid edit arguments are preserved as typed failure evidence, but the executable ToolCall/ToolOutput pair is omitted from provider replay so the next turn sees only the current target-exclusive edit surface".to_string(),
    }
}

fn malformed_edit_arguments_replay_policy(
    call_id: &str,
    tool: &ToolName,
    active_targets: &[String],
) -> RequestReplayPolicyDiagnostic {
    RequestReplayPolicyDiagnostic {
        policy: "malformed_edit_arguments_payload_sanitized_output_preserved".to_string(),
        call_id: Some(call_id.to_string()),
        tool_name: Some(tool.to_string()),
        omitted_targets: Vec::new(),
        active_targets: active_targets.to_vec(),
        reason: "malformed edit arguments are sanitized into a non-authoritative replay placeholder while the matching invalid_edit_arguments ToolOutput remains call-id-scoped provider-visible evidence".to_string(),
    }
}

fn mixed_target_invalid_edit_pair_replay_note(
    call_id: &str,
    tool: &ToolName,
    mixed: &MixedTargetInvalidEditReplay,
    active_targets: &[String],
) -> String {
    let active = active_targets
        .iter()
        .map(|target| format!("`{target}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let submitted_active = mixed
        .active_submitted_targets
        .iter()
        .map(|target| format!("`{target}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut note = format!(
        "Prior mixed-target invalid edit tool call/output `{call_id}` for `{tool}` is omitted from executable provider tool-call history. Runtime rejected it before side effects because it combined the current active submitted target(s) {submitted_active} with additional inactive hunks. Treat this as non-executable failure evidence; do not replay, repair, or continue the omitted call. The current target-exclusive requested-work set is {active}; submit a fresh target-only edit using the current TurnControlEnvelope and stable tool schema."
    );
    if let Some(skeleton) = target_only_apply_patch_recovery_skeleton(tool, active_targets) {
        note.push('\n');
        note.push_str(&skeleton);
    }
    note
}

fn inactive_target_content_shape_pair_replay_note(
    call_id: &str,
    tool: &ToolName,
    active_targets: &[String],
) -> String {
    let active = active_targets
        .iter()
        .map(|target| format!("`{target}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut note = format!(
        "Prior content-shape rejected tool call/output `{call_id}` for `{tool}` is omitted from executable provider tool-call history because it targeted an inactive artifact while the current target-exclusive requested-work set is {active}. Treat this as non-executable failure evidence; do not replay, repair, or continue the omitted call. Submit a fresh edit for the active target using the current TurnControlEnvelope and generated artifact shape."
    );
    if let Some(skeleton) = target_only_apply_patch_recovery_skeleton(tool, active_targets) {
        note.push('\n');
        note.push_str(&skeleton);
    }
    note
}

fn target_exclusive_contract_violation_replay_policy(
    call_id: &str,
    tool: &ToolName,
    omitted_targets: &[String],
    active_targets: &[String],
) -> RequestReplayPolicyDiagnostic {
    RequestReplayPolicyDiagnostic {
        policy: "target_exclusive_apply_patch_contract_violation_pair_omitted".to_string(),
        call_id: Some(call_id.to_string()),
        tool_name: Some(tool.to_string()),
        omitted_targets: omitted_targets.to_vec(),
        active_targets: active_targets.to_vec(),
        reason: "canonical target-exclusive apply_patch violation ToolCall/ToolOutput items are preserved, but the malformed or inactive-target patch payload is omitted from executable provider replay while exact active-target recovery remains open".to_string(),
    }
}

fn target_exclusive_contract_violation_pair_replay_note(
    call_id: &str,
    tool: &ToolName,
    omitted_targets: &[String],
    active_targets: &[String],
) -> String {
    let omitted = omitted_targets
        .iter()
        .map(|target| format!("`{target}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let active = active_targets
        .iter()
        .map(|target| format!("`{target}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let omitted_clause = if omitted.is_empty() {
        "with malformed target-exclusive patch shape".to_string()
    } else {
        format!("with inactive or non-admitted target evidence {omitted}")
    };
    let mut note = format!(
        "Prior target-exclusive apply_patch contract violation `{call_id}` for `{tool}` is omitted from executable provider tool-call history {omitted_clause}. Treat this as non-executable failed edit evidence; do not replay, repair, or continue the omitted patch body. The current target-exclusive requested-work set is {active}; submit a fresh single-operation edit for the active target using the current TurnControlEnvelope and stable tool schema."
    );
    if let Some(skeleton) = target_only_apply_patch_recovery_skeleton(tool, active_targets) {
        note.push('\n');
        note.push_str(&skeleton);
    }
    note
}

fn target_only_apply_patch_recovery_skeleton(
    tool: &ToolName,
    active_targets: &[String],
) -> Option<String> {
    if tool != &ToolName::ApplyPatch || active_targets.len() != 1 {
        return None;
    }
    let target = active_targets.first()?.trim();
    if target.is_empty() {
        return None;
    }
    Some(format!(
        "single-operation active-target patch skeleton:\n*** Begin Patch\n*** Add File: {target}\n+<complete content for {target}>\n*** End Patch\nIf `{target}` already exists, use one `*** Update File: {target}` operation instead. The fresh patch must contain exactly one file operation, exactly one final `*** End Patch`, and no inactive target hunks."
    ))
}

fn consumed_supporting_context_replay_policy(
    call_id: &str,
    tool: &ToolName,
    context_target: &str,
    active_targets: &[String],
) -> RequestReplayPolicyDiagnostic {
    RequestReplayPolicyDiagnostic {
        policy: "consumed_supporting_context_pair_omitted".to_string(),
        call_id: Some(call_id.to_string()),
        tool_name: Some(tool.to_string()),
        omitted_targets: vec![context_target.to_string()],
        active_targets: active_targets.to_vec(),
        reason: "canonical supporting-context ToolCall/ToolOutput items are preserved, but after a content-shape rejection for the active write target they are replayed as consumed evidence instead of executable provider tool-call history".to_string(),
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

fn rejected_final_assistant_message_replay_policy(
    proposal: &crate::protocol::RejectedToolProposal,
    active_targets: &[String],
) -> RequestReplayPolicyDiagnostic {
    RequestReplayPolicyDiagnostic {
        policy: "rejected_final_assistant_message_non_executable_replay".to_string(),
        call_id: Some(proposal.source_call_id.to_string()),
        tool_name: Some(proposal.effective_tool.clone()),
        omitted_targets: Vec::new(),
        active_targets: active_targets.to_vec(),
        reason: "final assistant text emitted while obligations were open is preserved as typed non-executable rejected action evidence so the next provider request keeps the active edit authority instead of treating closeout prose as progress".to_string(),
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
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let source_path =
        camino::Utf8PathBuf::from("C:/workspace/reference/workflow-visual-reference.jpg");
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
        && !rendered_text.contains("workflow-visual-reference.jpg")
        && !rendered_text.contains("C:/workspace/reference")
        && matches!(
            parts.get(2),
            Some(ModelContentPart::Image {
                mime_type,
                data_base64
            }) if mime_type == "image/jpeg" && data_base64 == "AAAA"
        )
}

pub(crate) fn provider_replay_uses_protocol_visibility_roles_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "provider visibility role fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let projection = crate::session::TurnDecisionDiagnostic {
        route: "code".to_string(),
        process_phase: "author".to_string(),
        active_work_kind: Some("projection-only".to_string()),
        active_work_summary: Some("projection-only stale target stale.rs".to_string()),
        active_targets: vec![Utf8PathBuf::from("stale.rs")],
        verification_pending: false,
        closeout_ready: false,
        required_verification_commands: Vec::new(),
        policy_targets: vec!["stale.rs".to_string()],
        allowed_tools: vec!["write".to_string()],
        tool_choice: Some("auto".to_string()),
        warnings: Vec::new(),
        repair_lane: None,
    };
    let mut state = SessionStateSnapshot::default();
    state.active_targets = vec![Utf8PathBuf::from("stale.rs")];
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
                    text: "Create active.rs".to_string(),
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
            payload: HistoryItemPayload::StateProjection { projection },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::SessionState { state },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::LifecycleGuard {
                snapshot: crate::protocol::LifecycleGuardSnapshot::default(),
            },
        },
    ];
    let messages = build_provider_replay_messages_from_history_items(&session, &items, 10);
    let rendered = messages
        .iter()
        .map(|message| match message {
            ModelMessage::System { content }
            | ModelMessage::User { content }
            | ModelMessage::Assistant { content } => content.clone(),
            ModelMessage::Tool { result, .. } => result.clone(),
            ModelMessage::UserParts { parts } => parts
                .iter()
                .filter_map(|part| match part {
                    ModelContentPart::Text { text } => Some(text.clone()),
                    ModelContentPart::Image { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
            ModelMessage::AssistantToolCalls { content, .. } => content.clone().unwrap_or_default(),
        })
        .collect::<Vec<_>>()
        .join("\n");

    messages.len() == 1 && rendered.contains("active.rs") && !rendered.contains("stale.rs")
}

pub(crate) fn provider_replay_sanitizes_content_shape_mismatch_from_typed_metadata_fixture_passes()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let patch_call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "content shape replay fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create active.test.js".to_string(),
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
                    "path": "inactive.js",
                    "content": "function staleProductionSource() { return 1; }"
                }),
                model_arguments: Value::Null,
                effective_arguments: Value::Null,
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: Vec::new(),
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
                status: ToolLifecycleStatus::Completed,
                title: "Content shape rejected by typed metadata".to_string(),
                output_text: "[tool feedback] content shape mismatch".to_string(),
                metadata: json!({
                    "tool_feedback_envelope": {
                        "operation_progress_class": "required_write_content_shape_mismatch",
                        "kind": "required_write_content_shape_mismatch"
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-content-shape-metadata".to_string()),
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
                call_id: patch_call_id,
                tool: ToolName::ApplyPatch,
                arguments: json!({
                    "patch_text": "*** Begin Patch\n*** Add File: inactive.py\n+def stalePatchProductionSource():\n+    return 1\n*** End Patch"
                }),
                model_arguments: Value::Null,
                effective_arguments: Value::Null,
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: Vec::new(),
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
                call_id: patch_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Patch content shape rejected by typed metadata".to_string(),
                output_text: "[tool feedback] patch content shape mismatch".to_string(),
                metadata: json!({
                    "tool_feedback_envelope": {
                        "operation_progress_class": "required_write_content_shape_mismatch",
                        "kind": "required_write_content_shape_mismatch"
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-patch-content-shape-metadata".to_string()),
                verification_run: None,
            },
        },
    ];
    let messages = build_provider_replay_messages_from_history_items(&session, &history_items, 20);
    let replay_json = serde_json::to_string(&messages).unwrap_or_default();
    replay_json.contains("omitted incompatible write payload")
        && replay_json.contains("omitted incompatible patch payload")
        && replay_json.contains("[tool feedback] content shape mismatch")
        && replay_json.contains("[tool feedback] patch content shape mismatch")
        && !replay_json.contains("staleProductionSource")
        && !replay_json.contains("stalePatchProductionSource")
        && session.model == PROMPT_FIXTURE_MODEL
        && session.base_url == PROMPT_FIXTURE_BASE_URL
}

pub(crate) fn provider_replay_suppresses_inactive_filechange_during_target_exclusive_content_shape_recovery_fixture_passes()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let accepted_call_id = crate::session::ToolCallId::new();
    let rejected_call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "target-exclusive content shape replay fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 6,
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
                    text: "Create calculator.py and test_calculator.py".to_string(),
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
            payload: HistoryItemPayload::FileChange {
                call_id: accepted_call_id,
                change_ids: vec![crate::session::ChangeId::new()],
                changes: vec![crate::protocol::FileChangeEvidence {
                    change_id: crate::session::ChangeId::new(),
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("calculator.py")),
                    summary: "Added production source containing def calculate".to_string(),
                }],
                summary: "accepted calculator.py source".to_string(),
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolCall {
                call_id: rejected_call_id,
                tool: ToolName::ApplyPatch,
                arguments: json!({
                    "patch_text": "*** Begin Patch\n*** Add File: calculator.py\n+def calculate(a, op, b):\n+    return a\n*** End Patch"
                }),
                model_arguments: Value::Null,
                effective_arguments: Value::Null,
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::ApplyPatch],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::ToolOutput {
                call_id: rejected_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Required write content shape mismatch".to_string(),
                output_text: "retry only test_calculator.py with positive test-module shape"
                    .to_string(),
                metadata: json!({
                    "operation_progress_class": "required_write_content_shape_mismatch",
                    "submitted_targets": ["calculator.py"],
                    "tool_feedback_envelope": {
                        "kind": "required_write_content_shape_mismatch",
                        "operation_progress_class": "required_write_content_shape_mismatch",
                        "active_targets": ["test_calculator.py"],
                        "submitted_targets": ["calculator.py"]
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-target-exclusive-content-shape".to_string()),
                verification_run: None,
            },
        },
    ];
    let replay_context = ProviderReplayContext {
        active_authoring_targets: vec!["test_calculator.py".to_string()],
        workspace_root: Some(Utf8PathBuf::from("C:/workspace/project")),
    };
    let projection = build_provider_replay_projection_from_history_items(
        &session,
        &history_items,
        20,
        &replay_context,
    );
    let rendered = serde_json::to_string(&projection.messages).unwrap_or_default();
    let policy_rendered = serde_json::to_string(&projection.replay_policies).unwrap_or_default();

    rendered.contains("test_calculator.py")
        && rendered.contains("target-exclusive requested-work set")
        && !rendered.contains("Reference-only accepted artifact snapshot for inactive target")
        && !rendered.contains("def calculate")
        && !policy_rendered.contains("inactive_filechange_reference_snapshot_projected")
        && policy_rendered.contains("inactive_target_content_shape_executable_pair_omitted")
}

pub(crate) fn provider_replay_omits_target_exclusive_apply_patch_contract_violation_fixture_passes()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "target-exclusive patch violation replay fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 4,
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
                    text: "Create source module and active verification artifact".to_string(),
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
                tool: ToolName::ApplyPatch,
                arguments: json!({
                    "patch_text": "*** Begin Patch\n*** Add File: src/workflow.rs\n+pub fn stale_production_source() {}\n*** End Patch\n*** Begin Patch\n*** Add File: tests/workflow.spec.ts\n+import { strict as assert } from 'assert';\n*** End Patch\n*** End Patch"
                }),
                model_arguments: Value::Null,
                effective_arguments: Value::Null,
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::ApplyPatch],
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
                status: ToolLifecycleStatus::Completed,
                title: "Target-exclusive apply_patch contract violation".to_string(),
                output_text: "Submit one active-target operation for tests/workflow.spec.ts"
                    .to_string(),
                metadata: json!({
                    "operation_progress_class": "target_exclusive_apply_patch_contract_violation",
                    "inactive_submitted_targets": ["src/workflow.rs"],
                    "submitted_targets": ["src/workflow.rs", "tests/workflow.spec.ts"],
                    "tool_feedback_envelope": {
                        "kind": "target_exclusive_apply_patch_contract_violation",
                        "operation_progress_class": "target_exclusive_apply_patch_contract_violation",
                        "active_targets": ["tests/workflow.spec.ts"],
                        "inactive_submitted_targets": ["src/workflow.rs"],
                        "submitted_targets": ["src/workflow.rs", "tests/workflow.spec.ts"]
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("target-exclusive-violation-fixture".to_string()),
                verification_run: None,
            },
        },
    ];
    let replay_context = ProviderReplayContext {
        active_authoring_targets: vec!["tests/workflow.spec.ts".to_string()],
        workspace_root: Some(Utf8PathBuf::from("C:/workspace/project")),
    };
    let projection = build_provider_replay_projection_from_history_items(
        &session,
        &history_items,
        20,
        &replay_context,
    );
    let rendered = serde_json::to_string(&projection.messages).unwrap_or_default();
    let policy_rendered = serde_json::to_string(&projection.replay_policies).unwrap_or_default();

    rendered.contains("tests/workflow.spec.ts")
        && rendered.contains("target-exclusive apply_patch contract violation")
        && rendered.contains("single-operation active-target patch skeleton")
        && !rendered.contains("stale_production_source")
        && !rendered.contains("AssistantToolCalls")
        && policy_rendered.contains("target_exclusive_apply_patch_contract_violation_pair_omitted")
        && policy_rendered.contains("src/workflow.rs")
}

pub fn prompt_provider_replay_fixtures_use_current_provider_profile_fixture_passes() -> bool {
    let prompt_provider_replay_fixture_current_provider_profile =
        "prompt_provider_replay_fixture_current_provider_profile";
    provider_replay_sanitizes_content_shape_mismatch_from_typed_metadata_fixture_passes()
        && provider_replay_suppresses_inactive_filechange_during_target_exclusive_content_shape_recovery_fixture_passes()
        && provider_replay_omits_target_exclusive_apply_patch_contract_violation_fixture_passes()
        && provider_replay_compaction_boundary_uses_canonical_history_order_fixture_passes()
        && stale_inactive_authoring_replay_omits_fake_executable_arguments()
        && provider_replay_preserves_failed_inactive_authoring_feedback()
        && prompt_provider_replay_fixture_current_provider_profile
            == "prompt_provider_replay_fixture_current_provider_profile"
}

pub(crate) fn prompt_projection_uses_typed_tool_output_feedback_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "typed prompt fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let user_item = HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "Create src/workflow.ts".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    };
    let typed_history = vec![
        user_item.clone(),
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                tool: ToolName::Write,
                arguments: json!({"path":"src/inactive-workflow.ts","content":"stale"}),
                model_arguments: Value::Null,
                effective_arguments: json!({"path":"src/inactive-workflow.ts","content":"stale"}),
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
            session_id: session.id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: ToolLifecycleStatus::Completed,
                title: "arbitrary displayed title".to_string(),
                output_text: "typed wrong-target output for active target src/workflow.ts"
                    .to_string(),
                metadata: json!({
                    "tool_feedback_envelope": {
                        "kind": "wrong_authoring_target",
                        "operation_progress_class": "wrong_authoring_target",
                        "progress_effect": "no_progress",
                        "active_targets": ["src/workflow.ts"],
                        "side_effects_applied": false
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("wrong-target-fixture".to_string()),
                verification_run: None,
            },
        },
    ];
    let typed_transcript = transcript_from_history_items(&session, &typed_history);
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("src/workflow.ts")];
    state.completion.open_work_count = 1;
    let agent_config = ResolvedConfig::default().agent;
    let typed_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&typed_transcript.session),
        &typed_history,
        &[],
        &agent_config,
        Some(&state),
    );

    let legacy_transcript = Transcript {
        session: session.clone(),
        messages: vec![crate::session::TranscriptMessage {
            record: crate::session::MessageRecord {
                id: crate::session::MessageId::new(),
                session_id: session.id,
                role: MessageRole::Assistant,
                parent_message_id: None,
                sequence_no: 1,
                created_at_ms: 1,
                metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                    model: PROMPT_FIXTURE_MODEL.to_string(),
                    base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
                    finish_reason: None,
                    token_usage: None,
                    summary: false,
                }),
            },
            parts: vec![crate::session::PartRecord {
                id: crate::session::PartId::new(),
                message_id: crate::session::MessageId::new(),
                sequence_no: 1,
                kind: crate::session::PartKind::ToolResult,
                payload: MessagePart::ToolResult(ToolResultPart {
                    tool_call_id: call_id,
                    status: ToolCallStatus::Completed,
                    title: "Inactive target edit blocked".to_string(),
                    summary: "legacy title-only wrong target".to_string(),
                    success: Some(false),
                    progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                    blocked_action: None,
                    result_hash: None,
                }),
            }],
        }],
    };
    let legacy_suppressed = !detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&legacy_transcript.session),
        &[user_item],
        &[],
        &agent_config,
        Some(&state),
    )
    .inactive_target_edit_recovery_mode;

    typed_signals.inactive_target_edit_recovery_mode
        && typed_signals.inactive_target_edit_recovery_targets == vec!["src/workflow.ts"]
        && latest_wrong_authoring_target_rejection_from_history(&typed_history, 0).as_deref()
            == Some("typed wrong-target output for active target src/workflow.ts")
        && legacy_suppressed
}

pub(crate) fn message_only_history_does_not_recreate_tool_lifecycle_prompt_state_fixture_passes()
-> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "message-only prompt authority fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let turn_id = crate::protocol::TurnId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let call_id = crate::session::ToolCallId::new();
    let transcript = Transcript {
        session: session.clone(),
        messages: vec![
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: user_message_id,
                    session_id: session.id,
                    role: MessageRole::User,
                    parent_message_id: None,
                    sequence_no: 1,
                    created_at_ms: 1,
                    metadata: MessageMetadata::User(crate::session::UserMessageMeta {
                        cwd: Utf8PathBuf::from("."),
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
                        text: "Create src/workflow.ts".to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: assistant_message_id,
                    session_id: session.id,
                    role: MessageRole::Assistant,
                    parent_message_id: Some(user_message_id),
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                parts: vec![crate::session::PartRecord {
                    id: crate::session::PartId::new(),
                    message_id: assistant_message_id,
                    sequence_no: 1,
                    kind: crate::session::PartKind::ToolResult,
                    payload: MessagePart::ToolResult(ToolResultPart {
                        tool_call_id: call_id,
                        status: ToolCallStatus::Completed,
                        title: "Inactive target edit blocked".to_string(),
                        summary: "legacy transcript prose claims wrong-target recovery".to_string(),
                        success: Some(false),
                        progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                        blocked_action: None,
                        result_hash: None,
                    }),
                }],
            },
        ],
    };
    let message_only_history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::Message {
                message_id: Some(user_message_id),
                role: MessageRole::User,
                content: vec![ContentPart::Text {
                    text: "Create src/workflow.ts".to_string(),
                }],
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::Message {
                message_id: Some(assistant_message_id),
                role: MessageRole::Assistant,
                content: vec![ContentPart::Text {
                    text: "Inactive target edit blocked: legacy transcript prose claims recovery"
                        .to_string(),
                }],
            },
        },
    ];
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("src/workflow.ts")];
    state.completion.open_work_count = 1;
    let agent_config = ResolvedConfig::default().agent;
    let signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &message_only_history,
        &[],
        &agent_config,
        Some(&state),
    );

    !signals.inactive_target_edit_recovery_mode
        && signals.inactive_target_edit_recovery_targets.is_empty()
        && latest_wrong_authoring_target_rejection_from_history(&message_only_history, 0).is_none()
}

pub(crate) fn verification_repair_read_budget_exhaustion_uses_typed_history_item_authority_fixture_passes()
-> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "verification read budget prompt authority fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let turn_id = crate::protocol::TurnId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let call_id = crate::session::ToolCallId::new();
    let transcript = Transcript {
        session: session.clone(),
        messages: vec![
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: user_message_id,
                    session_id: session.id,
                    role: MessageRole::User,
                    parent_message_id: None,
                    sequence_no: 1,
                    created_at_ms: 1,
                    metadata: MessageMetadata::User(crate::session::UserMessageMeta {
                        cwd: Utf8PathBuf::from("."),
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
                        text: "Fix the failing verification.".to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: assistant_message_id,
                    session_id: session.id,
                    role: MessageRole::Assistant,
                    parent_message_id: Some(user_message_id),
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                parts: vec![crate::session::PartRecord {
                    id: crate::session::PartId::new(),
                    message_id: assistant_message_id,
                    sequence_no: 1,
                    kind: crate::session::PartKind::ToolResult,
                    payload: MessagePart::ToolResult(ToolResultPart {
                        tool_call_id: call_id,
                        status: ToolCallStatus::Completed,
                        title: "Verification repair focus required".to_string(),
                        summary: "Required next `write.path`: exactly `src/workflow.ts`."
                            .to_string(),
                        success: Some(false),
                        progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                        blocked_action: None,
                        result_hash: None,
                    }),
                }],
            },
        ],
    };
    let history = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: Some(user_message_id),
            content: vec![ContentPart::Text {
                text: "Fix the failing verification.".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![Utf8PathBuf::from("src/workflow.ts")];
    state.completion.open_work_count = 1;
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed for src/workflow.ts".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: vec![Utf8PathBuf::from("src/workflow.ts")],
    });
    let agent_config = ResolvedConfig::default().agent;
    let legacy_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &history,
        &[],
        &agent_config,
        Some(&state),
    );

    let mut typed_history = history.clone();
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 2,
        payload: HistoryItemPayload::ToolOutput {
            call_id: crate::session::ToolCallId::new(),
            status: crate::protocol::ToolLifecycleStatus::Completed,
            title: "Verification failed".to_string(),
            output_text: "verification failed: src/workflow.ts".to_string(),
            metadata: json!({}),
            success: Some(false),
            progress_effect: crate::protocol::ToolProgressEffect::VerificationFailed,
            blocked_action: None,
            result_hash: None,
            verification_run: Some(crate::protocol::VerificationRunResult {
                command: "fixture verify".to_string(),
                status: crate::protocol::VerificationRunStatus::Failed,
                exit_code: Some(1),
                timed_out: false,
                output_summary: "verification failed for src/workflow.ts".to_string(),
                failure_cluster: Some(VerificationFailureCluster {
                    cluster_id: "fixture-verification-failure".to_string(),
                    failing_labels: vec!["src/workflow.ts".to_string()],
                    primary_failure: Some("src/workflow.ts".to_string()),
                    evidence: Vec::new(),
                    sibling_obligations: Vec::new(),
                    source_refs: Vec::new(),
                    test_refs: Vec::new(),
                }),
                satisfies_command_identities: Vec::new(),
                artifact_refs: Vec::new(),
                requirement_refs: Vec::new(),
            }),
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 3,
        created_at_ms: 3,
        payload: HistoryItemPayload::ToolOutput {
            call_id,
            status: crate::protocol::ToolLifecycleStatus::Completed,
            title: "display text must not be prompt authority".to_string(),
            output_text: "typed verification repair focus required".to_string(),
            metadata: json!({
                "tool_feedback_envelope": {
                    "kind": "verification_repair_focus_required",
                    "operation_progress_class": "verification_repair_focus_required",
                    "progress_effect": "no_progress",
                    "read_budget_exhausted": true,
                    "required_next_action": {
                        "tool": "write",
                        "target": "src/workflow.ts"
                    },
                    "active_targets": ["src/workflow.ts"]
                },
                "verification_repair_focus_required": true,
                "verification_repair_read_budget_exhausted": true
            }),
            success: Some(false),
            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
            blocked_action: Some("write:src/workflow.ts".to_string()),
            result_hash: Some("typed-focus-required".to_string()),
            verification_run: None,
        },
    });
    let typed_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &typed_history,
        &[],
        &agent_config,
        Some(&state),
    );

    !legacy_signals.verification_repair_read_budget_exhausted
        && !legacy_signals.verification_failure_repair_edit_focused_mode
        && typed_signals.verification_repair_read_budget_exhausted
        && typed_signals.verification_failure_repair_edit_focused_mode
        && typed_signals.verification_repair_focus_target.as_deref() == Some("src/workflow.ts")
}

pub(crate) fn verification_repair_target_rotation_uses_typed_history_item_authority_fixture_passes()
-> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "verification target rotation prompt authority fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let turn_id = crate::protocol::TurnId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let shell_call_id = crate::session::ToolCallId::new();
    let rotation_call_id = crate::session::ToolCallId::new();
    let transcript = Transcript {
        session: session.clone(),
        messages: vec![
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: user_message_id,
                    session_id: session.id,
                    role: MessageRole::User,
                    parent_message_id: None,
                    sequence_no: 1,
                    created_at_ms: 1,
                    metadata: MessageMetadata::User(crate::session::UserMessageMeta {
                        cwd: Utf8PathBuf::from("."),
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
                        text: "Fix both failing verification targets.".to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: assistant_message_id,
                    session_id: session.id,
                    role: MessageRole::Assistant,
                    parent_message_id: Some(user_message_id),
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                parts: vec![crate::session::PartRecord {
                    id: crate::session::PartId::new(),
                    message_id: assistant_message_id,
                    sequence_no: 1,
                    kind: crate::session::PartKind::ToolResult,
                    payload: MessagePart::ToolResult(ToolResultPart {
                        tool_call_id: rotation_call_id,
                        status: ToolCallStatus::Completed,
                        title: "Verification repair target rotation required".to_string(),
                        summary: "The next edit must target `src/second.ts`.".to_string(),
                        success: Some(false),
                        progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                        blocked_action: None,
                        result_hash: None,
                    }),
                }],
            },
        ],
    };
    let history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: Some(user_message_id),
                content: vec![ContentPart::Text {
                    text: "Fix both failing verification targets.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id: shell_call_id,
                tool: ToolName::Shell,
                arguments: json!({"command":"fixture verify"}),
                model_arguments: Value::Null,
                effective_arguments: json!({"command":"fixture verify"}),
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
            session_id: session.id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: shell_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Verification failed".to_string(),
                output_text: "typed verification failure".to_string(),
                metadata: json!({}),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: None,
                verification_run: Some(crate::protocol::VerificationRunResult {
                    command: "fixture verify".to_string(),
                    status: crate::protocol::VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "verification failed for src/first.ts and src/second.ts"
                        .to_string(),
                    failure_cluster: Some(VerificationFailureCluster {
                        cluster_id: "fixture-verification-failure".to_string(),
                        failing_labels: vec![
                            "src/first.ts".to_string(),
                            "src/second.ts".to_string(),
                        ],
                        primary_failure: Some("src/first.ts".to_string()),
                        evidence: Vec::new(),
                        sibling_obligations: Vec::new(),
                        source_refs: Vec::new(),
                        test_refs: Vec::new(),
                    }),
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
    ];
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Repair;
    state.active_targets = vec![
        Utf8PathBuf::from("src/first.ts"),
        Utf8PathBuf::from("src/second.ts"),
    ];
    state.completion.open_work_count = 1;
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::VerificationFailed,
        summary: "verification failed for src/first.ts and src/second.ts".to_string(),
        tool_name: Some(ToolName::Shell),
        targets: vec![
            Utf8PathBuf::from("src/first.ts"),
            Utf8PathBuf::from("src/second.ts"),
        ],
    });
    let agent_config = ResolvedConfig::default().agent;
    let legacy_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &history,
        &[],
        &agent_config,
        Some(&state),
    );

    let mut typed_history = history.clone();
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 4,
        created_at_ms: 4,
        payload: HistoryItemPayload::ToolOutput {
            call_id: rotation_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "display text must not be prompt authority".to_string(),
            output_text: "typed verification repair target rotation required".to_string(),
            metadata: json!({
                "tool_feedback_envelope": {
                    "kind": "verification_repair_target_rotation_required",
                    "operation_progress_class": "verification_repair_target_rotation_required",
                    "progress_effect": "no_progress",
                    "required_next_action": {
                        "tool": "write",
                        "target": "src/second.ts"
                    },
                    "active_targets": ["src/first.ts", "src/second.ts"]
                },
                "verification_repair_target_rotation_required": true
            }),
            success: Some(false),
            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
            blocked_action: Some("write:src/second.ts".to_string()),
            result_hash: Some("typed-target-rotation-required".to_string()),
            verification_run: None,
        },
    });
    let typed_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &typed_history,
        &[],
        &agent_config,
        Some(&state),
    );

    !legacy_signals.verification_failure_repair_edit_focused_mode
        && legacy_signals.verification_repair_focus_target.is_none()
        && typed_signals.verification_failure_repair_edit_focused_mode
        && typed_signals.verification_repair_focus_target.as_deref() == Some("src/second.ts")
}

pub(crate) fn prompt_projection_fixture_domain_neutral_fixture_passes() -> bool {
    vision_input_provider_projection_fixture_passes()
        && prompt_projection_uses_typed_tool_output_feedback_fixture_passes()
        && message_only_history_does_not_recreate_tool_lifecycle_prompt_state_fixture_passes()
        && verification_repair_read_budget_exhaustion_uses_typed_history_item_authority_fixture_passes()
        && verification_repair_target_rotation_uses_typed_history_item_authority_fixture_passes()
}

pub(crate) fn verification_evidence_uses_typed_history_item_authority_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "verification evidence authority fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let turn_id = crate::protocol::TurnId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let shell_call_id = crate::session::ToolCallId::new();
    let transcript = Transcript {
        session: session.clone(),
        messages: vec![
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: user_message_id,
                    session_id: session.id,
                    role: MessageRole::User,
                    parent_message_id: None,
                    sequence_no: 1,
                    created_at_ms: 1,
                    metadata: MessageMetadata::User(crate::session::UserMessageMeta {
                        cwd: Utf8PathBuf::from("."),
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
                        text: "Run tests before completion.".to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: assistant_message_id,
                    session_id: session.id,
                    role: MessageRole::Assistant,
                    parent_message_id: Some(user_message_id),
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
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
                            tool_call_id: shell_call_id,
                            tool_name: ToolName::Shell,
                            arguments_json: json!({"command":"cargo test"}).to_string(),
                            model_arguments_json: None,
                            effective_arguments_json: None,
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 2,
                        kind: crate::session::PartKind::ToolResult,
                        payload: MessagePart::ToolResult(ToolResultPart {
                            tool_call_id: shell_call_id,
                            status: ToolCallStatus::Completed,
                            title: "cargo test".to_string(),
                            summary: "test result: ok. all tests pass".to_string(),
                            success: Some(true),
                            progress_effect: crate::protocol::ToolProgressEffect::Unknown,
                            blocked_action: None,
                            result_hash: Some("display-only-success".to_string()),
                        }),
                    },
                ],
            },
        ],
    };
    let history = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: Some(user_message_id),
            content: vec![ContentPart::Text {
                text: "Run tests before completion.".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let mut state = SessionStateSnapshot::default();
    state.process_phase = ProcessPhase::Verify;
    state.completion.open_work_count = 0;
    let agent_config = ResolvedConfig::default().agent;
    let legacy_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &history,
        &[],
        &agent_config,
        Some(&state),
    );

    let mut typed_history = history.clone();
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 2,
        payload: HistoryItemPayload::ToolCall {
            call_id: shell_call_id,
            tool: ToolName::Shell,
            arguments: json!({"command":"cargo test"}),
            model_arguments: Value::Null,
            effective_arguments: json!({"command":"cargo test"}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: vec![ToolName::Shell],
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 3,
        created_at_ms: 3,
        payload: HistoryItemPayload::ToolOutput {
            call_id: shell_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "display text is not verification authority".to_string(),
            output_text: "typed verification run passed".to_string(),
            metadata: Value::Null,
            success: Some(true),
            progress_effect: crate::protocol::ToolProgressEffect::Unknown,
            blocked_action: None,
            result_hash: Some("typed-verification-pass".to_string()),
            verification_run: Some(crate::protocol::VerificationRunResult {
                command: "cargo test".to_string(),
                status: crate::protocol::VerificationRunStatus::Passed,
                exit_code: Some(0),
                timed_out: false,
                output_summary: "test result: ok. all tests pass".to_string(),
                failure_cluster: None,
                satisfies_command_identities: Vec::new(),
                artifact_refs: Vec::new(),
                requirement_refs: Vec::new(),
            }),
        },
    });
    let typed_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &typed_history,
        &[],
        &agent_config,
        Some(&state),
    );

    legacy_signals.verification_pending_without_open_work
        && !typed_signals.verification_pending_without_open_work
}

pub(crate) fn staged_task_closeout_repair_targets_use_typed_history_authority_fixture_passes()
-> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "staged closeout repair authority fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let turn_id = crate::protocol::TurnId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let read_call_id = crate::session::ToolCallId::new();
    let output_write_call_id = crate::session::ToolCallId::new();
    let denied_write_call_id = crate::session::ToolCallId::new();
    let transcript = Transcript {
        session: session.clone(),
        messages: vec![
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: user_message_id,
                    session_id: session.id,
                    role: MessageRole::User,
                    parent_message_id: None,
                    sequence_no: 1,
                    created_at_ms: 1,
                    metadata: MessageMetadata::User(crate::session::UserMessageMeta {
                        cwd: Utf8PathBuf::from("."),
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
                        text: "Read task.md and produce docs/output.md.".to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: assistant_message_id,
                    session_id: session.id,
                    role: MessageRole::Assistant,
                    parent_message_id: Some(user_message_id),
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
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
                            tool_call_id: read_call_id,
                            tool_name: ToolName::Read,
                            arguments_json: json!({"path":"task.md"}).to_string(),
                            model_arguments_json: None,
                            effective_arguments_json: None,
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 2,
                        kind: crate::session::PartKind::ToolResult,
                        payload: MessagePart::ToolResult(ToolResultPart {
                            tool_call_id: read_call_id,
                            status: ToolCallStatus::Completed,
                            title: "Read task.md".to_string(),
                            summary: "Create docs/output.md.".to_string(),
                            success: Some(true),
                            progress_effect: crate::protocol::ToolProgressEffect::Unknown,
                            blocked_action: None,
                            result_hash: Some("read-task".to_string()),
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 3,
                        kind: crate::session::PartKind::DiffSummary,
                        payload: MessagePart::DiffSummary(crate::session::DiffSummaryPart {
                            tool_call_id: Some(output_write_call_id),
                            change_ids: Vec::new(),
                            changes: Vec::new(),
                            summary: "Updated docs/output.md".to_string(),
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 4,
                        kind: crate::session::PartKind::ToolCall,
                        payload: MessagePart::ToolCall(crate::session::ToolCallPart {
                            tool_call_id: denied_write_call_id,
                            tool_name: ToolName::Write,
                            arguments_json: json!({"path":"docs/output.md","content":"draft"})
                                .to_string(),
                            model_arguments_json: None,
                            effective_arguments_json: None,
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 5,
                        kind: crate::session::PartKind::ToolResult,
                        payload: MessagePart::ToolResult(ToolResultPart {
                            tool_call_id: denied_write_call_id,
                            status: ToolCallStatus::Completed,
                            title: "Tool not allowed in current run state".to_string(),
                            summary: "The `write` tool is not available in the current run state."
                                .to_string(),
                            success: Some(false),
                            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                            blocked_action: None,
                            result_hash: Some("display-only-denied-write".to_string()),
                        }),
                    },
                ],
            },
        ],
    };
    let history = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: Some(user_message_id),
            content: vec![ContentPart::Text {
                text: "Read task.md and produce docs/output.md.".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let mut todo = TodoItem::simple(
        "Read task.md and produce docs/output.md.",
        crate::session::TodoStatus::InProgress,
        crate::session::TodoPriority::High,
    );
    todo.targets = vec![Utf8PathBuf::from("task.md")];
    let todos = vec![todo];
    let agent_config = ResolvedConfig::default().agent;
    let legacy_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &history,
        &todos,
        &agent_config,
        None,
    );

    let mut typed_history = history.clone();
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 2,
        payload: HistoryItemPayload::ToolCall {
            call_id: read_call_id,
            tool: ToolName::Read,
            arguments: json!({"path":"task.md"}),
            model_arguments: Value::Null,
            effective_arguments: json!({"path":"task.md"}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: vec![ToolName::Read],
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 3,
        created_at_ms: 3,
        payload: HistoryItemPayload::ToolOutput {
            call_id: read_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "display title is not authority".to_string(),
            output_text: "Create docs/output.md.".to_string(),
            metadata: json!({
                "operation_progress_class": "supporting_context",
                "consumed_supporting_context": true,
                "target": "task.md"
            }),
            success: Some(true),
            progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
            blocked_action: None,
            result_hash: Some("typed-read-task".to_string()),
            verification_run: None,
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 4,
        created_at_ms: 4,
        payload: HistoryItemPayload::FileChange {
            call_id: output_write_call_id,
            change_ids: vec![crate::session::ChangeId::new()],
            changes: vec![crate::protocol::FileChangeEvidence {
                change_id: crate::session::ChangeId::new(),
                kind: crate::session::ChangeKind::Add,
                path_before: None,
                path_after: Some(Utf8PathBuf::from("docs/output.md")),
                summary: "created docs/output.md".to_string(),
            }],
            summary: "Updated docs/output.md".to_string(),
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 5,
        created_at_ms: 5,
        payload: HistoryItemPayload::ToolCall {
            call_id: denied_write_call_id,
            tool: ToolName::Write,
            arguments: json!({"path":"docs/output.md","content":"draft"}),
            model_arguments: Value::Null,
            effective_arguments: json!({"path":"docs/output.md","content":"draft"}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: vec![ToolName::Write],
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 6,
        created_at_ms: 6,
        payload: HistoryItemPayload::ToolOutput {
            call_id: denied_write_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "display text must not be prompt authority".to_string(),
            output_text: "typed unavailable write feedback".to_string(),
            metadata: json!({
                "tool_feedback_envelope": {
                    "kind": "tool_outside_allowed_surface",
                    "operation_progress_class": "tool_outside_allowed_surface",
                    "submitted_targets": ["docs/output.md"],
                    "blocked_action": "write:docs/output.md",
                    "required_next_action": {
                        "tool": "write",
                        "target": "docs/output.md"
                    }
                }
            }),
            success: Some(false),
            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
            blocked_action: Some("write:docs/output.md".to_string()),
            result_hash: Some("typed-denied-write".to_string()),
            verification_run: None,
        },
    });
    let typed_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &typed_history,
        &todos,
        &agent_config,
        None,
    );

    !legacy_signals.staged_task_closeout_mode
        && !legacy_signals.staged_task_closeout_repair_mode
        && legacy_signals
            .staged_task_closeout_repair_targets
            .is_empty()
        && typed_signals.staged_task_closeout_mode
        && typed_signals.staged_task_closeout_repair_mode
        && typed_signals.staged_task_closeout_repair_targets == vec!["docs/output.md"]
}

pub(crate) fn staged_task_output_lifecycle_uses_typed_history_authority_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "staged output lifecycle authority fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let turn_id = crate::protocol::TurnId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let task_read_call_id = crate::session::ToolCallId::new();
    let output_write_call_id = crate::session::ToolCallId::new();
    let output_read_call_id = crate::session::ToolCallId::new();
    let transcript = Transcript {
        session: session.clone(),
        messages: vec![
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: user_message_id,
                    session_id: session.id,
                    role: MessageRole::User,
                    parent_message_id: None,
                    sequence_no: 1,
                    created_at_ms: 1,
                    metadata: MessageMetadata::User(crate::session::UserMessageMeta {
                        cwd: Utf8PathBuf::from("."),
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
                        text: "Read task.md and produce docs/output.md.".to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: assistant_message_id,
                    session_id: session.id,
                    role: MessageRole::Assistant,
                    parent_message_id: Some(user_message_id),
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
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
                            tool_call_id: task_read_call_id,
                            tool_name: ToolName::Read,
                            arguments_json: json!({"path":"task.md"}).to_string(),
                            model_arguments_json: None,
                            effective_arguments_json: None,
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 2,
                        kind: crate::session::PartKind::ToolResult,
                        payload: MessagePart::ToolResult(ToolResultPart {
                            tool_call_id: task_read_call_id,
                            status: ToolCallStatus::Completed,
                            title: "Read task.md".to_string(),
                            summary: "Create docs/output.md.".to_string(),
                            success: Some(true),
                            progress_effect: crate::protocol::ToolProgressEffect::Unknown,
                            blocked_action: None,
                            result_hash: Some("display-task-read".to_string()),
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 3,
                        kind: crate::session::PartKind::DiffSummary,
                        payload: MessagePart::DiffSummary(crate::session::DiffSummaryPart {
                            tool_call_id: Some(output_write_call_id),
                            change_ids: Vec::new(),
                            changes: Vec::new(),
                            summary: "Updated docs/output.md".to_string(),
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 4,
                        kind: crate::session::PartKind::ToolCall,
                        payload: MessagePart::ToolCall(crate::session::ToolCallPart {
                            tool_call_id: output_read_call_id,
                            tool_name: ToolName::Read,
                            arguments_json: json!({"path":"docs/output.md"}).to_string(),
                            model_arguments_json: None,
                            effective_arguments_json: None,
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 5,
                        kind: crate::session::PartKind::ToolResult,
                        payload: MessagePart::ToolResult(ToolResultPart {
                            tool_call_id: output_read_call_id,
                            status: ToolCallStatus::Completed,
                            title: "Read docs/output.md".to_string(),
                            summary: "# Output\n\nDone.".to_string(),
                            success: Some(true),
                            progress_effect: crate::protocol::ToolProgressEffect::Unknown,
                            blocked_action: None,
                            result_hash: Some("display-output-read".to_string()),
                        }),
                    },
                ],
            },
        ],
    };
    let history = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: Some(user_message_id),
            content: vec![ContentPart::Text {
                text: "Read task.md and produce docs/output.md.".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let mut todo = TodoItem::simple(
        "Read task.md and produce docs/output.md.",
        crate::session::TodoStatus::InProgress,
        crate::session::TodoPriority::High,
    );
    todo.targets = vec![Utf8PathBuf::from("task.md")];
    let todos = vec![todo];
    let agent_config = ResolvedConfig::default().agent;
    let transcript_only_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &history,
        &todos,
        &agent_config,
        None,
    );

    let mut typed_history = history.clone();
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 2,
        payload: HistoryItemPayload::ToolCall {
            call_id: task_read_call_id,
            tool: ToolName::Read,
            arguments: json!({"path":"task.md"}),
            model_arguments: Value::Null,
            effective_arguments: json!({"path":"task.md"}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: vec![ToolName::Read],
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 3,
        created_at_ms: 3,
        payload: HistoryItemPayload::ToolOutput {
            call_id: task_read_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "display title is not authority".to_string(),
            output_text: "Create docs/output.md.".to_string(),
            metadata: json!({
                "operation_progress_class": "supporting_context",
                "consumed_supporting_context": true,
                "target": "task.md"
            }),
            success: Some(true),
            progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
            blocked_action: None,
            result_hash: Some("typed-task-read".to_string()),
            verification_run: None,
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 4,
        created_at_ms: 4,
        payload: HistoryItemPayload::ToolCall {
            call_id: output_write_call_id,
            tool: ToolName::Write,
            arguments: json!({"path":"docs/output.md","content":"# Output\n\nDone."}),
            model_arguments: Value::Null,
            effective_arguments: json!({"path":"docs/output.md","content":"# Output\n\nDone."}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: vec![ToolName::Write],
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 5,
        created_at_ms: 5,
        payload: HistoryItemPayload::FileChange {
            call_id: output_write_call_id,
            change_ids: vec![crate::session::ChangeId::new()],
            changes: vec![crate::protocol::FileChangeEvidence {
                change_id: crate::session::ChangeId::new(),
                kind: crate::session::ChangeKind::Add,
                path_before: None,
                path_after: Some(Utf8PathBuf::from("docs/output.md")),
                summary: "created docs/output.md".to_string(),
            }],
            summary: "Updated docs/output.md".to_string(),
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 6,
        created_at_ms: 6,
        payload: HistoryItemPayload::ToolCall {
            call_id: output_read_call_id,
            tool: ToolName::Read,
            arguments: json!({"path":"docs/output.md"}),
            model_arguments: Value::Null,
            effective_arguments: json!({"path":"docs/output.md"}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: vec![ToolName::Read],
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 7,
        created_at_ms: 7,
        payload: HistoryItemPayload::ToolOutput {
            call_id: output_read_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "display title is not authority".to_string(),
            output_text: "# Output\n\nDone.".to_string(),
            metadata: json!({
                "operation_progress_class": "supporting_context",
                "consumed_supporting_context": true,
                "target": "docs/output.md"
            }),
            success: Some(true),
            progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
            blocked_action: None,
            result_hash: Some("typed-output-read".to_string()),
            verification_run: None,
        },
    });
    let typed_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &typed_history,
        &todos,
        &agent_config,
        None,
    );

    !transcript_only_signals.staged_task_closeout_mode
        && !transcript_only_signals.staged_task_closeout_read_complete
        && transcript_only_signals
            .staged_task_output_targets
            .is_empty()
        && typed_signals.staged_task_closeout_mode
        && typed_signals.staged_task_closeout_read_complete
        && typed_signals.staged_task_output_targets == vec!["docs/output.md"]
}

pub(crate) fn documentation_prompt_lifecycle_uses_typed_history_authority_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "documentation prompt lifecycle authority fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let turn_id = crate::protocol::TurnId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let docs_read_call_id = crate::session::ToolCallId::new();
    let source_read_call_id = crate::session::ToolCallId::new();
    let _compatibility_transcript_with_display_only_evidence = Transcript {
        session: session.clone(),
        messages: vec![
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: user_message_id,
                    session_id: session.id,
                    role: MessageRole::User,
                    parent_message_id: None,
                    sequence_no: 1,
                    created_at_ms: 1,
                    metadata: MessageMetadata::User(crate::session::UserMessageMeta {
                        cwd: Utf8PathBuf::from("."),
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
                        text: "Update docs/output.md from repository evidence.".to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: assistant_message_id,
                    session_id: session.id,
                    role: MessageRole::Assistant,
                    parent_message_id: Some(user_message_id),
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
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
                            tool_call_id: docs_read_call_id,
                            tool_name: ToolName::Read,
                            arguments_json: json!({"path":"docs/guide.md"}).to_string(),
                            model_arguments_json: None,
                            effective_arguments_json: None,
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 2,
                        kind: crate::session::PartKind::ToolResult,
                        payload: MessagePart::ToolResult(ToolResultPart {
                            tool_call_id: docs_read_call_id,
                            status: ToolCallStatus::Completed,
                            title: "Read docs/guide.md".to_string(),
                            summary: "# Guide\n\nExisting notes.".to_string(),
                            success: Some(true),
                            progress_effect: crate::protocol::ToolProgressEffect::Unknown,
                            blocked_action: None,
                            result_hash: Some("display-docs-read".to_string()),
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 3,
                        kind: crate::session::PartKind::ToolCall,
                        payload: MessagePart::ToolCall(crate::session::ToolCallPart {
                            tool_call_id: source_read_call_id,
                            tool_name: ToolName::Read,
                            arguments_json: json!({"path":"src/app.rs"}).to_string(),
                            model_arguments_json: None,
                            effective_arguments_json: None,
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: assistant_message_id,
                        sequence_no: 4,
                        kind: crate::session::PartKind::ToolResult,
                        payload: MessagePart::ToolResult(ToolResultPart {
                            tool_call_id: source_read_call_id,
                            status: ToolCallStatus::Completed,
                            title: "Read src/app.rs".to_string(),
                            summary: "1: pub fn build_app() {}\n2: export runtime surface"
                                .to_string(),
                            success: Some(true),
                            progress_effect: crate::protocol::ToolProgressEffect::Unknown,
                            blocked_action: None,
                            result_hash: Some("display-source-read".to_string()),
                        }),
                    },
                ],
            },
        ],
    };
    let history = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: Some(user_message_id),
            content: vec![ContentPart::Text {
                text: "Update docs/output.md from repository evidence.".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let mut agent_config = ResolvedConfig::default().agent;
    agent_config.readonly_stall_threshold_general = 2;
    let (readonly_stall, readonly_targets) =
        recent_tool_call_stalled_with_config(&history, 0, false, &agent_config);
    let documentation_scope = documentation_scope_targets(&[], &history, 0, FollowUpFocus::Unknown);
    let staged_evidence =
        staged_task_documentation_evidence_snapshot(&history, 0, &["docs/output.md".to_string()]);
    let mut typed_history = history.clone();
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 2,
        payload: HistoryItemPayload::ToolCall {
            call_id: docs_read_call_id,
            tool: ToolName::Read,
            arguments: json!({"path":"docs/guide.md"}),
            model_arguments: Value::Null,
            effective_arguments: json!({"path":"docs/guide.md"}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: vec![ToolName::Read],
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 3,
        created_at_ms: 3,
        payload: HistoryItemPayload::ToolOutput {
            call_id: docs_read_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "display title is not authority".to_string(),
            output_text: "# Guide\n\nExisting notes.".to_string(),
            metadata: json!({"operation_progress_class":"supporting_context","target":"docs/guide.md"}),
            success: Some(true),
            progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
            blocked_action: None,
            result_hash: Some("typed-docs-read".to_string()),
            verification_run: None,
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 4,
        created_at_ms: 4,
        payload: HistoryItemPayload::ToolCall {
            call_id: source_read_call_id,
            tool: ToolName::Read,
            arguments: json!({"path":"src/app.rs"}),
            model_arguments: Value::Null,
            effective_arguments: json!({"path":"src/app.rs"}),
            adjusted_arguments: None,
            permission_decision: None,
            sandbox_decision: None,
            allowed_surface: vec![ToolName::Read],
            retry_policy: None,
            terminal_guard_policy: None,
        },
    });
    typed_history.push(HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 5,
        created_at_ms: 5,
        payload: HistoryItemPayload::ToolOutput {
            call_id: source_read_call_id,
            status: ToolLifecycleStatus::Completed,
            title: "display title is not authority".to_string(),
            output_text: "1: pub fn build_app() {}\n2: export runtime surface".to_string(),
            metadata: json!({"operation_progress_class":"supporting_context","target":"src/app.rs"}),
            success: Some(true),
            progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
            blocked_action: None,
            result_hash: Some("typed-source-read".to_string()),
            verification_run: None,
        },
    });
    let (typed_readonly_stall, typed_readonly_targets) =
        recent_tool_call_stalled_with_config(&typed_history, 0, false, &agent_config);
    let typed_documentation_scope =
        documentation_scope_targets(&[], &typed_history, 0, FollowUpFocus::Unknown);
    let typed_staged_evidence = staged_task_documentation_evidence_snapshot(
        &typed_history,
        0,
        &["docs/output.md".to_string()],
    );

    !readonly_stall
        && readonly_targets.is_empty()
        && documentation_scope.is_empty()
        && staged_evidence.is_none()
        && typed_readonly_stall
        && typed_readonly_targets == vec!["src/app.rs", "docs/guide.md"]
        && typed_documentation_scope == vec!["docs/guide.md"]
        && typed_staged_evidence
            .as_deref()
            .is_some_and(|value| value.contains("src/app.rs"))
}

pub(crate) fn follow_up_focus_uses_typed_history_authority_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "follow-up focus authority fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let turn_id = crate::protocol::TurnId::new();
    let previous_user_message_id = crate::session::MessageId::new();
    let previous_assistant_message_id = crate::session::MessageId::new();
    let latest_user_message_id = crate::session::MessageId::new();
    let prior_doc_write_call_id = crate::session::ToolCallId::new();
    let transcript = Transcript {
        session: session.clone(),
        messages: vec![
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: previous_user_message_id,
                    session_id: session.id,
                    role: MessageRole::User,
                    parent_message_id: None,
                    sequence_no: 1,
                    created_at_ms: 1,
                    metadata: MessageMetadata::User(crate::session::UserMessageMeta {
                        cwd: Utf8PathBuf::from("."),
                        requested_model: None,
                        editor_context: None,
                    }),
                },
                parts: vec![crate::session::PartRecord {
                    id: crate::session::PartId::new(),
                    message_id: previous_user_message_id,
                    sequence_no: 1,
                    kind: crate::session::PartKind::Text,
                    payload: MessagePart::Text(crate::session::TextPart {
                        text: "Draft docs/design.md.".to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: previous_assistant_message_id,
                    session_id: session.id,
                    role: MessageRole::Assistant,
                    parent_message_id: Some(previous_user_message_id),
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                parts: vec![
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: previous_assistant_message_id,
                        sequence_no: 1,
                        kind: crate::session::PartKind::ToolCall,
                        payload: MessagePart::ToolCall(crate::session::ToolCallPart {
                            tool_call_id: prior_doc_write_call_id,
                            tool_name: ToolName::Write,
                            arguments_json: json!({"path":"docs/design.md","content":"# Design"})
                                .to_string(),
                            model_arguments_json: None,
                            effective_arguments_json: None,
                        }),
                    },
                    crate::session::PartRecord {
                        id: crate::session::PartId::new(),
                        message_id: previous_assistant_message_id,
                        sequence_no: 2,
                        kind: crate::session::PartKind::DiffSummary,
                        payload: MessagePart::DiffSummary(crate::session::DiffSummaryPart {
                            tool_call_id: Some(prior_doc_write_call_id),
                            change_ids: Vec::new(),
                            changes: Vec::new(),
                            summary: "Updated docs/design.md".to_string(),
                        }),
                    },
                ],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: latest_user_message_id,
                    session_id: session.id,
                    role: MessageRole::User,
                    parent_message_id: Some(previous_assistant_message_id),
                    sequence_no: 3,
                    created_at_ms: 3,
                    metadata: MessageMetadata::User(crate::session::UserMessageMeta {
                        cwd: Utf8PathBuf::from("."),
                        requested_model: None,
                        editor_context: Some(crate::session::EditorContext {
                            active_file: Some(Utf8PathBuf::from("docs/active.md")),
                            visible_files: vec![Utf8PathBuf::from("docs/visible.md")],
                            open_tabs: vec![Utf8PathBuf::from("docs/open.md")],
                            shell_family: ShellFamily::PowerShell,
                            current_time_ms: 1,
                        }),
                    }),
                },
                parts: vec![crate::session::PartRecord {
                    id: crate::session::PartId::new(),
                    message_id: latest_user_message_id,
                    sequence_no: 1,
                    kind: crate::session::PartKind::Text,
                    payload: MessagePart::Text(crate::session::TextPart {
                        text: "Documentation only follow-up: continue the prior design notes."
                            .to_string(),
                    }),
                }],
            },
        ],
    };
    let latest_only_history = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: Some(latest_user_message_id),
            content: vec![ContentPart::Text {
                text: "Documentation only follow-up: continue the prior design notes.".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let agent_config = ResolvedConfig::default().agent;
    let transcript_only_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &latest_only_history,
        &[],
        &agent_config,
        None,
    );

    let typed_history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: Some(previous_user_message_id),
                content: vec![ContentPart::Text {
                    text: "Draft docs/design.md.".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id: prior_doc_write_call_id,
                tool: ToolName::Write,
                arguments: json!({"path":"docs/design.md","content":"# Design"}),
                model_arguments: Value::Null,
                effective_arguments: json!({"path":"docs/design.md","content":"# Design"}),
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
            session_id: session.id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::FileChange {
                call_id: prior_doc_write_call_id,
                change_ids: vec![crate::session::ChangeId::new()],
                changes: vec![crate::protocol::FileChangeEvidence {
                    change_id: crate::session::ChangeId::new(),
                    kind: crate::session::ChangeKind::Update,
                    path_before: Some(Utf8PathBuf::from("docs/design.md")),
                    path_after: Some(Utf8PathBuf::from("docs/design.md")),
                    summary: "updated docs/design.md".to_string(),
                }],
                summary: "Updated docs/design.md".to_string(),
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::UserTurn {
                message_id: Some(latest_user_message_id),
                content: vec![ContentPart::Text {
                    text: "Documentation only follow-up: continue the prior design notes."
                        .to_string(),
                }],
                prompt_dispatch: None,
                editor_context: Some(crate::session::EditorContext {
                    active_file: Some(Utf8PathBuf::from("docs/active.md")),
                    visible_files: vec![Utf8PathBuf::from("docs/visible.md")],
                    open_tabs: vec![Utf8PathBuf::from("docs/open.md")],
                    shell_family: ShellFamily::PowerShell,
                    current_time_ms: 1,
                }),
                turn_context: None,
            },
        },
    ];
    let typed_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &typed_history,
        &[],
        &agent_config,
        None,
    );

    !transcript_only_signals.follow_up_boundary
        && transcript_only_signals.follow_up_focus == FollowUpFocus::Documentation
        && transcript_only_signals
            .documentation_scope_targets
            .is_empty()
        && typed_signals.follow_up_boundary
        && typed_signals.follow_up_focus == FollowUpFocus::Documentation
        && typed_signals.documentation_scope_targets
            == vec![
                "docs/active.md",
                "docs/visible.md",
                "docs/open.md",
                "docs/design.md",
            ]
}

pub(crate) fn staged_task_recovery_stall_uses_typed_history_authority_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "staged recovery stall authority fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let turn_id = crate::protocol::TurnId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let agent_config = ResolvedConfig::default().agent;
    let threshold = agent_config
        .staged_task_recovery_stall_threshold
        .max(1)
        .min(RECENT_TOOL_CALL_WINDOW);
    let mut transcript_parts = Vec::new();
    for index in 0..threshold {
        let call_id = crate::session::ToolCallId::new();
        transcript_parts.push(crate::session::PartRecord {
            id: crate::session::PartId::new(),
            message_id: assistant_message_id,
            sequence_no: (index + 1) as i64,
            kind: crate::session::PartKind::ToolResult,
            payload: MessagePart::ToolResult(ToolResultPart {
                tool_call_id: call_id,
                status: ToolCallStatus::Completed,
                title: "display no-progress feedback".to_string(),
                summary: "Display/archive feedback only; not typed item authority.".to_string(),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some(format!("display-no-progress-{index}")),
            }),
        });
    }
    let transcript = Transcript {
        session: session.clone(),
        messages: vec![
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: user_message_id,
                    session_id: session.id,
                    role: MessageRole::User,
                    parent_message_id: None,
                    sequence_no: 1,
                    created_at_ms: 1,
                    metadata: MessageMetadata::User(crate::session::UserMessageMeta {
                        cwd: Utf8PathBuf::from("."),
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
                        text: "Read task.md and produce docs/output.md.".to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: assistant_message_id,
                    session_id: session.id,
                    role: MessageRole::Assistant,
                    parent_message_id: Some(user_message_id),
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                parts: transcript_parts,
            },
        ],
    };
    let history = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: Some(user_message_id),
            content: vec![ContentPart::Text {
                text: "Read task.md and produce docs/output.md.".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let legacy_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &history,
        &[],
        &agent_config,
        None,
    );

    let mut typed_history = history.clone();
    for index in 0..threshold {
        let call_id = crate::session::ToolCallId::new();
        typed_history.push(HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: (index as i64 * 2) + 2,
            created_at_ms: (index as i64 * 2) + 2,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                tool: ToolName::Write,
                arguments: json!({"path":"docs/output.md","content":"draft"}),
                model_arguments: Value::Null,
                effective_arguments: json!({"path":"docs/output.md","content":"draft"}),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::Write],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        });
        typed_history.push(HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: (index as i64 * 2) + 3,
            created_at_ms: (index as i64 * 2) + 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: ToolLifecycleStatus::Completed,
                title: "typed no-progress feedback".to_string(),
                output_text: "typed recovery feedback".to_string(),
                metadata: json!({
                    "tool_feedback_envelope": {
                        "kind": "wrong_authoring_target",
                        "operation_progress_class": "wrong_authoring_target",
                        "progress_effect": "no_progress",
                        "submitted_targets": ["docs/output.md"]
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: Some("write:docs/output.md".to_string()),
                result_hash: Some(format!("typed-no-progress-{index}")),
                verification_run: None,
            },
        });
    }
    let typed_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &typed_history,
        &[],
        &agent_config,
        None,
    );

    !legacy_signals.staged_task_recovery_stall && typed_signals.staged_task_recovery_stall
}

pub(crate) fn prompt_projection_uses_typed_verification_run_cycle_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "typed verification prompt fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let turn_id = crate::protocol::TurnId::new();
    let shell_call_id = crate::session::ToolCallId::new();
    let read_call_id = crate::session::ToolCallId::new();
    let edit_call_id = crate::session::ToolCallId::new();
    let history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Run workflow verification and fix failures".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::ToolCall {
                call_id: shell_call_id,
                tool: ToolName::Shell,
                arguments: json!({"command":"verify-workflow --behavior repair"}),
                model_arguments: Value::Null,
                effective_arguments: json!({"command":"verify-workflow --behavior repair"}),
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
            session_id: session.id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: shell_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Command finished".to_string(),
                output_text: "See structured verification_run metadata.".to_string(),
                metadata: json!({
                    "operation_progress_class": "required_write_content_shape_mismatch",
                    "tool_feedback_envelope": {
                        "kind": "required_write_content_shape_mismatch",
                        "operation_progress_class": "required_write_content_shape_mismatch"
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::VerificationFailed,
                blocked_action: None,
                result_hash: Some("verification-run-failed".to_string()),
                verification_run: Some(crate::protocol::VerificationRunResult {
                    command: "verify-workflow --behavior repair".to_string(),
                    status: crate::protocol::VerificationRunStatus::Failed,
                    exit_code: Some(1),
                    timed_out: false,
                    output_summary: "typed verification failure cluster".to_string(),
                    failure_cluster: None,
                    satisfies_command_identities: Vec::new(),
                    artifact_refs: Vec::new(),
                    requirement_refs: Vec::new(),
                }),
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::ToolCall {
                call_id: read_call_id,
                tool: ToolName::Read,
                arguments: json!({"path":"src/workflow.rs"}),
                model_arguments: Value::Null,
                effective_arguments: json!({"path":"src/workflow.rs"}),
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
            session_id: session.id,
            turn_id,
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::ToolOutput {
                call_id: read_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Read file".to_string(),
                output_text: "workflow_state = draft".to_string(),
                metadata: json!({
                    "operation_progress_class": "required_write_content_shape_mismatch",
                    "tool_feedback_envelope": {
                        "kind": "required_write_content_shape_mismatch",
                        "operation_progress_class": "required_write_content_shape_mismatch"
                    }
                }),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: Some("read-workflow-source".to_string()),
                verification_run: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 6,
            created_at_ms: 6,
            payload: HistoryItemPayload::ToolCall {
                call_id: edit_call_id,
                tool: ToolName::ApplyPatch,
                arguments: json!({"path":"src/workflow.rs","patch_text":"*** Begin Patch\n*** Update File: src/workflow.rs\n@@\n-workflow_state = draft\n+workflow_state = repaired\n*** End Patch\n"}),
                model_arguments: Value::Null,
                effective_arguments: json!({"path":"src/workflow.rs","patch_text":"*** Begin Patch\n*** Update File: src/workflow.rs\n@@\n-workflow_state = draft\n+workflow_state = repaired\n*** End Patch\n"}),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::ApplyPatch],
                retry_policy: None,
                terminal_guard_policy: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id: session.id,
            turn_id,
            sequence_no: 7,
            created_at_ms: 7,
            payload: HistoryItemPayload::ToolOutput {
                call_id: edit_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Patch applied".to_string(),
                output_text: "Updated src/workflow.rs".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: Some("patch-workflow-source".to_string()),
                verification_run: None,
            },
        },
    ];
    let typed_cycle =
        latest_verification_repair_cycle_from_history_items(&history, 0, &session.cwd);
    let typed_labels = recent_verification_failures_from_history(&history, 0);
    typed_cycle.as_ref().is_some_and(|cycle| {
        cycle.failed_command == "verify-workflow --behavior repair"
            && cycle.repair_recorded
            && cycle.post_failure_read_attempt_count == 1
            && cycle
                .post_failure_read_targets
                .iter()
                .any(|target| target.as_str() == "src/workflow.rs")
    }) && typed_labels.as_deref()
        == Some(&["verification command failed: verify-workflow --behavior repair".to_string()])
}

pub(crate) fn prompt_projection_uses_rejected_tool_proposal_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let source_call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "typed rejected tool prompt fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Create src/workflow.rs".to_string(),
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
            payload: HistoryItemPayload::RejectedToolProposal {
                proposal: crate::protocol::RejectedToolProposal {
                    proposal_id: crate::protocol::ToolProposalId::new(),
                    source_call_id,
                    requested_tool: "inspect_directory".to_string(),
                    effective_tool: "inspect_directory".to_string(),
                    resolved_tool: ToolName::InspectDirectory,
                    original_arguments: json!({"path":"."}),
                    adjusted_arguments: None,
                    allowed_surface: vec![ToolName::ApplyPatch, ToolName::Write],
                    blocked_reason: "Tool outside allowed surface".to_string(),
                    projection_id: crate::protocol::ProjectionId::new(),
                    semantic_class: "tool_outside_allowed_surface".to_string(),
                    candidate_repair_id: None,
                    payload_hash: "outside-surface".to_string(),
                    contract_refs: vec!["active:authoring".to_string()],
                    evidence_refs: vec!["turn-control-envelope".to_string()],
                },
            },
        },
    ];
    let transcript = transcript_from_history_items(&session, &history);
    recent_invalid_tool_result_stall_from_history(&history, 0)
        && !recent_invalid_tool_result_stall(&transcript, 0)
}

pub(crate) fn prompt_projection_uses_typed_pseudo_tool_rejection_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let source_call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "typed pseudo tool rejection prompt fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Edit src/workflow.rs; do not close until the file is updated"
                        .to_string(),
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
            payload: HistoryItemPayload::RejectedToolProposal {
                proposal: crate::protocol::RejectedToolProposal {
                    proposal_id: crate::protocol::ToolProposalId::new(),
                    source_call_id,
                    requested_tool: "final_assistant_message".to_string(),
                    effective_tool: "final_assistant_message".to_string(),
                    resolved_tool: ToolName::Invalid,
                    original_arguments: json!({
                        "text": "<tool_call>{\"name\":\"write\"}</tool_call>",
                        "projection_id": crate::protocol::ProjectionId::new().to_string(),
                    }),
                    adjusted_arguments: None,
                    allowed_surface: vec![ToolName::Write, ToolName::ApplyPatch],
                    blocked_reason:
                        "The provider emitted a final message while obligations remain open."
                            .to_string(),
                    projection_id: crate::protocol::ProjectionId::new(),
                    semantic_class: "text_final_while_obligations_open".to_string(),
                    candidate_repair_id: None,
                    payload_hash: "pseudo-final".to_string(),
                    contract_refs: vec!["turn-control-envelope".to_string()],
                    evidence_refs: vec!["provider-final-text".to_string()],
                },
            },
        },
    ];
    let transcript = transcript_from_history_items(&session, &history);
    recent_pseudo_tool_call_rejection_from_history(&history, 0)
        && !recent_assistant_pseudo_tool_call_stall(&transcript, 0)
}

pub(crate) fn code_block_stall_uses_typed_history_authority_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let source_call_id = crate::session::ToolCallId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "typed code-block stall prompt fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let _transcript_only = Transcript {
        session: session.clone(),
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
                        cwd: Utf8PathBuf::from("."),
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
                        text: "Fix verification failure before closing.".to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: assistant_message_id,
                    session_id,
                    role: MessageRole::Assistant,
                    parent_message_id: Some(user_message_id),
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                parts: vec![crate::session::PartRecord {
                    id: crate::session::PartId::new(),
                    message_id: assistant_message_id,
                    sequence_no: 1,
                    kind: crate::session::PartKind::Text,
                    payload: MessagePart::Text(crate::session::TextPart {
                        text: "```text\nworkflow display-only final text\n```".to_string(),
                    }),
                }],
            },
        ],
    };
    let latest_only_history = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: Some(user_message_id),
            content: vec![ContentPart::Text {
                text: "Fix verification failure before closing.".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let agent_config = ResolvedConfig::default().agent;
    let transcript_only_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&session),
        &latest_only_history,
        &[],
        &agent_config,
        None,
    );

    let typed_history = vec![
        latest_only_history[0].clone(),
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::RejectedToolProposal {
                proposal: crate::protocol::RejectedToolProposal {
                    proposal_id: crate::protocol::ToolProposalId::new(),
                    source_call_id,
                    requested_tool: "final_assistant_message".to_string(),
                    effective_tool: "final_assistant_message".to_string(),
                    resolved_tool: ToolName::Invalid,
                    original_arguments: json!({
                        "text": "```text\nworkflow typed rejected final text\n```",
                    }),
                    adjusted_arguments: None,
                    allowed_surface: vec![ToolName::Write, ToolName::ApplyPatch, ToolName::Shell],
                    blocked_reason:
                        "The provider emitted a final message while obligations remain open."
                            .to_string(),
                    projection_id: crate::protocol::ProjectionId::new(),
                    semantic_class: "text_final_while_obligations_open".to_string(),
                    candidate_repair_id: None,
                    payload_hash: "code-block-final".to_string(),
                    contract_refs: vec!["turn-control-envelope".to_string()],
                    evidence_refs: vec!["provider-final-text".to_string()],
                },
            },
        },
    ];
    let _typed_transcript = transcript_from_history_items(&session, &typed_history);
    let typed_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&session),
        &typed_history,
        &[],
        &agent_config,
        None,
    );

    !transcript_only_signals.code_block_stall
        && !transcript_only_signals.verification_recovery_mode
        && typed_signals.code_block_stall
}

pub(crate) fn superseded_tool_denial_uses_typed_history_authority_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let denied_call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "typed superseded tool denial prompt fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let _transcript_only = Transcript {
        session: session.clone(),
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
                        cwd: Utf8PathBuf::from("."),
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
                        text: "Continue authoring docs/output.md.".to_string(),
                    }),
                }],
            },
            crate::session::TranscriptMessage {
                record: crate::session::MessageRecord {
                    id: assistant_message_id,
                    session_id,
                    role: MessageRole::Assistant,
                    parent_message_id: Some(user_message_id),
                    sequence_no: 2,
                    created_at_ms: 2,
                    metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
                        finish_reason: None,
                        token_usage: None,
                        summary: false,
                    }),
                },
                parts: vec![crate::session::PartRecord {
                    id: crate::session::PartId::new(),
                    message_id: assistant_message_id,
                    sequence_no: 1,
                    kind: crate::session::PartKind::ToolResult,
                    payload: MessagePart::ToolResult(ToolResultPart {
                        tool_call_id: denied_call_id,
                        status: ToolCallStatus::Completed,
                        title: "Tool not allowed in current run state".to_string(),
                        summary: "The `write` tool is not available in the current run state."
                            .to_string(),
                        success: Some(false),
                        progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                        blocked_action: None,
                        result_hash: Some("display-only-denial".to_string()),
                    }),
                }],
            },
        ],
    };
    let latest_only_history = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: Some(user_message_id),
            content: vec![ContentPart::Text {
                text: "Continue authoring docs/output.md.".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let state = SessionStateSnapshot::default();
    let transcript_only_messages = build_messages_with_state(
        PromptProjectionInput::from_session(&session),
        &session,
        &latest_only_history,
        &state,
        &[],
        20,
        &["write".to_string()],
        &PromptSignals::default(),
        None,
    )
    .messages;
    let transcript_only_reminder =
        projection_contains_superseded_tool_denial_reminder(&transcript_only_messages);

    let typed_history = vec![
        latest_only_history[0].clone(),
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::RejectedToolProposal {
                proposal: crate::protocol::RejectedToolProposal {
                    proposal_id: crate::protocol::ToolProposalId::new(),
                    source_call_id: denied_call_id,
                    requested_tool: "write".to_string(),
                    effective_tool: "write".to_string(),
                    resolved_tool: ToolName::Write,
                    original_arguments: json!({"path":"docs/output.md","content":"draft"}),
                    adjusted_arguments: None,
                    allowed_surface: vec![ToolName::Read],
                    blocked_reason: "Tool outside allowed surface".to_string(),
                    projection_id: crate::protocol::ProjectionId::new(),
                    semantic_class: "tool_outside_allowed_surface".to_string(),
                    candidate_repair_id: None,
                    payload_hash: "typed-denied-write".to_string(),
                    contract_refs: vec!["turn-control-envelope".to_string()],
                    evidence_refs: vec!["allowed-surface".to_string()],
                },
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolOutput {
                call_id: denied_call_id,
                status: ToolLifecycleStatus::Completed,
                title: "typed denial feedback".to_string(),
                output_text: "typed unavailable write feedback".to_string(),
                metadata: json!({
                    "tool_feedback_envelope": {
                        "kind": "tool_outside_allowed_surface",
                        "operation_progress_class": "tool_outside_allowed_surface",
                        "requested_tool": "write",
                        "effective_tool": "write",
                        "blocked_reason": "Tool outside allowed surface",
                        "allowed_surface": ["read"],
                        "projection_id": "typed-fixture-projection"
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: Some("write:docs/output.md".to_string()),
                result_hash: Some("typed-denied-write-output".to_string()),
                verification_run: None,
            },
        },
    ];
    let _typed_transcript = transcript_from_history_items(&session, &typed_history);
    let typed_messages = build_messages_with_state(
        PromptProjectionInput::from_session(&session),
        &session,
        &typed_history,
        &state,
        &[],
        20,
        &["write".to_string()],
        &PromptSignals::default(),
        None,
    )
    .messages;
    let typed_reminder = projection_contains_superseded_tool_denial_reminder(&typed_messages);

    !transcript_only_reminder && typed_reminder
}

fn projection_contains_superseded_tool_denial_reminder(messages: &[ModelMessage]) -> bool {
    messages.iter().any(|message| match message {
        ModelMessage::System { content } => {
            content.contains("Earlier tool-availability failures came from an older run state")
        }
        _ => false,
    })
}

pub(crate) fn prompt_projection_uses_typed_docs_audit_metadata_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "typed docs audit prompt fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let history = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "Write docs/workflow-design.md from the latest behavior evidence"
                        .to_string(),
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
                    "path": "docs/workflow-design.md",
                    "content": "draft",
                }),
                model_arguments: Value::Null,
                effective_arguments: json!({
                    "path": "docs/workflow-design.md",
                    "content": "draft",
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
                status: ToolLifecycleStatus::Completed,
                title: "Docs/spec semantic reconciliation failed".to_string(),
                output_text: "typed docs reconciliation failed".to_string(),
                metadata: json!({
                    "success": false,
                    "operation_progress_class": "docs_spec_semantic_reconciliation_failed",
                    "progress_effect": "no_progress",
                    "targets": ["docs/workflow-design.md"],
                    "missing_required_claim_details": [{
                        "id": "workflow_validation_failure_contract",
                        "description": "document the workflow validation failure contract",
                        "evidence_refs": ["shell:verify-workflow --docs"]
                    }],
                    "prohibited_claim_details": [],
                    "tool_feedback_envelope": {
                        "kind": "docs_spec_semantic_reconciliation_failed",
                        "operation_progress_class": "docs_spec_semantic_reconciliation_failed",
                        "success": false,
                        "progress_effect": "no_progress",
                        "side_effects_applied": false,
                        "target": "docs/workflow-design.md"
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("docs-audit".to_string()),
                verification_run: None,
            },
        },
    ];
    let transcript = transcript_from_history_items(&session, &history);
    let typed = latest_staged_task_documentation_audit_state_from_history(
        &history,
        0,
        &["docs/workflow-design.md".to_string()],
    );
    let legacy = latest_staged_task_documentation_audit_state(
        &transcript,
        0,
        &["docs/workflow-design.md".to_string()],
    );
    typed.as_ref().is_some_and(|state| {
        state.target == "docs/workflow-design.md"
            && state.actionable_feedback
            && state
                .feedback
                .contains("document the workflow validation failure contract")
            && state.feedback.contains("shell:verify-workflow --docs")
    }) && legacy.is_none()
}

pub(crate) fn prompt_projection_uses_state_patch_recovery_fixture_passes() -> bool {
    let session = SessionRecord {
        id: crate::session::SessionId::new(),
        project_id: crate::session::ProjectId::new(),
        title: "typed patch recovery prompt fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("."),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let user_item = HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id: session.id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "Fix src/workflow.rs".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    };
    let transcript = Transcript {
        session: session.clone(),
        messages: vec![crate::session::TranscriptMessage {
            record: crate::session::MessageRecord {
                id: crate::session::MessageId::new(),
                session_id: session.id,
                role: MessageRole::Assistant,
                parent_message_id: None,
                sequence_no: 1,
                created_at_ms: 1,
                metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                    model: PROMPT_FIXTURE_MODEL.to_string(),
                    base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
                    finish_reason: None,
                    token_usage: None,
                    summary: false,
                }),
            },
            parts: vec![crate::session::PartRecord {
                id: crate::session::PartId::new(),
                message_id: crate::session::MessageId::new(),
                sequence_no: 1,
                kind: crate::session::PartKind::ToolResult,
                payload: MessagePart::ToolResult(ToolResultPart {
                    tool_call_id: call_id,
                    status: ToolCallStatus::Completed,
                    title: "Patch repair escalation".to_string(),
                    summary: "legacy patch escalation for stale workflow source".to_string(),
                    success: Some(false),
                    progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                    blocked_action: None,
                    result_hash: None,
                }),
            }],
        }],
    };
    let mut state = SessionStateSnapshot::default();
    state.failure = Some(crate::session::FailureState {
        kind: FailureKind::PatchMismatch,
        summary: "patch context mismatch".to_string(),
        tool_name: Some(ToolName::ApplyPatch),
        targets: vec![Utf8PathBuf::from("src/workflow.rs")],
    });
    let agent_config = ResolvedConfig::default().agent;
    let typed = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        std::slice::from_ref(&user_item),
        &[],
        &agent_config,
        Some(&state),
    );
    let legacy_suppressed = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&transcript.session),
        &[user_item],
        &[],
        &agent_config,
        None,
    )
    .patch_recovery_targets
    .is_empty();
    typed.patch_recovery_mode
        && typed.patch_recovery_targets == vec!["src/workflow.rs"]
        && legacy_suppressed
}

pub(crate) fn prompt_verification_repair_fixture_language_neutral_fixture_passes() -> bool {
    prompt_projection_uses_typed_verification_run_cycle_fixture_passes()
        && prompt_projection_uses_rejected_tool_proposal_fixture_passes()
        && prompt_projection_uses_typed_pseudo_tool_rejection_fixture_passes()
        && code_block_stall_uses_typed_history_authority_fixture_passes()
        && prompt_projection_uses_typed_docs_audit_metadata_fixture_passes()
        && prompt_projection_uses_state_patch_recovery_fixture_passes()
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
    classify_language_artifact_target(target).role == ArtifactRole::Test
}

fn target_is_python_source_like(target: &str) -> bool {
    let spec = classify_language_artifact_target(target);
    spec.language == LanguageFamily::Python && spec.role == ArtifactRole::Source
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
    workspace_root: &Utf8Path,
    history_items: &[HistoryItem],
    latest_user: Option<&str>,
) -> Option<String> {
    let snapshot = structured_document_summary_snapshot_from_history_items(
        workspace_root,
        history_items,
        latest_user,
    )?;
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
    prompt_input: PromptProjectionInput<'_>,
    history_items: &[HistoryItem],
    todos: &[TodoItem],
    agent_config: &AgentConfig,
    state: Option<&SessionStateSnapshot>,
) -> PromptSignals {
    let canonical_history_items = canonical_history_items_for_projection(history_items);
    let history_items = canonical_history_items.as_ref();
    let history_start_index = prompt_history_window_start_index(history_items);
    let latest_user_text = latest_user_text_from_history_items(history_items, history_start_index);
    let requested_contract = latest_user_text
        .as_deref()
        .map(requested_work_contract_from_instruction_text)
        .unwrap_or_default();
    let follow_up_boundary =
        has_historical_turns_before_latest_user(history_items, history_start_index);
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
    let editor_context_targets =
        latest_user_editor_context_targets(history_items, history_start_index);
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
                    history_items,
                    history_start_index,
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
                    history_items,
                    history_start_index,
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
    let observed_activity = observe_follow_up_activity(history_items, history_start_index);
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
            .chain(staged_task_artifacts_seen(
                history_items,
                history_start_index,
            ))
            .collect(),
    );
    let staged_task_output_targets =
        staged_task_output_targets(history_items, history_start_index, &staged_task_artifacts);
    let staged_task_verification_commands = staged_task_verification_commands(
        latest_user_text.as_deref(),
        &staged_task_artifacts,
        prompt_input.workspace_cwd,
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
        .then(|| {
            structured_document_summary_snapshot_from_history_items(
                prompt_input.workspace_cwd,
                history_items,
                latest_user_text.as_deref(),
            )
        })
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
        history_items,
        history_start_index,
        follow_up_implementation,
        agent_config,
    );
    let code_block_stall = recent_code_block_stall_from_history(history_items, history_start_index);
    let pseudo_tool_call_stall =
        recent_pseudo_tool_call_rejection_from_history(history_items, history_start_index);
    let invalid_tool_stall =
        recent_invalid_tool_result_stall_from_history(history_items, history_start_index);
    let verification_pending_error_stall = false;
    let no_tool_authoring_error_stall = false;
    let staged_task_recovery_stall = recent_nonprogress_recovery_result_stall_with_config(
        history_items,
        history_start_index,
        agent_config,
    );
    let patch_recovery_targets = state
        .map(patch_recovery_targets_from_state)
        .filter(|targets| !targets.is_empty())
        .unwrap_or_default();
    let interrupted_resume = false;
    let last_failure = None;
    let documentation_scope_targets = {
        let targets = documentation_scope_targets(
            &focus_seed_targets,
            history_items,
            history_start_index,
            focus,
        );
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
    let verification_failure_labels =
        recent_verification_failures_from_history(history_items, history_start_index)
            .unwrap_or_default();
    let inactive_target_edit_recovery_targets =
        latest_wrong_authoring_target_rejection_for_prompt(history_items, history_start_index)
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
    let verification_repair_cycle = latest_verification_repair_cycle_from_history_items(
        history_items,
        history_start_index,
        prompt_input.workspace_cwd,
    );
    let verification_repair_focus_required =
        latest_verification_repair_focus_required_from_history_items(
            history_items,
            history_start_index,
        );
    let verification_repair_read_budget_exhausted = verification_repair_focus_required
        .as_ref()
        .is_some_and(|state| state.read_budget_exhausted);
    let staged_task_output_targets_changed = staged_task_output_targets_changed_after_latest_user(
        history_items,
        history_start_index,
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
            history_items,
            history_start_index,
            &staged_task_output_targets,
        );
    let staged_task_closeout_recovery_mode = staged_task_closeout_mode
        && !staged_task_closeout_read_complete
        && (no_tool_authoring_error_stall || pseudo_tool_call_stall || invalid_tool_stall);
    let staged_task_closeout_repair_targets = staged_task_closeout_mode
        .then(|| {
            latest_denied_edit_targets_after_latest_user_from_history(
                history_items,
                history_start_index,
            )
        })
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
        latest_staged_task_documentation_audit_state_from_history(
            history_items,
            history_start_index,
            &staged_task_documentation_focus_targets,
        )
    })
    .flatten();
    let staged_task_documentation_evidence_snapshot = staged_task_documentation_authoring_mode
        .then(|| {
            staged_task_documentation_evidence_snapshot(
                history_items,
                history_start_index,
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
                history_items,
                history_start_index,
                &staged_task_documentation_focus_targets,
            );
    let verification_requirements = verification_requirements(latest_user_text.as_deref(), todos);
    let verification_freshness_targets =
        verification_freshness_targets_after_latest_user(history_items, history_start_index, todos);
    let verification_evidence = verification_evidence_after_latest_user_with_freshness(
        history_items,
        history_start_index,
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
        latest_verification_repair_target_rotation_required_target_from_history_items(
            history_items,
            history_start_index,
        );
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
                .then(|| {
                    verification_repair_rotated_focus_target(
                        history_items,
                        history_start_index,
                        state,
                    )
                })
                .flatten()
        });
    let verification_repair_import_focus_target = verification_failure_repair_mode
        .then(|| state.and_then(verification_repair_import_export_focus_target))
        .flatten();
    let verification_repair_feedback_focus_target = verification_repair_focus_required
        .as_ref()
        .and_then(|state| state.focus_target.clone());
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
    let verification_repair_active_targets = state
        .and_then(|state| state.failure.as_ref())
        .map(|failure| failure.targets.clone())
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
        compaction_replay: latest_compaction_history_index(history_items).is_some(),
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
        staged_task_recovery_stall,
        inactive_target_edit_recovery_mode,
        inactive_target_edit_recovery_targets,
        inactive_target_edit_recovery_read_target: None,
        edit_recovery_mode,
        patch_recovery_mode,
        patch_recovery_targets,
        verification_failure_repair_mode,
        verification_repair_rerun_due,
        verification_pending_without_open_work,
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

fn apply_active_content_shape_to_write_schema(
    tools: &mut [ToolSchema],
    state: &SessionStateSnapshot,
) {
    if state.completion.closeout_ready
        || !matches!(
            state.process_phase,
            ProcessPhase::Author | ProcessPhase::Repair
        )
        || state.active_targets.len() != 1
    {
        return;
    }
    let target = state.active_targets[0].as_str();
    apply_write_content_shape_to_write_schema_for_target(tools, target);
}

pub(crate) fn apply_write_content_shape_to_write_schema_for_required_action(
    tools: &mut [ToolSchema],
    required_action: Option<&RequiredAction>,
) {
    let Some(target) = required_action
        .filter(|action| action.tool == ToolName::Write)
        .and_then(RequiredAction::edit_target)
        .map(Utf8Path::as_str)
        .map(str::trim)
        .filter(|target| !target.is_empty())
    else {
        return;
    };
    apply_write_content_shape_to_write_schema_for_target(tools, target);
}

fn apply_write_content_shape_to_write_schema_for_target(tools: &mut [ToolSchema], target: &str) {
    let description =
        crate::agent::content_shape_contract::artifact_content_shape_tool_schema_description(
            target,
        );
    let Some(description) = description else {
        return;
    };
    for tool in tools.iter_mut().filter(|tool| tool.name == "write") {
        if let Some(content_description) = tool
            .input_schema
            .pointer_mut("/properties/content/description")
        {
            *content_description = Value::String(description.clone());
        }
        if let Some(path_schema) = tool.input_schema.pointer_mut("/properties/path") {
            let mut constrained = path_schema.clone();
            if !constrained.is_object() {
                constrained = json!({"type": "string"});
            }
            if let Some(path_schema_object) = constrained.as_object_mut() {
                path_schema_object.insert("type".to_string(), Value::String("string".to_string()));
                path_schema_object.insert(
                    "description".to_string(),
                    Value::String(format!(
                        "Exact target path for this turn. Must be `{target}`."
                    )),
                );
                path_schema_object.insert("enum".to_string(), json!([target]));
            }
            *path_schema = constrained;
        }
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
    if !target_is_test_like(target) {
        return None;
    }
    Some(target.to_string())
}

fn inactive_target_recovery_authoring_tool(tool_names: &[String]) -> Option<String> {
    if tool_names.iter().any(|tool| tool == "apply_patch") {
        return Some("apply_patch".to_string());
    }
    if tool_names.iter().any(|tool| tool == "write") {
        return Some("write".to_string());
    }
    tool_names
        .iter()
        .find(|tool| tool.as_str() != "read")
        .cloned()
}

fn latest_content_shape_repair_contract(
    history_items: &[HistoryItem],
    state: &SessionStateSnapshot,
) -> Option<String> {
    let active_targets = state
        .active_targets
        .iter()
        .map(|target| target.as_str().to_string())
        .collect::<Vec<_>>();
    if active_targets.is_empty() {
        return None;
    }
    history_items.iter().rev().find_map(|item| {
        let HistoryItemPayload::ToolOutput { metadata, .. } = &item.payload else {
            return None;
        };
        if !tool_output_payload_is_content_shape_mismatch(metadata) {
            return None;
        }
        let contract = metadata
            .get("content_shape_contract")
            .or_else(|| metadata.pointer("/tool_feedback_envelope/content_shape_contract"))?;
        let target = contract
            .get("target")
            .and_then(Value::as_str)
            .filter(|target| {
                prompt_target_matches_required_output(target, &active_targets)
                    || active_targets.iter().any(|active| {
                        prompt_target_matches_required_output(active, &[target.to_string()])
                    })
            })?;
        match contract.get("kind").and_then(Value::as_str) {
            Some("text_artifact_readable_content_shape") => {
                Some(crate::agent::content_shape_contract::text_artifact_prompt_contract(target))
            }
            Some("python_test_module_content_shape") => Some(exact_write_target_contract(target)),
            Some("python_source_executable_content_shape") => {
                crate::agent::content_shape_contract::artifact_content_shape_prompt_contract(target)
            }
            Some("generic_code_artifact_effective_content_shape") => {
                Some(crate::agent::content_shape_contract::code_artifact_prompt_contract(target))
            }
            _ => None,
        }
    })
}

fn latest_content_shape_repair_contract_for_prompt(
    history_items: &[HistoryItem],
    state: &SessionStateSnapshot,
) -> Option<String> {
    let start_index = prompt_history_window_start_index(history_items).min(history_items.len());
    latest_content_shape_repair_contract(&history_items[start_index..], state)
}

pub(crate) fn content_shape_repair_contract_uses_canonical_history_window_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let stale_metadata = json!({
        "operation_progress_class": "required_write_content_shape_mismatch",
        "progress_effect": "no_progress",
        "content_shape_contract": {
            "kind": "python_test_module_content_shape",
            "target": "tests/workflow_contract.py"
        },
        "tool_feedback_envelope": {
            "kind": "required_write_content_shape_mismatch",
            "operation_progress_class": "required_write_content_shape_mismatch",
            "progress_effect": "no_progress",
            "content_shape_contract": {
                "kind": "python_test_module_content_shape",
                "target": "tests/workflow_contract.py"
            }
        }
    });
    let history_items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: ToolLifecycleStatus::Completed,
                title: "Required write content shape mismatch".to_string(),
                output_text: "stale pre-compaction content-shape feedback".to_string(),
                metadata: stale_metadata,
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("stale-content-shape".to_string()),
                verification_run: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::PreTurn,
                summary: "CompactionContinuity\nstale content-shape feedback summarized"
                    .to_string(),
                replacement_item_ids: Vec::new(),
                continuation: None,
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
                    text: "continue current work".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
    ];
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("tests/workflow_contract.py")];
    state.completion.open_work_count = 1;

    prompt_history_window_start_index(&history_items) == 2
        && latest_content_shape_repair_contract_for_prompt(&history_items, &state).is_none()
}

pub(crate) fn prompt_content_shape_window_fixture_workflow_neutral_fixture_passes() -> bool {
    content_shape_repair_contract_uses_canonical_history_window_fixture_passes()
}

pub(crate) fn provider_replay_compaction_boundary_uses_canonical_history_order_fixture_passes()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "provider replay canonical compaction order".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 5,
        completed_at_ms: None,
    };
    let old_compaction = HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 2,
        payload: HistoryItemPayload::Compaction {
            mode: crate::protocol::CompactionMode::PreTurn,
            summary: "OLD_COMPACTION_CONTEXT_SHOULD_NOT_BE_CURRENT".to_string(),
            replacement_item_ids: Vec::new(),
            continuation: None,
        },
    };
    let latest_compaction = HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 4,
        created_at_ms: 4,
        payload: HistoryItemPayload::Compaction {
            mode: crate::protocol::CompactionMode::PreTurn,
            summary: "LATEST_COMPACTION_CONTEXT_IS_AUTHORITY".to_string(),
            replacement_item_ids: Vec::new(),
            continuation: None,
        },
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
                    text: "start work".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        latest_compaction,
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "continue after latest compaction".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
        old_compaction,
    ];

    let replay = build_provider_replay_messages_from_history_items(&session, &history_items, 16);
    let serialized = serde_json::to_string(&replay).unwrap_or_default();
    serialized.contains("LATEST_COMPACTION_CONTEXT_IS_AUTHORITY")
        && !serialized.contains("OLD_COMPACTION_CONTEXT_SHOULD_NOT_BE_CURRENT")
        && serialized.contains("continue after latest compaction")
        && session.model == PROMPT_FIXTURE_MODEL
        && session.base_url == PROMPT_FIXTURE_BASE_URL
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
        crate::session::DocsArea::Backend => "backend/",
        crate::session::DocsArea::Frontend => "frontend/",
        crate::session::DocsArea::Tests => "tests/",
        crate::session::DocsArea::Data => "data/",
        crate::session::DocsArea::Examples => "examples/",
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

fn latest_user_text_from_history_items(
    history_items: &[HistoryItem],
    start_index: usize,
) -> Option<String> {
    let latest_user = latest_user_turn_index_after(history_items, start_index)?;
    history_user_text(&history_items[latest_user])
}

fn history_user_text(item: &HistoryItem) -> Option<String> {
    let content = match &item.payload {
        HistoryItemPayload::UserTurn { content, .. }
        | HistoryItemPayload::Message {
            role: MessageRole::User,
            content,
            ..
        } => content,
        _ => return None,
    };
    let text = content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
}

fn has_historical_turns_before_latest_user(
    history_items: &[HistoryItem],
    start_index: usize,
) -> bool {
    let Some(latest_user) = latest_user_turn_index_after(history_items, start_index) else {
        return false;
    };
    history_items[start_index..latest_user].iter().any(|item| {
        matches!(
            item.payload,
            HistoryItemPayload::UserTurn { .. }
                | HistoryItemPayload::Message {
                    role: MessageRole::User | MessageRole::Assistant,
                    ..
                }
        )
    })
}

fn latest_user_editor_context_targets(
    history_items: &[HistoryItem],
    start_index: usize,
) -> Vec<String> {
    let Some(latest_user) = latest_user_turn_index_after(history_items, start_index) else {
        return Vec::new();
    };
    let Some(editor_context) = (match &history_items[latest_user].payload {
        HistoryItemPayload::UserTurn { editor_context, .. } => editor_context.as_ref(),
        _ => None,
    }) else {
        return Vec::new();
    };

    editor_context_artifact_targets(editor_context)
}

fn editor_context_artifact_targets(editor_context: &crate::session::EditorContext) -> Vec<String> {
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
    history_items: &[HistoryItem],
    start_index: usize,
) -> Vec<String> {
    let Some(latest_user) = latest_user_turn_index_after(history_items, start_index) else {
        return Vec::new();
    };

    for item in history_items[start_index..latest_user].iter().rev() {
        let targets = dedupe_targets(
            documentation_scope_targets_from_history_item(item)
                .into_iter()
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

fn observe_follow_up_activity(
    history_items: &[HistoryItem],
    start_index: usize,
) -> FollowUpActivity {
    let Some(latest_user) = latest_user_turn_index_after(history_items, start_index) else {
        return FollowUpActivity::default();
    };

    let mut activity = FollowUpActivity::default();
    let mut pending_read_calls: HashMap<String, Vec<String>> = HashMap::new();
    for item in history_items.iter().skip(latest_user + 1) {
        match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                effective_arguments,
                ..
            } => {
                if is_readonly_tool_name(&tool.to_string()) {
                    let targets = artifact_targets_from_history_tool_call(
                        tool,
                        arguments,
                        effective_arguments,
                    );
                    if !targets.is_empty() {
                        pending_read_calls.insert(call_id.to_string(), targets);
                    }
                }
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                success,
                progress_effect,
                ..
            } if history_tool_output_is_successful(*status, *success, progress_effect) => {
                if let Some(targets) = pending_read_calls.remove(&call_id.to_string()) {
                    record_follow_up_activity_targets(&mut activity, targets, false);
                }
            }
            HistoryItemPayload::FileChange { changes, .. } => {
                let targets = changes
                    .iter()
                    .filter_map(|change| change.path_after.as_ref().or(change.path_before.as_ref()))
                    .map(|path| path.as_str().to_string())
                    .collect::<Vec<_>>();
                record_follow_up_activity_targets(&mut activity, targets, true);
            }
            _ => {}
        }
    }

    activity
}

fn record_follow_up_activity_targets(
    activity: &mut FollowUpActivity,
    targets: Vec<String>,
    is_write: bool,
) {
    for target in targets {
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

fn is_readonly_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "read" | "list" | "glob" | "grep" | "inspect_directory"
    )
}

fn recent_tool_call_stalled_with_config(
    history_items: &[HistoryItem],
    start_index: usize,
    follow_up_implementation: bool,
    agent_config: &AgentConfig,
) -> (bool, Vec<String>) {
    let Some(latest_user) = latest_user_turn_index_after(history_items, start_index) else {
        return (false, Vec::new());
    };
    let mut readonly_calls: HashMap<String, (String, String)> = HashMap::new();
    let mut completed_activity = Vec::new();
    for item in history_items.iter().skip(latest_user + 1) {
        match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                effective_arguments,
                ..
            } => {
                if let Some(target) = extract_readonly_target_from_value(
                    tool,
                    history_tool_arguments(arguments, effective_arguments),
                ) {
                    readonly_calls.insert(call_id.to_string(), (tool.to_string(), target));
                }
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                success,
                progress_effect,
                ..
            } if history_tool_output_is_successful(*status, *success, progress_effect) => {
                if let Some((tool, target)) = readonly_calls.get(&call_id.to_string()) {
                    completed_activity.push((tool.clone(), Some(target.clone())));
                }
            }
            HistoryItemPayload::FileChange { .. } => {
                completed_activity.push(("__write__".to_string(), None));
            }
            _ => {}
        }
    }
    let recent_calls = completed_activity
        .into_iter()
        .rev()
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

fn documentation_scope_targets(
    requested_targets: &[String],
    history_items: &[HistoryItem],
    start_index: usize,
    focus: FollowUpFocus,
) -> Vec<String> {
    let Some(latest_user) = latest_user_turn_index_after(history_items, start_index) else {
        return Vec::new();
    };
    let observed_targets = dedupe_targets(
        history_items[latest_user + 1..]
            .iter()
            .flat_map(|item| documentation_scope_targets_from_history_item(item))
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

fn documentation_scope_targets_from_history_item(item: &HistoryItem) -> Vec<String> {
    match &item.payload {
        HistoryItemPayload::ToolCall {
            tool,
            arguments,
            effective_arguments,
            ..
        } => artifact_targets_from_history_tool_call(tool, arguments, effective_arguments),
        HistoryItemPayload::FileChange { changes, .. } => changes
            .iter()
            .filter_map(|change| change.path_after.as_ref().or(change.path_before.as_ref()))
            .map(|path| path.as_str().to_string())
            .collect(),
        _ => Vec::new(),
    }
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
    let mut reference_authority_section_active = false;
    for raw_line in instruction_authority_lines(text) {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let (line_without_code, code_spans) = split_backtick_spans(line);
        let normalized_line = line.to_ascii_lowercase().replace('`', "");
        let reference_section_header = reference_authority_section_header(&normalized_line);
        let reference_section_item = reference_authority_section_active
            && (reference_section_list_item(line)
                || reference_section_header
                || line_has_explicit_reference_input_marker(&normalized_line));
        if reference_authority_section_active
            && !reference_section_item
            && !reference_section_header
        {
            reference_authority_section_active = false;
        }
        if reference_section_header {
            reference_authority_section_active = true;
        }
        let line_intent = if reference_authority_section_active
            && (reference_section_item || reference_section_header)
        {
            RequestedLineIntent::Reference
        } else {
            classify_requested_line_intent(line, &line_without_code, &code_spans)
        };
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
            for target in artifact_tokens_from_instruction_segment(trimmed) {
                record_requested_target(&mut contract, &target, line_intent);
            }
        }

        for token in line_without_code
            .split(artifact_token_separator)
            .flat_map(artifact_tokens_from_instruction_segment)
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
    promote_same_document_reference_updates(text, &mut contract);

    RequestedWorkContract {
        deliverable_targets: dedupe_targets(contract.deliverable_targets),
        reference_inputs: dedupe_targets(contract.reference_inputs),
        example_targets: dedupe_targets(contract.example_targets),
        naming_patterns: dedupe_targets(contract.naming_patterns),
        verification_commands: dedupe_targets(contract.verification_commands),
    }
}

fn promote_same_document_reference_updates(text: &str, contract: &mut RequestedWorkContract) {
    if !contract.deliverable_targets.is_empty() || !same_document_update_alias_requested(text) {
        return;
    }

    for reference in &contract.reference_inputs {
        if classify_artifact_target(reference) == ArtifactTargetKind::Documentation
            && !matches!(
                Utf8Path::new(reference).file_name(),
                Some("scenario_contract.md" | "scenario_contract.json")
            )
        {
            contract.deliverable_targets.push(reference.clone());
        }
    }
}

fn reference_authority_section_header(normalized_line: &str) -> bool {
    let header = normalized_line
        .trim()
        .trim_start_matches("- ")
        .trim_end_matches(':')
        .trim();
    matches!(
        header,
        "scenario contract authority"
            | "scenario contract"
            | "contract authority"
            | "contract references"
            | "reference inputs"
            | "reference input"
            | "context references"
            | "context inputs"
    ) || (header.contains("contract")
        && header.contains("reference")
        && !header.contains("deliverable"))
}

fn reference_section_list_item(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
        || trimmed
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_digit())
            && trimmed
                .chars()
                .skip_while(|ch| ch.is_ascii_digit())
                .next()
                .is_some_and(|ch| ch == '.' || ch == ')')
}

pub(crate) fn same_document_update_alias_requested(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let references_existing_document = [
        "based on",
        "use the previous",
        "from the previous",
        "existing document",
        "same document",
        "just-created document",
        "just created document",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || text.contains("をもとに")
        || text.contains("を基に")
        || text.contains("前回作成")
        || text.contains("いま作成")
        || text.contains("今作成")
        || text.contains("作成した")
        || text.contains("既存")
        || text.contains("現在の");
    let updates_same_document = [
        "update the document",
        "update the docs",
        "update docs only",
        "documentation only",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || text.contains("設計書だけを更新")
        || text.contains("設計書のみを更新")
        || text.contains("設計書を更新")
        || (text.contains("設計書") && text.contains("更新"))
        || text.contains("文書だけを更新")
        || text.contains("文書のみを更新")
        || (text.contains("文書") && text.contains("更新"))
        || text.contains("ドキュメントだけを更新")
        || text.contains("ドキュメントのみを更新")
        || (text.contains("ドキュメント") && text.contains("更新"));
    references_existing_document && updates_same_document
}

fn explicit_artifact_targets_in_text(text: &str) -> Vec<String> {
    artifact_targets_from_instruction_text(text, true)
}

fn artifact_targets_from_instruction_text(text: &str, include_unknown: bool) -> Vec<String> {
    let mut targets = Vec::new();
    for raw_line in instruction_authority_lines(text) {
        let (line_without_code, code_spans) = split_backtick_spans(raw_line);
        for code_span in code_spans {
            for target in artifact_tokens_from_instruction_segment(code_span.trim()) {
                if include_unknown
                    || classify_artifact_target(&target) != ArtifactTargetKind::Unknown
                {
                    targets.push(target);
                }
            }
        }
        targets.extend(
            line_without_code
                .split(artifact_token_separator)
                .flat_map(artifact_tokens_from_instruction_segment)
                .filter(|target| {
                    include_unknown
                        || classify_artifact_target(target) != ArtifactTargetKind::Unknown
                }),
        );
    }
    dedupe_targets(targets)
}

fn instruction_authority_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut skipping_non_authority_section = false;
    let expected_artifacts_are_continuation_evidence =
        typed_continuation_expected_artifacts_are_evidence(text);

    for raw_line in text.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            lines.push(raw_line);
            continue;
        }
        if non_authority_context_section_header(
            trimmed,
            expected_artifacts_are_continuation_evidence,
        ) {
            skipping_non_authority_section = true;
            continue;
        }
        if skipping_non_authority_section {
            if non_diagnostic_authority_section_header(trimmed) {
                skipping_non_authority_section = false;
            } else {
                continue;
            }
        }
        if diagnostic_traceback_line(trimmed) {
            continue;
        }
        lines.push(raw_line);
    }

    lines
}

fn typed_continuation_expected_artifacts_are_evidence(text: &str) -> bool {
    let mut has_expected_artifacts = false;
    let mut has_actionable_continuation_section = false;
    for raw_line in text.lines() {
        let normalized = normalized_instruction_section_header(raw_line);
        match normalized.as_str() {
            "repair targets"
            | "missing expected artifacts"
            | "open obligations"
            | "required verification still missing" => has_actionable_continuation_section = true,
            "expected artifacts" => has_expected_artifacts = true,
            "failed required verification commands"
            | "required verification failed in the latest evidence"
            | "latest verification failure evidence"
            | "verification failure evidence" => has_actionable_continuation_section = true,
            _ => {}
        }
    }
    has_actionable_continuation_section
        && has_expected_artifacts
        && instruction_text_has_typed_continuation_contract(text)
}

fn instruction_text_has_typed_continuation_contract(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("typed continuation contract")
        || lower.contains("continuationcontract")
        || lower.contains("compactioncontinuity")
        || lower.contains("turncontrolenvelope")
        || lower.contains("verification-repair continuation")
        || lower.contains("closeout continuation")
        || lower.contains("verification continuation")
        || lower.contains("repair continuation")
        || lower.contains("latest required verification command failed")
        || lower.contains("rerun the failed required verification")
        || lower.contains("after the repair edit")
}

pub fn requested_work_parser_does_not_use_manual_st_harness_marker_fixture_passes() -> bool {
    let harness_named_continuation = "\
Manual ST continuation

Expected artifacts:
- docs/probe-output.md

Open obligations:
- src/main.rs
";
    let harness_contract =
        requested_work_contract_from_instruction_text(harness_named_continuation);
    if !harness_contract
        .deliverable_targets
        .iter()
        .any(|target| target == "docs/probe-output.md")
    {
        return false;
    }

    let typed_continuation = "\
Typed continuation contract: ContinuationContract

Expected artifacts:
- docs/prior-evidence.md

Open obligations:
- src/main.rs
";
    let typed_contract = requested_work_contract_from_instruction_text(typed_continuation);
    !typed_contract
        .deliverable_targets
        .iter()
        .any(|target| target == "docs/prior-evidence.md")
        && typed_contract
            .deliverable_targets
            .iter()
            .any(|target| target == "src/main.rs")
}

fn normalized_instruction_section_header(line: &str) -> String {
    line.trim()
        .trim_start_matches("- ")
        .trim_end_matches(':')
        .trim()
        .to_ascii_lowercase()
}

fn non_authority_context_section_header(
    line: &str,
    expected_artifacts_are_continuation_evidence: bool,
) -> bool {
    if diagnostic_evidence_section_header(line) {
        return true;
    }
    let normalized = normalized_instruction_section_header(line);
    if expected_artifacts_are_continuation_evidence && normalized == "expected artifacts" {
        return true;
    }
    matches!(
        normalized.as_str(),
        "previous final assistant message"
            | "previous assistant message"
            | "previous final assistant response"
            | "previous final response"
            | "previous assistant response"
            | "prior final assistant message"
            | "prior assistant message"
            | "prior final response"
            | "previous final answer"
            | "prior final answer"
            | "assistant summary"
            | "previous assistant summary"
            | "prior assistant summary"
            | "previous closeout message"
            | "prior closeout message"
    )
}

fn diagnostic_evidence_section_header(line: &str) -> bool {
    let normalized = normalized_instruction_section_header(line);
    matches!(
        normalized.as_str(),
        "latest verification failure evidence"
            | "verification failure evidence"
            | "latest failure evidence"
            | "failure evidence"
            | "request diagnostics"
            | "diagnostics"
            | "stdout"
            | "stderr"
            | "traceback"
    )
}

fn non_diagnostic_authority_section_header(line: &str) -> bool {
    let normalized = normalized_instruction_section_header(line);
    matches!(
        normalized.as_str(),
        "repair targets"
            | "expected artifacts"
            | "open obligations"
            | "required verification still missing"
            | "failed required verification commands"
            | "required verification failed in the latest evidence"
            | "required verification commands"
    )
}

fn diagnostic_traceback_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("file \"")
        || lower.starts_with("file '")
        || lower.starts_with("traceback (most recent call last)")
        || lower.starts_with("unicodeerror:")
        || lower.starts_with("unicodedecodeerror:")
        || lower.starts_with("typeerror:")
        || lower.starts_with("assertionerror:")
        || lower.starts_with("importerror:")
        || lower.starts_with("modulenotfounderror:")
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

    for phrase in [
        "do not change",
        "don't change",
        "without changing",
        "do not edit",
        "don't edit",
        "without editing",
    ] {
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
        if !line_has_explicit_reference_input_marker(&normalized_line) {
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
        "document the current design",
        "describe the current design",
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
        "specification changes",
        "specification becomes",
        "update the specification",
        "update the contract",
        "contract changes",
        "new capability",
        "new behavior",
        "new public behavior",
        "add support for",
        "support new",
        "extend behavior",
        "extend the behavior",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || text.contains("仕様へ更新")
        || text.contains("扱える仕様")
        || text.contains("仕様変更")
        || text.contains("新機能")
        || text.contains("機能追加")
        || text.contains("新しい挙動")
        || text.contains("公開挙動を追加");
    let implementation_locked = documentation_only_follow_up_requested(text);

    implementation_locked && deferred_turn && spec_shift
}

pub(crate) fn prompt_docs_followup_heuristic_domain_neutral_fixture_passes() -> bool {
    let generic_spec_shift = "\
For this turn, keep current implementation unchanged and only update docs.
The specification changes to add support for a new workflow validation behavior.
";
    let fact_only = "\
For this turn, keep current implementation unchanged and only update docs.
Document the current design and current implementation only.
";

    documentation_change_may_lead_implementation(generic_spec_shift)
        && !documentation_change_may_lead_implementation(fact_only)
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
    if line_explicitly_mutates_target(&normalized_line, &lower_target) {
        return false;
    }
    if line_documents_target_into_separate_document(&normalized_line, &lower_target) {
        return true;
    }
    if [
        format!("{lower_target} を参照"),
        format!("{lower_target} を参考"),
        format!("{lower_target} に従って"),
        format!("{lower_target} に合わせて"),
        format!("{lower_target} に合わせ"),
        format!("{lower_target} をもとに"),
        format!("{lower_target} を基に"),
        format!("{lower_target} に基づ"),
        format!("{lower_target} に沿って"),
        format!("{lower_target} に準拠"),
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

    if normalized_line.contains(&format!("{lower_target} の"))
        && [
            "合わせて",
            "に合わせ",
            "もとに",
            "基に",
            "基づ",
            "沿って",
            "準拠",
            "参照",
            "参考",
            "従って",
            "仕様",
            "設計",
            "要件",
            "requirements",
            "spec",
            "specification",
            "design",
        ]
        .into_iter()
        .any(|marker| normalized_line.contains(marker))
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

fn line_documents_target_into_separate_document(normalized_line: &str, lower_target: &str) -> bool {
    if classify_artifact_target(lower_target) != ArtifactTargetKind::Implementation {
        return false;
    }

    let mentions_target_as_subject = [
        format!("{lower_target} の"),
        format!("about {lower_target}"),
        format!("{lower_target} usage"),
        format!("{lower_target} how to"),
    ]
    .into_iter()
    .any(|pattern| normalized_line.contains(&pattern));
    if !mentions_target_as_subject {
        return false;
    }

    let documents_usage_or_behavior = [
        "使い方",
        "使用方法",
        "利用方法",
        "実行方法",
        "テスト実行方法",
        "テスト方法",
        "確認方法",
        "説明",
        "解説",
        "usage",
        "how to use",
        "how-to",
        "test command",
        "test instructions",
        "describe",
        "document",
    ]
    .into_iter()
    .any(|marker| normalized_line.contains(marker));
    if !documents_usage_or_behavior {
        return false;
    }

    artifact_targets_from_instruction_text(normalized_line, false)
        .into_iter()
        .any(|target| {
            target.to_ascii_lowercase() != lower_target
                && classify_artifact_target(&target) == ArtifactTargetKind::Documentation
        })
}

fn line_explicitly_mutates_target(normalized_line: &str, lower_target: &str) -> bool {
    if !normalized_line.contains(lower_target) {
        return false;
    }

    [
        format!("{lower_target} を作成"),
        format!("{lower_target} を生成"),
        format!("{lower_target} を更新"),
        format!("{lower_target} を修正"),
        format!("{lower_target} を編集"),
        format!("{lower_target} を追記"),
        format!("{lower_target} を追加"),
        format!("{lower_target} を変更"),
        format!("{lower_target} を書き"),
        format!("{lower_target} に追記"),
        format!("{lower_target} に追加"),
        format!("{lower_target} へ追記"),
        format!("{lower_target} へ追加"),
        format!("create {lower_target}"),
        format!("write {lower_target}"),
        format!("update {lower_target}"),
        format!("modify {lower_target}"),
        format!("edit {lower_target}"),
        format!("rewrite {lower_target}"),
        format!("generate {lower_target}"),
        format!("add {lower_target}"),
    ]
    .into_iter()
    .any(|pattern| normalized_line.contains(&pattern))
}

fn line_target_is_protected_reference_input(normalized_line: &str, lower_target: &str) -> bool {
    if !normalized_line.contains(lower_target) {
        return false;
    }

    if target_is_contract_reference(lower_target)
        && line_has_explicit_reference_input_marker(normalized_line)
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

fn line_has_explicit_reference_input_marker(normalized_line: &str) -> bool {
    [
        "reference input",
        "reference inputs",
        "contract reference",
        "contract references",
        "context reference",
        "context references",
        "protected reference",
        "read-only reference",
        "readonly reference",
        "external evidence",
        "external reference",
        "参照入力",
        "参照資料",
        "契約参照",
        "設計参照",
        "根拠資料",
    ]
    .into_iter()
    .any(|marker| normalized_line.contains(marker))
}

fn target_is_contract_reference(target: &str) -> bool {
    let normalized = target.replace('\\', "/").to_ascii_lowercase();
    let filename = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    filename.contains("contract")
}

pub fn requested_work_parser_does_not_use_case_stage_or_harness_owned_markers_fixture_passes()
-> bool {
    let diagnostic_case_stage = "\
Request diagnostics:
- stale generated output docs/stale-evidence.md

Case:
- src/from_case.rs

Stage:
- src/from_stage.rs

Verification attempt:
- src/from_attempt.rs

Open obligations:
- src/active.rs
";
    let diagnostic_contract = requested_work_contract_from_instruction_text(diagnostic_case_stage);
    if diagnostic_contract
        .deliverable_targets
        .iter()
        .any(|target| {
            matches!(
                target.as_str(),
                "src/from_case.rs" | "src/from_stage.rs" | "src/from_attempt.rs"
            )
        })
    {
        return false;
    }
    if !diagnostic_contract
        .deliverable_targets
        .iter()
        .any(|target| target == "src/active.rs")
    {
        return false;
    }

    let harness_owned_reference = "harness-owned docs/runtime_contract.md";
    if extract_protected_artifact_targets(harness_owned_reference)
        .iter()
        .any(|target| target == "docs/runtime_contract.md")
    {
        return false;
    }
    let harness_owned_contract =
        requested_work_contract_from_instruction_text(harness_owned_reference);
    !harness_owned_contract
        .reference_inputs
        .iter()
        .any(|target| target == "docs/runtime_contract.md")
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
        || lower.contains("verification");

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

fn artifact_token_separator(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            ',' | '、'
                | '，'
                | '。'
                | '；'
                | ';'
                | '：'
                | ':'
                | '（'
                | '）'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
        )
}

fn artifact_tokens_from_instruction_segment(segment: &str) -> Vec<String> {
    normalize_artifact_token(segment).into_iter().collect()
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
    let candidate = truncate_after_artifact_extension(candidate).unwrap_or(candidate);
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

fn truncate_after_artifact_extension(candidate: &str) -> Option<&str> {
    let lower = candidate.to_ascii_lowercase();
    let mut selected_end = None;
    for extension in ARTIFACT_TOKEN_EXTENSIONS {
        let mut search_from = 0usize;
        while let Some(offset) = lower[search_from..].find(extension) {
            let start = search_from + offset;
            let end = start + extension.len();
            let next = candidate[end..].chars().next();
            if next.is_none_or(|ch| !artifact_extension_continuation(ch)) {
                selected_end = Some(selected_end.map_or(end, |current: usize| current.min(end)));
                break;
            }
            search_from = end;
        }
    }
    selected_end.map(|end| &candidate[..end])
}

fn artifact_extension_continuation(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\')
}

const ARTIFACT_TOKEN_EXTENSIONS: &[&str] = &[
    ".rs", ".py", ".js", ".ts", ".tsx", ".jsx", ".mjs", ".cjs", ".go", ".java", ".kt", ".swift",
    ".c", ".h", ".hpp", ".cpp", ".cc", ".cs", ".rb", ".php", ".sh", ".ps1", ".bat", ".cmd", ".sql",
    ".html", ".css", ".scss", ".sass", ".vue", ".svelte", ".json", ".toml", ".yaml", ".yml",
    ".xml", ".ini", ".env", ".md", ".rst", ".adoc", ".txt", ".csv", ".tsv", ".pdf", ".docx",
    ".xlsx", ".pptx",
];

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

    let spec = classify_language_artifact_target(target);
    if matches!(spec.role, ArtifactRole::Source | ArtifactRole::Test) || lower.contains("/src/") {
        return ArtifactTargetKind::Implementation;
    }

    ArtifactTargetKind::Unknown
}

pub(crate) fn prompt_artifact_target_kind_uses_language_adapter_fixture_passes() -> bool {
    classify_artifact_target("src/workflow.ts") == ArtifactTargetKind::Implementation
        && classify_artifact_target("tests/workflow.spec.tsx") == ArtifactTargetKind::Implementation
        && classify_artifact_target("docs/workflow-design.md") == ArtifactTargetKind::Documentation
        && classify_artifact_target("task.md") == ArtifactTargetKind::Unknown
}

pub(crate) fn verification_repair_prompt_uses_language_projection_fixture_passes() -> bool {
    crate::agent::prompt_assets::verification_repair_prompt_uses_language_projection_fixture_passes(
    )
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

fn staged_task_artifacts_seen(history_items: &[HistoryItem], start_index: usize) -> Vec<String> {
    let mut artifacts = Vec::new();
    for item in history_items.iter().skip(start_index) {
        if let HistoryItemPayload::ToolCall {
            tool,
            arguments,
            effective_arguments,
            ..
        } = &item.payload
        {
            for target in
                artifact_targets_from_history_tool_call(tool, arguments, effective_arguments)
            {
                if is_staged_task_artifact_target(&target) {
                    artifacts.push(target);
                }
            }
        }
    }
    dedupe_targets(artifacts)
}

fn staged_task_output_targets(
    history_items: &[HistoryItem],
    start_index: usize,
    staged_task_artifacts: &[String],
) -> Vec<String> {
    if staged_task_artifacts.is_empty() {
        return Vec::new();
    }

    let mut staged_task_read_calls = HashMap::new();
    let mut targets = Vec::new();
    for item in history_items.iter().skip(start_index) {
        match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                effective_arguments,
                ..
            } if *tool == ToolName::Read => {
                if let Some(target) = extract_readonly_target_from_value(
                    tool,
                    history_tool_arguments(arguments, effective_arguments),
                ) {
                    if is_staged_task_artifact_target(&target) {
                        staged_task_read_calls.insert(call_id.to_string(), target);
                    }
                }
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                output_text,
                success,
                progress_effect,
                ..
            } => {
                if staged_task_read_calls.contains_key(&call_id.to_string())
                    && history_tool_output_is_successful(*status, *success, progress_effect)
                {
                    targets.extend(extract_requested_artifact_targets(output_text));
                }
            }
            _ => {}
        };
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

fn history_tool_output_is_successful(
    status: ToolLifecycleStatus,
    success: Option<bool>,
    progress_effect: &crate::protocol::ToolProgressEffect,
) -> bool {
    status == ToolLifecycleStatus::Completed
        && success != Some(false)
        && !matches!(
            progress_effect,
            crate::protocol::ToolProgressEffect::NoProgress
                | crate::protocol::ToolProgressEffect::Blocked
                | crate::protocol::ToolProgressEffect::VerificationFailed
        )
}

fn artifact_targets_from_history_tool_call(
    tool: &ToolName,
    arguments: &Value,
    effective_arguments: &Value,
) -> Vec<String> {
    let args = history_tool_arguments(arguments, effective_arguments);
    if matches!(
        tool,
        ToolName::Read
            | ToolName::List
            | ToolName::Glob
            | ToolName::Grep
            | ToolName::InspectDirectory
    ) {
        return extract_readonly_target_from_value(tool, args)
            .into_iter()
            .collect();
    }

    if *tool == ToolName::ApplyPatch {
        return args
            .get("patch_text")
            .and_then(Value::as_str)
            .map(extract_patch_targets)
            .unwrap_or_default();
    }

    if *tool == ToolName::Write {
        return args
            .get("path")
            .and_then(Value::as_str)
            .map(|path| vec![path.to_string()])
            .unwrap_or_default();
    }

    Vec::new()
}

fn history_tool_arguments<'a>(arguments: &'a Value, effective_arguments: &'a Value) -> &'a Value {
    if effective_arguments.is_null() {
        arguments
    } else {
        effective_arguments
    }
}

fn extract_readonly_target_from_value(tool: &ToolName, arguments: &Value) -> Option<String> {
    if !matches!(
        tool,
        ToolName::Read
            | ToolName::List
            | ToolName::Glob
            | ToolName::Grep
            | ToolName::InspectDirectory
    ) {
        return None;
    }

    arguments
        .get("path")
        .and_then(Value::as_str)
        .or_else(|| arguments.get("pattern").and_then(Value::as_str))
        .or_else(|| arguments.get("query").and_then(Value::as_str))
        .map(|value| value.to_string())
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

fn recent_code_block_stall_from_history(history_items: &[HistoryItem], start_index: usize) -> bool {
    let Some(latest_user) = latest_user_history_index(history_items, start_index) else {
        return false;
    };

    for item in history_items.iter().skip(latest_user + 1).rev() {
        match &item.payload {
            HistoryItemPayload::FileChange { .. } => return false,
            HistoryItemPayload::RejectedToolProposal { proposal }
                if proposal.semantic_class == "text_final_while_obligations_open" =>
            {
                return rejected_final_message_contains_code_block(&proposal.original_arguments);
            }
            HistoryItemPayload::ToolOutput {
                status, metadata, ..
            } if *status == ToolLifecycleStatus::Completed
                && typed_final_text_drift_metadata(metadata) =>
            {
                return metadata_contains_code_block_text(metadata);
            }
            _ => {}
        }
    }

    false
}

fn rejected_final_message_contains_code_block(arguments: &Value) -> bool {
    arguments
        .get("text")
        .and_then(Value::as_str)
        .map(text_contains_code_block)
        .unwrap_or(false)
}

fn typed_final_text_drift_metadata(metadata: &Value) -> bool {
    matches!(
        tool_output_metadata_kind(metadata),
        Some("completion_drift" | "text_final_while_obligations_open" | "final_text_drift")
    ) && metadata_contains_code_block_text(metadata)
}

fn metadata_contains_code_block_text(metadata: &Value) -> bool {
    [
        "/tool_feedback_envelope/original_text",
        "/tool_feedback_envelope/final_text",
        "/tool_feedback_envelope/text",
        "/original_text",
        "/final_text",
        "/text",
    ]
    .iter()
    .any(|pointer| {
        metadata
            .pointer(pointer)
            .and_then(Value::as_str)
            .is_some_and(text_contains_code_block)
    })
}

fn text_contains_code_block(text: &str) -> bool {
    text.contains("```")
}

fn recent_pseudo_tool_call_rejection_from_history(
    history_items: &[HistoryItem],
    start_index: usize,
) -> bool {
    history_items
        .iter()
        .skip(start_index)
        .rev()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::RejectedToolProposal { proposal }
                if proposal.semantic_class == "text_final_while_obligations_open" =>
            {
                Some(rejected_final_message_contains_pseudo_tool_call_markup(
                    &proposal.original_arguments,
                ))
            }
            _ => None,
        })
        .unwrap_or(false)
}

fn rejected_final_message_contains_pseudo_tool_call_markup(arguments: &Value) -> bool {
    arguments
        .get("text")
        .and_then(Value::as_str)
        .map(contains_pseudo_tool_call_markup)
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

fn recent_invalid_tool_result_stall_from_history(
    history_items: &[HistoryItem],
    start_index: usize,
) -> bool {
    for item in history_items.iter().skip(start_index).rev() {
        match &item.payload {
            HistoryItemPayload::RejectedToolProposal { proposal } => {
                if matches!(
                    proposal.semantic_class.as_str(),
                    "invalid_tool"
                        | "tool_outside_allowed_surface"
                        | "provider_noncompliance"
                        | "malformed_tool_arguments"
                        | "schema_outside_tool_proposal"
                ) || proposal.resolved_tool == ToolName::Invalid
                {
                    return true;
                }
            }
            HistoryItemPayload::ToolOutput {
                status, metadata, ..
            } if *status == ToolLifecycleStatus::Completed => {
                if matches!(
                    tool_output_metadata_kind(metadata),
                    Some(
                        "invalid_tool_arguments"
                            | "invalid_edit_arguments"
                            | "schema_outside_tool_proposal"
                            | "tool_outside_allowed_surface"
                            | "provider_noncompliance"
                    )
                ) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

fn recent_nonprogress_recovery_result_stall_with_config(
    history_items: &[HistoryItem],
    start_index: usize,
    agent_config: &AgentConfig,
) -> bool {
    let required = agent_config.staged_task_recovery_stall_threshold;
    if required == 0 {
        return true;
    }
    let Some(latest_user) = latest_user_history_index(history_items, start_index) else {
        return false;
    };
    let mut consecutive_recovery_results = 0usize;
    for item in history_items.iter().skip(latest_user + 1).rev() {
        match &item.payload {
            HistoryItemPayload::ToolOutput {
                status,
                metadata,
                success,
                progress_effect,
                ..
            } if *status == ToolLifecycleStatus::Completed => {
                if !typed_recovery_tool_output_is_nonprogress(*success, progress_effect, metadata) {
                    return false;
                }
                consecutive_recovery_results += 1;
            }
            HistoryItemPayload::RejectedToolProposal { proposal } => {
                if !typed_rejected_proposal_is_nonprogress_recovery(
                    proposal.semantic_class.as_str(),
                ) {
                    return false;
                }
                consecutive_recovery_results += 1;
            }
            HistoryItemPayload::FileChange { .. } => return false,
            _ => continue,
        }
        if consecutive_recovery_results >= required {
            return true;
        }
        if consecutive_recovery_results >= RECENT_TOOL_CALL_WINDOW {
            return false;
        }
    }
    false
}

fn typed_recovery_tool_output_is_nonprogress(
    success: Option<bool>,
    progress_effect: &crate::protocol::ToolProgressEffect,
    metadata: &Value,
) -> bool {
    if success == Some(false)
        || matches!(
            progress_effect,
            crate::protocol::ToolProgressEffect::NoProgress
                | crate::protocol::ToolProgressEffect::Blocked
                | crate::protocol::ToolProgressEffect::VerificationFailed
        )
    {
        return true;
    }
    let metadata_progress_effect = metadata
        .pointer("/tool_feedback_envelope/progress_effect")
        .or_else(|| metadata.get("progress_effect"))
        .and_then(Value::as_str);
    if matches!(
        metadata_progress_effect,
        Some("no_progress" | "blocked" | "verification_failed")
    ) {
        return true;
    }
    matches!(
        tool_output_metadata_kind(metadata),
        Some(
            "wrong_authoring_target"
                | "invalid_tool_arguments"
                | "invalid_edit_arguments"
                | "schema_outside_tool_proposal"
                | "tool_outside_allowed_surface"
                | "provider_noncompliance"
                | "required_write_content_shape_mismatch"
                | "artifact_content_shape_no_progress"
                | "idempotent_file_write_no_progress"
                | "progress_projection"
        )
    )
}

fn typed_rejected_proposal_is_nonprogress_recovery(semantic_class: &str) -> bool {
    matches!(
        semantic_class,
        "invalid_tool"
            | "tool_outside_allowed_surface"
            | "provider_noncompliance"
            | "malformed_tool_arguments"
            | "schema_outside_tool_proposal"
            | "wrong_authoring_target"
            | "text_final_while_obligations_open"
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VerificationRepairFocusRequiredPromptState {
    read_budget_exhausted: bool,
    focus_target: Option<String>,
}

fn latest_verification_repair_focus_required_from_history_items(
    history_items: &[HistoryItem],
    start_index: usize,
) -> Option<VerificationRepairFocusRequiredPromptState> {
    let latest_user = latest_user_turn_index_after(history_items, start_index)?;
    for item in history_items.iter().skip(latest_user + 1).rev() {
        let HistoryItemPayload::ToolOutput {
            status,
            metadata,
            success,
            progress_effect,
            verification_run,
            ..
        } = &item.payload
        else {
            continue;
        };
        if *status != ToolLifecycleStatus::Completed {
            continue;
        }
        if verification_repair_focus_required_metadata(metadata) {
            return Some(VerificationRepairFocusRequiredPromptState {
                read_budget_exhausted: verification_repair_read_budget_exhausted_from_metadata(
                    metadata,
                ),
                focus_target: verification_repair_focus_target_from_metadata(metadata),
            });
        }
        if verification_repair_focus_required_cleared(
            *success,
            progress_effect,
            verification_run.as_ref(),
        ) {
            return None;
        }
    }
    None
}

fn latest_verification_repair_target_rotation_required_target_from_history_items(
    history_items: &[HistoryItem],
    start_index: usize,
) -> Option<String> {
    let latest_user = latest_user_turn_index_after(history_items, start_index)?;
    for item in history_items.iter().skip(latest_user + 1).rev() {
        let HistoryItemPayload::ToolOutput {
            status,
            metadata,
            success,
            progress_effect,
            verification_run,
            ..
        } = &item.payload
        else {
            continue;
        };
        if *status != ToolLifecycleStatus::Completed {
            continue;
        }
        if verification_repair_target_rotation_required_metadata(metadata) {
            return verification_repair_focus_target_from_metadata(metadata);
        }
        if verification_repair_focus_required_cleared(
            *success,
            progress_effect,
            verification_run.as_ref(),
        ) {
            return None;
        }
    }
    None
}

fn verification_repair_target_rotation_required_metadata(metadata: &Value) -> bool {
    matches!(
        tool_output_metadata_kind(metadata),
        Some("verification_repair_target_rotation_required")
    ) || metadata
        .get("verification_repair_target_rotation_required")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn verification_repair_focus_required_metadata(metadata: &Value) -> bool {
    matches!(
        tool_output_metadata_kind(metadata),
        Some("verification_repair_focus_required" | "verification_repair_read_budget_exhausted")
    ) || metadata
        .get("verification_repair_focus_required")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || metadata
            .get("verification_repair_read_budget_exhausted")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn verification_repair_read_budget_exhausted_from_metadata(metadata: &Value) -> bool {
    [
        "/tool_feedback_envelope/read_budget_exhausted",
        "/tool_feedback_envelope/verification_repair/read_budget_exhausted",
        "/verification_repair_read_budget_exhausted",
        "/read_budget_exhausted",
    ]
    .iter()
    .any(|pointer| {
        metadata
            .pointer(pointer)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    })
}

fn verification_repair_focus_target_from_metadata(metadata: &Value) -> Option<String> {
    [
        "/tool_feedback_envelope/required_next_action/target",
        "/tool_feedback_envelope/required_next_action/path",
        "/tool_feedback_envelope/required_write_path",
        "/tool_feedback_envelope/target",
        "/required_next_action/target",
        "/required_next_action/path",
        "/required_write_path",
        "/target",
    ]
    .iter()
    .find_map(|pointer| {
        metadata
            .pointer(pointer)
            .and_then(Value::as_str)
            .and_then(normalize_verification_repair_focus_target)
    })
    .or_else(|| {
        metadata
            .pointer("/tool_feedback_envelope/active_targets")
            .and_then(Value::as_array)
            .and_then(|targets| {
                targets.iter().find_map(|target| {
                    target
                        .as_str()
                        .and_then(normalize_verification_repair_focus_target)
                })
            })
    })
    .or_else(|| {
        metadata
            .get("active_targets")
            .and_then(Value::as_array)
            .and_then(|targets| {
                targets.iter().find_map(|target| {
                    target
                        .as_str()
                        .and_then(normalize_verification_repair_focus_target)
                })
            })
    })
}

fn normalize_verification_repair_focus_target(target: &str) -> Option<String> {
    let normalized = target.trim().replace('\\', "/");
    (!normalized.is_empty()).then_some(normalized)
}

fn verification_repair_focus_required_cleared(
    success: Option<bool>,
    progress_effect: &crate::protocol::ToolProgressEffect,
    verification_run: Option<&crate::protocol::VerificationRunResult>,
) -> bool {
    if matches!(
        progress_effect,
        crate::protocol::ToolProgressEffect::MadeProgress
            | crate::protocol::ToolProgressEffect::VerificationPassed
            | crate::protocol::ToolProgressEffect::VerificationFailed
    ) || success == Some(true)
    {
        return true;
    }
    verification_run.is_some_and(|run| {
        matches!(
            run.status,
            crate::protocol::VerificationRunStatus::Passed
                | crate::protocol::VerificationRunStatus::Failed
                | crate::protocol::VerificationRunStatus::TimedOut
        )
    })
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

fn latest_staged_task_documentation_audit_state_from_history(
    history_items: &[HistoryItem],
    start_index: usize,
    required_targets: &[String],
) -> Option<StagedTaskDocumentationAuditPromptState> {
    let latest_user = latest_user_history_index(history_items, start_index)?;
    let mut write_targets_by_call = HashMap::new();
    let mut current: Option<StagedTaskDocumentationAuditPromptState> = None;

    for item in &history_items[latest_user + 1..] {
        match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                model_arguments,
                effective_arguments,
                ..
            } if *tool == ToolName::Write => {
                let arguments_json =
                    replay_tool_arguments_json(arguments, model_arguments, effective_arguments);
                if let Some(path) = write_path_from_arguments_json(&arguments_json) {
                    write_targets_by_call.insert(call_id.to_string(), path);
                }
            }
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                output_text,
                metadata,
                success,
                progress_effect,
                ..
            } if *status == ToolLifecycleStatus::Completed => {
                if staged_task_documentation_audit_metadata(metadata) {
                    let target = staged_task_documentation_audit_target_from_metadata(metadata)
                        .or_else(|| write_targets_by_call.get(&call_id.to_string()).cloned())
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
                    let feedback = staged_task_documentation_audit_feedback_from_metadata(metadata)
                        .unwrap_or_else(|| {
                            staged_task_documentation_audit_feedback_excerpt(output_text)
                        });
                    let actionable_feedback =
                        staged_task_documentation_audit_metadata_has_actionable_feedback(metadata);
                    current = Some(StagedTaskDocumentationAuditPromptState {
                        target,
                        feedback,
                        actionable_feedback,
                        failure_count,
                    });
                    continue;
                }
                let Some(state) = current.as_mut() else {
                    continue;
                };
                if *success != Some(true)
                    && *progress_effect != crate::protocol::ToolProgressEffect::MadeProgress
                {
                    continue;
                }
                if let Some(target) = write_targets_by_call.get(&call_id.to_string()) {
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
                    {
                        current = None;
                    }
                }
            }
            _ => {}
        }
    }

    current
}

fn latest_user_history_index(history_items: &[HistoryItem], start_index: usize) -> Option<usize> {
    history_items
        .iter()
        .enumerate()
        .skip(start_index)
        .rev()
        .find_map(|(index, item)| match &item.payload {
            HistoryItemPayload::UserTurn { .. }
            | HistoryItemPayload::Message {
                role: MessageRole::User,
                ..
            } => Some(index),
            _ => None,
        })
}

fn staged_task_documentation_audit_metadata(metadata: &Value) -> bool {
    matches!(
        tool_output_metadata_kind(metadata),
        Some(
            "docs_spec_semantic_reconciliation_failed"
                | "staged_task_documentation_audit_failed"
                | "staged_task_documentation_closeout_audit_failed"
        )
    )
}

fn staged_task_documentation_audit_target_from_metadata(metadata: &Value) -> Option<String> {
    metadata
        .get("targets")
        .and_then(Value::as_array)
        .and_then(|targets| targets.iter().find_map(Value::as_str))
        .or_else(|| metadata.get("target").and_then(Value::as_str))
        .or_else(|| {
            metadata
                .pointer("/tool_feedback_envelope/target")
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .map(str::to_string)
}

fn staged_task_documentation_audit_feedback_from_metadata(metadata: &Value) -> Option<String> {
    let mut notes = Vec::new();
    notes.extend(staged_task_documentation_claim_feedback(
        metadata
            .get("missing_required_claim_details")
            .and_then(Value::as_array),
        "Add",
    ));
    notes.extend(staged_task_documentation_claim_feedback(
        metadata
            .get("prohibited_claim_details")
            .and_then(Value::as_array),
        "Remove",
    ));
    if notes.is_empty() {
        return None;
    }
    Some(format!(
        "Apply these concrete fixes in the next rewrite: {}",
        notes.join("; ")
    ))
}

fn staged_task_documentation_claim_feedback(
    details: Option<&Vec<Value>>,
    verb: &str,
) -> Vec<String> {
    details
        .into_iter()
        .flatten()
        .filter_map(|detail| {
            let id = detail.get("id").and_then(Value::as_str).unwrap_or("");
            let description = detail
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            let refs = detail
                .get("evidence_refs")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let label = if !description.is_empty() {
                description
            } else {
                id
            };
            if label.is_empty() {
                None
            } else if refs.is_empty() {
                Some(format!("{verb} `{label}`"))
            } else {
                Some(format!("{verb} `{label}` from evidence `{refs}`"))
            }
        })
        .collect()
}

fn staged_task_documentation_audit_metadata_has_actionable_feedback(metadata: &Value) -> bool {
    !metadata
        .get("missing_required_claim_details")
        .and_then(Value::as_array)
        .map(Vec::is_empty)
        .unwrap_or(true)
        || !metadata
            .get("prohibited_claim_details")
            .and_then(Value::as_array)
            .map(Vec::is_empty)
            .unwrap_or(true)
}

fn is_staged_task_documentation_audit_result_title(title: &str) -> bool {
    matches!(
        title,
        "Staged task documentation audit failed"
            | "Staged task documentation close-out audit failed"
    )
}

fn recent_verification_failures_from_history(
    history_items: &[HistoryItem],
    start_index: usize,
) -> Option<Vec<String>> {
    for item in history_items.iter().skip(start_index).rev() {
        let HistoryItemPayload::ToolOutput {
            title,
            verification_run,
            ..
        } = &item.payload
        else {
            continue;
        };
        let Some(verification_run) = verification_run else {
            continue;
        };
        match verification_run.status {
            crate::protocol::VerificationRunStatus::Passed => return Some(Vec::new()),
            crate::protocol::VerificationRunStatus::Failed
            | crate::protocol::VerificationRunStatus::TimedOut => {
                if let Some(cluster) = verification_run.failure_cluster.as_ref() {
                    let labels = dedupe_targets(
                        cluster
                            .failing_labels
                            .iter()
                            .filter(|label| !label.trim().is_empty())
                            .take(MAX_VERIFICATION_FAILURE_LABELS)
                            .cloned()
                            .collect(),
                    );
                    if !labels.is_empty() {
                        return Some(labels);
                    }
                }
                let labels = extract_failure_labels(&verification_run.output_summary);
                if !labels.is_empty() {
                    return Some(labels);
                }
                return Some(vec![fallback_verification_failure_label(
                    Some(&verification_run.command),
                    title,
                )]);
            }
            crate::protocol::VerificationRunStatus::NotVerification => continue,
        }
    }
    None
}

fn verification_repair_rotated_focus_target(
    history_items: &[HistoryItem],
    history_start_index: usize,
    state: Option<&SessionStateSnapshot>,
) -> Option<String> {
    let failure = state.and_then(|state| state.failure.as_ref())?;
    let preceding_repair_targets =
        latest_failed_verification_preceding_repair_targets_from_history_items(
            history_items,
            history_start_index,
        );
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
    dedupe_targets(language_failure_labels_from_summary(
        summary,
        MAX_VERIFICATION_FAILURE_LABELS,
    ))
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

fn latest_wrong_authoring_target_rejection_for_prompt(
    history_items: &[HistoryItem],
    history_start_index: usize,
) -> Option<String> {
    latest_wrong_authoring_target_rejection_from_history(history_items, history_start_index)
}

fn latest_wrong_authoring_target_rejection_from_history(
    history_items: &[HistoryItem],
    start_index: usize,
) -> Option<String> {
    history_items
        .iter()
        .skip(start_index)
        .rev()
        .find_map(|item| match &item.payload {
            HistoryItemPayload::ToolOutput {
                status,
                output_text,
                metadata,
                ..
            } if *status == ToolLifecycleStatus::Completed
                && tool_output_metadata_kind(metadata) == Some("wrong_authoring_target") =>
            {
                Some(output_text.clone())
            }
            _ => None,
        })
}

fn tool_output_metadata_kind(metadata: &Value) -> Option<&str> {
    metadata
        .pointer("/tool_feedback_envelope/kind")
        .and_then(Value::as_str)
        .or_else(|| {
            metadata
                .pointer("/tool_feedback_envelope/operation_progress_class")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            metadata
                .get("operation_progress_class")
                .and_then(Value::as_str)
        })
}

fn prompt_history_window_start_index(history_items: &[HistoryItem]) -> usize {
    latest_compaction_history_index(history_items)
        .map(|index| index + 1)
        .unwrap_or(0)
}

fn inactive_target_recovery_required_read_target(
    history_items: &[HistoryItem],
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
    let start_index = prompt_history_window_start_index(history_items);
    let latest_user = latest_user_turn_index_after(history_items, start_index)?;
    let latest_rejection_index = history_items[latest_user + 1..]
        .iter()
        .enumerate()
        .filter_map(|(offset, item)| {
            matches!(
                &item.payload,
                HistoryItemPayload::ToolOutput {
                    status,
                    metadata,
                    ..
                } if *status == ToolLifecycleStatus::Completed
                    && tool_output_metadata_kind(metadata) == Some("wrong_authoring_target")
            )
            .then_some(latest_user + 1 + offset)
        })
        .last()?;
    let read_after_rejection = history_items[latest_rejection_index + 1..]
        .iter()
        .any(|item| match &item.payload {
            HistoryItemPayload::ToolCall {
                tool,
                arguments,
                model_arguments,
                effective_arguments,
                ..
            } if *tool == ToolName::Read => {
                let arguments =
                    canonical_tool_call_arguments(arguments, model_arguments, effective_arguments);
                extract_readonly_target(&tool.to_string(), &arguments.to_string()).is_some_and(
                    |read_target| {
                        prompt_target_matches_required_output(&read_target, &[target.clone()])
                    },
                )
            }
            _ => false,
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
    let stale_payload = "pub const WORKFLOW_STATE: &str = \"archived\";\npub const WORKFLOW_NOTE: &str = \"inactive source\";\n";
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "stale inactive authoring replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create src/inactive-workflow.rs and tests/workflow.behavior.md"
                        .to_string(),
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
                    "path": "src/inactive-workflow.rs",
                    "content": stale_payload,
                }),
                model_arguments: Value::Null,
                effective_arguments: json!({
                    "path": "src/inactive-workflow.rs",
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
                title: "Wrote Added src/inactive-workflow.rs".to_string(),
                output_text: format!("Added src/inactive-workflow.rs\n{stale_payload}"),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: Some("fixture-inactive-workflow-write".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["tests/workflow.behavior.md".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let mut saw_omission_note = false;
    let mut saw_reference_snapshot = false;
    let mut saw_stale_tool_call = false;
    let mut saw_stale_tool_output = false;
    for message in &projection.messages {
        match message {
            ModelMessage::System { content } => {
                if content.contains("inactive target")
                    && content.contains("non-executable historical context")
                    && content.contains("tests/workflow.behavior.md")
                    && !content.contains("[omitted inactive authoring target]")
                    && !content.contains("[omitted stale inactive authoring payload")
                {
                    saw_omission_note = true;
                }
                if content.contains("Reference-only accepted artifact snapshot")
                    && content.contains("artifact_path: `src/inactive-workflow.rs`")
                    && content.contains("WORKFLOW_STATE")
                    && content.contains("Do not rewrite this inactive target")
                {
                    saw_reference_snapshot = true;
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
        && saw_reference_snapshot
        && !saw_stale_tool_call
        && !saw_stale_tool_output
        && !serialized.contains("[omitted inactive authoring target]")
        && !serialized.contains("[omitted stale inactive authoring payload")
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "stale_inactive_authoring_payload_omitted"
                && policy.call_id.as_deref() == Some(&call_id.to_string())
                && policy.omitted_targets == vec!["src/inactive-workflow.rs".to_string()]
                && policy.active_targets == vec!["tests/workflow.behavior.md".to_string()]
        })
}

pub fn stale_inactive_authoring_replay_omits_fake_executable_arguments() -> bool {
    stale_inactive_authoring_replay_uses_live_builder()
        && stale_inactive_apply_patch_filechange_replay_uses_reference_snapshot()
        && metadata_only_tool_output_does_not_create_filechange_reference_snapshot()
        && stale_inactive_filechange_without_replayable_tool_call_uses_reference_snapshot()
        && provider_replay_omits_stale_inactive_authoring_prelude_text()
}

pub(crate) fn provider_replay_omits_stale_inactive_authoring_prelude_text() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let stale_prelude = "`src/inactive-workflow.rs` を作成します。";
    let stale_payload = "pub const WORKFLOW_STATE: &str = \"archived\";\npub const WORKFLOW_NOTE: &str = \"inactive source\";\n";
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "stale inactive prelude replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create src/inactive-workflow.rs and tests/workflow.behavior.md"
                        .to_string(),
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
            payload: HistoryItemPayload::Message {
                message_id: None,
                role: MessageRole::Assistant,
                content: vec![ContentPart::Text {
                    text: stale_prelude.to_string(),
                }],
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 3,
            created_at_ms: 3,
            payload: HistoryItemPayload::ToolCall {
                call_id,
                tool: ToolName::Write,
                arguments: json!({
                    "path": "src/inactive-workflow.rs",
                    "content": stale_payload,
                }),
                model_arguments: Value::Null,
                effective_arguments: json!({
                    "path": "src/inactive-workflow.rs",
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
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::ToolOutput {
                call_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Wrote Added src/inactive-workflow.rs".to_string(),
                output_text: format!("Added src/inactive-workflow.rs\n{stale_payload}"),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: Some("fixture-inactive-workflow-write".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["tests/workflow.behavior.md".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let serialized = serde_json::to_string(&projection.messages).unwrap_or_default();
    !serialized.contains(stale_prelude)
        && serialized.contains("Reference-only accepted artifact snapshot")
        && serialized.contains("artifact_path: `src/inactive-workflow.rs`")
        && serialized.contains("tests/workflow.behavior.md")
        && !serialized.contains(stale_payload)
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "stale_inactive_authoring_payload_omitted"
                && policy.call_id.as_deref() == Some(&call_id.to_string())
                && policy.omitted_targets == vec!["src/inactive-workflow.rs".to_string()]
                && policy.active_targets == vec!["tests/workflow.behavior.md".to_string()]
        })
}

pub(crate) fn stale_inactive_apply_patch_filechange_replay_uses_reference_snapshot() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let change_id = crate::session::ChangeId::new();
    let patch_text = "*** Begin Patch\n*** Add File: src/inactive-workflow.rs\n+pub const WORKFLOW_STATE: &str = \"accepted\";\n+pub const WORKFLOW_NOTE: &str = \"inactive source\";\n*** End Patch";
    let diff_text = "--- /dev/null\n+++ C:/workspace/src/inactive-workflow.rs\n@@ -0,0 +1,2 @@\n+pub const WORKFLOW_STATE: &str = \"accepted\";\n+pub const WORKFLOW_NOTE: &str = \"inactive source\";\n";
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "stale inactive apply_patch replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create src/inactive-workflow.rs and tests/workflow.behavior.md"
                        .to_string(),
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
                tool: ToolName::ApplyPatch,
                arguments: json!({ "patch_text": patch_text }),
                model_arguments: Value::Null,
                effective_arguments: json!({ "patch_text": patch_text }),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::ApplyPatch],
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
                title: "Applied 1 change(s)".to_string(),
                output_text: "Added src/inactive-workflow.rs".to_string(),
                metadata: json!({
                    "changes": [{
                        "kind": "add",
                        "path_after": "C:/workspace/src/inactive-workflow.rs",
                        "summary": "Added src/inactive-workflow.rs"
                    }],
                    "diff_text": diff_text,
                    "operation_progress_class": "content_changing_progress",
                    "progress_effect": "made_progress",
                    "tool_feedback_envelope": {
                        "operation_progress_class": "content_changing_progress",
                        "progress_effect": "made_progress",
                        "side_effects_applied": true
                    }
                }),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: Some("fixture-inactive-workflow-apply-patch".to_string()),
                verification_run: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 4,
            created_at_ms: 4,
            payload: HistoryItemPayload::FileChange {
                call_id,
                change_ids: vec![change_id],
                changes: vec![crate::protocol::FileChangeEvidence {
                    change_id,
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("C:/workspace/src/inactive-workflow.rs")),
                    summary: "Added src/inactive-workflow.rs".to_string(),
                }],
                summary: "Added src/inactive-workflow.rs".to_string(),
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["tests/workflow.behavior.md".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let call_id_text = call_id.to_string();
    let mut saw_omission_note = false;
    let mut saw_reference_snapshot = false;
    let mut saw_stale_tool_call = false;
    let mut saw_stale_tool_output = false;
    for message in &projection.messages {
        match message {
            ModelMessage::System { content } => {
                if content.contains("inactive target")
                    && content.contains("non-executable historical context")
                    && content.contains("tests/workflow.behavior.md")
                {
                    saw_omission_note = true;
                }
                if content.contains("Reference-only accepted artifact snapshot")
                    && content.contains("artifact_path: `src/inactive-workflow.rs`")
                    && content.contains("summary: Added src/inactive-workflow.rs")
                    && content.contains("Do not rewrite this inactive target")
                {
                    saw_reference_snapshot = true;
                }
            }
            ModelMessage::AssistantToolCalls { tool_calls, .. } => {
                if tool_calls
                    .iter()
                    .any(|tool_call| tool_call.call_id == call_id_text)
                {
                    saw_stale_tool_call = true;
                }
            }
            ModelMessage::Tool {
                call_id: replayed_call_id,
                ..
            } => {
                if replayed_call_id == &call_id_text {
                    saw_stale_tool_output = true;
                }
            }
            _ => {}
        }
    }
    let serialized = serde_json::to_string(&projection.messages).unwrap_or_default();
    saw_omission_note
        && saw_reference_snapshot
        && !saw_stale_tool_call
        && !saw_stale_tool_output
        && !serialized.contains("WORKFLOW_NOTE")
        && !serialized.contains(patch_text)
        && !serialized.contains("C:/workspace/src/inactive-workflow.rs")
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "stale_inactive_authoring_payload_omitted"
                && policy.call_id.as_deref() == Some(call_id_text.as_str())
                && policy.omitted_targets == vec!["src/inactive-workflow.rs".to_string()]
                && policy.active_targets == vec!["tests/workflow.behavior.md".to_string()]
        })
}

pub(crate) fn metadata_only_tool_output_does_not_create_filechange_reference_snapshot() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let patch_text = "*** Begin Patch\n*** Add File: src/inactive-workflow.rs\n+pub const WORKFLOW_STATE: &str = \"metadata_only\";\n+pub const WORKFLOW_NOTE: &str = \"inactive source\";\n*** End Patch";
    let diff_text = "--- /dev/null\n+++ C:/workspace/src/inactive-workflow.rs\n@@ -0,0 +1,2 @@\n+pub const WORKFLOW_STATE: &str = \"metadata_only\";\n+pub const WORKFLOW_NOTE: &str = \"inactive source\";\n";
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "metadata-only filechange replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create src/inactive-workflow.rs and tests/workflow.behavior.md"
                        .to_string(),
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
                tool: ToolName::ApplyPatch,
                arguments: json!({ "patch_text": patch_text }),
                model_arguments: Value::Null,
                effective_arguments: json!({ "patch_text": patch_text }),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::ApplyPatch],
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
                title: "Applied 1 change(s)".to_string(),
                output_text: "Added src/inactive-workflow.rs".to_string(),
                metadata: json!({
                    "changes": [{
                        "kind": "add",
                        "path_after": "C:/workspace/src/inactive-workflow.rs",
                        "summary": "Added src/inactive-workflow.rs"
                    }],
                    "diff_text": diff_text,
                    "operation_progress_class": "content_changing_progress",
                    "progress_effect": "made_progress",
                    "tool_feedback_envelope": {
                        "operation_progress_class": "content_changing_progress",
                        "progress_effect": "made_progress",
                        "side_effects_applied": true
                    }
                }),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: Some(
                    "fixture-metadata-only-inactive-workflow-apply-patch".to_string(),
                ),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["tests/workflow.behavior.md".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let serialized = serde_json::to_string(&projection.messages).unwrap_or_default();
    serialized.contains("Previous authoring tool call/output pair for inactive target")
        && !serialized.contains("Reference-only accepted artifact snapshot")
        && !serialized.contains("metadata_only")
        && !serialized.contains("artifact_path: `src/inactive-workflow.rs`")
        && !serialized.contains(patch_text)
}

pub(crate) fn stale_inactive_filechange_without_replayable_tool_call_uses_reference_snapshot()
-> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let change_id = crate::session::ChangeId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "stale inactive filechange-only replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create src/inactive-workflow.rs and tests/workflow.behavior.md"
                        .to_string(),
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
            payload: HistoryItemPayload::FileChange {
                call_id: crate::session::ToolCallId::new(),
                change_ids: vec![change_id],
                changes: vec![crate::protocol::FileChangeEvidence {
                    change_id,
                    kind: crate::session::ChangeKind::Add,
                    path_before: None,
                    path_after: Some(Utf8PathBuf::from("src/inactive-workflow.rs")),
                    summary: "Added src/inactive-workflow.rs".to_string(),
                }],
                summary: "Added src/inactive-workflow.rs".to_string(),
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["tests/workflow.behavior.md".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let serialized = serde_json::to_string(&projection.messages).unwrap_or_default();
    serialized.contains("Reference-only accepted artifact snapshot")
        && serialized.contains("artifact_path: `src/inactive-workflow.rs`")
        && serialized.contains("already exists")
        && serialized.contains("Do not rewrite this inactive target")
        && serialized.contains("tests/workflow.behavior.md")
        && !serialized.contains("inactive-workflow.rs and tests/workflow.behavior.md are both open")
}

pub(crate) fn failed_inactive_authoring_replay_uses_call_scoped_summary() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let stale_payload = "pub const WORKFLOW_STATE: &str = \"wrong_target\";\npub const WORKFLOW_NOTE: &str = \"inactive source\";\n";
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "failed inactive authoring replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text:
                        "Create src/inactive-workflow.rs, docs/workflow-notes.md, and tests/workflow.behavior.md"
                            .to_string(),
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
                    "path": "src/inactive-workflow.rs",
                    "content": stale_payload,
                }),
                model_arguments: Value::Null,
                effective_arguments: json!({
                    "path": "src/inactive-workflow.rs",
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
                output_text: "The submitted content-changing `write` call targets `src/inactive-workflow.rs`, but the current active requested deliverables are `docs/workflow-notes.md`, `tests/workflow.behavior.md`.".to_string(),
                metadata: json!({
                    "operation_progress_class": "wrong_authoring_target",
                    "progress_effect": "no_progress",
                    "submitted_targets": ["src/inactive-workflow.rs"],
                    "active_authoring_targets": ["docs/workflow-notes.md", "tests/workflow.behavior.md"],
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-wrong-target".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec![
            "docs/workflow-notes.md".to_string(),
            "tests/workflow.behavior.md".to_string(),
        ],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let call_id_text = call_id.to_string();
    let mut saw_executable_failed_tool_call = false;
    let mut saw_executable_failed_tool_output = false;
    let mut saw_non_executable_feedback = false;
    let mut saw_raw_stale_arguments = false;
    for message in &projection.messages {
        match message {
            ModelMessage::AssistantToolCalls { tool_calls, .. } => {
                if tool_calls.iter().any(|tool_call| {
                    tool_call.call_id == call_id_text
                        && tool_call.tool_name == "write"
                        && tool_call
                            .arguments_json
                            .contains("omitted wrong-target write payload")
                        && !tool_call.arguments_json.contains("\"path\"")
                        && !tool_call.arguments_json.contains("WORKFLOW_STATE")
                }) {
                    saw_executable_failed_tool_call = true;
                }
                if tool_calls
                    .iter()
                    .any(|tool_call| tool_call.arguments_json.contains("WORKFLOW_STATE"))
                {
                    saw_raw_stale_arguments = true;
                }
            }
            ModelMessage::Tool {
                call_id: replayed_call_id,
                result,
                ..
            } => {
                if replayed_call_id == &call_id_text
                    && result.contains("src/inactive-workflow.rs")
                    && result.contains("docs/workflow-notes.md")
                    && result.contains("tests/workflow.behavior.md")
                {
                    saw_executable_failed_tool_output = true;
                }
            }
            ModelMessage::System { content } => {
                if content.contains("failed wrong-target authoring tool call/output")
                    && content.contains(&call_id_text)
                    && content.contains("src/inactive-workflow.rs")
                    && content.contains("docs/workflow-notes.md")
                    && content.contains("tests/workflow.behavior.md")
                    && content.contains("non-executable historical feedback")
                {
                    saw_non_executable_feedback = true;
                }
            }
            _ => {}
        }
    }
    let serialized = serde_json::to_string(&projection.messages).unwrap_or_default();
    !saw_executable_failed_tool_call
        && !saw_executable_failed_tool_output
        && saw_non_executable_feedback
        && !saw_raw_stale_arguments
        && !serialized.contains(stale_payload)
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "failed_inactive_authoring_executable_pair_omitted"
                && policy.call_id.as_deref() == Some(call_id_text.as_str())
                && policy.omitted_targets == vec!["src/inactive-workflow.rs".to_string()]
                && policy.active_targets
                    == vec![
                        "docs/workflow-notes.md".to_string(),
                        "tests/workflow.behavior.md".to_string(),
                    ]
        })
}

pub(crate) fn failed_inactive_apply_patch_replay_uses_call_scoped_summary() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let stale_payload = "*** Begin Patch\n*** Add File: src/inactive-workflow.rs\n+pub const WORKFLOW_STATE: &str = \"wrong_target\";\n+pub const WORKFLOW_NOTE: &str = \"inactive source\";\n*** End Patch";
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "failed inactive apply_patch replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create src/inactive-workflow.rs and tests/workflow.behavior.md"
                        .to_string(),
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
                tool: ToolName::ApplyPatch,
                arguments: json!({ "patch_text": stale_payload }),
                model_arguments: Value::Null,
                effective_arguments: json!({ "patch_text": stale_payload }),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::ApplyPatch],
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
                output_text: "The submitted content-changing `apply_patch` call targets `src/inactive-workflow.rs`, but the current active requested deliverables are `tests/workflow.behavior.md`.".to_string(),
                metadata: json!({
                    "operation_progress_class": "wrong_authoring_target",
                    "progress_effect": "no_progress",
                    "submitted_targets": ["src/inactive-workflow.rs"],
                    "active_authoring_targets": ["tests/workflow.behavior.md"],
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-wrong-target-patch".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["tests/workflow.behavior.md".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let call_id_text = call_id.to_string();
    let mut saw_executable_failed_tool_call = false;
    let mut saw_executable_failed_tool_output = false;
    let mut saw_non_executable_feedback = false;
    let mut saw_raw_stale_arguments = false;
    for message in &projection.messages {
        match message {
            ModelMessage::AssistantToolCalls { tool_calls, .. } => {
                if tool_calls.iter().any(|tool_call| {
                    tool_call.call_id == call_id_text
                        && tool_call.tool_name == "apply_patch"
                        && tool_call
                            .arguments_json
                            .contains("omitted wrong-target patch payload")
                        && !tool_call.arguments_json.contains("WORKFLOW_STATE")
                }) {
                    saw_executable_failed_tool_call = true;
                }
                if tool_calls
                    .iter()
                    .any(|tool_call| tool_call.arguments_json.contains("WORKFLOW_STATE"))
                {
                    saw_raw_stale_arguments = true;
                }
            }
            ModelMessage::Tool {
                call_id: replayed_call_id,
                result,
                ..
            } => {
                if replayed_call_id == &call_id_text
                    && result.contains("src/inactive-workflow.rs")
                    && result.contains("tests/workflow.behavior.md")
                {
                    saw_executable_failed_tool_output = true;
                }
            }
            ModelMessage::System { content } => {
                if content.contains("failed wrong-target authoring tool call/output")
                    && content.contains(&call_id_text)
                    && content.contains("src/inactive-workflow.rs")
                    && content.contains("tests/workflow.behavior.md")
                    && content.contains("non-executable historical feedback")
                {
                    saw_non_executable_feedback = true;
                }
            }
            _ => {}
        }
    }
    let serialized = serde_json::to_string(&projection.messages).unwrap_or_default();
    !saw_executable_failed_tool_call
        && !saw_executable_failed_tool_output
        && saw_non_executable_feedback
        && !saw_raw_stale_arguments
        && !serialized.contains(stale_payload)
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "failed_inactive_authoring_executable_pair_omitted"
                && policy.call_id.as_deref() == Some(call_id_text.as_str())
                && policy.omitted_targets == vec!["src/inactive-workflow.rs".to_string()]
                && policy.active_targets == vec!["tests/workflow.behavior.md".to_string()]
        })
}

pub fn provider_replay_preserves_failed_inactive_authoring_feedback() -> bool {
    failed_inactive_authoring_replay_uses_call_scoped_summary()
        && failed_inactive_apply_patch_replay_uses_call_scoped_summary()
}

pub(crate) fn mixed_target_invalid_edit_replay_is_target_exclusive_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let stale_payload = "*** Begin Patch\n*** Add File: src/inactive-workflow.rs\n+pub const WORKFLOW_STATE: &str = \"inactive\";\n*** End Patch\n*** Add File: tests/workflow.behavior.md\n+workflow behavior assertion\n*** End Patch";
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "mixed target invalid edit replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create the requested active test artifact.".to_string(),
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
                tool: ToolName::ApplyPatch,
                arguments: json!({ "patch_text": stale_payload }),
                model_arguments: Value::Null,
                effective_arguments: json!({ "patch_text": stale_payload }),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::ApplyPatch],
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
                title: "Invalid tool arguments".to_string(),
                output_text:
                    "Invalid mixed-target edit arguments were rejected before side effects."
                        .to_string(),
                metadata: json!({
                    "operation_progress_class": "invalid_edit_arguments",
                    "progress_effect": "no_progress",
                    "submitted_targets": ["src/inactive-workflow.rs", "tests/workflow.behavior.md"],
                    "active_submitted_targets": ["tests/workflow.behavior.md"],
                    "inactive_submitted_targets": ["src/inactive-workflow.rs"],
                    "tool_feedback_envelope": {
                        "kind": "invalid_edit_arguments",
                        "operation_progress_class": "invalid_edit_arguments",
                        "progress_effect": "no_progress",
                        "submitted_targets": ["src/inactive-workflow.rs", "tests/workflow.behavior.md"],
                        "active_submitted_targets": ["tests/workflow.behavior.md"],
                        "inactive_submitted_targets": ["src/inactive-workflow.rs"],
                        "side_effects_applied": false
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-mixed-target-invalid-edit".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["tests/workflow.behavior.md".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let call_id_text = call_id.to_string();
    let serialized_messages = serde_json::to_string(&projection.messages).unwrap_or_default();
    let executable_pair_absent = !projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|tool_call| tool_call.call_id == call_id_text)
        ) || matches!(
            message,
            ModelMessage::Tool { call_id: replayed, .. } if replayed == &call_id_text
        )
    });
    let target_exclusive_note = projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::System { content }
                if content.contains("Prior mixed-target invalid edit tool call/output")
                    && content.contains(&call_id_text)
                    && content.contains("tests/workflow.behavior.md")
                    && content.contains("target-exclusive requested-work")
                    && content.contains("single-operation active-target patch skeleton")
                    && content.contains("*** Add File: tests/workflow.behavior.md")
                    && content.contains("+<complete content for tests/workflow.behavior.md>")
                    && content.contains("exactly one file operation")
                    && !content.contains("src/inactive-workflow.rs")
                    && !content.contains("WORKFLOW_STATE")
        )
    });
    executable_pair_absent
        && target_exclusive_note
        && !serialized_messages.contains(stale_payload)
        && !serialized_messages.contains("WORKFLOW_STATE")
        && !serialized_messages.contains("src/inactive-workflow.rs")
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "mixed_target_invalid_edit_executable_pair_omitted"
                && policy.call_id.as_deref() == Some(&call_id_text)
                && policy.tool_name.as_deref() == Some("apply_patch")
                && policy.omitted_targets == vec!["src/inactive-workflow.rs".to_string()]
                && policy.active_targets == vec!["tests/workflow.behavior.md".to_string()]
        })
}

pub(crate) fn inactive_target_content_shape_replay_is_target_exclusive_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let stale_payload = "*** Begin Patch\n*** Add File: src/inactive-workflow.rs\n+def workflow_compute(value):\n+    return value\n+\n+def main():\n+    print(workflow_compute(1))\n*** End Patch";
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "inactive target content-shape replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create the requested test artifact.".to_string(),
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
                tool: ToolName::ApplyPatch,
                arguments: json!({ "patch_text": stale_payload }),
                model_arguments: Value::Null,
                effective_arguments: json!({ "patch_text": stale_payload }),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::ApplyPatch],
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
                output_text:
                    "The submitted content does not match the active test artifact contract."
                        .to_string(),
                metadata: json!({
                    "operation_progress_class": "required_write_content_shape_mismatch",
                    "progress_effect": "no_progress",
                    "submitted_targets": ["src/inactive-workflow.rs"],
                    "active_targets": ["tests/workflow.behavior.md"],
                    "required_target": "tests/workflow.behavior.md",
                    "target": "tests/workflow.behavior.md",
                    "tool_feedback_envelope": {
                        "kind": "required_write_content_shape_mismatch",
                        "operation_progress_class": "required_write_content_shape_mismatch",
                        "progress_effect": "no_progress",
                        "submitted_targets": ["src/inactive-workflow.rs"],
                        "active_targets": ["tests/workflow.behavior.md"],
                        "required_target": "tests/workflow.behavior.md",
                        "target": "tests/workflow.behavior.md",
                        "side_effects_applied": false
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-inactive-content-shape".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["tests/workflow.behavior.md".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let call_id_text = call_id.to_string();
    let serialized_messages = serde_json::to_string(&projection.messages).unwrap_or_default();
    let executable_pair_absent = !projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|tool_call| tool_call.call_id == call_id_text)
        ) || matches!(
            message,
            ModelMessage::Tool { call_id: replayed, .. } if replayed == &call_id_text
        )
    });
    let target_exclusive_note = projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::System { content }
                if content.contains("Prior content-shape rejected tool call/output")
                    && content.contains(&call_id_text)
                    && content.contains("tests/workflow.behavior.md")
                    && content.contains("target-exclusive requested-work")
                    && content.contains("single-operation active-target patch skeleton")
                    && content.contains("*** Add File: tests/workflow.behavior.md")
                    && content.contains("+<complete content for tests/workflow.behavior.md>")
                    && content.contains("exactly one file operation")
                    && !content.contains("src/inactive-workflow.rs")
                    && !content.contains("workflow_compute")
                    && !content.contains("def main")
        )
    });
    executable_pair_absent
        && target_exclusive_note
        && !serialized_messages.contains(stale_payload)
        && !serialized_messages.contains("src/inactive-workflow.rs")
        && !serialized_messages.contains("workflow_compute")
        && !serialized_messages.contains("def main")
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "inactive_target_content_shape_executable_pair_omitted"
                && policy.call_id.as_deref() == Some(&call_id_text)
                && policy.tool_name.as_deref() == Some("apply_patch")
                && policy.omitted_targets == vec!["src/inactive-workflow.rs".to_string()]
                && policy.active_targets == vec!["tests/workflow.behavior.md".to_string()]
        })
}

pub(crate) fn failed_inactive_authoring_feedback_requires_typed_metadata() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "untyped wrong-target title replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Create tests/workflow.behavior.md".to_string(),
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
                tool: ToolName::ApplyPatch,
                arguments: json!({ "patch_text": "*** Update File: src/inactive-workflow.rs\n@@\n+stale" }),
                model_arguments: Value::Null,
                effective_arguments: json!({ "patch_text": "*** Update File: src/inactive-workflow.rs\n@@\n+stale" }),
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::ApplyPatch],
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
                output_text: "The submitted content-changing call targets src/inactive-workflow.rs, but the current active requested deliverable is tests/workflow.behavior.md.".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("untyped-wrong-target-title".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["tests/workflow.behavior.md".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let call_id = call_id.to_string();
    let failed_policy_absent = !projection.replay_policies.iter().any(|policy| {
        policy.policy == "failed_inactive_authoring_executable_pair_omitted"
            && policy.call_id.as_deref() == Some(&call_id)
    });
    let stale_policy_present = projection.replay_policies.iter().any(|policy| {
        policy.policy == "stale_inactive_authoring_payload_omitted"
            && policy.call_id.as_deref() == Some(&call_id)
    });
    let no_failed_feedback_note = !projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::System { content }
                if content.contains("failed wrong-target authoring tool call/output")
        )
    });
    failed_policy_absent && stale_policy_present && no_failed_feedback_note
}

pub(crate) fn invalid_edit_arguments_replay_requires_typed_metadata() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "untyped invalid edit title replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Update src/workflow.rs".to_string(),
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
                arguments: json!({ "path": "src/workflow.rs", "content": "original replay payload" }),
                model_arguments: Value::Null,
                effective_arguments: json!({ "path": "src/workflow.rs", "content": "original replay payload" }),
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
                title: "Invalid tool arguments".to_string(),
                output_text: "The provider submitted arguments that were not accepted.".to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("untyped-invalid-edit-title".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["src/workflow.rs".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let call_id = call_id.to_string();
    let malformed_policy_absent = !projection.replay_policies.iter().any(|policy| {
        policy.policy == "malformed_edit_arguments_payload_sanitized_output_preserved"
            && policy.call_id.as_deref() == Some(&call_id)
    });
    let serialized = serde_json::to_string(&projection.messages).unwrap_or_default();
    let original_payload_preserved = serialized.contains("original replay payload");
    let sanitized_payload_absent = !serialized.contains("omitted malformed edit payload");
    let tool_output_preserved = projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::Tool { call_id: tool_call_id, result, .. }
                if tool_call_id == &call_id
                    && result.contains("provider submitted arguments")
        )
    });
    malformed_policy_absent
        && original_payload_preserved
        && sanitized_payload_absent
        && tool_output_preserved
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
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let stale_plan_text = "src/inactive-workflow.rs draft";
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
                            "targets": ["src/inactive-workflow.rs"]
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
                    "Plan updated [tool feedback] progress_projection no_progress src/inactive-workflow.rs"
                        .to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-plan".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec![
            "docs/workflow-notes.md".to_string(),
            "tests/workflow.behavior.md".to_string(),
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
                    && content.contains("docs/workflow-notes.md")
                    && content.contains("tests/workflow.behavior.md")
                    && !content.contains("src/inactive-workflow.rs")
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
                && policy.omitted_targets.is_empty()
                && policy.active_targets
                    == vec![
                        "docs/workflow-notes.md".to_string(),
                        "tests/workflow.behavior.md".to_string(),
                    ]
        })
}

pub fn provider_replay_omits_stale_progress_projection_arguments() -> bool {
    stale_progress_projection_replay_uses_live_builder()
        && current_progress_projection_feedback_replay_preserves_call_output()
        && current_progress_projection_feedback_requires_typed_metadata()
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
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create docs/workflow-notes.md and tests/workflow.behavior.md."
                        .to_string(),
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
                        "content": "src/inactive-workflow.rs draft",
                        "status": "in_progress",
                        "targets": ["src/inactive-workflow.rs"]
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
                output_text: "Plan updated [tool feedback] progress_projection no_progress src/inactive-workflow.rs"
                    .to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
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
                        "content": "tests/workflow.behavior.md authoring",
                        "status": "in_progress",
                        "targets": ["tests/workflow.behavior.md"]
                    }, {
                        "id": "step3",
                        "content": "docs/workflow-notes.md authoring",
                        "status": "pending",
                        "targets": ["docs/workflow-notes.md"]
                    }]
                }),
                model_arguments: json!({
                    "todos": [{
                        "id": "step2",
                        "content": "tests/workflow.behavior.md authoring",
                        "status": "in_progress",
                        "targets": ["tests/workflow.behavior.md"]
                    }, {
                        "id": "step3",
                        "content": "docs/workflow-notes.md authoring",
                        "status": "pending",
                        "targets": ["docs/workflow-notes.md"]
                    }]
                }),
                effective_arguments: json!({
                    "todos": [{
                        "id": "step2",
                        "content": "tests/workflow.behavior.md authoring",
                        "status": "in_progress",
                        "targets": ["tests/workflow.behavior.md"]
                    }, {
                        "id": "step3",
                        "content": "docs/workflow-notes.md authoring",
                        "status": "pending",
                        "targets": ["docs/workflow-notes.md"]
                    }]
                }),
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
                    "Plan updated\n\n[tool feedback]\noperation_progress_class: progress_projection\nprogress_effect: no_progress\nactive_targets: docs/workflow-notes.md, tests/workflow.behavior.md\nContinue with a file-changing tool output."
                        .to_string(),
                metadata: json!({
                    "tool_feedback_envelope": {
                        "operation_progress_class": "progress_projection",
                        "progress_effect": "no_progress",
                        "active_targets": ["docs/workflow-notes.md", "tests/workflow.behavior.md"]
                    }
                }),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("current-plan".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec![
            "docs/workflow-notes.md".to_string(),
            "tests/workflow.behavior.md".to_string(),
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
                    && result.contains("docs/workflow-notes.md")
                    && result.contains("tests/workflow.behavior.md")
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

pub(crate) fn current_progress_projection_feedback_requires_typed_metadata() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "untyped current progress projection feedback replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    text: "Create docs/workflow-notes.md and tests/workflow.behavior.md."
                        .to_string(),
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
                    "todos": [{
                        "id": "step2",
                        "content": "tests/workflow.behavior.md authoring",
                        "status": "in_progress",
                        "targets": ["tests/workflow.behavior.md"]
                    }, {
                        "id": "step3",
                        "content": "docs/workflow-notes.md authoring",
                        "status": "pending",
                        "targets": ["docs/workflow-notes.md"]
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
                call_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Plan updated".to_string(),
                output_text: "Plan updated [tool feedback] operation_progress_class: progress_projection progress_effect: no_progress active_targets: docs/workflow-notes.md, tests/workflow.behavior.md".to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("untyped-current-plan".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec![
            "docs/workflow-notes.md".to_string(),
            "tests/workflow.behavior.md".to_string(),
        ],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let call_id = call_id.to_string();
    let untyped_pair_preserved = projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|call| call.call_id == call_id)
        ) || matches!(message, ModelMessage::Tool { call_id: replayed_call_id, .. } if replayed_call_id == &call_id)
    });
    !untyped_pair_preserved
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "progress_projection_payload_omitted"
                && policy.call_id.as_deref() == Some(&call_id)
        })
}

pub(crate) fn prompt_provider_replay_residual_fixture_workflow_neutral_fixture_passes() -> bool {
    stale_inactive_authoring_replay_uses_live_builder()
        && provider_replay_omits_stale_inactive_authoring_prelude_text()
        && stale_inactive_apply_patch_filechange_replay_uses_reference_snapshot()
        && metadata_only_tool_output_does_not_create_filechange_reference_snapshot()
        && stale_inactive_filechange_without_replayable_tool_call_uses_reference_snapshot()
        && failed_inactive_authoring_replay_uses_call_scoped_summary()
        && failed_inactive_apply_patch_replay_uses_call_scoped_summary()
        && failed_inactive_authoring_feedback_requires_typed_metadata()
        && invalid_edit_arguments_replay_requires_typed_metadata()
        && stale_progress_projection_replay_uses_live_builder()
        && current_progress_projection_feedback_replay_preserves_call_output()
        && current_progress_projection_feedback_requires_typed_metadata()
}

pub(crate) fn content_shape_mismatch_replay_preserves_tool_lifecycle_without_payload() -> bool {
    let session_id = crate::session::SessionId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let call_id = crate::session::ToolCallId::new();
    let stale_payload = "export function workflowAdvance(state) {\n  return state;\n}\n";
    let transcript = Transcript {
        session: SessionRecord {
            id: session_id,
            project_id: crate::session::ProjectId::new(),
            title: "content-shape mismatch replay".to_string(),
            status: crate::session::SessionStatus::Running,
            cwd: Utf8PathBuf::from("C:/workspace"),
            model: PROMPT_FIXTURE_MODEL.to_string(),
            base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                        text: "create src/workflow.rs and tests/workflow.spec.ts".to_string(),
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
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
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
                                "path": "src/workflow.rs",
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
                            summary: "The submitted `write` call targeted `src/workflow.rs`, but current active work requires test content in `tests/workflow.spec.ts` that exercises `workflow.advance`.".to_string(),
                            success: Some(false),
                            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                            blocked_action: None,
                            result_hash: Some("fixture-required-write-content-shape-mismatch".to_string()),
                        }),
                    },
                ],
            },
        ],
    };
    let state = SessionStateSnapshot {
        process_phase: ProcessPhase::Author,
        active_targets: vec![Utf8PathBuf::from("tests/workflow.spec.ts")],
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
                    text: "create src/workflow.rs and tests/workflow.spec.ts".to_string(),
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
                    "path": "src/workflow.rs",
                    "content": stale_payload,
                }),
                model_arguments: Value::Null,
                effective_arguments: json!({
                    "path": "src/workflow.rs",
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
                output_text: "The submitted `write` call targeted `src/workflow.rs`, but current active work requires test content in `tests/workflow.spec.ts` that exercises `workflow.advance`.".to_string(),
                metadata: json!({
                    "operation_progress_class": "required_write_content_shape_mismatch",
                    "tool_feedback_envelope": {
                        "kind": "required_write_content_shape_mismatch",
                        "operation_progress_class": "required_write_content_shape_mismatch"
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-required-write-content-shape-mismatch".to_string()),
                verification_run: None,
            },
        },
    ];
    let messages = build_messages_with_state(
        PromptProjectionInput::from_session(&transcript.session),
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
                        && !call.arguments_json.contains("src/workflow.rs")
                    {
                        saw_sanitized_tool_call = true;
                    }
                }
            }
            ModelMessage::Tool {
                call_id: replayed_call_id,
                tool_name,
                result,
                ..
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

pub(crate) fn content_shape_mismatch_replay_requires_typed_metadata() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "untyped content-shape title replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
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
                    text: "Write src/workflow.rs".to_string(),
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
                arguments: json!({ "path": "src/workflow.rs", "content": "original content-shape replay payload" }),
                model_arguments: Value::Null,
                effective_arguments: json!({ "path": "src/workflow.rs", "content": "original content-shape replay payload" }),
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
                output_text: "The submitted write call did not match the required content shape."
                    .to_string(),
                metadata: Value::Null,
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("untyped-content-shape-title".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["src/workflow.rs".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &history_items, 32, &context);
    let serialized = serde_json::to_string(&projection.messages).unwrap_or_default();
    serialized.contains("original content-shape replay payload")
        && !serialized.contains("omitted incompatible write payload")
        && projection.messages.iter().any(|message| {
            matches!(
                message,
                ModelMessage::Tool { call_id: replayed_call_id, result, .. }
                    if replayed_call_id == &call_id.to_string()
                        && result.contains("required content shape")
            )
        })
}

pub(crate) fn exact_write_repair_omits_consumed_supporting_context_replay() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let source_read_id = crate::session::ToolCallId::new();
    let test_read_id = crate::session::ToolCallId::new();
    let write_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "consumed supporting context replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
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
                    text: "Write docs/workflow-design.md from src/workflow.rs and tests/workflow.spec.ts"
                        .to_string(),
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
                call_id: source_read_id,
                tool: ToolName::Read,
                arguments: json!({ "path": "src/workflow.rs" }),
                model_arguments: Value::Null,
                effective_arguments: json!({ "path": "src/workflow.rs" }),
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
                call_id: source_read_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Read src/workflow.rs".to_string(),
                output_text: "1: pub fn workflow_advance(value: i32) -> i32 {\n2:     value + 1\n3: }".to_string(),
                metadata: json!({
                    "operation_progress_class": "supporting_context",
                    "tool_feedback_envelope": {
                        "kind": "supporting_context",
                        "operation_progress_class": "supporting_context",
                        "progress_effect": "no_progress",
                        "side_effects_applied": false
                    }
                }),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: Some("workflow-source-read".to_string()),
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
                call_id: test_read_id,
                tool: ToolName::Read,
                arguments: json!({ "path": "tests/workflow.spec.ts" }),
                model_arguments: Value::Null,
                effective_arguments: json!({ "path": "tests/workflow.spec.ts" }),
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
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::ToolOutput {
                call_id: test_read_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Read tests/workflow.spec.ts".to_string(),
                output_text: "1: import { workflowAdvance } from '../src/workflow'\n2: expect(workflowAdvance(1)).toBe(2)".to_string(),
                metadata: json!({
                    "operation_progress_class": "supporting_context",
                    "tool_feedback_envelope": {
                        "kind": "supporting_context",
                        "operation_progress_class": "supporting_context",
                        "progress_effect": "no_progress",
                        "side_effects_applied": false
                    }
                }),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: Some("workflow-test-read".to_string()),
                verification_run: None,
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 6,
            created_at_ms: 6,
            payload: HistoryItemPayload::ToolCall {
                call_id: write_id,
                tool: ToolName::Write,
                arguments: json!({
                    "path": "docs/workflow-design.md",
                    "content": "\"# Workflow design\\n\\nSerialized markdown\""
                }),
                model_arguments: Value::Null,
                effective_arguments: json!({
                    "path": "docs/workflow-design.md",
                    "content": "\"# Workflow design\\n\\nSerialized markdown\""
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
            sequence_no: 7,
            created_at_ms: 7,
            payload: HistoryItemPayload::ToolOutput {
                call_id: write_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Rejected write content".to_string(),
                output_text: "The submitted content does not match `docs/workflow-design.md`'s contract. Required positive text artifact shape: real newline-separated Markdown.".to_string(),
                metadata: json!({
                    "operation_progress_class": "required_write_content_shape_mismatch",
                    "content_shape_contract": {
                        "kind": "text_artifact_readable_content_shape",
                        "target": "docs/workflow-design.md"
                    },
                    "tool_feedback_envelope": {
                        "kind": "required_write_content_shape_mismatch",
                        "operation_progress_class": "required_write_content_shape_mismatch",
                        "progress_effect": "no_progress",
                        "content_shape_contract": {
                            "kind": "text_artifact_readable_content_shape",
                            "target": "docs/workflow-design.md"
                        },
                        "side_effects_applied": false
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("docs-shape-mismatch".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["docs/workflow-design.md".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &items, 32, &context);
    let source_read_id = source_read_id.to_string();
    let test_read_id = test_read_id.to_string();
    let write_id = write_id.to_string();
    let read_pairs_omitted = !projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|call| {
                    call.call_id == source_read_id || call.call_id == test_read_id
                })
        ) || matches!(
            message,
            ModelMessage::Tool { call_id, .. }
                if call_id == &source_read_id || call_id == &test_read_id
        )
    });
    let evidence_notes_present = projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::System { content }
                if content.contains("already-consumed evidence")
                    && content.contains("src/workflow.rs")
                    && content.contains("provider-visible edit tool")
        )
    }) && projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::System { content }
                if content.contains("already-consumed evidence")
                    && content.contains("tests/workflow.spec.ts")
        )
    });
    let rejected_write_pair_preserved = projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|call| {
                    call.call_id == write_id
                        && call.tool_name == "write"
                        && call
                            .arguments_json
                            .contains("omitted incompatible write payload")
                        && !call.arguments_json.contains("Serialized markdown")
                })
        )
    }) && projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::Tool { call_id, result, .. }
                if call_id == &write_id
                    && result.contains("Required positive text artifact shape")
        )
    });
    let policies_present = projection
        .replay_policies
        .iter()
        .filter(|policy| policy.policy == "consumed_supporting_context_pair_omitted")
        .count()
        == 2;
    read_pairs_omitted
        && evidence_notes_present
        && rejected_write_pair_preserved
        && policies_present
}

pub(crate) fn exact_write_repair_does_not_consume_untyped_read_as_supporting_context_replay() -> bool
{
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let read_id = crate::session::ToolCallId::new();
    let write_id = crate::session::ToolCallId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "untyped read is not consumed supporting context".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
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
                    text: "Repair docs/workflow-design.md".to_string(),
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
                call_id: read_id,
                tool: ToolName::Read,
                arguments: json!({ "path": "src/workflow.rs" }),
                model_arguments: Value::Null,
                effective_arguments: json!({ "path": "src/workflow.rs" }),
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
                call_id: read_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Read src/workflow.rs".to_string(),
                output_text:
                    "1: pub fn workflow_advance(value: i32) -> i32 {\n2:     value + 1\n3: }"
                        .to_string(),
                metadata: Value::Null,
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::MadeProgress,
                blocked_action: None,
                result_hash: Some("workflow-source-read".to_string()),
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
                call_id: write_id,
                tool: ToolName::Write,
                arguments: json!({
                    "path": "docs/workflow-design.md",
                    "content": "\"# Workflow design\\n\\nSerialized markdown\""
                }),
                model_arguments: Value::Null,
                effective_arguments: json!({
                    "path": "docs/workflow-design.md",
                    "content": "\"# Workflow design\\n\\nSerialized markdown\""
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
            sequence_no: 5,
            created_at_ms: 5,
            payload: HistoryItemPayload::ToolOutput {
                call_id: write_id,
                status: crate::protocol::ToolLifecycleStatus::Completed,
                title: "Rejected write content".to_string(),
                output_text:
                    "The submitted content does not match `docs/workflow-design.md`'s contract."
                        .to_string(),
                metadata: json!({
                    "operation_progress_class": "required_write_content_shape_mismatch",
                    "tool_feedback_envelope": {
                        "kind": "required_write_content_shape_mismatch",
                        "operation_progress_class": "required_write_content_shape_mismatch",
                        "progress_effect": "no_progress",
                        "side_effects_applied": false
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("docs-shape-mismatch".to_string()),
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["docs/workflow-design.md".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &items, 32, &context);
    let read_id = read_id.to_string();
    let read_pair_preserved = projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|call| call.call_id == read_id && call.tool_name == "read")
        )
    }) && projection.messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::Tool { call_id, .. }
                if call_id == &read_id
        )
    });
    let no_consumed_supporting_context_policy = projection
        .replay_policies
        .iter()
        .all(|policy| policy.policy != "consumed_supporting_context_pair_omitted");
    let no_already_consumed_note = projection.messages.iter().all(|message| {
        !matches!(
            message,
            ModelMessage::System { content }
                if content.contains("already-consumed evidence")
        )
    });

    read_pair_preserved && no_consumed_supporting_context_policy && no_already_consumed_note
}

pub fn provider_replay_preserves_current_invalid_edit_argument_feedback() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let call_id = crate::session::ToolCallId::new();
    let orphan_call_id = crate::session::ToolCallId::new();
    let raw_malformed_payload = r#"{"content":"pub fn workflow_result() -> &'static str {\n    \"ok\"\n}", "path":"src/workflow.rs"#;
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "malformed edit replay".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
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
                    text: "repair src/workflow.rs after verification failure".to_string(),
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
                arguments: Value::String(raw_malformed_payload.to_string()),
                model_arguments: Value::Null,
                effective_arguments: Value::Null,
                adjusted_arguments: None,
                permission_decision: None,
                sandbox_decision: None,
                allowed_surface: vec![ToolName::Write, ToolName::ApplyPatch],
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
                title: "Invalid tool arguments".to_string(),
                output_text: "Invalid tool arguments for `write`: EOF while parsing a string at line 1 column 96.\n\n[tool feedback]\noperation_progress_class: invalid_edit_arguments\nprogress_effect: no_progress\nparser_error_family: json_eof\nraw_argument_shape_hash: malformed-fixture-hash\nactive_targets: src/workflow.rs\nRequired action: write:src/workflow.rs".to_string(),
                metadata: json!({
                    "operation_progress_class": "invalid_edit_arguments",
                    "tool_feedback_envelope": {
                        "kind": "invalid_edit_arguments",
                        "operation_progress_class": "invalid_edit_arguments",
                        "parser_error": "EOF while parsing a string at line 1 column 96",
                        "raw_argument_shape_hash": "malformed-fixture-hash",
                        "active_targets": ["src/workflow.rs"],
                        "allowed_surface": ["write", "apply_patch"],
                        "required_action": {
                            "kind": "edit_target",
                            "tool": "write",
                            "target": "src/workflow.rs"
                        },
                        "side_effects_applied": false
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: Some("write".to_string()),
                result_hash: Some("malformed-fixture-hash".to_string()),
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
                output_text: "orphan malformed edit output must not be provider-visible"
                    .to_string(),
                metadata: json!({
                    "operation_progress_class": "invalid_edit_arguments"
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: None,
                verification_run: None,
            },
        },
    ];
    let context = ProviderReplayContext {
        workspace_root: None,
        active_authoring_targets: vec!["src/workflow.rs".to_string()],
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &items, 32, &context);
    let serialized = serde_json::to_string(&projection.messages).unwrap_or_default();
    let call_id_text = call_id.to_string();
    let call_index = projection.messages.iter().position(|message| {
        matches!(
            message,
            ModelMessage::AssistantToolCalls { tool_calls, .. }
                if tool_calls.iter().any(|tool_call| {
                    tool_call.call_id == call_id_text
                        && tool_call.tool_name == "write"
                        && tool_call
                            .arguments_json
                            .contains("omitted malformed edit payload")
                        && !tool_call.arguments_json.contains(raw_malformed_payload)
                        && !tool_call.arguments_json.contains("src/workflow.rs")
                })
        )
    });
    let output_index = projection.messages.iter().position(|message| {
        matches!(
            message,
            ModelMessage::Tool {
                call_id: replayed,
                tool_name,
                result,
                ..
            } if replayed == &call_id_text
                && tool_name == "write"
                && result.contains("invalid_edit_arguments")
                && result.contains("EOF while parsing")
                && result.contains("Required action: write:src/workflow.rs")
        )
    });
    matches!((call_index, output_index), (Some(call), Some(output)) if call < output)
        && !serialized.contains(raw_malformed_payload)
        && !serialized.contains("orphan malformed edit output must not be provider-visible")
        && projection.replay_policies.iter().any(|policy| {
            policy.policy == "malformed_edit_arguments_payload_sanitized_output_preserved"
                && policy.call_id.as_deref() == Some(&call_id_text)
                && policy.tool_name.as_deref() == Some("write")
        })
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
            model: PROMPT_FIXTURE_MODEL.to_string(),
            base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
            access_mode: crate::config::AccessMode::Default,
            model_parameters: crate::session::SessionModelParameters::default(),
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
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
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
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
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
            model: PROMPT_FIXTURE_MODEL.to_string(),
            base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
            access_mode: crate::config::AccessMode::Default,
            model_parameters: crate::session::SessionModelParameters::default(),
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
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
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
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
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
                                    "content": "write src/workflow.rs",
                                    "status": "in_progress",
                                    "priority": "high",
                                    "targets": ["src/workflow.rs"]
                                },
                                {
                                    "id": "test",
                                    "content": "write tests/workflow.spec.ts",
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
    contract.contains("apply_patch")
        && contract.contains("patch_text")
        && contract.contains(target)
        && contract.contains("provider-visible stable code edit surface is `apply_patch`")
        && !contract.contains("ActionAuthority")
        && contract.contains("Older assistant narration")
        && if target_is_test_like(target) {
            if let Some(shape) =
                crate::agent::language_evidence::language_test_artifact_shape_contract(target)
            {
                contract.contains("test module")
                    && contract.contains("Required positive test-module shape")
                    && contract.contains("Forbidden shape")
                    && contract.contains(&format!(
                        "`{}` is the inferred production source",
                        shape.source_path
                    ))
                    && contract.contains(&format!("import `{}`", shape.module_name))
                    && contract.contains(&format!("do not rewrite `{}`", shape.source_path))
                    && contract.contains("Test*")
                    && contract.contains("unittest.TestCase")
            } else {
                contract.contains("Required positive code artifact shape")
                    && contract.contains("real newline-separated code structure")
                    && contract.contains("quote-wrapped whole-file string")
                    && !contract.contains("unittest.TestCase")
            }
        } else if target_is_python_source_like(target) {
            contract.contains("Required positive Python source shape")
                && contract.contains("real newline-separated source structure")
                && contract.contains("quote-wrapped whole-file source string")
        } else {
            contract.contains("active target only")
        }
}

pub(crate) fn text_artifact_content_shape_repair_projection_carries_positive_contract() -> bool {
    let target = "docs/workflow-design.md";
    let session_id = crate::session::SessionId::new();
    let user_message_id = crate::session::MessageId::new();
    let assistant_message_id = crate::session::MessageId::new();
    let call_id = crate::session::ToolCallId::new();
    let serialized_markdown = "\"# Workflow Design\\n\\n## Tests\\n\\n- `tests/workflow.spec.ts` covers `workflow.advance`.\\n\"";
    let transcript = Transcript {
        session: SessionRecord {
            id: session_id,
            project_id: crate::session::ProjectId::new(),
            title: "text artifact content-shape repair".to_string(),
            status: crate::session::SessionStatus::Running,
            cwd: Utf8PathBuf::from("C:/workspace"),
            model: PROMPT_FIXTURE_MODEL.to_string(),
            base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                        text: format!("Create `{target}` from repository evidence."),
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
                        model: PROMPT_FIXTURE_MODEL.to_string(),
                        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
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
                                "path": target,
                                "content": serialized_markdown,
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
                            summary: crate::agent::content_shape_contract::text_artifact_positive_shape_guidance(target),
                            success: Some(false),
                            progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                            blocked_action: None,
                            result_hash: Some("fixture-text-artifact-content-shape".to_string()),
                        }),
                    },
                ],
            },
        ],
    };
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Docs,
        process_phase: ProcessPhase::Author,
        active_targets: vec![Utf8PathBuf::from(target)],
        ..SessionStateSnapshot::default()
    };
    state.completion.open_work_count = 1;
    state.completion.route_contract_pending = true;
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
                    text: format!("Create `{target}` from repository evidence."),
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
                arguments: json!({"path": target, "content": serialized_markdown}),
                model_arguments: Value::Null,
                effective_arguments: json!({"path": target, "content": serialized_markdown}),
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
                title: "Rejected write content".to_string(),
                output_text:
                    crate::agent::content_shape_contract::text_artifact_positive_shape_guidance(
                        target,
                    ),
                metadata: json!({
                    "operation_progress_class": "artifact_content_shape_violation",
                    "content_shape_contract": crate::agent::content_shape_contract::text_artifact_content_shape_metadata(target),
                    "tool_feedback_envelope": {
                        "kind": "artifact_content_shape_violation",
                        "operation_progress_class": "artifact_content_shape_violation",
                        "progress_effect": "no_progress",
                        "content_shape_contract": crate::agent::content_shape_contract::text_artifact_content_shape_metadata(target),
                        "side_effects_applied": false
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-text-artifact-content-shape".to_string()),
                verification_run: None,
            },
        },
    ];
    let messages = build_messages_with_state(
        PromptProjectionInput::from_session(&transcript.session),
        &transcript.session,
        &history_items,
        &state,
        &[],
        50,
        &["apply_patch".to_string(), "write".to_string()],
        &PromptSignals::default(),
        None,
    )
    .messages;
    let system_contract_present = messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::System { content }
                if content.contains("Active write target contract")
                    && content.contains(target)
                    && content.contains("Required positive text artifact shape")
                    && content.contains("real newline-separated document structure")
                    && content.contains("quote-wrapped whole-document string")
        )
    });
    let projection = crate::protocol::ProjectionSurface {
        surface: crate::protocol::ProjectionSurfaceKind::Prompt,
        projection_id: crate::protocol::ProjectionId::new(),
        required_action: Some(crate::protocol::RequiredAction::edit(
            ToolName::Write,
            camino::Utf8PathBuf::from(target),
        )),
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
        forbidden_tools: Vec::new(),
        operation_intents: vec![crate::protocol::OperationIntent::ContentChangingAuthoringRequired],
        obligation_ids: vec!["active_work".to_string(), "control_projection".to_string()],
        contract_refs: Vec::new(),
        evidence_refs: Vec::new(),
    };
    let rendered_projection = projection.render_prompt_block();
    let mut tools = vec![ToolSchema {
        name: "write".to_string(),
        description: "write a file".to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string", "description": "Complete final file contents."}
            }
        }),
        strict: false,
    }];
    apply_active_content_shape_to_write_schema(&mut tools, &state);
    let schema_description = tools
        .first()
        .and_then(|tool| tool.input_schema.pointer("/properties/content/description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    system_contract_present
        && rendered_projection.contains("Required positive artifact shape")
        && rendered_projection.contains("effective Markdown/text")
        && rendered_projection.contains("real newline-separated document structure")
        && rendered_projection.contains("quote-wrapped whole-document string")
        && schema_description.contains("Complete final Markdown/text contents")
        && schema_description.contains("real newline-separated structure")
        && schema_description.contains("quote-wrapped whole-document string")
}

pub(crate) fn prompt_fixtures_are_workflow_neutral_fixture_passes() -> bool {
    prompt_fixtures_workflow_neutral_failures().is_empty()
}

pub(crate) fn prompt_fixtures_workflow_neutral_failures() -> Vec<&'static str> {
    let prompt_fixture_workflow_neutral = "prompt_fixture_workflow_neutral";
    [
        (
            "content_shape_mismatch_replay_preserves_tool_lifecycle_without_payload",
            content_shape_mismatch_replay_preserves_tool_lifecycle_without_payload(),
        ),
        (
            "content_shape_mismatch_replay_requires_typed_metadata",
            content_shape_mismatch_replay_requires_typed_metadata(),
        ),
        (
            "exact_write_repair_omits_consumed_supporting_context_replay",
            exact_write_repair_omits_consumed_supporting_context_replay(),
        ),
        (
            "exact_write_repair_does_not_consume_untyped_read_as_supporting_context_replay",
            exact_write_repair_does_not_consume_untyped_read_as_supporting_context_replay(),
        ),
        (
            "provider_replay_preserves_current_invalid_edit_argument_feedback",
            provider_replay_preserves_current_invalid_edit_argument_feedback(),
        ),
        (
            "stale_todo_progress_replay_omits_prior_plan",
            stale_todo_progress_replay_omits_prior_plan(
                "tests/workflow.spec.ts",
                "Plan: write src/workflow.rs, then write tests/workflow.spec.ts",
            ),
        ),
        (
            "text_artifact_content_shape_repair_projection_carries_positive_contract",
            text_artifact_content_shape_repair_projection_carries_positive_contract(),
        ),
        (
            "active_target_apply_patch_schema_projects_single_operation_skeleton",
            active_target_apply_patch_schema_projects_single_operation_skeleton(),
        ),
        (
            "prompt_fixture_workflow_neutral_marker",
            prompt_fixture_workflow_neutral == "prompt_fixture_workflow_neutral",
        ),
    ]
    .into_iter()
    .filter_map(|(name, passed)| (!passed).then_some(name))
    .collect()
}

pub(crate) fn prompt_content_shape_projection_uses_adapter_contract_fixture_passes() -> bool {
    let initial_description = "complete file content";
    let mut python_tools = vec![ToolSchema {
        name: "write".to_string(),
        description: "write a file".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string", "description": initial_description}
            }
        }),
        strict: true,
    }];
    apply_write_content_shape_to_write_schema_for_target(
        &mut python_tools,
        "tests/test_workflow.py",
    );
    let python_description = python_tools[0]
        .input_schema
        .pointer("/properties/content/description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let expected_python =
        crate::agent::content_shape_contract::artifact_content_shape_tool_schema_description(
            "tests/test_workflow.py",
        )
        .unwrap_or_default();

    let mut js_tools = vec![ToolSchema {
        name: "write".to_string(),
        description: "write a file".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string", "description": initial_description}
            }
        }),
        strict: true,
    }];
    apply_write_content_shape_to_write_schema_for_target(&mut js_tools, "tests/workflow.test.ts");
    let js_description = js_tools[0]
        .input_schema
        .pointer("/properties/content/description")
        .and_then(Value::as_str)
        .unwrap_or_default();

    let expected_js =
        crate::agent::content_shape_contract::artifact_content_shape_tool_schema_description(
            "tests/workflow.test.ts",
        )
        .unwrap_or_default();

    !expected_python.is_empty()
        && !expected_js.is_empty()
        && python_description == expected_python
        && js_description == expected_js
        && expected_js.contains("Required positive code artifact shape")
        && !expected_js.contains("unittest")
}

pub(crate) fn active_target_apply_patch_schema_projects_single_operation_skeleton() -> bool {
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Author,
        active_targets: vec![Utf8PathBuf::from("tests/test_output.py")],
        ..SessionStateSnapshot::default()
    };
    state.completion.open_work_count = 1;
    let mut tools = vec![ToolSchema {
        name: "apply_patch".to_string(),
        description: "apply patch".to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["patch_text"],
            "properties": {
                "patch_text": {
                    "type": "string",
                    "description": "Entire patch text."
                }
            }
        }),
        strict: false,
    }];
    apply_active_target_to_apply_patch_schema(&mut tools, &state);
    let description = tools[0]
        .input_schema
        .pointer("/properties/patch_text/description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    description.contains("Current active target-only patch skeleton")
        && description.contains("*** Add File: tests/test_output.py")
        && description.contains("+<complete content for tests/test_output.py>")
        && description.contains("exactly one file operation")
        && description.contains("no inactive target hunks")
        && description.contains("Required positive")
        && !description.contains("*** Add File: src/output.py")
}

pub(crate) fn python_source_content_shape_repair_projection_carries_positive_contract() -> bool {
    let target = "src/workflow.py";
    let session_id = crate::session::SessionId::new();
    let user_message_id = crate::session::MessageId::new();
    let call_id = crate::session::ToolCallId::new();
    let escaped_source = "\"import math\\n\\ndef square(value):\\n    return value * value\\n\\nif __name__ == \\\"__main__\\\":\\n    print(square(3))\\n\"";
    let transcript = Transcript {
        session: SessionRecord {
            id: session_id,
            project_id: crate::session::ProjectId::new(),
            title: "source content-shape repair".to_string(),
            status: crate::session::SessionStatus::Running,
            cwd: Utf8PathBuf::from("C:/workspace"),
            model: PROMPT_FIXTURE_MODEL.to_string(),
            base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
            access_mode: crate::config::AccessMode::Default,
            model_parameters: crate::session::SessionModelParameters::default(),
            created_at_ms: 1,
            updated_at_ms: 2,
            completed_at_ms: None,
        },
        messages: vec![crate::session::TranscriptMessage {
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
                    text: format!("Repair `{target}` after verification failure."),
                }),
            }],
        }],
    };
    let mut state = SessionStateSnapshot {
        route: TaskRoute::Code,
        process_phase: ProcessPhase::Repair,
        active_targets: vec![Utf8PathBuf::from(target)],
        ..SessionStateSnapshot::default()
    };
    state.completion.open_work_count = 1;
    state.completion.verification_pending = true;
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
                    text: format!("Repair `{target}` after verification failure."),
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
                arguments: json!({"path": target, "content": escaped_source}),
                model_arguments: Value::Null,
                effective_arguments: json!({"path": target, "content": escaped_source}),
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
                title: "Rejected write content".to_string(),
                output_text:
                    crate::agent::content_shape_contract::artifact_content_shape_positive_guidance(
                        target,
                    )
                    .unwrap_or_default(),
                metadata: json!({
                    "operation_progress_class": "artifact_content_shape_violation",
                    "content_shape_contract": crate::agent::content_shape_contract::artifact_content_shape_metadata_for_feedback(target),
                    "tool_feedback_envelope": {
                        "kind": "artifact_content_shape_violation",
                        "operation_progress_class": "artifact_content_shape_violation",
                        "progress_effect": "no_progress",
                        "content_shape_contract": crate::agent::content_shape_contract::artifact_content_shape_metadata_for_feedback(target),
                        "side_effects_applied": false
                    }
                }),
                success: Some(false),
                progress_effect: crate::protocol::ToolProgressEffect::NoProgress,
                blocked_action: None,
                result_hash: Some("fixture-python-source-content-shape".to_string()),
                verification_run: None,
            },
        },
    ];
    let messages = build_messages_with_state(
        PromptProjectionInput::from_session(&transcript.session),
        &transcript.session,
        &history_items,
        &state,
        &[],
        50,
        &["apply_patch".to_string(), "write".to_string()],
        &PromptSignals::default(),
        None,
    )
    .messages;
    let system_contract_present = messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::System { content }
                if content.contains("Active write target contract")
                    && content.contains(target)
                    && content.contains("Required positive Python source shape")
                    && content.contains("real newline-separated source structure")
                    && content.contains("quote-wrapped whole-file source string")
        )
    });
    let projection = crate::protocol::ProjectionSurface {
        surface: crate::protocol::ProjectionSurfaceKind::Prompt,
        projection_id: crate::protocol::ProjectionId::new(),
        required_action: Some(crate::protocol::RequiredAction::edit(
            ToolName::Write,
            camino::Utf8PathBuf::from(target),
        )),
        allowed_tools: vec![ToolName::ApplyPatch, ToolName::Write],
        forbidden_tools: Vec::new(),
        operation_intents: vec![crate::protocol::OperationIntent::ContentChangingAuthoringRequired],
        obligation_ids: vec!["active_work".to_string(), "control_projection".to_string()],
        contract_refs: Vec::new(),
        evidence_refs: Vec::new(),
    };
    let rendered_projection = projection.render_prompt_block();
    let mut tools = vec![ToolSchema {
        name: "write".to_string(),
        description: "write a file".to_string(),
        input_schema: json!({
            "type": "object",
            "required": ["path", "content"],
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string", "description": "Complete final file contents."}
            }
        }),
        strict: false,
    }];
    apply_active_content_shape_to_write_schema(&mut tools, &state);
    let schema_description = tools
        .first()
        .and_then(|tool| tool.input_schema.pointer("/properties/content/description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    system_contract_present
        && rendered_projection.contains("Required positive artifact shape")
        && rendered_projection.contains("effective Python module text")
        && rendered_projection.contains("real newline-separated source structure")
        && rendered_projection.contains("quote-wrapped serialized source")
        && schema_description.contains("Complete final Python source contents")
        && schema_description.contains("real newline-separated source structure")
        && schema_description.contains("quote-wrapped whole-file source string")
}

pub(crate) fn exact_authoring_write_required_preserves_source_progress_projection() -> bool {
    let mut source_state = SessionStateSnapshot {
        process_phase: ProcessPhase::Author,
        active_targets: vec![Utf8PathBuf::from("src/workflow.py")],
        ..SessionStateSnapshot::default()
    };
    source_state.completion.open_work_count = 1;
    let mut test_state = source_state.clone();
    test_state.active_targets = vec![Utf8PathBuf::from("tests/test_workflow.py")];
    exact_active_authoring_write_required(&source_state).is_none()
        && exact_active_authoring_write_required(&test_state).as_deref()
            == Some("tests/test_workflow.py")
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
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let call_id = crate::session::ToolCallId::new();
    let original_user_text = "create src/workflow.py and tests/test_workflow.py";
    let current_hook_text = "Verification-repair continuation: repair src/workflow.py, then rerun verify-workflow --behavior.";
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
                arguments: json!({"command": "verify-workflow --behavior"}),
                model_arguments: json!({"command": "verify-workflow --behavior"}),
                effective_arguments: json!({"command": "verify-workflow --behavior"}),
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
                        "repair src/workflow.py then rerun verify-workflow --behavior".to_string(),
                    ),
                    target_files: vec![Utf8PathBuf::from("src/workflow.py")],
                    verification_commands: vec!["verify-workflow --behavior".to_string()],
                    failure_kind: Some("VerificationFailed".to_string()),
                    failure_summary: Some("unit test failed".to_string()),
                    completion_blocker: Some("verification failed".to_string()),
                    invariant_refs: vec!["CompactionContinuity".to_string()],
                    ..crate::session::ContinuationContract::default()
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

pub(crate) fn compaction_provider_context_projects_typed_contract_before_summary() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "compaction provider context order".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let items = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::MidTurn,
                summary: "Conversation summary prose mentions obsolete-target.txt.".to_string(),
                replacement_item_ids: vec![crate::protocol::HistoryItemId::new()],
                continuation: Some(crate::session::ContinuationContract {
                    route: "code".to_string(),
                    process_phase: "repair".to_string(),
                    active_work_kind: Some("typed_continuation".to_string()),
                    active_work_summary: Some("repair src/lib.rs".to_string()),
                    target_files: vec![Utf8PathBuf::from("src/lib.rs")],
                    invariant_refs: vec!["CompactionContinuity".to_string()],
                    ..crate::session::ContinuationContract::default()
                }),
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "continue".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
    ];
    let replay = build_provider_replay_messages_from_history_items(&session, &items, 32);
    let Some(ModelMessage::System { content }) = replay.first() else {
        return false;
    };
    let Some(typed_index) = content.find("Typed continuation contract:") else {
        return false;
    };
    let Some(summary_index) = content.find("Conversation summary from earlier turns:") else {
        return false;
    };
    typed_index < summary_index
        && content.contains("\"target_files\":[\"src/lib.rs\"]")
        && content.contains("\"invariant_refs\":[\"CompactionContinuity\"]")
        && content.contains("Conversation summary prose mentions obsolete-target.txt")
        && matches!(replay.get(1), Some(ModelMessage::User { .. }))
}

pub(crate) fn compaction_replay_uses_typed_history_authority_fixture_passes() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "compaction replay typed authority".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 3,
        completed_at_ms: None,
    };
    let legacy_summary_message_id = crate::session::MessageId::new();
    let legacy_transcript = Transcript {
        session: session.clone(),
        messages: vec![crate::session::TranscriptMessage {
            record: crate::session::MessageRecord {
                id: legacy_summary_message_id,
                session_id,
                role: MessageRole::Assistant,
                parent_message_id: None,
                sequence_no: 1,
                created_at_ms: 1,
                metadata: MessageMetadata::Assistant(crate::session::AssistantMessageMeta {
                    model: PROMPT_FIXTURE_MODEL.to_string(),
                    base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
                    finish_reason: None,
                    token_usage: None,
                    summary: true,
                }),
            },
            parts: vec![crate::session::PartRecord {
                id: crate::session::PartId::new(),
                message_id: legacy_summary_message_id,
                sequence_no: 1,
                kind: crate::session::PartKind::Text,
                payload: MessagePart::Text(crate::session::TextPart {
                    text: "legacy transcript summary without canonical compaction".to_string(),
                }),
            }],
        }],
    };
    let history_without_compaction = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 2,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "continue current work".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let mut state = SessionStateSnapshot::default();
    state.route = TaskRoute::Code;
    state.process_phase = ProcessPhase::Author;
    state.active_targets = vec![Utf8PathBuf::from("src/lib.rs")];
    state.completion.open_work_count = 1;
    let agent_config = ResolvedConfig::default().agent;
    let legacy_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&legacy_transcript.session),
        &history_without_compaction,
        &[],
        &agent_config,
        Some(&state),
    );
    let legacy_projection = build_messages_with_state(
        PromptProjectionInput::from_session(&session),
        &session,
        &history_without_compaction,
        &state,
        &[],
        32,
        &[],
        &legacy_signals,
        None,
    );
    let legacy_projected_replay =
        prompt_messages_contain_compaction_replay_reminder(&legacy_projection.messages);

    let history_with_compaction = vec![
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 1,
            created_at_ms: 1,
            payload: HistoryItemPayload::Compaction {
                mode: crate::protocol::CompactionMode::PreTurn,
                summary: "CompactionContinuity\nContinue src/lib.rs authoring.".to_string(),
                replacement_item_ids: Vec::new(),
                continuation: Some(crate::session::ContinuationContract {
                    route: "code".to_string(),
                    process_phase: "author".to_string(),
                    active_work_kind: Some("typed_continuation".to_string()),
                    active_work_summary: Some("continue src/lib.rs authoring".to_string()),
                    target_files: vec![Utf8PathBuf::from("src/lib.rs")],
                    invariant_refs: vec!["CompactionContinuity".to_string()],
                    ..crate::session::ContinuationContract::default()
                }),
            },
        },
        HistoryItem {
            id: crate::protocol::HistoryItemId::new(),
            session_id,
            turn_id,
            sequence_no: 2,
            created_at_ms: 2,
            payload: HistoryItemPayload::UserTurn {
                message_id: None,
                content: vec![ContentPart::Text {
                    text: "continue current work".to_string(),
                }],
                prompt_dispatch: None,
                editor_context: None,
                turn_context: None,
            },
        },
    ];
    let typed_transcript = transcript_from_history_items(&session, &history_with_compaction);
    let typed_signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&typed_transcript.session),
        &history_with_compaction,
        &[],
        &agent_config,
        Some(&state),
    );
    let typed_projection = build_messages_with_state(
        PromptProjectionInput::from_session(&session),
        &session,
        &history_with_compaction,
        &state,
        &[],
        32,
        &[],
        &typed_signals,
        None,
    );
    let typed_projected_replay =
        prompt_messages_contain_compaction_replay_reminder(&typed_projection.messages);

    !legacy_signals.compaction_replay
        && !legacy_projected_replay
        && typed_signals.compaction_replay
        && typed_projected_replay
}

pub(crate) fn prompt_projection_workspace_root_uses_typed_runtime_input_fixture_passes() -> bool {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let typed_root = match Utf8PathBuf::from_path_buf(
        std::env::temp_dir().join(format!("moyai-typed-runtime-root-{nonce}")),
    ) {
        Ok(path) => path,
        Err(_) => return false,
    };
    let legacy_root = match Utf8PathBuf::from_path_buf(
        std::env::temp_dir().join(format!("moyai-legacy-transcript-root-{nonce}")),
    ) {
        Ok(path) => path,
        Err(_) => return false,
    };
    if fs::create_dir_all(typed_root.as_std_path()).is_err()
        || fs::create_dir_all(legacy_root.as_std_path()).is_err()
    {
        return false;
    }
    if fs::write(
        typed_root.join("instructions.md").as_std_path(),
        "Follow the typed runtime workspace.\nRun `cargo test --typed-runtime-root`.",
    )
    .is_err()
        || fs::write(
            legacy_root.join("instructions.md").as_std_path(),
            "Follow the compatibility transcript workspace.\nRun `cargo test --legacy-transcript-root`.",
        )
        .is_err()
    {
        return false;
    }

    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let typed_session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "prompt projection typed workspace root".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: typed_root,
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 2,
        completed_at_ms: None,
    };
    let mut legacy_session = typed_session.clone();
    legacy_session.cwd = legacy_root;
    let _transcript = Transcript {
        session: legacy_session,
        messages: Vec::new(),
    };
    let history = vec![HistoryItem {
        id: crate::protocol::HistoryItemId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        payload: HistoryItemPayload::UserTurn {
            message_id: None,
            content: vec![ContentPart::Text {
                text: "Use `instructions.md` to produce `summary.md`.".to_string(),
            }],
            prompt_dispatch: None,
            editor_context: None,
            turn_context: None,
        },
    }];
    let agent_config = ResolvedConfig::default().agent;
    let signals = detect_prompt_signals_with_config(
        PromptProjectionInput::from_session(&typed_session),
        &history,
        &[],
        &agent_config,
        None,
    );
    signals
        .staged_task_verification_commands
        .iter()
        .any(|command| command.contains("cargo test --typed-runtime-root"))
        && !signals
            .staged_task_verification_commands
            .iter()
            .any(|command| command.contains("cargo test --legacy-transcript-root"))
}

fn prompt_messages_contain_compaction_replay_reminder(messages: &[ModelMessage]) -> bool {
    messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::System { content } if content.contains(compaction_replay_reminder())
        )
    })
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
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
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
                    target_files: Vec::new(),
                    verification_commands: Vec::new(),
                    failure_kind: None,
                    failure_summary: None,
                    completion_blocker: None,
                    invariant_refs: vec!["CompactionContinuity".to_string()],
                    ..crate::session::ContinuationContract::default()
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
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
    let hook_text = "Closeout continuation: create tests/workflow.spec.ts.";
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
                model_arguments: json!({"path": "src/workflow.rs"}),
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
                title: "Read src/workflow.rs".to_string(),
                output_text: "pub struct Workflow;".to_string(),
                metadata: json!({"success": true}),
                success: Some(true),
                progress_effect: crate::protocol::ToolProgressEffect::Unknown,
                blocked_action: None,
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
                        && tool_call.arguments_json.contains("src/workflow.rs")
                })
        )
    });
    let output_index = replay.iter().position(|message| {
        matches!(
            message,
            ModelMessage::Tool { call_id: replayed, result, .. }
                if replayed == &call_id_text && result.contains("pub struct Workflow")
        )
    });
    let user_index = replay.iter().rposition(
        |message| matches!(message, ModelMessage::User { content } if content == hook_text),
    );

    matches!((call_index, output_index, user_index), (Some(call), Some(output), Some(user)) if call < output && output < user)
        && !serialized.contains("orphan output must not be provider-visible")
}

pub fn provider_replay_projects_rejected_final_message_evidence() -> bool {
    let session_id = crate::session::SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let source_call_id = crate::session::ToolCallId::new();
    let projection_id = crate::protocol::ProjectionId::new();
    let session = SessionRecord {
        id: session_id,
        project_id: crate::session::ProjectId::new(),
        title: "rejected final replay fixture".to_string(),
        status: crate::session::SessionStatus::Running,
        cwd: Utf8PathBuf::from("C:/workspace/project"),
        model: PROMPT_FIXTURE_MODEL.to_string(),
        base_url: PROMPT_FIXTURE_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::Default,
        model_parameters: crate::session::SessionModelParameters::default(),
        created_at_ms: 1,
        updated_at_ms: 1,
        completed_at_ms: None,
    };
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
                    text: "create src/workflow.rs and tests/workflow.spec.ts".to_string(),
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
            payload: HistoryItemPayload::RejectedToolProposal {
                proposal: crate::protocol::RejectedToolProposal {
                    proposal_id: crate::protocol::ToolProposalId::new(),
                    source_call_id,
                    requested_tool: "final_assistant_message".to_string(),
                    effective_tool: "final_assistant_message".to_string(),
                    resolved_tool: ToolName::Invalid,
                    original_arguments: json!({}),
                    adjusted_arguments: None,
                    allowed_surface: vec![ToolName::ApplyPatch, ToolName::Write, ToolName::Shell],
                    blocked_reason:
                        "The provider emitted a final message while obligations remain open."
                            .to_string(),
                    projection_id,
                    semantic_class: "text_final_while_obligations_open".to_string(),
                    candidate_repair_id: None,
                    payload_hash: "payload-hash".to_string(),
                    contract_refs: vec!["contract:open_obligation".to_string()],
                    evidence_refs: vec!["artifact:run".to_string()],
                },
            },
        },
    ];

    let replay_context = ProviderReplayContext {
        active_authoring_targets: vec!["tests/workflow.spec.ts".to_string()],
        workspace_root: Some(Utf8PathBuf::from("C:/workspace/project")),
    };
    let projection =
        build_provider_replay_projection_from_history_items(&session, &items, 32, &replay_context);
    let user_index = projection.messages.iter().position(|message| {
        matches!(
            message,
            ModelMessage::User { content }
                if content.contains("create src/workflow.rs and tests/workflow.spec.ts")
        )
    });
    let evidence_index = projection.messages.iter().position(|message| {
        matches!(
            message,
            ModelMessage::System { content }
                if content.contains("Rejected model action evidence")
                    && content.contains("final_assistant_message")
                    && content.contains("text_final_while_obligations_open")
                    && content.contains("current TurnControlEnvelope")
                    && content.contains("Allowed tool surface: [apply_patch, write, shell]")
        )
    });
    let policy_present = projection.replay_policies.iter().any(|policy| {
        policy.policy == "rejected_final_assistant_message_non_executable_replay"
            && policy.tool_name.as_deref() == Some("final_assistant_message")
            && policy
                .active_targets
                .iter()
                .any(|target| target == "tests/workflow.spec.ts")
    });
    matches!((user_index, evidence_index), (Some(user), Some(evidence)) if user < evidence)
        && policy_present
}

pub(crate) fn prompt_residual_fixtures_are_workflow_neutral_fixture_passes() -> bool {
    let prompt_residual_fixture_workflow_neutral = "prompt_residual_fixture_workflow_neutral";
    python_source_content_shape_repair_projection_carries_positive_contract()
        && exact_authoring_write_required_preserves_source_progress_projection()
        && provider_replay_preserves_latest_user_across_trailing_compaction()
        && compaction_provider_context_projects_typed_contract_before_summary()
        && provider_replay_preserves_tool_pair_symmetry_with_model_arguments()
        && provider_replay_projects_rejected_final_message_evidence()
        && prompt_residual_fixture_workflow_neutral == "prompt_residual_fixture_workflow_neutral"
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

fn superseded_tool_denial_replay_state_from_history(
    history_items: &[HistoryItem],
    start_index: usize,
    current_tool_names: &[String],
) -> (Vec<String>, BTreeSet<String>) {
    let mut denied_tools: Vec<String> = Vec::new();
    let mut suppressed_ids = BTreeSet::new();
    let scan_start = latest_user_history_index(history_items, start_index)
        .map(|index| index + 1)
        .unwrap_or(start_index);
    for item in history_items.iter().skip(scan_start) {
        match &item.payload {
            HistoryItemPayload::RejectedToolProposal { proposal } => {
                if !typed_superseded_tool_denial_rejected_proposal(proposal) {
                    continue;
                }
                let candidates = superseded_denial_candidate_tools_from_proposal(proposal);
                if push_current_denied_tools(&candidates, current_tool_names, &mut denied_tools) {
                    suppressed_ids.insert(proposal.source_call_id.to_string());
                }
            }
            HistoryItemPayload::ToolOutput {
                call_id, metadata, ..
            } => {
                if !typed_superseded_tool_denial_metadata(metadata) {
                    continue;
                }
                let candidates = superseded_denial_candidate_tools_from_metadata(metadata);
                if push_current_denied_tools(&candidates, current_tool_names, &mut denied_tools) {
                    suppressed_ids.insert(call_id.to_string());
                }
            }
            _ => {}
        }
    }
    (denied_tools, suppressed_ids)
}

fn typed_superseded_tool_denial_rejected_proposal(
    proposal: &crate::protocol::RejectedToolProposal,
) -> bool {
    matches!(
        proposal.semantic_class.as_str(),
        "tool_outside_allowed_surface" | "provider_noncompliance" | "unavailable_tool"
    )
}

fn typed_superseded_tool_denial_metadata(metadata: &Value) -> bool {
    matches!(
        tool_output_metadata_kind(metadata),
        Some("tool_outside_allowed_surface" | "provider_noncompliance" | "unavailable_tool")
    )
}

fn superseded_denial_candidate_tools_from_proposal(
    proposal: &crate::protocol::RejectedToolProposal,
) -> Vec<String> {
    let mut candidates = Vec::new();
    push_unique_tool_candidate(&mut candidates, &proposal.effective_tool);
    push_unique_tool_candidate(&mut candidates, &proposal.requested_tool);
    if proposal.resolved_tool != ToolName::Invalid {
        push_unique_tool_candidate(&mut candidates, &proposal.resolved_tool.to_string());
    }
    candidates
}

fn superseded_denial_candidate_tools_from_metadata(metadata: &Value) -> Vec<String> {
    let mut candidates = Vec::new();
    for pointer in [
        "/tool_feedback_envelope/effective_tool",
        "/tool_feedback_envelope/requested_tool",
        "/tool_feedback_envelope/tool",
        "/effective_tool",
        "/requested_tool",
        "/tool",
    ] {
        if let Some(tool) = metadata.pointer(pointer).and_then(Value::as_str) {
            push_unique_tool_candidate(&mut candidates, tool);
        }
    }
    candidates
}

fn push_current_denied_tools(
    candidates: &[String],
    current_tool_names: &[String],
    denied_tools: &mut Vec<String>,
) -> bool {
    let mut pushed = false;
    for candidate in candidates {
        let Some(current_name) = current_tool_names
            .iter()
            .find(|tool| tool.eq_ignore_ascii_case(candidate.as_str()))
        else {
            continue;
        };
        if denied_tools
            .iter()
            .any(|tool| tool.eq_ignore_ascii_case(current_name.as_str()))
        {
            pushed = true;
            continue;
        }
        denied_tools.push(current_name.clone());
        pushed = true;
    }
    pushed
}

fn push_unique_tool_candidate(candidates: &mut Vec<String>, tool: &str) {
    let trimmed = tool.trim();
    if trimmed.is_empty() {
        return;
    }
    if candidates
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(trimmed))
    {
        return;
    }
    candidates.push(trimmed.to_string());
}

fn latest_denied_edit_targets_after_latest_user_from_history(
    history_items: &[HistoryItem],
    start_index: usize,
) -> Vec<String> {
    let Some(latest_user) = latest_user_history_index(history_items, start_index) else {
        return Vec::new();
    };

    let mut tool_calls: HashMap<String, (String, String)> = HashMap::new();
    for item in history_items.iter().skip(latest_user + 1) {
        match &item.payload {
            HistoryItemPayload::ToolCall {
                call_id,
                tool,
                arguments,
                model_arguments,
                effective_arguments,
                ..
            } => {
                let arguments_json =
                    replay_tool_arguments_json(arguments, model_arguments, effective_arguments);
                tool_calls.insert(call_id.to_string(), (tool.to_string(), arguments_json));
            }
            _ => {}
        }
    }

    for item in history_items.iter().skip(latest_user + 1).rev() {
        match &item.payload {
            HistoryItemPayload::ToolOutput {
                call_id,
                status,
                metadata,
                blocked_action,
                ..
            } if *status == ToolLifecycleStatus::Completed
                && typed_unavailable_edit_feedback_metadata(metadata) =>
            {
                let metadata_targets = denied_edit_targets_from_metadata(metadata)
                    .or_else(|| {
                        blocked_action
                            .as_deref()
                            .and_then(denied_edit_targets_from_blocked_action)
                    })
                    .unwrap_or_default();
                if !metadata_targets.is_empty() {
                    return metadata_targets;
                }
                if let Some((tool_name, arguments_json)) = tool_calls.get(&call_id.to_string()) {
                    return prompt_edit_targets_from_arguments_json(tool_name, arguments_json);
                }
            }
            HistoryItemPayload::RejectedToolProposal { proposal }
                if typed_unavailable_edit_rejected_proposal(proposal) =>
            {
                let arguments = proposal
                    .adjusted_arguments
                    .as_ref()
                    .unwrap_or(&proposal.original_arguments);
                let arguments_json =
                    serde_json::to_string(arguments).unwrap_or_else(|_| arguments.to_string());
                let targets = prompt_edit_targets_from_arguments_json(
                    &proposal.effective_tool,
                    &arguments_json,
                );
                if !targets.is_empty() {
                    return targets;
                }
            }
            _ => {}
        }
    }
    Vec::new()
}

fn typed_unavailable_edit_feedback_metadata(metadata: &Value) -> bool {
    matches!(
        tool_output_metadata_kind(metadata),
        Some("tool_outside_allowed_surface" | "provider_noncompliance" | "unavailable_tool")
    )
}

fn typed_unavailable_edit_rejected_proposal(
    proposal: &crate::protocol::RejectedToolProposal,
) -> bool {
    matches!(
        proposal.semantic_class.as_str(),
        "tool_outside_allowed_surface" | "provider_noncompliance"
    ) && is_write_tool_name(&proposal.effective_tool)
}

fn denied_edit_targets_from_metadata(metadata: &Value) -> Option<Vec<String>> {
    let targets = [
        "/tool_feedback_envelope/submitted_targets",
        "/tool_feedback_envelope/targets",
        "/submitted_targets",
        "/targets",
    ]
    .iter()
    .find_map(|pointer| {
        metadata
            .pointer(pointer)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .flat_map(|target| normalize_artifact_token(target).into_iter())
                    .collect::<Vec<_>>()
            })
    })
    .filter(|targets| !targets.is_empty())
    .or_else(|| {
        [
            "/tool_feedback_envelope/submitted_target",
            "/tool_feedback_envelope/target",
            "/tool_feedback_envelope/required_next_action/target",
            "/target",
            "/required_next_action/target",
        ]
        .iter()
        .find_map(|pointer| {
            metadata
                .pointer(pointer)
                .and_then(Value::as_str)
                .and_then(normalize_artifact_token)
                .map(|target| vec![target])
        })
    })
    .or_else(|| {
        [
            "/tool_feedback_envelope/blocked_action",
            "/blocked_action",
            "/tool_feedback_envelope/required_action",
            "/required_action",
        ]
        .iter()
        .find_map(|pointer| {
            metadata
                .pointer(pointer)
                .and_then(Value::as_str)
                .and_then(denied_edit_targets_from_blocked_action)
        })
    });

    targets.map(dedupe_targets)
}

fn denied_edit_targets_from_blocked_action(action: &str) -> Option<Vec<String>> {
    let (tool, target) = action.split_once(':')?;
    if !is_write_tool_name(tool.trim()) {
        return None;
    }
    normalize_artifact_token(target).map(|target| vec![target])
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
    history_items: &[HistoryItem],
    start_index: usize,
) -> Vec<String> {
    let Some(latest_user) = latest_user_history_index(history_items, start_index) else {
        return Vec::new();
    };

    let mut targets = Vec::new();
    for item in history_items.iter().skip(latest_user + 1) {
        if let HistoryItemPayload::FileChange { changes, .. } = &item.payload {
            for change in changes {
                if let Some(path) = change
                    .path_after
                    .as_ref()
                    .or(change.path_before.as_ref())
                    .map(|path| path.as_str().to_string())
                {
                    targets.push(path);
                }
            }
        }
    }
    dedupe_targets(targets)
}

fn staged_task_output_targets_read_after_latest_user(
    history_items: &[HistoryItem],
    start_index: usize,
    required_targets: &[String],
) -> bool {
    if required_targets.is_empty() {
        return false;
    }
    let Some(latest_user) = latest_user_history_index(history_items, start_index) else {
        return false;
    };

    let mut readonly_targets_by_call = HashMap::new();
    for item in history_items.iter().skip(latest_user + 1) {
        if let HistoryItemPayload::ToolCall {
            call_id,
            tool,
            arguments,
            effective_arguments,
            ..
        } = &item.payload
        {
            if let Some(target) = extract_readonly_target_from_value(
                tool,
                history_tool_arguments(arguments, effective_arguments),
            ) {
                readonly_targets_by_call.insert(call_id.to_string(), target);
            }
        }
    }

    let mut successful_reads = BTreeSet::new();
    for item in history_items.iter().skip(latest_user + 1) {
        if let HistoryItemPayload::ToolOutput {
            call_id,
            status,
            success,
            progress_effect,
            ..
        } = &item.payload
        {
            if !history_tool_output_is_successful(*status, *success, progress_effect) {
                continue;
            }
            let Some(target) = readonly_targets_by_call.get(&call_id.to_string()) else {
                continue;
            };
            for required in required_targets {
                if prompt_target_matches_required_output(target, std::slice::from_ref(required)) {
                    successful_reads.insert(normalize_prompt_target(required));
                }
            }
        }
    }

    required_targets
        .iter()
        .all(|target| successful_reads.contains(&normalize_prompt_target(target)))
}

fn staged_task_output_targets_changed_after_latest_user(
    history_items: &[HistoryItem],
    start_index: usize,
    required_targets: &[String],
) -> bool {
    if required_targets.is_empty() {
        return false;
    }

    let changed_targets = changed_artifact_targets_after_latest_user(history_items, start_index);
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
    history_items: &[HistoryItem],
    start_index: usize,
    documentation_targets: &[String],
) -> Option<String> {
    let focus_targets = staged_task_documentation_focus_targets(documentation_targets);
    if focus_targets.is_empty() {
        return None;
    }
    let Some(latest_user) = latest_user_turn_index_after(history_items, start_index) else {
        return None;
    };

    let mut readonly_targets_by_call = HashMap::new();
    let mut readonly_tool_names_by_call = HashMap::new();
    for item in &history_items[latest_user + 1..] {
        if let HistoryItemPayload::ToolCall {
            call_id,
            tool,
            arguments,
            effective_arguments,
            ..
        } = &item.payload
        {
            if let Some(target) = extract_readonly_target_from_value(
                tool,
                history_tool_arguments(arguments, effective_arguments),
            ) {
                readonly_targets_by_call.insert(call_id.to_string(), target);
                readonly_tool_names_by_call.insert(call_id.to_string(), tool.to_string());
            }
        }
    }

    let mut seen = BTreeSet::new();
    let mut lines = Vec::new();
    for item in &history_items[latest_user + 1..] {
        let HistoryItemPayload::ToolOutput {
            call_id,
            status,
            output_text,
            success,
            progress_effect,
            ..
        } = &item.payload
        else {
            continue;
        };
        if output_text.trim().is_empty()
            || !history_tool_output_is_successful(*status, *success, progress_effect)
        {
            continue;
        }
        let tool_call_id = call_id.to_string();
        let Some(target) = readonly_targets_by_call.get(&tool_call_id) else {
            continue;
        };
        let Some(tool_name) = readonly_tool_names_by_call.get(&tool_call_id) else {
            continue;
        };
        if let Some(line) =
            staged_task_documentation_evidence_line(tool_name, target, output_text, &focus_targets)
        {
            if seen.insert(line.clone()) {
                lines.push(line);
                if lines.len() >= STAGED_TASK_EVIDENCE_LINE_LIMIT {
                    break;
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

fn exact_write_target_contract(target: &str) -> String {
    let content_contract =
        crate::agent::content_shape_contract::artifact_content_shape_positive_guidance(target)
            .unwrap_or_else(|| {
                "The patch must create or update the active target only. Do not paste content from a completed or inactive target.".to_string()
            });
    format!(
        "Active apply_patch target contract:\n- Use the `apply_patch` tool with `patch_text` that adds or updates `{target}`.\n- The provider-visible stable code edit surface is `apply_patch`; target validation belongs to the tool lifecycle for the submitted call.\n- {content_contract}\n- Older assistant narration, previous tool arguments, and prior progress output are not tool-call authority for this turn."
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
    let mut normalized = target.trim().replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    normalized
}

fn prompt_target_matches_required_output(target: &str, required_targets: &[String]) -> bool {
    let normalized_target = normalize_prompt_target(target).to_ascii_lowercase();
    required_targets.iter().any(|required| {
        let normalized_required = normalize_prompt_target(required).to_ascii_lowercase();
        normalized_target == normalized_required
    })
}

pub(crate) fn prompt_staged_task_target_identity_exact_fixture_passes() -> bool {
    let required = vec!["docs/workflow-design.md".to_string()];
    prompt_target_matches_required_output("docs/workflow-design.md", &required)
        && prompt_target_matches_required_output(".\\docs\\workflow-design.md", &required)
        && !prompt_target_matches_required_output("C:/workspace/docs/workflow-design.md", &required)
        && !prompt_target_matches_required_output(
            "C:/other/workspace/docs/workflow-design.md",
            &required,
        )
        && !prompt_target_matches_required_output("../docs/workflow-design.md", &required)
        && !prompt_target_matches_required_output("sibling/docs/workflow-design.md", &required)
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

pub fn runtime_input_view_no_compatibility_transcript_authority_fixture_passes() -> bool {
    let source = include_str!("prompt.rs");
    let Some(runtime_struct) = source
        .split("pub struct RuntimeInputView")
        .nth(1)
        .and_then(|tail| tail.split("}\n\nimpl RuntimeInputView").next())
    else {
        return false;
    };
    let Some(runtime_impl) = source
        .split("impl RuntimeInputView")
        .nth(1)
        .and_then(|tail| {
            tail.split("\n}\n\n#[derive(Debug, Clone)]\npub struct PromptBundle")
                .next()
        })
    else {
        return false;
    };
    let runtime_input_source = format!("{runtime_struct}\n{runtime_impl}");
    let legacy_helper = ["materialized", "transcript", "projection"].join("_");
    let legacy_constructor = ["pub fn from_history_items(session", "&SessionRecord"].join(": ");
    let legacy_session_field = ["session", " SessionRecord,"].join(":");
    let legacy_session_clone = ["session", " session.clone()"].join(":");
    let legacy_materialization = [
        "transcript_from_history_items(&self.session",
        "&self.history_items)",
    ]
    .join(", ");

    !runtime_input_source.contains(&legacy_helper)
        && !runtime_input_source.contains(&legacy_constructor)
        && !runtime_input_source.contains(&legacy_session_field)
        && !runtime_input_source.contains(&legacy_session_clone)
        && !runtime_input_source.contains(&legacy_materialization)
}

#[cfg(test)]
mod tests {
    #[test]
    fn stale_inactive_authoring_replay_live_builder_fixture_passes() {
        assert!(super::stale_inactive_authoring_replay_uses_live_builder());
    }

    #[test]
    fn stale_inactive_filechange_only_reference_snapshot_fixture_passes() {
        assert!(
            super::stale_inactive_filechange_without_replayable_tool_call_uses_reference_snapshot()
        );
    }

    #[test]
    fn metadata_only_tool_output_filechange_reference_snapshot_rejected_fixture_passes() {
        assert!(super::metadata_only_tool_output_does_not_create_filechange_reference_snapshot());
    }

    #[test]
    fn stale_inactive_authoring_prelude_replay_fixture_passes() {
        assert!(super::provider_replay_omits_stale_inactive_authoring_prelude_text());
    }

    #[test]
    fn stale_progress_projection_replay_live_builder_fixture_passes() {
        assert!(super::stale_progress_projection_replay_uses_live_builder());
    }

    #[test]
    fn typed_prompt_feedback_projection_fixture_passes() {
        assert!(super::prompt_projection_uses_typed_tool_output_feedback_fixture_passes());
    }

    #[test]
    fn message_only_history_prompt_state_authority_fixture_passes() {
        assert!(
            super::message_only_history_does_not_recreate_tool_lifecycle_prompt_state_fixture_passes(
            )
        );
    }

    #[test]
    fn verification_repair_read_budget_history_authority_fixture_passes() {
        assert!(
            super::verification_repair_read_budget_exhaustion_uses_typed_history_item_authority_fixture_passes(
            )
        );
    }

    #[test]
    fn verification_repair_target_rotation_uses_typed_history_item_authority() {
        assert!(
            super::verification_repair_target_rotation_uses_typed_history_item_authority_fixture_passes(
            )
        );
    }

    #[test]
    fn verification_evidence_uses_typed_history_item_authority() {
        assert!(super::verification_evidence_uses_typed_history_item_authority_fixture_passes());
    }

    #[test]
    fn staged_task_closeout_repair_targets_use_typed_history_authority() {
        assert!(
            super::staged_task_closeout_repair_targets_use_typed_history_authority_fixture_passes()
        );
    }

    #[test]
    fn staged_task_recovery_stall_uses_typed_history_authority() {
        assert!(super::staged_task_recovery_stall_uses_typed_history_authority_fixture_passes());
    }

    #[test]
    fn staged_task_output_lifecycle_uses_typed_history_authority() {
        assert!(super::staged_task_output_lifecycle_uses_typed_history_authority_fixture_passes());
    }

    #[test]
    fn documentation_prompt_lifecycle_uses_typed_history_authority() {
        assert!(
            super::documentation_prompt_lifecycle_uses_typed_history_authority_fixture_passes()
        );
    }

    #[test]
    fn follow_up_focus_uses_typed_history_authority() {
        assert!(super::follow_up_focus_uses_typed_history_authority_fixture_passes());
    }

    #[test]
    fn typed_verification_run_prompt_cycle_fixture_passes() {
        assert!(super::prompt_projection_uses_typed_verification_run_cycle_fixture_passes());
    }

    #[test]
    fn typed_rejected_tool_proposal_prompt_fixture_passes() {
        assert!(super::prompt_projection_uses_rejected_tool_proposal_fixture_passes());
    }

    #[test]
    fn typed_pseudo_tool_rejection_prompt_fixture_passes() {
        assert!(super::prompt_projection_uses_typed_pseudo_tool_rejection_fixture_passes());
    }

    #[test]
    fn code_block_stall_uses_typed_history_authority() {
        assert!(super::code_block_stall_uses_typed_history_authority_fixture_passes());
    }

    #[test]
    fn superseded_tool_denial_uses_typed_history_authority() {
        assert!(super::superseded_tool_denial_uses_typed_history_authority_fixture_passes());
    }

    #[test]
    fn typed_docs_audit_metadata_prompt_fixture_passes() {
        assert!(super::prompt_projection_uses_typed_docs_audit_metadata_fixture_passes());
    }

    #[test]
    fn typed_patch_recovery_state_prompt_fixture_passes() {
        assert!(super::prompt_projection_uses_state_patch_recovery_fixture_passes());
    }

    #[test]
    fn failed_inactive_authoring_replay_live_builder_fixture_passes() {
        assert!(super::failed_inactive_authoring_replay_uses_call_scoped_summary());
    }

    #[test]
    fn malformed_edit_argument_replay_live_builder_fixture_passes() {
        assert!(super::provider_replay_preserves_current_invalid_edit_argument_feedback());
    }

    #[test]
    fn rejected_final_message_replay_fixture_passes() {
        assert!(super::provider_replay_projects_rejected_final_message_evidence());
    }

    #[test]
    fn compaction_provider_context_projects_typed_contract_first_fixture_passes() {
        assert!(super::compaction_provider_context_projects_typed_contract_before_summary());
    }

    #[test]
    fn compaction_replay_uses_typed_history_authority() {
        assert!(super::compaction_replay_uses_typed_history_authority_fixture_passes());
    }

    #[test]
    fn provider_replay_includes_active_turn_steer() {
        assert!(super::provider_replay_includes_active_turn_steer_fixture_passes());
    }

    #[test]
    fn prompt_projection_workspace_root_uses_typed_runtime_input() {
        assert!(super::prompt_projection_workspace_root_uses_typed_runtime_input_fixture_passes());
    }

    #[test]
    fn runtime_input_view_has_no_compatibility_transcript_projection() {
        assert!(super::runtime_input_view_no_compatibility_transcript_authority_fixture_passes());
    }

    #[test]
    fn exact_write_repair_omits_consumed_supporting_context_fixture_passes() {
        assert!(super::exact_write_repair_omits_consumed_supporting_context_replay());
    }

    #[test]
    fn target_exclusive_apply_patch_contract_violation_replay_fixture_passes() {
        assert!(
            super::provider_replay_omits_target_exclusive_apply_patch_contract_violation_fixture_passes()
        );
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
