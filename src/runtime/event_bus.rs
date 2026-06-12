use std::collections::{HashSet, VecDeque};

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
    sender: broadcast::Sender<RuntimeEvent>,
}

#[derive(Clone)]
pub struct SessionRuntimeEventPublisher {
    sender: broadcast::Sender<RuntimeEvent>,
}

pub struct SessionRuntimeEventSubscription {
    session_id: SessionId,
    backfill: VecDeque<RuntimeEvent>,
    delivered_event_ids: HashSet<RuntimeEventId>,
    receiver: broadcast::Receiver<RuntimeEvent>,
}

pub trait RunEventSink {
    fn emit(&mut self, event: RunEvent) -> Result<(), RuntimeError>;

    fn reserve_protocol_sequence_no(&mut self) -> Option<i64> {
        None
    }

    fn emit_pre_recorded(&mut self, event: RunEvent) -> Result<(), RuntimeError> {
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
        let (sender, _) = broadcast::channel(buffer);
        Self { sender }
    }

    pub fn publisher(&self) -> SessionRuntimeEventPublisher {
        SessionRuntimeEventPublisher {
            sender: self.sender.clone(),
        }
    }

    pub fn subscribe(&self, session_id: SessionId) -> SessionRuntimeEventSubscription {
        SessionRuntimeEventSubscription::new(session_id, Vec::new(), self.sender.subscribe())
    }

    pub fn subscribe_with_backfill(
        &self,
        session_id: SessionId,
        backfill: Vec<RuntimeEvent>,
    ) -> SessionRuntimeEventSubscription {
        SessionRuntimeEventSubscription::new(session_id, backfill, self.sender.subscribe())
    }
}

impl SessionRuntimeEventPublisher {
    pub fn publish(&self, event: RuntimeEvent) -> Result<(), RuntimeError> {
        match self.sender.send(event) {
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
        receiver: broadcast::Receiver<RuntimeEvent>,
    ) -> Self {
        let delivered_event_ids = backfill.iter().map(|event| event.id).collect();
        Self {
            session_id,
            backfill: VecDeque::from(backfill),
            delivered_event_ids,
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
            if event.session_id == self.session_id && self.delivered_event_ids.insert(event.id) {
                return Ok(event);
            }
        }
    }

    pub fn with_backfill(mut self, backfill: Vec<RuntimeEvent>) -> Self {
        self.delivered_event_ids = backfill.iter().map(|event| event.id).collect();
        self.backfill = VecDeque::from(backfill);
        self
    }
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
        msg: crate::protocol::RuntimeEventMsg::ThreadConfigured {
            model: "backfill".to_string(),
            base_url: "http://local".to_string(),
        },
    };
    let live_event = RuntimeEvent {
        id: crate::protocol::RuntimeEventId::new(),
        session_id,
        turn_id,
        sequence_no: 2,
        created_at_ms: 2,
        msg: crate::protocol::RuntimeEventMsg::ThreadConfigured {
            model: "live".to_string(),
            base_url: "http://local".to_string(),
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
    let hub = SessionRuntimeEventHub::new(16);
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
        msg: crate::protocol::RuntimeEventMsg::ThreadConfigured {
            model: "wanted".to_string(),
            base_url: "http://local".to_string(),
        },
    };
    let other_event = RuntimeEvent {
        id: crate::protocol::RuntimeEventId::new(),
        session_id: other_session,
        turn_id,
        sequence_no: 0,
        created_at_ms: 1,
        msg: crate::protocol::RuntimeEventMsg::ThreadConfigured {
            model: "other".to_string(),
            base_url: "http://local".to_string(),
        },
    };

    if publisher.publish(other_event).is_err() || publisher.publish(wanted_event).is_err() {
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
}
