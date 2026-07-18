use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::RuntimeError;
use crate::protocol::{TurnId, TurnInterruptionCause};
use crate::runtime::{RunCancelOutcome, RunControl};
use crate::session::SessionId;
use tokio::sync::watch;

#[derive(Clone, Default)]
pub struct ActiveRunRegistry {
    state: Arc<Mutex<ActiveRunState>>,
}

#[derive(Default)]
struct ActiveRunState {
    next_generation: u64,
    runs: HashMap<SessionId, ActiveRunEntry>,
}

struct ActiveRunEntry {
    generation: u64,
    control: RunControl,
    steer_activity_tx: watch::Sender<u64>,
    turn_id: Option<TurnId>,
}

pub struct ActiveRunLease {
    registry: ActiveRunRegistry,
    session_id: SessionId,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveRunInterruptOutcome {
    Applied,
    Deferred,
    AlreadyClassified,
    NotActive,
}

impl ActiveRunRegistry {
    pub fn try_start(
        &self,
        session_id: SessionId,
        control: RunControl,
    ) -> Result<ActiveRunLease, RuntimeError> {
        let mut state = self.lock()?;
        if state.runs.contains_key(&session_id) {
            return Err(RuntimeError::Message(format!(
                "session {session_id} already has an active run"
            )));
        }
        state.next_generation = state.next_generation.wrapping_add(1);
        let generation = state.next_generation;
        let (steer_activity_tx, _) = watch::channel(0);
        state.runs.insert(
            session_id,
            ActiveRunEntry {
                generation,
                control,
                steer_activity_tx,
                turn_id: None,
            },
        );
        drop(state);
        Ok(ActiveRunLease {
            registry: self.clone(),
            session_id,
            generation,
        })
    }

    pub fn is_active(&self, session_id: SessionId) -> bool {
        self.lock()
            .map(|state| state.runs.contains_key(&session_id))
            .unwrap_or(false)
    }

    pub fn cancel(
        &self,
        session_id: SessionId,
        cause: TurnInterruptionCause,
    ) -> ActiveRunInterruptOutcome {
        let Ok(state) = self.lock() else {
            return ActiveRunInterruptOutcome::NotActive;
        };
        let Some(run) = state.runs.get(&session_id) else {
            return ActiveRunInterruptOutcome::NotActive;
        };
        match run
            .control
            .request_cancel(crate::runtime::RunCancellationCause::Interruption(cause))
        {
            RunCancelOutcome::Applied => ActiveRunInterruptOutcome::Applied,
            RunCancelOutcome::Deferred(_) => ActiveRunInterruptOutcome::Deferred,
            RunCancelOutcome::Rejected => ActiveRunInterruptOutcome::AlreadyClassified,
        }
    }

    pub fn run_control(&self, session_id: SessionId) -> Option<RunControl> {
        self.lock()
            .ok()
            .and_then(|state| state.runs.get(&session_id).map(|run| run.control.clone()))
    }

    pub fn active_turn_id(&self, session_id: SessionId) -> Option<TurnId> {
        self.lock()
            .ok()
            .and_then(|state| state.runs.get(&session_id).and_then(|run| run.turn_id))
    }

