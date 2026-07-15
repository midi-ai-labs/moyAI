use crate::harness::gate::{GateDerivedOutput, GateEvaluation, GateKind, QualityGateResult};
use crate::harness::{HarnessEvent, HarnessEventKind};

pub fn evaluate(events: &[HarnessEvent]) -> GateEvaluation {
    let has_state = events.iter().any(|event| {
        matches!(
            event.kind,
            HarnessEventKind::StateSnapshotRecorded | HarnessEventKind::StateTransitionRecorded
        )
    });
    let result = if has_state {
        QualityGateResult::pass(
            GateKind::StateTransition,
            "state transition evidence exists for replay",
        )
    } else {
        QualityGateResult::blocked(
            GateKind::StateTransition,
            crate::harness::FailureOwner::HarnessCapture,
            "state transition evidence is missing",
        )
    };
    GateEvaluation {
        result,
        derived: GateDerivedOutput::default(),
    }
}
