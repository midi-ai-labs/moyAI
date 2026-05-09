pub mod artifact;
pub mod e2e;
pub mod scenario;
pub mod schema;
pub mod state_transition;
pub mod tool_dispatch;

use serde::{Deserialize, Serialize};

use crate::harness::{ArtifactId, ContractRef, GateId, HarnessEventId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    Schema,
    StateTransition,
    ToolDispatch,
    Artifact,
    Scenario,
    E2E,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateStatus {
    Pass,
    Fail,
    Blocked,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateSeverity {
    Info,
    Warning,
    Error,
    Fatal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureOwner {
    Flow,
    RuntimeContract,
    ScenarioContract,
    HarnessCapture,
    GateFixture,
    Provider,
    GeneratedArtifact,
    Operator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateNextAction {
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualityGateResult {
    pub gate_id: GateId,
    pub gate_kind: GateKind,
    pub status: GateStatus,
    pub severity: GateSeverity,
    pub owner: Option<FailureOwner>,
    pub summary: String,
    #[serde(default)]
    pub evidence_refs: Vec<ArtifactId>,
    #[serde(default)]
    pub event_refs: Vec<HarnessEventId>,
    #[serde(default)]
    pub contract_refs: Vec<ContractRef>,
    #[serde(default)]
    pub next_actions: Vec<GateNextAction>,
}

impl QualityGateResult {
    pub fn pass(gate_kind: GateKind, summary: impl Into<String>) -> Self {
        Self {
            gate_id: GateId::new(),
            gate_kind,
            status: GateStatus::Pass,
            severity: GateSeverity::Info,
            owner: None,
            summary: summary.into(),
            evidence_refs: Vec::new(),
            event_refs: Vec::new(),
            contract_refs: Vec::new(),
            next_actions: Vec::new(),
        }
    }

    pub fn blocked(gate_kind: GateKind, owner: FailureOwner, summary: impl Into<String>) -> Self {
        Self {
            gate_id: GateId::new(),
            gate_kind,
            status: GateStatus::Blocked,
            severity: GateSeverity::Error,
            owner: Some(owner),
            summary: summary.into(),
            evidence_refs: Vec::new(),
            event_refs: Vec::new(),
            contract_refs: Vec::new(),
            next_actions: Vec::new(),
        }
    }

    pub fn fail(gate_kind: GateKind, owner: FailureOwner, summary: impl Into<String>) -> Self {
        Self {
            gate_id: GateId::new(),
            gate_kind,
            status: GateStatus::Fail,
            severity: GateSeverity::Error,
            owner: Some(owner),
            summary: summary.into(),
            evidence_refs: Vec::new(),
            event_refs: Vec::new(),
            contract_refs: Vec::new(),
            next_actions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateDerivedOutput {
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateEvaluation {
    pub result: QualityGateResult,
    pub derived: GateDerivedOutput,
}
