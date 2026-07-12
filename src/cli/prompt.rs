use crate::error::CliPromptError;
use crate::tool::PermissionRequest;

use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use tokio_util::sync::CancellationToken;

pub trait ConfirmationPrompt {
    fn confirm(&mut self, request: &PermissionRequest) -> Result<bool, CliPromptError>;

    fn confirm_with_cancel(
        &mut self,
        request: &PermissionRequest,
        cancel: &CancellationToken,
    ) -> Result<bool, CliPromptError> {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        self.confirm(request)
    }
}

/// A cloneable, serialized permission channel shared by a root run and its child agents.
///
/// A dedicated dispatcher owns the surface prompt and handles tickets in FIFO order. Callers do
/// not hold a mutex while a user answers, and a cancelled caller can stop waiting immediately.
#[derive(Clone)]
pub struct SharedConfirmationPrompt {
    inner: Arc<ConfirmationBroker>,
}

struct ConfirmationBroker {
    tickets: mpsc::Sender<ConfirmationTicket>,
}

struct ConfirmationTicket {
    request: PermissionRequest,
    cancel: CancellationToken,
    response: mpsc::SyncSender<Result<bool, CliPromptError>>,
}

struct PendingConfirmation {
    response: mpsc::Receiver<Result<bool, CliPromptError>>,
}

impl SharedConfirmationPrompt {
    pub fn new(prompt: impl ConfirmationPrompt + Send + 'static) -> Self {
        let (tickets, receiver) = mpsc::channel::<ConfirmationTicket>();
        std::thread::Builder::new()
            .name("moyai-permission-broker".to_string())
            .spawn(move || permission_dispatch_loop(Box::new(prompt), receiver))
            .expect("failed to start permission broker thread");
        Self {
            inner: Arc::new(ConfirmationBroker { tickets }),
        }
    }

    #[cfg(test)]
    pub(crate) fn shares_broker_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    fn enqueue(
        &self,
        request: &PermissionRequest,
        cancel: CancellationToken,
    ) -> Result<PendingConfirmation, CliPromptError> {
        let (response, receiver) = mpsc::sync_channel(1);
        self.inner
            .tickets
            .send(ConfirmationTicket {
                request: request.clone(),
                cancel,
                response,
            })
            .map_err(|_| {
                CliPromptError::Message("permission prompt broker is unavailable".to_string())
            })?;
        Ok(PendingConfirmation { response: receiver })
    }
}

impl fmt::Debug for SharedConfirmationPrompt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedConfirmationPrompt")
            .finish_non_exhaustive()
    }
}

impl ConfirmationPrompt for SharedConfirmationPrompt {
    fn confirm(&mut self, request: &PermissionRequest) -> Result<bool, CliPromptError> {
        self.confirm_with_cancel(request, &CancellationToken::new())
    }

    fn confirm_with_cancel(
        &mut self,
        request: &PermissionRequest,
        cancel: &CancellationToken,
    ) -> Result<bool, CliPromptError> {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        self.enqueue(request, cancel.clone())?.wait(cancel)
    }
}

impl PendingConfirmation {
    fn wait(self, cancel: &CancellationToken) -> Result<bool, CliPromptError> {
        loop {
            if cancel.is_cancelled() {
                return Ok(false);
            }
            match self.response.recv_timeout(Duration::from_millis(25)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(CliPromptError::Message(
                        "permission prompt broker stopped before answering".to_string(),
                    ));
                }
            }
        }
    }
}

fn permission_dispatch_loop(
    mut prompt: Box<dyn ConfirmationPrompt + Send>,
    tickets: mpsc::Receiver<ConfirmationTicket>,
) {
    while let Ok(ticket) = tickets.recv() {
        let result = if ticket.cancel.is_cancelled() {
            Ok(false)
        } else {
            prompt.confirm_with_cancel(&ticket.request, &ticket.cancel)
        };
        let _ = ticket.response.send(result);
    }
}

#[derive(Default)]
pub struct StdConfirmationPrompt;

impl ConfirmationPrompt for StdConfirmationPrompt {
    fn confirm(&mut self, request: &PermissionRequest) -> Result<bool, CliPromptError> {
        self.confirm_with_cancel(request, &CancellationToken::new())
    }

