use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::harness::{ArtifactId, ContractRef, HarnessEventId, HarnessRunId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessEventKind {
    RunStarted,
    ProviderPreflightChecked,
    UserTurnAccepted,
    AttachmentRegistered,
    ContractVersionSelected,
    StateSnapshotRecorded,
    ActiveWorkContractSelected,
    ControlEnvelopePrepared,
    ModelProjectionBuilt,
    ModelRequestSent,
    ModelResponseReceived,
    ModelNoToolStop,
    ToolDispatchRequested,
    ToolDispatchDenied,
    PermissionRequested,
    PermissionResolved,
    ToolExecuted,
    ToolResultNormalized,
    CorrectiveResultEmitted,
    StateTransitionRecorded,
    ArtifactRegistered,
    QualityGateEvaluated,
    ScenarioGateEvaluated,
    RunTerminalized,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum HarnessEventPayload {
    RunStarted {
        workspace_root: String,
        artifact_root: String,
        mode: String,
    },
    Generic(Value),
}

impl HarnessEventPayload {
    pub fn generic(value: impl Into<Value>) -> Self {
        Self::Generic(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessEvent {
    pub id: HarnessEventId,
    pub run_id: HarnessRunId,
    pub sequence_no: i64,
    pub created_at_ms: i64,
    pub kind: HarnessEventKind,
    pub payload: HarnessEventPayload,
    #[serde(default)]
    pub contract_refs: Vec<ContractRef>,
    #[serde(default)]
    pub artifact_refs: Vec<ArtifactId>,
    pub parent_event_id: Option<HarnessEventId>,
}
