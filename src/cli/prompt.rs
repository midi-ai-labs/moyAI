use crate::error::CliPromptError;
use crate::protocol::{ReviewDecision, ToolApprovalDecision, TurnInterruptionCause};
use crate::runtime::{RunCancelOutcome, RunCancellationCause, RunControl};
use crate::tool::PermissionRequest;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmationOutcome {
    Resolved(ToolApprovalDecision),
    AbortRequested,
    Aborted,
    Interrupted,
}

impl ConfirmationOutcome {
    pub(crate) fn into_review_decision(self) -> Result<ReviewDecision, CliPromptError> {
        match self {
            Self::Resolved(ToolApprovalDecision::Approved) => Ok(ReviewDecision::Approved),
            Self::Resolved(ToolApprovalDecision::Denied { .. }) => Ok(ReviewDecision::Denied),
            Self::AbortRequested | Self::Aborted => Ok(ReviewDecision::Abort),
            Self::Interrupted => Err(CliPromptError::Interrupted),
        }
    }
}

pub trait ConfirmationPrompt {
    fn confirm(&mut self, request: &PermissionRequest) -> Result<ReviewDecision, CliPromptError>;

    fn confirm_with_control(
        &mut self,
        request: &PermissionRequest,
        control: &RunControl,
    ) -> Result<ConfirmationOutcome, CliPromptError> {
        if control.is_cancelled() {
            return Ok(ConfirmationOutcome::Interrupted);
        }
        let decision = self.confirm(request)?;
        if control.is_cancelled() {
            return Ok(ConfirmationOutcome::Interrupted);
        }
        Ok(match decision {
            ReviewDecision::Approved => {
                ConfirmationOutcome::Resolved(ToolApprovalDecision::Approved)
            }
            ReviewDecision::Denied => ConfirmationOutcome::Resolved(ToolApprovalDecision::Denied {
                reason: "permission denied by user".to_string(),
            }),
            ReviewDecision::Abort => ConfirmationOutcome::AbortRequested,
        })
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
    approval_abort_handler: Arc<Mutex<Option<ApprovalAbortHandler>>>,
}

type ApprovalAbortHandler = Arc<dyn Fn(&RunControl) -> RunCancelOutcome + Send + Sync + 'static>;

struct ConfirmationTicket {
    request: PermissionRequest,
    control: RunControl,
    abort_origin: Arc<AtomicBool>,
    response: mpsc::SyncSender<Result<ConfirmationOutcome, CliPromptError>>,
}

struct PendingConfirmation {
    response: mpsc::Receiver<Result<ConfirmationOutcome, CliPromptError>>,
    abort_origin: Arc<AtomicBool>,
}

impl SharedConfirmationPrompt {
    pub fn new(prompt: impl ConfirmationPrompt + Send + 'static) -> Self {
        Self::new_inner(prompt, None)
    }

    pub fn new_with_root_control(
        prompt: impl ConfirmationPrompt + Send + 'static,
        root_control: RunControl,
    ) -> Self {
        Self::new_inner(prompt, Some(root_control))
    }

    fn new_inner(
        prompt: impl ConfirmationPrompt + Send + 'static,
        root_control: Option<RunControl>,
    ) -> Self {
        let (tickets, receiver) = mpsc::channel::<ConfirmationTicket>();
        let approval_abort_handler = Arc::new(Mutex::new(None));
        let dispatcher_abort_handler = Arc::clone(&approval_abort_handler);
        std::thread::Builder::new()
            .name("moyai-permission-broker".to_string())
            .spawn(move || {
                permission_dispatch_loop(
                    Box::new(prompt),
                    receiver,
                    root_control,
                    dispatcher_abort_handler,
                )
            })
            .expect("failed to start permission broker thread");
        Self {
            inner: Arc::new(ConfirmationBroker {
                tickets,
                approval_abort_handler,
            }),
        }
    }