    pub fn active_session_ids(&self) -> Vec<SessionId> {
        self.lock()
            .map(|state| state.runs.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Notifies local waiters after the canonical steer transaction commits.
    ///
    /// The notification deliberately carries no item identity or content. A
    /// coalesced or missed notification is harmless because canonical history
    /// remains the sole input owner and is consumed through its durable cursor.
    pub fn notify_steer_activity(
        &self,
        session_id: SessionId,
        expected_turn_id: TurnId,
    ) -> Result<(), RuntimeError> {
        let state = self.lock()?;
        let run = state.runs.get(&session_id).ok_or_else(|| {
            RuntimeError::Message(format!(
                "session {session_id} has no active run to receive steer input"
            ))
        })?;
        let turn_id = run.turn_id.ok_or_else(|| {
            RuntimeError::Message(format!(
                "session {session_id} has not started an active turn yet"
            ))
        })?;
        if turn_id != expected_turn_id {
            return Err(RuntimeError::Message(format!(
                "expected active turn id `{expected_turn_id}` but current active turn id is `{turn_id}`"
            )));
        }
        run.steer_activity_tx
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        Ok(())
    }

    pub fn steer_generation(&self, session_id: SessionId) -> Result<u64, RuntimeError> {
        let state = self.lock()?;
        let run = state.runs.get(&session_id).ok_or_else(|| {
            RuntimeError::Message(format!(
                "session {session_id} has no active run to observe steer input"
            ))
        })?;
        Ok(*run.steer_activity_tx.borrow())
    }

    pub async fn wait_for_steer_activity(
        &self,
        session_id: SessionId,
        observed_generation: u64,
    ) -> Result<u64, RuntimeError> {
        let mut activity = {
            let state = self.lock()?;
            let run = state.runs.get(&session_id).ok_or_else(|| {
                RuntimeError::Message(format!(
                    "session {session_id} has no active run to observe steer input"
                ))
            })?;
            run.steer_activity_tx.subscribe()
        };
        let current = *activity.borrow_and_update();
        if current != observed_generation {
            return Ok(current);
        }
        activity.changed().await.map_err(|_| {
            RuntimeError::Message(format!(
                "active run for session {session_id} stopped while waiting for steer input"
            ))
        })?;
        Ok(*activity.borrow_and_update())
    }

    fn set_turn_id(
        &self,
        session_id: SessionId,
        generation: u64,
        turn_id: TurnId,
    ) -> Result<(), RuntimeError> {
        let mut state = self.lock()?;
        let run = state.runs.get_mut(&session_id).ok_or_else(|| {
            RuntimeError::Message(format!("active run for session {session_id} disappeared"))
        })?;
        if run.generation != generation {
            return Err(RuntimeError::Message(format!(
                "active run generation changed for session {session_id}"
            )));
        }
        run.turn_id = Some(turn_id);
        Ok(())
    }

    fn remove_if_generation(&self, session_id: SessionId, generation: u64) {
        let Ok(mut state) = self.lock() else {
            return;
        };
        if state
            .runs
            .get(&session_id)
            .is_some_and(|run| run.generation == generation)
        {
            state.runs.remove(&session_id);
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, ActiveRunState>, RuntimeError> {
        self.state
            .lock()
            .map_err(|_| RuntimeError::Message("active run registry lock was poisoned".to_string()))
    }
}

impl ActiveRunLease {
    pub fn set_turn_id(&self, turn_id: TurnId) -> Result<(), RuntimeError> {
        self.registry
            .set_turn_id(self.session_id, self.generation, turn_id)
    }
}

impl Drop for ActiveRunLease {
    fn drop(&mut self) {
        self.registry
            .remove_if_generation(self.session_id, self.generation);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::*;

    #[test]
    fn concurrent_admission_allows_exactly_one_active_run() {
        let registry = ActiveRunRegistry::default();
        let session_id = SessionId::new();
        let barrier = Arc::new(Barrier::new(3));
        let attempted = Arc::new(Barrier::new(3));
        let release = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let registry = registry.clone();
            let barrier = barrier.clone();
            let attempted = attempted.clone();
            let release = release.clone();
            workers.push(thread::spawn(move || {
                barrier.wait();
                let lease = registry.try_start(session_id, RunControl::new());
                let admitted = lease.is_ok();
                attempted.wait();
                release.wait();
                drop(lease);
                admitted
            }));
        }
        barrier.wait();
        attempted.wait();
        assert!(registry.is_active(session_id));
        release.wait();
        let admitted = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker"))
            .filter(|admitted| *admitted)
            .count();
        assert_eq!(admitted, 1);
    }

    #[test]
    fn cancel_and_steer_notification_target_the_registered_run() {
        let registry = ActiveRunRegistry::default();
        let session_id = SessionId::new();
        let control = RunControl::new();
        let lease = registry
            .try_start(session_id, control.clone())
            .expect("register run");
        let turn_id = TurnId::new();
        lease.set_turn_id(turn_id).expect("set turn");
        let observed = registry.steer_generation(session_id).expect("generation");

        registry
            .notify_steer_activity(session_id, turn_id)
            .expect("notify steer");
        assert_ne!(
            registry.steer_generation(session_id).expect("generation"),
            observed
        );
        assert_eq!(
            registry.cancel(session_id, TurnInterruptionCause::UserStop),
            ActiveRunInterruptOutcome::Applied
        );
        assert_eq!(
            registry.cancel(session_id, TurnInterruptionCause::ApprovalAborted),
            ActiveRunInterruptOutcome::AlreadyClassified,
            "a later classification cannot replace the first interruption cause"
        );
        assert!(control.is_cancelled());
        assert_eq!(
            control.cause(),
            Some(crate::runtime::RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
    }

    #[test]
    fn stop_does_not_reclassify_an_active_run_that_already_failed() {
        let registry = ActiveRunRegistry::default();
        let session_id = SessionId::new();
        let control = RunControl::new();
        let _lease = registry
            .try_start(session_id, control.clone())
            .expect("register run");

        assert!(control.fail("provider transport failed"));
        assert_eq!(
            registry.cancel(session_id, TurnInterruptionCause::UserStop),
            ActiveRunInterruptOutcome::AlreadyClassified
        );
        assert_eq!(
            control.cause(),
            Some(crate::runtime::RunCancellationCause::Failure(
                "provider transport failed".to_string()
            ))
        );
    }

    #[test]
    fn stop_is_reported_as_deferred_while_success_commit_owns_classification() {
        let registry = ActiveRunRegistry::default();
        let session_id = SessionId::new();
        let control = RunControl::new();
        let _lease = registry
            .try_start(session_id, control.clone())
            .expect("register run");
        let success_commit = control
            .begin_success_commit()
            .expect("reserve success commit");

        assert_eq!(
            registry.cancel(session_id, TurnInterruptionCause::UserStop),
            ActiveRunInterruptOutcome::Deferred
        );
        assert_eq!(
            registry.cancel(session_id, TurnInterruptionCause::ApprovalAborted),
            ActiveRunInterruptOutcome::AlreadyClassified
        );
        assert_eq!(control.cause(), None);
        assert!(!control.is_cancelled());

        assert!(success_commit.seal());
        assert!(control.success_is_sealed());
    }

    #[tokio::test]
    async fn steer_activity_wakes_observers_without_copying_canonical_input() {
        let registry = ActiveRunRegistry::default();
        let session_id = SessionId::new();
        let lease = registry
            .try_start(session_id, RunControl::new())
            .expect("register run");
        let turn_id = TurnId::new();
        lease.set_turn_id(turn_id).expect("set turn");
        let observed = registry.steer_generation(session_id).expect("generation");

        registry
            .notify_steer_activity(session_id, turn_id)
            .expect("notify steer");

        assert_ne!(
            registry
                .wait_for_steer_activity(session_id, observed)
                .await
                .expect("activity"),
            observed
        );
    }

    #[test]
    fn steer_activity_is_constant_memory_and_rejects_stale_turns() {
        let registry = ActiveRunRegistry::default();
        let session_id = SessionId::new();
        let lease = registry
            .try_start(session_id, RunControl::new())
            .expect("register run");
        let turn_id = TurnId::new();
        lease.set_turn_id(turn_id).expect("set turn");
        let observed = registry.steer_generation(session_id).expect("generation");

        for _ in 0..10_000 {
            registry
                .notify_steer_activity(session_id, turn_id)
                .expect("coalesced activity notification");
        }
        assert_ne!(
            registry.steer_generation(session_id).expect("generation"),
            observed
        );

        let error = registry
            .notify_steer_activity(session_id, TurnId::new())
            .expect_err("stale turn notification must be rejected");
        assert!(error.to_string().contains("expected active turn id"));
    }
}
