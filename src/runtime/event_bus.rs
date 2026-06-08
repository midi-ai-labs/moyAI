use tokio::sync::broadcast;

use crate::error::RuntimeError;
use crate::session::RunEvent;

#[derive(Clone)]
pub struct RunEventPublisher {
    sender: broadcast::Sender<RunEvent>,
}

pub struct RunEventSubscriber {
    receiver: broadcast::Receiver<RunEvent>,
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

pub(crate) fn run_event_publisher_tolerates_observer_absence_fixture_passes() -> bool {
    let (publisher, subscriber) = RunEventBus::channel(16);
    drop(subscriber);
    let observer_absent_event = RunEvent::SessionStarted {
        session_id: crate::session::SessionId::new(),
        title: "observer absence".to_string(),
    };
    if publisher.publish(observer_absent_event).is_err() {
        return false;
    }

    let (publisher, mut subscriber) = RunEventBus::channel(16);
    let session_id = crate::session::SessionId::new();
    let title = "active observer".to_string();
    if publisher
        .publish(RunEvent::SessionStarted {
            session_id,
            title: title.clone(),
        })
        .is_err()
    {
        return false;
    }
    matches!(
        subscriber.receiver.try_recv(),
        Ok(RunEvent::SessionStarted {
            session_id: delivered_session_id,
            title: delivered_title,
        }) if delivered_session_id == session_id && delivered_title == title
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn run_event_publisher_tolerates_observer_absence() {
        assert!(super::run_event_publisher_tolerates_observer_absence_fixture_passes());
    }
}
