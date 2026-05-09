use crate::harness::gate::{GateDerivedOutput, GateEvaluation, GateKind, QualityGateResult};

pub fn not_applicable() -> GateEvaluation {
    let mut result = QualityGateResult::pass(
        GateKind::E2E,
        "e2e gate is not executed during provider-free replay",
    );
    result.status = crate::harness::GateStatus::NotApplicable;
    GateEvaluation {
        result,
        derived: GateDerivedOutput::default(),
    }
}
