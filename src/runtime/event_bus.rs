use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::broadcast;

use crate::error::RuntimeError;
use crate::protocol::{RuntimeEvent, RuntimeEventId};
use crate::session::{RunEvent, SessionId};

#[derive(Clone)]
pub struct RunEventPublisher {
    sender: broadcast::Sender<RunEvent>,
}

pub struct RunEventSubscriber {
    receiver: broadcast::Receiver<RunEvent>,
}

#[derive(Clone)]
pub struct SessionRuntimeEventHub {
    state: Arc<Mutex<SessionRuntimeEventHubState>>,
}

#[derive(Clone)]
pub struct SessionRuntimeEventPublisher {
    state: Arc<Mutex<SessionRuntimeEventHubState>>,
}

struct SessionRuntimeEventHubState {
    buffer: usize,
    senders: HashMap<SessionId, broadcast::Sender<RuntimeEvent>>,
}

pub struct SessionRuntimeEventSubscription {
    session_id: SessionId,
    backfill: VecDeque<RuntimeEvent>,
    backfill_event_ids: HashSet<RuntimeEventId>,
    omitted_backfill_events: usize,
    state: Weak<Mutex<SessionRuntimeEventHubState>>,
    sender: broadcast::Sender<RuntimeEvent>,
    receiver: broadcast::Receiver<RuntimeEvent>,
}

pub trait RunEventSink {
    fn emit(&mut self, event: RunEvent) -> Result<(), RuntimeError>;

    fn reserve_protocol_sequence_no(&mut self) -> Option<i64> {
        None
    }

    fn emit_committed(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
        self.emit(event)
    }

    fn emit_runtime_only(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
        self.emit(event)
    }
}

#[derive(Clone)]
pub struct RunEventBus {
    sender: broadcast::Sender<RunEvent>,
}

impl RunEventBus {
    pub fn channel(buffer: usize) -> (RunEventPublisher, RunEventSubscriber) {
        let (sender, receiver) = broadcast::channel(buffer);
        (
            RunEventPublisher {
                sender: sender.clone(),
            },
            RunEventSubscriber { receiver },
        )
    }

    pub fn new(buffer: usize) -> Self {
        let (sender, _) = broadcast::channel(buffer);
        Self { sender }
    }

    pub fn publisher(&self) -> RunEventPublisher {
        RunEventPublisher {
            sender: self.sender.clone(),
        }
    }

    pub fn subscribe(&self) -> RunEventSubscriber {
        RunEventSubscriber {
            receiver: self.sender.subscribe(),
        }
    }
}

impl RunEventPublisher {
    pub fn publish(&self, event: RunEvent) -> Result<(), RuntimeError> {
        match self.sender.send(event) {
            Ok(_) => Ok(()),
            // broadcast::Sender::send only errors when there are no active receivers.
            // Observer absence is projection fanout state, not runtime control-plane failure.
            Err(_) => Ok(()),
        }
    }
}

impl RunEventSubscriber {
    pub async fn recv(&mut self) -> Result<RunEvent, RuntimeError> {
        self.receiver
            .recv()
            .await
            .map_err(|error| RuntimeError::Message(format!("failed to receive run event: {error}")))
    }
}

impl SessionRuntimeEventHub {
    pub fn new(buffer: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(SessionRuntimeEventHubState {
                buffer: buffer.max(1),
                senders: HashMap::new(),
            })),
        }
    }

    pub fn publisher(&self) -> SessionRuntimeEventPublisher {
        SessionRuntimeEventPublisher {
            state: Arc::clone(&self.state),
        }
    }

    pub fn subscribe(&self, session_id: SessionId) -> SessionRuntimeEventSubscription {
        let mut state = self.state.lock().expect("session event hub mutex poisoned");
        let buffer = state.buffer;
        let sender = state
            .senders
            .entry(session_id)
            .or_insert_with(|| broadcast::channel(buffer).0)
            .clone();
        let receiver = sender.subscribe();
        SessionRuntimeEventSubscription::new(
            session_id,
            Vec::new(),
            0,
            Arc::downgrade(&self.state),
            sender,
            receiver,
        )
    }

    pub fn subscribe_with_backfill(
        &self,
        session_id: SessionId,
        backfill: Vec<RuntimeEvent>,
    ) -> SessionRuntimeEventSubscription {
        self.subscribe(session_id).with_backfill(backfill)
    }
}

