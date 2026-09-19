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

#[test]
fn reconfirmed_approval_keeps_one_waiter_and_uses_only_the_new_effect_identity() {
    for cancel in [false, true] {
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
                        summary: "same suspended operation".into(),
                        details: vec!["no replay".into()],
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
        assert!(
            approvals
                .reconfirm(Ulid::new(), pending.approval_id)
                .is_none()
        );
        assert!(approvals.reconfirm(run, Ulid::new()).is_none());
        let replacement = approvals.reconfirm(run, pending.approval_id).unwrap();
        assert_ne!(replacement, pending.approval_id);
        assert_eq!(
            serde_json::to_value(&approvals.pending(run).unwrap().request).unwrap(),
            serde_json::to_value(&pending.request).unwrap()
        );
        assert!(approvals.reconfirm(run, pending.approval_id).is_none());
        assert!(!approvals.answer(run, pending.approval_id, LocalApprovalDecision::Approve));
        assert!(!worker.is_finished());
        assert!(control.take_approval_identity().is_none());
        if cancel {
            control.cancel(crate::runtime::RunCancellationCause::Interruption(
                crate::protocol::TurnInterruptionCause::UserStop,
            ));
            assert!(approvals.reconfirm(run, replacement).is_none());
            assert!(!approvals.answer(run, replacement, LocalApprovalDecision::Approve));
            assert_eq!(worker.join().unwrap(), ConfirmationOutcome::Interrupted);
            assert!(control.take_approval_identity().is_none());
        } else {
            assert!(approvals.answer(run, replacement, LocalApprovalDecision::Approve));
            assert_eq!(
                worker.join().unwrap(),
                ConfirmationOutcome::Resolved(ToolApprovalDecision::Approved)
            );
            assert_eq!(
                control.take_approval_identity().as_deref(),
                Some(replacement.to_string().as_str())
            );
        }
        assert!(approvals.pending(run).is_none());
        assert!(!approvals.answer(run, replacement, LocalApprovalDecision::Approve));
    }
}
