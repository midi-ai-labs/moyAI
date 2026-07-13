use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{
    mpsc::{self, UnboundedReceiver, UnboundedSender},
    watch,
};
use tokio_util::sync::CancellationToken;

use crate::error::RuntimeError;
use crate::protocol::{HistoryItemId, SteerTurn, TurnId};
use crate::session::SessionId;

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
    cancel: CancellationToken,
    steer_tx: UnboundedSender<ActiveSteerInput>,
    steer_activity_tx: watch::Sender<u64>,
    turn_id: Option<TurnId>,
}

pub struct ActiveRunLease {
    registry: ActiveRunRegistry,
    session_id: SessionId,
    generation: u64,
    steer_rx: Option<UnboundedReceiver<ActiveSteerInput>>,
}

#[derive(Clone)]
pub struct ActiveSteerInput {
    pub history_item_id: HistoryItemId,
    pub steer: SteerTurn,
}

impl ActiveRunRegistry {
    pub fn try_start(
        &self,
        session_id: SessionId,
        cancel: CancellationToken,
    ) -> Result<ActiveRunLease, RuntimeError> {
        let mut state = self.lock()?;
        if state.runs.contains_key(&session_id) {
            return Err(RuntimeError::Message(format!(
                "session {session_id} already has an active run"
            )));
        }
        state.next_generation = state.next_generation.wrapping_add(1);
        let generation = state.next_generation;
        let (steer_tx, steer_rx) = mpsc::unbounded_channel();
        let (steer_activity_tx, _) = watch::channel(0);
        state.runs.insert(
            session_id,
            ActiveRunEntry {
                generation,
                cancel,
                steer_tx,
                steer_activity_tx,
                turn_id: None,
            },
        );
        drop(state);
        Ok(ActiveRunLease {
            registry: self.clone(),
            session_id,
            generation,
            steer_rx: Some(steer_rx),
        })
    }

    pub fn is_active(&self, session_id: SessionId) -> bool {
        self.lock()
            .map(|state| state.runs.contains_key(&session_id))
            .unwrap_or(false)
    }

    pub fn cancel(&self, session_id: SessionId) -> bool {
        let Ok(state) = self.lock() else {
            return false;
        };
        let Some(run) = state.runs.get(&session_id) else {
            return false;
        };
        run.cancel.cancel();
        true
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

    pub fn enqueue_steer(
        &self,
        session_id: SessionId,
        expected_turn_id: TurnId,
        history_item_id: HistoryItemId,
        steer: SteerTurn,
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
        run.steer_tx
            .send(ActiveSteerInput {
                history_item_id,
                steer,
            })
            .map_err(|_| {
                RuntimeError::Message(format!(
                    "active run for session {session_id} stopped before steer input was delivered"
                ))
            })?;
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

    pub fn take_steer_receiver(&mut self) -> Option<UnboundedReceiver<ActiveSteerInput>> {
        self.steer_rx.take()
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
        let release = Arc::new(Barrier::new(2));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let registry = registry.clone();
            let barrier = barrier.clone();
            let release = release.clone();
            workers.push(thread::spawn(move || {
                barrier.wait();
                let lease = registry.try_start(session_id, CancellationToken::new());
                if lease.is_ok() {
                    release.wait();
                }
                lease.is_ok()
            }));
        }
        barrier.wait();
        while !registry.is_active(session_id) {
            thread::yield_now();
        }
        release.wait();
        let admitted = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker"))
            .filter(|admitted| *admitted)
            .count();
        assert_eq!(admitted, 1);
    }

    #[test]
    fn cancel_and_steer_target_the_registered_run() {
        let registry = ActiveRunRegistry::default();
        let session_id = SessionId::new();
        let token = CancellationToken::new();
        let mut lease = registry
            .try_start(session_id, token.clone())
            .expect("register run");
        let turn_id = TurnId::new();
        lease.set_turn_id(turn_id).expect("set turn");
        let mut receiver = lease.take_steer_receiver().expect("receiver");
        let steer = SteerTurn {
            expected_turn_id: turn_id,
            items: Vec::new(),
            additional_context: Default::default(),
            client_user_message_id: None,
        };
        let history_item_id = HistoryItemId::new();

        registry
            .enqueue_steer(session_id, turn_id, history_item_id, steer)
            .expect("enqueue steer");
        assert_eq!(
            receiver.try_recv().expect("steer").history_item_id,
            history_item_id
        );
        assert!(registry.cancel(session_id));
        assert!(token.is_cancelled());
    }

    #[tokio::test]
    async fn steer_activity_wakes_observers_without_consuming_the_input() {
        let registry = ActiveRunRegistry::default();
        let session_id = SessionId::new();
        let mut lease = registry
            .try_start(session_id, CancellationToken::new())
            .expect("register run");
        let turn_id = TurnId::new();
        lease.set_turn_id(turn_id).expect("set turn");
        let mut receiver = lease.take_steer_receiver().expect("receiver");
        let observed = registry.steer_generation(session_id).expect("generation");
        let history_item_id = HistoryItemId::new();

        registry
            .enqueue_steer(
                session_id,
                turn_id,
                history_item_id,
                SteerTurn {
                    expected_turn_id: turn_id,
                    items: Vec::new(),
                    additional_context: Default::default(),
                    client_user_message_id: None,
                },
            )
            .expect("enqueue steer");

        assert_ne!(
            registry
                .wait_for_steer_activity(session_id, observed)
                .await
                .expect("activity"),
            observed
        );
        assert_eq!(
            receiver
                .try_recv()
                .expect("steer remains queued")
                .history_item_id,
            history_item_id
        );
    }
}