impl SessionRuntimeEventPublisher {
    pub fn publish(&self, event: RuntimeEvent) -> Result<(), RuntimeError> {
        let sender = self
            .state
            .lock()
            .expect("session event hub mutex poisoned")
            .senders
            .get(&event.session_id)
            .cloned();
        let Some(sender) = sender else {
            return Ok(());
        };
        match sender.send(event) {
            Ok(_) => Ok(()),
            // Subscriber absence is external listener fanout state, not protocol commit failure.
            Err(_) => Ok(()),
        }
    }
}

impl SessionRuntimeEventSubscription {
    fn new(
        session_id: SessionId,
        backfill: Vec<RuntimeEvent>,
        omitted_backfill_events: usize,
        state: Weak<Mutex<SessionRuntimeEventHubState>>,
        sender: broadcast::Sender<RuntimeEvent>,
        receiver: broadcast::Receiver<RuntimeEvent>,
    ) -> Self {
        let (backfill, omitted_backfill_events) =
            bounded_backfill(backfill, omitted_backfill_events);
        let backfill_event_ids = backfill.iter().map(|event| event.id).collect();
        Self {
            session_id,
            backfill: VecDeque::from(backfill),
            backfill_event_ids,
            omitted_backfill_events,
            state,
            sender,
            receiver,
        }
    }

    pub async fn recv(&mut self) -> Result<RuntimeEvent, RuntimeError> {
        if let Some(event) = self.backfill.pop_front() {
            return Ok(event);
        }
        loop {
            let event = self.receiver.recv().await.map_err(|error| {
                RuntimeError::Message(format!("failed to receive session runtime event: {error}"))
            })?;
            if event.session_id != self.session_id {
                return Err(RuntimeError::Message(format!(
                    "session event channel delivered session {} to subscriber {}",
                    event.session_id, self.session_id
                )));
            }
            if self.backfill_event_ids.remove(&event.id) {
                continue;
            }
            if event.session_id == self.session_id {
                return Ok(event);
            }
        }
    }

    pub fn with_backfill(mut self, backfill: Vec<RuntimeEvent>) -> Self {
        let (backfill, omitted) = bounded_backfill(backfill, 0);
        self.backfill_event_ids = backfill.iter().map(|event| event.id).collect();
        self.backfill = VecDeque::from(backfill);
        self.omitted_backfill_events = omitted;
        self
    }

    pub fn with_bounded_backfill_page(
        mut self,
        backfill: Vec<RuntimeEvent>,
        omitted_backfill_events: usize,
    ) -> Self {
        let (backfill, omitted) = bounded_backfill(backfill, omitted_backfill_events);
        self.backfill_event_ids = backfill.iter().map(|event| event.id).collect();
        self.backfill = VecDeque::from(backfill);
        self.omitted_backfill_events = omitted;
        self
    }

    pub fn omitted_backfill_events(&self) -> usize {
        self.omitted_backfill_events
    }
}

impl Drop for SessionRuntimeEventSubscription {
    fn drop(&mut self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let mut state = state.lock().expect("session event hub mutex poisoned");
        if self.sender.receiver_count() == 1 {
            state.senders.remove(&self.session_id);
        }
    }
}

fn bounded_backfill(
    mut backfill: Vec<RuntimeEvent>,
    omitted_backfill_events: usize,
) -> (Vec<RuntimeEvent>, usize) {
    let limit = crate::protocol::MAX_PROTOCOL_PAGE_LIMIT;
    if backfill.len() <= limit {
        return (backfill, omitted_backfill_events);
    }
    let additionally_omitted = backfill.len() - limit;
    backfill.drain(..additionally_omitted);
    (
        backfill,
        omitted_backfill_events.saturating_add(additionally_omitted),
    )
}