    pub(crate) fn set_approval_abort_handler(
        &self,
        handler: impl Fn(&RunControl) -> RunCancelOutcome + Send + Sync + 'static,
    ) {
        *self
            .inner
            .approval_abort_handler
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Arc::new(handler));
    }

    #[cfg(test)]
    pub(crate) fn shares_broker_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    fn enqueue(
        &self,
        request: &PermissionRequest,
        control: RunControl,
    ) -> Result<PendingConfirmation, CliPromptError> {
        let (response, receiver) = mpsc::sync_channel(1);
        let abort_origin = Arc::new(AtomicBool::new(false));
        self.inner
            .tickets
            .send(ConfirmationTicket {
                request: request.clone(),
                control,
                abort_origin: Arc::clone(&abort_origin),
                response,
            })
            .map_err(|_| {
                CliPromptError::Message("permission prompt broker is unavailable".to_string())
            })?;
        Ok(PendingConfirmation {
            response: receiver,
            abort_origin,
        })
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
    fn confirm(&mut self, request: &PermissionRequest) -> Result<ReviewDecision, CliPromptError> {
        let control = RunControl::new();
        self.confirm_with_control(request, &control)?
            .into_review_decision()
    }

    fn confirm_with_control(
        &mut self,
        request: &PermissionRequest,
        control: &RunControl,
    ) -> Result<ConfirmationOutcome, CliPromptError> {
        if control.is_cancelled() {
            return Ok(ConfirmationOutcome::Interrupted);
        }
        self.enqueue(request, control.clone())?.wait(control)
    }
}

impl PendingConfirmation {
    fn wait(self, control: &RunControl) -> Result<ConfirmationOutcome, CliPromptError> {
        loop {
            if control.is_cancelled() && !self.abort_origin.load(Ordering::Acquire) {
                return Ok(ConfirmationOutcome::Interrupted);
            }
            match self.response.recv_timeout(Duration::from_millis(25)) {
                Ok(Ok(ConfirmationOutcome::Aborted))
                    if self.abort_origin.load(Ordering::Acquire) =>
                {
                    return Ok(ConfirmationOutcome::Aborted);
                }
                Ok(result) => {
                    if control.is_cancelled() {
                        return Ok(ConfirmationOutcome::Interrupted);
                    }
                    return result;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if control.is_cancelled() && !self.abort_origin.load(Ordering::Acquire) {
                        return Ok(ConfirmationOutcome::Interrupted);
                    }
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
    root_control: Option<RunControl>,
    approval_abort_handler: Arc<Mutex<Option<ApprovalAbortHandler>>>,
) {
    while let Ok(ticket) = tickets.recv() {
        let mut result = if ticket.control.is_cancelled() {
            Ok(ConfirmationOutcome::Interrupted)
        } else {
            prompt.confirm_with_control(&ticket.request, &ticket.control)
        };
        if matches!(result, Ok(ConfirmationOutcome::Resolved(_))) && ticket.control.is_cancelled() {
            result = Ok(ConfirmationOutcome::Interrupted);
        }
        if matches!(result, Ok(ConfirmationOutcome::AbortRequested))
            && ticket.control.is_cancelled()
        {
            result = Ok(ConfirmationOutcome::Interrupted);
        } else if matches!(result, Ok(ConfirmationOutcome::AbortRequested)) {
            let handler = approval_abort_handler
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            let abort_outcome = claim_ticket_abort_with(
                &ticket.abort_origin,
                || {},
                || {
                    if let Some(handler) = handler {
                        handler(&ticket.control)
                    } else if let Some(root_control) = &root_control {
                        let cause = RunCancellationCause::Interruption(
                            TurnInterruptionCause::ApprovalAborted,
                        );
                        RunControl::request_linked_cancellation(
                            root_control,
                            cause.clone(),
                            &ticket.control,
                            cause,
                        )
                    } else {
                        ticket
                            .control
                            .request_cancel(RunCancellationCause::Interruption(
                                TurnInterruptionCause::ApprovalAborted,
                            ))
                    }
                },
            );
            if matches!(
                abort_outcome,
                RunCancelOutcome::Applied | RunCancelOutcome::Deferred(_)
            ) {
                result = Ok(ConfirmationOutcome::Aborted);
            } else {
                // A pre-existing terminal producer owns this ticket. A late surface Abort is an
                // observation of that interruption, not a new approval-abort origin.
                result = Ok(ConfirmationOutcome::Interrupted);
            }
        }
        let _ = ticket.response.send(result);
    }
}

#[cfg(test)]
fn claim_ticket_abort(
    control: &RunControl,
    abort_origin: &AtomicBool,
    after_origin_published: impl FnOnce(),
) -> RunCancelOutcome {
    claim_ticket_abort_with(abort_origin, after_origin_published, || {
        control.request_cancel(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted,
        ))
    })
}

fn claim_ticket_abort_with(
    abort_origin: &AtomicBool,
    after_origin_published: impl FnOnce(),
    classify: impl FnOnce() -> RunCancelOutcome,
) -> RunCancelOutcome {
    // The ticket-local origin must become visible before the cancellation wake. Otherwise the
    // requesting waiter can observe a cancelled control and incorrectly project its own Abort as
    // an unrelated interruption.
    abort_origin.store(true, Ordering::Release);
    after_origin_published();
    let outcome = classify();
    if outcome == RunCancelOutcome::Rejected {
        abort_origin.store(false, Ordering::Release);
    }
    outcome
}

#[derive(Default)]
pub struct StdConfirmationPrompt;

impl ConfirmationPrompt for StdConfirmationPrompt {
    fn confirm(&mut self, request: &PermissionRequest) -> Result<ReviewDecision, CliPromptError> {
        let control = RunControl::new();
        self.confirm_with_control(request, &control)?
            .into_review_decision()
    }

    fn confirm_with_control(
        &mut self,
        request: &PermissionRequest,
        control: &RunControl,
    ) -> Result<ConfirmationOutcome, CliPromptError> {
        use std::io::{self, Write};

        if control.is_cancelled() {
            return Ok(ConfirmationOutcome::Interrupted);
        }
        let stdin = stdin_line_reader();
        let lines = stdin
            .lines
            .lock()
            .map_err(|_| CliPromptError::Message("stdin reader lock was poisoned".to_string()))?;
        if !drain_stale_stdin_lines(&lines)? {
            return Ok(ConfirmationOutcome::AbortRequested);
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
        write!(
            stderr,
            "Proceed? [y/N] (N = do not run; stop this task for new instructions) "
        )?;
        stderr.flush()?;
        let result = if control.is_cancelled() {
            Ok(ConfirmationOutcome::Interrupted)
        } else {
            wait_for_stdin_confirmation(&lines, control)
        };
        if control.is_cancelled() {
            writeln!(stderr, "\n[confirm cancelled]")?;
            return Ok(ConfirmationOutcome::Interrupted);
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
    control: &RunControl,
) -> Result<ConfirmationOutcome, CliPromptError> {
    loop {
        if control.is_cancelled() {
            return Ok(ConfirmationOutcome::Interrupted);
        }
        match lines.recv_timeout(Duration::from_millis(25)) {
            Ok(StdinLineEvent::Line(input)) => {
                if control.is_cancelled() {
                    return Ok(ConfirmationOutcome::Interrupted);
                }
                if matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    return Ok(ConfirmationOutcome::Resolved(
                        ToolApprovalDecision::Approved,
                    ));
                }
                return Ok(ConfirmationOutcome::AbortRequested);
            }
            Ok(StdinLineEvent::Eof) => {
                return Ok(ConfirmationOutcome::AbortRequested);
            }
            Ok(StdinLineEvent::Error(error)) => return Err(error.into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if control.is_cancelled() {
                    return Ok(ConfirmationOutcome::Interrupted);
                }
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

    struct FixedPrompt(Result<ReviewDecision, &'static str>);

    struct LateAbortAfterClassification(RunCancellationCause);

    impl ConfirmationPrompt for FixedPrompt {
        fn confirm(
            &mut self,
            _request: &PermissionRequest,
        ) -> Result<ReviewDecision, CliPromptError> {
            self.0
                .map_err(|message| CliPromptError::Message(message.to_string()))
        }
    }

    impl ConfirmationPrompt for LateAbortAfterClassification {
        fn confirm(
            &mut self,
            _request: &PermissionRequest,
        ) -> Result<ReviewDecision, CliPromptError> {
            Ok(ReviewDecision::Abort)
        }

        fn confirm_with_control(
            &mut self,
            _request: &PermissionRequest,
            control: &RunControl,
        ) -> Result<ConfirmationOutcome, CliPromptError> {
            control.cancel(self.0.clone());
            Ok(ConfirmationOutcome::AbortRequested)
        }
    }

    impl ConfirmationPrompt for BlockingPrompt {
        fn confirm(
            &mut self,
            request: &PermissionRequest,
        ) -> Result<ReviewDecision, CliPromptError> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            self.entered
                .send(request.summary.clone())
                .map_err(|error| CliPromptError::Message(error.to_string()))?;
            let result = self
                .release
                .recv()
                .map(|_| ReviewDecision::Approved)
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

    #[test]
    fn legacy_review_decision_conversion_never_relabels_interruption_as_abort() {
        assert!(matches!(
            ConfirmationOutcome::Interrupted.into_review_decision(),
            Err(CliPromptError::Interrupted)
        ));
        assert_eq!(
            ConfirmationOutcome::Aborted
                .into_review_decision()
                .expect("explicit abort"),
            ReviewDecision::Abort
        );
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
        let control = RunControl::new();
        let first = broker
            .enqueue(&permission("first"), control.clone())
            .expect("enqueue first");
        let second = broker
            .enqueue(&permission("second"), control.clone())
            .expect("enqueue second");
        let third = broker
            .enqueue(&permission("third"), control.clone())
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

        assert_eq!(
            first.wait(&control).expect("first result"),
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Approved)
        );
        assert_eq!(
            second.wait(&control).expect("second result"),
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Approved)
        );
        assert_eq!(
            third.wait(&control).expect("third result"),
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Approved)
        );
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn queued_cancellation_releases_waiter_and_skips_surface_prompt() {
        let (broker, entered, release, _) = broker_fixture();
        let first_control = RunControl::new();
        let first = broker
            .enqueue(&permission("active"), first_control.clone())
            .expect("enqueue active");
        assert_eq!(
            entered
                .recv_timeout(Duration::from_secs(1))
                .expect("active"),
            "active"
        );

        let queued_control = RunControl::new();
        let queued = broker
            .enqueue(&permission("cancelled"), queued_control.clone())
            .expect("enqueue cancelled");
        let (wait_done, waited) = mpsc::sync_channel(1);
        let wait_control = queued_control.clone();
        std::thread::spawn(move || {
            let _ = wait_done.send(queued.wait(&wait_control));
        });
        queued_control.interrupt(TurnInterruptionCause::UserStop);
        assert_eq!(
            waited
                .recv_timeout(Duration::from_secs(1))
                .expect("cancelled waiter")
                .expect("cancelled result"),
            ConfirmationOutcome::Interrupted
        );

        release.send(()).expect("release active");
        assert_eq!(
            first.wait(&first_control).expect("active result"),
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Approved)
        );
        assert!(matches!(
            entered.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }

    #[test]
    fn broker_keeps_denial_abort_stop_and_surface_failure_distinct() {
        let request = permission("typed outcomes");

        let root_control = RunControl::new();
        let denied_control = RunControl::new();
        let mut denied = SharedConfirmationPrompt::new_with_root_control(
            FixedPrompt(Ok(ReviewDecision::Denied)),
            root_control.clone(),
        );
        assert_eq!(
            denied
                .confirm_with_control(&request, &denied_control)
                .expect("denied outcome"),
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Denied {
                reason: "permission denied by user".to_string(),
            })
        );
        assert_eq!(denied_control.cause(), None);
        assert_eq!(root_control.cause(), None);

        let abort_control = RunControl::new();
        let mut abort = SharedConfirmationPrompt::new_with_root_control(
            FixedPrompt(Ok(ReviewDecision::Abort)),
            root_control.clone(),
        );
        assert_eq!(
            abort
                .confirm_with_control(&request, &abort_control)
                .expect("abort outcome"),
            ConfirmationOutcome::Aborted
        );
        let approval_abort = Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted,
        ));
        assert_eq!(abort_control.cause(), approval_abort);
        assert_eq!(root_control.cause(), approval_abort);

        let stopped_root = RunControl::new();
        let stopped_control = RunControl::new();
        stopped_control.interrupt(TurnInterruptionCause::UserStop);
        let mut stopped = SharedConfirmationPrompt::new_with_root_control(
            FixedPrompt(Ok(ReviewDecision::Approved)),
            stopped_root.clone(),
        );
        assert_eq!(
            stopped
                .confirm_with_control(&request, &stopped_control)
                .expect("stop outcome"),
            ConfirmationOutcome::Interrupted
        );
        assert_eq!(
            stopped_control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::UserStop
            ))
        );
        assert_eq!(stopped_root.cause(), None);

        let failure_root = RunControl::new();
        let failed_control = RunControl::new();
        let mut failed = SharedConfirmationPrompt::new_with_root_control(
            FixedPrompt(Err("surface disconnected")),
            failure_root.clone(),
        );
        let error = failed
            .confirm_with_control(&request, &failed_control)
            .expect_err("surface failure");
        assert!(error.to_string().contains("surface disconnected"));
        assert_eq!(failed_control.cause(), None);
        assert_eq!(failure_root.cause(), None);
    }

    #[test]
    fn late_surface_abort_cannot_steal_a_preexisting_terminal_owner() {
        for existing_cause in [
            RunCancellationCause::Interruption(TurnInterruptionCause::UserStop),
            RunCancellationCause::Failure("permission surface failed".to_string()),
            RunCancellationCause::Superseded,
        ] {
            let root_control = RunControl::new();
            let ticket_control = RunControl::new();
            let handler_calls = Arc::new(AtomicUsize::new(0));
            let mut broker = SharedConfirmationPrompt::new_with_root_control(
                LateAbortAfterClassification(existing_cause.clone()),
                root_control.clone(),
            );
            let observed_handler_calls = Arc::clone(&handler_calls);
            broker.set_approval_abort_handler(move |_| {
                observed_handler_calls.fetch_add(1, Ordering::SeqCst);
                RunCancelOutcome::Rejected
            });

            assert_eq!(
                broker
                    .confirm_with_control(&permission("late abort"), &ticket_control)
                    .expect("typed outcome"),
                ConfirmationOutcome::Interrupted
            );
            std::thread::sleep(Duration::from_millis(30));
            assert_eq!(ticket_control.cause(), Some(existing_cause));
            assert_eq!(root_control.cause(), None);
            assert_eq!(handler_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn requesting_ticket_observes_abort_origin_before_the_cancellation_wake() {
        let control = RunControl::new();
        let abort_origin = Arc::new(AtomicBool::new(false));
        let (response, receiver) = mpsc::sync_channel(1);
        let pending = PendingConfirmation {
            response: receiver,
            abort_origin: Arc::clone(&abort_origin),
        };
        let (waited_tx, waited_rx) = mpsc::sync_channel(1);
        let waiter_control = control.clone();
        std::thread::spawn(move || {
            let _ = waited_tx.send(pending.wait(&waiter_control));
        });

        let origin_published = Arc::new(std::sync::Barrier::new(2));
        let release_cancellation = Arc::new(std::sync::Barrier::new(2));
        let dispatcher_control = control.clone();
        let dispatcher_origin = Arc::clone(&abort_origin);
        let dispatcher_published = Arc::clone(&origin_published);
        let dispatcher_release = Arc::clone(&release_cancellation);
        let (claimed_tx, claimed_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let outcome = claim_ticket_abort(&dispatcher_control, &dispatcher_origin, || {
                dispatcher_published.wait();
                dispatcher_release.wait();
            });
            response
                .send(Ok(ConfirmationOutcome::Aborted))
                .expect("publish abort response");
            claimed_tx.send(outcome).expect("publish claim outcome");
        });

        origin_published.wait();
        assert!(abort_origin.load(Ordering::Acquire));
        assert!(!control.is_cancelled());
        release_cancellation.wait();

        assert_eq!(
            claimed_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("abort claim"),
            RunCancelOutcome::Applied
        );
        assert_eq!(
            waited_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("ticket waiter")
                .expect("ticket outcome"),
            ConfirmationOutcome::Aborted
        );
        assert_eq!(
            control.cause(),
            Some(RunCancellationCause::Interruption(
                TurnInterruptionCause::ApprovalAborted
            ))
        );
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

        let control = RunControl::new();
        control.interrupt(TurnInterruptionCause::UserStop);
        assert_eq!(
            wait_for_stdin_confirmation(&receiver, &control).expect("cancelled"),
            ConfirmationOutcome::Interrupted
        );

        lines
            .send(StdinLineEvent::Line("yes".to_string()))
            .expect("queue next response");
        assert_eq!(
            wait_for_stdin_confirmation(&receiver, &RunControl::new()).expect("next response"),
            ConfirmationOutcome::Resolved(ToolApprovalDecision::Approved)
        );

        lines
            .send(StdinLineEvent::Line("yes".to_string()))
            .expect("queue racing response");
        let racing_control = RunControl::new();
        racing_control.interrupt(TurnInterruptionCause::UserStop);
        assert_eq!(
            wait_for_stdin_confirmation(&receiver, &racing_control)
                .expect("cancellation wins over queued approval"),
            ConfirmationOutcome::Interrupted
        );
    }

    #[test]
    fn stdin_confirmation_maps_only_yes_to_approval() {
        for (input, expected) in [
            (
                "y",
                ConfirmationOutcome::Resolved(ToolApprovalDecision::Approved),
            ),
            (
                "YES",
                ConfirmationOutcome::Resolved(ToolApprovalDecision::Approved),
            ),
            ("n", ConfirmationOutcome::AbortRequested),
            ("", ConfirmationOutcome::AbortRequested),
            ("anything else", ConfirmationOutcome::AbortRequested),
        ] {
            let (response, receiver) = mpsc::channel();
            response
                .send(StdinLineEvent::Line(input.to_string()))
                .expect("queue response");
            let control = RunControl::new();
            assert_eq!(
                wait_for_stdin_confirmation(&receiver, &control).expect("confirmation response"),
                expected,
                "input={input:?}"
            );
            assert_eq!(control.cause(), None, "input={input:?}");
        }

        let (response, receiver) = mpsc::channel();
        response
            .send(StdinLineEvent::Eof)
            .expect("queue EOF response");
        let control = RunControl::new();
        assert_eq!(
            wait_for_stdin_confirmation(&receiver, &control).expect("EOF response"),
            ConfirmationOutcome::AbortRequested
        );
        assert_eq!(control.cause(), None);
    }

    #[test]
    fn stdin_wait_preserves_eof_abort_and_read_errors() {
        let (eof_response, eof_receiver) = mpsc::channel();
        eof_response
            .send(StdinLineEvent::Eof)
            .expect("queue EOF response");
        assert!(!drain_stale_stdin_lines(&eof_receiver).expect("EOF abort"));

        let (error_response, error_receiver) = mpsc::channel();
        error_response
            .send(StdinLineEvent::Error(std::io::Error::other("stdin failed")))
            .expect("queue stdin error");
        assert!(drain_stale_stdin_lines(&error_receiver).is_err());
    }
}
