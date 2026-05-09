use crate::harness::gate::{
    FailureOwner, GateDerivedOutput, GateEvaluation, GateKind, QualityGateResult,
};
use crate::harness::{ArtifactManifest, ArtifactTag, ContractKind, ContractRecord};

pub fn evaluate(
    scenario_id: &str,
    artifacts: &[ArtifactManifest],
    contracts: &[ContractRecord],
) -> GateEvaluation {
    let scenario_contracts = contracts
        .iter()
        .filter(|contract| {
            contract.kind == ContractKind::Scenario
                && contract_matches_scenario(contract, scenario_id)
        })
        .collect::<Vec<_>>();
    if scenario_contracts.is_empty() {
        return GateEvaluation {
            result: QualityGateResult::blocked(
                GateKind::Scenario,
                FailureOwner::ScenarioContract,
                format!("scenario contract is not registered for scenario `{scenario_id}`"),
            ),
            derived: GateDerivedOutput::default(),
        };
    }
    if scenario_contracts.iter().any(|contract| {
        contract
            .model_visible_summary
            .as_deref()
            .is_none_or(|summary| summary.trim().is_empty())
    }) {
        return GateEvaluation {
            result: QualityGateResult::fail(
                GateKind::Scenario,
                FailureOwner::ScenarioContract,
                format!("scenario `{scenario_id}` has a contract without model-visible summary"),
            ),
            derived: GateDerivedOutput::default(),
        };
    }
    let has_scenario_output = artifacts
        .iter()
        .any(|artifact| artifact.tags.contains(&ArtifactTag::ScenarioOutput));
    if !has_scenario_output {
        return GateEvaluation {
            result: QualityGateResult::blocked(
                GateKind::Scenario,
                FailureOwner::HarnessCapture,
                format!("scenario output evidence is missing for `{scenario_id}`"),
            ),
            derived: GateDerivedOutput::default(),
        };
    }
    let mut result = QualityGateResult::pass(
        GateKind::Scenario,
        format!("scenario `{scenario_id}` has visible contract and output evidence"),
    );
    result.contract_refs = scenario_contracts
        .iter()
        .map(|contract| contract.as_ref())
        .collect();
    GateEvaluation {
        result,
        derived: GateDerivedOutput::default(),
    }
}

fn contract_matches_scenario(contract: &ContractRecord, scenario_id: &str) -> bool {
    let contract_id = contract.id.to_string();
    contract_id == scenario_id
        || contract_id.contains(scenario_id)
        || contract_id.ends_with(&format!(".{scenario_id}"))
        || contract.source_path.as_str().contains(scenario_id)
}
