use std::collections::BTreeSet;

use crate::harness::gate::{
    FailureOwner, GateDerivedOutput, GateEvaluation, GateKind, QualityGateResult,
};
use crate::harness::{ArtifactManifest, ContractRecord, HarnessEvent};

pub fn evaluate(
    events: &[HarnessEvent],
    artifacts: &[ArtifactManifest],
    contracts: &[ContractRecord],
) -> GateEvaluation {
    let result = if let Some(result) = event_stream_identity_violation(events) {
        result
    } else if events.iter().any(|event| event.sequence_no < 0) {
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

fn event_stream_identity_violation(events: &[HarnessEvent]) -> Option<QualityGateResult> {
    if events.is_empty() {
        return None;
    }

    let expected_run_id = events[0].run_id;
    if events.iter().any(|event| event.run_id != expected_run_id) {
        return Some(QualityGateResult::fail(
            GateKind::Schema,
            FailureOwner::HarnessCapture,
            "event stream contains mixed run_id values",
        ));
    }

    let mut event_ids = BTreeSet::new();
    let mut sequence_numbers = BTreeSet::new();
    let mut previous_sequence = None;
    for event in events {
        if !event_ids.insert(event.id) {
            return Some(QualityGateResult::fail(
                GateKind::Schema,
                FailureOwner::HarnessCapture,
                "event stream contains duplicate event ids",
            ));
        }
        if !sequence_numbers.insert(event.sequence_no) {
            return Some(QualityGateResult::fail(
                GateKind::Schema,
                FailureOwner::HarnessCapture,
                "event stream contains duplicate sequence numbers",
            ));
        }
        if let Some(previous) = previous_sequence
            && event.sequence_no <= previous
        {
            return Some(QualityGateResult::fail(
                GateKind::Schema,
                FailureOwner::HarnessCapture,
                "event stream sequence numbers are not strictly increasing",
            ));
        }
        previous_sequence = Some(event.sequence_no);
    }

    None
}

pub fn event_stream_identity_coherence_fixture_passes() -> bool {
    use crate::harness::{HarnessEventId, HarnessEventKind, HarnessEventPayload, HarnessRunId};

    fn event(run_id: HarnessRunId, id: HarnessEventId, sequence_no: i64) -> HarnessEvent {
        HarnessEvent {
            id,
            run_id,
            sequence_no,
            created_at_ms: sequence_no,
            kind: HarnessEventKind::StateSnapshotRecorded,
            payload: HarnessEventPayload::generic(serde_json::json!({
                "fixture": "event_stream_identity_coherence"
            })),
            contract_refs: Vec::new(),
            artifact_refs: Vec::new(),
            parent_event_id: None,
        }
    }

    fn status_for(events: Vec<HarnessEvent>) -> crate::harness::GateStatus {
        evaluate(&events, &[], &[]).result.status
    }

    let run_id = HarnessRunId::new();
    let first_id = HarnessEventId::new();
    let second_id = HarnessEventId::new();

    status_for(vec![
        event(run_id, first_id, 0),
        event(run_id, second_id, 1),
    ]) == crate::harness::GateStatus::Pass
        && status_for(vec![
            event(run_id, HarnessEventId::new(), 0),
            event(HarnessRunId::new(), HarnessEventId::new(), 1),
        ]) == crate::harness::GateStatus::Fail
        && status_for(vec![event(run_id, first_id, 0), event(run_id, first_id, 1)])
            == crate::harness::GateStatus::Fail
        && status_for(vec![
            event(run_id, HarnessEventId::new(), 0),
            event(run_id, HarnessEventId::new(), 0),
        ]) == crate::harness::GateStatus::Fail
        && status_for(vec![
            event(run_id, HarnessEventId::new(), 2),
            event(run_id, HarnessEventId::new(), 1),
        ]) == crate::harness::GateStatus::Fail
}
