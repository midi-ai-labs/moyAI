use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::app::{AppCommand, RunConfigInput, RunRequest, RunService};
use crate::cli::{EventRenderer, OutputMode, SharedConfirmationPrompt};
use crate::config::{ResolvedConfig, ResolvedTurnConfig};
use crate::error::{AppRunError, CliRenderError};
use crate::protocol::{
    ContentPart, HistoryItemId, InterAgentCommunication, InterAgentMessageType,
    SubAgentActivityKind, TurnId, TurnInterruptionCause, TurnTerminalOutcome,
    render_inter_agent_message,
};
use crate::runtime::agent_control::AgentExecutionWakeCause;
use crate::runtime::{
    ActiveAgentStatus, AgentControl, AgentControlError, AgentExecutionLease, AgentExecutionScope,
    AgentMailCommit, AgentMailDeliveryOutcome, AgentMailboxDeliveryCommit, AgentPath,
    AgentRootContinuationOutcome, AgentSnapshot, AgentStatus, GRACEFUL_TASK_ABORT_TIMEOUT,
    InactiveAgentStatus, LocalTaskExecutor, OwnedTaskHandle, PendingTriggerTerminalCommit,
    RunCancellationCause, RunControl,
};
#[cfg(test)]
use crate::session::SessionRepository;
use crate::session::{
    AdmissionId, CanonicalHistoryPage, CanonicalRuntimeEventPage, CanonicalSessionRead,
    CanonicalTurnPage, IdleTurnAdmission, LoadedSessionList, NewSession, RunEvent, RunSummary,
    RunningSessionRejoin, SessionContext, SessionId, SessionRecord, SessionSettingsPatch,
    SessionSpawnEdge, SessionStatus, ThreadGoalClearResult, ThreadGoalGetResult,
    ThreadGoalSetResult,
};
use crate::storage::{
    StoreBundle,
    session_repo::{
        AgentExecutionWakeTerminalOwner, AgentExecutionWakeTerminalSettlement,
        AgentMailboxDeliverySelector, PendingAgentTriggerSettlement, SpawnContextFork,
        StoredAgentCompletionHandoff,
    },
};
use crate::workspace::Workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentForkTurns {
    None,
    All,
    Recent(usize),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWaitResult {
    pub message: String,
    pub timed_out: bool,
    pub updated_agents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentActivityRecord {
    pub agent_path: String,
    pub session_id: SessionId,
    pub task_name: String,
    pub task_preview: String,
    pub status: AgentStatus,
    pub current_activity: String,
    pub result_preview: String,
    pub started_order: u64,
    pub updated: bool,
    pub is_current_turn: bool,
    pub active_turn_id: Option<TurnId>,
    pub can_interrupt: bool,
}

#[derive(Clone)]
pub struct AgentRunContext {
    runtime: Arc<AgentRuntime>,
    tree: Arc<AgentTreeRuntime>,
    path: AgentPath,
    session_id: SessionId,
    wake_cause: Option<AgentExecutionWakeCause>,
    execution: AgentExecutionScope,
    turn_owner: Arc<OnceLock<AgentDurableTurnOwner>>,
    config: Arc<ResolvedTurnConfig>,
    workspace: Workspace,
    confirmation: SharedConfirmationPrompt,
    run_service: Option<Arc<RunService>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AgentDurableTurnOwner {
    session_id: SessionId,
    admission_id: AdmissionId,
    turn_id: TurnId,
}

impl fmt::Debug for AgentRunContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentRunContext")
            .field("root_session_id", &self.tree.root_session_id)
            .field("session_id", &self.session_id)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl AgentRunContext {
    pub fn path(&self) -> &AgentPath {
        &self.path
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub(crate) fn trigger_history_item_id(&self) -> Option<HistoryItemId> {
        match self.wake_cause {
            Some(AgentExecutionWakeCause::ExplicitTask(history_item_id)) => Some(history_item_id),
            Some(AgentExecutionWakeCause::OwnerResume(_)) | None => None,
        }
    }

    pub(crate) fn owner_resume_request_id(
        &self,
    ) -> Option<crate::storage::session_repo::OwnerResumeRequestId> {
        match self.wake_cause {
            Some(AgentExecutionWakeCause::OwnerResume(request_id)) => Some(request_id),
            Some(AgentExecutionWakeCause::ExplicitTask(_)) | None => None,
        }
    }

    pub(crate) fn has_pending_mailbox_input(&self) -> Result<bool, String> {
        let projected = self
            .tree
            .control
            .list_agents(Some(&self.path))
            .map_err(agent_control_error)?
            .into_iter()
            .find(|agent| agent.path == self.path)
            .map(|agent| agent.pending_mail_count > 0)
            .ok_or_else(|| format!("agent `{}` was not found", self.path))?;
        if projected {
            return Ok(true);
        }
        self.runtime
            .store
            .session_repo()
            .has_pending_agent_mailbox_messages(self.session_id)
            .map_err(|error| error.to_string())
    }

    pub(crate) fn has_pending_turn_steer_input(&self) -> Result<bool, String> {
        let owner = self.durable_turn_owner()?;
        self.runtime
            .store
            .session_repo()
            .has_pending_turn_steers_for_admitted_turn(
                owner.session_id,
                owner.admission_id,
                owner.turn_id,
            )
            .map_err(|error| error.to_string())
    }

    pub fn root_session_id(&self) -> SessionId {
        self.tree.root_session_id
    }

    pub fn is_sub_agent(&self) -> bool {
        !self.path.is_root()
    }

    pub fn task_name(&self) -> &str {
        self.path.name()
    }

    pub(crate) fn confirmation_prompt(&self) -> SharedConfirmationPrompt {
        self.confirmation.clone()
    }

    pub(crate) fn model_request_gate(&self) -> Arc<tokio::sync::Semaphore> {
        Arc::clone(&self.tree.model_request_gate)
    }

    pub(crate) fn mark_durable_turn_admitted(&self) -> Result<(), String> {
        if !self.is_sub_agent() {
            return Ok(());
        }
        let wake_cause = self.wake_cause.ok_or_else(|| {
            format!(
                "sub-agent `{}` has no durable execution wake owner",
                self.path
            )
        })?;
        let turn_id = self.durable_turn_owner()?.turn_id;
        let repository = self.runtime.store.session_repo();
        let session_id = self.session_id;
        let scheduled = self
            .tree
            .control
            .mark_execution_admitted(
                &self.execution,
                wake_cause,
                turn_id,
                Some("Running assigned task".to_string()),
                move || {
                    repository
                        .schedulable_owner_resume_request_id(session_id)
                        .map_err(|error| error.to_string())
                },
            )
            .map_err(agent_control_error)?;
        self.runtime.launch_scheduled_turns(&self.tree, scheduled);
        Ok(())
    }

    fn effective_config(&self) -> ResolvedConfig {
        self.config.runtime_config().clone()
    }

    pub(crate) fn cancel_for_durable_terminal(&self) -> Result<(), String> {
        self.tree
            .control
            .cancel_for_durable_terminal(&self.path)
            .map_err(agent_control_error)?;
        self.runtime.schedule_cancelled_worker_abort(&self.tree);
        Ok(())
    }

    pub async fn spawn_agent(
        &self,
        task_name: &str,
        message: String,
        fork_turns: AgentForkTurns,
        activity_id: String,
    ) -> Result<AgentSnapshot, String> {
        self.runtime
            .spawn_agent(self, task_name, message, fork_turns, activity_id)
            .await
    }

    pub async fn send_message(
        &self,
        target: &str,
        message: String,
        trigger_turn: bool,
        activity_id: String,
    ) -> Result<AgentPath, String> {
        self.runtime
            .send_message(self, target, message, trigger_turn, activity_id)
            .await
    }

    pub async fn wait_for_activity(&self, timeout_ms: u64) -> Result<AgentWaitResult, String> {
        let own = self
            .tree
            .control
            .list_agents(Some(&self.path))
            .map_err(agent_control_error)?
            .into_iter()
            .find(|agent| agent.path == self.path)
            .ok_or_else(|| format!("agent `{}` was not found", self.path))?;
        if own.pending_mail_count > 0 {
            return Ok(self.wait_result(false)?);
        }

        let wait = self
            .tree
            .control
            .wait_for_mailbox_activity(&self.path, own.mailbox_generation);
        match tokio::time::timeout(Duration::from_millis(timeout_ms), wait).await {
            Ok(Ok(_)) => self.wait_result(false),
            Ok(Err(error)) => Err(agent_control_error(error)),
            Err(_) => Ok(AgentWaitResult {
                message: "Wait timed out.".to_string(),
                timed_out: true,
                updated_agents: Vec::new(),
            }),
        }
    }

    pub fn interrupt_agent(
        &self,
        target: &str,
        activity_id: String,
    ) -> Result<(AgentPath, AgentStatus), String> {
        self.runtime.interrupt_agent(self, target, activity_id)
    }

    fn resolve_agent_target(&self, target: &str) -> Result<AgentPath, String> {
        if let Ok(session_id) = target.parse::<SessionId>() {
            return self
                .tree
                .control
                .path_for_session(session_id)
                .map_err(agent_control_error)?
                .ok_or_else(|| format!("live agent id `{session_id}` was not found"));
        }
        self.path.resolve(target)
    }

    pub fn list_agents(&self, path_prefix: Option<&str>) -> Result<Vec<AgentSnapshot>, String> {
        let prefix = path_prefix
            .map(|prefix| self.path.resolve(prefix).map_err(|error| error.to_string()))
            .transpose()?;
        self.tree
            .control
            .list_agents(prefix.as_ref())
            .map_err(agent_control_error)
    }

    pub(crate) fn set_activity(&self, activity: impl Into<String>) {
        let _ = self.execution.set_activity(Some(activity.into()));
    }

    pub(crate) fn bind_durable_turn_owner(
        &self,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) -> Result<(), String> {
        let owner = AgentDurableTurnOwner {
            session_id: self.session_id,
            admission_id,
            turn_id,
        };
        self.turn_owner.set(owner).map_err(|_| {
            format!(
                "agent `{}` is already bound to a durable turn owner",
                self.path
            )
        })?;
        if self.path.is_root() {
            *self
                .tree
                .active_root_turn_owner
                .lock()
                .map_err(|_| "active root turn owner lock was poisoned".to_string())? = Some(owner);
            let scheduled = self
                .tree
                .control
                .schedule_pending_triggered_executions()
                .map_err(agent_control_error)?;
            self.runtime.launch_scheduled_turns(&self.tree, scheduled);
        }
        Ok(())
    }

    fn durable_turn_owner(&self) -> Result<AgentDurableTurnOwner, String> {
        self.turn_owner
            .get()
            .copied()
            .ok_or_else(|| format!("agent `{}` has no durable turn owner", self.path))
    }

    fn has_durable_turn_owner(&self) -> bool {
        self.turn_owner.get().is_some()
    }

    pub(crate) fn commit_pending_mailbox_delivery(
        &self,
        selector: AgentMailboxDeliverySelector,
        limit: usize,
    ) -> Result<AgentMailboxDeliveryCommit, String> {
        let owner = self.durable_turn_owner()?;
        let committed = self
            .tree
            .control
            .commit_pending_mailbox_delivery(&self.execution, || {
                let page = self
                    .runtime
                    .store
                    .session_repo()
                    .deliver_pending_agent_mail_for_admitted_turn_with_selector(
                        owner.session_id,
                        owner.admission_id,
                        owner.turn_id,
                        selector,
                        limit,
                    )
                    .map_err(|error| error.to_string())?;
                Ok(AgentMailboxDeliveryCommit {
                    history_item_ids: page.history_item_ids,
                    has_more: page.has_more,
                })
            })
            .map_err(agent_control_error)?;
        let authors = self
            .durable_mailbox_authors(&committed.history_item_ids)
            .unwrap_or_default();
        if let Ok(mut metadata) = self.tree.metadata.lock() {
            for author_path in authors {
                if let Some(author) = metadata.get_mut(&author_path) {
                    author.updated = false;
                }
            }
        }
        Ok(committed)
    }

    fn wait_result(&self, timed_out: bool) -> Result<AgentWaitResult, String> {
        let item_ids = self
            .tree
            .control
            .mailbox_history_item_ids(&self.path)
            .map_err(agent_control_error)?;
        let updated_agents = self
            .durable_mailbox_authors(&item_ids)?
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        Ok(AgentWaitResult {
            message: if updated_agents.is_empty() {
                "Wait completed.".to_string()
            } else {
                format!("Updates are available from {}.", updated_agents.join(", "))
            },
            timed_out,
            updated_agents,
        })
    }

    fn durable_mailbox_authors(
        &self,
        item_ids: &[HistoryItemId],
    ) -> Result<Vec<AgentPath>, String> {
        let items = self
            .runtime
            .store
            .session_repo()
            .agent_mailbox_communications_by_id(self.session_id, item_ids)
            .map_err(|error| error.to_string())?;
        let mut authors = Vec::new();
        for (message_id, communication) in items {
            if communication.recipient != self.path.as_str() {
                return Err(format!(
                    "mailbox notice {} targets `{}` instead of `{}`",
                    message_id, communication.recipient, self.path
                ));
            }
            let author = AgentPath::try_from(communication.author.as_str())?;
            if !authors.contains(&author) {
                authors.push(author);
            }
        }
        Ok(authors)
    }
}

pub(crate) struct AgentRuntimeExecution {
    pub context: AgentRunContext,
    lease: Option<AgentExecutionLease>,
}

pub(crate) enum AgentRuntimeContinuationOutcome {
    Admitted(AgentRuntimeExecution),
    Blocked,
    NotReady,
    Invalid,
}

impl AgentRuntimeExecution {
    pub(crate) fn run_control(&self) -> RunControl {
        self.lease
            .as_ref()
            .map(AgentExecutionLease::run_control)
            .expect("an active agent runtime execution must retain its lease")
    }

    fn complete(mut self, status: AgentStatus) -> Result<Vec<AgentExecutionLease>, String> {
        let lease = self
            .lease
            .take()
            .ok_or_else(|| "agent execution lease is unavailable".to_string())?;
        let scheduled = self
            .context
            .tree
            .control
            .complete_execution(lease, inactive_agent_status(status, None)?, None)
            .map_err(agent_control_error)?;
        Ok(scheduled)
    }
}

impl Drop for AgentRuntimeExecution {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            let _ = self.context.tree.control.complete_execution(
                lease,
                InactiveAgentStatus::Errored(
                    "agent execution ended before terminal handoff".to_string(),
                ),
                None,
            );
        }
    }
}

pub struct AgentRuntime {
    store: StoreBundle,
    session_service: crate::session::SessionService,
    trees: Mutex<HashMap<SessionId, Arc<AgentTreeRuntime>>>,
    worker_runtime: LocalTaskExecutor,
    workers: Mutex<AgentWorkerRegistry>,
    #[cfg(test)]
    test_run_service: Mutex<Option<Weak<RunService>>>,
}

struct AgentTreeRuntime {
    root_session_id: SessionId,
    control: AgentControl,
    limits: AgentTreeLimits,
    model_request_gate: Arc<tokio::sync::Semaphore>,
    active_root_turn_owner: Mutex<Option<AgentDurableTurnOwner>>,
    metadata: Mutex<HashMap<AgentPath, AgentNodeMetadata>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AgentTreeLimits {
    max_concurrent_agents: usize,
    max_concurrent_model_requests: usize,
}

impl AgentTreeLimits {
    fn capture(config: &ResolvedConfig) -> Self {
        Self {
            max_concurrent_agents: config.multi_agent.max_concurrent_agents,
            max_concurrent_model_requests: config.multi_agent.max_concurrent_model_requests.max(1),
        }
    }

    fn validate_requested(
        self,
        root_session_id: SessionId,
        config: &ResolvedConfig,
    ) -> Result<(), String> {
        let requested = Self::capture(config);
        if self == requested {
            return Ok(());
        }
        Err(format!(
            "session {root_session_id} retains an agent scheduler with immutable limits \
             max_concurrent_agents={} and max_concurrent_model_requests={}, but this turn \
             requested {} and {}; start a new session to change multi-agent scheduler limits",
            self.max_concurrent_agents,
            self.max_concurrent_model_requests,
            requested.max_concurrent_agents,
            requested.max_concurrent_model_requests,
        ))
    }
}

#[derive(Clone)]
struct AgentNodeMetadata {
    task_name: String,
    task_preview: String,
    config: Arc<ResolvedTurnConfig>,
    workspace: Workspace,
    confirmation: SharedConfirmationPrompt,
    run_service: Option<Arc<RunService>>,
    updated: bool,
    activity_owner: Option<AgentDurableTurnOwner>,
}

struct DurableAgentChild {
    edge: SessionSpawnEdge,
    session_id: SessionId,
    session_status: SessionStatus,
    active_turn_id: Option<TurnId>,
    pending_deferred_turn_id: Option<TurnId>,
    pending_trigger_history_item_id: Option<HistoryItemId>,
    pending_trigger_schedule_ready: bool,
    pending_owner_resume_request_id: Option<crate::storage::session_repo::OwnerResumeRequestId>,
    task_preview: String,
    result: Option<String>,
    interruption_cause: Option<TurnInterruptionCause>,
}

struct AgentLaunchFailure {
    message: String,
    context: AgentRunContext,
    lease: AgentExecutionLease,
}

#[derive(Default)]
struct AgentWorkerRegistry {
    next_generation: u64,
    tasks: HashMap<(SessionId, AgentPath), AgentWorkerEntry>,
}

struct AgentWorkerEntry {
    task: OwnedTaskHandle,
    terminal_owner: AgentWorkerTerminalOwner,
}

#[derive(Clone)]
struct AgentWorkerTerminalOwner {
    session_id: SessionId,
    wake_cause: AgentExecutionWakeCause,
    lease: Arc<Mutex<Option<AgentExecutionLease>>>,
}

#[derive(Clone)]
struct CapturedCancelledWorker {
    path: AgentPath,
    generation: u64,
}

struct AgentWorkerCompletion {
    runtime: Weak<AgentRuntime>,
    root_session_id: SessionId,
    path: AgentPath,
    generation: u64,
}

impl Drop for AgentWorkerCompletion {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.detach_finished_worker(self.root_session_id, &self.path, self.generation);
        }
    }
}

struct AgentTurnCompletion {
    status: AgentStatus,
    activity: Option<String>,
    awaiting_deferred_turn_id: Option<TurnId>,
}

impl AgentTurnCompletion {
    fn new(status: AgentStatus) -> Self {
        Self {
            status,
            activity: None,
            awaiting_deferred_turn_id: None,
        }
    }
}

fn bind_execution_confirmation(confirmation: &SharedConfirmationPrompt) {
    confirmation.set_approval_abort_handler(|requesting_control| {
        requesting_control.request_cancel(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted,
        ))
    });
}