#[cfg(test)]
pub(crate) fn session_runtime_event_subscription_replays_backfill_before_live_without_duplicates_fixture_passes()
-> bool {
    let hub = SessionRuntimeEventHub::new(16);
    let publisher = hub.publisher();
    let session_id = SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let duplicated_event = RuntimeEvent {
        id: crate::protocol::RuntimeEventId::new(),
        session_id,
        turn_id,
        sequence_no: 1,
        created_at_ms: 1,
        msg: crate::protocol::RuntimeEventMsg::Warning {
            message: "backfill".to_string(),
        },
    };
    let live_event = RuntimeEvent {
        id: crate::protocol::RuntimeEventId::new(),
        session_id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 2,
        msg: crate::protocol::RuntimeEventMsg::Warning {
            message: "live".to_string(),
        },
    };
    let mut subscriber = hub.subscribe_with_backfill(session_id, vec![duplicated_event.clone()]);

    if publisher.publish(duplicated_event).is_err() || publisher.publish(live_event).is_err() {
        return false;
    }

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    runtime.block_on(async {
        let first = subscriber.recv().await;
        let second = subscriber.recv().await;
        matches!(first, Ok(event) if event.sequence_no == 1)
            && matches!(second, Ok(event) if event.sequence_no == 2)
    })
}

#[cfg(test)]
pub(crate) fn session_runtime_event_hub_fans_out_committed_events_by_session_fixture_passes() -> bool
{
    let hub = SessionRuntimeEventHub::new(2);
    let publisher = hub.publisher();
    let wanted_session = SessionId::new();
    let other_session = SessionId::new();
    let turn_id = crate::protocol::TurnId::new();
    let mut subscriber = hub.subscribe(wanted_session);
    let wanted_event = RuntimeEvent {
        id: crate::protocol::RuntimeEventId::new(),
        session_id: wanted_session,
        turn_id,
        sequence_no: 0,
        created_at_ms: 1,
        msg: crate::protocol::RuntimeEventMsg::Warning {
            message: "wanted".to_string(),
        },
    };
    let other_event = RuntimeEvent {
        id: crate::protocol::RuntimeEventId::new(),
        session_id: other_session,
        turn_id,
        sequence_no: 0,
        created_at_ms: 1,
        msg: crate::protocol::RuntimeEventMsg::Warning {
            message: "other".to_string(),
        },
    };

    for sequence_no in 0..128 {
        let mut event = other_event.clone();
        event.id = crate::protocol::RuntimeEventId::new();
        event.sequence_no = sequence_no;
        if publisher.publish(event).is_err() {
            return false;
        }
    }
    if publisher.publish(wanted_event).is_err() {
        return false;
    }

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    runtime.block_on(async {
        matches!(
            subscriber.recv().await,
            Ok(event) if event.session_id == wanted_session && event.sequence_no == 0
        )
    })
}

#[cfg(test)]
mod tests {
    use crate::protocol::{RuntimeEvent, RuntimeEventId, RuntimeEventMsg, TurnId};
    use crate::session::SessionId;

    #[test]
    fn session_runtime_event_hub_fans_out_committed_events_by_session() {
        assert!(
            super::session_runtime_event_hub_fans_out_committed_events_by_session_fixture_passes()
        );
    }

    #[test]
    fn session_runtime_event_subscription_replays_backfill_before_live_without_duplicates() {
        assert!(
            super::session_runtime_event_subscription_replays_backfill_before_live_without_duplicates_fixture_passes()
        );
    }

    #[tokio::test]
    async fn runtime_backfill_is_capped_to_one_protocol_page() {
        let hub = super::SessionRuntimeEventHub::new(8);
        let session_id = SessionId::new();
        let turn_id = TurnId::new();
        let backfill = (0..(crate::protocol::MAX_PROTOCOL_PAGE_LIMIT + 25))
            .map(|sequence_no| RuntimeEvent {
                id: RuntimeEventId::new(),
                session_id,
                turn_id,
                sequence_no: sequence_no as i64,
                created_at_ms: sequence_no as i64,
                msg: RuntimeEventMsg::Warning {
                    message: sequence_no.to_string(),
                },
            })
            .collect::<Vec<_>>();
        let mut subscription = hub.subscribe_with_backfill(session_id, backfill);

        assert_eq!(subscription.omitted_backfill_events(), 25);
        assert_eq!(
            subscription
                .recv()
                .await
                .expect("first retained")
                .sequence_no,
            25
        );
    }

    #[test]
    fn dropping_last_session_subscription_releases_its_channel() {
        let hub = super::SessionRuntimeEventHub::new(8);
        let session_id = SessionId::new();
        let subscription = hub.subscribe(session_id);
        assert_eq!(
            hub.state
                .lock()
                .expect("hub")
                .senders
                .get(&session_id)
                .expect("sender")
                .receiver_count(),
            1
        );
        drop(subscription);
        assert!(
            !hub.state
                .lock()
                .expect("hub")
                .senders
                .contains_key(&session_id)
        );
    }
}