    fn confirm_with_cancel(
        &mut self,
        request: &PermissionRequest,
        cancel: &CancellationToken,
    ) -> Result<bool, CliPromptError> {
        use std::io::{self, Write};

        if cancel.is_cancelled() {
            return Ok(false);
        }
        let stdin = stdin_line_reader();
        let lines = stdin
            .lines
            .lock()
            .map_err(|_| CliPromptError::Message("stdin reader lock was poisoned".to_string()))?;
        if !drain_stale_stdin_lines(&lines)? {
            return Ok(false);
        }
        let mut stderr = io::stderr().lock();
        if let Some(identity) = permission_agent_identity(request) {
            writeln!(stderr, "requesting_agent={identity}")?;
        }
        writeln!(
            stderr,
            "[confirm] {} [{}]",
            request.summary,
            request
                .targets
                .iter()
                .map(|value| value.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )?;
        writeln!(
            stderr,
            "outside_workspace={}  risks={}",
            request.outside_workspace,
            if request.risks.is_empty() {
                "none".to_string()
            } else {
                request
                    .risks
                    .iter()
                    .map(|risk| risk.label())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        )?;
        if !request.details.is_empty() {
            writeln!(stderr, "details:")?;
            for detail in &request.details {
                writeln!(stderr, "  - {detail}")?;
            }
        }
        write!(stderr, "Proceed? [y/N] ")?;
        stderr.flush()?;
        let result = if cancel.is_cancelled() {
            Ok(false)
        } else {
            wait_for_stdin_confirmation(&lines, cancel)
        };
        if cancel.is_cancelled() {
            writeln!(stderr, "\n[confirm cancelled]")?;
            return Ok(false);
        }
        result
    }
}

struct StdinLineReader {
    lines: Mutex<mpsc::Receiver<StdinLineEvent>>,
}

enum StdinLineEvent {
    Line(String),
    Eof,
    Error(std::io::Error),
}

fn stdin_line_reader() -> &'static StdinLineReader {
    static READER: OnceLock<StdinLineReader> = OnceLock::new();
    READER.get_or_init(|| {
        let (lines, receiver) = mpsc::channel::<StdinLineEvent>();
        std::thread::Builder::new()
            .name("moyai-stdin-reader".to_string())
            .spawn(move || stdin_read_loop(lines))
            .expect("failed to start stdin reader thread");
        StdinLineReader {
            lines: Mutex::new(receiver),
        }
    })
}

fn stdin_read_loop(lines: mpsc::Sender<StdinLineEvent>) {
    loop {
        let mut input = String::new();
        match std::io::stdin().read_line(&mut input) {
            Ok(0) => {
                let _ = lines.send(StdinLineEvent::Eof);
                return;
            }
            Ok(_) => {
                if lines.send(StdinLineEvent::Line(input)).is_err() {
                    return;
                }
            }
            Err(error) => {
                let _ = lines.send(StdinLineEvent::Error(error));
                return;
            }
        }
    }
}

/// Drops input that arrived before this prompt became active. The receiver lock is held only for
/// the lifetime of the active prompt and is released immediately when its cancellation is seen.
fn drain_stale_stdin_lines(lines: &mpsc::Receiver<StdinLineEvent>) -> Result<bool, CliPromptError> {
    loop {
        match lines.try_recv() {
            Ok(StdinLineEvent::Line(_)) => {}
            Ok(StdinLineEvent::Eof) => return Ok(false),
            Ok(StdinLineEvent::Error(error)) => return Err(error.into()),
            Err(mpsc::TryRecvError::Empty) => return Ok(true),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(CliPromptError::Message(
                    "stdin reader stopped before prompting".to_string(),
                ));
            }
        }
    }
}

fn wait_for_stdin_confirmation(
    lines: &mpsc::Receiver<StdinLineEvent>,
    cancel: &CancellationToken,
) -> Result<bool, CliPromptError> {
    loop {
        if cancel.is_cancelled() {
            return Ok(false);
        }
        match lines.recv_timeout(Duration::from_millis(25)) {
            Ok(StdinLineEvent::Line(input)) => {
                if cancel.is_cancelled() {
                    return Ok(false);
                }
                return Ok(matches!(
                    input.trim().to_ascii_lowercase().as_str(),
                    "y" | "yes"
                ));
            }
            Ok(StdinLineEvent::Eof) => return Ok(false),
            Ok(StdinLineEvent::Error(error)) => return Err(error.into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(CliPromptError::Message(
                    "stdin reader stopped before answering".to_string(),
                ));
            }
        }
    }
}