fn reusable_tree_for_limits(
    trees: &mut HashMap<SessionId, Arc<AgentTreeRuntime>>,
    root_session_id: SessionId,
    config: &ResolvedConfig,
) -> Result<Option<Arc<AgentTreeRuntime>>, String> {
    let Some(tree) = trees.get(&root_session_id).cloned() else {
        return Ok(None);
    };
    if tree.control.tree_is_cancelled() {
        trees.remove(&root_session_id);
        return Ok(None);
    }
    if tree.limits == AgentTreeLimits::capture(config) {
        return Ok(Some(tree));
    }
    tree.limits.validate_requested(root_session_id, config)?;
    unreachable!("a mismatched live tree must return a typed limit error")
}

impl AgentRuntime {
    pub fn new(store: StoreBundle, session_service: crate::session::SessionService) -> Self {
        Self {
            store,
            session_service,
            trees: Mutex::new(HashMap::new()),
            worker_runtime: LocalTaskExecutor::new("moyai-agent-runtime")
                .expect("failed to start agent task runtime"),
            workers: Mutex::new(AgentWorkerRegistry::default()),
            #[cfg(test)]
            test_run_service: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub fn bind_run_service(&self, run_service: Weak<RunService>) -> Result<(), String> {
        *self
            .test_run_service
            .lock()
            .map_err(|_| "test run-service binding lock was poisoned".to_string())? =
            Some(run_service);
        Ok(())
    }

    #[cfg(test)]
    fn test_run_service(&self) -> Option<Arc<RunService>> {
        self.test_run_service
            .lock()
            .ok()
            .and_then(|service| service.as_ref().and_then(Weak::upgrade))
    }

    fn reserve_worker_generation(
        &self,
        root_session_id: SessionId,
        path: &AgentPath,
    ) -> Result<u64, String> {
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| "agent worker registry lock was poisoned".to_string())?;
        let generation = workers.next_generation;
        workers.next_generation = generation
            .checked_add(1)
            .ok_or_else(|| "agent worker generation is exhausted".to_string())?;
        if workers.tasks.contains_key(&(root_session_id, path.clone())) {
            return Err(format!(
                "agent `{path}` already has an owned runtime worker"
            ));
        }
        Ok(generation)
    }

    fn install_worker(
        &self,
        root_session_id: SessionId,
        path: AgentPath,
        worker: OwnedTaskHandle,
        terminal_owner: AgentWorkerTerminalOwner,
    ) -> Result<(), (String, OwnedTaskHandle)> {
        let mut workers = match self.workers.lock() {
            Ok(workers) => workers,
            Err(_) => {
                return Err((
                    "agent worker registry lock was poisoned".to_string(),
                    worker,
                ));
            }
        };
        let key = (root_session_id, path.clone());
        if workers.tasks.contains_key(&key) {
            return Err((
                format!("agent `{path}` already has an owned runtime worker"),
                worker,
            ));
        }
        workers.tasks.insert(
            key,
            AgentWorkerEntry {
                task: worker,
                terminal_owner,
            },
        );
        Ok(())
    }

    fn detach_finished_worker(
        &self,
        root_session_id: SessionId,
        path: &AgentPath,
        generation: u64,
    ) {
        let Ok(mut workers) = self.workers.lock() else {
            return;
        };
        let key = (root_session_id, path.clone());
        let Some(current) = workers.tasks.get(&key) else {
            return;
        };
        if current.task.generation() != generation {
            return;
        }
        if let Some(worker) = workers.tasks.remove(&key) {
            worker.task.detach();
        }
    }

    fn schedule_cancelled_worker_abort(self: &Arc<Self>, tree: &Arc<AgentTreeRuntime>) {
        let captured = self.capture_cancelled_workers(tree);
        if captured.is_empty() {
            return;
        }
        let runtime = Arc::downgrade(self);
        let tree = Arc::downgrade(tree);
        let monitor_runtime = runtime.clone();
        let monitor_tree = tree.clone();
        let fallback_captured = captured.clone();
        match self.worker_runtime.spawn(0, move || async move {
            tokio::time::sleep(GRACEFUL_TASK_ABORT_TIMEOUT).await;
            let (Some(runtime), Some(tree)) = (runtime.upgrade(), tree.upgrade()) else {
                return;
            };
            runtime.abort_cancelled_workers(&tree, captured).await;
        }) {
            Ok(monitor) => {
                // The monitor is bounded by the fixed grace period and keeps only weak lifecycle
                // references. It is not an admitted turn and therefore does not belong in the
                // worker registry.
                monitor.detach();
            }
            Err(_) => {
                // Losing the grace timer must never turn an accepted Stop back into a detached
                // worker. If the local executor cannot own the timer, abort the already-cancelled
                // exact generations immediately.
                if let (Some(runtime), Some(tree)) =
                    (monitor_runtime.upgrade(), monitor_tree.upgrade())
                {
                    runtime.abort_cancelled_workers_without_monitor(&tree, fallback_captured);
                }
            }
        }
    }

    fn capture_cancelled_workers(&self, tree: &AgentTreeRuntime) -> Vec<CapturedCancelledWorker> {
        let Ok(cancelled_paths) = tree.control.cancelled_execution_paths() else {
            return Vec::new();
        };
        let Ok(workers) = self.workers.lock() else {
            return Vec::new();
        };
        cancelled_paths
            .into_iter()
            .filter_map(|path| {
                workers
                    .tasks
                    .get(&(tree.root_session_id, path.clone()))
                    .map(|worker| CapturedCancelledWorker {
                        path,
                        generation: worker.task.generation(),
                    })
            })
            .collect()
    }

    fn take_captured_workers(
        &self,
        tree: &AgentTreeRuntime,
        captured: Vec<CapturedCancelledWorker>,
    ) -> Vec<(AgentPath, AgentWorkerEntry)> {
        let Ok(mut workers) = self.workers.lock() else {
            return Vec::new();
        };
        let mut taken = Vec::new();
        for captured in captured {
            let key = (tree.root_session_id, captured.path.clone());
            if workers
                .tasks
                .get(&key)
                .is_some_and(|worker| worker.task.generation() == captured.generation)
                && let Some(worker) = workers.tasks.remove(&key)
            {
                taken.push((captured.path, worker));
            }
        }
        taken
    }

    fn abort_cancelled_workers_without_monitor(
        self: &Arc<Self>,
        tree: &Arc<AgentTreeRuntime>,
        captured: Vec<CapturedCancelledWorker>,
    ) {
        for (path, worker) in self.take_captured_workers(tree, captured) {
            let owner = worker.terminal_owner;
            let lease = owner
                .lease
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            worker.task.abort();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !worker.task.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            if !worker.task.is_finished() {
                eprintln!(
                    "warning: exact hard-abort worker `{path}` did not drop after its grace monitor failed"
                );
                continue;
            }
            worker.task.detach();
            if let Some(lease) = lease {
                let runtime = Arc::clone(self);
                let tree = Arc::clone(tree);
                let fallback = std::thread::Builder::new()
                    .name("moyai-agent-hard-abort-fallback".to_string())
                    .spawn(move || {
                        let runtime_handle = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .expect("failed to build hard-abort fallback runtime");
                        runtime_handle.block_on(runtime.settle_hard_aborted_worker(
                            &tree,
                            path,
                            owner,
                            lease,
                            "worker hard-aborted because the grace monitor was unavailable",
                        ));
                    });
                if let Ok(fallback) = fallback {
                    let _ = fallback.join();
                }
            }
        }
    }

    async fn abort_cancelled_workers(
        self: &Arc<Self>,
        tree: &Arc<AgentTreeRuntime>,
        captured: Vec<CapturedCancelledWorker>,
    ) {
        for (path, worker) in self.take_captured_workers(tree, captured) {
            let owner = worker.terminal_owner;
            let lease = owner
                .lease
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
            // The whole exact worker is dropped before durable Interrupted is committed. Tool,
            // process and filesystem guards therefore cannot mutate after the terminal owner wins.
            worker.task.abort_and_wait().await;
            if let Some(lease) = lease {
                self.settle_hard_aborted_worker(
                    tree,
                    path,
                    owner,
                    lease,
                    "worker exceeded the cooperative cancellation grace period",
                )
                .await;
            }
        }
    }

    async fn settle_hard_aborted_worker(
        self: &Arc<Self>,
        tree: &Arc<AgentTreeRuntime>,
        path: AgentPath,
        owner: AgentWorkerTerminalOwner,
        lease: AgentExecutionLease,
        activity: &str,
    ) {
        let wake = match owner.wake_cause {
            AgentExecutionWakeCause::ExplicitTask(history_item_id) => {
                AgentExecutionWakeTerminalOwner::ExplicitTask(history_item_id)
            }
            AgentExecutionWakeCause::OwnerResume(request_id) => {
                AgentExecutionWakeTerminalOwner::OwnerResume(request_id)
            }
        };
        let fallback_cause = match lease.run_control().cause() {
            Some(RunCancellationCause::Interruption(cause)) => cause,
            Some(RunCancellationCause::Failure(_))
            | Some(RunCancellationCause::Superseded)
            | None => TurnInterruptionCause::AgentInterrupted,
        };
        let requested_terminal = crate::session::DurableTurnTerminal {
            outcome: TurnTerminalOutcome::Interrupted {
                cause: fallback_cause,
            },
            final_response_id: None,
            tool_call_count: 0,
            failed_tool_count: 0,
            change_count: 0,
            metrics: Default::default(),
        };
        let settlement = loop {
            match self
                .session_service
                .settle_agent_execution_wake_with_terminal(
                    owner.session_id,
                    wake,
                    requested_terminal.clone(),
                ) {
                Ok(settlement) => break settlement,
                Err(error) => {
                    let _ = lease.set_activity(Some(format!(
                        "{activity}; retrying external durable terminal settlement after: {error}"
                    )));
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        };
        let (turn_id, terminal) = match settlement {
            AgentExecutionWakeTerminalSettlement::Applied {
                turn_id, terminal, ..
            }
            | AgentExecutionWakeTerminalSettlement::AlreadyTerminal { turn_id, terminal } => {
                (turn_id, terminal)
            }
            AgentExecutionWakeTerminalSettlement::BlockedByPendingDeferredCompletion {
                deferred_turn_id,
            } => {
                let _ = tree.control.release_unsettled_trigger_execution(
                        lease,
                        Some(format!(
                            "{activity}; durable wake remains blocked by deferred turn {deferred_turn_id}"
                        )),
                    );
                return;
            }
            AgentExecutionWakeTerminalSettlement::WakeUnavailable => {
                let scheduled = tree
                    .control
                    .retire_resolved_wake_execution(
                        lease,
                        Some(format!(
                            "{activity}; durable wake was already resolved by another owner"
                        )),
                    )
                    .unwrap_or_default();
                self.launch_scheduled_turns(tree, scheduled);
                return;
            }
        };

        let summary = RunSummary::from_terminal(owner.session_id, turn_id, terminal.clone());
        let durable_result: Result<RunSummary, AppRunError> = Ok(summary);
        let completed_content = self
            .final_child_result_content(&durable_result, None)
            .await
            .ok()
            .flatten();
        let mut status = match &terminal.outcome {
            TurnTerminalOutcome::Completed => InactiveAgentStatus::Completed(completed_content),
            TurnTerminalOutcome::Failed { error } => InactiveAgentStatus::Errored(error.clone()),
            TurnTerminalOutcome::Interrupted { .. } => InactiveAgentStatus::Interrupted,
        };
        let mut projected_activity = Some(format!(
            "{activity}; durable turn {turn_id} settled as {}",
            terminal.session_status().key()
        ));
        if let Ok(mut metadata) = tree.metadata.lock()
            && let Some(node) = metadata.get_mut(&path)
        {
            node.updated = true;
        }
        match self
            .store
            .session_repo()
            .agent_terminal_effects(owner.session_id, turn_id)
        {
            Ok(effects) => {
                if let Some(deferred) = effects.deferred.as_ref().filter(|deferred| {
                    deferred.state
                        == crate::storage::session_repo::DeferredAgentCompletionState::Pending
                }) {
                    status = InactiveAgentStatus::AwaitingDescendants(deferred.agent_turn_id);
                } else if effects.deferred.as_ref().is_some_and(|deferred| {
                    deferred.state
                        == crate::storage::session_repo::DeferredAgentCompletionState::Superseded
                }) {
                    let repository = self.store.session_repo();
                    let session_id = owner.session_id;
                    match tree.control.restore_current_owner_resume(&path, move || {
                        repository
                            .schedulable_owner_resume_request_id(session_id)
                            .map_err(|error| error.to_string())
                    }) {
                        Ok(scheduled) => self.launch_scheduled_turns(tree, scheduled),
                        Err(error) => {
                            projected_activity = Some(append_agent_activity(
                                projected_activity,
                                format!(
                                    "durable deferred owner-resume projection could not be restored: {error}"
                                ),
                            ));
                        }
                    }
                }
                for released in &effects.released_deferred_handoffs {
                    if let Err(error) = self.project_released_deferred_handoff(tree, released).await
                    {
                        projected_activity = Some(append_agent_activity(
                            projected_activity,
                            format!("released deferred completion could not be projected: {error}"),
                        ));
                    }
                }
                if !matches!(terminal.outcome, TurnTerminalOutcome::Interrupted { .. })
                    && let Some(handoff) = effects.completion_handoff.as_ref()
                    && let Err(error) = self.enqueue_completion_handoff(tree, &path, handoff)
                {
                    projected_activity = Some(append_agent_activity(
                        projected_activity,
                        format!("durable completion notice could not be enqueued: {error}"),
                    ));
                }
            }
            Err(error) => {
                projected_activity = Some(append_agent_activity(
                    projected_activity,
                    format!("durable terminal effects could not be read: {error}"),
                ));
            }
        }
        let scheduled = tree
            .control
            .complete_execution(lease, status, projected_activity)
            .unwrap_or_default();
        self.launch_scheduled_turns(tree, scheduled);
    }

    pub(crate) async fn begin_root_with_run_service(
        self: &Arc<Self>,
        session: &SessionContext,
        config: Arc<ResolvedTurnConfig>,
        confirmation: SharedConfirmationPrompt,
        run_control: RunControl,
        run_service: Arc<RunService>,
    ) -> Result<AgentRuntimeExecution, String> {
        self.begin_root_with_optional_run_service(
            session,
            config,
            confirmation,
            run_control,
            Some(run_service),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn begin_root(
        self: &Arc<Self>,
        session: &SessionContext,
        config: Arc<ResolvedTurnConfig>,
        confirmation: SharedConfirmationPrompt,
        run_control: RunControl,
    ) -> Result<AgentRuntimeExecution, String> {
        let run_service = self.test_run_service();
        self.begin_root_with_optional_run_service(
            session,
            config,
            confirmation,
            run_control,
            run_service,
        )
        .await
    }

    async fn begin_root_with_optional_run_service(
        self: &Arc<Self>,
        session: &SessionContext,
        config: Arc<ResolvedTurnConfig>,
        confirmation: SharedConfirmationPrompt,
        run_control: RunControl,
        run_service: Option<Arc<RunService>>,
    ) -> Result<AgentRuntimeExecution, String> {
        let effective_config = config.runtime_config();
        let root_session_id = session.session.id;
        bind_execution_confirmation(&confirmation);
        let existing = {
            let mut trees = self
                .trees
                .lock()
                .map_err(|_| "agent tree registry lock was poisoned".to_string())?;
            if let Some(tree) =
                reusable_tree_for_limits(&mut trees, root_session_id, effective_config)?
            {
                let lease = tree
                    .control
                    .try_acquire_root_execution(run_control.clone())
                    .map_err(agent_control_error)?;
                Some((tree, lease))
            } else {
                None
            }
        };
        let (tree, lease) = if let Some(existing) = existing {
            existing
        } else {
            // Durable restoration performs bounded storage work without holding the process-wide
            // tree registry. Revalidate after the await because another admission may have
            // installed the retained tree meanwhile.
            let durable_children = self.load_durable_children(root_session_id).await?;
            let mut trees = self
                .trees
                .lock()
                .map_err(|_| "agent tree registry lock was poisoned".to_string())?;
            if let Some(tree) =
                reusable_tree_for_limits(&mut trees, root_session_id, effective_config)?
            {
                let lease = tree
                    .control
                    .try_acquire_root_execution(run_control.clone())
                    .map_err(agent_control_error)?;
                (tree, lease)
            } else {
                let (control, lease) = AgentControl::with_root_control(
                    root_session_id,
                    effective_config.multi_agent.max_concurrent_agents,
                    run_control,
                )
                .map_err(agent_control_error)?;
                let limits = AgentTreeLimits::capture(effective_config);
                let tree = Arc::new(AgentTreeRuntime {
                    root_session_id,
                    control,
                    limits,
                    model_request_gate: Arc::new(tokio::sync::Semaphore::new(
                        limits.max_concurrent_model_requests,
                    )),
                    active_root_turn_owner: Mutex::new(None),
                    metadata: Mutex::new(HashMap::new()),
                });
                self.restore_durable_children(
                    &tree,
                    durable_children,
                    &config,
                    &session.workspace,
                    &confirmation,
                    &run_service,
                )?;
                trees.insert(root_session_id, tree.clone());
                (tree, lease)
            }
        };
        lease
            .set_status(ActiveAgentStatus::Running)
            .map_err(agent_control_error)?;
        let mut metadata = tree
            .metadata
            .lock()
            .map_err(|_| "agent metadata lock was poisoned".to_string())?;
        metadata.insert(
            AgentPath::root(),
            AgentNodeMetadata {
                task_name: "root".to_string(),
                task_preview: String::new(),
                config: Arc::clone(&config),
                workspace: session.workspace.clone(),
                confirmation: confirmation.clone(),
                run_service: run_service.clone(),
                updated: false,
                activity_owner: None,
            },
        );
        drop(metadata);
        let context = AgentRunContext {
            runtime: self.clone(),
            tree: tree.clone(),
            path: AgentPath::root(),
            session_id: root_session_id,
            wake_cause: None,
            execution: lease.scope(),
            turn_owner: Arc::new(OnceLock::new()),
            config,
            workspace: session.workspace.clone(),
            confirmation,
            run_service,
        };
        Ok(AgentRuntimeExecution {
            context,
            lease: Some(lease),
        })
    }

    pub(crate) fn begin_root_continuation_with_run_service(
        self: &Arc<Self>,
        root_session_id: SessionId,
        run_control: RunControl,
        confirmation: Option<SharedConfirmationPrompt>,
        run_service: Arc<RunService>,
    ) -> Result<AgentRuntimeContinuationOutcome, String> {
        self.begin_root_continuation_with_optional_run_service(
            root_session_id,
            run_control,
            confirmation,
            Some(run_service),
        )
    }

    #[cfg(test)]
    pub(crate) fn begin_root_continuation(
        self: &Arc<Self>,
        root_session_id: SessionId,
        run_control: RunControl,
        confirmation: Option<SharedConfirmationPrompt>,
    ) -> Result<AgentRuntimeContinuationOutcome, String> {
        let run_service = self.test_run_service();
        self.begin_root_continuation_with_optional_run_service(
            root_session_id,
            run_control,
            confirmation,
            run_service,
        )
    }

    fn begin_root_continuation_with_optional_run_service(
        self: &Arc<Self>,
        root_session_id: SessionId,
        run_control: RunControl,
        confirmation: Option<SharedConfirmationPrompt>,
        run_service: Option<Arc<RunService>>,
    ) -> Result<AgentRuntimeContinuationOutcome, String> {
        let tree = self
            .trees
            .lock()
            .map_err(|_| "agent tree registry lock was poisoned".to_string())?
            .get(&root_session_id)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "session {root_session_id} has no retained root task scope for continuation"
                )
            })?;
        let confirmation = confirmation.ok_or_else(|| {
            "root continuation requires a shared permission confirmation channel".to_string()
        })?;
        bind_execution_confirmation(&confirmation);
        let lease = match tree
            .control
            .try_acquire_root_continuation(run_control.clone())
            .map_err(agent_control_error)?
        {
            AgentRootContinuationOutcome::Admitted(lease) => lease,
            AgentRootContinuationOutcome::Blocked => {
                return Ok(AgentRuntimeContinuationOutcome::Blocked);
            }
            AgentRootContinuationOutcome::NotReady => {
                return Ok(AgentRuntimeContinuationOutcome::NotReady);
            }
            AgentRootContinuationOutcome::Invalid => {
                return Ok(AgentRuntimeContinuationOutcome::Invalid);
            }
        };
        let mut context = self.context_for_execution(&tree, &lease)?;
        context.confirmation = confirmation.clone();
        context.run_service = run_service.clone();
        if let Ok(mut metadata) = tree.metadata.lock()
            && let Some(root) = metadata.get_mut(&AgentPath::root())
        {
            root.confirmation = confirmation;
            root.run_service = run_service;
        }
        let continuation_control = lease.run_control();
        if let Err(error) = lease.set_status(ActiveAgentStatus::Running) {
            let message = agent_control_error(error);
            continuation_control.fail(message.clone());
            drop(lease);
            return Err(message);
        }
        Ok(AgentRuntimeContinuationOutcome::Admitted(
            AgentRuntimeExecution {
                context,
                lease: Some(lease),
            },
        ))
    }

    async fn load_durable_children(
        &self,
        root_session_id: SessionId,
    ) -> Result<Vec<DurableAgentChild>, String> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || load_durable_agent_children(&store, root_session_id))
            .await
            .map_err(|error| format!("agent tree rehydration worker failed: {error}"))?
    }

