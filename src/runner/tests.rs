use super::*;
use crate::cli::{ConfirmationOutcome, ConfirmationPrompt};
use crate::protocol::ToolApprovalDecision;

#[test]
fn local_approval_is_bound_to_one_run_and_consumed_once() {
    let approvals = approval::LocalApprovals::default();
    let run = Ulid::new();
    let mut prompt = approvals.prompt(run);
    let control = RunControl::new();
    let worker_control = control.clone();
    let worker = std::thread::spawn(move || {
        prompt
            .confirm_with_control(
                &crate::tool::PermissionRequest {
                    access: crate::workspace::AccessKind::Shell,
                    summary: "local request".into(),
                    details: vec![],
                    targets: vec![],
                    outside_workspace: false,
                    risks: vec![],
                    agent_path: None,
                    agent_task_name: None,
                },
                &worker_control,
            )
            .unwrap()
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let pending = loop {
        if let Some(pending) = approvals.pending(run) {
            break pending;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(!approvals.answer(
        Ulid::new(),
        pending.approval_id,
        LocalApprovalDecision::Approve
    ));
    assert!(!approvals.answer(run, Ulid::new(), LocalApprovalDecision::Approve));
    assert!(approvals.answer(run, pending.approval_id, LocalApprovalDecision::Deny));
    assert!(!approvals.answer(run, pending.approval_id, LocalApprovalDecision::Approve));
    assert!(matches!(
        worker.join().unwrap(),
        ConfirmationOutcome::Resolved(ToolApprovalDecision::Denied { .. })
    ));
    assert!(approvals.pending(run).is_none());
}

#[test]
fn stopped_approval_cannot_be_approved_later() {
    let approvals = approval::LocalApprovals::default();
    let run = Ulid::new();
    let mut prompt = approvals.prompt(run);
    let control = RunControl::new();
    let worker_control = control.clone();
    let worker = std::thread::spawn(move || {
        prompt
            .confirm_with_control(
                &crate::tool::PermissionRequest {
                    access: crate::workspace::AccessKind::Shell,
                    summary: "local request".into(),
                    details: vec![],
                    targets: vec![],
                    outside_workspace: false,
                    risks: vec![],
                    agent_path: None,
                    agent_task_name: None,
                },
                &worker_control,
            )
            .unwrap()
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let pending = loop {
        if let Some(pending) = approvals.pending(run) {
            break pending;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    control.cancel(crate::runtime::RunCancellationCause::Interruption(
        crate::protocol::TurnInterruptionCause::UserStop,
    ));
    assert!(!approvals.answer(run, pending.approval_id, LocalApprovalDecision::Approve));
    assert_eq!(worker.join().unwrap(), ConfirmationOutcome::Interrupted);
    assert!(approvals.pending(run).is_none());
}
