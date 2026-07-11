use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

use crate::error::RuntimeError;
use crate::harness::gate::{self, GateStatus};
use crate::harness::stored_artifact;
use crate::harness::{
    ArtifactManifest, ContractRecord, FailureOwner, HarnessEvent, HarnessRunId, ReplayReport,
    ReplayStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMode {
    StoredArtifact,
    TypedEventLog,
    Hybrid,
}

impl std::str::FromStr for ReplayMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "stored-artifact" | "stored_artifact" => Ok(Self::StoredArtifact),
            "typed-event-log" | "typed_event_log" => Ok(Self::TypedEventLog),
            "hybrid" => Ok(Self::Hybrid),
            _ => Err(format!("unknown replay mode `{value}`")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayProfile {
    #[serde(default)]
    pub gates: Vec<gate::GateKind>,
    pub provider_replay: bool,
    pub shell_reexecution: bool,
    pub contract_override_policy: String,
}

impl Default for ReplayProfile {
    fn default() -> Self {
        Self {
            gates: vec![
                gate::GateKind::Schema,
                gate::GateKind::StateTransition,
                gate::GateKind::ToolDispatch,
                gate::GateKind::Artifact,
                gate::GateKind::Scenario,
                gate::GateKind::E2E,
            ],
            provider_replay: false,
            shell_reexecution: false,
            contract_override_policy: "recorded_versions".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayRunInput {
    pub schema_version: String,
    pub run_id: Option<HarnessRunId>,
    pub mode: ReplayMode,
    pub scenario_id: String,
    pub artifact_root: Utf8PathBuf,
    pub event_log: Option<Utf8PathBuf>,
    pub artifact_manifest: Option<Utf8PathBuf>,
    pub contract_registry: Option<Utf8PathBuf>,
    pub profile: ReplayProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayExecution {
    pub report: ReplayReport,
    pub events: Vec<HarnessEvent>,
    pub artifacts: Vec<ArtifactManifest>,
    pub contracts: Vec<ContractRecord>,
}

pub struct ReplayService;

impl ReplayService {
    pub fn replay(input: ReplayRunInput) -> Result<ReplayReport, RuntimeError> {
        Ok(Self::replay_with_evidence(input)?.report)
    }

    pub fn replay_with_evidence(input: ReplayRunInput) -> Result<ReplayExecution, RuntimeError> {
        validate_replay_input(&input)?;
        match input.mode {
            ReplayMode::StoredArtifact => replay_stored_artifact(input),
            ReplayMode::TypedEventLog => replay_typed_event_log(input),
            ReplayMode::Hybrid => {
                if input.event_log.is_some() {
                    replay_typed_event_log(input)
                } else {
                    replay_stored_artifact(input)
                }
            }
        }
    }
}

fn replay_stored_artifact(input: ReplayRunInput) -> Result<ReplayExecution, RuntimeError> {
    let snapshot =
        stored_artifact::synthesize_from_artifact_root(&input.artifact_root, &input.scenario_id)?;
    let gate_result = crate::harness::QualityGateResult::blocked(
        gate::GateKind::Schema,
        FailureOwner::HarnessCapture,
        "stored-artifact import cannot prove runtime events; use a typed event log for a pass verdict",
    );
    Ok(ReplayExecution {
        report: ReplayReport {
            schema_version: "replay.report.v1".to_string(),
            run_id: snapshot.run_id,
            status: ReplayStatus::Blocked,
            primary_owner: Some(FailureOwner::HarnessCapture),
            summary: "replay is blocked by missing typed runtime evidence".to_string(),
            gate_results: vec![gate_result],
            restart_point: Some("capture_typed_runtime_events".to_string()),
            next_actions: vec![
                "replay a typed event log captured by the native runtime".to_string(),
            ],
        },
        events: snapshot.events,
        artifacts: snapshot.artifacts,
        contracts: snapshot.contracts,
    })
}

fn replay_typed_event_log(input: ReplayRunInput) -> Result<ReplayExecution, RuntimeError> {
    let event_log = input.event_log.as_ref().ok_or_else(|| {
        RuntimeError::Message("typed-event-log replay requires --event-log".to_string())
    })?;
    let events: Vec<HarnessEvent> = read_json_file(event_log)?;
    let event_run_id = events.first().map(|event| event.run_id).ok_or_else(|| {
        RuntimeError::Message("typed-event-log replay requires at least one event".to_string())
    })?;
    let artifacts: Vec<ArtifactManifest> = match input.artifact_manifest.as_ref() {
        Some(path) => read_json_file(path)?,
        None => Vec::new(),
    };
    let contracts: Vec<ContractRecord> = match input.contract_registry.as_ref() {
        Some(path) => read_json_file(path)?,
        None => Vec::new(),
    };
    if input
        .run_id
        .is_some_and(|requested| requested != event_run_id)
    {
        return Err(RuntimeError::Message(format!(
            "requested replay run_id does not match event stream run_id `{event_run_id}`"
        )));
    }
    if artifacts
        .iter()
        .any(|artifact| artifact.run_id != event_run_id)
    {
        return Err(RuntimeError::Message(
            "artifact manifest contains a run_id that does not match the event stream".to_string(),
        ));
    }
    evaluate_replay(
        event_run_id,
        &input.scenario_id,
        &input.artifact_root,
        events,
        artifacts,
        contracts,
        &input.profile,
    )
}

fn evaluate_replay(
    run_id: HarnessRunId,
    scenario_id: &str,
    artifact_root: &camino::Utf8Path,
    events: Vec<HarnessEvent>,
    artifacts: Vec<ArtifactManifest>,
    contracts: Vec<ContractRecord>,
    profile: &ReplayProfile,
) -> Result<ReplayExecution, RuntimeError> {
    let mut gate_results = Vec::new();
    let schema = gate::schema::evaluate(&events, &artifacts, &contracts);
    let schema_status = schema.result.status;
    gate_results.push(schema.result);
    if schema_status == GateStatus::Pass {
        for gate_kind in &profile.gates {
            let result = match gate_kind {
                gate::GateKind::Schema => continue,
                gate::GateKind::StateTransition => gate::state_transition::evaluate(&events).result,
                gate::GateKind::ToolDispatch => gate::tool_dispatch::evaluate(&events).result,
                gate::GateKind::Artifact => {
                    gate::artifact::evaluate(artifact_root, &artifacts).result
                }
                gate::GateKind::Scenario => {
                    gate::scenario::evaluate(scenario_id, &artifacts, &contracts).result
                }
                gate::GateKind::E2E => gate::e2e::not_applicable().result,
            };
            gate_results.push(result);
        }
    }
    let status = if gate_results
        .iter()
        .any(|result| result.status == GateStatus::Fail)
    {
        ReplayStatus::Fail
    } else if gate_results
        .iter()
        .any(|result| result.status == GateStatus::Blocked)
    {
        ReplayStatus::Blocked
    } else {
        ReplayStatus::Pass
    };
    let primary_owner = gate_results.iter().find_map(|result| result.owner);
    let mut next_actions = Vec::new();
    if matches!(primary_owner, Some(FailureOwner::ScenarioContract)) {
        next_actions.push("align scenario contract, prompt wording, and gate fixture".to_string());
    }
    let summary = match status {
        ReplayStatus::Pass => "replay gates passed".to_string(),
        ReplayStatus::Fail => "replay gates found a contract failure".to_string(),
        ReplayStatus::Blocked => "replay gates are blocked by missing evidence".to_string(),
    };
    let restart_point = match status {
        ReplayStatus::Pass => None,
        ReplayStatus::Fail | ReplayStatus::Blocked => {
            Some("register_failure_then_restart_representative_sweep".to_string())
        }
    };
    let report = ReplayReport {
        schema_version: "replay.report.v1".to_string(),
        run_id,
        status,
        primary_owner,
        summary,
        gate_results,
        restart_point,
        next_actions,
    };
    Ok(ReplayExecution {
        report,
        events,
        artifacts,
        contracts,
    })
}

fn validate_replay_input(input: &ReplayRunInput) -> Result<(), RuntimeError> {
    if input.schema_version != "replay.run_input.v1" {
        return Err(RuntimeError::Message(format!(
            "unsupported replay input schema `{}`",
            input.schema_version
        )));
    }
    if input.profile.gates.is_empty() {
        return Err(RuntimeError::Message(
            "replay profile must select at least one gate".to_string(),
        ));
    }
    if input.profile.provider_replay {
        return Err(RuntimeError::Message(
            "provider replay is not implemented; disable provider_replay".to_string(),
        ));
    }
    if input.profile.shell_reexecution {
        return Err(RuntimeError::Message(
            "shell reexecution is not implemented; disable shell_reexecution".to_string(),
        ));
    }
    if input.profile.contract_override_policy != "recorded_versions" {
        return Err(RuntimeError::Message(format!(
            "unsupported contract override policy `{}`",
            input.profile.contract_override_policy
        )));
    }
    Ok(())
}

fn read_json_file<T>(path: &camino::Utf8Path) -> Result<T, RuntimeError>
where
    T: serde::de::DeserializeOwned,
{
    let text = std::fs::read_to_string(path.as_std_path())
        .map_err(|error| RuntimeError::Message(format!("failed to read {path}: {error}")))?;
    serde_json::from_str(&text)
        .map_err(|error| RuntimeError::Message(format!("failed to parse {path}: {error}")))
}

pub fn write_report(report: &ReplayReport, output: &camino::Utf8Path) -> Result<(), RuntimeError> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent.as_std_path())
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
    }
    let json = serde_json::to_string_pretty(report)
        .map_err(|error| RuntimeError::Message(error.to_string()))?;
    std::fs::write(output.as_std_path(), json)
        .map_err(|error| RuntimeError::Message(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored_input(root: Utf8PathBuf) -> ReplayRunInput {
        ReplayRunInput {
            schema_version: "replay.run_input.v1".to_string(),
            run_id: None,
            mode: ReplayMode::StoredArtifact,
            scenario_id: "case1".to_string(),
            artifact_root: root,
            event_log: None,
            artifact_manifest: None,
            contract_registry: None,
            profile: ReplayProfile::default(),
        }
    }

    #[test]
    fn arbitrary_stored_files_cannot_pass_replay() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("junk.txt"), "not evidence").expect("junk");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");

        let execution = ReplayService::replay_with_evidence(stored_input(root)).expect("replay");

        assert_eq!(execution.report.status, ReplayStatus::Blocked);
        assert!(
            execution
                .report
                .gate_results
                .iter()
                .any(|result| result.status == GateStatus::Blocked)
        );
    }

    #[test]
    fn stored_artifacts_cannot_pass_with_a_reduced_gate_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("junk.txt"), "not evidence").expect("junk");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let mut input = stored_input(root);
        input.profile.gates = vec![gate::GateKind::Schema];

        let execution = ReplayService::replay_with_evidence(input).expect("replay");

        assert_eq!(execution.report.status, ReplayStatus::Blocked);
        assert!(execution.report.gate_results.iter().any(|result| {
            result.status == GateStatus::Blocked
                && result.summary.contains("cannot prove runtime events")
        }));
    }

    #[test]
    fn unsupported_replay_profile_capabilities_fail_closed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 root");
        let mut input = stored_input(root);
        input.profile.provider_replay = true;

        let error = ReplayService::replay_with_evidence(input).expect_err("unsupported profile");

        assert!(
            error
                .to_string()
                .contains("provider replay is not implemented")
        );
    }
}
