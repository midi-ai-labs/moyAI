use crate::harness::gate::{
    FailureOwner, GateDerivedOutput, GateEvaluation, GateKind, QualityGateResult,
};
use crate::harness::{ArtifactManifest, ContractRecord, HarnessEvent};

pub fn evaluate(
    events: &[HarnessEvent],
    artifacts: &[ArtifactManifest],
    contracts: &[ContractRecord],
) -> GateEvaluation {
    let result = if events.iter().any(|event| event.sequence_no < 0) {
        QualityGateResult::fail(
            GateKind::Schema,
            FailureOwner::HarnessCapture,
            "event stream contains negative sequence numbers",
        )
    } else if artifacts
        .iter()
        .any(|artifact| artifact.sha256.trim().is_empty())
    {
        QualityGateResult::blocked(
            GateKind::Schema,
            FailureOwner::HarnessCapture,
            "artifact manifest contains missing hash",
        )
    } else if contracts
        .iter()
        .any(|contract| contract.content_sha256.trim().is_empty())
    {
        QualityGateResult::blocked(
            GateKind::Schema,
            FailureOwner::HarnessCapture,
            "contract snapshot contains missing content hash",
        )
    } else {
        QualityGateResult::pass(GateKind::Schema, "typed replay inputs satisfy schema gate")
    };
    GateEvaluation {
        result,
        derived: GateDerivedOutput::default(),
    }
}
