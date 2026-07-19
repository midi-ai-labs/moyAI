use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::app::{AppCommand, RunConfigInput, RunRequest, RunService};
use crate::cli::{EventRenderer, OutputMode, SharedConfirmationPrompt};
use crate::config::{ResolvedConfig, ResolvedTurnConfig};
use crate::error::{AppRunError, CliRenderError};
use crate::protocol::{
    ContentPart, HistoryItemId, HistoryItemPayload, InterAgentCommunication, ProtocolEventStore,
    SubAgentActivityKind, TurnId, TurnInterruptionCause,
};
use crate::runtime::{
    ActiveAgentStatus, AgentControl, AgentControlError, AgentExecutionLease, AgentExecutionScope,
    AgentMailDeliveryOutcome, AgentMailboxNotice, AgentPath, AgentRootContinuationOutcome,
    AgentSnapshot, AgentStatus, InactiveAgentStatus, RunCancellationCause, RunControl,
};
#[cfg(test)]
use crate::session::SessionRepository;
use crate::session::{
    AdmissionId, CanonicalHistoryPage, CanonicalRuntimeEventPage, CanonicalSessionRead,
    CanonicalTurnPage, IdleTurnAdmission, LoadedSessionList, RunEvent, RunSummary,
    RunningSessionRejoin, SessionContext, SessionId, SessionRecord, SessionSettingsPatch,
    SessionSpawnEdge, SessionStartRequest, SessionStatus, ThreadGoalClearResult,
    ThreadGoalGetResult, ThreadGoalSetResult,
};
use crate::storage::StoreBundle;
use crate::workspace::Workspace;

const SUPPRESSED_MAIL_DELIVERY_ERROR: &str = "durable evidence was recorded, but the message was not delivered because the recipient became terminal or the agent tree was stopped";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentForkTurns {
    None,
    All,
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
}

#[derive(Clone)]
pub struct AgentRunContext {
    runtime: Arc<AgentRuntime>,
    tree: Arc<AgentTreeRuntime>,
    path: AgentPath,
    session_id: SessionId,
    execution: AgentExecutionScope,
    root_turn_owner: Arc<OnceLock<AgentDurableTurnOwner>>,
    config: Arc<ResolvedTurnConfig>,
    workspace: Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AgentDurableTurnOwner {
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
        self.tree.confirmation_prompt()
    }

    pub(crate) fn model_request_gate(&self) -> Arc<tokio::sync::Semaphore> {
        self.tree.model_request_gate()
    }

    fn effective_config(&self) -> ResolvedConfig {
        self.config.runtime_config().clone()
    }

    pub(crate) fn cancel_for_durable_terminal(&self) -> Result<(), String> {
        self.tree
            .control
            .cancel_for_durable_terminal(&self.path)
            .map_err(agent_control_error)
    }

