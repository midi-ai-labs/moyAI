use crate::harness::gate::{GateDerivedOutput, GateEvaluation, GateKind, QualityGateResult};
use crate::harness::{HarnessEvent, HarnessEventKind};

pub fn evaluate(events: &[HarnessEvent]) -> GateEvaluation {
    let has_tool_or_model = events.iter().any(|event| {
        matches!(
            event.kind,
            HarnessEventKind::ModelRequestPrepared
                | HarnessEventKind::ModelRequestSent
                | HarnessEventKind::ToolDispatchRequested
                | HarnessEventKind::ToolExecuted
                | HarnessEventKind::ToolDispatchDenied
                | HarnessEventKind::ToolDeclined
                | HarnessEventKind::ToolCancelled
                | HarnessEventKind::ToolFailed
                | HarnessEventKind::PermissionRequested
                | HarnessEventKind::PermissionResolved
        )
    });
    let result = if has_tool_or_model {
        QualityGateResult::pass(
            GateKind::ToolDispatch,
            "tool dispatch evidence is available for replay",
        )
    } else {
        QualityGateResult::blocked(
            GateKind::ToolDispatch,
            crate::harness::FailureOwner::HarnessCapture,
            "tool dispatch evidence is missing",
        )
    };
    GateEvaluation {
        result,
        derived: GateDerivedOutput::default(),
    }
}
