use camino::{Utf8Path, Utf8PathBuf};
use serde_json::json;

use crate::error::RuntimeError;
use crate::harness::artifact::ArtifactManifest;
use crate::harness::contract::ContractRecord;
use crate::harness::event::{HarnessEvent, HarnessEventKind, HarnessEventPayload};
use crate::harness::{HarnessEventId, HarnessRunId};
use crate::runtime::SystemClock;

#[derive(Debug, Clone)]
pub struct StoredArtifactReplaySnapshot {
    pub run_id: HarnessRunId,
    pub artifact_root: Utf8PathBuf,
    pub events: Vec<HarnessEvent>,
    pub artifacts: Vec<ArtifactManifest>,
    pub contracts: Vec<ContractRecord>,
}

pub fn synthesize_from_artifact_root(
    artifact_root: &Utf8Path,
    scenario_id: &str,
) -> Result<StoredArtifactReplaySnapshot, RuntimeError> {
    if !artifact_root.is_dir() {
        return Err(RuntimeError::Message(format!(
            "artifact root is not a directory: {artifact_root}"
        )));
    }
    let run_id = HarnessRunId::new();
    let now = SystemClock::now_ms();
    let mut events = vec![HarnessEvent {
        id: HarnessEventId::new(),
        run_id,
        sequence_no: 0,
        created_at_ms: now,
        kind: HarnessEventKind::RunStarted,
        payload: HarnessEventPayload::RunStarted {
            workspace_root: artifact_root.as_str().to_string(),
            artifact_root: artifact_root.as_str().to_string(),
            mode: "stored_artifact".to_string(),
        },
        contract_refs: Vec::new(),
        artifact_refs: Vec::new(),
        parent_event_id: None,
    }];
    let artifacts = Vec::new();
    events.push(generic_event(
        run_id,
        1,
        HarnessEventKind::RunTerminalized,
        json!({
            "stored_artifact_import": true,
            "status": "blocked",
            "scenario_id": scenario_id,
            "artifact_count": artifacts.len(),
            "reason": "raw files do not prove state transitions, tool execution, verification, or a scenario contract"
        }),
    ));
    let contracts = Vec::new();
    Ok(StoredArtifactReplaySnapshot {
        run_id,
        artifact_root: artifact_root.to_path_buf(),
        events,
        artifacts,
        contracts,
    })
}

fn generic_event(
    run_id: HarnessRunId,
    sequence_no: i64,
    kind: HarnessEventKind,
    payload: serde_json::Value,
) -> HarnessEvent {
    HarnessEvent {
        id: HarnessEventId::new(),
        run_id,
        sequence_no,
        created_at_ms: SystemClock::now_ms(),
        kind,
        payload: HarnessEventPayload::generic(payload),
        contract_refs: Vec::new(),
        artifact_refs: Vec::new(),
        parent_event_id: None,
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8Path;

    use crate::harness::HarnessEventKind;

    use super::synthesize_from_artifact_root;

    #[test]
    fn arbitrary_files_do_not_synthesize_success_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("junk.txt"), "not replay evidence").expect("junk");
        let root = Utf8Path::from_path(temp.path()).expect("utf8 root");

        let snapshot = synthesize_from_artifact_root(root, "case1").expect("snapshot");

        assert!(snapshot.contracts.is_empty());
        assert!(snapshot.artifacts.is_empty());
        assert!(!snapshot.events.iter().any(|event| matches!(
            event.kind,
            HarnessEventKind::StateSnapshotRecorded | HarnessEventKind::ToolExecuted
        )));
        assert!(
            snapshot
                .events
                .iter()
                .any(|event| event.kind == HarnessEventKind::RunTerminalized)
        );
    }
}
