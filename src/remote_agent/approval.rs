//! Receiver-local surface for the ordinary root permission broker.
//! Only the local Desktop command can answer; network job status reveals no targets.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::cli::{ConfirmationOutcome, ConfirmationPrompt};
use crate::error::CliPromptError;
use crate::mcp_publish::PublishProfileId;
use crate::protocol::{ReviewDecision, ToolApprovalDecision};
use crate::runtime::RunControl;
use crate::session::SessionId;
use crate::tool::PermissionRequest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteApprovalContext {
    pub job_id: Ulid,
    pub profile_id: PublishProfileId,
    pub session_id: SessionId,
    pub requester_label: Option<String>,
    pub target_label: String,
}

#[derive(Clone)]
pub(crate) struct RemoteApproval {
    pub confirmation_id: Ulid,
    pub context: RemoteApprovalContext,
    pub request: PermissionRequest,
}

struct Pending {
    projection: RemoteApproval,
    control: RunControl,
    response: mpsc::SyncSender<ReviewDecision>,
}

#[derive(Clone, Default)]
pub(crate) struct ReceiverApprovals(Arc<Mutex<BTreeMap<Ulid, Pending>>>);

impl ReceiverApprovals {
    pub fn pending(&self) -> Option<RemoteApproval> {
        let mut pending = self.0.lock().ok()?;
        pending.retain(|_, entry| !entry.control.is_cancelled());
        pending
            .values()
            .next()
            .map(|entry| entry.projection.clone())
    }

    pub fn waiting(&self, job: Ulid) -> bool {
        self.0.lock().is_ok_and(|pending| {
            pending.values().any(|entry| {
                entry.projection.context.job_id == job && !entry.control.is_cancelled()
            })
        })
    }

    pub fn answer(
        &self,
        id: Ulid,
        job: Ulid,
        profile: PublishProfileId,
        decision: ReviewDecision,
    ) -> bool {
        let Ok(mut pending) = self.0.lock() else {
            return false;
        };
        let Some(entry) = pending.get(&id) else {
            return false;
        };
        if entry.projection.context.job_id != job || entry.projection.context.profile_id != profile
        {
            return false;
        }
        let entry = pending.remove(&id).expect("validated pending approval");
        !entry.control.is_cancelled() && entry.response.try_send(decision).is_ok()
    }

    pub fn prompt(&self, context: RemoteApprovalContext) -> ReceiverConfirmation {
        ReceiverConfirmation {
            approvals: self.clone(),
            context,
        }
    }
}

pub(crate) struct ReceiverConfirmation {
    approvals: ReceiverApprovals,
    context: RemoteApprovalContext,
}

// Even a cancelled or failed worker removes only its own current prompt.
struct PendingGuard(ReceiverApprovals, Ulid);
impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.0.0.lock() {
            pending.remove(&self.1);
        }
    }
}

impl ConfirmationPrompt for ReceiverConfirmation {
    fn confirm(&mut self, request: &PermissionRequest) -> Result<ReviewDecision, CliPromptError> {
        self.confirm_with_control(request, &RunControl::new())?
            .into_review_decision()
    }

