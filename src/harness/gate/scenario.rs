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
    identifier_has_scenario_segment(&contract_id, scenario_id)
        || path_has_scenario_segment(contract.source_path.as_str(), scenario_id)
}

fn identifier_has_scenario_segment(value: &str, scenario_id: &str) -> bool {
    if scenario_id.is_empty() {
        return false;
    }
    let mut offset = 0;
    while let Some(relative_start) = value[offset..].find(scenario_id) {
        let start = offset + relative_start;
        let end = start + scenario_id.len();
        let left_boundary = value[..start]
            .chars()
            .next_back()
            .is_none_or(is_contract_boundary);
        let right_boundary = value[end..].chars().next().is_none_or(is_contract_boundary);
        if left_boundary && right_boundary {
            return true;
        }
        offset = end;
    }
    false
}

fn path_has_scenario_segment(path: &str, scenario_id: &str) -> bool {
    path.split(['/', '\\']).any(|segment| {
        segment == scenario_id
            || segment
                .strip_suffix(".md")
                .or_else(|| segment.strip_suffix(".json"))
                .or_else(|| segment.strip_suffix(".toml"))
                .is_some_and(|stem| stem == scenario_id)
    })
}

fn is_contract_boundary(character: char) -> bool {
    !character.is_ascii_alphanumeric() && character != '_'
}

#[cfg(test)]
mod tests {
    use super::identifier_has_scenario_segment;

    #[test]
    fn scenario_contract_matching_requires_identifier_boundaries() {
        assert!(identifier_has_scenario_segment(
            "manual_st.required_core.case1",
            "case1"
        ));
        assert!(identifier_has_scenario_segment(
            "manual-st-required-core-case1",
            "case1"
        ));
        assert!(!identifier_has_scenario_segment(
            "manual_st.required_core.case10",
            "case1"
        ));
        assert!(!identifier_has_scenario_segment(
            "manual_st.required_core.case1_extra",
            "case1"
        ));
    }
}
