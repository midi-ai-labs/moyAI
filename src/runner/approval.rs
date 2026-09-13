use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::cli::{ConfirmationOutcome, ConfirmationPrompt};
use crate::error::CliPromptError;
use crate::protocol::{ReviewDecision, ToolApprovalDecision};
use crate::runtime::RunControl;
use crate::tool::PermissionRequest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalApproval {
    pub approval_id: Ulid,
    pub request: PermissionRequest,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalApprovalDecision {
    Approve,
    Deny,
    Stop,
}

struct Pending {
    projection: LocalApproval,
    control: RunControl,
    response: mpsc::SyncSender<LocalApprovalDecision>,
}

#[derive(Clone, Default)]
pub(super) struct LocalApprovals(Arc<Mutex<BTreeMap<Ulid, Pending>>>);

impl LocalApprovals {
    pub fn pending(&self, run: Ulid) -> Option<LocalApproval> {
        self.0
            .lock()
            .ok()?
            .get(&run)
            .filter(|entry| !entry.control.is_cancelled())
            .map(|entry| entry.projection.clone())
    }

    pub fn answer(&self, run: Ulid, id: Ulid, decision: LocalApprovalDecision) -> bool {
        let Ok(mut pending) = self.0.lock() else {
            return false;
        };
        let Some(entry) = pending.get(&run) else {
            return false;
        };
        if entry.projection.approval_id != id || entry.control.is_cancelled() {
            return false;
        }
        let entry = pending.remove(&run).expect("matched approval");
        entry.response.try_send(decision).is_ok()
    }

    pub fn prompt(&self, run: Ulid) -> LocalConfirmation {
        LocalConfirmation {
            approvals: self.clone(),
            run,
        }
    }
}

pub(super) struct LocalConfirmation {
    approvals: LocalApprovals,
    run: Ulid,
}

struct PendingGuard(LocalApprovals, Ulid, Ulid);
impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.0.0.lock() {
            if pending
                .get(&self.1)
                .is_some_and(|entry| entry.projection.approval_id == self.2)
            {
                pending.remove(&self.1);
            }
        }
    }
}

impl ConfirmationPrompt for LocalConfirmation {
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
                CliPromptError::Message("Runner permission channel unavailable".into())
            })?;
            if pending.contains_key(&self.run) {
                return Err(CliPromptError::Message(
                    "Runner approval is already pending".into(),
                ));
            }
            pending.insert(
                self.run,
                Pending {
                    projection: LocalApproval {
                        approval_id: id,
                        request: request.clone(),
                    },
                    control: control.clone(),
                    response,
                },
            );
        }
        let _guard = PendingGuard(self.approvals.clone(), self.run, id);
        loop {
            let decision = receiver.recv_timeout(Duration::from_millis(25));
            if control.is_cancelled() {
                return Ok(ConfirmationOutcome::Interrupted);
            }
            match decision {
                Ok(LocalApprovalDecision::Approve) => {
                    control.record_approval_identity(id.to_string());
                    return Ok(ConfirmationOutcome::Resolved(
                        ToolApprovalDecision::Approved,
                    ));
                }
                Ok(LocalApprovalDecision::Deny) => {
                    return Ok(ConfirmationOutcome::Resolved(
                        ToolApprovalDecision::Denied {
                            reason: "permission denied by the approving user".into(),
                        },
                    ));
                }
                Ok(LocalApprovalDecision::Stop) => return Ok(ConfirmationOutcome::AbortRequested),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(CliPromptError::Message(
                        "Runner permission reply disconnected".into(),
                    ));
                }
            }
        }
    }
}
