use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::OwnedTaskHandle;

const LOCAL_TASK_COMMAND_CAPACITY: usize = 256;
const LOCAL_TASK_SPAWN_ACK_TIMEOUT: Duration = Duration::from_secs(5);

type LocalTaskFuture = Pin<Box<dyn Future<Output = ()> + 'static>>;
type LocalTaskFactory = Box<dyn FnOnce() -> LocalTaskFuture + Send + 'static>;

enum LocalTaskCommand {
    Spawn {
        factory: LocalTaskFactory,
        response: mpsc::SyncSender<Result<tokio::task::JoinHandle<()>, String>>,
    },
}

static NEXT_EXECUTOR_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static CURRENT_EXECUTOR_ID: Cell<Option<u64>> = const { Cell::new(None) };
}

/// Application-lifetime owner for `!Send` runtime tasks.
///
/// moyAI's storage and tool traits intentionally expose local futures, so moving an admitted turn
/// to Tokio's multi-thread scheduler would require an unrelated Send rewrite. This executor keeps
/// those futures on one dedicated `LocalSet`, while returning a real abortable task handle to the
/// session/agent lifecycle owner.
#[derive(Clone)]
pub(crate) struct LocalTaskExecutor {
    inner: Arc<LocalTaskExecutorInner>,
}

struct LocalTaskExecutorInner {
    id: u64,
    commands: tokio::sync::mpsc::Sender<LocalTaskCommand>,
    shutdown: CancellationToken,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
    spawn_ack_timeout: Duration,
}

impl LocalTaskExecutor {
    pub(crate) fn new(thread_name: &str) -> Result<Self, String> {
        Self::new_with_spawn_ack_behavior(thread_name, None, LOCAL_TASK_SPAWN_ACK_TIMEOUT)
    }

    fn new_with_spawn_ack_behavior(
        thread_name: &str,
        spawn_ack_delay: Option<Duration>,
        spawn_ack_timeout: Duration,
    ) -> Result<Self, String> {
        let id = NEXT_EXECUTOR_ID.fetch_add(1, Ordering::Relaxed);
        let (commands, mut receiver) =
            tokio::sync::mpsc::channel::<LocalTaskCommand>(LOCAL_TASK_COMMAND_CAPACITY);
        let shutdown = CancellationToken::new();
        let runtime_shutdown = shutdown.clone();
        let name = thread_name.to_string();
        let thread = thread::Builder::new()
            .name(name)
            .spawn(move || {
                CURRENT_EXECUTOR_ID.with(|current| current.set(Some(id)));
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("failed to build local task runtime");
                let local = tokio::task::LocalSet::new();
                runtime.block_on(local.run_until(async move {
                    loop {
                        tokio::select! {
                            biased;
                            _ = runtime_shutdown.cancelled() => break,
                            command = receiver.recv() => {
                                let Some(LocalTaskCommand::Spawn { factory, response }) = command else {
                                    break;
                                };
                                let handle = tokio::task::spawn_local(factory());
                                if let Some(delay) = spawn_ack_delay {
                                    tokio::time::sleep(delay).await;
                                }
                                deliver_spawn_handle(response, handle);
                            }
                        }
                    }
                }));
                CURRENT_EXECUTOR_ID.with(|current| current.set(None));
            })
            .map_err(|error| format!("failed to start local task runtime: {error}"))?;
        Ok(Self {
            inner: Arc::new(LocalTaskExecutorInner {
                id,
                commands,
                shutdown,
                thread: Mutex::new(Some(thread)),
                spawn_ack_timeout,
            }),
        })
    }

    pub(crate) fn spawn<F, Fut>(
        &self,
        generation: u64,
        factory: F,
    ) -> Result<OwnedTaskHandle, String>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        let mut factory = Some(factory);
        let same_executor =
            CURRENT_EXECUTOR_ID.with(|current| current.get() == Some(self.inner.id));
        let handle = if same_executor {
            tokio::task::spawn_local(factory.take().expect("local task factory is consumed once")())
        } else {
            let (response, receiver) = mpsc::sync_channel(1);
            let factory = factory.take().expect("local task factory is consumed once");
            self.inner
                .commands
                .try_send(LocalTaskCommand::Spawn {
                    factory: Box::new(move || Box::pin(factory())),
                    response,
                })
                .map_err(|error| match error {
                    tokio::sync::mpsc::error::TrySendError::Full(_) => format!(
                        "local task runtime reached its command capacity of {LOCAL_TASK_COMMAND_CAPACITY}"
                    ),
                    tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                        "local task runtime is unavailable".to_string()
                    }
                })?;
            receiver
                .recv_timeout(self.inner.spawn_ack_timeout)
                .map_err(|error| {
                    format!("local task runtime did not accept the worker: {error}")
                })??
        };
        Ok(OwnedTaskHandle::new(generation, handle))
    }
}