fn permission_agent_identity(request: &PermissionRequest) -> Option<String> {
    let path = request.agent_path.as_deref()?.trim();
    if path.is_empty() {
        return None;
    }
    let task_name = request
        .agent_task_name
        .as_deref()
        .unwrap_or_default()
        .trim();
    Some(if task_name.is_empty() {
        path.to_string()
    } else {
        format!("{task_name} ({path})")
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use camino::Utf8PathBuf;

    use super::*;
    use crate::workspace::AccessKind;

    struct BlockingPrompt {
        entered: mpsc::Sender<String>,
        release: mpsc::Receiver<()>,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl ConfirmationPrompt for BlockingPrompt {
        fn confirm(&mut self, request: &PermissionRequest) -> Result<bool, CliPromptError> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            self.entered
                .send(request.summary.clone())
                .map_err(|error| CliPromptError::Message(error.to_string()))?;
            let result = self
                .release
                .recv()
                .map(|_| true)
                .map_err(|error| CliPromptError::Message(error.to_string()));
            self.active.fetch_sub(1, Ordering::SeqCst);
            result
        }
    }

    fn permission(summary: &str) -> PermissionRequest {
        PermissionRequest {
            access: AccessKind::Shell,
            summary: summary.to_string(),
            details: Vec::new(),
            targets: vec![Utf8PathBuf::from("C:/workspace")],
            outside_workspace: false,
            risks: Vec::new(),
            agent_path: None,
            agent_task_name: None,
        }
    }

    fn broker_fixture() -> (
        SharedConfirmationPrompt,
        mpsc::Receiver<String>,
        mpsc::Sender<()>,
        Arc<AtomicUsize>,
    ) {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let broker = SharedConfirmationPrompt::new(BlockingPrompt {
            entered: entered_tx,
            release: release_rx,
            active,
            max_active: max_active.clone(),
        });
        (broker, entered_rx, release_tx, max_active)
    }

    #[test]
    fn broker_dispatches_fifo_with_at_most_one_active_confirmation() {
        let (broker, entered, release, max_active) = broker_fixture();
        let cancel = CancellationToken::new();
        let first = broker
            .enqueue(&permission("first"), cancel.clone())
            .expect("enqueue first");
        let second = broker
            .enqueue(&permission("second"), cancel.clone())
            .expect("enqueue second");
        let third = broker
            .enqueue(&permission("third"), cancel.clone())
            .expect("enqueue third");

        assert_eq!(
            entered.recv_timeout(Duration::from_secs(1)).expect("first"),
            "first"
        );
        release.send(()).expect("release first");
        assert_eq!(
            entered
                .recv_timeout(Duration::from_secs(1))
                .expect("second"),
            "second"
        );
        release.send(()).expect("release second");
        assert_eq!(
            entered.recv_timeout(Duration::from_secs(1)).expect("third"),
            "third"
        );
        release.send(()).expect("release third");

        assert!(first.wait(&cancel).expect("first result"));
        assert!(second.wait(&cancel).expect("second result"));
        assert!(third.wait(&cancel).expect("third result"));
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn queued_cancellation_releases_waiter_and_skips_surface_prompt() {
        let (broker, entered, release, _) = broker_fixture();
        let first_cancel = CancellationToken::new();
        let first = broker
            .enqueue(&permission("active"), first_cancel.clone())
            .expect("enqueue active");
        assert_eq!(
            entered
                .recv_timeout(Duration::from_secs(1))
                .expect("active"),
            "active"
        );

        let queued_cancel = CancellationToken::new();
        let queued = broker
            .enqueue(&permission("cancelled"), queued_cancel.clone())
            .expect("enqueue cancelled");
        let (wait_done, waited) = mpsc::sync_channel(1);
        let wait_cancel = queued_cancel.clone();
        std::thread::spawn(move || {
            let _ = wait_done.send(queued.wait(&wait_cancel));
        });
        queued_cancel.cancel();
        assert!(
            !waited
                .recv_timeout(Duration::from_secs(1))
                .expect("cancelled waiter")
                .expect("cancelled result")
        );

        release.send(()).expect("release active");
        assert!(first.wait(&first_cancel).expect("active result"));
        assert!(matches!(
            entered.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }

    #[test]
    fn cli_permission_identity_includes_task_name_and_agent_path() {
        let mut request = permission("child shell");
        request.agent_path = Some("/root/runtime_worker".to_string());
        request.agent_task_name = Some("runtime_worker".to_string());
        assert_eq!(
            permission_agent_identity(&request).as_deref(),
            Some("runtime_worker (/root/runtime_worker)")
        );

        request.agent_task_name = None;
        assert_eq!(
            permission_agent_identity(&request).as_deref(),
            Some("/root/runtime_worker")
        );
    }

    #[test]
    fn continuous_stdin_reader_drains_stale_line_before_next_prompt() {
        let (lines, receiver) = mpsc::channel();
        lines
            .send(StdinLineEvent::Line("yes".to_string()))
            .expect("queue stale line");
        assert!(drain_stale_stdin_lines(&receiver).expect("drain stale line"));

        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(!wait_for_stdin_confirmation(&receiver, &cancel).expect("cancelled"));

        lines
            .send(StdinLineEvent::Line("yes".to_string()))
            .expect("queue next response");
        assert!(
            wait_for_stdin_confirmation(&receiver, &CancellationToken::new())
                .expect("next response")
        );

        lines
            .send(StdinLineEvent::Line("yes".to_string()))
            .expect("queue racing response");
        let racing_cancel = CancellationToken::new();
        racing_cancel.cancel();
        assert!(
            !wait_for_stdin_confirmation(&receiver, &racing_cancel)
                .expect("cancellation wins over queued approval")
        );
    }

    #[test]
    fn stdin_wait_preserves_eof_denial_and_read_errors() {
        let (eof_response, eof_receiver) = mpsc::channel();
        eof_response
            .send(StdinLineEvent::Eof)
            .expect("queue EOF response");
        assert!(!drain_stale_stdin_lines(&eof_receiver).expect("EOF denial"));

        let (error_response, error_receiver) = mpsc::channel();
        error_response
            .send(StdinLineEvent::Error(std::io::Error::other("stdin failed")))
            .expect("queue stdin error");
        assert!(drain_stale_stdin_lines(&error_receiver).is_err());
    }
}
