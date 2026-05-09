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
        self.sender
            .send(event)
            .map(|_| ())
            .map_err(|error| RuntimeError::Message(format!("failed to publish run event: {error}")))
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
