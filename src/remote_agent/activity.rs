//! Bounded, receiver-wide view of the existing live workers. This is a projection,
//! not a second owner of job lifecycle or a scan of historical executions.

use serde::{Deserialize, Serialize};

use super::RemoteJobState;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteActivityProjection {
    pub running: usize,
    pub waiting: usize,
    pub awaiting_approval: usize,
    pub cancelling: usize,
    pub unavailable: bool,
}

impl RemoteActivityProjection {
    pub(super) fn from_states(states: impl IntoIterator<Item = RemoteJobState>) -> Self {
        let mut activity = Self::default();
        for state in states {
            match state {
                RemoteJobState::Accepted => activity.waiting += 1,
                RemoteJobState::Running => activity.running += 1,
                RemoteJobState::AwaitingApproval => activity.awaiting_approval += 1,
                RemoteJobState::Cancelling => activity.cancelling += 1,
                RemoteJobState::Completed
                | RemoteJobState::Failed
                | RemoteJobState::Interrupted => {}
            }
        }
        activity
    }

    #[cfg(test)]
    pub(crate) fn active(&self) -> bool {
        self.running + self.waiting + self.awaiting_approval + self.cancelling > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receiver_activity_counts_every_live_state_and_excludes_all_terminals() {
        let activity = RemoteActivityProjection::from_states([
            RemoteJobState::Running,
            RemoteJobState::Running,
            RemoteJobState::Accepted,
            RemoteJobState::AwaitingApproval,
            RemoteJobState::Cancelling,
            RemoteJobState::Completed,
            RemoteJobState::Failed,
            RemoteJobState::Interrupted,
        ]);
        assert_eq!(
            (
                activity.running,
                activity.waiting,
                activity.awaiting_approval,
                activity.cancelling
            ),
            (2, 1, 1, 1)
        );
        assert!(activity.active());
        assert!(
            !RemoteActivityProjection::from_states([
                RemoteJobState::Completed,
                RemoteJobState::Failed,
                RemoteJobState::Interrupted,
            ])
            .active()
        );
        assert!(!RemoteActivityProjection::default().active());
    }
}
