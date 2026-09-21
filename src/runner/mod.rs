//! A process-owned execution host for private local work and explicit Hub assignments.
//!
//! Receipts are scoped to one Runner incarnation. They survive client disconnects, not a Runner
//! crash. Canonical sessions use the ordinary store. The `shared` module owns a separate durable
//! start journal and accepts only authenticated Hub assignments for operator-installed mappings.

mod approval;
mod autostart;
pub mod operations;
pub mod provision;
mod render;
pub mod shared;
#[cfg(windows)]
pub mod windows;

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

use crate::app::{
    AppBootstrap, AppCommand, AppCommandOutcome, AppProcessRuntime, RunAdmissionKind,
    RunConfigInput, RunRequest, RunService,
};
use crate::cli::{OutputMode, SharedConfirmationPrompt};
use crate::config::ConfigLoader;
use crate::runtime::{LocalTaskExecutor, OwnedTaskHandle, RunControl};
use crate::session::{ActiveTurnExpectation, RunSummary, SessionId, SessionRepository};
use crate::storage::{SqliteStore, StoragePaths, StoreBundle};

pub use approval::{LocalApproval, LocalApprovalDecision};

const MAX_RUNS: usize = 128;
const MAX_PROMPT_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalRunRequest {
    pub directory: Utf8PathBuf,
    pub prompt: String,
    pub session_id: Option<SessionId>,
    pub title: Option<String>,
    /// Explicitly run without detached child agents until their independent controls are exposed.
    #[serde(default)]
    pub single_agent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerIdentity {
    pub runner_id: Ulid,
    pub process_id: u32,
    pub accepting: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalRunState {
    Starting,
    Running,
    WaitingApproval,
    Stopping,
    Draining,
    ProcessesRunning,
    Settled,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalRunSnapshot {
    pub runner_id: Ulid,
    pub run_id: Ulid,
    pub session_id: Option<SessionId>,
    pub state: LocalRunState,
    pub summary: Option<RunSummary>,
    pub error: Option<String>,
    /// Bounded projection of the canonical final response. The session retains the full history.
    pub result_text: Option<String>,
    pub result_truncated: bool,
    pub approval: Option<LocalApproval>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunnerCommand {
    Identity,
    Submit {
        runner_id: Ulid,
        run_id: Ulid,
        request: LocalRunRequest,
    },
    Status {
        runner_id: Ulid,
        run_id: Ulid,
    },
    List {
        runner_id: Ulid,
    },
    Stop {
        runner_id: Ulid,
        run_id: Ulid,
    },
    Approve {
        runner_id: Ulid,
        run_id: Ulid,
        approval_id: Ulid,
        decision: LocalApprovalDecision,
    },
    Shutdown {
        runner_id: Ulid,
    },
    SharedStatus {
        runner_id: Ulid,
    },
    Operations {
        runner_id: Ulid,
        operation: operations::RunnerOperation,
    },
    ExternalAcquire {
        runner_id: Ulid,
        request: shared::external::ExternalAcquire,
    },
    ExternalFinish {
        runner_id: Ulid,
        request: shared::external::ExternalFinish,
    },
    ExternalStatus {
        runner_id: Ulid,
        attempt_id: String,
        generation: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunnerResponse {
    Identity {
        identity: RunnerIdentity,
    },
    Run {
        run: LocalRunSnapshot,
    },
    Runs {
        runs: Vec<LocalRunSnapshot>,
    },
    ApprovalRecorded,
    ShutdownRequested,
    SharedStatus {
        status: Option<shared::SharedProjection>,
    },
    Operations {
        projection: operations::RunnerOperationsProjection,
    },
    ResourceAssignment {
        assignment: shared::Assignment,
    },
    ResourceStatus {
        status: shared::AttemptStatus,
    },
    ResourceFinished,
}

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
#[error("{message}")]
pub struct RunnerError {
    pub message: String,
}

impl RunnerError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Clone)]
pub struct RunnerHost {
    inner: Arc<Host>,
}

struct Host {
    id: Ulid,
    process: AppProcessRuntime,
    executor: LocalTaskExecutor,
    state: Mutex<State>,
    approvals: approval::LocalApprovals,
    stopped: CancellationToken,
    operations: Mutex<operations::OperationsStore>,
}

#[derive(Default)]
struct State {
    closing: bool,
    shared_mode: bool,
    shared_projection: Option<shared::SharedProjection>,
    shared_commands: Option<tokio::sync::mpsc::Sender<shared::OperatorRequest>>,
    external_commands: Option<tokio::sync::mpsc::Sender<shared::external::ExternalRequest>>,
    runs: BTreeMap<Ulid, Execution>,
}

struct Execution {
    request: LocalRunRequest,
    session_id: Option<SessionId>,
    control: RunControl,
    process_lifetime: CancellationToken,
    // Monotonic receipt closure, confirmed only after this worker and its processes have ended.
    // A later turn in the same Session must never reopen an already-drained receipt.
    processes_drained: bool,
    service: Option<Arc<RunService>>,
    worker: Option<OwnedTaskHandle>,
    result: Option<Result<ExecutionOutcome, String>>,
    response: Option<(crate::protocol::ModelResponseId, String, bool)>,
    shared: bool,
    resource: Option<Arc<crate::runtime::resource_admission::ResourceGuard>>,
}

#[derive(Clone)]
enum ExecutionOutcome {
    Completed(RunSummary),
    Yielded(crate::agent::shared::SharedYield),
}

impl ExecutionOutcome {
    fn summary(&self) -> Option<&RunSummary> {
        match self {
            Self::Completed(summary) => Some(summary),
            Self::Yielded(_) => None,
        }
    }
}

#[derive(Clone)]
struct SharedExecution {
    context: crate::agent::shared::SharedRunContext,
    access_mode: crate::config::AccessMode,
    authority: Arc<dyn crate::runtime::ExternalEffectAuthority>,
    resource: Arc<crate::runtime::resource_admission::ResourceGuard>,
    model_client: shared::transport::SharedClient,
}

impl Execution {
    fn active(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(|worker| !worker.is_finished())
    }

    fn observe_process_completion(&mut self, shells: &crate::tool::shell::ManagedShells) {
        if !self.processes_drained
            && !self.active()
            && !self
                .session_id
                .is_some_and(|session| shells.has_local_work(session))
        {
            self.processes_drained = true;
            self.resource = None;
        }
    }
}

impl RunnerHost {
    /// Open the ordinary local store once. No Hub registration, network listener, or elevation.
    pub async fn open() -> Result<Self, RunnerError> {
        ConfigLoader::ensure_default_global_config()
            .map_err(|e| RunnerError::new(e.to_string()))?;
        let paths = StoragePaths::discover().map_err(|e| RunnerError::new(e.to_string()))?;
        let sqlite = SqliteStore::open(&paths).map_err(|e| RunnerError::new(e.to_string()))?;
        sqlite
            .migrate()
            .map_err(|e| RunnerError::new(e.to_string()))?;
        let process = AppBootstrap::create_process_runtime(StoreBundle::new(sqlite))
            .await
            .map_err(|e| RunnerError::new(e.to_string()))?;
        Self::from_process(process)
    }

    fn from_process(process: AppProcessRuntime) -> Result<Self, RunnerError> {
        let operations = operations::OperationsStore::open(&process.store().paths().data_dir)?;
        Ok(Self {
            inner: Arc::new(Host {
                id: Ulid::new(),
                process,
                executor: LocalTaskExecutor::new("moyai-runner").map_err(RunnerError::new)?,
                state: Mutex::new(State::default()),
                approvals: Default::default(),
                stopped: CancellationToken::new(),
                operations: Mutex::new(operations),
            }),
        })
    }

    pub fn identity(&self) -> RunnerIdentity {
        RunnerIdentity {
            runner_id: self.inner.id,
            process_id: std::process::id(),
            accepting: self.inner.state.lock().is_ok_and(|state| !state.closing),
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.inner.stopped.is_cancelled()
    }

    pub async fn wait_stopped(&self) {
        self.inner.stopped.cancelled().await;
    }

    /// Every mutating command names the incarnation the client actually observed. In particular,
    /// an uncertain submission must never be redirected to a replacement Runner automatically.
    pub async fn dispatch(&self, command: RunnerCommand) -> Result<RunnerResponse, RunnerError> {
        match command {
            RunnerCommand::Identity => Ok(RunnerResponse::Identity {
                identity: self.identity(),
            }),
            RunnerCommand::Submit {
                runner_id,
                run_id,
                request,
            } => {
                self.check_incarnation(runner_id)?;
                self.submit(run_id, request)
                    .await
                    .map(|run| RunnerResponse::Run { run })
            }
            RunnerCommand::Status { runner_id, run_id } => {
                self.check_incarnation(runner_id)?;
                self.snapshot(run_id).map(|run| RunnerResponse::Run { run })
            }
            RunnerCommand::List { runner_id } => {
                self.check_incarnation(runner_id)?;
                let ids = self
                    .inner
                    .state
                    .lock()
                    .map_err(|_| RunnerError::new("Runner unavailable"))?
                    .runs
                    .keys()
                    .copied()
                    .collect::<Vec<_>>();
                Ok(RunnerResponse::Runs {
                    runs: ids
                        .into_iter()
                        .map(|id| self.snapshot(id))
                        .collect::<Result<_, _>>()?,
                })
            }
            RunnerCommand::Stop { runner_id, run_id } => {
                self.check_incarnation(runner_id)?;
                self.on_executor(move |host| async move { host.stop_execution(run_id).await })
                    .await?;
                self.snapshot(run_id).map(|run| RunnerResponse::Run { run })
            }
            RunnerCommand::Approve {
                runner_id,
                run_id,
                approval_id,
                decision,
            } => {
                self.check_incarnation(runner_id)?;
                if self.inner.state.lock().map_or(true, |state| {
                    state.runs.get(&run_id).is_none_or(|run| run.shared)
                }) {
                    return Err(RunnerError::new(
                        "Shared execution approvals must be answered through the authenticated Hub; local emergency Stop remains available",
                    ));
                }
                if !self.inner.approvals.answer(run_id, approval_id, decision) {
                    return Err(RunnerError::new(
                        "Approval is no longer pending for this execution",
                    ));
                }
                Ok(RunnerResponse::ApprovalRecorded)
            }
            RunnerCommand::Shutdown { runner_id } => {
                self.check_incarnation(runner_id)?;
                self.begin_shutdown()?;
                Ok(RunnerResponse::ShutdownRequested)
            }
            RunnerCommand::SharedStatus { runner_id } => {
                self.check_incarnation(runner_id)?;
                let status = self
                    .inner
                    .state
                    .lock()
                    .map_err(|_| RunnerError::new("Runner unavailable"))?
                    .shared_projection
                    .clone();
                Ok(RunnerResponse::SharedStatus { status })
            }
            RunnerCommand::Operations {
                runner_id,
                operation,
            } => {
                self.check_incarnation(runner_id)?;
                self.operate(operation)
                    .await
                    .map(|projection| RunnerResponse::Operations { projection })
            }
            command @ (RunnerCommand::ExternalAcquire { runner_id, .. }
            | RunnerCommand::ExternalFinish { runner_id, .. }
            | RunnerCommand::ExternalStatus { runner_id, .. }) => {
                self.check_incarnation(runner_id)?;
                self.dispatch_external(command).await
            }
        }
    }

    fn check_incarnation(&self, id: Ulid) -> Result<(), RunnerError> {
        if id != self.inner.id {
            return Err(RunnerError::new(
                "Runner changed; prior execution state is unknown here. Do not resubmit automatically.",
            ));
        }
        Ok(())
    }

    async fn on_executor<F, Fut, T>(&self, operation: F) -> Result<T, RunnerError>
    where
        F: FnOnce(Self) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, RunnerError>> + 'static,
        T: Send + 'static,
    {
        let host = self.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let handle = self
            .inner
            .executor
            .spawn(0, move || async move {
                let result = operation(host).await;
                let _ = sender.send(result);
            })
            .map_err(RunnerError::new)?;
        // The host owns an accepted command even when its client disconnects before the reply.
        handle.detach();
        receiver
            .await
            .map_err(|_| RunnerError::new("Runner command interrupted"))?
    }

    async fn submit(
        &self,
        id: Ulid,
        request: LocalRunRequest,
    ) -> Result<LocalRunSnapshot, RunnerError> {
        self.submit_execution(id, request, None).await
    }

    async fn submit_execution(
        &self,
        id: Ulid,
        request: LocalRunRequest,
        shared: Option<SharedExecution>,
    ) -> Result<LocalRunSnapshot, RunnerError> {
        if !request.directory.is_absolute()
            || request.prompt.trim().is_empty()
            || request.prompt.len() > MAX_PROMPT_BYTES
            || request
                .title
                .as_ref()
                .is_some_and(|value| value.len() > 256)
        {
            return Err(RunnerError::new(
                "An absolute directory and a nonempty prompt of at most 32 KiB are required",
            ));
        }
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| RunnerError::new("Runner unavailable"))?;
            if let Some(existing) = state.runs.get(&id) {
                if existing.request != request {
                    return Err(RunnerError::new(
                        "Run ID was already used with different input",
                    ));
                }
                drop(state);
                return self.snapshot(id);
            }
            if state.closing {
                return Err(RunnerError::new("Runner is shutting down"));
            }
            let shells = self.inner.process.managed_shells();
            for run in state.runs.values_mut() {
                run.observe_process_completion(&shells);
            }
            if state.runs.values().any(|run| !run.processes_drained) {
                return Err(RunnerError::new(
                    "Runner is busy; another local execution still owns its worker",
                ));
            }
            // Retain every accepted key for the lifetime of this incarnation. Eviction would
            // turn an old client's retransmission into a fresh execution.
            if state.runs.len() >= MAX_RUNS {
                return Err(RunnerError::new(
                    "Runner receipt capacity reached; shut down and start a new Runner after retaining the session IDs",
                ));
            }
            state.runs.insert(
                id,
                Execution {
                    request: request.clone(),
                    session_id: None,
                    control: RunControl::new(),
                    process_lifetime: CancellationToken::new(),
                    processes_drained: false,
                    service: None,
                    worker: None,
                    result: None,
                    response: None,
                    shared: shared.is_some(),
                    resource: shared.as_ref().map(|value| value.resource.clone()),
                },
            );
            if let Some(shared) = &shared {
                state.runs[&id]
                    .control
                    .set_effect_authority(shared.authority.clone());
            }
        }
        let host = self.clone();
        let worker = match self.inner.executor.spawn(0, move || async move {
            let result = host.execute(id, request, shared).await;
            if let Ok(mut state) = host.inner.state.lock() {
                if let Some(run) = state.runs.get_mut(&id) {
                    run.result = Some(result.map_err(|e| e.to_string()));
                }
            }
        }) {
            Ok(worker) => worker,
            Err(error) => {
                self.inner
                    .state
                    .lock()
                    .map_err(|_| RunnerError::new("Runner unavailable"))?
                    .runs
                    .remove(&id);
                return Err(RunnerError::new(error));
            }
        };
        self.inner
            .state
            .lock()
            .map_err(|_| RunnerError::new("Runner unavailable"))?
            .runs
            .get_mut(&id)
            .expect("accepted execution retained")
            .worker = Some(worker);
        self.snapshot(id)
    }

    async fn execute(
        &self,
        id: Ulid,
        request: LocalRunRequest,
        shared: Option<SharedExecution>,
    ) -> Result<ExecutionOutcome, RunnerError> {
        let process = self.inner.process.clone();
        // Capture the turn before checking retained ownership. A concurrent root turn that adds
        // children cannot finish and be silently adopted after the single-agent check.
        let expected_active_turn = match request.session_id {
            Some(session_id) => process
                .session_service()
                .active_turn_expectation_for_session(session_id)
                .await
                .map_err(|e| RunnerError::new(e.to_string()))?
                .ok_or_else(|| RunnerError::new("Session admission state is unavailable"))?,
            None => ActiveTurnExpectation::initial_idle(),
        };
        let mut app = if let Some(session_id) = request.session_id {
            let store = process.store();
            if store.remote_job_store().job_id_for_session(session_id)
                .map_err(|e| RunnerError::new(e.to_string()))?.is_some() {
                return Err(RunnerError::new("A shared receiver session cannot be resumed as independent local work"));
            }
            if !store.session_repo().list_session_spawn_edges(session_id).await
                .map_err(|e| RunnerError::new(e.to_string()))?.is_empty() {
                return Err(RunnerError::new("This session retains child agents; use its existing surface until Runner child controls are available"));
            }
            let session = process
                .store()
                .session_repo()
                .get_session(session_id)
                .await
                .map_err(|e| RunnerError::new(e.to_string()))?;
            AppBootstrap::rebuild_for_session_with_process_runtime(&session, process).await
        } else if shared.is_some() {
            AppBootstrap::rebuild_for_directory_as_workspace_root_with_process_runtime(&request.directory, process).await
        } else {
            AppBootstrap::rebuild_for_directory_with_process_runtime(&request.directory, process)
                .await
        }
        .map_err(|e| RunnerError::new(e.to_string()))?;
        // Detached children have their own stop and lifetime contract. Expose them only when the
        // Runner's independent child controls are wired, rather than imply that root Stop drains
        // the entire tree. Ordinary tools and existing permission/Guardian policy remain intact.
        if app.config.multi_agent.enabled && !request.single_agent {
            return Err(RunnerError::new(
                "Standalone Runner currently requires explicit --single-agent or multi_agent.enabled = false",
            ));
        }
        if request.single_agent {
            app.config.multi_agent.enabled = false;
        }
        if let Some(shared) = &shared {
            app.config.permissions.access_mode = shared.access_mode;
        }
        let (control, lifetime) = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| RunnerError::new("Runner unavailable"))?;
            let run = state.runs.get(&id).expect("accepted execution retained");
            (run.control.clone(), run.process_lifetime.clone())
        };
        app.run_service = Arc::new(app.run_service.with_managed_shell_lifetime(lifetime));
        // Capture once for this assignment. A Desktop save affects the next job, and a Hub
        // failure must never send this job to a retained manual provider.
        let hub_guard = if let Some(shared) = &shared {
            let route = shared.model_client.model_route(control.token()).await?;
            let guard = route.execution_guard();
            app.run_service = Arc::new(app.run_service.with_hub_turn(route));
            Some(guard)
        } else {
            None
        };
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| RunnerError::new("Runner unavailable"))?;
            state
                .runs
                .get_mut(&id)
                .expect("accepted execution retained")
                .service = Some(app.run_service.clone());
        }
        let prompt = self.inner.approvals.prompt(id);
        let mut confirmation =
            SharedConfirmationPrompt::new_with_root_control(prompt, control.clone());
        let mut renderer = render::RunnerRenderer {
            host: self.clone(),
            id,
        };
        let run = RunRequest {
            prompt: request.prompt,
            session_id: request.session_id,
            continue_last: false,
            title: request.title,
            cwd: request.directory,
            config: RunConfigInput::Resolved(app.config.clone()),
            output_mode: OutputMode::Json,
            show_reasoning_summary: false,
            prompt_dispatch: None,
            editor_context: None,
            review_request: None,
            image_paths: vec![],
            run_control: control,
            session_access_mode_adoption: None,
            // Normal root ownership and goal continuation need this broker even without children.
            // Resuming a stored child tree is rejected above until child controls are exposed.
            agent_confirmation: Some(confirmation.clone()),
            agent_context: None,
            admission_kind: RunAdmissionKind::NewUserRun,
            expected_active_turn,
        };
        if let Some(shared) = shared {
            let result = app
                .run_service
                .execute_shared(run, shared.context, &mut renderer, &mut confirmation)
                .await
                .map(|outcome| match outcome {
                    crate::agent::shared::SharedRunOutcome::Completed(summary) => {
                        ExecutionOutcome::Completed(summary)
                    }
                    crate::agent::shared::SharedRunOutcome::Yielded(yielded) => {
                        ExecutionOutcome::Yielded(yielded)
                    }
                })
                .map_err(|e| RunnerError::new(e.to_string()));
            if let Some(guard) = hub_guard {
                guard.finish().await;
            }
            return result;
        }
        match app
            .run_service
            .execute(AppCommand::Run(run), &mut renderer, &mut confirmation)
            .await
        {
            Ok(AppCommandOutcome::Turn(summary)) => Ok(ExecutionOutcome::Completed(summary)),
            Ok(AppCommandOutcome::ControlCompleted) => {
                Err(RunnerError::new("Run returned no terminal"))
            }
            Err(error) => Err(RunnerError::new(error.to_string())),
        }
    }

    fn snapshot(&self, id: Ulid) -> Result<LocalRunSnapshot, RunnerError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| RunnerError::new("Runner unavailable"))?;
        let run = state
            .runs
            .get_mut(&id)
            .ok_or_else(|| RunnerError::new("Unknown run ID in this Runner"))?;
        let approval = self.inner.approvals.pending(id);
        run.observe_process_completion(&self.inner.process.managed_shells());
        let response = run
            .result
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .and_then(ExecutionOutcome::summary)
            .and_then(|summary| {
                run.response
                    .as_ref()
                    .filter(|(id, _, _)| Some(*id) == summary.final_response_id())
            });
        let status = if !run.active() && run.result.is_none() {
            LocalRunState::Unknown
        } else if !run.active() && !run.processes_drained {
            LocalRunState::ProcessesRunning
        } else if !run.active() {
            LocalRunState::Settled
        } else if run.result.is_some() {
            LocalRunState::Draining
        } else if run.control.is_cancelled() {
            LocalRunState::Stopping
        } else if approval.is_some() {
            LocalRunState::WaitingApproval
        } else if run.session_id.is_some() {
            LocalRunState::Running
        } else {
            LocalRunState::Starting
        };
        Ok(LocalRunSnapshot {
            runner_id: self.inner.id,
            run_id: id,
            session_id: run.session_id,
            state: status,
            summary: run
                .result
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .and_then(ExecutionOutcome::summary)
                .cloned(),
            error: run
                .result
                .as_ref()
                .and_then(|result| result.as_ref().err())
                .cloned(),
            approval,
            result_text: response.map(|(_, text, _)| text.clone()),
            result_truncated: response.is_some_and(|(_, _, truncated)| *truncated),
        })
    }

    async fn stop_execution(&self, id: Ulid) -> Result<(), RunnerError> {
        let (service, control) = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| RunnerError::new("Runner unavailable"))?;
            let run = state
                .runs
                .get_mut(&id)
                .ok_or_else(|| RunnerError::new("Unknown run ID in this Runner"))?;
            run.observe_process_completion(&self.inner.process.managed_shells());
            if run.processes_drained {
                return Ok(());
            }
            run.process_lifetime.cancel();
            (run.service.clone(), run.control.clone())
        };
        if let Some(service) = service {
            service
                .request_root_execution_stop(&control)
                .await
                .map_err(|e| RunnerError::new(e.to_string()))?;
        } else {
            // Admission has not started: the real RunService receives the already-cancelled owner.
            control.cancel(crate::runtime::RunCancellationCause::Interruption(
                crate::protocol::TurnInterruptionCause::UserStop,
            ));
        }
        Ok(())
    }

    pub fn begin_shutdown(&self) -> Result<(), RunnerError> {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| RunnerError::new("Runner unavailable"))?;
            if state.closing {
                return Ok(());
            }
            state.closing = true;
        }
        self.spawn_shutdown()
    }

    /// Called at the shared worker's serialized operator boundary, after its
    /// journal is empty. A concurrent local run must also be absent/drained.
    pub(crate) fn begin_quiescent_shutdown(&self) -> Result<(), RunnerError> {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| RunnerError::new("Runner unavailable"))?;
            if state.closing
                || state
                    .runs
                    .values()
                    .any(|run| run.active() || !run.processes_drained)
            {
                return Err(RunnerError::new(
                    "Execution is still active or unconfirmed; wait before changing the Hub endpoint",
                ));
            }
            state.closing = true;
        }
        self.spawn_shutdown()
    }

    fn spawn_shutdown(&self) -> Result<(), RunnerError> {
        let host = self.clone();
        self.inner
            .executor
            .spawn(0, move || async move {
                let ids = host
                    .inner
                    .state
                    .lock()
                    .expect("Runner state")
                    .runs
                    .keys()
                    .copied()
                    .collect::<Vec<_>>();
                for id in ids {
                    let _ = host.stop_execution(id).await;
                }
                host.inner.process.managed_shells().shutdown().await;
                loop {
                    if host
                        .inner
                        .state
                        .lock()
                        .is_ok_and(|state| state.runs.values().all(|run| !run.active()))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                host.inner.stopped.cancel();
            })
            .map_err(RunnerError::new)?
            .detach();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