    pub async fn spawn_agent(
        &self,
        task_name: &str,
        message: String,
        fork_turns: AgentForkTurns,
        activity_id: String,
    ) -> Result<AgentSnapshot, String> {
        self.ensure_spawn_depth()?;
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
    ) -> Result<(), String> {
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
    ) -> Result<AgentStatus, String> {
        self.runtime.interrupt_agent(self, target, activity_id)
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

    pub(crate) fn bind_root_turn_owner(
        &self,
        admission_id: AdmissionId,
        turn_id: TurnId,
    ) -> Result<(), String> {
        if !self.path.is_root() {
            return Err(format!(
                "agent `{}` cannot own the root durable turn fence",
                self.path
            ));
        }
        let owner = AgentDurableTurnOwner {
            admission_id,
            turn_id,
        };
        self.root_turn_owner
            .set(owner)
            .map_err(|_| "root execution is already bound to a durable turn owner".to_string())?;
        *self
            .tree
            .active_root_turn_owner
            .lock()
            .map_err(|_| "active root turn owner lock was poisoned".to_string())? = Some(owner);
        Ok(())
    }

    fn durable_root_turn_owner(&self) -> Result<AgentDurableTurnOwner, String> {
        self.root_turn_owner.get().copied().ok_or_else(|| {
            format!(
                "agent `{}` has no originating durable root turn owner",
                self.path
            )
        })
    }

    pub(crate) fn drain_mailbox(&self) -> Result<Vec<AgentMailboxNotice>, String> {
        let notices = self
            .tree
            .control
            .drain_mailbox(&self.path)
            .map_err(agent_control_error)?;
        let item_ids = notices
            .iter()
            .map(|notice| notice.history_item_id)
            .collect::<Vec<_>>();
        let authors = self.durable_mailbox_authors(&item_ids).unwrap_or_default();
        if let Ok(mut metadata) = self.tree.metadata.lock() {
            for author_path in authors {
                if let Some(author) = metadata.get_mut(&author_path) {
                    author.updated = false;
                }
            }
        }
        Ok(notices)
    }

    fn ensure_spawn_depth(&self) -> Result<(), String> {
        if self.is_sub_agent() {
            return Err(
                "sub-agents cannot spawn another agent; moyAI multi-agent depth is limited to root → child"
                    .to_string(),
            );
        }
        Ok(())
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
            .protocol_event_store()
            .history_items_by_id(self.session_id, item_ids)
            .map_err(|error| error.to_string())?;
        let mut authors = Vec::new();
        for item in items {
            let HistoryItemPayload::InterAgentCommunication { communication } = item.payload else {
                return Err(format!(
                    "mailbox notice {} does not reference canonical inter-agent communication",
                    item.id
                ));
            };
            if communication.recipient != self.path.as_str() {
                return Err(format!(
                    "mailbox notice {} targets `{}` instead of `{}`",
                    item.id, communication.recipient, self.path
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
            .complete_execution(lease, inactive_agent_status(status)?, None)
            .map_err(agent_control_error)?;
        self.context.tree.release_confirmation_if_quiescent();
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
            self.context.tree.release_confirmation_if_quiescent();
        }
    }
}

pub struct AgentRuntime {
    store: StoreBundle,
    session_service: crate::session::SessionService,
    trees: Mutex<HashMap<SessionId, Arc<AgentTreeRuntime>>>,
    run_service: OnceLock<Weak<RunService>>,
}

struct AgentTreeRuntime {
    root_session_id: SessionId,
    control: AgentControl,
    confirmation: Mutex<Option<SharedConfirmationPrompt>>,
    model_request_gate: Mutex<Arc<tokio::sync::Semaphore>>,
    active_root_turn_owner: Mutex<Option<AgentDurableTurnOwner>>,
    metadata: Mutex<HashMap<AgentPath, AgentNodeMetadata>>,
}

#[derive(Clone)]
struct AgentNodeMetadata {
    task_name: String,
    task_preview: String,
    config: Arc<ResolvedTurnConfig>,
    workspace: Workspace,
    updated: bool,
}

struct DurableAgentChild {
    edge: SessionSpawnEdge,
    session_id: SessionId,
    session_status: SessionStatus,
    task_preview: String,
    result: Option<String>,
    interruption_cause: Option<TurnInterruptionCause>,
}

struct AgentLaunchFailure {
    message: String,
    context: AgentRunContext,
    lease: AgentExecutionLease,
}

struct AgentTurnCompletion {
    status: AgentStatus,
    activity: Option<String>,
}

impl AgentTurnCompletion {
    fn new(status: AgentStatus) -> Self {
        Self {
            status,
            activity: None,
        }
    }

    fn with_delivery_outcome(
        mut self,
        outcome: Result<AgentMailDeliveryOutcome, AgentControlError>,
    ) -> Self {
        self.activity = match outcome {
            Ok(AgentMailDeliveryOutcome::Enqueued { .. }) => None,
            Ok(AgentMailDeliveryOutcome::Suppressed) => {
                Some(SUPPRESSED_MAIL_DELIVERY_ERROR.to_string())
            }
            Err(error) => Some(format!(
                "agent result could not be delivered durably: {error}"
            )),
        };
        self
    }
}

impl AgentTreeRuntime {
    fn confirmation_prompt(&self) -> SharedConfirmationPrompt {
        self.confirmation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .cloned()
            .expect("an active agent tree must retain its permission broker")
    }

    fn model_request_gate(&self) -> Arc<tokio::sync::Semaphore> {
        self.model_request_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn install_run_resources(
        &self,
        confirmation: SharedConfirmationPrompt,
        max_concurrent_model_requests: usize,
    ) {
        let control = self.control.clone();
        confirmation.set_approval_abort_handler(move |requesting_control| {
            control.abort_tree_from_permission(requesting_control)
        });
        *self
            .model_request_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(
            tokio::sync::Semaphore::new(max_concurrent_model_requests.max(1)),
        );
        *self
            .confirmation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(confirmation);
    }

    fn release_confirmation_if_quiescent(&self) {
        // Lock the resource before rechecking control state so a resumed root cannot install a
        // fresh broker between the quiescence decision and the release.
        let mut confirmation = self
            .confirmation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.control.is_quiescent().is_ok_and(|quiescent| quiescent) {
            *confirmation = None;
        }
    }
}

impl AgentRuntime {
    pub fn new(store: StoreBundle, session_service: crate::session::SessionService) -> Self {
        Self {
            store,
            session_service,
            trees: Mutex::new(HashMap::new()),
            run_service: OnceLock::new(),
        }
    }

    pub fn bind_run_service(&self, run_service: Weak<RunService>) -> Result<(), String> {
        self.run_service
            .set(run_service)
            .map_err(|_| "agent runtime is already bound to a run service".to_string())
    }

    pub(crate) async fn begin_root(
        self: &Arc<Self>,
        session: &SessionContext,
        config: Arc<ResolvedTurnConfig>,
        confirmation: SharedConfirmationPrompt,
        run_control: RunControl,
    ) -> Result<AgentRuntimeExecution, String> {
        let effective_config = config.runtime_config();
        let root_session_id = session.session.id;
        let existing = {
            let trees = self
                .trees
                .lock()
                .map_err(|_| "agent tree registry lock was poisoned".to_string())?;
            if let Some(tree) = trees
                .get(&root_session_id)
                .filter(|tree| !tree.control.tree_is_cancelled())
                .cloned()
            {
                if !tree.control.is_quiescent().map_err(agent_control_error)? {
                    return Err(format!(
                        "agent tree for session {root_session_id} is still active"
                    ));
                }
                tree.control
                    .reconfigure_max_concurrent_agents(
                        effective_config.multi_agent.max_concurrent_agents,
                    )
                    .map_err(agent_control_error)?;
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
            if let Some(tree) = trees
                .get(&root_session_id)
                .filter(|tree| !tree.control.tree_is_cancelled())
                .cloned()
            {
                if !tree.control.is_quiescent().map_err(agent_control_error)? {
                    return Err(format!(
                        "agent tree for session {root_session_id} is still active"
                    ));
                }
                tree.control
                    .reconfigure_max_concurrent_agents(
                        effective_config.multi_agent.max_concurrent_agents,
                    )
                    .map_err(agent_control_error)?;
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
                let tree = Arc::new(AgentTreeRuntime {
                    root_session_id,
                    control,
                    confirmation: Mutex::new(None),
                    model_request_gate: Mutex::new(Arc::new(tokio::sync::Semaphore::new(1))),
                    active_root_turn_owner: Mutex::new(None),
                    metadata: Mutex::new(HashMap::new()),
                });
                self.restore_durable_children(
                    &tree,
                    durable_children,
                    &config,
                    &session.workspace,
                )?;
                trees.insert(root_session_id, tree.clone());
                (tree, lease)
            }
        };
        tree.install_run_resources(
            confirmation,
            effective_config.multi_agent.max_concurrent_model_requests,
        );
        lease
            .set_status(ActiveAgentStatus::Running)
            .map_err(agent_control_error)?;
        let mut metadata = tree
            .metadata
            .lock()
            .map_err(|_| "agent metadata lock was poisoned".to_string())?;
        for (path, node) in metadata.iter_mut() {
            if !path.is_root() {
                node.config = Arc::clone(&config);
                node.workspace = session.workspace.clone();
            }
        }
        metadata.insert(
            AgentPath::root(),
            AgentNodeMetadata {
                task_name: "root".to_string(),
                task_preview: String::new(),
                config: Arc::clone(&config),
                workspace: session.workspace.clone(),
                updated: false,
            },
        );
        drop(metadata);
        let context = AgentRunContext {
            runtime: self.clone(),
            tree: tree.clone(),
            path: AgentPath::root(),
            session_id: root_session_id,
            execution: lease.scope(),
            root_turn_owner: Arc::new(OnceLock::new()),
            config,
            workspace: session.workspace.clone(),
        };
        Ok(AgentRuntimeExecution {
            context,
            lease: Some(lease),
        })
    }

    pub(crate) fn begin_root_continuation(
        self: &Arc<Self>,
        root_session_id: SessionId,
        run_control: RunControl,
        confirmation: Option<SharedConfirmationPrompt>,
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
        let context = self.context_for_execution(&tree, &lease)?;
        let max_concurrent_model_requests = context
            .config
            .runtime_config()
            .multi_agent
            .max_concurrent_model_requests;
        let continuation_control = lease.run_control();
        if let Err(error) = lease.set_status(ActiveAgentStatus::Running) {
            let message = agent_control_error(error);
            continuation_control.fail(message.clone());
            drop(lease);
            return Err(message);
        }
        let execution = AgentRuntimeExecution {
            context,
            lease: Some(lease),
        };
        tree.install_run_resources(confirmation, max_concurrent_model_requests);
        Ok(AgentRuntimeContinuationOutcome::Admitted(execution))
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
        durable_children: Vec<DurableAgentChild>,
        config: &Arc<ResolvedTurnConfig>,
        workspace: &Workspace,
    ) -> Result<(), String> {
        let mut restored_metadata = Vec::with_capacity(durable_children.len());
        for durable_child in durable_children {
            let DurableAgentChild {
                edge,
                session_id,
                session_status,
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
            if edge.parent_session_id != tree.root_session_id {
                return Err(format!(
                    "spawn edge {} uses non-root parent session {}; only root → direct-child lineage is supported",
                    edge.agent_path, edge.parent_session_id
                ));
            }
            let root_path = AgentPath::root();
            let expected_path = root_path.join(&edge.task_name)?;
            let durable_path = AgentPath::try_from(edge.agent_path.as_str())?;
            if expected_path != durable_path {
                return Err(format!(
                    "spawn edge path {} does not match parent/task path {}",
                    durable_path, expected_path
                ));
            }
            let status =
                rehydrated_agent_state(session_id, session_status, result, interruption_cause)?;
            let snapshot = tree
                .control
                .restore_inactive_child(
                    &root_path,
                    &edge.task_name,
                    session_id,
                    inactive_agent_status(status)?,
                    None,
                )
                .map_err(agent_control_error)?;
            restored_metadata.push((
                snapshot.path,
                AgentNodeMetadata {
                    task_name: edge.task_name,
                    task_preview,
                    config: Arc::clone(config),
                    workspace: workspace.clone(),
                    updated: false,
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
            .enumerate()
            .map(|(index, child)| {
                let status = durable_projection_status(
                    child.session_id,
                    child.session_status,
                    child.result,
                    child.interruption_cause,
                );
                Ok(AgentActivityRecord {
                    agent_path: child.edge.agent_path,
                    session_id: child.session_id,
                    task_name: child.edge.task_name,
                    task_preview: preview(&child.task_preview, 240),
                    result_preview: agent_status_result(&status),
                    status,
                    current_activity: String::new(),
                    started_order: u64::try_from(index)
                        .map_err(|_| "durable agent spawn order exceeded u64".to_string())?
                        .saturating_add(1),
                    updated: false,
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
        let durable_success = matches!(
            result,
            Ok(summary) if summary.status() == SessionStatus::Completed
        );
        let terminal_cause = effective_run_terminal_cause(result, cancellation_cause);
        if !durable_success {
            if result.is_ok() {
                if let Some(cause) = terminal_cause.clone() {
                    tree.control.reconcile_durable_root_terminal(cause);
                }
            } else {
                match terminal_cause.as_ref() {
                    Some(RunCancellationCause::Interruption(cause)) => {
                        tree.control.interrupt_tree(*cause);
                    }
                    Some(RunCancellationCause::Superseded) => {
                        let _ = tree.control.cancel_for_durable_terminal(&AgentPath::root());
                    }
                    Some(RunCancellationCause::Failure(message)) => {
                        tree.control.fail_tree(message.clone());
                    }
                    None => {}
                }
            }
        }
        let status = agent_status_from_terminal_result(result, terminal_cause.as_ref(), None);
        if let Ok(scheduled) = execution.complete(status) {
            self.launch_scheduled_turns(&tree, scheduled);
        }
        tree.release_confirmation_if_quiescent();
    }

    pub(crate) fn release_unadmitted_root_continuation(
        self: &Arc<Self>,
        execution: AgentRuntimeExecution,
    ) -> Result<(), String> {
        let tree = execution.context.tree.clone();
        let scheduled = execution.complete(AgentStatus::Completed(None))?;
        self.launch_scheduled_turns(&tree, scheduled);
        tree.release_confirmation_if_quiescent();
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
                }
            })
            .collect()
    }

    pub fn cancel_tree_for_session(
        &self,
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
            tree.control.interrupt_tree(root_cause)
        } else {
            false
        }
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
            tree.release_confirmation_if_quiescent();
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
        caller.ensure_spawn_depth()?;
        if message.trim().is_empty() {
            return Err("spawn_agent requires a non-empty message".to_string());
        }
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
        let child_session = self
            .session_service
            .start_or_resume(
                SessionStartRequest {
                    selector: crate::session::SessionSelector::New,
                    title: Some(task_name.to_string()),
                    cwd: caller.workspace.cwd.clone(),
                    model: child_config.model.model.clone(),
                    base_url: child_config.model.base_url.clone(),
                    access_mode: child_config.permissions.access_mode,
                },
                caller.workspace.clone(),
            )
            .await
            .map_err(|error| error.to_string())?;
        let child_session_id = child_session.session.id;
        let cleanup_child = || async {
            let _ = self
                .store
                .session_repo()
                .delete_session_tree(child_session_id)
                .await;
        };

        if let Err(error) = self
            .store
            .session_repo()
            .insert_session_spawn_edge(
                caller.tree.root_session_id,
                caller.session_id,
                child_session_id,
                child_path.as_str(),
                task_name,
            )
            .await
        {
            cleanup_child().await;
            return Err(error.to_string());
        }
        if fork_turns == AgentForkTurns::All
            && let Err(error) = self
                .store
                .protocol_event_store()
                .fork_agent_context(caller.session_id, child_session_id)
        {
            cleanup_child().await;
            return Err(error.to_string());
        }

        let (snapshot, lease) = match caller.tree.control.register_child(
            &caller.path,
            task_name,
            child_session_id,
            Some("Starting assigned task".to_string()),
        ) {
            Ok(result) => result,
            Err(error) => {
                cleanup_child().await;
                return Err(agent_control_error(error));
            }
        };
        caller
            .tree
            .metadata
            .lock()
            .map_err(|_| "agent metadata lock was poisoned".to_string())?
            .insert(
                child_path.clone(),
                AgentNodeMetadata {
                    task_name: task_name.to_string(),
                    task_preview: message.clone(),
                    config: Arc::clone(&caller.config),
                    workspace: caller.workspace.clone(),
                    updated: false,
                },
            );
        if let Err(error) = self.append_activity(
            caller,
            &activity_id,
            child_session_id,
            &child_path,
            SubAgentActivityKind::Started,
        ) {
            return match self
                .rollback_spawn(&caller.tree, lease, child_session_id)
                .await
            {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(format!(
                    "{error}; failed to roll back the uncommitted child registration: {cleanup_error}"
                )),
            };
        }

        let child_context = AgentRunContext {
            runtime: self.clone(),
            tree: caller.tree.clone(),
            path: child_path.clone(),
            session_id: child_session_id,
            execution: lease.scope(),
            root_turn_owner: Arc::clone(&caller.root_turn_owner),
            config: Arc::clone(&caller.config),
            workspace: caller.workspace.clone(),
        };
        if let Err(failure) = self.launch_agent_turn(child_context, lease, message) {
            let AgentLaunchFailure { message, lease, .. } = failure;
            return match self
                .rollback_spawn(&caller.tree, lease, child_session_id)
                .await
            {
                Ok(()) => Err(message),
                Err(cleanup_error) => Err(format!(
                    "{message}; failed to roll back the uncommitted child registration: {cleanup_error}"
                )),
            };
        }
        Ok(snapshot)
    }

    async fn rollback_spawn(
        &self,
        tree: &Arc<AgentTreeRuntime>,
        lease: AgentExecutionLease,
        child_session_id: SessionId,
    ) -> Result<(), String> {
        let path = lease.path().clone();
        tree.control
            .rollback_child_registration(&lease, child_session_id)
            .map_err(agent_control_error)?;
        drop(lease);
        if let Ok(mut metadata) = tree.metadata.lock() {
            metadata.remove(&path);
        }
        self.store
            .session_repo()
            .delete_session_tree(child_session_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn send_message(
        self: &Arc<Self>,
        caller: &AgentRunContext,
        target: &str,
        message: String,
        trigger_turn: bool,
        activity_id: String,
    ) -> Result<(), String> {
        if message.trim().is_empty() {
            return Err("agent message must not be empty".to_string());
        }
        if caller.tree.control.tree_is_cancelled() {
            return Err("the agent tree has been cancelled".to_string());
        }
        let recipient_path = caller.path.resolve(target)?;
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
        let communication = InterAgentCommunication {
            author: caller.path.to_string(),
            recipient: recipient_path.to_string(),
            content: message.clone(),
            trigger_turn,
        };
        let require_active_recipient = recipient.is_active;
        let delivery = caller
            .tree
            .control
            .commit_and_enqueue_mail(&caller.path, &recipient_path, trigger_turn, || {
                self.append_communication(
                    recipient.session_id,
                    communication,
                    require_active_recipient,
                )
            })
            .map_err(agent_control_error)?;
        let scheduled = scheduled_mail_delivery(delivery)?;
        let (activity_session_id, activity_path) = if recipient_path.is_root() {
            (caller.session_id, &caller.path)
        } else {
            (recipient.session_id, &recipient_path)
        };
        let _ = self.append_activity(
            caller,
            &activity_id,
            activity_session_id,
            activity_path,
            SubAgentActivityKind::Interacted,
        );
        self.launch_scheduled_turns(&caller.tree, scheduled);
        Ok(())
    }

    fn interrupt_agent(
        &self,
        caller: &AgentRunContext,
        target: &str,
        activity_id: String,
    ) -> Result<AgentStatus, String> {
        let target_path = caller.path.resolve(target)?;
        if target_path.is_root() {
            return Err("root is not a spawned agent".to_string());
        }
        if target_path == caller.path {
            return Err("an agent cannot interrupt itself".to_string());
        }
        let snapshot = caller
            .tree
            .control
            .list_agents(Some(&target_path))
            .map_err(agent_control_error)?
            .into_iter()
            .find(|agent| agent.path == target_path)
            .ok_or_else(|| format!("agent `{target_path}` was not found"))?;
        let previous_status = snapshot.status.clone();
        if snapshot.is_active {
            caller
                .tree
                .control
                .cancel_agent(&target_path)
                .map_err(agent_control_error)?;
        }
        let _ = self.append_activity(
            caller,
            &activity_id,
            snapshot.session_id,
            &target_path,
            SubAgentActivityKind::Interrupted,
        );
        Ok(previous_status)
    }

    fn launch_agent_turn(
        self: &Arc<Self>,
        context: AgentRunContext,
        lease: AgentExecutionLease,
        prompt: String,
    ) -> Result<(), AgentLaunchFailure> {
        let Some(run_service) = self.run_service.get().and_then(Weak::upgrade) else {
            return Err(AgentLaunchFailure {
                message: "agent runtime is not bound to an active run service".to_string(),
                context,
                lease,
            });
        };
        let runtime = self.clone();
        let thread_name = format!("moyai-agent-{}", context.path.name());
        let launch_state = Arc::new(Mutex::new(Some((context, lease, prompt))));
        let worker_state = launch_state.clone();
        let spawn_result = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                let Some((context, lease, prompt)) =
                    worker_state.lock().ok().and_then(|mut state| state.take())
                else {
                    return;
                };
                if let Err(completion) = activate_child_execution(&lease) {
                    runtime.complete_child_before_run(&context, lease, completion);
                    return;
                }
                context.set_activity("Running assigned task");
                let mut confirmation = context.confirmation_prompt();
                let mut renderer = AgentEventRenderer;
                let local = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(local) => local,
                    Err(error) => {
                        let scheduled = context
                            .tree
                            .control
                            .complete_execution(
                                lease,
                                InactiveAgentStatus::Errored(format!(
                                    "failed to build sub-agent runtime: {error}"
                                )),
                                None,
                            )
                            .unwrap_or_default();
                        runtime.launch_scheduled_turns(&context.tree, scheduled);
                        return;
                    }
                };
                let run_context = context.clone();
                let run_control = lease.run_control();
                let request_run_control = run_control.clone();
                let config = local
                    .block_on(runtime.materialize_context_config_and_sync_session(&run_context));
                let result = match config {
                    Ok(config) => {
                        // The non-cloneable lease still owns its marker after activation. Recheck
                        // its turn-scoped terminal owner immediately before RunService can admit
                        // the durable turn, without publishing a duplicate Running transition.
                        if let Some(completion) =
                            child_completion_before_run_admission(&request_run_control)
                        {
                            runtime.complete_child_before_run(&context, lease, completion);
                            return;
                        }
                        local.block_on(async move {
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
                                .await?
                            {
                                crate::app::AppCommandOutcome::Turn(summary) => Ok(summary),
                                crate::app::AppCommandOutcome::ControlCompleted => {
                                    Err(AppRunError::Message(
                                        "an admitted child turn completed as a control command"
                                            .to_string(),
                                    ))
                                }
                            }
                        })
                    }
                    Err(error) => Err(AppRunError::Message(error)),
                };
                let cancellation_cause = run_control.cause();
                let completion = local.block_on(runtime.finish_agent_turn(
                    &context,
                    &result,
                    cancellation_cause,
                ));
                let AgentTurnCompletion { status, activity } = completion;
                let status = inactive_agent_status(status).unwrap_or_else(|error| {
                    InactiveAgentStatus::Errored(format!(
                        "invalid child terminal lifecycle handoff: {error}"
                    ))
                });
                let scheduled = context
                    .tree
                    .control
                    .complete_execution(lease, status, activity)
                    .unwrap_or_default();
                runtime.launch_scheduled_turns(&context.tree, scheduled);
            });
        match spawn_result {
            Ok(_) => Ok(()),
            Err(error) => {
                let (context, lease, _) = launch_state
                    .lock()
                    .ok()
                    .and_then(|mut state| state.take())
                    .expect("failed thread launch must retain its captured agent state");
                Err(AgentLaunchFailure {
                    message: format!("failed to launch agent thread: {error}"),
                    context,
                    lease,
                })
            }
        }
    }

    fn complete_child_before_run(
        self: &Arc<Self>,
        context: &AgentRunContext,
        lease: AgentExecutionLease,
        completion: AgentTurnCompletion,
    ) {
        let status = inactive_agent_status(completion.status).unwrap_or_else(|error| {
            InactiveAgentStatus::Errored(format!(
                "invalid pre-admission child terminal lifecycle handoff: {error}"
            ))
        });
        let scheduled = context
            .tree
            .control
            .complete_execution(lease, status, completion.activity)
            .unwrap_or_default();
        self.launch_scheduled_turns(&context.tree, scheduled);
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
        let status = match result {
            Ok(summary)
                if matches!(
                    summary.status(),
                    SessionStatus::Completed | SessionStatus::Failed
                ) =>
            {
                durable_child_terminal_status(summary.status(), final_content.clone())
            }
            _ => agent_status_from_terminal_result(
                result,
                terminal_cause.as_ref(),
                final_content.clone(),
            ),
        };
        if let Ok(mut metadata) = context.tree.metadata.lock()
            && let Some(node) = metadata.get_mut(&context.path)
        {
            node.updated = true;
        }
        if let Some(error) = result_read_error {
            // Do not turn a storage/read failure into durable evidence claiming that the child
            // completed without a text result. The durable terminal status remains authoritative;
            // the read failure is retained as execution activity for diagnosis.
            return AgentTurnCompletion {
                status,
                activity: Some(format!("durable child result could not be read: {error}")),
            };
        }
        if interruption_suppresses_child_result_delivery(terminal_cause.as_ref()) {
            // An interrupted child has no model result to deliver. In particular, approval Abort
            // stops the root tree and must not leave an "Agent interrupted" mailbox/history item
            // that would be replayed after the user's next instruction.
            return AgentTurnCompletion::new(status);
        }

        let Some(parent) = context.path.parent() else {
            return AgentTurnCompletion::new(status);
        };
        let parent_snapshot = context
            .tree
            .control
            .list_agents(Some(&parent))
            .ok()
            .and_then(|agents| agents.into_iter().find(|agent| agent.path == parent));
        let Some(parent_snapshot) = parent_snapshot else {
            return AgentTurnCompletion::new(status);
        };
        let content = final_content.unwrap_or_else(|| match &status {
            AgentStatus::Interrupted => "Agent interrupted.".to_string(),
            AgentStatus::Errored(error) => error.clone(),
            _ => "Agent completed without a text result.".to_string(),
        });
        let communication = InterAgentCommunication {
            author: context.path.to_string(),
            recipient: parent.to_string(),
            content: content.clone(),
            trigger_turn: false,
        };
        let delivery =
            context
                .tree
                .control
                .commit_and_enqueue_mail(&context.path, &parent, false, || {
                    // A child can finish after the parent's durable success commit but before the
                    // retained root execution marker is cleared. Result mail is consumed by the next
                    // root turn, so its durable append must not depend on that transient active flag.
                    self.append_communication(parent_snapshot.session_id, communication, false)
                });
        AgentTurnCompletion::new(status).with_delivery_outcome(delivery)
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
                    let additional =
                        settle_unlaunchable_scheduled_execution(&tree.control, lease, error);
                    pending.extend(additional);
                    continue;
                }
            };
            if let Err(failure) = self.launch_agent_turn(context, lease, String::new()) {
                if let Ok(mut metadata) = tree.metadata.lock()
                    && let Some(node) = metadata.get_mut(&failure.context.path)
                {
                    node.updated = true;
                }
                let additional = settle_unlaunchable_scheduled_execution(
                    &tree.control,
                    failure.lease,
                    failure.message,
                );
                pending.extend(additional);
            }
        }
        tree.release_confirmation_if_quiescent();
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
        let root_turn_owner = if path.is_root() {
            Arc::new(OnceLock::new())
        } else {
            let owner = tree
                .active_root_turn_owner
                .lock()
                .map_err(|_| "active root turn owner lock was poisoned".to_string())?
                .ok_or_else(|| format!("agent `{path}` has no active durable root turn owner"))?;
            let cell = OnceLock::new();
            cell.set(owner)
                .expect("a fresh child root-turn owner cell must be empty");
            Arc::new(cell)
        };
        Ok(AgentRunContext {
            runtime: self.clone(),
            tree: tree.clone(),
            path: path.clone(),
            session_id,
            execution: lease.scope(),
            root_turn_owner,
            config: metadata.config,
            workspace: metadata.workspace,
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
                    let projection = self
                        .store
                        .protocol_event_store()
                        .durable_child_result_projection(summary.session_id())
                        .map_err(|error| error.to_string())?;
                    Ok(durable_child_result_from_projection(
                        summary.status(),
                        projection.latest_assistant_content.as_deref(),
                        projection.latest_error.as_deref(),
                    ))
                }
                SessionStatus::Failed => {
                    let projection = self
                        .store
                        .protocol_event_store()
                        .durable_child_result_projection(summary.session_id())
                        .map_err(|error| error.to_string())?;
                    Ok(durable_child_result_from_projection(
                        summary.status(),
                        projection.latest_assistant_content.as_deref(),
                        projection.latest_error.as_deref(),
                    ))
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
        session_id: SessionId,
        communication: InterAgentCommunication,
        require_active_recipient: bool,
    ) -> Result<HistoryItemId, String> {
        self.store
            .session_repo()
            .append_inter_agent_communication_with_protocol_bundle(
                session_id,
                communication,
                require_active_recipient,
            )
            .map_err(|error| error.to_string())
    }

    fn append_activity(
        &self,
        caller: &AgentRunContext,
        activity_id: &str,
        agent_session_id: SessionId,
        agent_path: &AgentPath,
        activity_kind: SubAgentActivityKind,
    ) -> Result<(), String> {
        let owner = caller.durable_root_turn_owner()?;
        self.store
            .protocol_event_store()
            .append_sub_agent_activity(
                caller.tree.root_session_id,
                owner.admission_id,
                owner.turn_id,
                activity_id.to_string(),
                agent_session_id,
                agent_path.to_string(),
                activity_kind,
            )
            .map_err(|error| error.to_string())
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
        Ok(summary) => match summary.status() {
            SessionStatus::Failed => Some(RunCancellationCause::Failure(format!(
                "run {} settled with durable failed status",
                summary.session_id()
            ))),
            SessionStatus::Cancelled => summary
                .interruption_cause()
                .map(RunCancellationCause::Interruption)
                .or_else(|| {
                    Some(RunCancellationCause::Failure(
                        missing_interruption_cause_message(summary.session_id()),
                    ))
                }),
            SessionStatus::Idle | SessionStatus::Running => {
                Some(RunCancellationCause::Failure(format!(
                    "run {} returned non-terminal status {}",
                    summary.session_id(),
                    summary.status().key()
                )))
            }
            SessionStatus::Completed => None,
        },
    }
}

fn activate_child_execution(lease: &AgentExecutionLease) -> Result<(), AgentTurnCompletion> {
    let run_control = lease.run_control();
    if let Some(completion) = child_completion_before_run_admission(&run_control) {
        return Err(completion);
    }
    if let Err(error) = lease.set_status(ActiveAgentStatus::Running) {
        return Err(AgentTurnCompletion::new(AgentStatus::Errored(format!(
            "child execution lease was rejected before durable turn admission: {error}"
        ))));
    }
    if let Some(completion) = child_completion_before_run_admission(&run_control) {
        return Err(completion);
    }
    Ok(())
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

fn inactive_agent_status(status: AgentStatus) -> Result<InactiveAgentStatus, String> {
    match status {
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
        AgentMailDeliveryOutcome::Suppressed => Err(SUPPRESSED_MAIL_DELIVERY_ERROR.to_string()),
    }
}

fn settle_unlaunchable_scheduled_execution(
    control: &AgentControl,
    lease: AgentExecutionLease,
    error: String,
) -> Vec<AgentExecutionLease> {
    let path = lease.path().clone();
    // The canonical communication remains durable, but this in-memory trigger cannot be consumed
    // by a turn whose context or worker failed to launch. Retaining it while handing the lease back
    // as inactive would make `complete_execution` reserve the same trigger again in a tight loop.
    let error = match control.drain_mailbox(&path) {
        Ok(_) => error,
        Err(mailbox_error) => format!(
            "{error}; failed to clear undeliverable trigger notices for `{path}`: {mailbox_error}"
        ),
    };
    control
        .complete_execution(lease, InactiveAgentStatus::Errored(error), None)
        .unwrap_or_default()
}

fn load_durable_agent_children(
    store: &StoreBundle,
    root_session_id: SessionId,
) -> Result<Vec<DurableAgentChild>, String> {
    let protocol_store = store.protocol_event_store();
    let child_limit = crate::runtime::agent_control::MAX_RETAINED_AGENTS.saturating_sub(1);
    let mut projected_children = Vec::new();
    let mut expected_total = None;
    loop {
        let page = protocol_store
            .retained_direct_child_page(
                root_session_id,
                projected_children.len(),
                crate::protocol::MAX_PROTOCOL_PAGE_LIMIT,
            )
            .map_err(|error| error.to_string())?;
        if page.total > child_limit {
            return Err(format!(
                "session {root_session_id} retains {} direct children, exceeding the supported maximum {child_limit}",
                page.total
            ));
        }
        match expected_total {
            Some(expected) if expected != page.total => {
                return Err(format!(
                    "retained child projection for session {root_session_id} changed during bounded hydration"
                ));
            }
            None => expected_total = Some(page.total),
            _ => {}
        }
        let page_len = page.items.len();
        projected_children.extend(page.items);
        if projected_children.len() >= page.total {
            break;
        }
        if page_len == 0 {
            return Err(format!(
                "retained child projection for session {root_session_id} made no progress at offset {}",
                projected_children.len()
            ));
        }
    }
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
        durable_children.push(DurableAgentChild {
            session_id: child.edge.child_session_id,
            edge: child.edge,
            session_status,
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

#[cfg(test)]
fn durable_child_result(
    status: SessionStatus,
    history: &[crate::protocol::HistoryItem],
) -> Option<String> {
    if status == SessionStatus::Failed {
        latest_error_history_text(history).or_else(|| latest_assistant_history_text(history))
    } else {
        latest_assistant_history_text(history).or_else(|| latest_error_history_text(history))
    }
}

#[cfg(test)]
fn latest_error_history_text(history: &[crate::protocol::HistoryItem]) -> Option<String> {
    history.iter().rev().find_map(|item| match &item.payload {
        HistoryItemPayload::Error { message, .. } if !message.trim().is_empty() => {
            Some(message.trim().to_string())
        }
        _ => None,
    })
}

#[cfg(test)]
fn latest_assistant_history_text(history: &[crate::protocol::HistoryItem]) -> Option<String> {
    history.iter().rev().find_map(|item| match &item.payload {
        HistoryItemPayload::AssistantMessage { content, .. } => content_parts_text(content, "\n"),
        _ => None,
    })
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
    match terminal_cause {
        Some(RunCancellationCause::Interruption(_)) => AgentStatus::Interrupted,
        Some(RunCancellationCause::Failure(message)) => {
            AgentStatus::Errored(content.unwrap_or_else(|| message.clone()))
        }
        Some(RunCancellationCause::Superseded) => {
            AgentStatus::Errored(content.unwrap_or_else(|| {
                "agent run was superseded before a durable terminal result was returned".to_string()
            }))
        }
        None => match result {
            Ok(summary) if summary.status() == SessionStatus::Completed => {
                AgentStatus::Completed(content)
            }
            Ok(summary) => AgentStatus::Errored(format!(
                "run {} returned terminal status {} without a typed terminal cause",
                summary.session_id(),
                summary.status().key()
            )),
            Err(error) => AgentStatus::Errored(error.to_string()),
        },
    }
}

fn agent_status_result(status: &AgentStatus) -> String {
    match status {
        AgentStatus::Completed(Some(result)) | AgentStatus::Errored(result) => preview(result, 320),
        AgentStatus::Completed(None) => "Completed".to_string(),
        AgentStatus::Interrupted => "Interrupted".to_string(),
        AgentStatus::Shutdown => "Stopped".to_string(),
        AgentStatus::PendingInit | AgentStatus::Running => String::new(),
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
