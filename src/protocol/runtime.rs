use serde::{Deserialize, Serialize};

use super::{
    ActionAuthority, ControlEnvelopeValidation, DispatchPolicy, EvidenceRef, ObligationKind,
    ObligationSet, ObligationStatus, ProjectionBundle, TurnContext, TurnControlEnvelope, TurnId,
    TurnObligation,
};
use crate::tool::ToolName;

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
                targets: active.active_targets.clone(),
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
                    targets: decision.active_targets.clone(),
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
                    targets: decision.active_targets.clone(),
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
                targets: active.active_targets.clone(),
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
                targets: continuation.target_files.clone(),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_next_action: Option<String>,
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