    fn restore_durable_children(
        &self,
        tree: &Arc<AgentTreeRuntime>,
        mut durable_children: Vec<DurableAgentChild>,
        config: &Arc<ResolvedTurnConfig>,
        workspace: &Workspace,
        confirmation: &SharedConfirmationPrompt,
        run_service: &Option<Arc<RunService>>,
    ) -> Result<(), String> {
        let durable_spawn_orders = durable_children
            .iter()
            .map(|child| (child.session_id, child.edge.spawn_order))
            .collect::<HashMap<_, _>>();
        durable_children.sort_by(|left, right| {
            let left_depth = left.edge.agent_path.matches('/').count();
            let right_depth = right.edge.agent_path.matches('/').count();
            left_depth
                .cmp(&right_depth)
                .then_with(|| left.edge.spawn_order.cmp(&right.edge.spawn_order))
                .then_with(|| {
                    left.edge
                        .child_session_id
                        .to_string()
                        .cmp(&right.edge.child_session_id.to_string())
                })
        });
        let mut restored_metadata = Vec::with_capacity(durable_children.len());
        let mut session_paths = HashMap::from([(tree.root_session_id, AgentPath::root())]);
        for durable_child in durable_children {
            let DurableAgentChild {
                edge,
                session_id,
                session_status,
                active_turn_id: _,
                pending_deferred_turn_id,
                pending_trigger_history_item_id,
                pending_trigger_schedule_ready,
                pending_owner_resume_request_id,
                task_preview,
                result,
                interruption_cause,
            } = durable_child;
            if edge.root_session_id != tree.root_session_id {
                return Err(format!(
                    "spawn edge for child {} belongs to root {}, expected {}",
                    session_id, edge.root_session_id, tree.root_session_id
                ));
            }
            let parent_path = session_paths
                .get(&edge.parent_session_id)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "spawn edge {} references parent session {} before that parent is reachable from root {}",
                        edge.agent_path, edge.parent_session_id, tree.root_session_id
                    )
                })?;
            let expected_path = parent_path.join(&edge.task_name)?;
            let durable_path = AgentPath::try_from(edge.agent_path.as_str())?;
            if expected_path != durable_path {
                return Err(format!(
                    "spawn edge path {} does not match parent/task path {}",
                    durable_path, expected_path
                ));
            }
            let status =
                rehydrated_agent_state(session_id, session_status, result, interruption_cause)?;
            let inactive_status = if let Some(deferred_turn_id) = pending_deferred_turn_id {
                InactiveAgentStatus::AwaitingDescendants(deferred_turn_id)
            } else if (pending_trigger_history_item_id.is_some()
                || pending_owner_resume_request_id.is_some())
                && session_status == SessionStatus::Idle
            {
                InactiveAgentStatus::PendingInit
            } else {
                inactive_agent_status(status, None)?
            };
            let snapshot = tree
                .control
                .restore_inactive_child_with_order(
                    &parent_path,
                    &edge.task_name,
                    session_id,
                    inactive_status,
                    None,
                    durable_spawn_orders.get(&session_id).copied(),
                )
                .map_err(agent_control_error)?;
            if let Some(history_item_id) = pending_trigger_history_item_id {
                tree.control
                    .restore_pending_mail(
                        &snapshot.path,
                        history_item_id,
                        pending_trigger_schedule_ready,
                    )
                    .map_err(agent_control_error)?;
            }
            if let Some(request_id) = pending_owner_resume_request_id {
                tree.control
                    .restore_pending_owner_resume(&snapshot.path, request_id)
                    .map_err(agent_control_error)?;
            }
            session_paths.insert(session_id, snapshot.path.clone());
            restored_metadata.push((
                snapshot.path,
                AgentNodeMetadata {
                    task_name: edge.task_name,
                    task_preview,
                    config: Arc::clone(config),
                    workspace: workspace.clone(),
                    confirmation: confirmation.clone(),
                    run_service: run_service.clone(),
                    updated: false,
                    activity_owner: None,
                },
            ));
        }
        tree.metadata
            .lock()
            .map_err(|_| "agent metadata lock was poisoned".to_string())?
            .extend(restored_metadata);
        Ok(())
    }

    pub async fn durable_activity_records(
        &self,
        root_session_id: SessionId,
    ) -> Result<Vec<AgentActivityRecord>, String> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || load_durable_agent_children(&store, root_session_id))
            .await
            .map_err(|error| format!("durable agent projection worker failed: {error}"))??
            .into_iter()
            .map(|child| {
                let status = durable_projection_status(
                    child.session_id,
                    child.session_status,
                    child.result,
                    child.interruption_cause,
                );
                let status = if child.pending_deferred_turn_id.is_some() {
                    AgentStatus::AwaitingDescendants
                } else {
                    status
                };
                let can_interrupt =
                    matches!(&status, AgentStatus::Running) && child.active_turn_id.is_some();
                Ok(AgentActivityRecord {
                    agent_path: child.edge.agent_path,
                    session_id: child.session_id,
                    task_name: child.edge.task_name,
                    task_preview: preview(&child.task_preview, 240),
                    result_preview: agent_status_result(&status),
                    status,
                    current_activity: String::new(),
                    started_order: child.edge.spawn_order,
                    updated: false,
                    is_current_turn: false,
                    active_turn_id: child.active_turn_id,
                    can_interrupt,
                })
            })
            .collect()
    }

    pub(crate) fn complete_root(
        self: &Arc<Self>,
        execution: AgentRuntimeExecution,
        result: &Result<RunSummary, AppRunError>,
        cancellation_cause: Option<RunCancellationCause>,
    ) {
        let tree = execution.context.tree.clone();
        let terminal_cause = effective_run_terminal_cause(result, cancellation_cause);
        let status = agent_status_from_terminal_result(result, terminal_cause.as_ref(), None);
        if let Ok(scheduled) = execution.complete(status) {
            self.launch_scheduled_turns(&tree, scheduled);
        }
    }

    pub(crate) fn release_unadmitted_root_continuation(
        self: &Arc<Self>,
        execution: AgentRuntimeExecution,
    ) -> Result<(), String> {
        let tree = execution.context.tree.clone();
        let scheduled = execution.complete(AgentStatus::Completed(None))?;
        self.launch_scheduled_turns(&tree, scheduled);
        Ok(())
    }

    pub fn activity_records(&self, root_session_id: SessionId) -> Vec<AgentActivityRecord> {
        let Ok(trees) = self.trees.lock() else {
            return Vec::new();
        };
        let Some(tree) = trees.get(&root_session_id) else {
            return Vec::new();
        };
        let Ok(snapshot) = tree.control.snapshot() else {
            return Vec::new();
        };
        let active_owner = tree
            .active_root_turn_owner
            .lock()
            .ok()
            .and_then(|owner| *owner);
        let Ok(metadata) = tree.metadata.lock() else {
            return Vec::new();
        };
        snapshot
            .agents
            .into_iter()
            .filter(|agent| !agent.path.is_root())
            .map(|agent| {
                let node = metadata.get(&agent.path);
                let projected_status = if agent.is_active {
                    match &agent.status {
                        AgentStatus::PendingInit => AgentStatus::PendingInit,
                        _ => AgentStatus::Running,
                    }
                } else {
                    agent.status.clone()
                };
                let active_turn_id = self.store.active_runs().active_turn_id(agent.session_id);
                let can_interrupt =
                    matches!(&projected_status, AgentStatus::Running) && active_turn_id.is_some();
                AgentActivityRecord {
                    agent_path: agent.path.to_string(),
                    session_id: agent.session_id,
                    task_name: node.map(|node| node.task_name.clone()).unwrap_or_default(),
                    task_preview: node
                        .map(|node| preview(&node.task_preview, 240))
                        .unwrap_or_default(),
                    status: projected_status,
                    current_activity: agent.last_activity.unwrap_or_default(),
                    result_preview: agent_status_result(&agent.status),
                    started_order: agent.spawn_order,
                    updated: node.is_some_and(|node| node.updated),
                    is_current_turn: node
                        .and_then(|node| node.activity_owner)
                        .is_some_and(|owner| Some(owner) == active_owner),
                    active_turn_id,
                    can_interrupt,
                }
            })
            .collect()
    }

    pub fn cancel_tree_for_session(
        self: &Arc<Self>,
        session_id: SessionId,
        root_cause: TurnInterruptionCause,
    ) -> bool {
        let Ok(trees) = self.trees.lock() else {
            return false;
        };
        let tree = trees.get(&session_id).cloned().or_else(|| {
            trees.values().find_map(|tree| {
                tree.control
                    .path_for_session(session_id)
                    .ok()
                    .flatten()
                    .map(|_| tree.clone())
            })
        });
        if let Some(tree) = tree {
            let accepted = tree.control.interrupt_tree(root_cause);
            self.schedule_cancelled_worker_abort(&tree);
            accepted
        } else {
            false
        }
    }

    /// Reuses the ordinary cancelled-worker grace monitor for one exact UI-selected child.
    ///
    /// Durable lineage and turn identity are validated by `RunService` before cancellation. This
    /// local check prevents a stale process tree from scheduling an abort for a sibling or a
    /// replacement turn. An absent tree/turn means durable fallback already owns settlement.
    pub(crate) fn schedule_cancelled_agent_worker_abort(
        self: &Arc<Self>,
        root_session_id: SessionId,
        agent_path: &str,
        child_session_id: SessionId,
        expected_turn_id: TurnId,
    ) {
        let Ok(path) = AgentPath::try_from(agent_path) else {
            return;
        };
        if path.is_root() {
            return;
        }
        let tree = self
            .trees
            .lock()
            .ok()
            .and_then(|trees| trees.get(&root_session_id).cloned());
        let Some(tree) = tree else {
            return;
        };
        if tree
            .control
            .path_for_session(child_session_id)
            .ok()
            .flatten()
            .as_ref()
            != Some(&path)
            || self.store.active_runs().active_turn_id(child_session_id) != Some(expected_turn_id)
        {
            return;
        }
        self.schedule_cancelled_worker_abort(&tree);
    }

    pub async fn wait_for_tree_quiescence(&self, root_session_id: SessionId) -> Result<(), String> {
        let tree = self
            .trees
            .lock()
            .map_err(|_| "agent tree registry lock was poisoned".to_string())?
            .get(&root_session_id)
            .cloned();
        if let Some(tree) = tree {
            wait_for_control_quiescence(&tree.control)
                .await
                .map_err(agent_control_error)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn has_tree_for_session(&self, root_session_id: SessionId) -> bool {
        self.trees
            .lock()
            .is_ok_and(|trees| trees.contains_key(&root_session_id))
    }

    async fn spawn_agent(
        self: &Arc<Self>,
        caller: &AgentRunContext,
        task_name: &str,
        message: String,
        fork_turns: AgentForkTurns,
        activity_id: String,
    ) -> Result<AgentSnapshot, String> {
        if message.trim().is_empty() {
            return Err("spawn_agent requires a non-empty message".to_string());
        }
        let activity_owner = caller.durable_turn_owner()?;
        let child_path = caller.path.join(task_name)?;
        if caller
            .tree
            .control
            .list_agents(Some(&child_path))
            .map_err(agent_control_error)?
            .into_iter()
            .any(|agent| agent.path == child_path)
        {
            return Err(format!(
                "agent `{child_path}` already exists; use followup_task to reuse it"
            ));
        }
        let child_config = caller.effective_config();
        let child_session_id = SessionId::new();
        let child_draft = NewSession {
            project_id: caller.workspace.project_id,
            title: task_name.to_string(),
            cwd: caller.workspace.cwd.clone(),
            model: child_config.model.model.clone(),
            base_url: child_config.model.base_url.clone(),
            access_mode: child_config.permissions.access_mode,
        };

        let initial_task = InterAgentCommunication {
            author: caller.path.to_string(),
            recipient: child_path.to_string(),
            content: render_inter_agent_message(
                InterAgentMessageType::NewTask,
                child_path.as_str(),
                caller.path.as_str(),
                &message,
            ),
            trigger_turn: true,
        };
        let context_fork = match fork_turns {
            AgentForkTurns::None => SpawnContextFork::None,
            AgentForkTurns::All => SpawnContextFork::All,
            AgentForkTurns::Recent(turns) => SpawnContextFork::Recent(turns),
        };
        let spawn_commit = caller.tree.control.commit_spawn(
            &caller.execution,
            &caller.path,
            task_name,
            child_session_id,
            Some("Starting assigned task".to_string()),
            || {
                self.store
                    .session_repo()
                    .create_agent_spawn_with_initial_task_for_caller_turn(
                        caller.tree.root_session_id,
                        activity_owner.session_id,
                        child_session_id,
                        child_draft,
                        child_path.as_str(),
                        task_name,
                        activity_owner.admission_id,
                        activity_owner.turn_id,
                        context_fork,
                        initial_task,
                    )
                    .map(|stored| {
                        let spawn_order = stored.edge.spawn_order;
                        (stored, spawn_order)
                    })
                    .map_err(|error| error.to_string())
            },
        );
        let (stored_spawn, snapshot, lease) = match spawn_commit {
            Ok(committed) => committed,
            Err(error) => return Err(agent_control_error(error)),
        };
        let lease = match lease
            .try_bind_trigger_history_item_id(stored_spawn.initial_task_history_item_id)
        {
            Ok(lease) => lease,
            Err(lease) => {
                return Err(self.retain_failed_spawn(
                    &caller.tree,
                    lease,
                    "initial child execution already carried a different task trigger".to_string(),
                ));
            }
        };
        if stored_spawn.child_session.id != child_session_id {
            return Err(self.retain_failed_spawn(
                &caller.tree,
                lease,
                format!(
                    "durable child session {} does not match reserved child session {child_session_id}",
                    stored_spawn.child_session.id
                ),
            ));
        }
        let child_session_id = stored_spawn.child_session.id;
        let metadata_insert = caller.tree.metadata.lock().map(|mut metadata| {
            metadata.insert(
                child_path.clone(),
                AgentNodeMetadata {
                    task_name: task_name.to_string(),
                    task_preview: message.clone(),
                    config: Arc::clone(&caller.config),
                    workspace: caller.workspace.clone(),
                    confirmation: caller.confirmation.clone(),
                    run_service: caller.run_service.clone(),
                    updated: false,
                    activity_owner: Some(activity_owner),
                },
            );
        });
        if metadata_insert.is_err() {
            let error = "agent metadata lock was poisoned".to_string();
            return Err(self.retain_failed_spawn(&caller.tree, lease, error));
        }
        let child_context = AgentRunContext {
            runtime: self.clone(),
            tree: caller.tree.clone(),
            path: child_path.clone(),
            session_id: child_session_id,
            wake_cause: lease.wake_cause(),
            execution: lease.scope(),
            turn_owner: Arc::new(OnceLock::new()),
            config: Arc::clone(&caller.config),
            workspace: caller.workspace.clone(),
            confirmation: caller.confirmation.clone(),
            run_service: caller.run_service.clone(),
        };
        if let Err(failure) = self.launch_agent_turn(child_context, lease, String::new()) {
            let AgentLaunchFailure { message, lease, .. } = failure;
            return Err(self.retain_failed_spawn(&caller.tree, lease, message));
        }
        // Launch success is the spawn commit point. Activity is a best-effort
        // client projection and must not make the caller observe a failed spawn
        // after the child execution has already started.
        if let Err(error) = self.append_activity(
            caller,
            &activity_id,
            child_session_id,
            &child_path,
            SubAgentActivityKind::Started,
        ) {
            caller.set_activity(format!(
                "child {child_path} started, but its activity projection failed: {error}"
            ));
        }
        Ok(snapshot)
    }

    fn retain_failed_spawn(
        self: &Arc<Self>,
        tree: &Arc<AgentTreeRuntime>,
        lease: AgentExecutionLease,
        failure: String,
    ) -> String {
        match self.settle_pre_admission_execution(tree, lease, Some(failure.clone())) {
            Ok(scheduled) => {
                self.launch_scheduled_turns(tree, scheduled);
                failure
            }
            Err(settlement_error) => {
                format!(
                    "{failure}; additionally failed to settle the durable child: {settlement_error}"
                )
            }
        }
    }

    fn settle_pre_admission_execution(
        self: &Arc<Self>,
        tree: &Arc<AgentTreeRuntime>,
        lease: AgentExecutionLease,
        fallback_error: Option<String>,
    ) -> Result<Vec<AgentExecutionLease>, String> {
        let path = lease.path().clone();
        let session_id = tree
            .control
            .list_agents(Some(&path))
            .map_err(agent_control_error)?
            .into_iter()
            .find(|agent| agent.path == path)
            .map(|agent| agent.session_id)
            .ok_or_else(|| format!("agent `{path}` was not found"))?;
        let wake_cause = lease.wake_cause().ok_or_else(|| {
            format!("pre-admission child `{path}` has no canonical wake identity")
        })?;
        let (terminal, inactive_status, base_activity) =
            pre_admission_terminal(&lease, fallback_error);
        let committed = tree.control.commit_pending_trigger_terminal(
            &lease,
            Some("Durable explicit trigger remains blocked by its deferred owner".to_string()),
            || {
                match wake_cause {
                    AgentExecutionWakeCause::ExplicitTask(expected_history_item_id) => self
                        .session_service
                        .settle_pending_agent_trigger_with_terminal(
                            session_id,
                            expected_history_item_id,
                            terminal,
                        ),
                    AgentExecutionWakeCause::OwnerResume(expected_request_id) => self
                        .session_service
                        .settle_pending_owner_resume_with_terminal(
                            session_id,
                            expected_request_id,
                            terminal,
                        ),
                }
                .map_err(|error| error.to_string())
                .and_then(|settlement| match settlement {
                    PendingAgentTriggerSettlement::Applied { turn_id, handoff } => {
                        Ok(PendingTriggerTerminalCommit::Applied((turn_id, handoff)))
                    }
                    PendingAgentTriggerSettlement::BlockedByPendingDeferredCompletion {
                        deferred_turn_id,
                    } => Ok(
                        PendingTriggerTerminalCommit::BlockedByPendingDeferredCompletion {
                            deferred_turn_id,
                        },
                    ),
                    PendingAgentTriggerSettlement::WakeOwnedOrResolved => {
                        Ok(PendingTriggerTerminalCommit::WakeOwnedOrResolved)
                    }
                })
            },
        );
        let (turn_id, handoff) = match committed {
            Ok(PendingTriggerTerminalCommit::Applied(committed)) => committed,
            Ok(PendingTriggerTerminalCommit::WakeOwnedOrResolved) => {
                return tree
                    .control
                    .retire_resolved_wake_execution(
                        lease,
                        Some(
                            "Local child execution retired because its durable wake is already owned or resolved"
                                .to_string(),
                        ),
                    )
                    .map_err(agent_control_error);
            }
            Ok(PendingTriggerTerminalCommit::BlockedByPendingDeferredCompletion { .. }) => {
                drop(lease);
                return Ok(Vec::new());
            }
            Err(error) => {
                let durable_error = agent_control_error(error);
                tree
                    .control
                    .release_unsettled_trigger_execution(
                        lease,
                        Some(
                            "Durable pre-admission settlement failed; the canonical trigger remains recoverable"
                                .to_string(),
                        ),
                    )
                    .map_err(agent_control_error)?;
                return Err(durable_error);
            }
        };
        let mut activity = base_activity;
        if let Some(handoff) = handoff
            && let Err(error) = self.enqueue_completion_handoff(tree, &path, &handoff)
        {
            activity = Some(match activity {
                Some(activity) => {
                    format!("{activity}; durable completion notice could not be enqueued: {error}")
                }
                None => format!("durable completion notice could not be enqueued: {error}"),
            });
        }
        tree.control
            .complete_execution(
                lease,
                inactive_status,
                activity.or_else(|| Some(format!("Durably settled before turn {turn_id} ran"))),
            )
            .map_err(agent_control_error)
    }

    fn enqueue_completion_handoff(
        self: &Arc<Self>,
        tree: &Arc<AgentTreeRuntime>,
        child_path: &AgentPath,
        handoff: &StoredAgentCompletionHandoff,
    ) -> Result<(), String> {
        let scheduled = self.project_completion_handoff(tree, child_path, handoff)?;
        self.launch_scheduled_turns(tree, scheduled);
        Ok(())
    }

    fn project_completion_handoff(
        &self,
        tree: &Arc<AgentTreeRuntime>,
        child_path: &AgentPath,
        handoff: &StoredAgentCompletionHandoff,
    ) -> Result<Vec<AgentExecutionLease>, String> {
        let expected_parent = child_path.parent().ok_or_else(|| {
            format!("root agent `{child_path}` cannot deliver a child completion handoff")
        })?;
        if handoff.parent_agent_path != expected_parent {
            return Err(format!(
                "durable completion parent {} does not match immediate parent {expected_parent}",
                handoff.parent_agent_path
            ));
        }
        let parent = tree
            .control
            .list_agents(Some(&expected_parent))
            .map_err(agent_control_error)?
            .into_iter()
            .find(|agent| agent.path == expected_parent);
        let Some(parent) = parent else {
            return Ok(Vec::new());
        };
        if parent.session_id != handoff.parent_session_id {
            return Err(format!(
                "durable completion parent session {} does not match live parent {}",
                handoff.parent_session_id, parent.session_id
            ));
        }
        if matches!(parent.status, AgentStatus::Shutdown) {
            return Ok(Vec::new());
        }
        // Storage contributes exact release evidence and AgentControl matches it against the live
        // AwaitingDescendants generation under the same mail/state lock. OwnerResume is independent
        // scheduler state and is read inside that delivery fence.
        let repository = self.store.session_repo();
        let parent_session_id = handoff.parent_session_id;
        let parent_is_root = expected_parent.is_root();
        let history_item_id = handoff.history_item_id;
        let delivery = tree.control.commit_and_enqueue_completion_handoff(
            child_path,
            &expected_parent,
            handoff.released_owner_deferred_turn_id,
            move || {
                let current_owner_resume_request_id = repository
                    .schedulable_owner_resume_request_id(parent_session_id)
                    .map_err(|error| error.to_string())?;
                if parent_is_root && current_owner_resume_request_id.is_some() {
                    return Err(format!(
                        "root completion handoff {history_item_id} found an unexpected current OwnerResume identity"
                    ));
                }
                Ok(AgentMailCommit {
                    history_item_id,
                    schedule_turn: false,
                    owner_resume_request_id: current_owner_resume_request_id,
                })
            },
        );
        match delivery {
            Ok(delivery) => scheduled_mail_delivery(delivery),
            Err(delivery_error) => {
                tree
                    .control
                    .restore_released_owner_wake(
                        &expected_parent,
                        handoff.released_owner_deferred_turn_id,
                        {
                            let repository = self.store.session_repo();
                            move || {
                                let current = repository
                                    .schedulable_owner_resume_request_id(parent_session_id)
                                    .map_err(|error| error.to_string())?;
                                if parent_is_root && current.is_some() {
                                    return Err(format!(
                                        "root completion handoff {history_item_id} found an unexpected current OwnerResume identity"
                                    ));
                                }
                                Ok(current)
                            }
                        },
                    )
                    .map_err(|restore_error| {
                        format!(
                            "{}; durable released owner wake could not be restored after mail projection failure: {}",
                            agent_control_error(delivery_error),
                            agent_control_error(restore_error),
                        )
                    })
            }
        }
    }

    async fn send_message(
        self: &Arc<Self>,
        caller: &AgentRunContext,
        target: &str,
        message: String,
        trigger_turn: bool,
        activity_id: String,
    ) -> Result<AgentPath, String> {
        if message.trim().is_empty() {
            return Err("agent message must not be empty".to_string());
        }
        let caller_owner = caller.durable_turn_owner()?;
        if caller.tree.control.tree_is_cancelled() {
            return Err("the agent tree has been cancelled".to_string());
        }
        let recipient_path = caller.resolve_agent_target(target)?;
        if trigger_turn && recipient_path.is_root() {
            return Err("follow-up tasks cannot target the root agent".to_string());
        }
        let recipient = caller
            .tree
            .control
            .list_agents(Some(&recipient_path))
            .map_err(agent_control_error)?
            .into_iter()
            .find(|agent| agent.path == recipient_path)
            .ok_or_else(|| format!("agent `{recipient_path}` was not found"))?;
        let message_type = if trigger_turn {
            InterAgentMessageType::NewTask
        } else {
            InterAgentMessageType::Message
        };
        let communication = InterAgentCommunication {
            author: caller.path.to_string(),
            recipient: recipient_path.to_string(),
            content: render_inter_agent_message(
                message_type,
                recipient_path.as_str(),
                caller.path.as_str(),
                &message,
            ),
            trigger_turn,
        };
        // A follow-up races legitimately with the recipient's terminal commit:
        // the live projection can still say Running after storage has closed
        // that admission. Let the recipient transaction classify it as
        // current-turn mail or schedule it for the next turn. A non-triggering
        // message still requires the observed running admission.
        let require_active_recipient =
            !trigger_turn && matches!(recipient.status, AgentStatus::Running);
        let max_concurrent_agents = caller
            .tree
            .control
            .snapshot()
            .map_err(agent_control_error)?
            .max_concurrent_agents;
        let delivery = caller
            .tree
            .control
            .commit_and_enqueue_mail_with_capacity(
                &caller.execution,
                &caller.path,
                &recipient_path,
                trigger_turn,
                |ready_turn_capacity_granted| {
                    self.append_communication(
                        caller_owner,
                        recipient.session_id,
                        communication,
                        require_active_recipient,
                        ready_turn_capacity_granted,
                    )
                    .map_err(|error| match error {
                        crate::error::StorageError::AgentCapacityUnavailable { .. } => {
                            AgentControlError::AgentLimitReached {
                                max_concurrent_agents,
                            }
                        }
                        crate::error::StorageError::AgentMailboxFull { capacity, .. } => {
                            AgentControlError::MailboxFull {
                                recipient: recipient_path.clone(),
                                capacity,
                            }
                        }
                        error => AgentControlError::DurableMailboxCommit(error.to_string()),
                    })
                },
            )
            .map_err(agent_control_error)?;
        let scheduled = scheduled_mail_delivery(delivery)?;
        if recipient_path.is_root() {
            let _ = self.mark_activity_owner(caller, &caller.path);
        } else {
            let _ = self.append_activity(
                caller,
                &activity_id,
                recipient.session_id,
                &recipient_path,
                SubAgentActivityKind::Interacted,
            );
        }
        self.launch_scheduled_turns(&caller.tree, scheduled);
        Ok(recipient_path)
    }

    fn interrupt_agent(
        self: &Arc<Self>,
        caller: &AgentRunContext,
        target: &str,
        activity_id: String,
    ) -> Result<(AgentPath, AgentStatus), String> {
        let target_path = caller.resolve_agent_target(target)?;
        if target_path.is_root() {
            return Err("root is not a spawned agent".to_string());
        }
        if target_path == caller.path {
            return Err("an agent cannot interrupt itself".to_string());
        }
        let target = caller
            .tree
            .control
            .capture_interrupt_target(&caller.execution, &target_path)
            .map_err(agent_control_error)?;
        let previous_status = target.status().clone();
        caller
            .tree
            .control
            .commit_and_interrupt_captured(&caller.execution, &target, || {
                self.append_activity(
                    caller,
                    &activity_id,
                    target.session_id(),
                    target.path(),
                    SubAgentActivityKind::Interrupted,
                )
            })
            .map_err(agent_control_error)?;
        self.schedule_cancelled_worker_abort(&caller.tree);
        Ok((target_path, previous_status))
    }

    fn launch_agent_turn(
        self: &Arc<Self>,
        context: AgentRunContext,
        lease: AgentExecutionLease,
        prompt: String,
    ) -> Result<(), AgentLaunchFailure> {
        let Some(run_service) = context.run_service.clone() else {
            return Err(AgentLaunchFailure {
                message: "agent execution has no captured run service".to_string(),
                context,
                lease,
            });
        };
        let root_session_id = context.tree.root_session_id;
        let path = context.path.clone();
        let generation = match self.reserve_worker_generation(root_session_id, &path) {
            Ok(generation) => generation,
            Err(message) => {
                return Err(AgentLaunchFailure {
                    message,
                    context,
                    lease,
                });
            }
        };
        let lease_owner = Arc::new(Mutex::new(Some(lease)));
        let wake_cause = lease_owner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .and_then(AgentExecutionLease::wake_cause)
            .expect("spawned agent worker must retain one canonical wake identity");
        let terminal_owner = AgentWorkerTerminalOwner {
            session_id: context.session_id,
            wake_cause,
            lease: Arc::clone(&lease_owner),
        };
        let runtime = self.clone();
        let launch_state = Arc::new(Mutex::new(Some((context, prompt))));
        let worker_state = launch_state.clone();
        let worker_lease = Arc::clone(&lease_owner);
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let completion_runtime = Arc::downgrade(self);
        let completion_path = path.clone();
        let worker = match self.worker_runtime.spawn(generation, move || async move {
            if start_rx.await.is_err() {
                return;
            }
            let _completion = AgentWorkerCompletion {
                runtime: completion_runtime,
                root_session_id,
                path: completion_path,
                generation,
            };
            let Some((context, prompt)) =
                worker_state.lock().ok().and_then(|mut state| state.take())
            else {
                return;
            };
            let activation = worker_lease
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .map(activate_child_execution);
            if !matches!(activation, Some(Ok(()))) {
                let lease = worker_lease
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                let scheduled = lease.and_then(|lease| {
                    runtime
                        .settle_pre_admission_execution(&context.tree, lease, None)
                        .ok()
                });
                if let Some(scheduled) = scheduled {
                    runtime.launch_scheduled_turns(&context.tree, scheduled);
                }
                return;
            }
            let mut confirmation = context.confirmation_prompt();
            let mut renderer = AgentEventRenderer;
            let run_context = context.clone();
            let run_control = worker_lease
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .map(AgentExecutionLease::run_control)
                .expect("active agent worker must retain its execution lease");
            let request_run_control = run_control.clone();
            let config = runtime
                .materialize_context_config_and_sync_session(&run_context)
                .await;
            let result = match config {
                Ok(config) => {
                    // The non-cloneable lease still owns its marker after preflight. Recheck
                    // cancellation immediately before RunService admits the durable turn;
                    // RunService publishes Running only after that admission exists.
                    if child_completion_before_run_admission(&request_run_control).is_some() {
                        if let Some(lease) = worker_lease
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .take()
                            && let Ok(scheduled) =
                                runtime.settle_pre_admission_execution(&context.tree, lease, None)
                        {
                            runtime.launch_scheduled_turns(&context.tree, scheduled);
                        }
                        return;
                    }
                    let request = RunRequest {
                        prompt,
                        session_id: Some(run_context.session_id),
                        continue_last: false,
                        title: None,
                        cwd: run_context.workspace.cwd.clone(),
                        config: RunConfigInput::Resolved(config),
                        output_mode: OutputMode::Human,
                        show_reasoning_summary: false,
                        prompt_dispatch: None,
                        editor_context: None,
                        review_request: None,
                        image_paths: Vec::new(),
                        run_control: request_run_control,
                        session_access_mode_adoption: None,
                        agent_confirmation: Some(run_context.confirmation_prompt()),
                        agent_context: Some(run_context),
                    };
                    match run_service
                        .execute(AppCommand::Run(request), &mut renderer, &mut confirmation)
                        .await
                    {
                        Ok(crate::app::AppCommandOutcome::Turn(summary)) => Ok(summary),
                        Ok(crate::app::AppCommandOutcome::ControlCompleted) => {
                            Err(AppRunError::Message(
                                "an admitted child turn completed as a control command".to_string(),
                            ))
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(error) => {
                    if let Some(lease) = worker_lease
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take()
                        && let Ok(scheduled) = runtime.settle_pre_admission_execution(
                            &context.tree,
                            lease,
                            Some(error),
                        )
                    {
                        runtime.launch_scheduled_turns(&context.tree, scheduled);
                    }
                    return;
                }
            };
            if !context.has_durable_turn_owner() {
                let fallback_error = result.as_ref().err().map(ToString::to_string).or_else(|| {
                    Some("sub-agent run returned before binding its durable turn owner".to_string())
                });
                if let Some(lease) = worker_lease
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                    && let Ok(scheduled) =
                        runtime.settle_pre_admission_execution(&context.tree, lease, fallback_error)
                {
                    runtime.launch_scheduled_turns(&context.tree, scheduled);
                }
                return;
            }
            let cancellation_cause = run_control.cause();
            let completion = runtime
                .finish_agent_turn(&context, &result, cancellation_cause)
                .await;
            let AgentTurnCompletion {
                status,
                activity,
                awaiting_deferred_turn_id,
            } = completion;
            let status =
                inactive_agent_status(status, awaiting_deferred_turn_id).unwrap_or_else(|error| {
                    InactiveAgentStatus::Errored(format!(
                        "invalid child terminal lifecycle handoff: {error}"
                    ))
                });
            let scheduled = worker_lease
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .and_then(|lease| {
                    context
                        .tree
                        .control
                        .complete_execution(lease, status, activity)
                        .ok()
                })
                .unwrap_or_default();
            runtime.launch_scheduled_turns(&context.tree, scheduled);
        }) {
            Ok(worker) => worker,
            Err(message) => {
                let (context, _) = launch_state
                    .lock()
                    .ok()
                    .and_then(|mut state| state.take())
                    .expect("failed local task launch must retain its captured agent state");
                let lease = lease_owner
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take()
                    .expect("failed local task launch must retain its execution lease");
                return Err(AgentLaunchFailure {
                    message,
                    context,
                    lease,
                });
            }
        };
        if let Err((message, worker)) =
            self.install_worker(root_session_id, path.clone(), worker, terminal_owner)
        {
            worker.abort();
            let (context, _) = launch_state
                .lock()
                .ok()
                .and_then(|mut state| state.take())
                .expect("failed worker installation must retain its captured agent state");
            let lease = lease_owner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .expect("failed worker installation must retain its execution lease");
            return Err(AgentLaunchFailure {
                message,
                context,
                lease,
            });
        }
        if start_tx.send(()).is_err() {
            if let Ok(mut workers) = self.workers.lock() {
                workers.tasks.remove(&(root_session_id, path));
            }
            let (context, _) = launch_state
                .lock()
                .ok()
                .and_then(|mut state| state.take())
                .expect("failed worker start must retain its captured agent state");
            let lease = lease_owner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
                .expect("failed worker start must retain its execution lease");
            return Err(AgentLaunchFailure {
                message: "agent worker runtime stopped before task start".to_string(),
                context,
                lease,
            });
        }
        Ok(())
    }

    async fn materialize_context_config_and_sync_session(
        &self,
        context: &AgentRunContext,
    ) -> Result<ResolvedConfig, String> {
        let config = context.effective_config();
        if context.is_sub_agent() {
            self.session_service
                .update_session_settings(
                    context.session_id,
                    SessionSettingsPatch {
                        access_mode: Some(config.permissions.access_mode),
                        ..SessionSettingsPatch::default()
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(config)
    }

    async fn finish_agent_turn(
        self: &Arc<Self>,
        context: &AgentRunContext,
        result: &Result<RunSummary, AppRunError>,
        cancellation_cause: Option<RunCancellationCause>,
    ) -> AgentTurnCompletion {
        let terminal_cause = effective_run_terminal_cause(result, cancellation_cause);
        let final_content = self
            .final_child_result_content(result, terminal_cause.as_ref())
            .await;
        let result_read_error = final_content.as_ref().err().cloned();
        let final_content = final_content.ok().flatten();
        let mut status = agent_status_from_terminal_result(
            result,
            terminal_cause.as_ref(),
            final_content.clone(),
        );
        if let Ok(mut metadata) = context.tree.metadata.lock()
            && let Some(node) = metadata.get_mut(&context.path)
        {
            node.updated = true;
        }
        let mut activity = result_read_error
            .map(|error| format!("durable child result could not be read: {error}"));
        let mut awaiting_deferred_turn_id = None;
        let Ok(summary) = result else {
            return AgentTurnCompletion {
                status,
                activity,
                awaiting_deferred_turn_id,
            };
        };
        if !matches!(
            summary.status(),
            SessionStatus::Completed | SessionStatus::Failed | SessionStatus::Cancelled
        ) {
            return AgentTurnCompletion {
                status,
                activity,
                awaiting_deferred_turn_id,
            };
        }
        let effects = match self
            .store
            .session_repo()
            .agent_terminal_effects(summary.session_id(), summary.turn_id())
        {
            Ok(effects) => effects,
            Err(error) => {
                return AgentTurnCompletion {
                    status,
                    activity: Some(append_agent_activity(
                        activity,
                        format!("durable child terminal effects could not be read: {error}"),
                    )),
                    awaiting_deferred_turn_id,
                };
            }
        };
        if let Some(deferred) = effects.deferred.as_ref().filter(|deferred| {
            deferred.state == crate::storage::session_repo::DeferredAgentCompletionState::Pending
        }) {
            status = AgentStatus::AwaitingDescendants;
            awaiting_deferred_turn_id = Some(deferred.agent_turn_id);
        } else if matches!(
            summary.status(),
            SessionStatus::Completed | SessionStatus::Failed
        ) && effects.deferred.as_ref().is_some_and(|deferred| {
            deferred.state == crate::storage::session_repo::DeferredAgentCompletionState::Superseded
        }) {
            let repository = self.store.session_repo();
            let session_id = summary.session_id();
            match context
                .tree
                .control
                .restore_current_owner_resume(&context.path, move || {
                    repository
                        .schedulable_owner_resume_request_id(session_id)
                        .map_err(|error| error.to_string())
                }) {
                Ok(scheduled) => self.launch_scheduled_turns(&context.tree, scheduled),
                Err(error) => {
                    activity = Some(append_agent_activity(
                        activity,
                        format!(
                            "durable deferred owner-resume projection could not be restored: {error}"
                        ),
                    ));
                }
            }
        }
        for released in &effects.released_deferred_handoffs {
            if let Err(error) = self
                .project_released_deferred_handoff(&context.tree, released)
                .await
            {
                activity = Some(append_agent_activity(
                    activity,
                    format!("released deferred completion could not be projected: {error}"),
                ));
            }
        }
        if interruption_suppresses_child_result_delivery(terminal_cause.as_ref()) {
            // The interrupted child itself has no completion handoff. Storage may still have
            // released an ancestor's deferred terminal above, which is projected first.
            return AgentTurnCompletion {
                status,
                activity,
                awaiting_deferred_turn_id,
            };
        }

        let Some(receipt) = effects.completion_handoff else {
            return AgentTurnCompletion {
                status,
                activity,
                awaiting_deferred_turn_id,
            };
        };
        let Some(parent) = context.path.parent() else {
            return AgentTurnCompletion {
                status,
                activity: Some(
                    "durable child completion handoff exists for the root agent".to_string(),
                ),
                awaiting_deferred_turn_id,
            };
        };
        if receipt.child_session_id != context.session_id
            || receipt.child_session_id != summary.session_id()
            || receipt.child_turn_id != summary.turn_id()
            || receipt.parent_agent_path != parent
        {
            return AgentTurnCompletion {
                status,
                activity: Some(format!(
                    "durable child completion handoff identity does not match {} turn {}",
                    context.path,
                    summary.turn_id()
                )),
                awaiting_deferred_turn_id,
            };
        }
        let parent_snapshot = context
            .tree
            .control
            .list_agents(Some(&parent))
            .ok()
            .and_then(|agents| agents.into_iter().find(|agent| agent.path == parent));
        let Some(parent_snapshot) = parent_snapshot else {
            return AgentTurnCompletion {
                status,
                activity,
                awaiting_deferred_turn_id,
            };
        };
        if matches!(parent_snapshot.status, AgentStatus::Shutdown) {
            // Codex V2 delivers a completion only to the immediate live parent. A dead parent is
            // not bypassed and the result is not bubbled to root, because either choice would
            // attach the result to a different task owner.
            return AgentTurnCompletion {
                status,
                activity,
                awaiting_deferred_turn_id,
            };
        }
        if receipt.parent_session_id != parent_snapshot.session_id {
            return AgentTurnCompletion {
                status,
                activity: Some(format!(
                    "durable child completion handoff parent session {} does not match live parent {}",
                    receipt.parent_session_id, parent_snapshot.session_id
                )),
                awaiting_deferred_turn_id,
            };
        }
        // The completion receipt points to a durable pending mailbox envelope.
        // V50 validates its immutable payload, direct-child lineage, and exact
        // eventual history identity at the storage boundary; it is intentionally
        // absent from protocol history until the parent's safe delivery point.
        match self.enqueue_completion_handoff(&context.tree, &context.path, &receipt) {
            Ok(()) => AgentTurnCompletion {
                status,
                activity,
                awaiting_deferred_turn_id,
            },
            Err(error) => AgentTurnCompletion {
                status,
                activity: Some(append_agent_activity(
                    activity,
                    format!("durable child result notice could not be enqueued: {error}"),
                )),
                awaiting_deferred_turn_id,
            },
        }
    }

    async fn project_released_deferred_handoff(
        self: &Arc<Self>,
        tree: &Arc<AgentTreeRuntime>,
        handoff: &StoredAgentCompletionHandoff,
    ) -> Result<(), String> {
        let deferred_agent = tree
            .control
            .list_agents(None)
            .map_err(agent_control_error)?
            .into_iter()
            .find(|agent| agent.session_id == handoff.child_session_id)
            .ok_or_else(|| {
                format!(
                    "released deferred session {} is not retained in the live tree",
                    handoff.child_session_id
                )
            })?;
        if deferred_agent.path.is_root()
            || deferred_agent.path.parent().as_ref() != Some(&handoff.parent_agent_path)
        {
            return Err(format!(
                "released deferred handoff path {} does not match retained agent {}",
                handoff.parent_agent_path, deferred_agent.path
            ));
        }
        let terminal = self
            .store
            .session_repo()
            .durable_terminal_for_turn(handoff.child_session_id, handoff.child_turn_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| {
                format!(
                    "released deferred session {} turn {} has no durable terminal",
                    handoff.child_session_id, handoff.child_turn_id
                )
            })?;
        let summary =
            RunSummary::from_terminal(handoff.child_session_id, handoff.child_turn_id, terminal);
        if !matches!(
            summary.status(),
            SessionStatus::Completed | SessionStatus::Failed
        ) {
            return Err(format!(
                "released deferred session {} has unsupported terminal status {}",
                handoff.child_session_id,
                summary.status().key()
            ));
        }
        let durable_result: Result<RunSummary, AppRunError> = Ok(summary.clone());
        let content = self
            .final_child_result_content(&durable_result, None)
            .await?;
        let released_status = agent_status_from_durable_summary(&summary, content);
        let released_status = inactive_agent_status(released_status, None)?;
        let repository = self.store.session_repo();
        let parent_session_id = handoff.parent_session_id;
        let parent_is_root = handoff.parent_agent_path.is_root();
        let history_item_id = handoff.history_item_id;
        let scheduled = tree
            .control
            .project_released_deferred_completion(
                &deferred_agent.path,
                handoff.child_session_id,
                handoff.child_turn_id,
                &handoff.parent_agent_path,
                handoff.parent_session_id,
                released_status,
                None,
                handoff.history_item_id,
                handoff.released_owner_deferred_turn_id,
                move || {
                    let current = repository
                        .schedulable_owner_resume_request_id(parent_session_id)
                        .map_err(|error| error.to_string())?;
                    if parent_is_root && current.is_some() {
                        return Err(format!(
                            "released root completion handoff {history_item_id} found an unexpected current OwnerResume identity"
                        ));
                    }
                    Ok(current)
                },
            )
            .map_err(agent_control_error)?;
        self.launch_scheduled_turns(tree, scheduled);
        Ok(())
    }

    fn launch_scheduled_turns(
        self: &Arc<Self>,
        tree: &Arc<AgentTreeRuntime>,
        scheduled: Vec<AgentExecutionLease>,
    ) {
        let mut pending = scheduled.into_iter().collect::<VecDeque<_>>();
        while let Some(lease) = pending.pop_front() {
            let context = self.context_for_execution(tree, &lease);
            let context = match context {
                Ok(context) => context,
                Err(error) => {
                    match self.settle_pre_admission_execution(tree, lease, Some(error)) {
                        Ok(additional) => pending.extend(additional),
                        Err(settlement_error) => {
                            eprintln!(
                                "warning: failed to settle unlaunchable child execution: {settlement_error}"
                            );
                        }
                    }
                    continue;
                }
            };
            if let Err(failure) = self.launch_agent_turn(context, lease, String::new()) {
                if let Ok(mut metadata) = tree.metadata.lock()
                    && let Some(node) = metadata.get_mut(&failure.context.path)
                {
                    node.updated = true;
                }
                match self.settle_pre_admission_execution(
                    tree,
                    failure.lease,
                    Some(failure.message),
                ) {
                    Ok(additional) => pending.extend(additional),
                    Err(settlement_error) => {
                        eprintln!(
                            "warning: failed to settle child thread launch failure: {settlement_error}"
                        );
                    }
                }
            }
        }
    }

    fn context_for_execution(
        self: &Arc<Self>,
        tree: &Arc<AgentTreeRuntime>,
        lease: &AgentExecutionLease,
    ) -> Result<AgentRunContext, String> {
        let path = lease.path();
        let session_id = tree
            .control
            .list_agents(Some(path))
            .map_err(agent_control_error)?
            .into_iter()
            .find(|agent| agent.path == *path)
            .map(|agent| agent.session_id)
            .ok_or_else(|| format!("agent `{path}` was not found"))?;
        let metadata = tree
            .metadata
            .lock()
            .map_err(|_| "agent metadata lock was poisoned".to_string())?
            .get(path)
            .cloned()
            .ok_or_else(|| format!("agent `{path}` has no runtime metadata"))?;
        Ok(AgentRunContext {
            runtime: self.clone(),
            tree: tree.clone(),
            path: path.clone(),
            session_id,
            wake_cause: lease.wake_cause(),
            execution: lease.scope(),
            turn_owner: Arc::new(OnceLock::new()),
            config: metadata.config,
            workspace: metadata.workspace,
            confirmation: metadata.confirmation,
            run_service: metadata.run_service,
        })
    }

    async fn final_assistant_text(&self, summary: &RunSummary) -> Result<Option<String>, String> {
        let Some(response_id) = summary.final_response_id() else {
            return Ok(None);
        };
        let content = self
            .store
            .protocol_event_store()
            .assistant_content_for_response(summary.session_id(), response_id)
            .map_err(|error| error.to_string())?;
        Ok(content
            .as_deref()
            .and_then(|content| content_parts_text(content, "\n")))
    }

    async fn final_child_result_content(
        &self,
        result: &Result<RunSummary, AppRunError>,
        terminal_cause: Option<&RunCancellationCause>,
    ) -> Result<Option<String>, String> {
        match result {
            Ok(summary) => match summary.status() {
                SessionStatus::Completed => {
                    if summary.final_response_id().is_some() {
                        // A durable terminal response identity is authoritative. Do not substitute
                        // unrelated later assistant text if that exact point lookup is absent.
                        return self.final_assistant_text(summary).await;
                    }
                    Ok(None)
                }
                SessionStatus::Failed => {
                    // The typed terminal is the sole durable owner of a failed turn's error.
                    // History may contain partial assistant output or an earlier retryable error;
                    // neither may replace the terminal failure delivered to the parent.
                    let TurnTerminalOutcome::Failed { error } = &summary.terminal().outcome else {
                        return Err(format!(
                            "child session {} reported failed status without a failed terminal outcome",
                            summary.session_id()
                        ));
                    };
                    if error.trim().is_empty() {
                        return Err(format!(
                            "child session {} has an empty terminal failure error",
                            summary.session_id()
                        ));
                    }
                    Ok(Some(error.clone()))
                }
                SessionStatus::Cancelled | SessionStatus::Idle | SessionStatus::Running => Ok(None),
            },
            Err(error) => match terminal_cause {
                Some(RunCancellationCause::Interruption(_)) => Ok(None),
                Some(RunCancellationCause::Failure(message)) => Ok(Some(message.clone())),
                Some(RunCancellationCause::Superseded) | None => Ok(Some(error.to_string())),
            },
        }
    }

    fn append_communication(
        &self,
        caller_owner: AgentDurableTurnOwner,
        session_id: SessionId,
        communication: InterAgentCommunication,
        require_active_recipient: bool,
        ready_turn_capacity_granted: bool,
    ) -> Result<AgentMailCommit, crate::error::StorageError> {
        let stored = self
            .store
            .session_repo()
            .append_inter_agent_communication_for_caller_turn_with_protocol_bundle_and_capacity(
                caller_owner.session_id,
                caller_owner.admission_id,
                caller_owner.turn_id,
                session_id,
                communication,
                require_active_recipient,
                ready_turn_capacity_granted,
            )?;
        Ok(AgentMailCommit {
            history_item_id: stored.history_item_id,
            schedule_turn: stored.schedule_turn,
            owner_resume_request_id: None,
        })
    }

    fn append_activity(
        &self,
        caller: &AgentRunContext,
        activity_id: &str,
        agent_session_id: SessionId,
        agent_path: &AgentPath,
        activity_kind: SubAgentActivityKind,
    ) -> Result<(), String> {
        let owner = caller.durable_turn_owner()?;
        self.store
            .protocol_event_store()
            .append_sub_agent_activity(
                caller.tree.root_session_id,
                owner.session_id,
                owner.admission_id,
                owner.turn_id,
                activity_id.to_string(),
                agent_session_id,
                agent_path.to_string(),
                activity_kind,
            )
            .map_err(|error| error.to_string())?;
        if let Ok(mut metadata) = caller.tree.metadata.lock()
            && let Some(node) = metadata.get_mut(agent_path)
        {
            node.activity_owner = Some(owner);
        }
        Ok(())
    }

    fn mark_activity_owner(
        &self,
        caller: &AgentRunContext,
        agent_path: &AgentPath,
    ) -> Result<(), String> {
        let owner = caller.durable_turn_owner()?;
        if let Ok(mut metadata) = caller.tree.metadata.lock()
            && let Some(node) = metadata.get_mut(agent_path)
        {
            node.activity_owner = Some(owner);
        }
        Ok(())
    }
}

fn effective_run_terminal_cause(
    result: &Result<RunSummary, AppRunError>,
    cancellation_cause: Option<RunCancellationCause>,
) -> Option<RunCancellationCause> {
    match result {
        Err(error) => {
            cancellation_cause.or_else(|| Some(RunCancellationCause::Failure(error.to_string())))
        }
        // Once a durable summary exists, its typed terminal is the sole authority. A local
        // cancellation cause can be stale (for example, an approval abort racing a committed
        // failure) and must not reclassify the settled turn.
        Ok(summary) => durable_terminal_cause(summary),
    }
}

fn durable_terminal_cause(summary: &RunSummary) -> Option<RunCancellationCause> {
    match &summary.terminal().outcome {
        TurnTerminalOutcome::Completed => None,
        TurnTerminalOutcome::Failed { error } => Some(RunCancellationCause::Failure(error.clone())),
        TurnTerminalOutcome::Interrupted { cause } => {
            Some(RunCancellationCause::Interruption(*cause))
        }
    }
}

fn activate_child_execution(lease: &AgentExecutionLease) -> Result<(), AgentTurnCompletion> {
    let run_control = lease.run_control();
    if let Some(completion) = child_completion_before_run_admission(&run_control) {
        return Err(completion);
    }
    Ok(())
}

fn pre_admission_terminal(
    lease: &AgentExecutionLease,
    fallback_error: Option<String>,
) -> (
    crate::session::DurableTurnTerminal,
    InactiveAgentStatus,
    Option<String>,
) {
    let cause = lease.run_control().cause();
    match cause {
        Some(RunCancellationCause::Interruption(cause)) => (
            crate::session::DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Interrupted { cause },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            },
            InactiveAgentStatus::Interrupted,
            Some(format!(
                "Child interrupted before durable turn admission: {cause:?}"
            )),
        ),
        Some(RunCancellationCause::Failure(message)) => (
            crate::session::DurableTurnTerminal {
                outcome: TurnTerminalOutcome::Failed {
                    error: message.clone(),
                },
                final_response_id: None,
                tool_call_count: 0,
                failed_tool_count: 0,
                change_count: 0,
                metrics: Default::default(),
            },
            InactiveAgentStatus::Errored(message.clone()),
            Some(message),
        ),
        Some(RunCancellationCause::Superseded) => {
            let message =
                "child execution was superseded before durable turn admission".to_string();
            (
                crate::session::DurableTurnTerminal {
                    outcome: TurnTerminalOutcome::Failed {
                        error: message.clone(),
                    },
                    final_response_id: None,
                    tool_call_count: 0,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                },
                InactiveAgentStatus::Errored(message.clone()),
                Some(message),
            )
        }
        None => {
            let message = fallback_error.unwrap_or_else(|| {
                if lease.run_control().success_is_sealed() {
                    "child execution was sealed before durable turn admission".to_string()
                } else {
                    "child execution stopped before durable turn admission".to_string()
                }
            });
            (
                crate::session::DurableTurnTerminal {
                    outcome: TurnTerminalOutcome::Failed {
                        error: message.clone(),
                    },
                    final_response_id: None,
                    tool_call_count: 0,
                    failed_tool_count: 0,
                    change_count: 0,
                    metrics: Default::default(),
                },
                InactiveAgentStatus::Errored(message.clone()),
                Some(message),
            )
        }
    }
}

fn child_completion_before_run_admission(run_control: &RunControl) -> Option<AgentTurnCompletion> {
    if let Some(cause) = run_control.cause() {
        return Some(AgentTurnCompletion::new(child_status_before_run_admission(
            cause,
        )));
    }
    run_control.success_is_sealed().then(|| {
        AgentTurnCompletion::new(AgentStatus::Errored(
            "child execution was sealed before durable turn admission".to_string(),
        ))
    })
}

fn child_status_before_run_admission(cause: RunCancellationCause) -> AgentStatus {
    match cause {
        RunCancellationCause::Interruption(_) => AgentStatus::Interrupted,
        RunCancellationCause::Failure(message) => AgentStatus::Errored(message),
        RunCancellationCause::Superseded => AgentStatus::Errored(
            "child execution was superseded before durable turn admission".to_string(),
        ),
    }
}

async fn wait_for_control_quiescence(control: &AgentControl) -> Result<(), AgentControlError> {
    loop {
        if control.is_quiescent()? {
            return Ok(());
        }
        let observed_generation = control.activity_generation();
        if control.is_quiescent()? {
            return Ok(());
        }
        control.wait_for_activity(observed_generation).await?;
    }
}

fn agent_control_error(error: AgentControlError) -> String {
    error.to_string()
}

fn inactive_agent_status(
    status: AgentStatus,
    awaiting_deferred_turn_id: Option<TurnId>,
) -> Result<InactiveAgentStatus, String> {
    match status {
        AgentStatus::AwaitingDescendants => awaiting_deferred_turn_id
            .map(InactiveAgentStatus::AwaitingDescendants)
            .ok_or_else(|| {
                "AwaitingDescendants requires an exact durable deferred turn identity".to_string()
            }),
        AgentStatus::Interrupted => Ok(InactiveAgentStatus::Interrupted),
        AgentStatus::Completed(result) => Ok(InactiveAgentStatus::Completed(result)),
        AgentStatus::Errored(message) => Ok(InactiveAgentStatus::Errored(message)),
        AgentStatus::Shutdown => Ok(InactiveAgentStatus::Shutdown),
        AgentStatus::PendingInit | AgentStatus::Running => Err(format!(
            "active status {status:?} cannot be retained after execution completion"
        )),
    }
}

fn interruption_suppresses_child_result_delivery(cause: Option<&RunCancellationCause>) -> bool {
    matches!(
        cause,
        Some(RunCancellationCause::Interruption(
            TurnInterruptionCause::ApprovalAborted
                | TurnInterruptionCause::TreeStopped
                | TurnInterruptionCause::UserStop
        ))
    )
}

fn scheduled_mail_delivery(
    outcome: AgentMailDeliveryOutcome,
) -> Result<Vec<AgentExecutionLease>, String> {
    match outcome {
        AgentMailDeliveryOutcome::Enqueued { scheduled, .. } => Ok(scheduled),
    }
}

fn load_durable_agent_children(
    store: &StoreBundle,
    root_session_id: SessionId,
) -> Result<Vec<DurableAgentChild>, String> {
    let protocol_store = store.protocol_event_store();
    let descendant_limit = crate::runtime::agent_control::MAX_RETAINED_AGENTS.saturating_sub(1);
    let projected_children = protocol_store
        .retained_descendant_snapshot(root_session_id, descendant_limit)
        .map_err(|error| error.to_string())?;
    let mut durable_children = Vec::with_capacity(projected_children.len());
    for child in projected_children {
        let session_status = parse_durable_session_status(&child.session_status)?;
        let task_preview = child
            .latest_task_content
            .as_deref()
            .and_then(|content| content_parts_text(content, "\n"))
            .unwrap_or_else(|| child.edge.task_name.clone());
        let result = durable_child_result_from_projection(
            session_status,
            child.latest_assistant_content.as_deref(),
            child.latest_error.as_deref(),
        );
        let child_session_id = child.edge.child_session_id;
        durable_children.push(DurableAgentChild {
            session_id: child_session_id,
            edge: child.edge,
            session_status,
            active_turn_id: child.active_turn_id,
            pending_deferred_turn_id: child.pending_deferred_turn_id,
            pending_trigger_history_item_id: child.pending_trigger_history_item_id,
            pending_trigger_schedule_ready: child.pending_trigger_schedule_ready,
            pending_owner_resume_request_id: child.pending_owner_resume_request_id,
            task_preview,
            result,
            interruption_cause: child.interruption_cause,
        });
    }
    Ok(durable_children)
}

fn parse_durable_session_status(value: &str) -> Result<SessionStatus, String> {
    match value {
        "idle" => Ok(SessionStatus::Idle),
        "running" => Ok(SessionStatus::Running),
        "completed" => Ok(SessionStatus::Completed),
        "cancelled" => Ok(SessionStatus::Cancelled),
        "failed" => Ok(SessionStatus::Failed),
        _ => Err(format!("unknown persisted session status `{value}`")),
    }
}

fn durable_child_result_from_projection(
    status: SessionStatus,
    latest_assistant_content: Option<&[ContentPart]>,
    latest_error: Option<&str>,
) -> Option<String> {
    let assistant = latest_assistant_content.and_then(|content| content_parts_text(content, "\n"));
    let error = latest_error
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_string);
    if status == SessionStatus::Failed {
        error.or(assistant)
    } else {
        assistant.or(error)
    }
}

fn content_parts_text(content: &[ContentPart], separator: &str) -> Option<String> {
    let text = content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join(separator);
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

fn rehydrated_agent_state(
    session_id: SessionId,
    status: SessionStatus,
    result: Option<String>,
    interruption_cause: Option<TurnInterruptionCause>,
) -> Result<AgentStatus, String> {
    match status {
        SessionStatus::Running => {
            return Err(format!(
                "cannot rehydrate running child session {} without an active execution owner",
                session_id
            ));
        }
        _ => Ok(durable_projection_status(
            session_id,
            status,
            result,
            interruption_cause,
        )),
    }
}

fn durable_projection_status(
    session_id: SessionId,
    status: SessionStatus,
    result: Option<String>,
    interruption_cause: Option<TurnInterruptionCause>,
) -> AgentStatus {
    if status == SessionStatus::Cancelled {
        return match interruption_cause {
            Some(_) => AgentStatus::Interrupted,
            None => AgentStatus::Errored(missing_interruption_cause_message(session_id)),
        };
    }
    durable_child_terminal_status(status, result)
}

fn missing_interruption_cause_message(session_id: SessionId) -> String {
    format!("run {session_id} settled as cancelled without a typed interruption cause")
}

fn durable_child_terminal_status(status: SessionStatus, result: Option<String>) -> AgentStatus {
    match status {
        SessionStatus::Idle => AgentStatus::Shutdown,
        SessionStatus::Running => AgentStatus::Running,
        SessionStatus::Completed => AgentStatus::Completed(result),
        SessionStatus::Cancelled => AgentStatus::Interrupted,
        SessionStatus::Failed => {
            AgentStatus::Errored(result.unwrap_or_else(|| {
                "Child session failed without a durable error message".to_string()
            }))
        }
    }
}

fn agent_status_from_terminal_result(
    result: &Result<RunSummary, AppRunError>,
    terminal_cause: Option<&RunCancellationCause>,
    content: Option<String>,
) -> AgentStatus {
    match result {
        Ok(summary) => agent_status_from_durable_summary(summary, content),
        Err(error) => match terminal_cause {
            Some(RunCancellationCause::Interruption(_)) => AgentStatus::Interrupted,
            Some(RunCancellationCause::Failure(message)) => {
                AgentStatus::Errored(content.unwrap_or_else(|| message.clone()))
            }
            Some(RunCancellationCause::Superseded) => {
                AgentStatus::Errored(content.unwrap_or_else(|| {
                    "agent run was superseded before a durable terminal result was returned"
                        .to_string()
                }))
            }
            None => AgentStatus::Errored(error.to_string()),
        },
    }
}

fn agent_status_from_durable_summary(
    summary: &RunSummary,
    completed_content: Option<String>,
) -> AgentStatus {
    match &summary.terminal().outcome {
        TurnTerminalOutcome::Completed => AgentStatus::Completed(completed_content),
        TurnTerminalOutcome::Failed { error } => AgentStatus::Errored(error.clone()),
        TurnTerminalOutcome::Interrupted { .. } => AgentStatus::Interrupted,
    }
}

fn agent_status_result(status: &AgentStatus) -> String {
    match status {
        AgentStatus::Completed(Some(result)) | AgentStatus::Errored(result) => preview(result, 320),
        AgentStatus::Completed(None) => "Completed".to_string(),
        AgentStatus::AwaitingDescendants => "Waiting for descendants".to_string(),
        AgentStatus::Interrupted => "Interrupted".to_string(),
        AgentStatus::Shutdown => "Stopped".to_string(),
        AgentStatus::PendingInit | AgentStatus::Running => String::new(),
    }
}

fn append_agent_activity(existing: Option<String>, message: String) -> String {
    match existing {
        Some(existing) => format!("{existing}; {message}"),
        None => message,
    }
}

fn preview(value: &str, max_chars: usize) -> String {
    let mut chars = value.trim().chars();
    let preview = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

struct AgentEventRenderer;

impl EventRenderer for AgentEventRenderer {
    fn render(&mut self, _event: &RunEvent) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn finish(&mut self, _summary: &RunSummary) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_list(&mut self, _sessions: &[SessionRecord]) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_loaded_sessions(
        &mut self,
        _loaded: &LoadedSessionList,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_history_items(
        &mut self,
        _session: &SessionRecord,
        _history_items: &[crate::protocol::HistoryItem],
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_history_page(
        &mut self,
        _page: &CanonicalHistoryPage,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_read(&mut self, _read: &CanonicalSessionRead) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_rejoin(
        &mut self,
        _rejoin: &RunningSessionRejoin,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_turn_page(
        &mut self,
        _page: &CanonicalTurnPage,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_runtime_event_page(
        &mut self,
        _page: &CanonicalRuntimeEventPage,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_idle_turn_admission(
        &mut self,
        _admission: &IdleTurnAdmission,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_thread_goal_get(
        &mut self,
        _result: &ThreadGoalGetResult,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_thread_goal_set(
        &mut self,
        _result: &ThreadGoalSetResult,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_thread_goal_clear(
        &mut self,
        _result: &ThreadGoalClearResult,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "agent_runtime_tests.rs"]
mod tests;