impl Drop for LocalTaskExecutorInner {
    fn drop(&mut self) {
        // A dedicated cancellation owner cannot be displaced by a full command queue. Dropping
        // the executor therefore always closes its LocalSet and every still-running local task.
        self.shutdown.cancel();
        let same_executor = CURRENT_EXECUTOR_ID.with(|current| current.get() == Some(self.id));
        if let Ok(thread) = self.thread.get_mut()
            && let Some(thread) = thread.take()
            && !same_executor
        {
            let _ = thread.join();
        }
    }
}

fn deliver_spawn_handle(
    response: mpsc::SyncSender<Result<tokio::task::JoinHandle<()>, String>>,
    handle: tokio::task::JoinHandle<()>,
) {
    if let Err(mpsc::SendError(result)) = response.send(Ok(handle))
        && let Ok(handle) = result
    {
        // Tokio detaches a JoinHandle on drop. If the requester timed out or disappeared before
        // accepting ownership, abort here so no unowned worker can survive the failed handoff.
        handle.abort();
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::*;

    #[test]
    fn local_executor_owns_and_aborts_a_non_send_worker() {
        let executor = LocalTaskExecutor::new("moyai-local-task-test").expect("executor");
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
        let owner = executor
            .spawn(3, move || async move {
                struct DropSignal(mpsc::SyncSender<()>);
                impl Drop for DropSignal {
                    fn drop(&mut self) {
                        let _ = self.0.send(());
                    }
                }
                let _not_send = Rc::new(());
                let _drop_signal = DropSignal(dropped_tx);
                started_tx.send(()).expect("task started");
                std::future::pending::<()>().await;
            })
            .expect("local task");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("task start signal");

        drop(owner);
        dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("aborted task drop signal");
    }

    #[test]
    fn a_local_worker_can_spawn_another_owned_local_worker_without_deadlock() {
        let executor = LocalTaskExecutor::new("moyai-local-nested-test").expect("executor");
        let nested_executor = executor.clone();
        let (completed_tx, completed_rx) = mpsc::sync_channel(1);
        let owner = executor
            .spawn(1, move || async move {
                let nested = nested_executor
                    .spawn(2, move || async move {
                        let _not_send = Rc::new(());
                        completed_tx.send(()).expect("nested completion");
                    })
                    .expect("nested task");
                while !nested.is_finished() {
                    tokio::task::yield_now().await;
                }
                nested.detach();
            })
            .expect("outer task");
        completed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("nested worker must run");
        while !owner.is_finished() {
            thread::yield_now();
        }
        owner.detach();
    }

    #[tokio::test]
    async fn a_dropped_spawn_ack_receiver_aborts_the_unclaimed_worker() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);
            impl Drop for DropSignal {
                fn drop(&mut self) {
                    if let Some(sender) = self.0.take() {
                        let _ = sender.send(());
                    }
                }
            }
            let _drop_signal = DropSignal(Some(dropped_tx));
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        started_rx.await.expect("worker started");
        let (response, receiver) = mpsc::sync_channel(1);
        drop(receiver);

        deliver_spawn_handle(response, worker);

        tokio::time::timeout(Duration::from_secs(1), dropped_rx)
            .await
            .expect("unclaimed worker must be aborted")
            .expect("worker drop signal");
    }

    #[test]
    fn actual_spawn_timeout_aborts_the_worker_created_before_delayed_ack() {
        let executor = LocalTaskExecutor::new_with_spawn_ack_behavior(
            "moyai-local-delayed-ack-test",
            Some(Duration::from_millis(100)),
            Duration::from_millis(10),
        )
        .expect("executor");
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);

        let result = executor.spawn(17, move || async move {
            struct DropSignal(mpsc::SyncSender<()>);
            impl Drop for DropSignal {
                fn drop(&mut self) {
                    let _ = self.0.send(());
                }
            }
            let _signal = DropSignal(dropped_tx);
            started_tx.send(()).expect("worker start");
            std::future::pending::<()>().await;
        });

        assert!(
            result.is_err(),
            "the real spawn API must report the delayed acknowledgement timeout"
        );
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker was created before the acknowledgement");
        dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("late acknowledgement delivery must abort the unclaimed worker");
    }

    #[test]
    fn dropping_executor_closes_pending_local_tasks_even_when_commands_were_queued() {
        let executor = LocalTaskExecutor::new("moyai-local-shutdown-test").expect("executor");
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
        let owner = executor
            .spawn(9, move || async move {
                struct DropSignal(mpsc::SyncSender<()>);
                impl Drop for DropSignal {
                    fn drop(&mut self) {
                        let _ = self.0.send(());
                    }
                }
                let _signal = DropSignal(dropped_tx);
                started_tx.send(()).expect("task started");
                std::future::pending::<()>().await;
            })
            .expect("owned local task");
        started_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("task start signal");

        // The owner is deliberately detached from the caller; executor shutdown must still drop
        // the LocalSet instead of relying on a bounded command-slot handoff.
        owner.detach();
        drop(executor);

        dropped_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("executor shutdown must drop pending tasks");
    }
}