    fn confirm_with_control(
        &mut self,
        request: &PermissionRequest,
        control: &RunControl,
    ) -> Result<ConfirmationOutcome, CliPromptError> {
        if control.is_cancelled() {
            return Ok(ConfirmationOutcome::Interrupted);
        }
        let id = Ulid::new();
        let (response, receiver) = mpsc::sync_channel(1);
        {
            let mut pending = self.approvals.0.lock().map_err(|_| {
                CliPromptError::Message("receiver permission surface unavailable".into())
            })?;
            if pending.len() >= 16
                || pending
                    .values()
                    .any(|entry| entry.projection.context.job_id == self.context.job_id)
            {
                return Err(CliPromptError::Message(
                    "receiver permission surface already occupied".into(),
                ));
            }
            pending.insert(
                id,
                Pending {
                    projection: RemoteApproval {
                        confirmation_id: id,
                        context: self.context.clone(),
                        request: request.clone(),
                    },
                    control: control.clone(),
                    response,
                },
            );
        }
        let _guard = PendingGuard(self.approvals.clone(), id);
        loop {
            let response = receiver.recv_timeout(Duration::from_millis(25));
            if control.is_cancelled() {
                return Ok(ConfirmationOutcome::Interrupted);
            }
            match response {
                Ok(ReviewDecision::Approved) => {
                    return Ok(ConfirmationOutcome::Resolved(
                        ToolApprovalDecision::Approved,
                    ));
                }
                Ok(ReviewDecision::Denied) => {
                    return Ok(ConfirmationOutcome::Resolved(
                        ToolApprovalDecision::Denied {
                            reason: "permission denied by receiver user".into(),
                        },
                    ));
                }
                Ok(ReviewDecision::Abort) => return Ok(ConfirmationOutcome::AbortRequested),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(CliPromptError::Message(
                        "receiver permission response disconnected".into(),
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> RemoteApprovalContext {
        RemoteApprovalContext {
            job_id: Ulid::new(),
            profile_id: PublishProfileId(Ulid::new()),
            session_id: SessionId::new(),
            requester_label: Some("WinA".into()),
            target_label: "temp".into(),
        }
    }
    fn request() -> PermissionRequest {
        PermissionRequest {
            access: crate::workspace::AccessKind::Shell,
            summary: "inspect receiver".into(),
            details: vec!["reviewed command".into()],
            targets: vec![],
            outside_workspace: false,
            risks: vec![],
            agent_path: None,
            agent_task_name: None,
        }
    }
    fn start(
        approvals: &ReceiverApprovals,
        context: RemoteApprovalContext,
        control: &RunControl,
    ) -> std::thread::JoinHandle<ConfirmationOutcome> {
        let mut prompt = approvals.prompt(context);
        let control = control.clone();
        std::thread::spawn(move || prompt.confirm_with_control(&request(), &control).unwrap())
    }
    fn wait(approvals: &ReceiverApprovals) -> RemoteApproval {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(pending) = approvals.pending() {
                return pending;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn receiver_approval_requires_exact_request_job_and_profile_and_is_consumed_once() {
        let approvals = ReceiverApprovals::default();
        let context = context();
        let control = RunControl::new();
        let worker = start(&approvals, context.clone(), &control);
        let pending = wait(&approvals);
        assert!(approvals.waiting(context.job_id));
        assert!(!approvals.answer(
            Ulid::new(),
            context.job_id,
            context.profile_id,
            ReviewDecision::Approved
        ));
        assert!(!approvals.answer(
            pending.confirmation_id,
            Ulid::new(),
            context.profile_id,
            ReviewDecision::Approved
        ));
        assert!(!approvals.answer(
            pending.confirmation_id,
            context.job_id,
            PublishProfileId(Ulid::new()),
            ReviewDecision::Approved
        ));
        assert!(!worker.is_finished());
        assert!(approvals.answer(
            pending.confirmation_id,
            context.job_id,
            context.profile_id,
            ReviewDecision::Approved
        ));
        assert_eq!(
            worker.join().unwrap(),
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Approved)
        );
        assert!(!approvals.answer(
            pending.confirmation_id,
            context.job_id,
            context.profile_id,
            ReviewDecision::Approved
        ));
        assert!(approvals.pending().is_none());
    }

    #[test]
    fn receiver_denial_abort_and_cancel_have_distinct_outcomes_and_old_ids_cannot_answer() {
        let approvals = ReceiverApprovals::default();
        let context = context();
        let control = RunControl::new();
        let first = start(&approvals, context.clone(), &control);
        let old = wait(&approvals).confirmation_id;
        assert!(approvals.answer(
            old,
            context.job_id,
            context.profile_id,
            ReviewDecision::Denied
        ));
        assert!(matches!(
            first.join().unwrap(),
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Denied { .. })
        ));
        assert!(!control.is_cancelled());
        let second = start(&approvals, context.clone(), &control);
        let current = wait(&approvals).confirmation_id;
        assert_ne!(old, current);
        assert!(!approvals.answer(
            old,
            context.job_id,
            context.profile_id,
            ReviewDecision::Approved
        ));
        assert!(approvals.answer(
            current,
            context.job_id,
            context.profile_id,
            ReviewDecision::Abort
        ));
        assert_eq!(second.join().unwrap(), ConfirmationOutcome::AbortRequested);
        let third = start(&approvals, context.clone(), &control);
        let cancelled = wait(&approvals).confirmation_id;
        control.fail("receiver closed");
        assert!(!approvals.answer(
            cancelled,
            context.job_id,
            context.profile_id,
            ReviewDecision::Approved
        ));
        assert_eq!(third.join().unwrap(), ConfirmationOutcome::Interrupted);
        assert!(approvals.pending().is_none());
    }

    #[test]
    fn cancelling_one_receiver_request_keeps_the_other_job_prompt() {
        let approvals = ReceiverApprovals::default();
        let first_context = context();
        let second_context = context();
        let first_control = RunControl::new();
        let second_control = RunControl::new();
        let first = start(&approvals, first_context.clone(), &first_control);
        wait(&approvals);
        let second = start(&approvals, second_context.clone(), &second_control);
        first_control.fail("stopped first job");
        assert_eq!(first.join().unwrap(), ConfirmationOutcome::Interrupted);
        let pending = wait(&approvals);
        assert_eq!(pending.context.job_id, second_context.job_id);
        assert!(approvals.answer(
            pending.confirmation_id,
            second_context.job_id,
            second_context.profile_id,
            ReviewDecision::Denied
        ));
        assert!(matches!(
            second.join().unwrap(),
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Denied { .. })
        ));
        assert!(approvals.pending().is_none());
    }
}
