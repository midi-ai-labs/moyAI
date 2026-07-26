use std::time::Duration;

use tokio_util::task::AbortOnDropHandle;

/// Codex gives an interrupted task a short cooperative-cleanup window before aborting it.
///
/// This is deliberately a runtime lifecycle constant rather than a provider or UI timeout:
/// provider deadlines may be long, but an already-classified Stop must release its process-local
/// execution owner promptly.
pub(crate) const GRACEFUL_TASK_ABORT_TIMEOUT: Duration = Duration::from_millis(100);

/// Exact-generation ownership for one Tokio worker.
///
/// Tokio `JoinHandle` detaches on drop. That is the wrong default for an admitted agent turn:
/// losing the lifecycle owner must stop the worker, not leave it able to mutate state later.
/// Wrapping the handle in `AbortOnDropHandle` gives the same ownership rule used by Codex tasks.
pub(crate) struct OwnedTaskHandle {
    generation: u64,
    handle: Option<AbortOnDropHandle<()>>,
}

impl OwnedTaskHandle {
    pub(crate) fn new(generation: u64, handle: tokio::task::JoinHandle<()>) -> Self {
        Self {
            generation,
            handle: Some(AbortOnDropHandle::new(handle)),
        }
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(AbortOnDropHandle::is_finished)
    }

    pub(crate) fn abort(&self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }

    pub(crate) async fn abort_and_wait(mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
            let _ = handle.await;
        }
    }

    /// Releases an already-finished worker without triggering abort-on-drop.
    pub(crate) fn detach(mut self) {
        if let Some(handle) = self.handle.take() {
            drop(handle.detach());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    #[tokio::test]
    async fn dropping_the_exact_owner_aborts_a_noncooperative_async_worker() {
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_by_worker = Arc::clone(&dropped);
        let started = Arc::new(tokio::sync::Notify::new());
        let started_by_worker = Arc::clone(&started);
        let worker = tokio::spawn(async move {
            struct DropSignal(Arc<AtomicBool>);
            impl Drop for DropSignal {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _signal = DropSignal(dropped_by_worker);
            started_by_worker.notify_one();
            std::future::pending::<()>().await;
        });
        let owner = OwnedTaskHandle::new(7, worker);
        started.notified().await;

        drop(owner);
        tokio::task::yield_now().await;

        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn detach_preserves_a_gracefully_finished_generation() {
        let worker = tokio::spawn(async {});
        let owner = OwnedTaskHandle::new(11, worker);
        tokio::task::yield_now().await;
        assert_eq!(owner.generation(), 11);
        assert!(owner.is_finished());

        owner.detach();
    }
}
