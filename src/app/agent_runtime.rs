use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::app::{AppCommand, RunRequest, RunService};
use crate::cli::{EventRenderer, OutputMode, SharedConfirmationPrompt};
use crate::config::ResolvedConfig;
use crate::config::model::full_effective_override;
use crate::error::{AppRunError, CliRenderError};
use crate::protocol::{
    ContentPart, HistoryItemPayload, InterAgentCommunication, ProtocolEventStore,
    SubAgentActivityKind, TurnId, project_inter_agent_communication, project_sub_agent_activity,
};
use crate::runtime::{
    AgentControl, AgentControlError, AgentExecutionLease, AgentMailDeliveryOutcome,
    AgentMailboxMessage, AgentPath, AgentSnapshot, AgentStatus,
};
use crate::session::{
    CanonicalHistoryPage, CanonicalRuntimeEventPage, CanonicalSessionRead, CanonicalTurnPage,
    IdleTurnAdmission, LoadedSessionList, MessagePart, RunEvent, RunSummary, RunningSessionRejoin,
    SessionCompactResult, SessionContext, SessionId, SessionMemoryModeUpdate, SessionRecord,
    SessionRepository, SessionSettingsPatch, SessionSpawnEdge, SessionStartRequest, SessionStatus,
    ThreadGoalClearResult, ThreadGoalGetResult, ThreadGoalSetResult, Transcript,
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
    config: ResolvedConfig,
    workspace: Workspace,
    live_config: Option<crate::runtime::LiveConfigOverrides>,
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

    pub(crate) fn tree_cancel_token(&self) -> CancellationToken {
        self.tree.control.tree_cancel_token()
    }

    fn effective_config(&self) -> ResolvedConfig {
        let mut config = self.config.clone();
        if let Some(live_config) = self.live_config.as_ref() {
            live_config.apply_to(&mut config);
        }
        config
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
        let _ = self
            .tree
            .control
            .set_activity(&self.path, Some(activity.into()));
    }

    pub(crate) fn drain_mailbox(&self) -> Vec<AgentMailboxMessage> {
        let messages = self
            .tree
            .control
            .drain_mailbox(&self.path)
            .unwrap_or_default();
        if let Ok(mut metadata) = self.tree.metadata.lock() {
            for message in &messages {
                if let Some(author) = metadata.get_mut(&message.author) {
                    author.updated = false;
                }
            }
        }
        messages
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
        let updated_agents = self
            .tree
            .control
            .mailbox_senders(&self.path)
            .map_err(agent_control_error)?
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
}

pub(crate) struct AgentRuntimeExecution {
    pub context: AgentRunContext,
    lease: Option<AgentExecutionLease>,
}

impl AgentRuntimeExecution {
    pub fn cancel_token(&self) -> CancellationToken {
        self.lease
            .as_ref()
            .expect("agent execution lease is retained until drop")
            .cancel_token()
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
            .complete_execution(lease, status, None)
            .map_err(agent_control_error)?;
        self.context.tree.release_confirmation_if_quiescent();
        Ok(scheduled)
    }
}

impl Drop for AgentRuntimeExecution {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            let _ = self.context.tree.control.set_status(
                &self.context.path,
                AgentStatus::Errored("agent execution ended before terminal handoff".to_string()),
            );
            drop(lease);
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
    metadata: Mutex<HashMap<AgentPath, AgentNodeMetadata>>,
}

#[derive(Clone)]
struct AgentNodeMetadata {
    task_name: String,
    task_preview: String,
    config: ResolvedConfig,
    workspace: Workspace,
    live_config: Option<crate::runtime::LiveConfigOverrides>,
    updated: bool,
}

struct DurableAgentChild {
    edge: SessionSpawnEdge,
    session: SessionRecord,
    task_preview: String,
    result: Option<String>,
}

struct AgentLaunchFailure {
    message: String,
    context: AgentRunContext,
    lease: AgentExecutionLease,
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

    pub(crate) fn begin_root(
        self: &Arc<Self>,
        session: &SessionContext,
        config: ResolvedConfig,
        confirmation: SharedConfirmationPrompt,
        live_config: Option<crate::runtime::LiveConfigOverrides>,
        external_cancel: CancellationToken,
    ) -> Result<AgentRuntimeExecution, String> {
        let root_session_id = session.session.id;
        let (tree, lease) = {
            let mut trees = self
                .trees
                .lock()
                .map_err(|_| "agent tree registry lock was poisoned".to_string())?;
            if let Some(tree) = trees
                .get(&root_session_id)
                .filter(|tree| !tree.control.tree_cancel_token().is_cancelled())
                .cloned()
            {
                if !tree.control.is_quiescent().map_err(agent_control_error)? {
                    return Err(format!(
                        "agent tree for session {root_session_id} is still active"
                    ));
                }
                tree.control
                    .reconfigure_max_concurrent_agents(config.multi_agent.max_concurrent_agents)
                    .map_err(agent_control_error)?;
                let lease = tree
                    .control
                    .try_acquire_execution(&AgentPath::root())
                    .map_err(agent_control_error)?;
                (tree, lease)
            } else {
                let durable_children = self.load_durable_children(root_session_id)?;
                let (control, lease) =
                    AgentControl::new(root_session_id, config.multi_agent.max_concurrent_agents)
                        .map_err(agent_control_error)?;
                let tree = Arc::new(AgentTreeRuntime {
                    root_session_id,
                    control,
                    confirmation: Mutex::new(None),
                    model_request_gate: Mutex::new(Arc::new(tokio::sync::Semaphore::new(1))),
                    metadata: Mutex::new(HashMap::new()),
                });
                self.restore_durable_children(
                    &tree,
                    durable_children,
                    &config,
                    &session.workspace,
                    live_config.as_ref(),
                )?;
                trees.insert(root_session_id, tree.clone());
                (tree, lease)
            }
        };
        tree.install_run_resources(
            confirmation,
            config.multi_agent.max_concurrent_model_requests,
        );
        tree.control
            .set_status(&AgentPath::root(), AgentStatus::Running)
            .map_err(agent_control_error)?;
        let mut metadata = tree
            .metadata
            .lock()
            .map_err(|_| "agent metadata lock was poisoned".to_string())?;
        for (path, node) in metadata.iter_mut() {
            if !path.is_root() {
                node.config = config.clone();
                node.workspace = session.workspace.clone();
                node.live_config = live_config.clone();
            }
        }
        metadata.insert(
            AgentPath::root(),
            AgentNodeMetadata {
                task_name: "root".to_string(),
                task_preview: String::new(),
                config: config.clone(),
                workspace: session.workspace.clone(),
                live_config: live_config.clone(),
                updated: false,
            },
        );
        drop(metadata);
        let context = AgentRunContext {
            runtime: self.clone(),
            tree: tree.clone(),
            path: AgentPath::root(),
            session_id: root_session_id,
            config,
            workspace: session.workspace.clone(),
            live_config,
        };
        let control = tree.control.clone();
        tokio::spawn(forward_external_cancel_while_root_active(
            control,
            external_cancel,
        ));
        Ok(AgentRuntimeExecution {
            context,
            lease: Some(lease),
        })
    }

    fn load_durable_children(
        &self,
        root_session_id: SessionId,
    ) -> Result<Vec<DurableAgentChild>, String> {
        let store = self.store.clone();
        let worker = std::thread::Builder::new()
            .name("moyai-agent-tree-rehydrate".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| {
                        format!("failed to build agent tree rehydration runtime: {error}")
                    })?;
                runtime.block_on(load_durable_agent_children(&store, root_session_id))
            })
            .map_err(|error| format!("failed to start agent tree rehydration: {error}"))?;
        worker
            .join()
            .map_err(|_| "agent tree rehydration worker panicked".to_string())?
    }

    fn restore_durable_children(
        &self,
        tree: &Arc<AgentTreeRuntime>,
        durable_children: Vec<DurableAgentChild>,
        config: &ResolvedConfig,
        workspace: &Workspace,
        live_config: Option<&crate::runtime::LiveConfigOverrides>,
    ) -> Result<(), String> {
        let mut restored_metadata = Vec::with_capacity(durable_children.len());
        for durable_child in durable_children {
            let DurableAgentChild {
                edge,
                session: child,
                task_preview,
                result,
            } = durable_child;
            if edge.root_session_id != tree.root_session_id {
                return Err(format!(
                    "spawn edge for child {} belongs to root {}, expected {}",
                    child.id, edge.root_session_id, tree.root_session_id
                ));
            }
            let parent_path = tree
                .control
                .path_for_session(edge.parent_session_id)
                .map_err(agent_control_error)?
                .ok_or_else(|| {
                    format!(
                        "spawn edge {} refers to missing parent session {}",
                        edge.agent_path, edge.parent_session_id
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
            let status = rehydrated_agent_state(&child, result)?;
            let snapshot = tree
                .control
                .restore_inactive_child(&parent_path, &edge.task_name, child.id, status, None)
                .map_err(agent_control_error)?;
            restored_metadata.push((
                snapshot.path,
                AgentNodeMetadata {
                    task_name: edge.task_name,
                    task_preview,
                    config: config.clone(),
                    workspace: workspace.clone(),
                    live_config: live_config.cloned(),
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
        load_durable_agent_children(&self.store, root_session_id)
            .await?
            .into_iter()
            .enumerate()
            .map(|(index, child)| {
                let status = durable_projection_status(&child.session, child.result);
                Ok(AgentActivityRecord {
                    agent_path: child.edge.agent_path,
                    session_id: child.session.id,
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
        cancelled: bool,
    ) {
        let tree = execution.context.tree.clone();
        let durable_success = matches!(
            result,
            Ok(summary)
                if matches!(
                    summary.status,
                    SessionStatus::Completed | SessionStatus::AwaitingUser
                )
        );
        if !durable_success && (cancelled || result.is_err()) {
            tree.control.cancel_tree();
        }
        let status = if cancelled && !durable_success {
            AgentStatus::Interrupted
        } else {
            match result {
                Ok(summary) => agent_status_from_summary(summary, None),
                Err(error) => AgentStatus::Errored(error.to_string()),
            }
        };
        if let Ok(scheduled) = execution.complete(status) {
            self.launch_scheduled_turns(&tree, scheduled);
        }
        tree.release_confirmation_if_quiescent();
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

    pub fn cancel_tree_for_session(&self, session_id: SessionId) -> bool {
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
            tree.control.cancel_tree();
            true
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
                    config: child_config.clone(),
                    workspace: caller.workspace.clone(),
                    live_config: caller.live_config.clone(),
                    updated: false,
                },
            );
        if let Err(error) = self.append_activity(
            caller.session_id,
            &activity_id,
            child_session_id,
            &child_path,
            SubAgentActivityKind::Started,
        ) {
            drop(lease);
            self.rollback_spawn(&caller.tree, &child_path, child_session_id)
                .await;
            return Err(error);
        }

        let child_context = AgentRunContext {
            runtime: self.clone(),
            tree: caller.tree.clone(),
            path: child_path.clone(),
            session_id: child_session_id,
            config: child_config,
            workspace: caller.workspace.clone(),
            live_config: caller.live_config.clone(),
        };
        if let Err(failure) = self.launch_agent_turn(child_context, lease, message) {
            drop(failure.lease);
            self.rollback_spawn(&caller.tree, &child_path, child_session_id)
                .await;
            return Err(failure.message);
        }
        Ok(snapshot)
    }

    async fn rollback_spawn(
        &self,
        tree: &Arc<AgentTreeRuntime>,
        path: &AgentPath,
        child_session_id: SessionId,
    ) {
        if let Ok(mut metadata) = tree.metadata.lock() {
            metadata.remove(path);
        }
        let _ = tree.control.remove_agent(path);
        let _ = self
            .store
            .session_repo()
            .delete_session_tree(child_session_id)
            .await;
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
        if caller.tree.control.tree_cancel_token().is_cancelled() {
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
        let mailbox_message = AgentMailboxMessage::new(
            caller.path.clone(),
            recipient_path.clone(),
            message,
            trigger_turn,
        );
        let delivery = caller
            .tree
            .control
            .enqueue_mail_after_durable_commit(mailbox_message, trigger_turn, || {
                self.append_communication(recipient.session_id, communication)
            })
            .map_err(agent_control_error)?;
        let scheduled = scheduled_mail_delivery(delivery)?;
        let _ = caller.tree.control.set_activity(
            &recipient_path,
            Some(format!("Message queued from {}", caller.path)),
        );
        let _ = self.append_activity(
            caller.session_id,
            &activity_id,
            recipient.session_id,
            &recipient_path,
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
            let _ = caller
                .tree
                .control
                .set_activity(&target_path, Some("Interrupt requested".to_string()));
        }
        let _ = self.append_activity(
            caller.session_id,
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
                let _ = context
                    .tree
                    .control
                    .set_status(&context.path, AgentStatus::Running);
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
                                AgentStatus::Errored(format!(
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
                let runtime_for_run = runtime.clone();
                let cancel = lease.cancel_token();
                let result = local.block_on(async move {
                    let config = runtime_for_run
                        .materialize_context_config_and_sync_session(&run_context)
                        .await
                        .map_err(AppRunError::Message)?;
                    let request = RunRequest {
                        prompt,
                        session_id: Some(run_context.session_id),
                        continue_last: false,
                        title: None,
                        cwd: run_context.workspace.cwd.clone(),
                        model: config.model.model.clone(),
                        base_url: config.model.base_url.clone(),
                        config_override: Some(full_effective_override(&config)),
                        output_mode: OutputMode::Human,
                        show_reasoning: false,
                        prompt_dispatch: None,
                        editor_context: None,
                        review_request: None,
                        image_paths: Vec::new(),
                        cancel,
                        live_config: run_context.live_config.clone(),
                        agent_confirmation: Some(run_context.confirmation_prompt()),
                        agent_context: Some(run_context),
                    };
                    run_service
                        .execute(AppCommand::Run(request), &mut renderer, &mut confirmation)
                        .await
                });
                let cancelled = lease.cancel_token().is_cancelled();
                let status =
                    local.block_on(runtime.finish_agent_turn(&context, &result, cancelled));
                let scheduled = context
                    .tree
                    .control
                    .complete_execution(lease, status, None)
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
        cancelled: bool,
    ) -> AgentStatus {
        let final_content = match result {
            Ok(summary) => self.final_assistant_text(summary).await,
            Err(error) => Some(error.to_string()),
        };
        let mut status = if cancelled {
            AgentStatus::Interrupted
        } else {
            match result {
                Ok(summary) => agent_status_from_summary(summary, final_content.clone()),
                Err(error) => AgentStatus::Errored(error.to_string()),
            }
        };
        if let Ok(mut metadata) = context.tree.metadata.lock()
            && let Some(node) = metadata.get_mut(&context.path)
        {
            node.updated = true;
        }

        let Some(parent) = context.path.parent() else {
            return status;
        };
        let parent_session_id = context
            .tree
            .control
            .list_agents(Some(&parent))
            .ok()
            .and_then(|agents| agents.into_iter().find(|agent| agent.path == parent))
            .map(|agent| agent.session_id);
        let Some(parent_session_id) = parent_session_id else {
            return status;
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
        let mailbox_message =
            AgentMailboxMessage::new(context.path.clone(), parent.clone(), content, false);
        match context
            .tree
            .control
            .enqueue_mail_after_durable_commit(mailbox_message, false, || {
                self.append_communication(parent_session_id, communication)
            }) {
            Ok(AgentMailDeliveryOutcome::Enqueued { .. }) => {}
            Ok(AgentMailDeliveryOutcome::Suppressed) => {
                status = AgentStatus::Errored(
                    "agent result was recorded durably, but delivery was suppressed because the recipient became terminal or the agent tree was stopped"
                        .to_string(),
                );
            }
            Err(error) => {
                status = AgentStatus::Errored(format!(
                    "agent result could not be delivered durably: {error}"
                ));
                let _ = context.tree.control.enqueue_mail(AgentMailboxMessage::new(
                    context.path.clone(),
                    parent,
                    "Agent result delivery failed; inspect list_agents for status.",
                    false,
                ));
            }
        }
        status
    }

    fn launch_scheduled_turns(
        self: &Arc<Self>,
        tree: &Arc<AgentTreeRuntime>,
        scheduled: Vec<AgentExecutionLease>,
    ) {
        let mut pending = scheduled.into_iter().collect::<VecDeque<_>>();
        while let Some(lease) = pending.pop_front() {
            let path = lease.path().clone();
            let context = self.context_for_path(tree, &path);
            let context = match context {
                Ok(context) => context,
                Err(error) => {
                    let additional = tree
                        .control
                        .complete_execution(lease, AgentStatus::Errored(error), None)
                        .unwrap_or_default();
                    pending.extend(additional);
                    continue;
                }
            };
            if let Err(failure) = self.launch_agent_turn(context, lease, String::new()) {
                let _ = tree.control.drain_mailbox(&failure.context.path);
                if let Ok(mut metadata) = tree.metadata.lock()
                    && let Some(node) = metadata.get_mut(&failure.context.path)
                {
                    node.updated = true;
                }
                let additional = tree
                    .control
                    .complete_execution(failure.lease, AgentStatus::Errored(failure.message), None)
                    .unwrap_or_default();
                pending.extend(additional);
            }
        }
        tree.release_confirmation_if_quiescent();
    }

    fn context_for_path(
        self: &Arc<Self>,
        tree: &Arc<AgentTreeRuntime>,
        path: &AgentPath,
    ) -> Result<AgentRunContext, String> {
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
            config: metadata.config,
            workspace: metadata.workspace,
            live_config: metadata.live_config,
        })
    }

    async fn final_assistant_text(&self, summary: &RunSummary) -> Option<String> {
        let message_id = summary.assistant_message_id?;
        let transcript = self
            .store
            .session_repo()
            .compatibility_transcript(summary.session_id)
            .await
            .ok()?;
        transcript
            .messages
            .iter()
            .find(|message| message.record.id == message_id)
            .map(|message| {
                message
                    .parts
                    .iter()
                    .filter_map(|part| match &part.payload {
                        MessagePart::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("")
            })
            .filter(|text| !text.trim().is_empty())
    }

    fn append_communication(
        &self,
        session_id: SessionId,
        communication: InterAgentCommunication,
    ) -> Result<(), String> {
        let turn_id = self.latest_or_new_turn(session_id)?;
        let projection = project_inter_agent_communication(session_id, turn_id, 0, communication);
        self.append_projection(projection)
    }

    fn append_activity(
        &self,
        owner_session_id: SessionId,
        activity_id: &str,
        agent_session_id: SessionId,
        agent_path: &AgentPath,
        activity_kind: SubAgentActivityKind,
    ) -> Result<(), String> {
        let turn_id = self.latest_or_new_turn(owner_session_id)?;
        let projection = project_sub_agent_activity(
            owner_session_id,
            turn_id,
            0,
            activity_id.to_string(),
            agent_session_id,
            agent_path.to_string(),
            activity_kind,
        );
        self.append_projection(projection)
    }

    fn append_projection(
        &self,
        projection: crate::protocol::ProtocolRunEventProjection,
    ) -> Result<(), String> {
        self.store
            .protocol_event_store()
            .append_event_bundle(
                &projection.runtime_event,
                projection.history_item.as_ref(),
                projection.turn_item.as_ref(),
            )
            .map_err(|error| error.to_string())
    }

    fn latest_or_new_turn(&self, session_id: SessionId) -> Result<TurnId, String> {
        self.store
            .protocol_event_store()
            .latest_turn_position_for_session(session_id)
            .map(|position| {
                position
                    .map(|(turn_id, _)| turn_id)
                    .unwrap_or_else(TurnId::new)
            })
            .map_err(|error| error.to_string())
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

async fn forward_external_cancel_while_root_active(
    control: AgentControl,
    external_cancel: CancellationToken,
) {
    let root = AgentPath::root();
    loop {
        let root_active = control
            .list_agents(Some(&root))
            .ok()
            .and_then(|agents| agents.into_iter().find(|agent| agent.path == root))
            .is_some_and(|agent| agent.is_active);
        if !root_active {
            return;
        }
        let observed_generation = control.activity_generation();
        let root_still_active = control
            .list_agents(Some(&root))
            .ok()
            .and_then(|agents| agents.into_iter().find(|agent| agent.path == root))
            .is_some_and(|agent| agent.is_active);
        if !root_still_active {
            return;
        }
        tokio::select! {
            _ = external_cancel.cancelled() => {
                let _ = control.cancel_agent(&root);
                return;
            }
            result = control.wait_for_activity(observed_generation) => {
                if result.is_err() {
                    return;
                }
            }
        }
    }
}

fn agent_control_error(error: AgentControlError) -> String {
    error.to_string()
}

fn scheduled_mail_delivery(
    outcome: AgentMailDeliveryOutcome,
) -> Result<Vec<AgentExecutionLease>, String> {
    match outcome {
        AgentMailDeliveryOutcome::Enqueued { scheduled, .. } => Ok(scheduled),
        AgentMailDeliveryOutcome::Suppressed => Err(SUPPRESSED_MAIL_DELIVERY_ERROR.to_string()),
    }
}

async fn load_durable_agent_children(
    store: &StoreBundle,
    root_session_id: SessionId,
) -> Result<Vec<DurableAgentChild>, String> {
    let repo = store.session_repo();
    let edges = repo
        .list_session_spawn_edges(root_session_id)
        .await
        .map_err(|error| error.to_string())?;
    let protocol_store = store.protocol_event_store();
    let mut durable_children = Vec::with_capacity(edges.len());
    for edge in edges {
        let session = repo
            .get_session(edge.child_session_id)
            .await
            .map_err(|error| error.to_string())?;
        let history = protocol_store
            .list_history_items_for_session(edge.child_session_id)
            .map_err(|error| error.to_string())?;
        let task_preview = latest_history_text(&history, |payload| match payload {
            HistoryItemPayload::UserTurn { content, .. }
            | HistoryItemPayload::SteerTurn { content, .. } => Some(content),
            _ => None,
        })
        .unwrap_or_else(|| edge.task_name.clone());
        let result = durable_child_result(session.status, &history);
        durable_children.push(DurableAgentChild {
            edge,
            session,
            task_preview,
            result,
        });
    }
    Ok(durable_children)
}

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

fn latest_error_history_text(history: &[crate::protocol::HistoryItem]) -> Option<String> {
    history.iter().rev().find_map(|item| match &item.payload {
        HistoryItemPayload::Error { message, .. } if !message.trim().is_empty() => {
            Some(message.trim().to_string())
        }
        _ => None,
    })
}

fn latest_assistant_history_text(history: &[crate::protocol::HistoryItem]) -> Option<String> {
    let (message_id, latest_content) =
        history.iter().rev().find_map(|item| match &item.payload {
            HistoryItemPayload::Message {
                message_id,
                role: crate::session::MessageRole::Assistant,
                content,
            } => Some((*message_id, content.as_slice())),
            _ => None,
        })?;
    if let Some(message_id) = message_id {
        let text = history
            .iter()
            .filter_map(|item| match &item.payload {
                HistoryItemPayload::Message {
                    message_id: Some(candidate_id),
                    role: crate::session::MessageRole::Assistant,
                    content,
                } if *candidate_id == message_id => Some(content.as_slice()),
                _ => None,
            })
            .flatten()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::Image { .. } => None,
            })
            .collect::<String>();
        return (!text.trim().is_empty()).then(|| text.trim().to_string());
    }
    content_parts_text(latest_content, "\n")
}

fn latest_history_text<'a>(
    history: &'a [crate::protocol::HistoryItem],
    content: impl Fn(&'a HistoryItemPayload) -> Option<&'a [ContentPart]>,
) -> Option<String> {
    history
        .iter()
        .rev()
        .find_map(|item| content_parts_text(content(&item.payload)?, "\n"))
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
    session: &SessionRecord,
    result: Option<String>,
) -> Result<AgentStatus, String> {
    match session.status {
        SessionStatus::Running => {
            return Err(format!(
                "cannot rehydrate running child session {} without an active execution owner",
                session.id
            ));
        }
        _ => Ok(durable_projection_status(session, result)),
    }
}

fn durable_projection_status(session: &SessionRecord, result: Option<String>) -> AgentStatus {
    match session.status {
        SessionStatus::Idle => AgentStatus::Shutdown,
        SessionStatus::Running => AgentStatus::Running,
        SessionStatus::Completed | SessionStatus::AwaitingUser => AgentStatus::Completed(result),
        SessionStatus::Cancelled => AgentStatus::Interrupted,
        SessionStatus::Failed => AgentStatus::Errored(
            result
                .unwrap_or_else(|| "Child session failed before this process started".to_string()),
        ),
    }
}

fn agent_status_from_summary(summary: &RunSummary, result: Option<String>) -> AgentStatus {
    match summary.status {
        SessionStatus::Completed | SessionStatus::AwaitingUser => AgentStatus::Completed(result),
        SessionStatus::Cancelled => AgentStatus::Interrupted,
        SessionStatus::Failed => {
            AgentStatus::Errored(result.unwrap_or_else(|| "agent run failed".to_string()))
        }
        SessionStatus::Idle | SessionStatus::Running => AgentStatus::Running,
    }
}

fn agent_status_result(status: &AgentStatus) -> String {
    match status {
        AgentStatus::Completed(Some(result)) | AgentStatus::Errored(result) => preview(result, 320),
        AgentStatus::Completed(None) => "Completed".to_string(),
        AgentStatus::Interrupted => "Interrupted".to_string(),
        AgentStatus::Shutdown => "Stopped".to_string(),
        AgentStatus::NotFound => "Agent not found".to_string(),
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
    fn render_session_show(&mut self, _transcript: &Transcript) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_history_items(
        &mut self,
        _session: &SessionRecord,
        _history_items: &[crate::protocol::HistoryItem],
        _show_reasoning: bool,
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
    fn render_session_compact_result(
        &mut self,
        _result: &SessionCompactResult,
    ) -> Result<(), CliRenderError> {
        Ok(())
    }
    fn render_session_memory_mode_update(
        &mut self,
        _update: &SessionMemoryModeUpdate,
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
