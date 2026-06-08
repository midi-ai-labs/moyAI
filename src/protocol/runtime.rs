use serde::{Deserialize, Serialize};

use super::{
    ActionAuthority, ControlEnvelopeValidation, DispatchPolicy, EvidenceRef, ObligationKind,
    ObligationSet, ObligationStatus, ProjectionBundle, TurnContext, TurnControlEnvelope, TurnId,
    TurnObligation, canonicalize_workspace_targets,
};
use crate::tool::ToolName;

const CURRENT_PROVIDER_MODEL: &str = "qwen/qwen3.6-35b-a3b";
const CURRENT_PROVIDER_BASE_URL: &str = "http://127.0.0.1:1234";
const CURRENT_PROVIDER_CONTEXT_WINDOW: u32 = 131_072;
const CURRENT_PROVIDER_MAX_OUTPUT_TOKENS: u32 = 8_192;
const PROTOCOL_RUNTIME_PROVIDER_PROFILE_MARKER: &str =
    "protocol_runtime_fixture_current_provider_profile";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnEngineInput {
    pub turn_id: TurnId,
    pub context: TurnContext,
    pub obligations: ObligationSet,
    pub dispatch_policy: DispatchPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledTurn {
    pub envelope: TurnControlEnvelope,
    pub validation: ControlEnvelopeValidation,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TurnEngine;

impl TurnEngine {
    pub fn compile(input: TurnEngineInput) -> CompiledTurn {
        let action_authority = ActionAuthority::from_obligations(
            &input.context,
            &input.obligations,
            input.context.tool_choice.clone(),
        );
        let projection_bundle =
            ProjectionBundle::from_authority_and_obligations(&action_authority, &input.obligations);
        let envelope = TurnControlEnvelope::new(
            input.turn_id,
            input.context,
            input.obligations,
            action_authority,
            projection_bundle,
            input.dispatch_policy,
            input.evidence_refs,
        );
        let validation = envelope.validate();
        CompiledTurn {
            envelope,
            validation,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ObligationCompiler;

impl ObligationCompiler {
    pub fn compile(context: &TurnContext) -> ObligationSet {
        let mut items = Vec::new();
        let active = &context.active_contract;

        if !active.active_targets.is_empty() || active.active_work_kind.is_some() {
            items.push(TurnObligation {
                obligation_id: "active_work".to_string(),
                kind: ObligationKind::UserWork,
                summary: active.summary.clone(),
                targets: canonicalize_workspace_targets(
                    &active.active_targets,
                    &context.workspace_root,
                ),
                operation_intents: active.operation_intents.clone(),
                required_actions: Vec::new(),
                verification_commands: Vec::new(),
                contract_refs: Vec::new(),
                evidence_refs: Vec::new(),
                status: ObligationStatus::Open,
            });
        }

        if let Some(decision) = &context.turn_decision_projection {
            if !decision.policy_targets.is_empty() || !decision.allowed_tools.is_empty() {
                items.push(TurnObligation {
                    obligation_id: "control_projection".to_string(),
                    kind: ObligationKind::Contract,
                    summary: "Turn control projection must stay aligned across prompt, tool feedback, request diagnostics, handoff, and preflight surfaces.".to_string(),
                    targets: canonicalize_workspace_targets(
                        &decision.active_targets,
                        &context.workspace_root,
                    ),
                    operation_intents: active.operation_intents.clone(),
                    required_actions: Vec::new(),
                    verification_commands: decision.required_verification_commands.clone(),
                    contract_refs: Vec::new(),
                    evidence_refs: Vec::new(),
                    status: ObligationStatus::Open,
                });
            }

            if decision.closeout_ready {
                items.push(TurnObligation {
                    obligation_id: "closeout".to_string(),
                    kind: ObligationKind::Closeout,
                    summary: "Closeout may proceed only after open work and verification obligations are satisfied by item-stream evidence.".to_string(),
                    targets: canonicalize_workspace_targets(
                        &decision.active_targets,
                        &context.workspace_root,
                    ),
                    operation_intents: Vec::new(),
                    required_actions: Vec::new(),
                    verification_commands: Vec::new(),
                    contract_refs: Vec::new(),
                    evidence_refs: Vec::new(),
                    status: ObligationStatus::Open,
                });
            }
        }

        if !active.required_verification_commands.is_empty() {
            items.push(TurnObligation {
                obligation_id: "verification".to_string(),
                kind: ObligationKind::Verification,
                summary: "Required verification commands must be executed or preserved as a typed continuation before completion.".to_string(),
                targets: canonicalize_workspace_targets(
                    &active.active_targets,
                    &context.workspace_root,
                ),
                operation_intents: Vec::new(),
                required_actions: Vec::new(),
                verification_commands: active.required_verification_commands.clone(),
                contract_refs: Vec::new(),
                evidence_refs: Vec::new(),
                status: ObligationStatus::Open,
            });
        }

        if let Some(continuation) = &context.continuation {
            items.push(TurnObligation {
                obligation_id: "continuation".to_string(),
                kind: ObligationKind::Continuation,
                summary: continuation
                    .active_work_summary
                    .clone()
                    .or_else(|| continuation.completion_blocker.clone())
                    .unwrap_or_else(|| {
                        "Typed continuation contract must survive handoff and compaction."
                            .to_string()
                    }),
                targets: canonicalize_workspace_targets(
                    &continuation.target_files,
                    &context.workspace_root,
                ),
                operation_intents: Vec::new(),
                required_actions: Vec::new(),
                verification_commands: continuation.verification_commands.clone(),
                contract_refs: continuation.invariant_refs.clone(),
                evidence_refs: Vec::new(),
                status: ObligationStatus::Open,
            });
        }

        ObligationSet::new(items)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkOrder {
    pub order_id: String,
    pub state: WorkOrderState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obligations: Vec<TurnObligation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<ToolName>,
}

pub fn repair_target_identity_aliases_compile_exact_write_action_fixture_passes() -> bool {
    let projection_id = super::ProjectionId::new();
    let workspace_root = camino::Utf8PathBuf::from("C:/workspace/project");
    let relative_target = camino::Utf8PathBuf::from("tests/workflow.behavior.md");
    let absolute_target =
        camino::Utf8PathBuf::from("C:/workspace/project/tests/workflow.behavior.md");
    let active_contract = super::ActiveWorkContractProjection {
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Repair,
        active_work_kind: Some("verification".to_string()),
        summary: "Repair workflow behavior artifact before rerunning verification.".to_string(),
        active_targets: vec![relative_target.clone(), absolute_target.clone()],
        operation_intents: vec![super::OperationIntent::ContentChangingAuthoringRequired],
        required_verification_commands: vec![
            "verify-contract --behavior --encoding utf-8".to_string(),
        ],
        allowed_tools: vec![
            crate::tool::ToolName::ApplyPatch,
            crate::tool::ToolName::Write,
        ],
        forbidden_tools: Vec::new(),
        projection_id,
    };
    let turn_decision_projection = crate::session::TurnDecisionDiagnostic {
        route: "code".to_string(),
        process_phase: "repair".to_string(),
        active_work_kind: Some("verification".to_string()),
        active_work_summary: Some(
            "Repair workflow behavior artifact before rerunning verification.".to_string(),
        ),
        active_targets: vec![relative_target.clone(), absolute_target],
        verification_pending: true,
        closeout_ready: false,
        required_verification_commands: vec![
            "verify-contract --behavior --encoding utf-8".to_string(),
        ],
        policy_targets: Vec::new(),
        allowed_tools: vec!["apply_patch".to_string(), "write".to_string()],
        tool_choice: Some("named".to_string()),
        warnings: Vec::new(),
        repair_lane: None,
    };
    let context = TurnContext {
        session_id: crate::session::SessionId::new(),
        cwd: workspace_root.clone(),
        workspace_root: workspace_root.clone(),
        provider: "lm_studio".to_string(),
        model: CURRENT_PROVIDER_MODEL.to_string(),
        base_url: CURRENT_PROVIDER_BASE_URL.to_string(),
        access_mode: crate::config::AccessMode::AutoReview,
        sandbox: super::SandboxProfile::WorkspaceWrite,
        shell_family: crate::config::ShellFamily::PowerShell,
        model_capabilities: super::ModelCapabilities {
            supports_tools: true,
            supports_reasoning: false,
            supports_images: false,
            parallel_tool_calls: false,
            context_window: CURRENT_PROVIDER_CONTEXT_WINDOW,
            max_output_tokens: CURRENT_PROVIDER_MAX_OUTPUT_TOKENS,
        },
        route: crate::session::TaskRoute::Code,
        process_phase: crate::session::ProcessPhase::Repair,
        active_contract,
        allowed_tools: vec![
            crate::tool::ToolName::ApplyPatch,
            crate::tool::ToolName::Write,
        ],
        tool_choice: super::ToolChoice::Named(crate::tool::ToolName::Write),
        images: Vec::new(),
        output_contract: super::OutputContract {
            final_answer_required: false,
            structured_schema_name: None,
            history_markdown_projection: true,
        },
        continuation: None,
        turn_decision_projection: Some(turn_decision_projection),
    };
    let provider_profile_is_current = context.model == CURRENT_PROVIDER_MODEL
        && context.base_url == CURRENT_PROVIDER_BASE_URL
        && context.model_capabilities.context_window == CURRENT_PROVIDER_CONTEXT_WINDOW
        && context.model_capabilities.max_output_tokens == CURRENT_PROVIDER_MAX_OUTPUT_TOKENS
        && PROTOCOL_RUNTIME_PROVIDER_PROFILE_MARKER
            == "protocol_runtime_fixture_current_provider_profile";
    let obligations = ObligationCompiler::compile(&context);
    let compiled = TurnEngine::compile(TurnEngineInput {
        turn_id: super::TurnId::new(),
        context,
        obligations,
        dispatch_policy: DispatchPolicy::Dispatch,
        evidence_refs: Vec::new(),
    });
    let all_targets_are_canonical = compiled
        .envelope
        .obligations
        .items
        .iter()
        .all(|item| item.targets.is_empty() || item.targets == vec![relative_target.clone()]);

    compiled.validation.passes()
        && provider_profile_is_current
        && all_targets_are_canonical
        && compiled
            .envelope
            .action_authority
            .required_action
            .as_ref()
            .is_some_and(|action| {
                action.kind == super::RequiredActionKind::EditTarget
                    && action.tool == crate::tool::ToolName::Write
                    && action.target.as_deref()
                        == Some(camino::Utf8Path::new("tests/workflow.behavior.md"))
                    && action.command.is_none()
                    && action.projection_label() == "write:tests/workflow.behavior.md"
            })
        && compiled
            .envelope
            .projection_bundle
            .prompt
            .required_action
            .as_ref()
            .is_some_and(|action| action.tool == crate::tool::ToolName::Write)
        && compiled
            .envelope
            .projection_bundle
            .request_diagnostics
            .render_control_projection()
            .text
            .contains("Required action: write:tests/workflow.behavior.md")
}

impl WorkOrder {
    pub fn is_dispatchable(&self) -> bool {
        matches!(
            self.state,
            WorkOrderState::NeedGrounding
                | WorkOrderState::NeedEdit
                | WorkOrderState::NeedVerification
                | WorkOrderState::NeedCloseout
        ) && !self.allowed_tools.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkOrderState {
    NeedGrounding,
    NeedEdit,
    NeedVerification,
    NeedCloseout,
    AwaitingUser,
    Failed,
    Completed,
}

#[cfg(test)]
mod tests {
    #[test]
    fn repair_target_identity_aliases_compile_exact_write_action() {
        assert!(super::repair_target_identity_aliases_compile_exact_write_action_fixture_passes());
    }
}
