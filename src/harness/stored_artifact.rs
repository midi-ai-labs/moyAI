use std::collections::BTreeSet;

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::json;

use crate::error::RuntimeError;
use crate::harness::artifact::{ArtifactKind, ArtifactManifest, ArtifactTag, hash_file};
use crate::harness::contract::{ContractKind, ContractRecord};
use crate::harness::event::{HarnessEvent, HarnessEventKind, HarnessEventPayload};
use crate::harness::{ArtifactId, ContractId, HarnessEventId, HarnessRunId};
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
    if !artifact_root.exists() {
        return Err(RuntimeError::Message(format!(
            "artifact root does not exist: {artifact_root}"
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
    let mut artifacts = Vec::new();
    collect_artifacts(run_id, artifact_root, artifact_root, &mut artifacts)?;
    if artifacts.is_empty() {
        events.push(generic_event(
            run_id,
            1,
            HarnessEventKind::RunTerminalized,
            json!({"stored_artifact_synthesized": true, "status": "blocked", "reason": "no artifacts found"}),
        ));
    } else {
        events.push(generic_event(
            run_id,
            1,
            HarnessEventKind::StateSnapshotRecorded,
            json!({"stored_artifact_synthesized": true, "scenario_id": scenario_id}),
        ));
        events.push(generic_event(
            run_id,
            2,
            HarnessEventKind::ToolExecuted,
            json!({"stored_artifact_synthesized": true, "artifact_count": artifacts.len()}),
        ));
        events.push(generic_event(
            run_id,
            3,
            HarnessEventKind::RunTerminalized,
            json!({"stored_artifact_synthesized": true, "status": "recorded"}),
        ));
    }
    let contracts = vec![ContractRecord {
        id: ContractId::new(format!("scenario.{scenario_id}")),
        kind: ContractKind::Scenario,
        version: "stored-artifact-synthesized".to_string(),
        source_path: Utf8PathBuf::from(format!("moyai/tests/manual_ST/{scenario_id}.md")),
        content_sha256: "stored-artifact-synthesized".to_string(),
        schema_ref: None,
        model_visible_summary: Some(format!(
            "Stored artifact synthesized scenario contract for {scenario_id}"
        )),
    }];
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

fn collect_artifacts(
    run_id: HarnessRunId,
    root: &Utf8Path,
    current: &Utf8Path,
    output: &mut Vec<ArtifactManifest>,
) -> Result<(), RuntimeError> {
    for entry in std::fs::read_dir(current.as_std_path())
        .map_err(|error| RuntimeError::Message(error.to_string()))?
    {
        let entry = entry.map_err(|error| RuntimeError::Message(error.to_string()))?;
        let path = Utf8PathBuf::from_path_buf(entry.path()).map_err(|_| {
            RuntimeError::Message("stored artifact path is not valid UTF-8".to_string())
        })?;
        let file_type = entry
            .file_type()
            .map_err(|error| RuntimeError::Message(error.to_string()))?;
        if file_type.is_dir() {
            collect_artifacts(run_id, root, &path, output)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let relative = path.strip_prefix(root).map_err(|error| {
            RuntimeError::Message(format!("failed to compute artifact relative path: {error}"))
        })?;
        let (sha256, size_bytes) =
            hash_file(&path).map_err(|error| RuntimeError::Message(error.to_string()))?;
        let mut tags = BTreeSet::new();
        let kind = classify_artifact(relative.as_str(), &mut tags);
        output.push(ArtifactManifest {
            id: ArtifactId::new(),
            run_id,
            kind,
            relative_path: relative.to_path_buf(),
            sha256,
            size_bytes,
            tags,
            created_by_event: None,
            contract_refs: Vec::new(),
        });
    }
    Ok(())
}

fn classify_artifact(path: &str, tags: &mut BTreeSet<ArtifactTag>) -> ArtifactKind {
    if path.ends_with("result.json") {
        tags.insert(ArtifactTag::Replay);
        ArtifactKind::ReplayReport
    } else if path.contains("request") || path.contains("diagnostic") {
        tags.insert(ArtifactTag::Diagnostics);
        ArtifactKind::RequestDiagnostics
    } else if path.contains("transcript") {
        ArtifactKind::Transcript
    } else if path.ends_with(".png") || path.ends_with(".jpg") || path.ends_with(".jpeg") {
        tags.insert(ArtifactTag::ImageTransport);
        ArtifactKind::ImageAttachment
    } else if path.ends_with(".log") || path.contains("unittest") || path.contains("py_compile") {
        tags.insert(ArtifactTag::Verification);
        ArtifactKind::VerificationLog
    } else {
        tags.insert(ArtifactTag::ScenarioOutput);
        ArtifactKind::WorkspaceFile
    }
}
